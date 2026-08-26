//! [`mediadecode::demuxer::Demuxer`] impl backed by `libavformat`.
//!
//! Opens a container — from a path, or from any `Read + Seek` reader
//! through a custom `AVIOContext` — reads its track table once, and
//! then hands packets out one at a time in interleaved file order.
//!
//! # What normalization this layer does
//!
//! libavformat's track table is not quite the one the demux tier
//! promises, and the gap is entirely about attachments:
//!
//! - **Cover art is an attachment, not video.** A still image in an
//!   MP3, FLAC or MP4 arrives as a video stream carrying
//!   `AV_DISPOSITION_ATTACHED_PIC`. This layer maps it to
//!   [`TrackKind::Attachment`], so the `Video` arm carries true motion
//!   video and nothing else.
//! - **A font's bytes are not in the packet stream at all.** An
//!   `AVMEDIA_TYPE_ATTACHMENT` stream never produces a packet; its
//!   payload lives in `AVCodecParameters.extradata`. This layer
//!   synthesizes the packet at open time.
//! - **Cover art's packet is hoisted.** libavformat parks the real
//!   packet in `AVStream.attached_pic`; some demuxers also emit it in
//!   the packet stream, some do not. This layer takes it from
//!   `attached_pic` at open time and drops the duplicate if it ever
//!   arrives, so the count is exactly one either way.
//!
//! Both kinds are queued at open — every attachment track, without
//! exception, or the open fails. That is what makes the face's "exactly
//! one packet, before any timed packet" true *by construction* here:
//! the queue is complete and drains before the first `av_read_frame`
//! call ever runs, so no packet on an attachment track can be anything
//! but a duplicate, and no seek can move a packet that was never on the
//! timeline.
//!
//! # Seeking
//!
//! `seek` converts the target to `AV_TIME_BASE` units and calls
//! `avformat_seek_file` over the window `[i64::MIN, target]`, which is
//! FFmpeg's backward convention: the landing point is the nearest
//! keyframe at or before the target, never after. `avformat_seek_file`
//! flushes libavformat's own buffers; this layer clears the EOF latch
//! it set itself, and deliberately does **not** touch the attachment
//! bookkeeping — an attachment already handed out is never handed out
//! again, and one not yet handed out is still owed.

use std::{
  collections::VecDeque,
  ffi::{CStr, c_int},
  io::{Read, Seek},
  mem,
  num::NonZeroI32,
  path::Path,
  ptr::{addr_of, read_unaligned},
  sync::Arc,
};

use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::{
  Packet, Rational,
  ffi::{
    AV_DISPOSITION_ATTACHED_PIC, AV_DISPOSITION_TIMED_THUMBNAILS, AV_NOPTS_VALUE, AVDictionary,
    AVStream, av_dict_get,
  },
  format::{self, context::Input},
};
use mediadecode::{
  Timebase, Timestamp,
  demuxer::{
    AttachmentPacket, AttachmentTrackPacket, AttachmentTrackParams, AudioTrackPacket,
    AudioTrackParams, DataTrackPacket, DataTrackParams, DemuxedPacket, Demuxer,
    SubtitleTrackPacket, SubtitleTrackParams, TrackIndex, TrackInfo, TrackKind, TrackParams,
    UnknownTrackParams, VideoTrackPacket, VideoTrackParams,
  },
};
use smol_str::SmolStr;

use crate::{
  Ffmpeg, boundary,
  buffer::PacketBufferError,
  codec_id::CodecId,
  extras::{AttachmentPacketExtra, TrackExtra},
  limits::DemuxLimits,
  reader_guard::{GuardedReader, PanicLatch},
  sample_format::SampleFormat,
};

/// One microsecond — the timebase `avformat_seek_file` expects when no
/// reference stream is named (`stream_index == -1`).
fn av_time_base_q() -> Timebase {
  Timebase::new(1, NonZeroI32::new(1_000_000).expect("1e6 is non-zero"))
}

/// `mediadecode::demuxer::Demuxer` impl wrapping `ffmpeg::format::context::Input`.
///
/// Construction is deliberately not on the trait — see [`Self::open`]
/// and [`Self::open_reader`].
pub struct CarrierDemuxer<C: crate::FfmpegCarrier> {
  input: Input,
  tracks: Vec<TrackInfo<Ffmpeg>>,
  pending: VecDeque<(
    TrackIndex,
    AttachmentPacket<AttachmentPacketExtra, C::Buffer>,
  )>,
  /// `true` once this session has answered `Ok(None)`. Only then does
  /// [`Self::seek`] clear the `AVIOContext`'s EOF latch — clearing it
  /// unconditionally would also erase a genuine sticky I/O error, which
  /// `Input::seek` goes out of its way to preserve.
  eof: bool,
  /// Set for a session opened over a caller's reader: where a panic
  /// raised inside that reader is recorded. `None` for a path-opened
  /// session, which runs no caller code.
  reader_panic: Option<Arc<PanicLatch>>,
  /// The budgets this session spends: on any one timed packet, and —
  /// already spent, at open — on the file's attachments.
  limits: DemuxLimits,
  /// A packet `av_read_frame` has already handed over and whose
  /// conversion has **not committed**, with the provenance that was
  /// observed for it.
  ///
  /// `av_read_frame` advances the container: once it returns, that
  /// packet is off the wire and nothing brings it back. A conversion
  /// that then fails on an *allocation* — a refcount the view lane
  /// could not take, a copy the middle row could not make — used to
  /// drop it, leaving a live session that answered the next pull with
  /// the **following** packet. Compressed data and subtitle cues went
  /// missing under memory pressure, quietly.
  ///
  /// So the read and the conversion are one transaction with a seat
  /// between them: a transient refusal parks the packet here and the
  /// next pull re-attempts *this* packet before reading another. It is
  /// the same park-then-replay the decode household already runs —
  /// `CarrierVideoStreamDecoder` holds `sw_replay_frames`, and the
  /// probe holds its rescue history — for the same reason: a byte C
  /// has already given up is not re-askable.
  ///
  /// The provenance is parked **with** the packet rather than re-probed
  /// on replay. It is an observation about the moment of delivery, and
  /// a queue that has moved on could answer it differently.
  unconverted: Option<(Packet, crate::buffer::PayloadProvenance)>,
}

// The generic bodies. Crate-private, because their bound is: they are
// the implementation, and the public faces below are written per lane
// so that no signature a consumer reads names a trait they cannot.
impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierDemuxer<C> {
  /// Opens a container from a filesystem path.
  ///
  /// Runs `avformat_open_input` followed by
  /// `avformat_find_stream_info`, then builds the track table and
  /// captures every attachment payload.
  ///
  /// Call [`ffmpeg_next::init`] once before the first open if you want
  /// FFmpeg's logging and network protocols configured; probing a local
  /// container does not require it.
  pub(crate) fn open_impl<P: AsRef<Path> + ?Sized>(path: &P) -> Result<Self, DemuxError> {
    Self::open_with_impl(path, DemuxLimits::default())
  }

  /// [`Self::open`], with the session's resource budgets named.
  ///
  /// The budgets are taken **at open** rather than through a `with_*`
  /// builder because the attachment half of them is spent here: every
  /// attachment payload in the file is captured before this call
  /// returns, which is what makes the demux tier's "exactly one packet,
  /// before any timed packet" contract true by construction. A budget
  /// set afterwards would arrive after the spending.
  ///
  /// A file whose attachments exceed the budget **fails to open**, with
  /// [`DemuxError::AttachmentTooLarge`] or
  /// [`DemuxError::AttachmentBudgetExhausted`] naming the track that
  /// crossed the line.
  pub(crate) fn open_with_impl<P: AsRef<Path> + ?Sized>(
    path: &P,
    limits: DemuxLimits,
  ) -> Result<Self, DemuxError> {
    // **The probe knobs, set before libavformat reads a byte.** See
    // [`DemuxLimits::max_probe_bytes`]: `avformat_open_input` and
    // `avformat_find_stream_info` build the attachment, extradata and
    // coded-side-data buffers themselves, so every budget that measures
    // *this crate's* copies arrives after the original allocation. The
    // instrument that reaches behind that is the one bounding what the
    // parser is handed in the first place.
    //
    // On this entrypoint that is `probesize` / `formatprobesize` /
    // `max_streams` only: the hard byte meter needs an `AVIOContext`
    // this crate owns, and a path is opened by libavformat's own
    // protocol layer. The reader entrypoint gets both.
    Self::from_input(
      format::input_with_dictionary(path, probe_options(limits))?,
      limits,
    )
  }

  /// Opens a container from any `Read + Seek` byte source, through a
  /// custom `AVIOContext`.
  ///
  /// `Seek` is mandatory and not negotiable: MP4 files routinely put
  /// `moov` at the end, so a reader that cannot go backwards cannot be
  /// probed at all — and the seek law on the face would be
  /// unimplementable.
  ///
  /// `filename` is a probe hint, not a path: libavformat uses its
  /// extension to break ties between formats whose byte signatures are
  /// ambiguous. Pass `None` when there is nothing to hint with.
  ///
  /// # A panicking reader
  ///
  /// libavformat drives the reader from `extern "C"` callbacks, where a
  /// panic would abort the process rather than unwind. Every call into
  /// `reader` therefore runs under `catch_unwind`: a panic becomes an
  /// I/O error for libavformat and surfaces here — or from the next
  /// [`next_packet`](Demuxer::next_packet) / [`seek`](Demuxer::seek) —
  /// as [`DemuxError::ReaderPanic`], carrying the panic's message. The
  /// session is terminal from that point: the `AVIOContext`'s error
  /// state is sticky and the reader's own state is unknown.
  pub(crate) fn open_reader_impl<R: Read + Seek + Send + 'static>(
    reader: R,
    filename: Option<&str>,
  ) -> Result<Self, DemuxError> {
    Self::open_reader_with_impl(reader, filename, DemuxLimits::default())
  }

  /// [`Self::open_reader`], with the session's resource budgets named.
  /// See [`Self::open_with`] for why they are taken at open.
  pub(crate) fn open_reader_with_impl<R: Read + Seek + Send + 'static>(
    reader: R,
    filename: Option<&str>,
    limits: DemuxLimits,
  ) -> Result<Self, DemuxError> {
    let (guarded, latch, meter) = GuardedReader::new(reader, limits.max_probe_bytes());
    let io = format::context::StreamIo::from_read_seek(guarded)?;
    let input =
      format::input_from_stream(io, filename, Some(probe_options(limits))).map_err(|e| {
        // Three ways this can fail, and they must not be confused: a
        // panicked reader, a probe budget reached, or libavformat's own
        // verdict. The meter is consulted before the errno because
        // libavformat folds the reader's I/O error into whatever it was
        // doing at the time — usually "invalid data" — which would
        // report a refusal this crate made as a malformed file.
        reader_panic(&latch)
          .or_else(|| {
            meter.tripped().then(|| {
              DemuxError::ProbeBudgetExhausted(ProbeBudgetExhausted::new(
                meter.read(),
                meter.budget(),
              ))
            })
          })
          .unwrap_or(DemuxError::Ffmpeg(e))
      })?;
    if meter.tripped() {
      return Err(DemuxError::ProbeBudgetExhausted(ProbeBudgetExhausted::new(
        meter.read(),
        meter.budget(),
      )));
    }
    // Open and analysed: the seat bounds *probing*, and reading the
    // media itself afterwards is the caller's business, packet by
    // packet, already bounded by the packet seats.
    meter.release();
    // A panic libavformat tolerated (a failed probe it recovered from)
    // still poisoned the reader; the session must not open over it.
    if let Some(panicked) = reader_panic(&latch) {
      return Err(panicked);
    }
    let mut demuxer = Self::from_input(input, limits)?;
    demuxer.reader_panic = Some(latch);
    Ok(demuxer)
  }

  /// Borrows the wrapped `ffmpeg::format::context::Input` — for
  /// `av_dump_format`, container-level metadata, chapters, and anything
  /// else the portable track table has no seat for.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn input_impl(&self) -> &Input {
    &self.input
  }

  /// The budgets this session was opened with.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn limits_impl(&self) -> DemuxLimits {
    self.limits
  }

  fn from_input(input: Input, limits: DemuxLimits) -> Result<Self, DemuxError> {
    let (tracks, pending) = build_tracks::<C>(&input, limits)?;
    Ok(Self {
      input,
      tracks,
      pending,
      unconverted: None,
      eof: false,
      reader_panic: None,
      limits,
    })
  }

  /// The error a panicked reader owes this session, if one panicked.
  fn panicked(&self) -> Option<DemuxError> {
    self.reader_panic.as_deref().and_then(reader_panic)
  }
}

/// The libavformat options this crate sets before a container is
/// opened.
///
/// Passed as an `AVDictionary` because that is the only route to these
/// fields that works for both entrypoints: `avformat_open_input`
/// applies the dictionary to the context it allocates itself, and the
/// same names reach the context behind a custom `AVIOContext`.
///
/// * `probesize` / `formatprobesize` bound what the format probe and
///   the stream analysis are allowed to consume;
/// * `max_streams` bounds the `AVStream` array a header can conjure —
///   a container claiming a hundred thousand streams is an allocation
///   this crate's per-track budgets are downstream of.
fn probe_options(limits: DemuxLimits) -> ffmpeg_next::Dictionary<'static> {
  let mut options = ffmpeg_next::Dictionary::new();
  let probe = limits.max_probe_bytes().to_string();
  options.set("probesize", &probe);
  options.set("formatprobesize", &probe);
  options.set("max_streams", &limits.max_streams().to_string());
  options
}

/// Payload for [`DemuxError::ProbeBudgetExhausted`].
///
/// libavformat wanted more of the file than the probe budget allows.
///
/// # What this bounds
///
/// This is the only seat in the crate that reaches *behind*
/// libavformat: `avformat_open_input` and `avformat_find_stream_info`
/// build the attached picture, the extradata and the coded side data
/// out of the file themselves, so every budget measuring this crate's
/// own copies necessarily arrives after those allocations happened.
///
/// A parser cannot allocate from bytes it was never handed, so the
/// input is bounded instead. What is **not** bounded is amplification
/// inside a parser — a container can describe, in a few bytes, a
/// structure whose in-memory form is far larger, and nothing outside
/// libavformat can observe that. Bounding the output of that is the
/// substrate's own hardening territory; FFmpeg keeps `max_streams`,
/// `max_index_size` and `max_picture_buffer` for it, and this crate
/// sets the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("libavformat read {read} bytes probing the container, over a budget of {budget}")]
pub struct ProbeBudgetExhausted {
  read: u64,
  budget: u64,
}

impl ProbeBudgetExhausted {
  /// Constructs a `ProbeBudgetExhausted` payload.
  #[inline]
  pub const fn new(read: u64, budget: u64) -> Self {
    Self { read, budget }
  }
  /// Bytes libavformat was handed before the budget was reached.
  #[inline]
  pub const fn read(&self) -> u64 {
    self.read
  }
  /// The budget in force.
  #[inline]
  pub const fn budget(&self) -> u64 {
    self.budget
  }
}

/// Turns a latched reader panic into the error that names it.
fn reader_panic(latch: &PanicLatch) -> Option<DemuxError> {
  latch
    .message()
    .map(|message| DemuxError::ReaderPanic(ReaderPanic::new(message)))
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierDemuxer<C> {
  pub(crate) fn tracks_impl(&self) -> &[TrackInfo<Ffmpeg>] {
    &self.tracks
  }

  pub(crate) fn take_tracks_impl(&mut self) -> Vec<TrackInfo<Ffmpeg>> {
    mem::take(&mut self.tracks)
  }

  pub(crate) fn next_packet_impl(
    &mut self,
  ) -> Result<Option<DemuxedPacket<Ffmpeg, C::Buffer>>, DemuxError> {
    // A latched reader panic is terminal, and terminal starts here. The
    // queue is filled at open and owes nothing to the reader, so a pull
    // that drained it would answer `Ok` to a caller the session has
    // already told the truth to — `seek` can latch a panic while
    // attachments are still queued.
    if let Some(panicked) = self.panicked() {
      return Err(panicked);
    }

    // The attachment queue drains first and drains completely, which is
    // the whole of "exactly one packet, before any timed packet": no
    // `av_read_frame` has run yet when the last one leaves.
    if let Some((track, packet)) = self.pending.pop_front() {
      return Ok(Some(DemuxedPacket::Attachment(AttachmentTrackPacket::new(
        track, packet,
      ))));
    }

    loop {
      // **A parked packet is re-attempted before another is read.**
      // See [`Self::unconverted`]: `av_read_frame` has already given
      // this one up, so reading past it would lose it.
      let (packet, parked_provenance) = match self.unconverted.take() {
        Some((packet, provenance)) => (packet, Some(provenance)),
        None => {
          let mut packet = Packet::empty();
          let read = packet.read(&mut self.input);
          // A panicking reader reported an ordinary I/O error to C, and
          // libavformat may answer that with the error, with EOF (a
          // stream it cannot read looks finished), or with a packet it
          // had already buffered. None of those are the file's word, so
          // the latch is consulted whatever the outcome was.
          if let Some(panicked) = self.panicked() {
            return Err(panicked);
          }
          match read {
            Ok(()) => {}
            Err(ffmpeg_next::Error::Eof) => {
              self.eof = true;
              return Ok(None);
            }
            // A demuxer can resync past a corrupt packet, and
            // `AVERROR_INVALIDDATA` is not latched into the
            // `AVIOContext`, so reading again makes progress. Every
            // other error is sticky and is surfaced.
            Err(ffmpeg_next::Error::InvalidData) => continue,
            Err(e) => return Err(DemuxError::Ffmpeg(e)),
          }
          (packet, None)
        }
      };

      let index = packet.stream();
      // A packet for a stream the table does not describe cannot be
      // placed. libavformat does not produce these, but the index comes
      // from C and indexes a `Vec`.
      let Some(info) = self.tracks.get(index) else {
        continue;
      };
      let track = TrackIndex::new(index);
      let time_base = info.timebase();

      // A payload that is there and cannot be referenced is an error,
      // never a silently dropped packet: `Ok(None)` below means the
      // packet carried nothing, and that is the only thing that reads
      // the next one.
      // **Everything this loop delivers is demux-delivered**, whatever
      // its refcount: libavformat just handed it over, so any other
      // reference to its buffer is libavformat's own and no
      // `ffmpeg_next::Packet` wraps one. That is not the hazard a
      // caller's second handle is — see
      // [`crate::buffer::PayloadProvenance`].
      //
      // Sharing is ordinary here. A queue-backed demuxer — SubRip,
      // SubViewer and the rest of the `FFDemuxSubtitlesQueue` family —
      // keeps its parsed cues and delivers `av_packet_ref`s of them,
      // so *every* packet it produces arrives with two references.
      //
      // The one sub-case that is stronger still is the container's
      // parked picture, which a stream carrying
      // `ATTACHED_PIC | TIMED_THUMBNAILS` delivers as its first packet:
      // written once while the container opened, so the view lane may
      // window it rather than copy. See [`is_streams_attached_pic`] for
      // the identity proof.
      //
      // SAFETY: both the session's `AVFormatContext` and `packet` are
      // live here.
      let provenance = match parked_provenance {
        // Observed when this packet was delivered, and kept with it.
        Some(provenance) => provenance,
        None if unsafe { is_streams_attached_pic(&self.input, index, &packet) } => {
          crate::buffer::PayloadProvenance::AttachedPicture
        }
        None => crate::buffer::PayloadProvenance::DemuxDelivered,
      };

      // The packet this loop just read is **handed over**, not lent: the
      // view lane's carrier is a window into its buffer, and a source
      // that survived the conversion would be a mutable alias of it.
      // Exactly one arm runs, so exactly one move happens.
      // **The conversion borrows what this session owns.** The packet
      // stays in hand until the carrier exists, which is what lets a
      // failure park it instead of dropping it; on success it falls out
      // of scope at the end of the iteration and the carrier keeps its
      // buffer alive by refcount, exactly as when the conversion
      // consumed it. Nothing outside this loop ever sees the packet, so
      // the borrow cannot become the aliasing shape the public faces
      // refuse.
      let converted = match info.kind() {
        TrackKind::Video => boundary::video_packet_from_borrowed::<C>(
          &packet,
          time_base,
          self.limits.packet(),
          provenance,
        )
        .map(|built| built.map(|p| DemuxedPacket::Video(VideoTrackPacket::new(track, p)))),
        TrackKind::Audio => boundary::audio_packet_from_borrowed::<C>(
          &packet,
          time_base,
          self.limits.packet(),
          provenance,
        )
        .map(|built| built.map(|p| DemuxedPacket::Audio(AudioTrackPacket::new(track, p)))),
        TrackKind::Subtitle => boundary::subtitle_packet_from_borrowed::<C>(
          &packet,
          time_base,
          self.limits.packet(),
          provenance,
        )
        .map(|built| built.map(|p| DemuxedPacket::Subtitle(SubtitleTrackPacket::new(track, p)))),
        TrackKind::Data => boundary::data_packet_from_borrowed::<C>(
          &packet,
          time_base,
          self.limits.packet(),
          provenance,
        )
        .map(|built| built.map(|p| DemuxedPacket::Data(DataTrackPacket::new(track, p)))),
        // Every attachment track's one packet was queued at open time,
        // so anything arriving on one now is the duplicate some
        // demuxers emit for cover art. Drop it — the contract is
        // exactly one, and the one has already left. Nothing is
        // converted here, so there is nothing to park.
        TrackKind::Attachment => continue,
        // The roster of arms is five; a track nothing can name has no
        // arm and its packets are not delivered.
        TrackKind::Unknown => continue,
      };

      let built = match converted {
        Ok(built) => built,
        Err(source) => {
          // **Park a refusal that another attempt could survive.** An
          // allocation that failed says nothing about the packet, and
          // the packet is off the wire either way. Anything else is a
          // fact about the packet itself — a malformed one is not made
          // well-formed by retrying, and parking it would answer every
          // later pull with the same error instead of letting the
          // session make progress.
          if source.parks_in_demux() {
            self.unconverted = Some((packet, provenance));
          }
          return Err(DemuxError::PacketBuffer(PacketBuffer::new(index, source)));
        }
      };

      // `None` here means the packet carried no payload — an empty
      // packet, which some demuxers emit as a marker. Nothing to
      // deliver; read the next one.
      if let Some(out) = built {
        return Ok(Some(out));
      }
    }
  }

  pub(crate) fn seek_impl(&mut self, target: Timestamp) -> Result<(), DemuxError> {
    let ts = target.rescale_to(av_time_base_q()).pts();
    // Only our own EOF latch is cleared, and only before the seek —
    // the seek machinery gates on `eof_reached`, so clearing it
    // afterwards would be too late.
    if self.eof {
      self.input.clear_eof();
      self.eof = false;
    }
    // `..ts` is how ffmpeg-next spells the seek window: it reads only
    // the endpoint, and `avformat_seek_file`'s `max_ts` is inclusive,
    // so the window is `[i64::MIN, ts]`. FFmpeg picks the closest seek
    // point inside it — the nearest keyframe at or before the target.
    // Never after: a decoder started past the target has no reference
    // frame.
    let sought = self.input.seek(ts, ..ts);
    if let Some(panicked) = self.panicked() {
      return Err(panicked);
    }
    sought?;
    // **The seat is cleared by a seek that happened, not by one that
    // was attempted.** A parked packet belongs to the position the
    // session is leaving, so a successful seek discards it. A *failed*
    // one leaves the session where it was — and that packet is off the
    // wire, so dropping it here would be the same silent loss the seat
    // exists to prevent, with no re-read able to recover it.
    //
    // FFmpeg does not specify where a container sits after a seek that
    // returned an error, and this crate does not guess: it keeps a
    // packet the container really did deliver, and a caller who saw the
    // seek fail already knows the position is not the one they asked
    // for. Every timestamp needed to tell is on the packet.
    self.unconverted = None;
    Ok(())
  }
}

macro_rules! demuxer_lane_face {
  ($($lane:ty),+ $(,)?) => { $(
    impl CarrierDemuxer<$lane> {
      /// Opens a container from a filesystem path.
      ///
      /// Runs `avformat_open_input` followed by
      /// `avformat_find_stream_info`, then builds the track table and
      /// captures every attachment payload.
      ///
      /// Call [`ffmpeg_next::init`] once before the first open if you
      /// want FFmpeg's logging and network protocols configured;
      /// probing a local container does not require it.
      pub fn open<P: AsRef<Path> + ?Sized>(path: &P) -> Result<Self, DemuxError> {
        Self::open_impl(path)
      }

      /// [`Self::open`], with the session's resource budgets named.
      ///
      /// The budgets are taken **at open** rather than through a
      /// `with_*` builder because the attachment half of them is spent
      /// here: every attachment payload is captured during this call.
      pub fn open_with<P: AsRef<Path> + ?Sized>(
        path: &P,
        limits: DemuxLimits,
      ) -> Result<Self, DemuxError> {
        Self::open_with_impl(path, limits)
      }

      /// Opens a container from any `Read + Seek` source.
      pub fn open_reader<R: Read + Seek + Send + 'static>(
        reader: R,
        url: Option<&str>,
      ) -> Result<Self, DemuxError> {
        Self::open_reader_impl(reader, url)
      }

      /// [`Self::open_reader`], with the session's budgets named.
      pub fn open_reader_with<R: Read + Seek + Send + 'static>(
        reader: R,
        url: Option<&str>,
        limits: DemuxLimits,
      ) -> Result<Self, DemuxError> {
        Self::open_reader_with_impl(reader, url, limits)
      }

      /// The wrapped `AVFormatContext`.
      pub const fn input(&self) -> &Input {
        self.input_impl()
      }

      /// The budgets this session was opened with.
      pub const fn limits(&self) -> DemuxLimits {
        self.limits_impl()
      }
    }

    impl Demuxer for CarrierDemuxer<$lane> {
      type Adapter = Ffmpeg;
      type Buffer = <$lane as crate::FfmpegCarrier>::Buffer;
      type Error = DemuxError;

      fn tracks(&self) -> &[TrackInfo<Ffmpeg>] {
        self.tracks_impl()
      }

      fn take_tracks(&mut self) -> Vec<TrackInfo<Ffmpeg>> {
        self.take_tracks_impl()
      }

      /// Pulls the next packet.
      ///
      /// **A refusal that another attempt could survive costs no
      /// packet.** `av_read_frame` advances the container, so a
      /// conversion that then fails on an allocation would otherwise
      /// drop bytes nothing can ask for again. Such a packet is parked
      /// instead, and this method re-attempts *it* before reading
      /// another — so a caller who pulls again loses nothing. A refusal
      /// about the packet itself is not parked: retrying a malformed
      /// packet forever would be worse than passing it by.
      fn next_packet(
        &mut self,
      ) -> Result<Option<DemuxedPacket<Ffmpeg, Self::Buffer>>, DemuxError> {
        self.next_packet_impl()
      }

      fn seek(&mut self, target: Timestamp) -> Result<(), DemuxError> {
        self.seek_impl(target)
      }
    }
  )+ };
}

demuxer_lane_face!(crate::View, crate::Owned);

/// Payload for [`DemuxError::AttachmentTooLarge`].
///
/// One attachment's payload exceeds
/// [`DemuxLimits::max_attachment_bytes`].
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error(
  "the attachment on stream {stream_index} is {bytes} bytes, over the {limit}-byte per-attachment budget"
)]
pub struct AttachmentTooLarge {
  stream_index: usize,
  bytes: usize,
  limit: usize,
}

impl AttachmentTooLarge {
  /// Constructs an `AttachmentTooLarge` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, bytes: usize, limit: usize) -> Self {
    Self {
      stream_index,
      bytes,
      limit,
    }
  }
  /// The `AVStream.index` carrying the oversized attachment.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The attachment's payload length.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The per-attachment budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`DemuxError::AttachmentBudgetExhausted`].
///
/// The file's attachments, together, exceed
/// [`DemuxLimits::max_total_attachment_bytes`].
///
/// Separate from [`AttachmentTooLarge`] because it is a different
/// attack: every attachment can be modest and there can still be four
/// hundred of them. This arm names the track that ran the total past
/// the line, not the track that was individually at fault — there
/// need not be one.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error(
  "the attachment on stream {stream_index} brings the file's attachments to {total} bytes, over the {limit}-byte budget"
)]
pub struct AttachmentBudgetExhausted {
  stream_index: usize,
  total: usize,
  limit: usize,
}

impl AttachmentBudgetExhausted {
  /// Constructs an `AttachmentBudgetExhausted` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, total: usize, limit: usize) -> Self {
    Self {
      stream_index,
      total,
      limit,
    }
  }
  /// The `AVStream.index` whose attachment crossed the line.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The running total, including this attachment.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn total(&self) -> usize {
    self.total
  }
  /// The whole-file budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`DemuxError::ParametersTooLarge`].
///
/// One stream's codec parameters hold more heap bytes than
/// [`DemuxLimits::max_codec_parameter_bytes`] allows.
///
/// The bytes are `extradata` plus every `coded_side_data` entry plus a
/// custom channel map — the three seats `AVCodecParameters` reaches the
/// heap through. A MOV `prof` atom lands in the second of those as an
/// ICC profile, which is where the honest large values live and where
/// the forged ones do too.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error(
  "the codec parameters on stream {stream_index} hold {bytes} heap bytes, over the {limit}-byte budget"
)]
pub struct ParametersTooLarge {
  stream_index: usize,
  bytes: usize,
  limit: usize,
}

impl ParametersTooLarge {
  /// Constructs a `ParametersTooLarge` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, bytes: usize, limit: usize) -> Self {
    Self {
      stream_index,
      bytes,
      limit,
    }
  }
  /// The `AVStream.index` whose parameters were refused.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The heap bytes the parameters declared.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`DemuxError::ParametersBudgetExhausted`].
///
/// Every stream's codec parameters, together, hold more heap bytes than
/// [`DemuxLimits::max_total_codec_parameter_bytes`] allows.
///
/// A separate attack from [`ParametersTooLarge`], and separate for the
/// same reason the attachment pair are: each stream's parameters can be
/// individually modest and a container can still declare two hundred
/// streams. The arm names the stream that ran the total past the line,
/// which need not be one that was individually at fault.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error(
  "the codec parameters on stream {stream_index} bring the file's to {total} heap bytes, over the {limit}-byte budget"
)]
pub struct ParametersBudgetExhausted {
  stream_index: usize,
  total: usize,
  limit: usize,
}

impl ParametersBudgetExhausted {
  /// Constructs a `ParametersBudgetExhausted` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, total: usize, limit: usize) -> Self {
    Self {
      stream_index,
      total,
      limit,
    }
  }
  /// The `AVStream.index` whose parameters crossed the line.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The running total, including this stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn total(&self) -> usize {
    self.total
  }
  /// The whole-file budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`DemuxError::ParametersMissing`].
///
/// Codec parameters arrived that were never allocated.
///
/// `ffmpeg_next::codec::Parameters` has safe constructors that hand
/// back a null-backed value when FFmpeg's allocation failed, and they
/// report nothing. Copying from one dereferences null, so it is
/// refused where it arrives — at construction, and again in the
/// copier — rather than crashing later somewhere that has forgotten
/// the allocator ever failed.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the codec parameters for stream {stream_index} were never allocated")]
pub struct ParametersMissing {
  stream_index: usize,
}

impl ParametersMissing {
  /// Constructs a `ParametersMissing` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize) -> Self {
    Self { stream_index }
  }
  /// The `AVStream.index` the parameters were offered for.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
}

/// Payload for [`DemuxError::ParametersAlloc`].
///
/// Codec parameters for a track could not be allocated.
#[derive(thiserror::Error, Debug, Clone)]
#[error("out of memory allocating the codec parameters for stream {stream_index}")]
pub struct ParametersAlloc {
  stream_index: usize,
}

impl ParametersAlloc {
  /// Constructs a `ParametersAlloc` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize) -> Self {
    Self { stream_index }
  }
  /// The `AVStream.index` whose parameters could not be copied.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
}

/// Payload for [`DemuxError::ParametersCopy`].
///
/// Copying a track's codec parameters failed part way.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the codec parameters for stream {stream_index} could not be copied: {source}")]
pub struct ParametersCopy {
  stream_index: usize,
  #[source]
  source: ffmpeg_next::Error,
}

impl ParametersCopy {
  /// Constructs a `ParametersCopy` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, source: ffmpeg_next::Error) -> Self {
    Self {
      stream_index,
      source,
    }
  }
  /// The `AVStream.index` whose parameters could not be copied.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// What FFmpeg said.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn source(&self) -> &ffmpeg_next::Error {
    &self.source
  }
}

/// Payload for [`DemuxError::PacketBuffer`].
///
/// A packet's payload could not be referenced — the bytes are there
/// and this layer could not carry them.
///
/// Never raised for a packet that simply has no payload: an empty
/// packet is a marker some demuxers emit, and it is skipped in
/// silence. Distinguishing the two is what keeps a refcount failure
/// under memory pressure from looking like the file's own word and
/// dropping real compressed bytes.
#[derive(thiserror::Error, Debug, Clone)]
#[error("stream {stream_index}: {source}")]
pub struct PacketBuffer {
  stream_index: usize,
  #[source]
  source: PacketBufferError,
}

impl PacketBuffer {
  /// Constructs a `PacketBuffer` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, source: PacketBufferError) -> Self {
    Self {
      stream_index,
      source,
    }
  }
  /// The `AVStream.index` the packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// What went wrong.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn source(&self) -> &PacketBufferError {
    &self.source
  }
}

/// Payload for [`DemuxError::ReaderPanic`].
///
/// The `Read + Seek` source given to [`FfmpegDemuxer::open_reader`]
/// panicked inside a libavformat callback.
///
/// The panic was caught before it could cross the `extern "C"`
/// boundary and abort the process; this is what it said. The session
/// is terminal — every later call reports the same panic.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the reader panicked: {message}")]
pub struct ReaderPanic {
  message: SmolStr,
}

impl ReaderPanic {
  /// Constructs a `ReaderPanic` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(message: SmolStr) -> Self {
    Self { message }
  }
  /// What the panic payload said.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn message(&self) -> &str {
    self.message.as_str()
  }
}

/// Errors from [`FfmpegDemuxer`].
///
/// **Open fault taxonomy, so it is `#[non_exhaustive]`.** New ways to
/// fail are discovered — a backend, a ceiling, a corruption a codec
/// learns to report — and a consumer that meets one it has never heard
/// of should take its generic-fault path. That is exactly what the
/// wildcard arm this attribute forces is for. The two status
/// vocabularies opposite it,
/// [`Sent`](mediadecode::Sent) and [`Received`](mediadecode::Received),
/// are exhaustive for the mirror-image reason: their arms are the
/// substrate's fixed state set, and there the wildcard would be dead
/// weight hiding a state a consumer forgot.
#[derive(thiserror::Error, Debug, Clone, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
#[non_exhaustive]
pub enum DemuxError {
  /// The wrapped libavformat call reported an error — open, read or
  /// seek.
  #[error(transparent)]
  Ffmpeg(#[from] ffmpeg_next::Error),

  /// libavformat asked for more bytes than the probe budget allows
  /// while opening and analysing the container. See
  /// [`ProbeBudgetExhausted`].
  #[error(transparent)]
  ProbeBudgetExhausted(#[from] ProbeBudgetExhausted),

  /// One attachment's payload is over the per-attachment budget.
  /// Refused at open, before the copy.
  #[error(transparent)]
  AttachmentTooLarge(#[from] AttachmentTooLarge),

  /// The file's attachments, together, are over the whole-file budget.
  /// Refused at open, before the copy that would have crossed it.
  #[error(transparent)]
  AttachmentBudgetExhausted(#[from] AttachmentBudgetExhausted),

  /// One stream's codec parameters hold more heap bytes than the
  /// budget allows. Refused at open, before the clone.
  #[error(transparent)]
  ParametersTooLarge(#[from] ParametersTooLarge),

  /// Every stream's codec parameters together are over the whole-file
  /// budget. Refused at open, before the clone that would have crossed
  /// it.
  #[error(transparent)]
  ParametersBudgetExhausted(#[from] ParametersBudgetExhausted),

  /// Codec parameters arrived that were never allocated.
  #[error(transparent)]
  ParametersMissing(#[from] ParametersMissing),

  /// Codec parameters for a track could not be allocated.
  #[error(transparent)]
  ParametersAlloc(#[from] ParametersAlloc),

  /// Copying a track's codec parameters failed part way.
  #[error(transparent)]
  ParametersCopy(#[from] ParametersCopy),

  /// A packet's payload could not be referenced — the bytes are there
  /// and this layer could not carry them.
  #[error(transparent)]
  PacketBuffer(#[from] PacketBuffer),

  /// The `Read + Seek` source given to
  /// [`FfmpegDemuxer::open_reader`] panicked inside a libavformat
  /// callback.
  #[error(transparent)]
  ReaderPanic(#[from] ReaderPanic),
}

// ---------------------------------------------------------------------------
//  Track-table construction.
// ---------------------------------------------------------------------------

type BuiltTracks<C> = (
  Vec<TrackInfo<Ffmpeg>>,
  VecDeque<(
    TrackIndex,
    AttachmentPacket<AttachmentPacketExtra, <C as crate::FfmpegCarrier>::Buffer>,
  )>,
);

fn build_tracks<C: crate::FfmpegCarrier + crate::CarrierOps>(
  input: &Input,
  limits: DemuxLimits,
) -> Result<BuiltTracks<C>, DemuxError> {
  // **Admission before allocation.** Every attachment in the file is
  // judged here, in full, before the loop below allocates anything at
  // all — see [`admit_streams`] for why the charge cannot live
  // inside the capture.
  admit_streams(input, limits)?;

  let count = input.streams().len();
  let mut tracks = Vec::with_capacity(count);
  let mut pending = VecDeque::new();

  for stream in input.streams() {
    let index = stream.index();
    // `AVStream.index` is the stream's position in `ic->streams[]` and
    // libavformat keeps the two identical. The demux tier makes
    // `TrackIndex` mean "position in `tracks()`", so the two agree by
    // construction — but only if they really are dense and in order,
    // which is cheap to insist on rather than assume.
    debug_assert_eq!(
      index,
      tracks.len(),
      "AVStream indices are dense and ordered"
    );

    let parameters = stream.parameters();
    let par = unsafe { parameters.as_ptr() };
    // Never read `AVCodecParameters.codec_type` / `.codec_id` as their
    // bindgen enums: a value outside this build's discriminant set is
    // UB the moment it exists. Both are read as the raw integers they
    // are on the wire — the medium through [`boundary::media_kind_of`],
    // which folds anything unnamed into `Unknown`.
    //
    // The medium used to go through `Parameters::medium()` on the
    // argument that `AVMediaType`'s set is tiny and stable. It is; that
    // made the read unlikely to bite, not sound. The exception is gone
    // rather than defended, so no attacker-reachable path in this crate
    // forms a bindgen enum out of FFmpeg memory.
    let medium = boundary::media_kind_of(&parameters);
    let codec =
      CodecId::from_raw(unsafe { read_unaligned(addr_of!((*par).codec_id).cast::<i32>()) });

    let disposition = unsafe { (*stream.as_ptr()).disposition };
    let attached_pic = is_attachment_disposition(disposition);

    let time_base = rational_to_timebase(stream.time_base());
    let raw_duration = stream.duration();
    let duration = (raw_duration != AV_NOPTS_VALUE && raw_duration > 0)
      .then(|| Timestamp::new(raw_duration, time_base));
    let raw_start = stream.start_time();
    let frames = stream.frames();

    let params = if attached_pic {
      // Cover art. A still image in a video-shaped slot is an
      // attachment by every property that matters, and the `Video` arm
      // is reserved for motion video.
      TrackParams::Attachment(AttachmentTrackParams::new(codec))
    } else {
      match medium {
        boundary::MediaKind::Video => TrackParams::Video(VideoTrackParams::new(
          codec,
          unsafe { (*par).width }.max(0) as u32,
          unsafe { (*par).height }.max(0) as u32,
          boundary::from_av_pixel_format(unsafe { (*par).format }),
          rate_to_timebase(stream.avg_frame_rate()),
        )),
        boundary::MediaKind::Audio => {
          let ch_layout = unsafe { std::ptr::addr_of!((*par).ch_layout) };
          // SAFETY: `par` is a live `*const AVCodecParameters` for the
          // life of `parameters`; the helper validates `order` as an
          // `i32` before constructing any `AVChannelOrder`.
          let channel_layout =
            unsafe { crate::channel_layout::channel_layout_description_from_raw_ptr(ch_layout) };
          TrackParams::Audio(AudioTrackParams::new(
            codec,
            unsafe { (*par).sample_rate }.max(0) as u32,
            channel_layout.channels().min(255) as u8,
            SampleFormat::from_raw(unsafe { (*par).format }),
            channel_layout,
          ))
        }
        boundary::MediaKind::Subtitle => TrackParams::Subtitle(SubtitleTrackParams::new(codec)),
        boundary::MediaKind::Data => TrackParams::Data(DataTrackParams::new(codec)),
        boundary::MediaKind::Attachment => {
          TrackParams::Attachment(AttachmentTrackParams::new(codec))
        }
        boundary::MediaKind::Unknown => TrackParams::Unknown(UnknownTrackParams::new(codec)),
      }
    };

    // The parameter copy. For an `AVMEDIA_TYPE_ATTACHMENT` stream its
    // `extradata` **is** the attachment's payload — the same bytes the
    // carrier below already holds — so it is left behind rather than
    // copied. Censused before it was: nothing can use it. libavcodec
    // has no decoder for a font (`avcodec_find_decoder` answers null
    // for `AV_CODEC_ID_TTF` and its siblings), so no road in this crate
    // or downstream of it opens a codec context from these parameters;
    // the payload reaches a consumer as the attachment packet, which is
    // the delivery the demux tier promises.
    //
    // **Omitted, not stripped.** An earlier shape copied the extradata
    // and freed it immediately afterwards, which allocated the payload
    // for no reason and — worse — charged it against the *parameter*
    // ceiling on the way past. A font between the two ceilings passed
    // the admission pass and then failed inside the clone. See
    // [`ExtradataPolicy`](crate::extras::ExtradataPolicy).
    //
    // Cover art keeps its extradata: there the payload is the parked
    // `AVPacket`, extradata is *not* a copy of it, and a still codec
    // can legitimately need it (MJPEG with an external Huffman table).
    // Measured on this build: a cover-art stream carries none anyway.
    let extradata_policy = if medium.is_attachment() {
      crate::extras::ExtradataPolicy::Omit
    } else {
      crate::extras::ExtradataPolicy::Copy
    };
    let parameters_copy = crate::extras::bounded_clone_parameters_with(
      &parameters,
      index,
      limits.max_codec_parameter_bytes(),
      extradata_policy,
    )?;
    let extra = TrackExtra::new(index as i32, parameters_copy)?
      .with_disposition(disposition)
      .with_start_time((raw_start != AV_NOPTS_VALUE).then_some(raw_start))
      .with_frame_count((frames > 0).then_some(frames));

    // SAFETY: `stream` keeps the `AVStream` — and so its metadata
    // dictionary — live across both reads. The dictionary is read
    // through `av_dict_get` rather than through
    // `DictionaryRef::get`: see [`metadata_text`].
    let metadata = unsafe { (*stream.as_ptr()).metadata };
    let info = TrackInfo::new(time_base, params, extra)
      .with_duration(duration)
      .with_filename(unsafe { metadata_text(metadata, c"filename") })
      .with_mime_type(unsafe { metadata_text(metadata, c"mimetype") });

    // Capture the attachment payload now, so the queue is complete
    // before a single timed packet has been read. Every attachment
    // track leaves this loop with exactly one packet queued, or the
    // open fails: that is what makes "exactly one packet, before any
    // timed packet" a property of the construction rather than a
    // promise the pull loop has to keep.
    if info.kind() == TrackKind::Attachment {
      let packet = if attached_pic {
        // SAFETY: `stream` keeps the format context (and so the
        // `AVStream`) live; `attached_pic` is an `AVPacket` embedded by
        // value, and `addr_of!` reaches it without forming a reference
        // to the stream.
        let pkt = unsafe { std::ptr::addr_of!((*stream.as_ptr()).attached_pic) };
        unsafe { attached_pic_payload::<C>(pkt, index, limits) }?
      } else {
        extradata_payload::<C>(&stream, limits)?
      };
      pending.push_back((TrackIndex::new(index), packet));
    }

    tracks.push(info);
  }

  Ok((tracks, pending))
}

/// Whether `packet`'s payload is the very allocation the container has
/// parked in `AVStream.attached_pic` for stream `index`.
///
/// # Why this exists
///
/// libavformat queues a stream's attached picture as its **first
/// packet** — `read_frame_internal` does `av_packet_ref(pkt,
/// &st->attached_pic)` and keeps its own reference — so that packet
/// arrives with two references through nobody's fault. A pure cover-art
/// stream never reaches this road (it is an attachment, hoisted at
/// open), but a stream carrying `ATTACHED_PIC | TIMED_THUMBNAILS` is
/// deliberately classified as **video** by
/// [`is_attachment_disposition`], so its first pull comes through here
/// and would be refused as a shared payload. Every packet after it is
/// an ordinary timed one with a buffer of its own.
///
/// # The probe, and why it is a proof rather than a guess
///
/// `av_buffer_ref` sets the new reference's `buffer` field to the
/// source's, so two `AVBufferRef`s name one allocation **iff** their
/// `buffer` pointers are equal — the same identity
/// [`crate::FfmpegBuffer::ptr_eq`] rests on. Comparing them therefore
/// establishes the fact the carve-out needs: this payload's allocation
/// *is* `AVStream.attached_pic`'s, so one of its outstanding references
/// is the container's own.
///
/// The alternatives were heuristics and are not used: the disposition
/// bits say a stream *has* an attached picture, not that this packet is
/// it; "the first packet on the stream" is an ordering assumption that
/// nothing in libavformat's contract fixes.
///
/// # The soundness argument, restated for this packet
///
/// It is the same one the hoisted-attachment road rests on, and it
/// holds here for the same reason. `AVStream.attached_pic` is written
/// once, while the container is being opened, and never again; the
/// reference this crate is looking at is the container's, held for the
/// lifetime of the `AVFormatContext`, and there is no
/// `ffmpeg_next::Packet` wrapping it for anyone to call `data_mut` on.
/// What the uniqueness rule guards against is a *safe Rust* handle that
/// may write while this crate reads, and the container's reference is
/// not one.
///
/// # Safety
///
/// `input` and `packet` must both be live for the duration of the call.
unsafe fn is_streams_attached_pic(input: &Input, index: usize, packet: &Packet) -> bool {
  // SAFETY: `input` owns a live `AVFormatContext`; `streams` is an
  // array of `nb_streams` pointers, and `index` is checked against it.
  let stream = unsafe {
    let context = input.as_ptr();
    if index >= (*context).nb_streams as usize {
      return false;
    }
    *(*context).streams.add(index)
  };
  if stream.is_null() {
    return false;
  }
  // SAFETY: `stream` is one of the context's own live `AVStream`s and
  // `packet` is live per this function's contract.
  unsafe { packet_is_parked_picture(stream, packet) }
}

/// The identity itself: whether `packet`'s payload allocation is the
/// one `stream` has parked in `attached_pic`.
///
/// Split out from [`is_streams_attached_pic`] so the comparison can be
/// tested against a hand-built pair without forging an
/// `AVFormatContext` — see `a_queued_attached_picture_is_recognised`.
///
/// # Safety
///
/// `stream` must be a live `AVStream` and `packet` a live `AVPacket`.
unsafe fn packet_is_parked_picture(stream: *const AVStream, packet: &Packet) -> bool {
  use ffmpeg_next::packet::Ref;

  // SAFETY: both are live per the contract; `attached_pic` is an inline
  // `AVPacket` and both `buf` fields may be null, which is answered
  // before either is read through.
  unsafe {
    let parked = (*stream).attached_pic.buf;
    let carried = (*packet.as_ptr()).buf;
    if parked.is_null() || carried.is_null() {
      return false;
    }
    // The shared `AVBuffer`, not the `AVBufferRef`: `av_packet_ref`
    // mints a new reference struct around the same allocation, so
    // comparing the references themselves would answer "no" to exactly
    // the case this is for.
    (*parked).buffer == (*carried).buffer
  }
}

/// Whether a stream's disposition makes it an **attachment** — a
/// payload with no place on the timeline — rather than a timed track.
///
/// `AV_DISPOSITION_ATTACHED_PIC` alone says "cover art": one still
/// image, parked in `AVStream.attached_pic`, no timeline. But FFmpeg
/// pairs it with `AV_DISPOSITION_TIMED_THUMBNAILS` for a different
/// thing entirely — "the stream is sparse, and contains thumbnail
/// images, often corresponding to chapter markers", a flag its own
/// header documents as *only ever* used together with `ATTACHED_PIC`.
/// Such a stream has many images and every one of them has a
/// timestamp.
///
/// Classifying that as an attachment loses all but the first: the
/// attachment contract is exactly one packet, so the queue takes the
/// parked copy and the delivery loop drops every timed packet on the
/// track. It goes to the **`Video`** arm instead. That does not
/// contradict "cover art is an attachment, not video" — the reason
/// behind that ruling is that a single still with no timeline must not
/// look like a motion track, and a timed-thumbnail stream *is* on the
/// timeline. It is sparse video: a codec id, a frame size, a pixel
/// format and packets with timestamps, which is everything a consumer
/// needs to decode the images. The `Data` arm was the alternative and
/// is worse: it would strand encoded pictures in an arm that names no
/// decoder.
///
/// The bits are tested against the raw `AVStream.disposition` rather
/// than through `ffmpeg_next`'s `Disposition`, which mints no
/// `TIMED_THUMBNAILS` constant at all — its `from_bits_truncate` drops
/// every bit this build of the wrapper has no name for, which is how
/// the distinction went missing in the first place.
const fn is_attachment_disposition(disposition: c_int) -> bool {
  disposition & AV_DISPOSITION_ATTACHED_PIC != 0
    && disposition & AV_DISPOSITION_TIMED_THUMBNAILS == 0
}

/// Upper bound on the NUL search in [`metadata_text`].
///
/// Generous by four orders of magnitude for a filename or a MIME type,
/// and there only so that a value libavutil did not terminate cannot
/// turn the walk into an unbounded read — the same discipline
/// [`crate::channel_layout`] and the pixel-format namer follow. A value
/// longer than this is refused rather than truncated: a truncated
/// filename is a different filename.
const METADATA_VALUE_MAX_BYTES: usize = 64 * 1024;

/// Reads one entry out of a container's metadata dictionary as text
/// this crate can own.
///
/// **Why not `DictionaryRef::get`.** ffmpeg-next 9.0.0 builds its
/// `&str` with `from_utf8_unchecked`
/// (`src/util/dictionary/immutable.rs`), and FFmpeg does not validate
/// demuxed metadata as UTF-8 — an ID3 frame, a Matroska attachment
/// name or a MOV atom carries whatever bytes the file carries. A
/// `filename` holding a stray `0x80` would therefore have produced a
/// `&str` that is not UTF-8: undefined behaviour the moment it exists,
/// before `SmolStr` ever copies it.
///
/// Invalid bytes are replaced (`U+FFFD`), not refused. This is
/// *identity* metadata — the name a font was attached under, the MIME
/// type declared for a cover — and a file that names its attachment in
/// some legacy codepage is still a file worth opening. The replacement
/// characters say plainly that the container's bytes were not text.
///
/// # Safety
///
/// `dict` must be null or a live `*const AVDictionary` for the
/// duration of this call.
unsafe fn metadata_text(dict: *const AVDictionary, key: &CStr) -> Option<SmolStr> {
  if dict.is_null() {
    return None;
  }
  // SAFETY: `dict` is live per the contract above and `key` is a
  // NUL-terminated C string by construction; `av_dict_get` reads both
  // and returns a borrowed entry owned by the dictionary.
  let entry = unsafe { av_dict_get(dict, key.as_ptr(), std::ptr::null(), 0) };
  if entry.is_null() {
    return None;
  }
  // SAFETY: a non-null entry is a live `AVDictionaryEntry` for as long
  // as the dictionary is not modified, which it is not here.
  let value = unsafe { (*entry).value };
  if value.is_null() {
    return None;
  }
  for len in 0..METADATA_VALUE_MAX_BYTES {
    // SAFETY: `value` is a NUL-terminated string libavutil allocated
    // with `av_strdup`; the walk reads at most one byte past the last
    // value byte and stops at the terminator.
    if unsafe { *value.add(len).cast::<u8>() } == 0 {
      // SAFETY: the `len` bytes below the terminator were just walked,
      // so the slice is in bounds and initialised.
      let bytes = unsafe { std::slice::from_raw_parts(value.cast::<u8>(), len) };
      return Some(SmolStr::new(std::string::String::from_utf8_lossy(bytes)));
    }
  }
  None
}

/// Wraps `AVStream.attached_pic` — the real packet libavformat parsed
/// for a cover-art stream — as this track's one attachment packet.
///
/// A stream that declares cover art but parks no payload still gets a
/// packet: an empty one, marked `synthesized`, because the contract is
/// one packet per attachment track and a consumer that sees an empty
/// payload learns something true about the file. The alternative shipped
/// once — waiting for the payload to arrive as a packet later — and it
/// cannot hold: nothing stops a timed packet, or a seek, from coming
/// first, so the track's packet would arrive out of order or never.
///
/// Measured before it was written: across MP3 (ID3 APIC), M4A (`covr`),
/// FLAC (`METADATA_BLOCK_PICTURE`) and Matroska (an `image/*`
/// attachment), every stream libavformat gives
/// `AV_DISPOSITION_ATTACHED_PIC` also carries the parked packet —
/// `ff_add_attached_pic` sets the disposition and fills
/// `attached_pic` in the same breath. The empty case is the honest
/// answer to a state this build's demuxers do not produce, not a
/// fallback anything relies on.
///
/// # Safety
///
/// `pkt` must be a live `*const AVPacket` — in practice the
/// `attached_pic` embedded in the `AVStream` at `index` — for the
/// duration of this call.
unsafe fn attached_pic_payload<C: crate::FfmpegCarrier + crate::CarrierOps>(
  pkt: *const ffmpeg_next::ffi::AVPacket,
  index: usize,
  limits: DemuxLimits,
) -> Result<AttachmentPacket<AttachmentPacketExtra, C::Buffer>, DemuxError> {
  // Already admitted: [`admit_streams`] charged this payload — and
  // every other attachment in the file — before `build_tracks`
  // allocated anything. The per-attachment budget is passed down as
  // this packet's own ceiling anyway, so the funnel is guarded even if
  // a future caller reaches it without the admission pass.
  //
  // SAFETY: `pkt` is live per the contract above.
  // **The container's own cover art**, whose buffer libavformat also
  // holds — see [`crate::buffer::PayloadProvenance`] for why that
  // second reference is not the hazard a caller's second `Packet` is.
  let captured = unsafe {
    crate::buffer::payload_of::<C>(
      pkt,
      limits.max_attachment_bytes(),
      crate::buffer::PayloadProvenance::AttachedPicture,
    )
  }
  .map_err(|source| DemuxError::PacketBuffer(PacketBuffer::new(index, source)))?;
  let extra = AttachmentPacketExtra::new(index as i32);
  Ok(match captured {
    Some(payload) => {
      // The hoisted packet's own flags, through the same raw reader the
      // five boundary conversions use. FFmpeg marks an attached picture
      // `AV_PKT_FLAG_KEY` — a still image is a keyframe if anything is
      // — and building this one with empty flags dropped that, along
      // with `CORRUPT` and every other bit the packet really carried.
      // SAFETY: `pkt` points at the live embedded `AVPacket`.
      let flags = unsafe { boundary::md_flags_from_av_packet(pkt) }
        .map_err(|source| DemuxError::PacketBuffer(PacketBuffer::new(index, source)))?;
      AttachmentPacket::new(payload, extra).with_flags(flags)
    }
    // Nothing was parked, so there are no flags to read: an empty set
    // is the honest answer for a packet this layer invented.
    None => AttachmentPacket::new(C::empty(), extra.with_synthesized(true)),
  })
}

/// Builds an attachment payload out of a track's codec extradata — the
/// only place a font's bytes ever live, since an
/// `AVMEDIA_TYPE_ATTACHMENT` stream produces no packets at all.
///
/// A track with no extradata still gets a packet, with an empty
/// payload: the contract is one packet per attachment track, and a
/// consumer that sees an empty one learns something true about the
/// file. Only an allocation failure is an error.
fn extradata_payload<C: crate::FfmpegCarrier + crate::CarrierOps>(
  stream: &ffmpeg_next::format::stream::Stream<'_>,
  limits: DemuxLimits,
) -> Result<AttachmentPacket<AttachmentPacketExtra, C::Buffer>, DemuxError> {
  let index = stream.index();
  let parameters = stream.parameters();
  // SAFETY: `parameters` keeps the `AVCodecParameters` live;
  // `extradata` / `extradata_size` are public fields.
  let par = unsafe { parameters.as_ptr() };
  let ptr = unsafe { (*par).extradata };
  let len = unsafe { (*par).extradata_size }.max(0) as usize;
  // Already admitted, exactly as on the hoisted cover-art path — see
  // [`admit_streams`]. Re-judged here against the per-attachment
  // ceiling alone, so the helper is safe to call on its own.
  if len > limits.max_attachment_bytes() {
    return Err(DemuxError::AttachmentTooLarge(AttachmentTooLarge::new(
      index,
      len,
      limits.max_attachment_bytes(),
    )));
  }
  let bytes: &[u8] = if ptr.is_null() || len == 0 {
    &[]
  } else {
    // SAFETY: libavformat guarantees `extradata` is readable for
    // `extradata_size` bytes (plus its padding) while the parameters
    // live, and the slice is consumed before this function returns.
    unsafe { std::slice::from_raw_parts(ptr, len) }
  };
  // Extradata is a plain allocation with no `AVBufferRef` behind it —
  // an `AVMEDIA_TYPE_ATTACHMENT` stream produces no packets, so a
  // font's bytes never live in a refcounted buffer. **Both** lanes copy
  // here, which is what `from_bytes` is for.
  Ok(AttachmentPacket::new(
    C::from_bytes(bytes).ok_or_else(|| {
      DemuxError::PacketBuffer(PacketBuffer::new(
        index,
        crate::buffer::PacketBufferError::CaptureFailed(crate::buffer::CaptureFailed::new(len)),
      ))
    })?,
    AttachmentPacketExtra::new(index as i32).with_synthesized(true),
  ))
}

/// **The admission pass**: judges every stream in the file before the
/// track table allocates anything at all.
///
/// # Why this cannot live inside the capture
///
/// It used to, and that was a bypass. `build_tracks` deep-copies each
/// stream's `AVCodecParameters` on its way to building a `TrackExtra`,
/// and for an `AVMEDIA_TYPE_ATTACHMENT` stream **the extradata inside
/// those parameters is the attachment's payload**. So the loop paid for
/// the payload — a full `avcodec_parameters_copy` — one statement
/// before asking whether it was allowed to. A file declaring a gigabyte
/// of "font" allocated the gigabyte and then reported that a gigabyte
/// was too much.
///
/// The fix is not a check moved a few lines earlier: any per-track
/// interleaving of judging and paying has the same shape, because the
/// aggregate budget is only knowable once every track has been *seen*.
/// So the whole file is admitted here, in a pass that allocates
/// nothing — it reads two integers per stream — and only a container
/// that passes in full reaches the loop that builds carriers and
/// parameter copies.
///
/// # Why it is every stream, not every attachment
///
/// Because the track table copies **every** stream's codec parameters,
/// and `AVCodecParameters` reaches the heap three ways — `extradata`,
/// every `coded_side_data` entry, a custom channel map — all of them
/// sized by the file. A pass that walked only attachment streams left
/// the other road wide open: a MOV puts an ICC profile in
/// `coded_side_data`, on an ordinary video track, and the wholesale
/// copy took it before anything asked how big it was. That was the same
/// class of defect three review rounds running, which is why the copy
/// itself is gone (see
/// [`bounded_clone_parameters`](crate::extras::bounded_clone_parameters))
/// and why this pass sees everything.
///
/// # What is charged
///
/// The bytes this session will **retain**, which is not always the
/// declared size:
///
/// - every stream is charged its parameter clone's footprint against
///   the per-stream and whole-file codec-parameter budgets;
/// - a synthesized attachment's `extradata` is charged to the
///   *attachment* budget and left out of the parameter one, because the
///   clone strips it and the carrier holds it — one set of bytes, one
///   charge;
/// - the attachment budgets then see:
///
/// - a hoisted cover-art track retains its parked `AVPacket`'s payload
///   *and* the extradata in its parameter copy, which the still decoder
///   may need and which is not a duplicate of the payload;
/// - a synthesized `AVMEDIA_TYPE_ATTACHMENT` track retains only the
///   carrier, because `build_tracks` strips the duplicate extradata out
///   of the parameter copy (see the comment there for the census).
///
/// Charging residency rather than payload is what keeps the budget an
/// honest statement about memory instead of about file structure.
///
/// The per-attachment ceiling is judged first for each track: when a
/// single payload is itself over the line, that is the more specific
/// fact, and naming the aggregate instead would send a reader looking
/// for four hundred attachments that are not there.
fn admit_streams(input: &Input, limits: DemuxLimits) -> Result<(), DemuxError> {
  let mut attachment_spent: usize = 0;
  let mut parameter_spent: usize = 0;

  for stream in input.streams() {
    let index = stream.index();
    let parameters = stream.parameters();
    // SAFETY: `parameters` keeps the `AVCodecParameters` live for this
    // measurement, which allocates nothing and dereferences only what
    // it counts.
    let par = unsafe { parameters.as_ptr() };
    if par.is_null() {
      return Err(DemuxError::ParametersMissing(ParametersMissing::new(index)));
    }
    let footprint =
      unsafe { crate::extras::measure_parameters(par) }.ok_or(DemuxError::ParametersTooLarge(
        ParametersTooLarge::new(index, usize::MAX, limits.max_codec_parameter_bytes()),
      ))?;

    // SAFETY: `stream` keeps the `AVStream` live; `disposition` is a
    // public field.
    let disposition = unsafe { (*stream.as_ptr()).disposition };
    let cover_art = is_attachment_disposition(disposition);
    let synthesized = !cover_art && boundary::media_kind_of(&parameters).is_attachment();

    // What the *parameter clone* will retain for this stream. The
    // synthesized-attachment road strips `extradata` — the font's
    // payload rides the carrier instead — so counting it here would
    // charge the same bytes twice and make the budget a statement about
    // the file rather than about memory.
    let retained_parameters = if synthesized {
      footprint.total_without_extradata()
    } else {
      footprint.total()
    }
    .ok_or(DemuxError::ParametersTooLarge(ParametersTooLarge::new(
      index,
      usize::MAX,
      limits.max_codec_parameter_bytes(),
    )))?;

    if retained_parameters > limits.max_codec_parameter_bytes() {
      return Err(DemuxError::ParametersTooLarge(ParametersTooLarge::new(
        index,
        retained_parameters,
        limits.max_codec_parameter_bytes(),
      )));
    }
    parameter_spent = parameter_spent.saturating_add(retained_parameters);
    if parameter_spent > limits.max_total_codec_parameter_bytes() {
      return Err(DemuxError::ParametersBudgetExhausted(
        ParametersBudgetExhausted::new(
          index,
          parameter_spent,
          limits.max_total_codec_parameter_bytes(),
        ),
      ));
    }

    // And what the *carrier* will hold, for the two attachment roads.
    let carrier = if cover_art {
      // SAFETY: `attached_pic` is an `AVPacket` embedded in the
      // `AVStream` by value; `addr_of!` reaches its `size` without
      // forming a reference to the stream.
      unsafe {
        let pkt = std::ptr::addr_of!((*stream.as_ptr()).attached_pic);
        (*pkt).size
      }
      .max(0) as usize
    } else if synthesized {
      // The **payload**, not the padded clone figure. The carrier is
      // an `FfmpegBytes` over exactly these bytes and the clone omits
      // extradata entirely on this road, so nothing here allocates the
      // padding — charging it would bill sixty-four bytes that are
      // never spent, reject a payload in the last sixty-four below the
      // ceiling, and disagree with the image road about the same file
      // at exactly the cap.
      footprint.extradata_payload()
    } else {
      // Not an attachment: nothing is captured eagerly for it, so
      // nothing more is charged.
      continue;
    };

    charge_attachment(index, carrier, limits, &mut attachment_spent)?;
  }
  Ok(())
}

/// Charges `declared` bytes against both attachment budgets, refusing
/// before anything is copied. The one place a file's attachment
/// spending is decided; see [`admit_streams`] for when it runs.
fn charge_attachment(
  index: usize,
  declared: usize,
  limits: DemuxLimits,
  spent: &mut usize,
) -> Result<(), DemuxError> {
  if declared > limits.max_attachment_bytes() {
    return Err(DemuxError::AttachmentTooLarge(AttachmentTooLarge::new(
      index,
      declared,
      limits.max_attachment_bytes(),
    )));
  }
  let total = spent.saturating_add(declared);
  if total > limits.max_total_attachment_bytes() {
    return Err(DemuxError::AttachmentBudgetExhausted(
      AttachmentBudgetExhausted::new(index, total, limits.max_total_attachment_bytes()),
    ));
  }
  *spent = total;
  Ok(())
}

/// A stream's `AVRational` timebase as a [`Timebase`]. A zero or
/// negative denominator is clamped to 1 rather than refused: a
/// malformed timebase makes the track's timestamps meaningless, not the
/// file unreadable, and every other track still demuxes.
fn rational_to_timebase(value: Rational) -> Timebase {
  Timebase::new(
    value.numerator(),
    NonZeroI32::new(value.denominator().max(1)).expect("clamped to at least 1"),
  )
}

/// A frame *rate* as a rate-shaped [`Timebase`] (`30000/1001` for
/// 29.97 fps), or `None` when the container declares none.
fn rate_to_timebase(value: Rational) -> Option<Timebase> {
  let (num, den) = (value.numerator(), value.denominator());
  (num > 0 && den > 0).then(|| Timebase::new(num, NonZeroI32::new(den).expect("checked above")))
}

#[cfg(test)]
mod tests {
  use ffmpeg_next::ffi::{av_dict_free, av_dict_set};

  use ffmpeg_next::codec::Parameters;

  use super::*;
  use crate::extras::TrackExtra;

  /// Builds a dictionary holding one entry whose *value* is the given
  /// raw bytes. The bytes go in as a C string, which is all
  /// `av_dict_set` promises to copy — FFmpeg never asks whether they
  /// are UTF-8, which is the whole point of the lane below.
  fn dict_with(key: &CStr, value: &[u8]) -> *mut AVDictionary {
    let mut dict: *mut AVDictionary = std::ptr::null_mut();
    let mut terminated = value.to_vec();
    terminated.push(0);
    let rc = unsafe {
      av_dict_set(
        &mut dict,
        key.as_ptr(),
        terminated.as_ptr().cast::<std::ffi::c_char>(),
        0,
      )
    };
    assert!(rc >= 0, "av_dict_set failed: {rc}");
    dict
  }

  #[test]
  fn metadata_that_is_not_utf8_is_read_lossily_not_unsoundly() {
    // The bytes a real container can hold: a Latin-1 "café.ttf" whose
    // 0xE9 is not valid UTF-8 on its own. Read through
    // `DictionaryRef::get` this produced a `&str` that violates the
    // type's invariant — undefined behaviour before `SmolStr` ever
    // copied it.
    let raw = b"caf\xE9.ttf".to_vec();
    assert!(
      std::str::from_utf8(&raw).is_err(),
      "the source bytes really are not UTF-8",
    );
    let dict = dict_with(c"filename", &raw);
    let text = unsafe { metadata_text(dict, c"filename") }.expect("the entry exists");
    assert_eq!(text.as_str(), "caf\u{FFFD}.ttf");
    // A key the dictionary does not hold, and a null dictionary, are
    // both simply absent.
    assert_eq!(unsafe { metadata_text(dict, c"mimetype") }, None);
    assert_eq!(
      unsafe { metadata_text(std::ptr::null(), c"filename") },
      None
    );
    unsafe { av_dict_free(&mut { dict }) };
  }

  #[test]
  fn valid_metadata_survives_unchanged() {
    let dict = dict_with(c"mimetype", b"application/x-truetype-font");
    assert_eq!(
      unsafe { metadata_text(dict, c"mimetype") }.as_deref(),
      Some("application/x-truetype-font"),
    );
    unsafe { av_dict_free(&mut { dict }) };
  }

  #[test]
  fn an_unterminated_length_is_refused_rather_than_truncated() {
    // Nothing libavutil produces is this long; the cap exists so a
    // value it did not terminate cannot walk off the end. A value that
    // reaches the cap is absent, never a prefix of itself.
    let long = vec![b'a'; METADATA_VALUE_MAX_BYTES + 1];
    let dict = dict_with(c"filename", &long);
    assert_eq!(unsafe { metadata_text(dict, c"filename") }, None);
    unsafe { av_dict_free(&mut { dict }) };
  }

  /// A reader that panics with a payload whose destructor panics in
  /// turn. Both panics are safe code; the second one is what used to
  /// leave the guard and enter the `extern "C"` AVIO callback.
  struct PanicsWithAHostilePayload;

  struct PanicOnDrop;

  impl Drop for PanicOnDrop {
    fn drop(&mut self) {
      panic!("and the payload went too");
    }
  }

  impl std::io::Read for PanicsWithAHostilePayload {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
      std::panic::panic_any(PanicOnDrop);
    }
  }

  impl std::io::Seek for PanicsWithAHostilePayload {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
      std::panic::panic_any(PanicOnDrop);
    }
  }

  #[test]
  fn a_reader_panic_with_a_hostile_payload_does_not_abort_the_process() {
    // In its own process, because the assertion *is* the process: a
    // parent that sees the child exit cleanly has seen the abort not
    // happen. The guard caught the reader's panic and then dropped its
    // payload outside `catch_unwind`, so a payload whose `Drop` panics
    // sent that second panic straight out of `read` and into C —
    // through the very guard that exists to stop it.
    crate::fault_subprocess::in_subprocess(
      "demuxer::tests::a_reader_panic_with_a_hostile_payload_does_not_abort_the_process",
      || {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let opened =
          CarrierDemuxer::<crate::Owned>::open_reader(PanicsWithAHostilePayload, Some("x.mkv"));
        std::panic::set_hook(previous);
        match opened {
          Err(DemuxError::ReaderPanic(_)) => {}
          Err(other) => panic!("expected ReaderPanic, got {other:?}"),
          Ok(_) => panic!("a reader that only panics cannot open a container"),
        }
      },
    );
  }

  #[test]
  fn codec_parameters_that_cannot_be_allocated_are_named() {
    // `Parameters::new` does not check `avcodec_parameters_alloc`, and
    // `clone_from` dereferences the result immediately: under a failed
    // allocation the shipped clone would write through null.
    crate::fault_subprocess::in_subprocess(
      "demuxer::tests::codec_parameters_that_cannot_be_allocated_are_named",
      || {
        let source = Parameters::new();
        assert!(
          !unsafe { source.as_ptr() }.is_null(),
          "the source allocates before the cap goes on",
        );
        crate::fault_subprocess::cap_ffmpeg_allocations(1);
        let refused = crate::extras::bounded_clone_parameters(&source, 4, usize::MAX);
        crate::fault_subprocess::uncap_ffmpeg_allocations();
        assert!(
          matches!(
            refused,
            Err(DemuxError::ParametersAlloc(ref p)) if p.stream_index() == 4
          ),
          "expected ParametersAlloc, got {:?}",
          refused.map(|_| ()),
        );
        // And with the cap lifted the same copy succeeds, so the
        // refusal was the allocator's answer and not a broken helper.
        crate::extras::bounded_clone_parameters(&source, 4, usize::MAX).expect("an uncapped copy");
      },
    );
  }

  #[test]
  fn the_public_track_extra_copies_are_checked_too() {
    // The helper protected `build_tracks` and nothing else: `TrackExtra`
    // derived `Clone` and `Default` over `ffmpeg_next`'s `Parameters`,
    // whose clone dereferences an unchecked allocation — so safe public
    // code could still reach the SIGSEGV by copying a track row. The
    // derives are gone; what replaces them answers.
    crate::fault_subprocess::in_subprocess(
      "demuxer::tests::the_public_track_extra_copies_are_checked_too",
      || {
        let source = Parameters::new();
        assert!(!unsafe { source.as_ptr() }.is_null(), "allocated uncapped");
        let extra = TrackExtra::new(
          6,
          crate::extras::bounded_clone_parameters(&source, 6, usize::MAX).expect("uncapped"),
        )
        .expect("real parameters");

        crate::fault_subprocess::cap_ffmpeg_allocations(1);
        let cloned = extra.try_clone().map(|_| ());
        let handed = extra.clone_parameters().map(|_| ());
        crate::fault_subprocess::uncap_ffmpeg_allocations();

        assert!(
          matches!(cloned, Err(DemuxError::ParametersAlloc(ref p)) if p.stream_index() == 6),
          "TrackExtra::try_clone: {cloned:?}",
        );
        assert!(
          matches!(handed, Err(DemuxError::ParametersAlloc(ref p)) if p.stream_index() == 6),
          "TrackExtra::clone_parameters: {handed:?}",
        );

        // And both work once the allocator does.
        extra.try_clone().expect("an uncapped row copy");
        extra.clone_parameters().expect("an uncapped handoff");
      },
    );
  }

  #[test]
  fn parameters_that_never_allocated_are_refused_at_the_door() {
    // The route the destination check could not see. A safe
    // `Parameters::new()` under a failed allocation hands back a
    // null-backed value and says nothing; the copier then allocated its
    // own destination happily — the allocator having recovered by
    // then — and called `avcodec_parameters_copy(out, NULL)`, which
    // dereferences its source. Same crash, one recovery later, still
    // from safe public code.
    crate::fault_subprocess::in_subprocess(
      "demuxer::tests::parameters_that_never_allocated_are_refused_at_the_door",
      || {
        // The cap is on *while the source is built* — that is the whole
        // difference from the destination lane.
        crate::fault_subprocess::cap_ffmpeg_allocations(1);
        let never_allocated = Parameters::new();
        crate::fault_subprocess::uncap_ffmpeg_allocations();
        assert!(
          unsafe { never_allocated.as_ptr() }.is_null(),
          "the safe constructor really does hand back a null-backed value",
        );

        // The door: a `TrackExtra` cannot exist over it, so the copy
        // methods have nothing to be asked on.
        let refused = TrackExtra::new(9, never_allocated);
        let Err(DemuxError::ParametersMissing(p)) = refused.map(|_| ()) else {
          panic!("a null-backed source must not become a track row");
        };
        assert_eq!(p.stream_index(), 9);

        // And the copier refuses it too, so the invariant is not the
        // only thing standing between this and a null dereference.
        let never_allocated = {
          crate::fault_subprocess::cap_ffmpeg_allocations(1);
          let p = Parameters::new();
          crate::fault_subprocess::uncap_ffmpeg_allocations();
          p
        };
        assert!(matches!(
          crate::extras::bounded_clone_parameters(&never_allocated, 9, usize::MAX).map(|_| ()),
          Err(DemuxError::ParametersMissing(p)) if p.stream_index() == 9,
        ));

        // A row built over real parameters still copies both ways, so
        // the refusal is about the null and nothing else.
        let real = Parameters::new();
        let extra = TrackExtra::new(9, real).expect("real parameters");
        extra.try_clone().expect("row copy");
        extra.clone_parameters().expect("handoff");
      },
    );
  }

  #[cfg(feature = "resample")]
  #[test]
  fn a_spec_read_from_parameters_that_never_allocated_is_absent() {
    // The same trap at another public door, found by the sweep:
    // `ResampleSpec::from_parameters` asks `parameters.medium()`
    // first, and *that* dereferences the pointer inside ffmpeg-next
    // before any code of ours runs.
    crate::fault_subprocess::in_subprocess(
      "demuxer::tests::a_spec_read_from_parameters_that_never_allocated_is_absent",
      || {
        crate::fault_subprocess::cap_ffmpeg_allocations(1);
        let never_allocated = Parameters::new();
        crate::fault_subprocess::uncap_ffmpeg_allocations();
        assert!(unsafe { never_allocated.as_ptr() }.is_null());
        assert_eq!(
          crate::ResampleSpec::from_parameters(&never_allocated),
          None,
          "parameters that do not exist describe no audio",
        );
      },
    );
  }

  #[test]
  fn codec_parameters_whose_copy_fails_are_named() {
    // The other leg: the destination allocates, and the deep copy of
    // the extradata does not. `clone_from` discards that return value,
    // so the shipped clone handed back parameters missing the very
    // bytes a decoder needs to open — and said nothing.
    crate::fault_subprocess::in_subprocess(
      "demuxer::tests::codec_parameters_whose_copy_fails_are_named",
      || {
        const EXTRADATA: usize = 8 * 1024 * 1024;
        let mut source = Parameters::new();
        // SAFETY: `source` owns a live `AVCodecParameters`; the buffer
        // comes from FFmpeg's allocator and is handed to it, so
        // `avcodec_parameters_free` releases it with the rest.
        unsafe {
          let par = source.as_mut_ptr();
          let extradata = ffmpeg_next::ffi::av_mallocz(EXTRADATA) as *mut u8;
          assert!(!extradata.is_null(), "av_mallocz");
          (*par).extradata = extradata;
          (*par).extradata_size = EXTRADATA as i32;
        }

        // Big enough for the destination `AVCodecParameters`, far too
        // small for its extradata.
        crate::fault_subprocess::cap_ffmpeg_allocations(64 * 1024);
        let refused = crate::extras::bounded_clone_parameters(&source, 2, usize::MAX);
        crate::fault_subprocess::uncap_ffmpeg_allocations();
        match refused {
          Err(DemuxError::ParametersCopy(p)) => assert_eq!(p.stream_index(), 2),
          Err(other) => panic!("expected ParametersCopy, got {other:?}"),
          Ok(_) => panic!("a copy that could not copy the extradata must not succeed"),
        }
        crate::extras::bounded_clone_parameters(&source, 2, usize::MAX).expect("an uncapped copy");
      },
    );
  }

  /// A stream whose `attached_pic` is `parked`, and the packet
  /// libavformat would queue for it.
  ///
  /// # On the fixture road
  ///
  /// The container shape this guards — a stream carrying
  /// `ATTACHED_PIC | TIMED_THUMBNAILS` — **cannot be minted by the
  /// ffmpeg CLI**, and that was censused rather than assumed: no muxer
  /// has a field for those bits (`-disposition:v
  /// attached_pic+timed_thumbnails` round-trips to nothing through
  /// mp4, mov and matroska alike), because the mov *demuxer* derives
  /// them from a chapter-track reference its own muxer does not write
  /// in that direction.
  ///
  /// What is reproducible, and what actually matters, is the **packet
  /// shape**: `read_frame_internal` queues a stream's parked picture
  /// with `av_packet_ref` while keeping its own reference, which is
  /// exactly what `av_packet_ref` builds here. The classification half
  /// — that such a stream is video rather than an attachment — is
  /// pinned separately by
  /// [`a_timed_thumbnail_stream_is_not_an_attachment`].
  fn parked_picture_stream(parked: &Packet) -> (Box<AVStream>, Packet) {
    use ffmpeg_next::packet::{Mut, Ref};

    let mut stream: Box<AVStream> = Box::new(unsafe { std::mem::zeroed() });
    let mut queued = Packet::empty();
    // SAFETY: `parked` is a live refcounted packet; `av_packet_ref`
    // takes a reference to its buffer, which is precisely what
    // libavformat does when it queues an attached picture. The stream
    // is zeroed apart from the one field the probe reads.
    unsafe {
      assert_eq!(
        ffmpeg_next::ffi::av_packet_ref(queued.as_mut_ptr(), parked.as_ptr()),
        0,
      );
      stream.attached_pic.buf = (*parked.as_ptr()).buf;
      stream.attached_pic.data = (*parked.as_ptr()).data;
      stream.attached_pic.size = (*parked.as_ptr()).size;
    }
    (stream, queued)
  }

  #[test]
  fn a_queued_attached_picture_is_recognised() {
    use ffmpeg_next::packet::Ref;

    let parked = Packet::copy(&[9u8; 2048]);
    let (stream, queued) = parked_picture_stream(&parked);

    // The two references are different structs around one allocation —
    // which is the whole reason the probe compares `buffer` and not the
    // `AVBufferRef`. Asserting the difference is what makes this a test
    // of the right comparison rather than of a lucky one.
    // SAFETY: both packets are live.
    unsafe {
      assert_ne!(
        (*queued.as_ptr()).buf,
        (*parked.as_ptr()).buf,
        "av_packet_ref must mint a new reference struct",
      );
    }
    // SAFETY: the stream is a zeroed `AVStream` whose only populated
    // fields are the ones the probe reads, and `queued` is live.
    assert!(unsafe { packet_is_parked_picture(&*stream, &queued) });

    // An ordinary timed packet — the shape every pull after the first
    // one has — is not the parked picture.
    let ordinary = Packet::copy(&[1u8; 2048]);
    // SAFETY: as above.
    assert!(!unsafe { packet_is_parked_picture(&*stream, &ordinary) });

    // And a stream that parks nothing recognises nothing.
    let bare: Box<AVStream> = Box::new(unsafe { std::mem::zeroed() });
    // SAFETY: as above.
    assert!(!unsafe { packet_is_parked_picture(&*bare, &queued) });
  }

  #[test]
  fn the_queued_picture_is_admitted_and_later_packets_take_the_ordinary_road() {
    use crate::buffer::{PacketBufferError, PayloadProvenance, payload_of};
    use ffmpeg_next::packet::Ref;

    let parked = Packet::copy(&[9u8; 2048]);
    let (_stream, queued) = parked_picture_stream(&parked);
    // SAFETY: the packet is live; `buf` is a public field.
    let parked_buffer = unsafe { (*parked.as_ptr()).buf };

    // **The first pull.** Two references, one of them the container's.
    // From a *caller* that shape is refused, because a caller's second
    // reference may be a `Packet` with a safe `data_mut`.
    // SAFETY: `queued` is live for every call in this test.
    assert!(matches!(
      unsafe {
        payload_of::<crate::View>(
          queued.as_ptr(),
          usize::MAX,
          PayloadProvenance::CallerSupplied,
        )
      },
      Err(PacketBufferError::SharedPayload(_)),
    ));

    // Delivered by the demux loop, the same shape is carried — by copy,
    // because a window would outlive the exclusivity the read rests on.
    // SAFETY: as above.
    let copied = unsafe {
      payload_of::<crate::View>(
        queued.as_ptr(),
        usize::MAX,
        PayloadProvenance::DemuxDelivered,
      )
    }
    .expect("a demux-delivered shared payload is carriable")
    .expect("it has a payload");
    assert_eq!(copied.as_ref(), &[9u8; 2048][..]);
    // SAFETY: the packet is live; `data` is a public field.
    unsafe {
      assert_ne!(
        copied.as_ref().as_ptr() as usize,
        (*queued.as_ptr()).data as usize,
        "a shared demux-delivered payload is copied, not windowed",
      );
    }

    // With the provenance the probe establishes, both lanes carry it.
    // SAFETY: as above.
    let viewed = unsafe {
      payload_of::<crate::View>(
        queued.as_ptr(),
        usize::MAX,
        PayloadProvenance::AttachedPicture,
      )
    }
    .expect("the container's own picture is carriable")
    .expect("it has a payload");
    assert_eq!(viewed.as_ref(), &[9u8; 2048][..]);
    // And on the view lane it is a window into the parked allocation
    // rather than a copy of it.
    // SAFETY: both are live; `data`/`size` are public fields.
    unsafe {
      let start = (*parked_buffer).data as usize;
      let end = start + (*parked_buffer).size;
      let at = viewed.as_ref().as_ptr() as usize;
      assert!(
        at >= start && at + viewed.len() <= end,
        "the queued picture must be viewed, not copied",
      );
    }
    // SAFETY: as above.
    let owned = unsafe {
      payload_of::<crate::Owned>(
        queued.as_ptr(),
        usize::MAX,
        PayloadProvenance::AttachedPicture,
      )
    }
    .expect("the owned lane carries it too")
    .expect("it has a payload");
    assert_eq!(owned.as_ref(), &[9u8; 2048][..]);

    // **Every pull after it.** A timed packet has a buffer of its own,
    // so it stays on the `Delivered` road, is unique, and the view lane
    // shares it.
    let later = Packet::copy(&[4u8; 1024]);
    // SAFETY: `later` is live.
    let shared = unsafe {
      payload_of::<crate::View>(
        later.as_ptr(),
        usize::MAX,
        PayloadProvenance::DemuxDelivered,
      )
    }
    .expect("an ordinary packet is carriable")
    .expect("it has a payload");
    // SAFETY: as above.
    unsafe {
      assert_eq!(
        shared.as_ref().as_ptr() as usize,
        (*later.as_ptr()).data as usize,
        "a uniquely-referenced packet is still shared, not copied",
      );
    }
  }

  #[test]
  fn a_timed_thumbnail_stream_is_not_an_attachment() {
    // `TIMED_THUMBNAILS` is documented as only ever appearing beside
    // `ATTACHED_PIC`, so testing the picture bit alone reads a sparse
    // chapter-thumbnail track as cover art — and the attachment
    // contract then delivers exactly one of its images and drops the
    // rest, every one of which had a timestamp.
    assert!(
      is_attachment_disposition(AV_DISPOSITION_ATTACHED_PIC),
      "a plain attached picture is still an attachment",
    );
    assert!(
      !is_attachment_disposition(AV_DISPOSITION_ATTACHED_PIC | AV_DISPOSITION_TIMED_THUMBNAILS),
      "a timed-thumbnail stream is a timed track, whatever else it is flagged",
    );
    // Neither bit, and the other bits that ride along, change nothing.
    assert!(!is_attachment_disposition(0));
    assert!(!is_attachment_disposition(AV_DISPOSITION_TIMED_THUMBNAILS));
    assert!(is_attachment_disposition(
      AV_DISPOSITION_ATTACHED_PIC | ffmpeg_next::ffi::AV_DISPOSITION_DEFAULT
    ));
    // And the reason the raw bits are read at all: the wrapper's own
    // flag set cannot express the distinction.
    assert!(
      ffmpeg_next::format::stream::Disposition::from_bits(AV_DISPOSITION_TIMED_THUMBNAILS)
        .is_none(),
      "ffmpeg_next mints no TIMED_THUMBNAILS bit — from_bits_truncate would drop it silently",
    );
  }

  #[test]
  fn an_uncapturable_cover_still_gets_its_one_packet() {
    // The state the shipped `AwaitingPacket` fallback existed for: a
    // stream that declares cover art and parks no payload. The fallback
    // waited for a packet that may never come, and let timed packets —
    // and seeks — go first, which the face forbids. The track now gets
    // its one packet at open like every other attachment track: empty,
    // and marked as this layer's own work.
    //
    // Not reachable from a file: across MP3, M4A, FLAC and Matroska,
    // every ATTACHED_PIC stream libavformat produces carries the parked
    // packet, because `ff_add_attached_pic` sets the disposition and
    // fills it in the same call. A zeroed `AVPacket` is exactly what
    // `attached_pic` would hold if one ever did not.
    let empty: ffmpeg_next::ffi::AVPacket = unsafe { std::mem::zeroed() };
    let packet = unsafe { attached_pic_payload::<crate::Owned>(&empty, 7, DemuxLimits::default()) }
      .expect("an unparked cover is a degenerate track, not an unreadable file");
    assert!(packet.data().as_ref().is_empty());
    assert!(
      packet.extra().synthesized(),
      "nothing in the container handed this payload over",
    );
    assert_eq!(packet.extra().stream_index(), 7);
  }

  #[test]
  fn a_zero_denominator_timebase_is_clamped_not_refused() {
    // A malformed timebase makes one track's timestamps meaningless.
    // It must not make the file unreadable — every other track still
    // demuxes, and the caller can see the 1/1 for what it is.
    let tb = rational_to_timebase(Rational::new(1, 0));
    assert_eq!(tb.den().get(), 1);
    assert_eq!(tb.num(), 1);
  }

  #[test]
  fn a_declared_frame_rate_becomes_a_rate_shaped_timebase() {
    let ntsc = rate_to_timebase(Rational::new(30_000, 1001)).expect("declared");
    assert_eq!((ntsc.num(), ntsc.den().get()), (30_000, 1001));
    assert_eq!(
      rate_to_timebase(Rational::new(0, 1)),
      None,
      "0 fps is absent"
    );
    assert_eq!(
      rate_to_timebase(Rational::new(30, 0)),
      None,
      "no denominator"
    );
  }

  #[test]
  fn the_seek_timebase_is_microseconds() {
    // `avformat_seek_file` with `stream_index == -1` takes AV_TIME_BASE
    // units; a target expressed in anything else has to arrive there.
    let tb = av_time_base_q();
    assert_eq!((tb.num(), tb.den().get()), (1, 1_000_000));
    let target = Timestamp::new(1_500, Timebase::new(1, NonZeroI32::new(1000).expect("ms")));
    assert_eq!(target.rescale_to(tb).pts(), 1_500_000);
  }
}

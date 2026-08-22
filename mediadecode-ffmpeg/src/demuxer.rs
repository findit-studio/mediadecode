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
  ffi::CStr,
  io::{Read, Seek},
  num::NonZeroI32,
  path::Path,
  ptr::{addr_of, read_unaligned},
  sync::Arc,
};

use ffmpeg_next::{
  Packet, Rational,
  ffi::{AV_NOPTS_VALUE, AVDictionary, av_dict_get},
  format::{self, context::Input, stream::Disposition},
  media,
};
use mediadecode::{
  Timebase, Timestamp,
  demuxer::{
    AttachmentPacket, DemuxedPacket, Demuxer, TrackIndex, TrackInfo, TrackKind, TrackParams,
  },
};
use smol_str::SmolStr;

use crate::{
  Ffmpeg, FfmpegBuffer, boundary,
  buffer::PacketBufferError,
  codec_id::CodecId,
  extras::{AttachmentPacketExtra, TrackExtra},
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
pub struct FfmpegDemuxer {
  input: Input,
  tracks: Vec<TrackInfo<Ffmpeg>>,
  pending: VecDeque<(
    TrackIndex,
    AttachmentPacket<AttachmentPacketExtra, FfmpegBuffer>,
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
}

impl FfmpegDemuxer {
  /// Opens a container from a filesystem path.
  ///
  /// Runs `avformat_open_input` followed by
  /// `avformat_find_stream_info`, then builds the track table and
  /// captures every attachment payload.
  ///
  /// Call [`ffmpeg_next::init`] once before the first open if you want
  /// FFmpeg's logging and network protocols configured; probing a local
  /// container does not require it.
  pub fn open<P: AsRef<Path> + ?Sized>(path: &P) -> Result<Self, DemuxError> {
    Self::from_input(format::input(path)?)
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
  pub fn open_reader<R: Read + Seek + Send + 'static>(
    reader: R,
    filename: Option<&str>,
  ) -> Result<Self, DemuxError> {
    let (guarded, latch) = GuardedReader::new(reader);
    let io = format::context::StreamIo::from_read_seek(guarded)?;
    let input = format::input_from_stream(io, filename, None)
      .map_err(|e| reader_panic(&latch).unwrap_or(DemuxError::Ffmpeg(e)))?;
    // A panic libavformat tolerated (a failed probe it recovered from)
    // still poisoned the reader; the session must not open over it.
    if let Some(panicked) = reader_panic(&latch) {
      return Err(panicked);
    }
    let mut demuxer = Self::from_input(input)?;
    demuxer.reader_panic = Some(latch);
    Ok(demuxer)
  }

  /// Borrows the wrapped `ffmpeg::format::context::Input` — for
  /// `av_dump_format`, container-level metadata, chapters, and anything
  /// else the portable track table has no seat for.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn input(&self) -> &Input {
    &self.input
  }

  fn from_input(input: Input) -> Result<Self, DemuxError> {
    let (tracks, pending) = build_tracks(&input)?;
    Ok(Self {
      input,
      tracks,
      pending,
      eof: false,
      reader_panic: None,
    })
  }

  /// The error a panicked reader owes this session, if one panicked.
  fn panicked(&self) -> Option<DemuxError> {
    self.reader_panic.as_deref().and_then(reader_panic)
  }
}

/// Names the stream a payload failure happened on. Shared by all five
/// delivery arms so the failure cannot be swallowed on one of them.
fn on_stream<T>(
  stream_index: usize,
  result: Result<Option<T>, PacketBufferError>,
) -> Result<Option<T>, DemuxError> {
  result.map_err(|source| DemuxError::PacketBuffer {
    stream_index,
    source,
  })
}

/// Turns a latched reader panic into the error that names it.
fn reader_panic(latch: &PanicLatch) -> Option<DemuxError> {
  latch
    .message()
    .map(|message| DemuxError::ReaderPanic { message })
}

impl Demuxer for FfmpegDemuxer {
  type Adapter = Ffmpeg;
  type Buffer = FfmpegBuffer;
  type Error = DemuxError;

  fn tracks(&self) -> &[TrackInfo<Ffmpeg>] {
    &self.tracks
  }

  fn next_packet(&mut self) -> Result<Option<DemuxedPacket<Ffmpeg, FfmpegBuffer>>, DemuxError> {
    // The attachment queue drains first and drains completely, which is
    // the whole of "exactly one packet, before any timed packet": no
    // `av_read_frame` has run yet when the last one leaves.
    if let Some((track, packet)) = self.pending.pop_front() {
      return Ok(Some(DemuxedPacket::Attachment { track, packet }));
    }

    loop {
      let mut packet = Packet::empty();
      let read = packet.read(&mut self.input);
      // A panicking reader reported an ordinary I/O error to C, and
      // libavformat may answer that with the error, with EOF (a stream
      // it cannot read looks finished), or with a packet it had already
      // buffered. None of those are the file's word, so the latch is
      // consulted whatever the outcome was.
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
        // `AVERROR_INVALIDDATA` is not latched into the `AVIOContext`,
        // so reading again makes progress. Every other error is sticky
        // and is surfaced.
        Err(ffmpeg_next::Error::InvalidData) => continue,
        Err(e) => return Err(DemuxError::Ffmpeg(e)),
      }

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
      let built = match info.kind() {
        TrackKind::Video => on_stream(
          index,
          boundary::video_packet_from_ffmpeg_in(&packet, time_base),
        )?
        .map(|packet| DemuxedPacket::Video { track, packet }),
        TrackKind::Audio => on_stream(
          index,
          boundary::audio_packet_from_ffmpeg_in(&packet, time_base),
        )?
        .map(|packet| DemuxedPacket::Audio { track, packet }),
        TrackKind::Subtitle => on_stream(
          index,
          boundary::subtitle_packet_from_ffmpeg_in(&packet, time_base),
        )?
        .map(|packet| DemuxedPacket::Subtitle { track, packet }),
        TrackKind::Data => on_stream(
          index,
          boundary::data_packet_from_ffmpeg_in(&packet, time_base),
        )?
        .map(|packet| DemuxedPacket::Data { track, packet }),
        // Every attachment track's one packet was queued at open time,
        // so anything arriving on one now is the duplicate some
        // demuxers emit for cover art. Drop it — the contract is
        // exactly one, and the one has already left.
        TrackKind::Attachment => continue,
        // The roster of arms is five; a track nothing can name has no
        // arm and its packets are not delivered.
        TrackKind::Unknown => continue,
      };

      // `None` here means the packet carried no payload — an empty
      // packet, which some demuxers emit as a marker. Nothing to
      // deliver; read the next one.
      if let Some(out) = built {
        return Ok(Some(out));
      }
    }
  }

  fn seek(&mut self, target: Timestamp) -> Result<(), DemuxError> {
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
    Ok(())
  }
}

/// Errors from [`FfmpegDemuxer`].
#[derive(thiserror::Error, Debug, Clone)]
pub enum DemuxError {
  /// The wrapped libavformat call reported an error — open, read or
  /// seek.
  #[error(transparent)]
  Ffmpeg(#[from] ffmpeg_next::Error),

  /// FFmpeg refused a buffer allocation while capturing an
  /// attachment's payload at open time.
  #[error("out of memory capturing the attachment payload for stream {stream_index}")]
  AttachmentAlloc {
    /// The `AVStream.index` whose payload could not be captured.
    stream_index: usize,
  },

  /// A packet's payload could not be referenced — the bytes are there
  /// and this layer could not carry them.
  ///
  /// Never raised for a packet that simply has no payload: an empty
  /// packet is a marker some demuxers emit, and it is skipped in
  /// silence. Distinguishing the two is what keeps a refcount failure
  /// under memory pressure from looking like the file's own word and
  /// dropping real compressed bytes.
  #[error("stream {stream_index}: {source}")]
  PacketBuffer {
    /// The `AVStream.index` the packet belongs to.
    stream_index: usize,
    /// What went wrong.
    #[source]
    source: PacketBufferError,
  },

  /// The `Read + Seek` source given to
  /// [`FfmpegDemuxer::open_reader`] panicked inside a libavformat
  /// callback.
  ///
  /// The panic was caught before it could cross the `extern "C"`
  /// boundary and abort the process; this is what it said. The session
  /// is terminal — every later call reports the same panic.
  #[error("the reader panicked: {message}")]
  ReaderPanic {
    /// What the panic payload said.
    message: SmolStr,
  },
}

// ---------------------------------------------------------------------------
//  Track-table construction.
// ---------------------------------------------------------------------------

type BuiltTracks = (
  Vec<TrackInfo<Ffmpeg>>,
  VecDeque<(
    TrackIndex,
    AttachmentPacket<AttachmentPacketExtra, FfmpegBuffer>,
  )>,
);

fn build_tracks(input: &Input) -> Result<BuiltTracks, DemuxError> {
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
    // UB the moment it exists. The medium goes through `Parameters`
    // (which does construct the enum, but only from
    // `AVMediaType`'s tiny, stable set) and the codec id is read as the
    // raw integer it is on the wire.
    let medium = parameters.medium();
    let codec =
      CodecId::from_raw(unsafe { read_unaligned(addr_of!((*par).codec_id).cast::<i32>()) });

    let disposition = unsafe { (*stream.as_ptr()).disposition };
    let attached_pic = stream.disposition().contains(Disposition::ATTACHED_PIC);

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
      TrackParams::Attachment { codec }
    } else {
      match medium {
        media::Type::Video => TrackParams::Video {
          codec,
          width: unsafe { (*par).width }.max(0) as u32,
          height: unsafe { (*par).height }.max(0) as u32,
          pixel_format: boundary::from_av_pixel_format(unsafe { (*par).format }),
          frame_rate: rate_to_timebase(stream.avg_frame_rate()),
        },
        media::Type::Audio => {
          let ch_layout = unsafe { std::ptr::addr_of!((*par).ch_layout) };
          // SAFETY: `par` is a live `*const AVCodecParameters` for the
          // life of `parameters`; the helper validates `order` as an
          // `i32` before constructing any `AVChannelOrder`.
          let channel_layout =
            unsafe { crate::channel_layout::channel_layout_description_from_raw_ptr(ch_layout) };
          TrackParams::Audio {
            codec,
            sample_rate: unsafe { (*par).sample_rate }.max(0) as u32,
            channel_count: channel_layout.channels().min(255) as u8,
            sample_format: SampleFormat::from_raw(unsafe { (*par).format }),
            channel_layout,
          }
        }
        media::Type::Subtitle => TrackParams::Subtitle { codec },
        media::Type::Data => TrackParams::Data { codec },
        media::Type::Attachment => TrackParams::Attachment { codec },
        media::Type::Unknown => TrackParams::Unknown { codec },
      }
    };

    let extra = TrackExtra::new(index as i32, parameters.clone())
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
        unsafe { attached_pic_payload(pkt, index) }?
      } else {
        extradata_payload(&stream)?
      };
      pending.push_back((TrackIndex::new(index), packet));
    }

    tracks.push(info);
  }

  Ok((tracks, pending))
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
unsafe fn attached_pic_payload(
  pkt: *const ffmpeg_next::ffi::AVPacket,
  index: usize,
) -> Result<AttachmentPacket<AttachmentPacketExtra, FfmpegBuffer>, DemuxError> {
  // SAFETY: `pkt` is live per the contract above.
  let captured =
    unsafe { crate::buffer::payload_of(pkt) }.map_err(|source| DemuxError::PacketBuffer {
      stream_index: index,
      source,
    })?;
  let extra = AttachmentPacketExtra::new(index as i32);
  Ok(match captured {
    Some(payload) => AttachmentPacket::new(payload, extra),
    None => AttachmentPacket::new(
      FfmpegBuffer::copy_from_slice(&[]).ok_or(DemuxError::AttachmentAlloc {
        stream_index: index,
      })?,
      extra.with_synthesized(true),
    ),
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
fn extradata_payload(
  stream: &ffmpeg_next::format::stream::Stream<'_>,
) -> Result<AttachmentPacket<AttachmentPacketExtra, FfmpegBuffer>, DemuxError> {
  let index = stream.index();
  let parameters = stream.parameters();
  // SAFETY: `parameters` keeps the `AVCodecParameters` live;
  // `extradata` / `extradata_size` are public fields.
  let par = unsafe { parameters.as_ptr() };
  let ptr = unsafe { (*par).extradata };
  let len = unsafe { (*par).extradata_size }.max(0) as usize;
  let bytes: &[u8] = if ptr.is_null() || len == 0 {
    &[]
  } else {
    // SAFETY: libavformat guarantees `extradata` is readable for
    // `extradata_size` bytes (plus its padding) while the parameters
    // live, and the slice is consumed before this function returns.
    unsafe { std::slice::from_raw_parts(ptr, len) }
  };
  let payload = FfmpegBuffer::copy_from_slice(bytes).ok_or(DemuxError::AttachmentAlloc {
    stream_index: index,
  })?;
  Ok(AttachmentPacket::new(
    payload,
    AttachmentPacketExtra::new(index as i32).with_synthesized(true),
  ))
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

  use super::*;

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
    let packet = unsafe { attached_pic_payload(&empty, 7) }
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

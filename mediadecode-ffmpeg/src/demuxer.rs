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
//! Both kinds are queued at open, which is what makes the face's
//! "exactly one packet, before any timed packet" true here: the queue
//! drains before the first `av_read_frame` call ever runs.
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
  io::{Read, Seek},
  num::NonZeroI32,
  path::Path,
  ptr::{addr_of, read_unaligned},
};

use ffmpeg_next::{
  Packet, Rational,
  ffi::AV_NOPTS_VALUE,
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
  codec_id::CodecId,
  extras::{AttachmentPacketExtra, TrackExtra},
  sample_format::SampleFormat,
};

/// One microsecond — the timebase `avformat_seek_file` expects when no
/// reference stream is named (`stream_index == -1`).
fn av_time_base_q() -> Timebase {
  Timebase::new(1, NonZeroI32::new(1_000_000).expect("1e6 is non-zero"))
}

/// What this layer still owes a given attachment track.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum AttachmentState {
  /// Not an attachment track.
  None,
  /// The payload was captured at open time; it is either queued or
  /// already handed out. Either way a packet arriving on this track is
  /// a duplicate and is dropped.
  Captured,
  /// The payload could not be captured at open time (the container
  /// declares the track but libavformat parked no `attached_pic` and
  /// the codec carries no extradata). The first packet that arrives on
  /// this track is the attachment; every later one is dropped.
  AwaitingPacket,
}

/// `mediadecode::demuxer::Demuxer` impl wrapping `ffmpeg::format::context::Input`.
///
/// Construction is deliberately not on the trait — see [`Self::open`]
/// and [`Self::open_reader`].
pub struct FfmpegDemuxer {
  input: Input,
  tracks: Vec<TrackInfo<Ffmpeg>>,
  attachments: Vec<AttachmentState>,
  pending: VecDeque<(
    TrackIndex,
    AttachmentPacket<AttachmentPacketExtra, FfmpegBuffer>,
  )>,
  /// `true` once this session has answered `Ok(None)`. Only then does
  /// [`Self::seek`] clear the `AVIOContext`'s EOF latch — clearing it
  /// unconditionally would also erase a genuine sticky I/O error, which
  /// `Input::seek` goes out of its way to preserve.
  eof: bool,
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
  pub fn open_reader<R: Read + Seek + Send + 'static>(
    reader: R,
    filename: Option<&str>,
  ) -> Result<Self, DemuxError> {
    let io = format::context::StreamIo::from_read_seek(reader)?;
    Self::from_input(format::input_from_stream(io, filename, None)?)
  }

  /// Borrows the wrapped `ffmpeg::format::context::Input` — for
  /// `av_dump_format`, container-level metadata, chapters, and anything
  /// else the portable track table has no seat for.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn input(&self) -> &Input {
    &self.input
  }

  fn from_input(input: Input) -> Result<Self, DemuxError> {
    let (tracks, attachments, pending) = build_tracks(&input)?;
    Ok(Self {
      input,
      tracks,
      attachments,
      pending,
      eof: false,
    })
  }
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
      match packet.read(&mut self.input) {
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

      let built = match info.kind() {
        TrackKind::Video => boundary::video_packet_from_ffmpeg_in(&packet, time_base)
          .map(|packet| DemuxedPacket::Video { track, packet }),
        TrackKind::Audio => boundary::audio_packet_from_ffmpeg_in(&packet, time_base)
          .map(|packet| DemuxedPacket::Audio { track, packet }),
        TrackKind::Subtitle => boundary::subtitle_packet_from_ffmpeg_in(&packet, time_base)
          .map(|packet| DemuxedPacket::Subtitle { track, packet }),
        TrackKind::Data => boundary::data_packet_from_ffmpeg_in(&packet, time_base)
          .map(|packet| DemuxedPacket::Data { track, packet }),
        TrackKind::Attachment => match self.attachments[index] {
          // Already captured at open time: this is the duplicate some
          // demuxers emit for cover art. Drop it — the contract is
          // exactly one.
          AttachmentState::Captured | AttachmentState::None => continue,
          AttachmentState::AwaitingPacket => {
            // The state moves only once a payload really came out. An
            // unwrappable packet (no refcounted buffer) leaves the
            // track still owed, so the next one on it is taken instead
            // of silently swallowed.
            boundary::attachment_packet_from_ffmpeg(&packet).map(|packet| {
              self.attachments[index] = AttachmentState::Captured;
              DemuxedPacket::Attachment { track, packet }
            })
          }
        },
        // The roster of arms is five; a track nothing can name has no
        // arm and its packets are not delivered.
        TrackKind::Unknown => continue,
      };

      // `None` here means the packet had no refcounted payload — an
      // empty packet, which some demuxers emit as a marker. Nothing to
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
    self.input.seek(ts, ..ts)?;
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
}

// ---------------------------------------------------------------------------
//  Track-table construction.
// ---------------------------------------------------------------------------

type BuiltTracks = (
  Vec<TrackInfo<Ffmpeg>>,
  Vec<AttachmentState>,
  VecDeque<(
    TrackIndex,
    AttachmentPacket<AttachmentPacketExtra, FfmpegBuffer>,
  )>,
);

fn build_tracks(input: &Input) -> Result<BuiltTracks, DemuxError> {
  let count = input.streams().len();
  let mut tracks = Vec::with_capacity(count);
  let mut attachments = Vec::with_capacity(count);
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

    let metadata = stream.metadata();
    let info = TrackInfo::new(time_base, params, extra)
      .with_duration(duration)
      .with_filename(metadata.get("filename").map(SmolStr::new))
      .with_mime_type(metadata.get("mimetype").map(SmolStr::new));

    // Capture the attachment payload now, so the queue is complete
    // before a single timed packet has been read.
    let state = if info.kind() == TrackKind::Attachment {
      let captured = if attached_pic {
        attached_pic_payload(&stream)
      } else {
        extradata_payload(&stream)?
      };
      match captured {
        Some(packet) => {
          pending.push_back((TrackIndex::new(index), packet));
          AttachmentState::Captured
        }
        // Nothing to capture: an ATTACHED_PIC stream whose
        // `attached_pic` libavformat left empty. Fall back to taking
        // the first packet the stream produces.
        None => AttachmentState::AwaitingPacket,
      }
    } else {
      AttachmentState::None
    };

    tracks.push(info);
    attachments.push(state);
  }

  Ok((tracks, attachments, pending))
}

/// Wraps `AVStream.attached_pic` — the real packet libavformat parsed
/// for a cover-art stream — as an attachment payload. `None` when the
/// stream carries none.
fn attached_pic_payload(
  stream: &ffmpeg_next::format::stream::Stream<'_>,
) -> Option<AttachmentPacket<AttachmentPacketExtra, FfmpegBuffer>> {
  // SAFETY: `stream` keeps the format context (and so the `AVStream`)
  // live; `attached_pic` is an `AVPacket` embedded by value, and
  // `addr_of!` reaches it without forming a reference to the stream.
  let pkt = unsafe { std::ptr::addr_of!((*stream.as_ptr()).attached_pic) };
  let buf = unsafe { (*pkt).buf };
  let data = unsafe { (*pkt).data };
  let size = unsafe { (*pkt).size };
  if buf.is_null() || data.is_null() || size <= 0 {
    return None;
  }
  let buf_data = unsafe { (*buf).data };
  if buf_data.is_null() {
    return None;
  }
  let offset = (data as usize).wrapping_sub(buf_data as usize);
  // SAFETY: `buf` is a live `AVBufferRef` owned by the `AVStream`;
  // `from_ref_view` bumps its refcount and bounds-checks the view.
  let payload = unsafe { FfmpegBuffer::from_ref_view(buf, offset, size as usize) }?;
  Some(AttachmentPacket::new(
    payload,
    AttachmentPacketExtra::new(stream.index() as i32),
  ))
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
) -> Result<Option<AttachmentPacket<AttachmentPacketExtra, FfmpegBuffer>>, DemuxError> {
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
  Ok(Some(AttachmentPacket::new(
    payload,
    AttachmentPacketExtra::new(index as i32).with_synthesized(true),
  )))
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
  use super::*;

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

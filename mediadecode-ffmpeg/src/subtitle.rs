//! `mediadecode::SubtitleDecoder` impl backed by
//! `ffmpeg::decoder::Subtitle`.
//!
//! Subtitles use FFmpeg's legacy synchronous `decode()` API rather
//! than `send_packet`/`receive_frame`. We bridge the difference by
//! converting the produced `AVSubtitle` into a
//! [`mediadecode::SubtitleFrame`] inside [`SubtitleDecoder::send_packet`]
//! and stashing it in `pending` for the next [`SubtitleDecoder::receive_frame`]
//! call. This matches the trait's contract: `send_packet` enqueues
//! work, `receive_frame` drains one decoded frame at a time, and
//! `NoFrameReady` is signalled via [`SubtitleDecodeError::NoFrameReady`].

use std::option::Option;

use ffmpeg_next::{codec::Parameters, ffi::avsubtitle_free};
use mediadecode::{
  Timebase, decoder::SubtitleDecoder, frame::SubtitleFrame, packet::SubtitlePacket,
};

use crate::{
  Error, Ffmpeg, FfmpegBuffer, boundary,
  convert::{self, ConvertError},
  decoder::build_codec_context,
  extras::{SubtitleFrameExtra, SubtitlePacketExtra},
};

/// RAII wrapper that owns an `ffmpeg_next::Subtitle` scratch slot and
/// frees the FFmpeg-side rect allocations on drop / explicit `clear`.
///
/// `ffmpeg::Subtitle::new()` zero-initializes; `decoder.decode()` may
/// allocate per-rect storage (`AVSubtitleRect.text` / `.ass` /
/// `.data[0]` / `.data[1]`) which only `avsubtitle_free` releases.
/// Without this wrapper, every successful decode leaks until the
/// decoder drops.
struct ScratchSubtitle {
  inner: ffmpeg_next::Subtitle,
}

impl ScratchSubtitle {
  fn new() -> Self {
    Self {
      inner: ffmpeg_next::Subtitle::new(),
    }
  }

  fn clear(&mut self) {
    // SAFETY: `inner` holds a valid AVSubtitle (zero-initialized or
    // populated by `decode`). `avsubtitle_free` frees the rect array
    // and per-rect allocations, then leaves the struct in a state
    // suitable for reuse by the next decode call.
    unsafe { avsubtitle_free(self.inner.as_mut_ptr()) };
  }
}

impl Drop for ScratchSubtitle {
  fn drop(&mut self) {
    self.clear();
  }
}

/// `mediadecode::SubtitleDecoder` impl wrapping `ffmpeg::decoder::Subtitle`.
///
/// Subtitle decoders are stateless from FFmpeg's perspective — each
/// `decode()` call consumes one packet and produces zero-or-one
/// `AVSubtitle`. The pending-frame buffer here is a one-slot queue
/// so the trait's `send_packet` / `receive_frame` split works.
pub struct FfmpegSubtitleStreamDecoder {
  decoder: ffmpeg_next::decoder::Subtitle,
  scratch: ScratchSubtitle,
  pending: Option<SubtitleFrame<SubtitleFrameExtra, FfmpegBuffer>>,
  time_base: Timebase,
}

impl FfmpegSubtitleStreamDecoder {
  /// Opens a subtitle decoder for the given codec parameters.
  pub fn open(parameters: Parameters, time_base: Timebase) -> Result<Self, SubtitleDecodeError> {
    // Use the checked codec-context builder — `Context::from_parameters`
    // is OOM-UB-prone (see `crate::decoder::build_codec_context`).
    let ctx = build_codec_context(&parameters).map_err(SubtitleDecodeError::Decode)?;
    let decoder = ctx
      .decoder()
      .subtitle()
      .map_err(|e| SubtitleDecodeError::Decode(Error::Ffmpeg(e)))?;
    Ok(Self {
      decoder,
      scratch: ScratchSubtitle::new(),
      pending: None,
      time_base,
    })
  }

  /// Returns the time base associated with the source stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn time_base(&self) -> Timebase {
    self.time_base
  }

  /// Borrow the wrapped `ffmpeg::decoder::Subtitle`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn inner(&self) -> &ffmpeg_next::decoder::Subtitle {
    &self.decoder
  }
}

impl SubtitleDecoder for FfmpegSubtitleStreamDecoder {
  type Adapter = Ffmpeg;
  type Buffer = FfmpegBuffer;
  type Error = SubtitleDecodeError;

  fn send_packet(
    &mut self,
    packet: &SubtitlePacket<SubtitlePacketExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    // Disallow sending while a previously-decoded frame hasn't been
    // drained yet. The legacy `decode()` API produces a frame inline,
    // so a second send would silently drop the first — surface that
    // as an error so callers notice the drain ordering.
    if self.pending.is_some() {
      return Err(SubtitleDecodeError::FramePending);
    }
    let av_pkt = boundary::ffmpeg_packet_from_subtitle_packet(packet)
      .map_err(|e| SubtitleDecodeError::Decode(Error::Ffmpeg(e)))?;
    // Free any allocations from a previous decode before reusing the
    // scratch — avoids leaking when the previous packet produced no
    // frame (got == false, which still mutates the struct).
    self.scratch.clear();
    let got = self
      .decoder
      .decode(&av_pkt, &mut self.scratch.inner)
      .map_err(|e| SubtitleDecodeError::Decode(Error::Ffmpeg(e)))?;
    if got {
      // SAFETY: scratch.inner is a live AVSubtitle just filled by
      // decode. Conversion deep-copies all rect contents into owned
      // FfmpegBuffers; the FFmpeg-side allocations are released
      // unconditionally below (success and error paths both reach
      // the next `clear()` on the next decode or on drop).
      let result = unsafe {
        convert::av_subtitle_to_subtitle_frame(self.scratch.inner.as_ptr(), self.time_base)
      };
      match result {
        Ok(frame) => self.pending = Some(frame),
        Err(e) => {
          // Free immediately on conversion failure — without this, a
          // caller that ignores the error and calls `flush` would
          // bypass the scratch's deferred cleanup.
          self.scratch.clear();
          return Err(SubtitleDecodeError::Convert(e));
        }
      }
    }
    Ok(())
  }

  fn receive_frame(
    &mut self,
    dst: &mut SubtitleFrame<SubtitleFrameExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    match self.pending.take() {
      Some(frame) => {
        *dst = frame;
        Ok(())
      }
      None => Err(SubtitleDecodeError::NoFrameReady),
    }
  }

  fn send_eof(&mut self) -> Result<(), Self::Error> {
    // Subtitle decoders have no draining — the legacy decode() API
    // produces a frame inline with each packet. EOF is a no-op.
    Ok(())
  }

  fn flush(&mut self) -> Result<(), Self::Error> {
    self.decoder.flush();
    self.pending = None;
    self.scratch.clear();
    Ok(())
  }
}

/// Errors from [`FfmpegSubtitleStreamDecoder`].
#[derive(thiserror::Error, Debug)]
pub enum SubtitleDecodeError {
  /// The wrapped `ffmpeg::decoder::Subtitle` reported an error.
  #[error(transparent)]
  Decode(#[from] Error),
  /// Conversion from FFmpeg's `AVSubtitle` to mediadecode's
  /// `SubtitleFrame` failed.
  #[error(transparent)]
  Convert(#[from] ConvertError),
  /// `receive_frame` was called with no buffered frame ready — caller
  /// should send another packet.
  #[error("no subtitle frame ready; send another packet first")]
  NoFrameReady,
  /// `send_packet` was called while a decoded frame from a previous
  /// packet hasn't been drained — the legacy `decode()` API can't
  /// queue, so the caller must drain via `receive_frame` first.
  #[error("subtitle frame already pending; drain via receive_frame first")]
  FramePending,
}

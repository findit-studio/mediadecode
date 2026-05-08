//! `mediadecode::VideoStreamDecoder` impl — wraps this crate's
//! [`crate::VideoDecoder`] (HW probe across VideoToolbox / VAAPI / NVDEC
//! / D3D11VA) and produces `mediadecode::VideoFrame<Ffmpeg, FfmpegBuffer>`
//! through the [`crate::convert`] helper.
//!
//! Software fallback (`ffmpeg::decoder::Video` opened directly when the
//! HW probe exhausts every backend) is **not yet wired in** — the impl
//! delegates verbatim to the HW path. When `Error::AllBackendsFailed`
//! surfaces, the caller can construct a software decoder of their own
//! choice using the rescued `unconsumed_packets`. Integrating SW
//! fallback inside this impl is tracked as a follow-up.

use ffmpeg_next::codec::Parameters;
use mediadecode::{
  Timebase, decoder::VideoStreamDecoder, frame::VideoFrame, packet::VideoPacket,
};

use crate::{Error, Ffmpeg, FfmpegBuffer, Frame, VideoDecoder, convert};

/// `mediadecode::VideoStreamDecoder` impl over the FFmpeg backend.
///
/// Holds an internal [`VideoDecoder`] (the HW-probe wrapper from the
/// `hwdecode` ancestry of this crate) plus the source stream's
/// [`Timebase`] for labeling timestamps on emitted frames.
///
/// `receive_frame_into` writes into the caller-supplied
/// `mediadecode::VideoFrame<Ffmpeg, FfmpegBuffer>`, replacing its
/// contents with the freshly decoded frame. The internal `Frame`
/// scratch buffer is reused across calls to avoid per-frame
/// allocations on the FFmpeg side.
pub struct FfmpegVideoStreamDecoder {
  inner: VideoDecoder,
  scratch: Frame,
  time_base: Timebase,
}

impl FfmpegVideoStreamDecoder {
  /// Opens a decoder for the given codec parameters with the default
  /// HW backend probe order. The supplied `time_base` is used to label
  /// PTS / duration on emitted frames.
  ///
  /// Returns the same error set as [`VideoDecoder::open`] (notably
  /// [`Error::AllBackendsFailed`] when no HW backend opens).
  pub fn open(parameters: Parameters, time_base: Timebase) -> Result<Self, Error> {
    let inner = VideoDecoder::open(parameters)?;
    let scratch = Frame::empty()?;
    Ok(Self {
      inner,
      scratch,
      time_base,
    })
  }

  /// Borrow the underlying [`VideoDecoder`] (for backend introspection,
  /// raw FFmpeg access, etc.).
  pub fn inner(&self) -> &VideoDecoder {
    &self.inner
  }

  /// Mutably borrow the underlying [`VideoDecoder`].
  pub fn inner_mut(&mut self) -> &mut VideoDecoder {
    &mut self.inner
  }

  /// Returns the time base associated with the source stream.
  pub fn time_base(&self) -> Timebase {
    self.time_base
  }
}

impl VideoStreamDecoder for FfmpegVideoStreamDecoder {
  type Adapter = Ffmpeg;
  type Buffer = FfmpegBuffer;
  type Error = VideoDecodeError;

  fn send_packet(
    &mut self,
    _packet: &VideoPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    // Building an `ffmpeg::Packet` from the mediadecode VideoPacket
    // requires a small dance: we need to expose the FfmpegBuffer's
    // underlying AVBufferRef back to FFmpeg, attach it to a fresh
    // AVPacket, and call into `VideoDecoder::send_packet`. That round-
    // trip is the right scope for a follow-up commit; for now we
    // surface a structured error so callers know the path isn't live.
    Err(VideoDecodeError::SendPacketNotImplemented)
  }

  fn receive_frame(
    &mut self,
    dst: &mut VideoFrame<Self::Adapter, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    self
      .inner
      .receive_frame(&mut self.scratch)
      .map_err(VideoDecodeError::Decode)?;
    // SAFETY: `self.scratch` wraps a live AVFrame for the duration
    // of this call (we just wrote to it via `receive_frame` and we
    // haven't released it). The conversion bumps refcounts on the
    // AVBufferRefs it pulls into the produced VideoFrame, so the
    // source scratch frame can be reused on the next call.
    let av_frame: *const ffmpeg_next::ffi::AVFrame = unsafe { self.scratch.as_inner_mut().as_ptr() };
    let new_frame = unsafe { convert::av_frame_to_video_frame(av_frame, self.time_base) }
      .map_err(VideoDecodeError::Convert)?;
    *dst = new_frame;
    Ok(())
  }

  fn send_eof(&mut self) -> Result<(), Self::Error> {
    self.inner.send_eof().map_err(VideoDecodeError::Decode)
  }

  fn flush(&mut self) -> Result<(), Self::Error> {
    self.inner.flush();
    Ok(())
  }
}

/// Error type for [`FfmpegVideoStreamDecoder`].
#[derive(thiserror::Error, Debug)]
pub enum VideoDecodeError {
  /// The wrapped [`VideoDecoder`] reported an error.
  #[error("{0}")]
  Decode(#[from] Error),
  /// Frame conversion from FFmpeg's native types to mediadecode's
  /// types failed.
  #[error("frame conversion failed: {0}")]
  Convert(crate::convert::ConvertError),
  /// `send_packet` is not yet implemented for this decoder; submit
  /// packets through the lower-level [`VideoDecoder::send_packet`]
  /// API or wait for the follow-up commit that wires this path.
  #[error("send_packet via the VideoStreamDecoder trait isn't wired up yet — use VideoDecoder::send_packet directly for now")]
  SendPacketNotImplemented,
}

//! `mediadecode::SubtitleDecoder` implementation — currently a stub.
//! Same shape as [`crate::audio::FfmpegAudioStreamDecoder`] — trait
//! surface in place, method bodies deferred to a follow-up commit.

use mediadecode::{
  Timebase, decoder::SubtitleDecoder, frame::SubtitleFrame, packet::SubtitlePacket,
};

use crate::{Ffmpeg, FfmpegBuffer};

/// Stub `SubtitleDecoder` impl wrapping `ffmpeg::decoder::Subtitle`.
pub struct FfmpegSubtitleDecoder {
  /// Source-stream time base, used for labeling produced frames.
  time_base: Timebase,
}

impl FfmpegSubtitleDecoder {
  /// Constructs a stub decoder.
  pub fn new(time_base: Timebase) -> Self {
    Self { time_base }
  }

  /// Returns the time base associated with the source stream.
  pub fn time_base(&self) -> Timebase {
    self.time_base
  }
}

impl SubtitleDecoder for FfmpegSubtitleDecoder {
  type Adapter = Ffmpeg;
  type Buffer = FfmpegBuffer;
  type Error = SubtitleDecodeError;

  fn send_packet(
    &mut self,
    _packet: &SubtitlePacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    Err(SubtitleDecodeError::NotImplemented)
  }

  fn receive_frame(
    &mut self,
    _dst: &mut SubtitleFrame<Self::Adapter, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    Err(SubtitleDecodeError::NotImplemented)
  }

  fn send_eof(&mut self) -> Result<(), Self::Error> {
    Err(SubtitleDecodeError::NotImplemented)
  }

  fn flush(&mut self) -> Result<(), Self::Error> {
    Err(SubtitleDecodeError::NotImplemented)
  }
}

/// Errors from [`FfmpegSubtitleDecoder`].
#[derive(thiserror::Error, Debug)]
pub enum SubtitleDecodeError {
  /// The subtitle decoder methods aren't wired up yet.
  #[error("FfmpegSubtitleDecoder isn't implemented yet — coming in a follow-up commit")]
  NotImplemented,
}

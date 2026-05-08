//! `mediadecode::AudioStreamDecoder` implementation — currently a
//! stub. Trait shape is locked in so generic code can name the type;
//! method bodies return [`AudioDecodeError::NotImplemented`] until a
//! follow-up commit wires up `ffmpeg::decoder::Audio`.

use mediadecode::{
  Timebase, decoder::AudioStreamDecoder, frame::AudioFrame, packet::AudioPacket,
};

use crate::{Ffmpeg, FfmpegBuffer};

/// Stub `AudioStreamDecoder` impl that wraps `ffmpeg::decoder::Audio`.
///
/// The trait surface is in place so downstream code can write
/// `decoder: impl AudioStreamDecoder<Adapter = Ffmpeg, Buffer = FfmpegBuffer>`
/// today; the decode loop itself is a follow-up.
pub struct FfmpegAudioStreamDecoder {
  /// Source-stream time base, used for labeling produced frames.
  time_base: Timebase,
}

impl FfmpegAudioStreamDecoder {
  /// Constructs a stub decoder. The full constructor (taking codec
  /// parameters and opening an `ffmpeg::decoder::Audio`) lands in a
  /// follow-up commit.
  pub fn new(time_base: Timebase) -> Self {
    Self { time_base }
  }

  /// Returns the time base associated with the source stream.
  pub fn time_base(&self) -> Timebase {
    self.time_base
  }
}

impl AudioStreamDecoder for FfmpegAudioStreamDecoder {
  type Adapter = Ffmpeg;
  type Buffer = FfmpegBuffer;
  type Error = AudioDecodeError;

  fn send_packet(
    &mut self,
    _packet: &AudioPacket<Self::Adapter, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    Err(AudioDecodeError::NotImplemented)
  }

  fn receive_frame(
    &mut self,
    _dst: &mut AudioFrame<Self::Adapter, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    Err(AudioDecodeError::NotImplemented)
  }

  fn send_eof(&mut self) -> Result<(), Self::Error> {
    Err(AudioDecodeError::NotImplemented)
  }

  fn flush(&mut self) -> Result<(), Self::Error> {
    Err(AudioDecodeError::NotImplemented)
  }
}

/// Errors from [`FfmpegAudioStreamDecoder`].
#[derive(thiserror::Error, Debug)]
pub enum AudioDecodeError {
  /// The audio decoder methods aren't wired up yet.
  #[error("FfmpegAudioStreamDecoder isn't implemented yet — coming in a follow-up commit")]
  NotImplemented,
}

//! `mediadecode::AudioStreamDecoder` impl backed by
//! `ffmpeg::decoder::Audio`.
//!
//! Mirrors the shape of [`crate::FfmpegVideoStreamDecoder`] without
//! the HW-fallback wrinkle — audio decoders never go through a
//! hardware backend in the FFmpeg world, so there's no probe, no
//! state machine, just `send_packet` / `receive_frame` over the
//! software decoder.
//!
//! Frames produced via [`crate::convert::av_frame_to_audio_frame`]
//! carry zero-copy `FfmpegBuffer` plane views into the source
//! `AVFrame`'s refcounted buffers; the consumer can hold the frame
//! across decoder calls without copying.

use ffmpeg_next::{codec::Parameters, frame};
use mediadecode::{
  Timebase, channel::AudioChannelLayout, decoder::AudioStreamDecoder, frame::AudioFrame,
  packet::AudioPacket,
};

use crate::{
  Error, Ffmpeg, FfmpegBuffer, boundary,
  convert::{self, ConvertError},
  decoder::build_codec_context,
  extras::{AudioFrameExtra, AudioPacketExtra},
  frame::alloc_av_audio_frame,
  sample_format::SampleFormat,
};

/// `mediadecode::AudioStreamDecoder` impl wrapping `ffmpeg::decoder::Audio`.
pub struct FfmpegAudioStreamDecoder {
  decoder: ffmpeg_next::decoder::Audio,
  scratch: frame::Audio,
  time_base: Timebase,
}

impl FfmpegAudioStreamDecoder {
  /// Opens an audio decoder for the given codec parameters.
  pub fn open(parameters: Parameters, time_base: Timebase) -> Result<Self, AudioDecodeError> {
    // Use the checked codec-context builder — `Context::from_parameters`
    // is OOM-UB-prone (see `crate::decoder::build_codec_context`).
    let ctx = build_codec_context(&parameters).map_err(AudioDecodeError::Decode)?;
    let decoder = ctx
      .decoder()
      .audio()
      .map_err(|e| AudioDecodeError::Decode(Error::Ffmpeg(e)))?;
    let scratch = alloc_av_audio_frame().map_err(AudioDecodeError::Decode)?;
    Ok(Self {
      decoder,
      scratch,
      time_base,
    })
  }

  /// Returns the time base associated with the source stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn time_base(&self) -> Timebase {
    self.time_base
  }

  /// Borrow the wrapped `ffmpeg::decoder::Audio` (e.g. to query
  /// `channels()` / `rate()` / `format()`).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn inner(&self) -> &ffmpeg_next::decoder::Audio {
    &self.decoder
  }
}

impl AudioStreamDecoder for FfmpegAudioStreamDecoder {
  type Adapter = Ffmpeg;
  type Buffer = FfmpegBuffer;
  type Error = AudioDecodeError;

  fn send_packet(
    &mut self,
    packet: &AudioPacket<AudioPacketExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    let av_pkt = boundary::ffmpeg_packet_from_audio_packet(packet)
      .map_err(|e| AudioDecodeError::Decode(Error::Ffmpeg(e)))?;
    self
      .decoder
      .send_packet(&av_pkt)
      .map_err(|e| AudioDecodeError::Decode(Error::Ffmpeg(e)))
  }

  fn receive_frame(
    &mut self,
    dst: &mut AudioFrame<SampleFormat, AudioChannelLayout, AudioFrameExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    self
      .decoder
      .receive_frame(&mut self.scratch)
      .map_err(|e| AudioDecodeError::Decode(Error::Ffmpeg(e)))?;
    // SAFETY: scratch was just filled by receive_frame; convert
    // refcounts each plane buffer it pulls into the produced
    // AudioFrame so the scratch can be reused on the next call.
    let new_frame =
      unsafe { convert::av_frame_to_audio_frame(self.scratch.as_ptr(), self.time_base) }
        .map_err(AudioDecodeError::Convert)?;
    *dst = new_frame;
    Ok(())
  }

  fn send_eof(&mut self) -> Result<(), Self::Error> {
    self
      .decoder
      .send_eof()
      .map_err(|e| AudioDecodeError::Decode(Error::Ffmpeg(e)))
  }

  fn flush(&mut self) -> Result<(), Self::Error> {
    self.decoder.flush();
    Ok(())
  }
}

/// Errors from [`FfmpegAudioStreamDecoder`].
#[derive(thiserror::Error, Debug, Clone)]
pub enum AudioDecodeError {
  /// The wrapped `ffmpeg::decoder::Audio` reported an error.
  #[error(transparent)]
  Decode(#[from] Error),
  /// Conversion from FFmpeg's `AVFrame` to mediadecode's `AudioFrame`
  /// failed.
  #[error(transparent)]
  Convert(#[from] ConvertError),
}

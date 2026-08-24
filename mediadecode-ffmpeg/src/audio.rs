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
//! carry `FfmpegBytes` planes copied out of the source `AVFrame` — the
//! [D-seat amputation contract][law]. The consumer can hold the frame
//! across decoder calls, send it to another thread, and outlive the
//! decoder that made it.
//!
//! [law]: mediadecode::adapter#the-d-seat-amputation-contract

use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::{codec::Parameters, frame};
use mediadecode::{Timebase, decoder::AudioStreamDecoder, frame::AudioFrame, packet::AudioPacket};
use mediaframe::audio::ChannelLayoutDescription;

use crate::{
  DecoderLimits, Error, Ffmpeg, FfmpegBytes, boundary,
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
  limits: DecoderLimits,
  /// Keeps the [`CallbackState`](crate::ffi::CallbackState) alive for as
  /// long as the codec context that points at it.
  ///
  /// Declared **after** the decoder on purpose: struct fields drop in
  /// declaration order, so the `AVCodecContext` is freed first and the
  /// state it references outlives it.
  _callback_state: Box<crate::ffi::CallbackState>,
}

impl FfmpegAudioStreamDecoder {
  /// Opens an audio decoder for the given codec parameters.
  ///
  /// `limits` bounds what one decoded frame may cost and is taken here
  /// rather than through a builder: half of it is written straight into
  /// the `AVCodecContext` this call opens, and a context's ceiling
  /// cannot be moved after `avcodec_open2`. See [`DecoderLimits`].
  pub fn open(
    parameters: Parameters,
    time_base: Timebase,
    limits: DecoderLimits,
  ) -> Result<Self, AudioDecodeError> {
    // Use the checked codec-context builder — `Context::from_parameters`
    // is OOM-UB-prone (see `crate::decoder::build_codec_context`).
    let (ctx, callback_state) =
      build_codec_context(&parameters, limits).map_err(AudioDecodeError::Decode)?;
    // Opened without forming a bindgen enum from FFmpeg memory: the codec
    // is resolved off a raw `codec_id`, and the medium is proved off a raw
    // `codec_type`. See `crate::decoder::ensure_codec_type`.
    let codec = crate::decoder::find_decoder(&parameters).map_err(AudioDecodeError::Decode)?;
    let opened = ctx
      .decoder()
      .open_as(codec)
      .map_err(|e| AudioDecodeError::Decode(Error::Ffmpeg(e)))?;
    crate::decoder::ensure_codec_type(&opened, ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO)
      .map_err(AudioDecodeError::Decode)?;
    let decoder = ffmpeg_next::decoder::Audio(opened);
    let scratch = alloc_av_audio_frame().map_err(AudioDecodeError::Decode)?;
    Ok(Self {
      decoder,
      scratch,
      time_base,
      limits,
      _callback_state: callback_state,
    })
  }

  /// Returns the time base associated with the source stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn time_base(&self) -> Timebase {
    self.time_base
  }

  /// The frame ceilings this decoder was opened with.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limits(&self) -> DecoderLimits {
    self.limits
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
  type Buffer = FfmpegBytes;
  type Error = AudioDecodeError;

  fn send_packet(
    &mut self,
    packet: &AudioPacket<AudioPacketExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    let av_pkt = boundary::ffmpeg_packet_from_audio_packet(packet, self.limits.packet_limits())
      .map_err(|e| AudioDecodeError::Decode(Error::PacketBuild(e)))?;
    // Through the software funnel: a frame the allocator judge refused
    // surfaces named, not as the `EINVAL` a corrupt file also produces.
    let state: *const crate::ffi::CallbackState = &*self._callback_state;
    self
      .decoder
      .send_packet(&av_pkt)
      .map_err(|e| AudioDecodeError::Decode(crate::decoder::software_exit(state, e)))
  }

  fn receive_frame(
    &mut self,
    dst: &mut AudioFrame<SampleFormat, ChannelLayoutDescription, AudioFrameExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    let state: *const crate::ffi::CallbackState = &*self._callback_state;
    self
      .decoder
      .receive_frame(&mut self.scratch)
      .map_err(|e| AudioDecodeError::Decode(crate::decoder::software_exit(state, e)))?;
    // SAFETY: scratch was just filled by receive_frame; convert
    // copies every plane it takes into the produced AudioFrame, so the
    // scratch can be reused on the next call and the frame keeps
    // nothing of it.
    let new_frame = unsafe {
      convert::av_frame_to_audio_frame(self.scratch.as_ptr(), self.time_base, self.limits.frame())
    }
    .map_err(AudioDecodeError::Convert)?;
    *dst = new_frame;
    Ok(())
  }

  fn send_eof(&mut self) -> Result<(), Self::Error> {
    let state: *const crate::ffi::CallbackState = &*self._callback_state;
    self
      .decoder
      .send_eof()
      .map_err(|e| AudioDecodeError::Decode(crate::decoder::software_exit(state, e)))
  }

  fn flush(&mut self) -> Result<(), Self::Error> {
    self.decoder.flush();
    Ok(())
  }
}

/// Errors from [`FfmpegAudioStreamDecoder`].
#[derive(thiserror::Error, Debug, Clone, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
pub enum AudioDecodeError {
  /// The wrapped `ffmpeg::decoder::Audio` reported an error.
  #[error(transparent)]
  Decode(#[from] Error),
  /// Conversion from FFmpeg's `AVFrame` to mediadecode's `AudioFrame`
  /// failed.
  #[error(transparent)]
  Convert(#[from] ConvertError),
}

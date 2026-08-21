//! Boundary helpers for downstream code that constructs the
//! destination [`mediadecode::frame::VideoFrame`] /
//! [`mediadecode::frame::AudioFrame`] passed into `receive_frame`.
//!
//! The adapter overwrites the destination wholesale on success, so
//! the placeholder values these helpers fill in are never observed
//! by the caller — they exist purely to satisfy the type's
//! constructor signatures. Mirrors the
//! [`crate::WebCodecsBuffer::empty`] / `mediadecode-ffmpeg`
//! `empty_video_frame` / `empty_audio_frame` API shape so consumers
//! can swap adapters without touching their test scaffolding.
#![cfg(target_arch = "wasm32")]

use mediadecode::{
  frame::{AudioFrame, Dimensions, Plane, VideoFrame},
  pixel_format::PixelFormat,
};
use mediaframe::audio::ChannelLayoutDescription;

use crate::{
  buffer::WebCodecsBuffer,
  extras::{AudioFrameExtra, VideoFrameExtra},
  sample_format::SampleFormat,
};

/// Construct an empty [`mediadecode::frame::VideoFrame`] suitable as
/// the destination argument to
/// [`mediadecode::future::local::VideoStreamDecoder::receive_frame`].
/// All fields are zero / [`WebCodecsBuffer::empty`] / [`PixelFormat::None`]
/// — the adapter overwrites every one of them on a successful decode.
pub fn empty_video_frame() -> VideoFrame<PixelFormat, VideoFrameExtra, WebCodecsBuffer> {
  let planes = [
    Plane::new(WebCodecsBuffer::empty(), 0),
    Plane::new(WebCodecsBuffer::empty(), 0),
    Plane::new(WebCodecsBuffer::empty(), 0),
    Plane::new(WebCodecsBuffer::empty(), 0),
  ];
  VideoFrame::new(
    Dimensions::new(0, 0),
    // mediaframe 0.3's named "no format yet" member, and its `Default`.
    PixelFormat::None,
    planes,
    0,
    VideoFrameExtra::new(false),
  )
}

/// Construct an empty [`mediadecode::frame::AudioFrame`] suitable as
/// the destination argument to
/// [`mediadecode::future::local::AudioStreamDecoder::receive_frame`].
/// `SampleFormat` defaults to [`SampleFormat::S16`] (an arbitrary but
/// representative choice — the adapter overwrites it before the
/// frame is observable).
pub fn empty_audio_frame()
-> AudioFrame<SampleFormat, ChannelLayoutDescription, AudioFrameExtra, WebCodecsBuffer> {
  let planes = [
    Plane::new(WebCodecsBuffer::empty(), 0),
    Plane::new(WebCodecsBuffer::empty(), 0),
    Plane::new(WebCodecsBuffer::empty(), 0),
    Plane::new(WebCodecsBuffer::empty(), 0),
    Plane::new(WebCodecsBuffer::empty(), 0),
    Plane::new(WebCodecsBuffer::empty(), 0),
    Plane::new(WebCodecsBuffer::empty(), 0),
    Plane::new(WebCodecsBuffer::empty(), 0),
  ];
  AudioFrame::new(
    0,
    0,
    0,
    SampleFormat::S16,
    ChannelLayoutDescription::new(0),
    planes,
    0,
    AudioFrameExtra::new(false),
  )
}

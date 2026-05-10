//! Adapter trait impls for the [`WebCodecs`] zero-sized type.

use mediadecode::{
  adapter::{AudioAdapter, VideoAdapter},
  channel::AudioChannelLayout,
  pixel_format::PixelFormat,
};

use crate::{
  codec_id::{AudioCodecId, VideoCodecId},
  extras::{AudioFrameExtra, AudioPacketExtra, VideoFrameExtra, VideoPacketExtra},
  sample_format::SampleFormat,
};

/// Zero-sized adapter type. Implements
/// [`mediadecode::adapter::VideoAdapter`] and
/// [`mediadecode::adapter::AudioAdapter`]. WebCodecs has no
/// subtitle surface so `SubtitleAdapter` is intentionally not
/// implemented.
#[derive(Debug, Clone, Copy, Default)]
pub struct WebCodecs;

impl VideoAdapter for WebCodecs {
  type CodecId = VideoCodecId;
  /// Pixel format is reported by `mediadecode`'s closed
  /// [`PixelFormat`] enum after mapping from `VideoFrame.format`.
  type PixelFormat = PixelFormat;
  type PacketExtra = VideoPacketExtra;
  type FrameExtra = VideoFrameExtra;
}

impl AudioAdapter for WebCodecs {
  type CodecId = AudioCodecId;
  /// Sample format is reported via `mediadecode`'s
  /// [`mediadecode::pixel_format::PixelFormat`]'s audio cousin —
  /// see the core crate. WebCodecs `AudioSampleFormat` maps to
  /// the appropriate `mediadecode::sample_format::SampleFormat`
  /// at frame boundary.
  type SampleFormat = SampleFormat;
  type ChannelLayout = AudioChannelLayout;
  type PacketExtra = AudioPacketExtra;
  type FrameExtra = AudioFrameExtra;
}

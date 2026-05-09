//! `Ffmpeg` adapter — implements [`mediadecode::VideoAdapter`],
//! [`mediadecode::AudioAdapter`], and [`mediadecode::SubtitleAdapter`]
//! for this crate.
//!
//! The adapter is a zero-sized type whose sole purpose is to bind the
//! associated types together so the rest of the API (Packet / Frame /
//! Decoder) reads cleanly: `VideoPacket<Ffmpeg, FfmpegBuffer>` etc.

use mediadecode::{
  PixelFormat,
  adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
  channel::AudioChannelLayout,
};

use crate::{
  codec_id::CodecId,
  extras::{
    AudioFrameExtra, AudioPacketExtra, SubtitleFrameExtra, SubtitlePacketExtra, VideoFrameExtra,
    VideoPacketExtra,
  },
  sample_format::SampleFormat,
};

/// Zero-sized type carrying the FFmpeg adapter's vocabulary.
///
/// Used as the `A` parameter on `mediadecode::VideoPacket<A, B>` /
/// `Frame<A, B>` (and audio / subtitle counterparts) when this crate's
/// decoders are in play. Construction is `Ffmpeg` (unit struct);
/// nothing about the adapter is stateful.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Ffmpeg;

impl VideoAdapter for Ffmpeg {
  type CodecId = CodecId;
  type PixelFormat = PixelFormat;
  type PacketExtra = VideoPacketExtra;
  type FrameExtra = VideoFrameExtra;
}

impl AudioAdapter for Ffmpeg {
  type CodecId = CodecId;
  type SampleFormat = SampleFormat;
  type ChannelLayout = AudioChannelLayout;
  type PacketExtra = AudioPacketExtra;
  type FrameExtra = AudioFrameExtra;
}

impl SubtitleAdapter for Ffmpeg {
  type CodecId = CodecId;
  type PacketExtra = SubtitlePacketExtra;
  type FrameExtra = SubtitleFrameExtra;
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Compile-time proof that the three trait impls' associated types
  /// resolve correctly when the `Ffmpeg` adapter parameterizes
  /// mediadecode's generic types.
  #[test]
  fn adapter_parameterizes_mediadecode_types() {
    use crate::buffer::FfmpegBuffer;
    use mediadecode::{
      adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
      packet::{AudioPacket, SubtitlePacket, VideoPacket},
    };

    fn _video_packet_resolves(
      _: &VideoPacket<Ffmpeg, FfmpegBuffer>,
      _: <Ffmpeg as VideoAdapter>::CodecId,
      _: <Ffmpeg as VideoAdapter>::PixelFormat,
      _: &<Ffmpeg as VideoAdapter>::PacketExtra,
      _: &<Ffmpeg as VideoAdapter>::FrameExtra,
    ) {
    }

    fn _audio_packet_resolves(
      _: &AudioPacket<Ffmpeg, FfmpegBuffer>,
      _: <Ffmpeg as AudioAdapter>::CodecId,
      _: <Ffmpeg as AudioAdapter>::SampleFormat,
      _: &<Ffmpeg as AudioAdapter>::ChannelLayout,
      _: &<Ffmpeg as AudioAdapter>::PacketExtra,
      _: &<Ffmpeg as AudioAdapter>::FrameExtra,
    ) {
    }

    fn _subtitle_packet_resolves(
      _: &SubtitlePacket<Ffmpeg, FfmpegBuffer>,
      _: <Ffmpeg as SubtitleAdapter>::CodecId,
      _: &<Ffmpeg as SubtitleAdapter>::PacketExtra,
      _: &<Ffmpeg as SubtitleAdapter>::FrameExtra,
    ) {
    }
  }

  #[test]
  fn ffmpeg_is_zero_sized() {
    use core::mem::size_of;
    assert_eq!(size_of::<Ffmpeg>(), 0);
  }
}

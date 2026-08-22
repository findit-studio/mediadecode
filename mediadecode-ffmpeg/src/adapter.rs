//! `Ffmpeg` adapter — implements [`mediadecode::VideoAdapter`],
//! [`mediadecode::AudioAdapter`], [`mediadecode::SubtitleAdapter`] and
//! [`mediadecode::demuxer::DemuxAdapter`] for this crate.
//!
//! The adapter is a zero-sized type whose sole purpose is to bind the
//! associated types together so the rest of the API (Packet / Frame /
//! Decoder) reads cleanly: `VideoPacket<Ffmpeg, FfmpegBuffer>` etc.
//!
//! `DemuxAdapter` bundles the other three, and `Ffmpeg` fills all four
//! seats with itself — the demux tier's `CodecId` bound (one
//! codec-identifier namespace across a container's whole track table) is
//! trivially satisfied when every family already binds
//! [`crate::CodecId`].

use mediadecode::{
  PixelFormat,
  adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
  demuxer::DemuxAdapter,
};
use mediaframe::audio::ChannelLayoutDescription;
use smol_str::SmolStr;

use crate::{
  codec_id::CodecId,
  extras::{
    AttachmentPacketExtra, AudioFrameExtra, AudioPacketExtra, DataPacketExtra, SubtitleFrameExtra,
    SubtitlePacketExtra, TrackExtra, VideoFrameExtra, VideoPacketExtra,
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
  type ChannelLayout = ChannelLayoutDescription;
  type PacketExtra = AudioPacketExtra;
  type FrameExtra = AudioFrameExtra;
}

impl SubtitleAdapter for Ffmpeg {
  type CodecId = CodecId;
  type PacketExtra = SubtitlePacketExtra;
  type FrameExtra = SubtitleFrameExtra;
}

impl DemuxAdapter for Ffmpeg {
  type CodecId = CodecId;
  type Video = Ffmpeg;
  type Audio = Ffmpeg;
  type Subtitle = Ffmpeg;
  type DataExtra = DataPacketExtra;
  type AttachmentExtra = AttachmentPacketExtra;
  type TrackExtra = TrackExtra;
  // `AVStream.metadata` hands out borrowed `&str` that dies with the
  // format context, so a track row has to own its identity strings.
  // `SmolStr` stores a filename or a MIME type inline — both are short
  // — which is why the rest of this crate's FFI text handling already
  // uses it.
  type Text = SmolStr;
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

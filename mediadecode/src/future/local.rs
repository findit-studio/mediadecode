//! Local (`!Send`) async variants of the decoder traits.
//!
//! Each trait is also surfaced under [`super::send`] with a
//! `Send`-bounded future via [`trait_variant`]. The two variants
//! are mechanically identical apart from the auto-trait bounds.

use crate::{
  Received, Sent, Timebase, Timestamp,
  adapter::{AudioAdapter, ImageAdapter, SubtitleAdapter, VideoAdapter},
  demuxer::AttachmentPacket,
  frame::{AudioFrame, ImageFrame, SubtitleFrame, VideoFrame},
  packet::{AudioPacket, SubtitlePacket, VideoPacket},
};

/// Async push-style video decoder. Mirror of
/// [`crate::decoder::VideoStreamDecoder`] with `async fn`
/// methods. The `Send`-bounded variant is
/// [`crate::future::send::VideoStreamDecoder`].
#[trait_variant::make(SendVideoStreamDecoder: Send)]
pub trait VideoStreamDecoder {
  /// Backend-specific vocabulary.
  type Adapter: VideoAdapter;
  /// Buffer type held by the packets and frames this decoder
  /// produces or accepts.
  type Buffer: AsRef<[u8]>;
  /// Decoder-specific error type.
  type Error;

  /// Submits one compressed packet, awaiting any host-side back
  /// pressure it *can* wait out (e.g. WebCodecs `decodeQueueSize`
  /// saturation, which the host drains on its own).
  ///
  /// Answers the same [`Sent`] the sync face answers, and
  /// [`Sent::MustDrain`] is the pressure that awaiting cannot resolve:
  /// only the caller's own `receive_frame` can relieve it, and this
  /// method holds `&mut self`, so parking here would deadlock. The
  /// `async` decides *when* an answer arrives, never which answers
  /// exist.
  async fn send_packet(
    &mut self,
    packet: &VideoPacket<<Self::Adapter as VideoAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<Sent, Self::Error>;

  /// Awaits the next decoded frame and writes it to `dst`.
  ///
  /// Answers the same [`Received`] the sync face answers. The `await`
  /// changes when the answer arrives, never which answers exist: a
  /// backend that parks on a host completion event still resolves to
  /// [`Received::NeedsInput`] rather than parking forever when nothing
  /// is in flight and only the caller can supply more.
  async fn receive_frame(
    &mut self,
    dst: &mut VideoFrame<
      <Self::Adapter as VideoAdapter>::PixelFormat,
      <Self::Adapter as VideoAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<Received, Self::Error>;

  /// Signals end-of-stream and waits for the backend to drain.
  async fn send_eof(&mut self) -> Result<Sent, Self::Error>;

  /// Flushes / resets internal state.
  async fn flush(&mut self) -> Result<(), Self::Error>;
}

/// Async pull-style video frame source. Mirror of
/// [`crate::decoder::VideoFrameSource`].
#[trait_variant::make(SendVideoFrameSource: Send)]
pub trait VideoFrameSource {
  /// Backend-specific vocabulary.
  type Adapter: VideoAdapter;
  /// Buffer type for the produced frames.
  type Buffer: AsRef<[u8]>;
  /// Backend-specific clip-level metadata bag.
  type ClipMeta;
  /// Decoder-specific error type.
  type Error;

  /// Total frame count in the clip.
  fn frame_count(&self) -> u64;
  /// Video frame rate.
  fn frame_rate(&self) -> Timebase;
  /// Total clip duration.
  fn duration(&self) -> Timestamp;
  /// Backend-specific clip-level metadata.
  fn clip_meta(&self) -> &Self::ClipMeta;

  /// Decodes one frame at `index` into `dst`.
  async fn decode_frame(
    &mut self,
    index: u64,
    dst: &mut VideoFrame<
      <Self::Adapter as VideoAdapter>::PixelFormat,
      <Self::Adapter as VideoAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<(), Self::Error>;
}

/// Async push-style audio decoder. Mirror of
/// [`crate::decoder::AudioStreamDecoder`].
#[trait_variant::make(SendAudioStreamDecoder: Send)]
pub trait AudioStreamDecoder {
  /// Backend vocabulary.
  type Adapter: AudioAdapter;
  /// Buffer type.
  type Buffer: AsRef<[u8]>;
  /// Decoder-specific error.
  type Error;

  /// Submits a compressed audio packet. See
  /// [`VideoStreamDecoder::send_packet`].
  async fn send_packet(
    &mut self,
    packet: &AudioPacket<<Self::Adapter as AudioAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<Sent, Self::Error>;

  /// Awaits the next decoded frame. See
  /// [`VideoStreamDecoder::receive_frame`].
  async fn receive_frame(
    &mut self,
    dst: &mut AudioFrame<
      <Self::Adapter as AudioAdapter>::SampleFormat,
      <Self::Adapter as AudioAdapter>::ChannelLayout,
      <Self::Adapter as AudioAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<Received, Self::Error>;

  /// Signals EOF and waits for drain.
  async fn send_eof(&mut self) -> Result<Sent, Self::Error>;

  /// Flushes internal state.
  async fn flush(&mut self) -> Result<(), Self::Error>;
}

/// Async pull-style audio frame source. Mirror of
/// [`crate::decoder::AudioFrameSource`].
#[trait_variant::make(SendAudioFrameSource: Send)]
pub trait AudioFrameSource {
  /// Backend vocabulary.
  type Adapter: AudioAdapter;
  /// Buffer type.
  type Buffer: AsRef<[u8]>;
  /// Backend-specific clip-level metadata.
  type ClipMeta;
  /// Decoder-specific error.
  type Error;

  /// Total sample count across all channels.
  fn sample_count(&self) -> u64;
  /// Sample rate (Hz).
  fn sample_rate(&self) -> u32;
  /// Channel count.
  fn channel_count(&self) -> u8;
  /// Backend-specific clip metadata.
  fn clip_meta(&self) -> &Self::ClipMeta;

  /// Decodes a block starting at `sample_offset`, of `sample_count` samples.
  async fn decode_block(
    &mut self,
    sample_offset: u64,
    sample_count: u32,
    dst: &mut AudioFrame<
      <Self::Adapter as AudioAdapter>::SampleFormat,
      <Self::Adapter as AudioAdapter>::ChannelLayout,
      <Self::Adapter as AudioAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<(), Self::Error>;
}

/// Async push-style subtitle decoder. Mirror of
/// [`crate::decoder::SubtitleDecoder`].
#[trait_variant::make(SendSubtitleDecoder: Send)]
pub trait SubtitleDecoder {
  /// Backend vocabulary.
  type Adapter: SubtitleAdapter;
  /// Buffer type.
  type Buffer: AsRef<[u8]>;
  /// Decoder-specific error.
  type Error;

  /// Submits a compressed subtitle packet. See
  /// [`VideoStreamDecoder::send_packet`].
  async fn send_packet(
    &mut self,
    packet: &SubtitlePacket<<Self::Adapter as SubtitleAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<Sent, Self::Error>;

  /// Awaits the next decoded subtitle frame. See
  /// [`VideoStreamDecoder::receive_frame`].
  async fn receive_frame(
    &mut self,
    dst: &mut SubtitleFrame<<Self::Adapter as SubtitleAdapter>::FrameExtra, Self::Buffer>,
  ) -> Result<Received, Self::Error>;

  /// Signals EOF.
  async fn send_eof(&mut self) -> Result<Sent, Self::Error>;

  /// Flushes internal state.
  async fn flush(&mut self) -> Result<(), Self::Error>;
}

/// Async one-shot still-image decoder. Mirror of
/// [`crate::decoder::ImageDecoder`]. The `Send`-bounded variant is
/// [`crate::future::send::ImageDecoder`].
///
/// One `async fn`, because the sync trait has one method: an
/// attachment track's contract is exactly one packet and a still
/// codec's answer to it is exactly one picture. What the `async`
/// buys is a backend whose *decode* is asynchronous — a browser's
/// `createImageBitmap`, a GPU submission — not a rhythm the sync
/// trait was hiding.
#[trait_variant::make(SendImageDecoder: Send)]
pub trait ImageDecoder {
  /// Backend vocabulary.
  type Adapter: ImageAdapter;
  /// Buffer type held by the packet this decoder accepts and the frame
  /// it produces.
  type Buffer: AsRef<[u8]>;
  /// Decoder-specific error.
  type Error;

  /// Awaits the decode of one attachment payload — a whole image file
  /// — into a still.
  async fn decode(
    &mut self,
    packet: &AttachmentPacket<<Self::Adapter as ImageAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<
    ImageFrame<
      <Self::Adapter as ImageAdapter>::PixelFormat,
      <Self::Adapter as ImageAdapter>::FrameExtra,
      Self::Buffer,
    >,
    Self::Error,
  >;
}

#[cfg(test)]
mod tests {
  use super::*;
  use core::num::NonZeroI32;

  struct VLoop;
  impl VideoAdapter for VLoop {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  struct ALoop;
  impl AudioAdapter for ALoop {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  struct SLoop;
  impl SubtitleAdapter for SLoop {
    type CodecId = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  struct ILoop;
  impl ImageAdapter for ILoop {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  #[derive(Debug)]
  struct LoopError;

  struct AsyncVideo;
  impl VideoStreamDecoder for AsyncVideo {
    type Adapter = VLoop;
    type Buffer = &'static [u8];
    type Error = LoopError;

    async fn send_packet(&mut self, _: &VideoPacket<(), &'static [u8]>) -> Result<Sent, LoopError> {
      Ok(Sent::Accepted)
    }
    async fn receive_frame(
      &mut self,
      _: &mut VideoFrame<u32, (), &'static [u8]>,
    ) -> Result<Received, LoopError> {
      Ok(Received::NeedsInput)
    }
    async fn send_eof(&mut self) -> Result<Sent, LoopError> {
      Ok(Sent::Accepted)
    }
    async fn flush(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
  }

  struct AsyncVideoSrc;
  impl VideoFrameSource for AsyncVideoSrc {
    type Adapter = VLoop;
    type Buffer = &'static [u8];
    type ClipMeta = ();
    type Error = LoopError;

    fn frame_count(&self) -> u64 {
      0
    }
    fn frame_rate(&self) -> Timebase {
      Timebase::new(30, NonZeroI32::new(1).unwrap())
    }
    fn duration(&self) -> Timestamp {
      Timestamp::new(0, self.frame_rate())
    }
    fn clip_meta(&self) -> &() {
      &()
    }
    async fn decode_frame(
      &mut self,
      _: u64,
      _: &mut VideoFrame<u32, (), &'static [u8]>,
    ) -> Result<(), LoopError> {
      Err(LoopError)
    }
  }

  struct AsyncAudio;
  impl AudioStreamDecoder for AsyncAudio {
    type Adapter = ALoop;
    type Buffer = &'static [u8];
    type Error = LoopError;
    async fn send_packet(&mut self, _: &AudioPacket<(), &'static [u8]>) -> Result<Sent, LoopError> {
      Ok(Sent::Accepted)
    }
    async fn receive_frame(
      &mut self,
      _: &mut AudioFrame<u32, u32, (), &'static [u8]>,
    ) -> Result<Received, LoopError> {
      Ok(Received::NeedsInput)
    }
    async fn send_eof(&mut self) -> Result<Sent, LoopError> {
      Ok(Sent::Accepted)
    }
    async fn flush(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
  }

  struct AsyncAudioSrc;
  impl AudioFrameSource for AsyncAudioSrc {
    type Adapter = ALoop;
    type Buffer = &'static [u8];
    type ClipMeta = ();
    type Error = LoopError;
    fn sample_count(&self) -> u64 {
      0
    }
    fn sample_rate(&self) -> u32 {
      48_000
    }
    fn channel_count(&self) -> u8 {
      2
    }
    fn clip_meta(&self) -> &() {
      &()
    }
    async fn decode_block(
      &mut self,
      _: u64,
      _: u32,
      _: &mut AudioFrame<u32, u32, (), &'static [u8]>,
    ) -> Result<(), LoopError> {
      Err(LoopError)
    }
  }

  struct AsyncSubtitle;
  impl SubtitleDecoder for AsyncSubtitle {
    type Adapter = SLoop;
    type Buffer = &'static [u8];
    type Error = LoopError;
    async fn send_packet(
      &mut self,
      _: &SubtitlePacket<(), &'static [u8]>,
    ) -> Result<Sent, LoopError> {
      Ok(Sent::Accepted)
    }
    async fn receive_frame(
      &mut self,
      _: &mut SubtitleFrame<(), &'static [u8]>,
    ) -> Result<Received, LoopError> {
      Ok(Received::NeedsInput)
    }
    async fn send_eof(&mut self) -> Result<Sent, LoopError> {
      Ok(Sent::Accepted)
    }
    async fn flush(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
  }

  struct AsyncImage;
  impl ImageDecoder for AsyncImage {
    type Adapter = ILoop;
    type Buffer = &'static [u8];
    type Error = LoopError;
    async fn decode(
      &mut self,
      _: &AttachmentPacket<(), &'static [u8]>,
    ) -> Result<ImageFrame<u32, (), &'static [u8]>, LoopError> {
      Err(LoopError)
    }
  }

  #[test]
  fn local_traits_are_implementable() {
    fn _v<D: VideoStreamDecoder>() {}
    fn _vs<D: VideoFrameSource>() {}
    fn _a<D: AudioStreamDecoder>() {}
    fn _as<D: AudioFrameSource>() {}
    fn _s<D: SubtitleDecoder>() {}
    fn _i<D: ImageDecoder>() {}
    _v::<AsyncVideo>();
    _vs::<AsyncVideoSrc>();
    _a::<AsyncAudio>();
    _as::<AsyncAudioSrc>();
    _s::<AsyncSubtitle>();
    _i::<AsyncImage>();
  }
}

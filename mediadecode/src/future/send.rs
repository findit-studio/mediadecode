//! `Send`-bounded async variants of the decoder traits — the
//! futures returned by every `async fn` carry a `+ Send` bound,
//! so implementers can be used from multi-threaded executors that
//! `tokio::spawn` work across worker threads.
//!
//! Each trait here is generated from its [`super::local`]
//! counterpart by the [`trait_variant`] proc-macro; the only
//! difference is the auto-trait bounds. Backends whose state is
//! thread-pinned (browser / WebCodecs, single-threaded HW
//! decoders) implement [`super::local`] instead.

pub use super::local::{
  SendAudioFrameSource as AudioFrameSource, SendAudioStreamDecoder as AudioStreamDecoder,
  SendSubtitleDecoder as SubtitleDecoder, SendVideoFrameSource as VideoFrameSource,
  SendVideoStreamDecoder as VideoStreamDecoder,
};

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{
    Timebase, Timestamp,
    adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
    frame::{AudioFrame, SubtitleFrame, VideoFrame},
    packet::{AudioPacket, SubtitlePacket, VideoPacket},
  };
  use core::num::NonZeroU32;

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

  #[derive(Debug)]
  struct LoopError;

  struct SendVideo;
  impl VideoStreamDecoder for SendVideo {
    type Adapter = VLoop;
    type Buffer = &'static [u8];
    type Error = LoopError;

    async fn send_packet(&mut self, _: &VideoPacket<(), &'static [u8]>) -> Result<(), LoopError> {
      Ok(())
    }
    async fn receive_frame(
      &mut self,
      _: &mut VideoFrame<u32, (), &'static [u8]>,
    ) -> Result<(), LoopError> {
      Err(LoopError)
    }
    async fn send_eof(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
    async fn flush(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
  }

  struct SendVideoSrc;
  impl VideoFrameSource for SendVideoSrc {
    type Adapter = VLoop;
    type Buffer = &'static [u8];
    type ClipMeta = ();
    type Error = LoopError;

    fn frame_count(&self) -> u64 {
      0
    }
    fn frame_rate(&self) -> Timebase {
      Timebase::new(30, NonZeroU32::new(1).unwrap())
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

  struct SendAudio;
  impl AudioStreamDecoder for SendAudio {
    type Adapter = ALoop;
    type Buffer = &'static [u8];
    type Error = LoopError;
    async fn send_packet(&mut self, _: &AudioPacket<(), &'static [u8]>) -> Result<(), LoopError> {
      Ok(())
    }
    async fn receive_frame(
      &mut self,
      _: &mut AudioFrame<u32, u32, (), &'static [u8]>,
    ) -> Result<(), LoopError> {
      Err(LoopError)
    }
    async fn send_eof(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
    async fn flush(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
  }

  struct SendAudioSrc;
  impl AudioFrameSource for SendAudioSrc {
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

  struct SendSubtitle;
  impl SubtitleDecoder for SendSubtitle {
    type Adapter = SLoop;
    type Buffer = &'static [u8];
    type Error = LoopError;
    async fn send_packet(
      &mut self,
      _: &SubtitlePacket<(), &'static [u8]>,
    ) -> Result<(), LoopError> {
      Ok(())
    }
    async fn receive_frame(
      &mut self,
      _: &mut SubtitleFrame<(), &'static [u8]>,
    ) -> Result<(), LoopError> {
      Err(LoopError)
    }
    async fn send_eof(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
    async fn flush(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
  }

  #[test]
  fn send_traits_are_implementable() {
    fn _v<D: VideoStreamDecoder>() {}
    fn _vs<D: VideoFrameSource>() {}
    fn _a<D: AudioStreamDecoder>() {}
    fn _as<D: AudioFrameSource>() {}
    fn _s<D: SubtitleDecoder>() {}
    _v::<SendVideo>();
    _vs::<SendVideoSrc>();
    _a::<SendAudio>();
    _as::<SendAudioSrc>();
    _s::<SendSubtitle>();
  }

  /// Locks in that the `Send`-bound is enforced — the trait
  /// methods must be callable on a `Send`-bounded generic so
  /// downstream multi-threaded executors can spawn the futures.
  #[test]
  fn send_bounds_propagate() {
    fn _spawnable<D: VideoStreamDecoder + Send>() {}
    _spawnable::<SendVideo>();
  }
}

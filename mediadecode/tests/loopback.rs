//! End-to-end loopback adapter test.
//!
//! Implements the three adapter traits and the five decoder traits
//! with `()` extras and minimal payloads. The test demonstrates that
//! mediadecode's type-and-trait spine composes — packets are
//! accepted, frames flow through the trait machinery, all the
//! generic plumbing resolves.
//!
//! No external SDK is required.

use core::num::NonZeroU32;

use mediadecode::{
  Timebase, Timestamp,
  adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
  color::{ChromaLocation, ColorInfo, ColorMatrix, ColorPrimaries, ColorRange, ColorTransfer},
  decoder::{
    AudioFrameSource, AudioStreamDecoder, SubtitleDecoder, VideoFrameSource, VideoStreamDecoder,
  },
  frame::{AudioFrame, Plane, Rect, SubtitleFrame, VideoFrame},
  packet::{AudioPacket, PacketFlags, SubtitlePacket, VideoPacket},
  subtitle::SubtitlePayload,
};

/// Loopback "backend" — a zero-sized type that implements the three
/// adapter traits with primitive associated types and `()` extras.
pub struct Loop;

impl VideoAdapter for Loop {
  type CodecId = u32;
  type PixelFormat = u32;
  type PacketExtra = ();
  type FrameExtra = ();
}

impl AudioAdapter for Loop {
  type CodecId = u32;
  type SampleFormat = u32;
  type ChannelLayout = u32;
  type PacketExtra = ();
  type FrameExtra = ();
}

impl SubtitleAdapter for Loop {
  type CodecId = u32;
  type PacketExtra = ();
  type FrameExtra = ();
}

#[derive(Debug)]
pub struct Eof;

/// Trivial push-style video decoder that accepts any packet and
/// returns Eof from `receive_frame`.
pub struct VideoStream;

impl VideoStreamDecoder for VideoStream {
  type Adapter = Loop;
  type Buffer = &'static [u8];
  type Error = Eof;
  fn send_packet(&mut self, _: &VideoPacket<Loop, &'static [u8]>) -> Result<(), Eof> {
    Ok(())
  }
  fn receive_frame(&mut self, _: &mut VideoFrame<Loop, &'static [u8]>) -> Result<(), Eof> {
    Err(Eof)
  }
  fn send_eof(&mut self) -> Result<(), Eof> {
    Ok(())
  }
  fn flush(&mut self) -> Result<(), Eof> {
    Ok(())
  }
}

pub struct VideoSource {
  fps: Timebase,
  duration_pts: i64,
}

impl VideoFrameSource for VideoSource {
  type Adapter = Loop;
  type Buffer = &'static [u8];
  type ClipMeta = ();
  type Error = Eof;
  fn frame_count(&self) -> u64 {
    0
  }
  fn frame_rate(&self) -> Timebase {
    self.fps
  }
  fn duration(&self) -> Timestamp {
    Timestamp::new(self.duration_pts, self.fps)
  }
  fn clip_meta(&self) -> &() {
    &()
  }
  fn decode_frame(&mut self, _: u64, _: &mut VideoFrame<Loop, &'static [u8]>) -> Result<(), Eof> {
    Err(Eof)
  }
}

pub struct AudioStream;
impl AudioStreamDecoder for AudioStream {
  type Adapter = Loop;
  type Buffer = &'static [u8];
  type Error = Eof;
  fn send_packet(&mut self, _: &AudioPacket<Loop, &'static [u8]>) -> Result<(), Eof> {
    Ok(())
  }
  fn receive_frame(&mut self, _: &mut AudioFrame<Loop, &'static [u8]>) -> Result<(), Eof> {
    Err(Eof)
  }
  fn send_eof(&mut self) -> Result<(), Eof> {
    Ok(())
  }
  fn flush(&mut self) -> Result<(), Eof> {
    Ok(())
  }
}

pub struct AudioSource;
impl AudioFrameSource for AudioSource {
  type Adapter = Loop;
  type Buffer = &'static [u8];
  type ClipMeta = ();
  type Error = Eof;
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
  fn decode_block(
    &mut self,
    _: u64,
    _: u32,
    _: &mut AudioFrame<Loop, &'static [u8]>,
  ) -> Result<(), Eof> {
    Err(Eof)
  }
}

pub struct SubtitleStream;
impl SubtitleDecoder for SubtitleStream {
  type Adapter = Loop;
  type Buffer = &'static [u8];
  type Error = Eof;
  fn send_packet(&mut self, _: &SubtitlePacket<Loop, &'static [u8]>) -> Result<(), Eof> {
    Ok(())
  }
  fn receive_frame(&mut self, _: &mut SubtitleFrame<Loop, &'static [u8]>) -> Result<(), Eof> {
    Err(Eof)
  }
  fn send_eof(&mut self) -> Result<(), Eof> {
    Ok(())
  }
  fn flush(&mut self) -> Result<(), Eof> {
    Ok(())
  }
}

#[test]
fn video_stream_round_trip() {
  let mut s = VideoStream;
  let pkt: VideoPacket<Loop, &'static [u8]> = VideoPacket::new(b"compressed" as &[u8], ())
    .with_pts(Some(Timestamp::new(
      0,
      Timebase::new(1, NonZeroU32::new(1000).unwrap()),
    )))
    .with_flags(PacketFlags::KEY);
  assert!(s.send_packet(&pkt).is_ok());

  let planes = [
    Plane::new(&b"yyyy"[..], 4),
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
  ];
  let mut dst: VideoFrame<Loop, &'static [u8]> =
    VideoFrame::new(2, 2, /*pix_fmt=*/ 0u32, planes, 1, ())
      .with_visible_rect(Some(Rect::new(0, 0, 2, 2)))
      .with_color(
        ColorInfo::UNSPECIFIED
          .with_primaries(ColorPrimaries::Bt709)
          .with_transfer(ColorTransfer::Bt709)
          .with_matrix(ColorMatrix::Bt709)
          .with_range(ColorRange::Limited)
          .with_chroma_location(ChromaLocation::Left),
      );
  // Loopback's receive_frame returns Eof, but the call compiles
  // and dst's color metadata is settable through the builders.
  assert!(s.receive_frame(&mut dst).is_err());
  assert!(dst.color().matrix().is_bt_709());
  assert!(s.send_eof().is_ok());
  assert!(s.flush().is_ok());
}

#[test]
fn video_source_round_trip() {
  let fps = Timebase::new(30, NonZeroU32::new(1).unwrap());
  let mut src = VideoSource {
    fps,
    duration_pts: 0,
  };
  assert_eq!(src.frame_count(), 0);
  assert_eq!(src.frame_rate(), fps);
  assert_eq!(src.duration().pts(), 0);
  let _: &() = src.clip_meta();

  let planes = [
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
  ];
  let mut dst: VideoFrame<Loop, &'static [u8]> = VideoFrame::new(64, 64, 0u32, planes, 1, ());
  assert!(src.decode_frame(0, &mut dst).is_err());
}

#[test]
fn audio_stream_round_trip() {
  let mut s = AudioStream;
  let pkt: AudioPacket<Loop, &'static [u8]> = AudioPacket::new(b"compressed", ());
  assert!(s.send_packet(&pkt).is_ok());

  let planes = [
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
  ];
  let mut dst: AudioFrame<Loop, &'static [u8]> = AudioFrame::new(
    48_000,
    1024,
    2,
    /*sf=*/ 0u32,
    /*layout=*/ 0u32,
    planes,
    2,
    (),
  );
  assert!(s.receive_frame(&mut dst).is_err());
  assert_eq!(dst.sample_rate(), 48_000);
}

#[test]
fn audio_source_metadata() {
  let src = AudioSource;
  assert_eq!(src.sample_rate(), 48_000);
  assert_eq!(src.channel_count(), 2);
  let _: &() = src.clip_meta();
}

#[test]
fn subtitle_stream_round_trip() {
  let mut s = SubtitleStream;
  let pkt: SubtitlePacket<Loop, &'static [u8]> = SubtitlePacket::new(b"hi", ());
  assert!(s.send_packet(&pkt).is_ok());

  let payload: SubtitlePayload<&'static [u8]> = SubtitlePayload::Text {
    text: b"hi",
    language: Some(*b"eng"),
  };
  let mut dst: SubtitleFrame<Loop, &'static [u8]> = SubtitleFrame::new(payload, ());
  assert!(s.receive_frame(&mut dst).is_err());
}

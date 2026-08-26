//! End-to-end loopback adapter test.
//!
//! Implements the three adapter traits and the five decoder traits
//! with `()` extras and minimal payloads. The test demonstrates that
//! mediadecode's type-and-trait spine composes — packets are
//! accepted, frames flow through the trait machinery, all the
//! generic plumbing resolves.
//!
//! No external SDK is required.

use core::num::NonZeroI32;

use mediadecode::{
  Received, Sent, Timebase, Timestamp,
  adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
  color::{ChromaLocation, ColorInfo, ColorMatrix, ColorPrimaries, ColorRange, ColorTransfer},
  decoder::{
    AudioFrameSource, AudioStreamDecoder, SubtitleDecoder, VideoFrameSource, VideoStreamDecoder,
  },
  frame::{AudioFrame, Dimensions, Plane, Rect, SubtitleFrame, VideoFrame},
  packet::{AudioPacket, PacketFlags, SubtitlePacket, VideoPacket},
  subtitle::{SubtitlePayload, Text as SubtitleText},
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

/// The loopback's fault type. **Not** a home for end-of-stream: the
/// protocol's states leave through [`Received`], so what an error type
/// is left holding here is nothing at all.
#[derive(Debug, PartialEq, Eq)]
pub struct Fault;

/// Trivial push-style video decoder that accepts any packet and reports
/// an ended stream from `receive_frame`.
pub struct VideoStream;

impl VideoStreamDecoder for VideoStream {
  type Adapter = Loop;
  type Buffer = &'static [u8];
  type Error = Fault;
  fn send_packet(&mut self, _: &VideoPacket<(), &'static [u8]>) -> Result<Sent, Fault> {
    Ok(Sent::Accepted)
  }
  fn receive_frame(
    &mut self,
    _: &mut VideoFrame<u32, (), &'static [u8]>,
  ) -> Result<Received, Fault> {
    Ok(Received::Ended)
  }
  fn send_eof(&mut self) -> Result<Sent, Fault> {
    Ok(Sent::Accepted)
  }
  fn flush(&mut self) -> Result<(), Fault> {
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
  type Error = Fault;
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
  fn decode_frame(
    &mut self,
    _: u64,
    _: &mut VideoFrame<u32, (), &'static [u8]>,
  ) -> Result<(), Fault> {
    Err(Fault)
  }
}

pub struct AudioStream;
impl AudioStreamDecoder for AudioStream {
  type Adapter = Loop;
  type Buffer = &'static [u8];
  type Error = Fault;
  fn send_packet(&mut self, _: &AudioPacket<(), &'static [u8]>) -> Result<Sent, Fault> {
    Ok(Sent::Accepted)
  }
  fn receive_frame(
    &mut self,
    _: &mut AudioFrame<u32, u32, (), &'static [u8]>,
  ) -> Result<Received, Fault> {
    Ok(Received::Ended)
  }
  fn send_eof(&mut self) -> Result<Sent, Fault> {
    Ok(Sent::Accepted)
  }
  fn flush(&mut self) -> Result<(), Fault> {
    Ok(())
  }
}

pub struct AudioSource;
impl AudioFrameSource for AudioSource {
  type Adapter = Loop;
  type Buffer = &'static [u8];
  type ClipMeta = ();
  type Error = Fault;
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
    _: &mut AudioFrame<u32, u32, (), &'static [u8]>,
  ) -> Result<(), Fault> {
    Err(Fault)
  }
}

pub struct SubtitleStream;
impl SubtitleDecoder for SubtitleStream {
  type Adapter = Loop;
  type Buffer = &'static [u8];
  type Error = Fault;
  fn send_packet(&mut self, _: &SubtitlePacket<(), &'static [u8]>) -> Result<Sent, Fault> {
    Ok(Sent::Accepted)
  }
  fn receive_frame(&mut self, _: &mut SubtitleFrame<(), &'static [u8]>) -> Result<Received, Fault> {
    Ok(Received::Ended)
  }
  fn send_eof(&mut self) -> Result<Sent, Fault> {
    Ok(Sent::Accepted)
  }
  fn flush(&mut self) -> Result<(), Fault> {
    Ok(())
  }
}

#[test]
fn video_stream_round_trip() {
  let mut s = VideoStream;
  // VideoPacket's E slot is Loop (the adapter ZST flows through as
  // the extras payload — that's the pattern the decoder trait uses).
  let pkt: VideoPacket<(), &'static [u8]> = VideoPacket::new(b"compressed" as &[u8], ())
    .with_pts(Some(Timestamp::new(
      0,
      Timebase::new(1, NonZeroI32::new(1000).unwrap()),
    )))
    .with_flags(PacketFlags::KEY);
  assert_eq!(s.send_packet(&pkt), Ok(Sent::Accepted));

  let planes = [
    Plane::new(&b"yyyy"[..], 4),
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
    Plane::new(&b""[..], 0),
  ];
  // VideoFrame<P, E, D>: P=u32 (Loop's PixelFormat), E=Loop, D=&[u8].
  let mut dst: VideoFrame<u32, (), &'static [u8]> =
    VideoFrame::new(Dimensions::new(2, 2), /*pix_fmt=*/ 0u32, planes, 1, ())
      .with_visible_rect(Some(Rect::new(0, 0, 2, 2)))
      .with_color(
        ColorInfo::UNSPECIFIED
          .with_primaries(ColorPrimaries::Bt709)
          .with_transfer(ColorTransfer::Bt709)
          .with_matrix(ColorMatrix::Bt709)
          .with_range(ColorRange::Limited)
          .with_chroma_location(ChromaLocation::Left),
      );
  // Loopback's receive_frame reports an ended stream — in the `Ok`
  // arm, where a protocol state belongs — and dst's color metadata is
  // settable through the builders.
  assert_eq!(s.receive_frame(&mut dst), Ok(Received::Ended));
  assert!(dst.color().matrix().is_bt_709());
  assert_eq!(s.send_eof(), Ok(Sent::Accepted));
  assert!(s.flush().is_ok());
}

#[test]
fn video_source_round_trip() {
  let fps = Timebase::new(30, NonZeroI32::new(1).unwrap());
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
  let mut dst: VideoFrame<u32, (), &'static [u8]> =
    VideoFrame::new(Dimensions::new(64, 64), 0u32, planes, 1, ());
  assert!(src.decode_frame(0, &mut dst).is_err());
}

#[test]
fn audio_stream_round_trip() {
  let mut s = AudioStream;
  let pkt: AudioPacket<(), &'static [u8]> = AudioPacket::new(b"compressed" as &[u8], ());
  assert_eq!(s.send_packet(&pkt), Ok(Sent::Accepted));

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
  // AudioFrame<S, C, E, D>: S=u32, C=u32, E=Loop, D=&[u8].
  let mut dst: AudioFrame<u32, u32, (), &'static [u8]> = AudioFrame::new(
    48_000,
    1024,
    2,
    /*sf=*/ 0u32,
    /*layout=*/ 0u32,
    planes,
    2,
    (),
  );
  assert_eq!(s.receive_frame(&mut dst), Ok(Received::Ended));
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
  let pkt: SubtitlePacket<(), &'static [u8]> = SubtitlePacket::new(b"hi" as &[u8], ());
  assert_eq!(s.send_packet(&pkt), Ok(Sent::Accepted));

  let payload: SubtitlePayload<&'static [u8]> =
    SubtitlePayload::Text(SubtitleText::new(b"hi", Some(*b"eng")));
  let mut dst: SubtitleFrame<(), &'static [u8]> = SubtitleFrame::new(payload, ());
  assert_eq!(s.receive_frame(&mut dst), Ok(Received::Ended));
}

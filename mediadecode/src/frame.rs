//! Frame types and supporting building blocks.
//!
//! `Rect` and `Plane<B>` are the shared building blocks. The full
//! `VideoFrame` / `AudioFrame` / `SubtitleFrame` types land in later
//! tasks.

use crate::{
  Timestamp,
  adapter::{AudioAdapter, VideoAdapter},
  color::ColorInfo,
};

use crate::{adapter::SubtitleAdapter, subtitle::SubtitlePayload};

/// An axis-aligned integer rectangle.
///
/// Used for `VideoFrame::visible_rect` (FFmpeg crop /
/// WebCodecs `visibleRect` / ProRes RAW `CleanAperture`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
  x: u32,
  y: u32,
  width: u32,
  height: u32,
}

impl Rect {
  /// Constructs a `Rect` at `(x, y)` with the given size.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
    Self {
      x,
      y,
      width,
      height,
    }
  }

  /// Returns the X coordinate of the top-left corner.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn x(&self) -> u32 {
    self.x
  }

  /// Returns the Y coordinate of the top-left corner.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn y(&self) -> u32 {
    self.y
  }

  /// Returns the width.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn width(&self) -> u32 {
    self.width
  }

  /// Returns the height.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn height(&self) -> u32 {
    self.height
  }

  /// Sets the X coordinate (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_x(mut self, x: u32) -> Self {
    self.x = x;
    self
  }
  /// Sets the Y coordinate (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_y(mut self, y: u32) -> Self {
    self.y = y;
    self
  }
  /// Sets the width (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_width(mut self, w: u32) -> Self {
    self.width = w;
    self
  }
  /// Sets the height (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_height(mut self, h: u32) -> Self {
    self.height = h;
    self
  }

  /// Sets the X coordinate in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_x(&mut self, x: u32) -> &mut Self {
    self.x = x;
    self
  }
  /// Sets the Y coordinate in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_y(&mut self, y: u32) -> &mut Self {
    self.y = y;
    self
  }
  /// Sets the width in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_width(&mut self, w: u32) -> &mut Self {
    self.width = w;
    self
  }
  /// Sets the height in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_height(&mut self, h: u32) -> &mut Self {
    self.height = h;
    self
  }
}

/// One plane of pixel or audio data.
///
/// Generic over the buffer type `B` so the same `Plane` shape works
/// for owned (`Vec<u8>`, `bytes::Bytes`), borrowed (`&'a [u8]`), or
/// custom backend-supplied buffers. The bound `B: AsRef<[u8]>` lives
/// at the use site (`Frame<A, B: AsRef<[u8]>>`); `Plane` itself is
/// unbounded so it can be used in const contexts.
///
/// `stride` is bytes per row for video planes, total plane size in
/// bytes for audio planar formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Plane<B> {
  data: B,
  stride: u32,
}

impl<B> Plane<B> {
  /// Constructs a `Plane` from a buffer and a stride.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(data: B, stride: u32) -> Self {
    Self { data, stride }
  }

  /// Returns the stride in bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stride(&self) -> u32 {
    self.stride
  }

  /// Borrows the underlying buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data(&self) -> &B {
    &self.data
  }

  /// Mutably borrows the underlying buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn data_mut(&mut self) -> &mut B {
    &mut self.data
  }

  /// Consumes the plane and returns the underlying buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_data(self) -> B {
    self.data
  }

  /// Sets the stride (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_stride(mut self, stride: u32) -> Self {
    self.stride = stride;
    self
  }

  /// Sets the stride in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_stride(&mut self, stride: u32) -> &mut Self {
    self.stride = stride;
    self
  }
}

/// A decoded video frame.
///
/// `width` / `height` are the **coded** dimensions; `visible_rect`
/// (when present) is the displayable subregion (FFmpeg crop /
/// WebCodecs `visibleRect` / ProRes RAW `CleanAperture`).
///
/// `plane_count` is the number of populated entries in `planes`.
/// Four slots cover every realistic format: NV12 = 2, YUV420P = 3,
/// YUVA / packed-with-alpha = 4, packed RGB / Bayer CFA = 1.
pub struct VideoFrame<A: VideoAdapter, B: AsRef<[u8]>> {
  pts: Option<Timestamp>,
  duration: Option<Timestamp>,
  width: u32,
  height: u32,
  visible_rect: Option<Rect>,
  pixel_format: A::PixelFormat,
  plane_count: u8,
  planes: [Plane<B>; 4],
  color: ColorInfo,
  extra: A::FrameExtra,
}

impl<A: VideoAdapter, B: AsRef<[u8]>> VideoFrame<A, B> {
  /// Constructs a `VideoFrame`. Timestamps default to `None`,
  /// `visible_rect` to `None`, color to `ColorInfo::UNSPECIFIED`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn new(
    width: u32,
    height: u32,
    pixel_format: A::PixelFormat,
    planes: [Plane<B>; 4],
    plane_count: u8,
    extra: A::FrameExtra,
  ) -> Self {
    Self {
      pts: None,
      duration: None,
      width,
      height,
      visible_rect: None,
      pixel_format,
      plane_count,
      planes,
      color: ColorInfo::UNSPECIFIED,
      extra,
    }
  }

  /// Returns the presentation timestamp.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pts(&self) -> Option<Timestamp> {
    self.pts
  }
  /// Returns the duration.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Returns the coded width.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn width(&self) -> u32 {
    self.width
  }
  /// Returns the coded height.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn height(&self) -> u32 {
    self.height
  }
  /// Returns the visible / clean-aperture rectangle, if any.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn visible_rect(&self) -> Option<Rect> {
    self.visible_rect
  }
  /// Returns the pixel format identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn pixel_format(&self) -> A::PixelFormat {
    self.pixel_format
  }
  /// Returns the populated plane count.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn plane_count(&self) -> u8 {
    self.plane_count
  }
  /// Returns the populated planes as a slice.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn planes(&self) -> &[Plane<B>] {
    &self.planes[..self.plane_count as usize]
  }
  /// Returns one plane by index, or `None` if out of range.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn plane(&self, i: usize) -> Option<&Plane<B>> {
    if i < self.plane_count as usize {
      self.planes.get(i)
    } else {
      None
    }
  }
  /// Returns the color metadata.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn color(&self) -> ColorInfo {
    self.color
  }
  /// Returns the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &A::FrameExtra {
    &self.extra
  }
  /// Returns a mutable reference to the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra_mut(&mut self) -> &mut A::FrameExtra {
    &mut self.extra
  }

  /// Sets the PTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }
  /// Sets the visible rect (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_visible_rect(mut self, v: Option<Rect>) -> Self {
    self.visible_rect = v;
    self
  }
  /// Sets the color metadata (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_color(mut self, v: ColorInfo) -> Self {
    self.color = v;
    self
  }

  /// Sets the PTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.pts = v;
    self
  }
  /// Sets the duration in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.duration = v;
    self
  }
  /// Sets the visible rect in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_visible_rect(&mut self, v: Option<Rect>) -> &mut Self {
    self.visible_rect = v;
    self
  }
  /// Sets the color metadata in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_color(&mut self, v: ColorInfo) -> &mut Self {
    self.color = v;
    self
  }
}

/// A decoded audio frame.
///
/// `nb_samples` is **per channel**. `plane_count` is `1` for packed
/// (interleaved) formats and `channel_count` for planar; the
/// `[Plane; 8]` cap mirrors FFmpeg's `AV_NUM_DATA_POINTERS`.
/// Channel counts above 8 surface their extra channels through
/// `A::FrameExtra` (rare in practice).
pub struct AudioFrame<A: AudioAdapter, B: AsRef<[u8]>> {
  pts: Option<Timestamp>,
  duration: Option<Timestamp>,
  sample_rate: u32,
  nb_samples: u32,
  channel_count: u8,
  sample_format: A::SampleFormat,
  channel_layout: A::ChannelLayout,
  plane_count: u8,
  planes: [Plane<B>; 8],
  extra: A::FrameExtra,
}

impl<A: AudioAdapter, B: AsRef<[u8]>> AudioFrame<A, B> {
  /// Constructs an `AudioFrame`.
  #[allow(clippy::too_many_arguments)]
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn new(
    sample_rate: u32,
    nb_samples: u32,
    channel_count: u8,
    sample_format: A::SampleFormat,
    channel_layout: A::ChannelLayout,
    planes: [Plane<B>; 8],
    plane_count: u8,
    extra: A::FrameExtra,
  ) -> Self {
    Self {
      pts: None,
      duration: None,
      sample_rate,
      nb_samples,
      channel_count,
      sample_format,
      channel_layout,
      plane_count,
      planes,
      extra,
    }
  }

  /// Returns the presentation timestamp.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pts(&self) -> Option<Timestamp> {
    self.pts
  }
  /// Returns the duration.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Returns the sample rate (Hz).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn sample_rate(&self) -> u32 {
    self.sample_rate
  }
  /// Returns the per-channel sample count.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn nb_samples(&self) -> u32 {
    self.nb_samples
  }
  /// Returns the channel count.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn channel_count(&self) -> u8 {
    self.channel_count
  }
  /// Returns the sample format identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn sample_format(&self) -> A::SampleFormat {
    self.sample_format
  }
  /// Returns the channel layout identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn channel_layout(&self) -> &A::ChannelLayout {
    &self.channel_layout
  }
  /// Returns the populated plane count.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn plane_count(&self) -> u8 {
    self.plane_count
  }
  /// Returns the populated planes as a slice.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn planes(&self) -> &[Plane<B>] {
    &self.planes[..self.plane_count as usize]
  }
  /// Returns the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &A::FrameExtra {
    &self.extra
  }
  /// Returns a mutable reference to the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra_mut(&mut self) -> &mut A::FrameExtra {
    &mut self.extra
  }

  /// Sets the PTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }

  /// Sets the PTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.pts = v;
    self
  }
  /// Sets the duration in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.duration = v;
    self
  }
}

/// A decoded subtitle frame.
pub struct SubtitleFrame<A: SubtitleAdapter, B: AsRef<[u8]>> {
  pts: Option<Timestamp>,
  duration: Option<Timestamp>,
  payload: SubtitlePayload<B>,
  extra: A::FrameExtra,
}

impl<A: SubtitleAdapter, B: AsRef<[u8]>> SubtitleFrame<A, B> {
  /// Constructs a `SubtitleFrame`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn new(payload: SubtitlePayload<B>, extra: A::FrameExtra) -> Self {
    Self {
      pts: None,
      duration: None,
      payload,
      extra,
    }
  }

  /// Returns the PTS.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pts(&self) -> Option<Timestamp> {
    self.pts
  }
  /// Returns the duration.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Returns the payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn payload(&self) -> &SubtitlePayload<B> {
    &self.payload
  }
  /// Returns the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &A::FrameExtra {
    &self.extra
  }
  /// Returns a mutable reference to the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra_mut(&mut self) -> &mut A::FrameExtra {
    &mut self.extra
  }

  /// Sets the PTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }

  /// Sets the PTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.pts = v;
    self
  }
  /// Sets the duration in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.duration = v;
    self
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn rect_construct_and_access() {
    let r = Rect::new(10, 20, 1920, 1080);
    assert_eq!(r.x(), 10);
    assert_eq!(r.y(), 20);
    assert_eq!(r.width(), 1920);
    assert_eq!(r.height(), 1080);
  }

  #[test]
  fn rect_default_is_zero() {
    let r = Rect::default();
    assert_eq!((r.x(), r.y(), r.width(), r.height()), (0, 0, 0, 0));
  }

  #[test]
  fn rect_builders_chain() {
    let r = Rect::default()
      .with_x(1)
      .with_y(2)
      .with_width(3)
      .with_height(4);
    assert_eq!((r.x(), r.y(), r.width(), r.height()), (1, 2, 3, 4));
  }

  #[test]
  fn rect_setters_chain() {
    let mut r = Rect::default();
    r.set_x(5).set_y(6).set_width(7).set_height(8);
    assert_eq!((r.x(), r.y(), r.width(), r.height()), (5, 6, 7, 8));
  }

  #[test]
  fn rect_const_construction() {
    const R: Rect = Rect::new(0, 0, 1920, 1080);
    assert_eq!(R.width(), 1920);
  }

  #[test]
  fn plane_construct_and_access_borrowed() {
    let buf: [u8; 4] = [1, 2, 3, 4];
    let p: Plane<&[u8]> = Plane::new(&buf, 4);
    assert_eq!(p.stride(), 4);
    assert_eq!(p.data(), &&buf[..]);
  }

  #[test]
  fn plane_with_and_set_stride() {
    let buf: [u8; 0] = [];
    let p = Plane::new(&buf[..], 16).with_stride(32);
    assert_eq!(p.stride(), 32);
    let mut p2 = p;
    p2.set_stride(64);
    assert_eq!(p2.stride(), 64);
  }

  #[test]
  fn plane_into_data() {
    let buf: [u8; 4] = [1, 2, 3, 4];
    let p: Plane<&[u8]> = Plane::new(&buf, 4);
    let recovered = p.into_data();
    assert_eq!(recovered, &buf[..]);
  }

  use crate::{
    adapter::VideoAdapter,
    color::{ColorInfo, ColorMatrix},
  };

  struct VLoop;
  impl VideoAdapter for VLoop {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  fn empty_planes() -> [Plane<&'static [u8]>; 4] {
    [
      Plane::new(&[][..], 0),
      Plane::new(&[][..], 0),
      Plane::new(&[][..], 0),
      Plane::new(&[][..], 0),
    ]
  }

  #[test]
  fn video_frame_construct_and_access() {
    let f: VideoFrame<VLoop, &[u8]> =
      VideoFrame::new(1920, 1080, /*pix_fmt=*/ 0u32, empty_planes(), 1, ());
    assert_eq!(f.width(), 1920);
    assert_eq!(f.height(), 1080);
    assert_eq!(f.plane_count(), 1);
    assert!(f.color().matrix().is_bt_709());
    assert_eq!(f.planes().len(), 1);
  }

  #[test]
  fn video_frame_plane_index_clamped() {
    let f: VideoFrame<VLoop, &[u8]> = VideoFrame::new(64, 64, 0u32, empty_planes(), 2, ());
    assert!(f.plane(0).is_some());
    assert!(f.plane(1).is_some());
    assert!(f.plane(2).is_none());
    assert!(f.plane(3).is_none());
  }

  #[test]
  fn video_frame_builders_chain() {
    let ci = ColorInfo::UNSPECIFIED.with_matrix(ColorMatrix::Bt2020Ncl);
    let f: VideoFrame<VLoop, &[u8]> = VideoFrame::new(64, 64, 0u32, empty_planes(), 1, ())
      .with_color(ci)
      .with_visible_rect(Some(Rect::new(0, 0, 64, 64)));
    assert!(f.color().matrix().is_bt_2020_ncl());
    assert!(f.visible_rect().is_some());
  }

  struct ALoop;
  impl AudioAdapter for ALoop {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  fn audio_planes() -> [Plane<&'static [u8]>; 8] {
    [
      Plane::new(&[][..], 0),
      Plane::new(&[][..], 0),
      Plane::new(&[][..], 0),
      Plane::new(&[][..], 0),
      Plane::new(&[][..], 0),
      Plane::new(&[][..], 0),
      Plane::new(&[][..], 0),
      Plane::new(&[][..], 0),
    ]
  }

  #[test]
  fn audio_frame_construct_and_access() {
    let f: AudioFrame<ALoop, &[u8]> = AudioFrame::new(
      48_000,
      1024,
      2,
      /*sf=*/ 0u32,
      /*layout=*/ 0u32,
      audio_planes(),
      2,
      (),
    );
    assert_eq!(f.sample_rate(), 48_000);
    assert_eq!(f.nb_samples(), 1024);
    assert_eq!(f.channel_count(), 2);
    assert_eq!(f.plane_count(), 2);
    assert_eq!(f.planes().len(), 2);
  }

  use crate::{adapter::SubtitleAdapter, subtitle::SubtitlePayload};

  struct SLoop;
  impl SubtitleAdapter for SLoop {
    type CodecId = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  #[test]
  fn subtitle_frame_text_payload() {
    let payload: SubtitlePayload<&[u8]> = SubtitlePayload::Text {
      text: b"hi",
      language: None,
    };
    let f: SubtitleFrame<SLoop, &[u8]> = SubtitleFrame::new(payload, ());
    match f.payload() {
      SubtitlePayload::Text { text, .. } => assert_eq!(text, &&b"hi"[..]),
      #[cfg(feature = "alloc")]
      _ => panic!("unexpected variant"),
    }
  }
}

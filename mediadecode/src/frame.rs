//! Frame types and supporting building blocks.
//!
//! `Rect` and `Plane<B>` are the shared building blocks. The full
//! `VideoFrame` / `AudioFrame` / `SubtitleFrame` types land in later
//! tasks.

use crate::{Timestamp, color::ColorInfo, subtitle::SubtitlePayload};

/// A `(width, height)` pair in pixels.
///
/// Lives alongside the rest of the frame primitives in this module
/// because the same pair shows up everywhere a video stream is
/// described — the coded dimensions of a [`VideoFrame`], the
/// `coded_*` parameters a backend adapter takes when opening a
/// decoder, the per-plane layout helpers in the WebCodecs
/// adapter, etc. Passing it as a single struct rather than two
/// separate `u32` arguments removes a long-running footgun
/// (silent argument swap) and gives a natural place to hang
/// helpers like [`Self::is_zero`] or [`Self::Display`].
///
/// `u32` width / height matches WebCodecs' `coded_width` /
/// `coded_height` typing in `web_sys` and FFmpeg's
/// `AVCodecContext::width` / `height`. 65535×65535 (the smaller
/// `u16` packing some adjacent crates use) covers every realistic
/// resolution; the `u32` choice here keeps the public API plug-
/// compatible with both adapter typings.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Dimensions {
  width: u32,
  height: u32,
}

impl Dimensions {
  /// Constructs a `Dimensions` with the specified width and height
  /// in pixels.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(width: u32, height: u32) -> Self {
    Self { width, height }
  }

  /// Returns the width in pixels.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn width(&self) -> u32 {
    self.width
  }

  /// Returns the height in pixels.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn height(&self) -> u32 {
    self.height
  }

  /// Sets the width (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_width(mut self, width: u32) -> Self {
    self.width = width;
    self
  }

  /// Sets the width in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_width(&mut self, width: u32) -> &mut Self {
    self.width = width;
    self
  }

  /// Sets the height (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_height(mut self, height: u32) -> Self {
    self.height = height;
    self
  }

  /// Sets the height in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_height(&mut self, height: u32) -> &mut Self {
    self.height = height;
    self
  }

  /// Returns `true` when both width and height are zero — typically
  /// the default-constructed / unset state.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn is_zero(&self) -> bool {
    self.width == 0 && self.height == 0
  }
}

impl core::fmt::Display for Dimensions {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(f, "{}x{}", self.width, self.height)
  }
}

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
/// Generic parameters:
/// - `P` — pixel-format identifier (e.g. `mediadecode_ffmpeg::PixelFormat`).
/// - `E` — backend-specific frame extras (HDR mastering display, RAW
///   sensor metadata, picture type, …).
/// - `D` — plane data buffer type. Each populated `Plane<D>` carries one
///   plane's bytes; `D: AsRef<[u8]>` at the use site (e.g. `Bytes`,
///   `&'a [u8]`, refcounted FFmpeg buffer).
///
/// `width` / `height` are the **coded** dimensions; `visible_rect`
/// (when present) is the displayable subregion (FFmpeg crop /
/// WebCodecs `visibleRect` / ProRes RAW `CleanAperture`).
///
/// `plane_count` is the number of populated entries in `planes`.
/// Four slots cover every realistic format: NV12 = 2, YUV420P = 3,
/// YUVA / packed-with-alpha = 4, packed RGB / Bayer CFA = 1.
pub struct VideoFrame<P, E, D> {
  pts: Option<Timestamp>,
  duration: Option<Timestamp>,
  dimensions: Dimensions,
  visible_rect: Option<Rect>,
  pixel_format: P,
  plane_count: u8,
  planes: [Plane<D>; 4],
  color: ColorInfo,
  extra: E,
}

impl<P, E, D> VideoFrame<P, E, D> {
  /// Constructs a `VideoFrame`. Timestamps default to `None`,
  /// `visible_rect` to `None`, color to `ColorInfo::UNSPECIFIED`.
  ///
  /// `dimensions` is the coded width/height pair (see
  /// [`Dimensions`] and [`Self::dimensions`] for the visible-vs-
  /// coded distinction).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(
    dimensions: Dimensions,
    pixel_format: P,
    planes: [Plane<D>; 4],
    plane_count: u8,
    extra: E,
  ) -> Self {
    Self {
      pts: None,
      duration: None,
      dimensions,
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
  /// Returns the coded dimensions.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn dimensions(&self) -> Dimensions {
    self.dimensions
  }
  /// Returns the coded width.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn width(&self) -> u32 {
    self.dimensions.width()
  }
  /// Returns the coded height.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn height(&self) -> u32 {
    self.dimensions.height()
  }
  /// Returns the visible / clean-aperture rectangle, if any.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn visible_rect(&self) -> Option<Rect> {
    self.visible_rect
  }
  /// Returns a reference to the pixel format identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pixel_format(&self) -> &P {
    &self.pixel_format
  }
  /// Returns the populated plane count.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn plane_count(&self) -> u8 {
    self.plane_count
  }
  /// Returns the populated planes as a slice.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn planes(&self) -> &[Plane<D>] {
    &self.planes[..self.plane_count as usize]
  }
  /// Returns one plane by index, or `None` if out of range.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn plane(&self, i: usize) -> Option<&Plane<D>> {
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
  pub const fn extra(&self) -> &E {
    &self.extra
  }
  /// Returns a mutable reference to the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra_mut(&mut self) -> &mut E {
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
/// Generic parameters:
/// - `S` — sample-format identifier.
/// - `C` — channel layout (e.g. `mediadecode::channel::AudioChannelLayout`).
/// - `E` — backend-specific frame extras.
/// - `D` — plane data buffer type (`D: AsRef<[u8]>` at the use site).
///
/// `nb_samples` is **per channel**. `plane_count` is `1` for packed
/// (interleaved) formats and `channel_count` for planar; the
/// `[Plane; 8]` cap mirrors FFmpeg's `AV_NUM_DATA_POINTERS`. Channel
/// counts above 8 surface their extra channels through `E` (rare in
/// practice).
pub struct AudioFrame<S, C, E, D> {
  pts: Option<Timestamp>,
  duration: Option<Timestamp>,
  sample_rate: u32,
  nb_samples: u32,
  channel_count: u8,
  sample_format: S,
  channel_layout: C,
  plane_count: u8,
  planes: [Plane<D>; 8],
  extra: E,
}

impl<S, C, E, D> AudioFrame<S, C, E, D> {
  /// Constructs an `AudioFrame`.
  #[allow(clippy::too_many_arguments)]
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(
    sample_rate: u32,
    nb_samples: u32,
    channel_count: u8,
    sample_format: S,
    channel_layout: C,
    planes: [Plane<D>; 8],
    plane_count: u8,
    extra: E,
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
  /// Returns a reference to the sample format identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn sample_format(&self) -> &S {
    &self.sample_format
  }
  /// Returns the channel layout identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn channel_layout(&self) -> &C {
    &self.channel_layout
  }
  /// Returns the populated plane count.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn plane_count(&self) -> u8 {
    self.plane_count
  }
  /// Returns the populated planes as a slice.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn planes(&self) -> &[Plane<D>] {
    &self.planes[..self.plane_count as usize]
  }
  /// Returns the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &E {
    &self.extra
  }
  /// Returns a mutable reference to the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra_mut(&mut self) -> &mut E {
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
///
/// Generic parameters:
/// - `E` — backend-specific frame extras.
/// - `D` — payload data buffer type (`D: AsRef<[u8]>` at the use site).
pub struct SubtitleFrame<E, D> {
  pts: Option<Timestamp>,
  duration: Option<Timestamp>,
  payload: SubtitlePayload<D>,
  extra: E,
}

impl<E, D> SubtitleFrame<E, D> {
  /// Constructs a `SubtitleFrame`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(payload: SubtitlePayload<D>, extra: E) -> Self {
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
  pub const fn payload(&self) -> &SubtitlePayload<D> {
    &self.payload
  }
  /// Returns the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &E {
    &self.extra
  }
  /// Returns a mutable reference to the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra_mut(&mut self) -> &mut E {
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
    color::{ColorInfo, ColorMatrix},
    subtitle::SubtitlePayload,
  };

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
    // VideoFrame<P, E, D>: P=u32 (PixelFormat), E=VLoop (adapter ZST),
    // D=&[u8] (plane buffer).
    let f: VideoFrame<u32, (), &[u8]> = VideoFrame::new(
      Dimensions::new(1920, 1080),
      /*pix_fmt=*/ 0u32,
      empty_planes(),
      1,
      (),
    );
    assert_eq!(f.width(), 1920);
    assert_eq!(f.height(), 1080);
    assert_eq!(f.dimensions(), Dimensions::new(1920, 1080));
    assert_eq!(f.plane_count(), 1);
    assert!(f.color().matrix().is_bt_709());
    assert_eq!(f.planes().len(), 1);
  }

  #[test]
  fn video_frame_plane_index_clamped() {
    let f: VideoFrame<u32, (), &[u8]> =
      VideoFrame::new(Dimensions::new(64, 64), 0u32, empty_planes(), 2, ());
    assert!(f.plane(0).is_some());
    assert!(f.plane(1).is_some());
    assert!(f.plane(2).is_none());
    assert!(f.plane(3).is_none());
  }

  #[test]
  fn video_frame_builders_chain() {
    let ci = ColorInfo::UNSPECIFIED.with_matrix(ColorMatrix::Bt2020Ncl);
    let f: VideoFrame<u32, (), &[u8]> =
      VideoFrame::new(Dimensions::new(64, 64), 0u32, empty_planes(), 1, ())
        .with_color(ci)
        .with_visible_rect(Some(Rect::new(0, 0, 64, 64)));
    assert!(f.color().matrix().is_bt_2020_ncl());
    assert!(f.visible_rect().is_some());
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
    // AudioFrame<S, C, E, D>: S=u32 (SampleFormat), C=u32 (ChannelLayout),
    // E=ALoop (adapter ZST), D=&[u8].
    let f: AudioFrame<u32, u32, (), &[u8]> = AudioFrame::new(
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

  #[test]
  fn subtitle_frame_text_payload() {
    let payload: SubtitlePayload<&[u8]> = SubtitlePayload::Text {
      text: b"hi",
      language: None,
    };
    // SubtitleFrame<E, D>: E=SLoop, D=&[u8].
    let f: SubtitleFrame<(), &[u8]> = SubtitleFrame::new(payload, ());
    match f.payload() {
      SubtitlePayload::Text { text, .. } => assert_eq!(text, &&b"hi"[..]),
      #[cfg(any(feature = "std", feature = "alloc"))]
      _ => panic!("unexpected variant"),
    }
  }
}

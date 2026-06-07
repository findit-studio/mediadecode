//! Frame types and supporting building blocks.
//!
//! The frame structural primitives `Dimensions`, `Rect`, and `Plane<B>`
//! are re-exported from `mediaframe::frame` — they live in the lowest-
//! layer crate so colconv, mediadecode, and scenesdetect share a single
//! canonical definition.
//!
//! `VideoFrame<P, E, D>`, `AudioFrame<S, C, E, D>`, and
//! `SubtitleFrame<E, D>` remain in mediadecode because they carry
//! timestamp + backend-extras layers that are mediadecode's domain
//! (`mediaframe` stays the pure pixel-data layer).

pub use mediaframe::frame::{Dimensions, Plane, Rect};

use derive_more::IsVariant;
use thiserror::Error;

use crate::{Timestamp, color::ColorInfo, subtitle::SubtitlePayload};

/// Errors returned by the `try_new` constructors on the frame types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, IsVariant, Error)]
#[non_exhaustive]
pub enum FrameError {
  /// `VideoFrame::try_new` was called with `plane_count > 4`. The
  /// fixed plane array has exactly 4 slots; `plane_count` values
  /// up to and including 4 are accepted, larger values would
  /// later panic inside [`VideoFrame::planes`] far from the
  /// construction site. See [`TooManyVideoPlanes`] for the
  /// payload details. `#[from]` gives a free
  /// `impl From<TooManyVideoPlanes> for FrameError`, so inner
  /// helpers that return `Result<_, TooManyVideoPlanes>` can be
  /// `?`-propagated into `FrameError` directly.
  #[error(transparent)]
  TooManyVideoPlanes(#[from] TooManyVideoPlanes),
  /// `AudioFrame::try_new` was called with `plane_count > 8`. The
  /// fixed plane array has exactly 8 slots (matches FFmpeg's
  /// `AV_NUM_DATA_POINTERS`). See [`TooManyAudioPlanes`] for the
  /// payload details. `#[from]` gives a free
  /// `impl From<TooManyAudioPlanes> for FrameError`.
  #[error(transparent)]
  TooManyAudioPlanes(#[from] TooManyAudioPlanes),
}

/// Payload for [`FrameError::TooManyVideoPlanes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[error("VideoFrame: plane_count {plane_count} exceeds the fixed 4-plane array")]
pub struct TooManyVideoPlanes {
  /// The out-of-range `plane_count` value the caller supplied.
  plane_count: u8,
}

impl TooManyVideoPlanes {
  /// Constructs a new [`TooManyVideoPlanes`] payload.
  #[inline]
  pub const fn new(plane_count: u8) -> Self {
    Self { plane_count }
  }
  /// The out-of-range `plane_count` value the caller supplied.
  #[inline]
  pub const fn plane_count(&self) -> u8 {
    self.plane_count
  }
}

/// Payload for [`FrameError::TooManyAudioPlanes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[error("AudioFrame: plane_count {plane_count} exceeds the fixed 8-plane array")]
pub struct TooManyAudioPlanes {
  /// The out-of-range `plane_count` value the caller supplied.
  plane_count: u8,
}

impl TooManyAudioPlanes {
  /// Constructs a new [`TooManyAudioPlanes`] payload.
  #[inline]
  pub const fn new(plane_count: u8) -> Self {
    Self { plane_count }
  }
  /// The out-of-range `plane_count` value the caller supplied.
  #[inline]
  pub const fn plane_count(&self) -> u8 {
    self.plane_count
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
  ///
  /// # Panics
  ///
  /// Panics if `plane_count > 4`. The fixed-size `planes` array
  /// has four slots; passing a larger `plane_count` would later
  /// trip the slice indexing inside [`Self::planes`] far from
  /// the construction site. Asserting here fails fast with a
  /// clear message instead. Prefer [`Self::try_new`] when
  /// `plane_count` can't be statically proven `<= 4`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(
    dimensions: Dimensions,
    pixel_format: P,
    planes: [Plane<D>; 4],
    plane_count: u8,
    extra: E,
  ) -> Self {
    assert!(
      plane_count as usize <= 4,
      "VideoFrame::new: plane_count exceeds the fixed 4-plane array",
    );
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

  /// Fallible counterpart to [`Self::new`]. Returns
  /// [`FrameError::TooManyVideoPlanes`] when `plane_count > 4`
  /// (the fixed plane array's capacity) rather than panicking.
  ///
  /// Not `const fn` — returning `Result<Self, _>` would require
  /// dropping the moved generic-typed `planes` / `pixel_format` /
  /// `extra` on the error branch, which the const evaluator
  /// can't prove safe for arbitrary `P` / `E` / `D`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn try_new(
    dimensions: Dimensions,
    pixel_format: P,
    planes: [Plane<D>; 4],
    plane_count: u8,
    extra: E,
  ) -> Result<Self, FrameError> {
    if plane_count as usize > 4 {
      return Err(FrameError::TooManyVideoPlanes(TooManyVideoPlanes::new(
        plane_count,
      )));
    }
    Ok(Self {
      pts: None,
      duration: None,
      dimensions,
      visible_rect: None,
      pixel_format,
      plane_count,
      planes,
      color: ColorInfo::UNSPECIFIED,
      extra,
    })
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
  #[must_use]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }
  /// Sets the visible rect (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_visible_rect(mut self, v: Option<Rect>) -> Self {
    self.visible_rect = v;
    self
  }
  /// Sets the color metadata (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
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
  ///
  /// # Panics
  ///
  /// Panics if `plane_count > 8`. The fixed-size `planes` array
  /// has eight slots; passing a larger `plane_count` would
  /// later trip the slice indexing inside [`Self::planes`] far
  /// from the construction site.
  ///
  /// Prefer [`Self::try_new`] when `plane_count` can't be
  /// statically proven `<= 8`.
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
    assert!(
      plane_count as usize <= 8,
      "AudioFrame::new: plane_count exceeds the fixed 8-plane array",
    );
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

  /// Fallible counterpart to [`Self::new`]. Returns
  /// [`FrameError::TooManyAudioPlanes`] when `plane_count > 8`
  /// (the fixed plane array's capacity) rather than panicking.
  ///
  /// Not `const fn` — see the rationale on
  /// [`VideoFrame::try_new`].
  #[allow(clippy::too_many_arguments)]
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn try_new(
    sample_rate: u32,
    nb_samples: u32,
    channel_count: u8,
    sample_format: S,
    channel_layout: C,
    planes: [Plane<D>; 8],
    plane_count: u8,
    extra: E,
  ) -> Result<Self, FrameError> {
    if plane_count as usize > 8 {
      return Err(FrameError::TooManyAudioPlanes(TooManyAudioPlanes::new(
        plane_count,
      )));
    }
    Ok(Self {
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
    })
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
  #[must_use]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
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
  #[must_use]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
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
    assert_eq!(p.data_ref(), &&buf[..]);
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
    // mediaframe rename: `Matrix::default()` is now `Unspecified` (was `Bt709`).
    assert_eq!(f.color().matrix(), crate::color::ColorMatrix::Unspecified);
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
  #[should_panic(expected = "plane_count exceeds the fixed 4-plane array")]
  fn video_frame_rejects_plane_count_above_array_size() {
    let _f: VideoFrame<u32, (), &[u8]> =
      VideoFrame::new(Dimensions::new(64, 64), 0u32, empty_planes(), 5, ());
  }

  #[test]
  fn video_frame_try_new_returns_err_for_too_many_planes() {
    let res: Result<VideoFrame<u32, (), &[u8]>, FrameError> =
      VideoFrame::try_new(Dimensions::new(64, 64), 0u32, empty_planes(), 5, ());
    assert!(matches!(
      res,
      Err(FrameError::TooManyVideoPlanes(p)) if p.plane_count() == 5,
    ));
  }

  #[test]
  fn video_frame_try_new_accepts_valid_plane_count() {
    let f: VideoFrame<u32, (), &[u8]> =
      VideoFrame::try_new(Dimensions::new(64, 64), 0u32, empty_planes(), 2, ())
        .expect("plane_count = 2 is within the 4-slot capacity");
    assert_eq!(f.plane_count(), 2);
  }

  #[test]
  #[should_panic(expected = "plane_count exceeds the fixed 8-plane array")]
  fn audio_frame_rejects_plane_count_above_array_size() {
    let _f: AudioFrame<u32, u32, (), &[u8]> =
      AudioFrame::new(48_000, 1024, 2, 0u32, 0u32, audio_planes(), 9, ());
  }

  #[test]
  fn audio_frame_try_new_returns_err_for_too_many_planes() {
    let res: Result<AudioFrame<u32, u32, (), &[u8]>, FrameError> =
      AudioFrame::try_new(48_000, 1024, 2, 0u32, 0u32, audio_planes(), 9, ());
    assert!(matches!(
      res,
      Err(FrameError::TooManyAudioPlanes(p)) if p.plane_count() == 9,
    ));
  }

  #[test]
  fn audio_frame_try_new_accepts_valid_plane_count() {
    let f: AudioFrame<u32, u32, (), &[u8]> =
      AudioFrame::try_new(48_000, 1024, 2, 0u32, 0u32, audio_planes(), 8, ())
        .expect("plane_count = 8 is the 8-slot capacity boundary");
    assert_eq!(f.plane_count(), 8);
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

//! HDR metadata — the static description of a mastering display and of
//! a stream's light levels, as HDR10 carries them (SMPTE ST 2086 and the
//! content light level of CTA-861.3).
//!
//! These are plain values, the same whichever backend read them off a
//! stream or will write them into one: a decoder reports them on a
//! frame, and an encoder or a muxer takes them as side data.

/// HDR10 mastering display metadata.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MasteringDisplay {
  display_primaries: [(u32, u32); 3],
  white_point: (u32, u32),
  max_luminance: (u32, u32),
  min_luminance: (u32, u32),
}

impl MasteringDisplay {
  /// Constructs a `MasteringDisplay`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(
    display_primaries: [(u32, u32); 3],
    white_point: (u32, u32),
    max_luminance: (u32, u32),
    min_luminance: (u32, u32),
  ) -> Self {
    Self {
      display_primaries,
      white_point,
      max_luminance,
      min_luminance,
    }
  }

  /// Display primary chromaticities `(x, y)` for R, G, B in CIE 1931,
  /// each coordinate normalized to SMPTE ST 2086 fixed-point units —
  /// 0.00002 increments, so a floating value is `raw / 50000.0`. The
  /// same units [`mediaframe::color::ChromaCoord`] uses, so a `(x, y)`
  /// pair here constructs one directly.
  ///
  /// A tuple has room for one denominator, not two, so it cannot hold
  /// `x` and `y` each as an independent `AVRational` the way the
  /// source `AVMasteringDisplayMetadata` does — `parse_mastering_
  /// display` (`mediaio-ffmpeg`'s `convert` module) is the one
  /// place that resolves each coordinate's own rational to this
  /// shared fixed-point unit, exactly rather than by rounding, for
  /// every producer this crate has observed (which all emit a
  /// denominator of `50000` already).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn display_primaries(&self) -> [(u32, u32); 3] {
    self.display_primaries
  }
  /// White-point chromaticity `(x, y)`, in the same SMPTE ST 2086
  /// fixed-point units as [`Self::display_primaries`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn white_point(&self) -> (u32, u32) {
    self.white_point
  }
  /// Maximum luminance in `0.0001 cd/m²` units (rational `(num, den)`).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_luminance(&self) -> (u32, u32) {
    self.max_luminance
  }
  /// Minimum luminance in `0.0001 cd/m²` units.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn min_luminance(&self) -> (u32, u32) {
    self.min_luminance
  }

  /// Sets the display primaries (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_display_primaries(mut self, value: [(u32, u32); 3]) -> Self {
    self.display_primaries = value;
    self
  }
  /// Sets the white point (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_white_point(mut self, value: (u32, u32)) -> Self {
    self.white_point = value;
    self
  }
  /// Sets the max luminance (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_max_luminance(mut self, value: (u32, u32)) -> Self {
    self.max_luminance = value;
    self
  }
  /// Sets the min luminance (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_min_luminance(mut self, value: (u32, u32)) -> Self {
    self.min_luminance = value;
    self
  }

  /// Sets the display primaries in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_display_primaries(&mut self, value: [(u32, u32); 3]) -> &mut Self {
    self.display_primaries = value;
    self
  }
  /// Sets the white point in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_white_point(&mut self, value: (u32, u32)) -> &mut Self {
    self.white_point = value;
    self
  }
  /// Sets the max luminance in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_luminance(&mut self, value: (u32, u32)) -> &mut Self {
    self.max_luminance = value;
    self
  }
  /// Sets the min luminance in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_min_luminance(&mut self, value: (u32, u32)) -> &mut Self {
    self.min_luminance = value;
    self
  }
}

/// HDR10 content light level (`AV_FRAME_DATA_CONTENT_LIGHT_LEVEL`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct ContentLightLevel {
  max_cll: u32,
  max_fall: u32,
}

impl ContentLightLevel {
  /// Constructs a `ContentLightLevel`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(max_cll: u32, max_fall: u32) -> Self {
    Self { max_cll, max_fall }
  }

  /// Maximum content light level (cd/m²).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_cll(&self) -> u32 {
    self.max_cll
  }
  /// Maximum frame-average light level (cd/m²).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_fall(&self) -> u32 {
    self.max_fall
  }

  /// Sets `max_cll` (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_cll(mut self, value: u32) -> Self {
    self.max_cll = value;
    self
  }
  /// Sets `max_fall` (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_fall(mut self, value: u32) -> Self {
    self.max_fall = value;
    self
  }

  /// Sets `max_cll` in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_cll(&mut self, value: u32) -> &mut Self {
    self.max_cll = value;
    self
  }
  /// Sets `max_fall` in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_fall(&mut self, value: u32) -> &mut Self {
    self.max_fall = value;
    self
  }
}

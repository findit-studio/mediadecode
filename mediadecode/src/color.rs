//! Color metadata: enums for matrix, primaries, transfer, range, and
//! chroma location — all closed-form per ITU-T H.273.

use derive_more::IsVariant;

/// Color matrix coefficients per ITU-T H.273 MatrixCoefficients
/// (Table 4) / ISO/IEC 23001-8.
///
/// Read from `AVFrame.colorspace` / `VideoColorSpace.matrix` /
/// `kCVImageBufferYCbCrMatrixKey`.
///
/// For `AVCOL_SPC_UNSPECIFIED` (value `2`), FFmpeg's convention is
/// `Bt709` for sources with `height >= 720` and `Bt601` otherwise —
/// the caller applies that rule when building `ColorInfo`. The
/// `Default` for this enum is `Bt709` (matches FFmpeg's
/// height-≥-720 default).
///
/// Copied verbatim from `colconv::ColorMatrix` (`#[default]`
/// attribute on `Bt709` is the only addition to enable
/// `ColorInfo::default()`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, IsVariant)]
#[non_exhaustive]
pub enum ColorMatrix {
  /// ITU-R BT.601 (SDTV); also the correct choice for SMPTE170M /
  /// BT470BG (identical coefficients).
  Bt601,
  /// ITU-R BT.709 (HDTV).
  #[default]
  Bt709,
  /// ITU-R BT.2020 non-constant-luminance (UHDTV / HDR10).
  Bt2020Ncl,
  /// SMPTE 240M (legacy 1990s HDTV).
  Smpte240m,
  /// FCC CFR 47 §73.682 (legacy NTSC, very close to BT.601 numerically).
  Fcc,
  /// YCgCo per ITU-T H.273 MatrixCoefficients = 8.
  YCgCo,
}

/// Color primaries per ITU-T H.273 ColourPrimaries (Table 2) /
/// ISO/IEC 23001-8.
///
/// Read from `AVFrame.color_primaries` / `VideoColorSpace.primaries` /
/// `kCVImageBufferColorPrimariesKey`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, IsVariant)]
#[non_exhaustive]
pub enum ColorPrimaries {
  /// ITU-R BT.709 (HDTV).
  Bt709,
  /// Unspecified — caller infers from height.
  #[default]
  Unspecified,
  /// ITU-R BT.470 System M (legacy NTSC).
  Bt470M,
  /// ITU-R BT.470 System BG (PAL/SECAM).
  Bt470Bg,
  /// SMPTE 170M (NTSC SD; same primaries as BT.601).
  Smpte170M,
  /// SMPTE 240M (legacy 1990s HDTV).
  Smpte240M,
  /// Generic film (ITU-T H.273).
  Film,
  /// ITU-R BT.2020 (UHDTV / HDR10).
  Bt2020,
  /// SMPTE ST 428-1 (XYZ).
  SmpteSt428,
  /// SMPTE RP 431-2 (DCI-P3).
  SmpteRp431,
  /// SMPTE EG 432-1 (Display P3).
  SmpteEg432,
  /// EBU Tech. 3213-E (legacy).
  Ebu3213E,
}

/// Transfer characteristics per ITU-T H.273 (Table 3).
///
/// Read from `AVFrame.color_trc` / `VideoColorSpace.transfer` /
/// `kCVImageBufferTransferFunctionKey`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, IsVariant)]
#[non_exhaustive]
pub enum ColorTransfer {
  /// ITU-R BT.709.
  Bt709,
  /// Unspecified.
  #[default]
  Unspecified,
  /// BT.470 System M (gamma 2.2).
  Bt470M,
  /// BT.470 System BG (gamma 2.8).
  Bt470Bg,
  /// SMPTE 170M (BT.601).
  Smpte170M,
  /// SMPTE 240M.
  Smpte240M,
  /// Linear transfer.
  Linear,
  /// Log 100:1.
  Log100,
  /// Log 316.22:1.
  Log316,
  /// IEC 61966-2-4 (xvYCC).
  Iec6196624,
  /// ITU-R BT.1361 ECG.
  Bt1361Ecg,
  /// IEC 61966-2-1 (sRGB).
  Iec6196621,
  /// ITU-R BT.2020 10-bit.
  Bt2020_10Bit,
  /// ITU-R BT.2020 12-bit.
  Bt2020_12Bit,
  /// SMPTE ST 2084 — Perceptual Quantizer (HDR10).
  SmpteSt2084Pq,
  /// SMPTE ST 428.
  SmpteSt428,
  /// ARIB STD-B67 — Hybrid Log-Gamma.
  AribStdB67Hlg,
}

/// Sample range — limited (TV / studio swing) vs. full (PC).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, IsVariant)]
#[non_exhaustive]
pub enum ColorRange {
  /// Unspecified — caller assumes Limited.
  #[default]
  Unspecified,
  /// Limited / studio swing (8-bit luma 16..235, chroma 16..240).
  Limited,
  /// Full / PC swing (8-bit 0..255).
  Full,
}

/// Chroma sample location (for subsampled YUV formats).
///
/// Aligns with H.265 SPS chroma_loc / FFmpeg `AVChromaLocation`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, IsVariant)]
#[non_exhaustive]
pub enum ChromaLocation {
  /// Unspecified.
  #[default]
  Unspecified,
  /// MPEG-2 / H.264 default (chroma at the left of two luma samples).
  Left,
  /// MPEG-1 / JPEG (chroma centered between four luma samples).
  Center,
  /// DV PAL — top-left.
  TopLeft,
  /// Top.
  Top,
  /// Bottom-left.
  BottomLeft,
  /// Bottom.
  Bottom,
}

/// Bundled color metadata that rides on every [`crate::frame::VideoFrame`].
///
/// Every backend except R3D and BRAW exposes color metadata natively;
/// RAW backends populate from clip-level color science and leave
/// `Unspecified` if absent. `ColorInfo::UNSPECIFIED` is the sensible
/// default for RAW backends that don't carry per-frame color data.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorInfo {
  primaries: ColorPrimaries,
  transfer: ColorTransfer,
  matrix: ColorMatrix,
  range: ColorRange,
  chroma_location: ChromaLocation,
}

impl ColorInfo {
  /// All-`Unspecified` color info (for `Default` / RAW-backend use).
  /// Matrix defaults to `Bt709` (matches FFmpeg's height-≥-720
  /// fallback for `AVCOL_SPC_UNSPECIFIED`).
  pub const UNSPECIFIED: Self = Self {
    primaries: ColorPrimaries::Unspecified,
    transfer: ColorTransfer::Unspecified,
    matrix: ColorMatrix::Bt709,
    range: ColorRange::Unspecified,
    chroma_location: ChromaLocation::Unspecified,
  };

  /// Constructs a `ColorInfo` from explicit components.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(
    primaries: ColorPrimaries,
    transfer: ColorTransfer,
    matrix: ColorMatrix,
    range: ColorRange,
    chroma_location: ChromaLocation,
  ) -> Self {
    Self {
      primaries,
      transfer,
      matrix,
      range,
      chroma_location,
    }
  }

  /// Returns the color primaries.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn primaries(&self) -> ColorPrimaries {
    self.primaries
  }

  /// Returns the transfer characteristics.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn transfer(&self) -> ColorTransfer {
    self.transfer
  }

  /// Returns the YUV→RGB matrix coefficients.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn matrix(&self) -> ColorMatrix {
    self.matrix
  }

  /// Returns the sample range (limited / full).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn range(&self) -> ColorRange {
    self.range
  }

  /// Returns the chroma sample location.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn chroma_location(&self) -> ChromaLocation {
    self.chroma_location
  }

  /// Sets the primaries (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_primaries(mut self, v: ColorPrimaries) -> Self {
    self.primaries = v;
    self
  }

  /// Sets the transfer (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_transfer(mut self, v: ColorTransfer) -> Self {
    self.transfer = v;
    self
  }

  /// Sets the matrix (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_matrix(mut self, v: ColorMatrix) -> Self {
    self.matrix = v;
    self
  }

  /// Sets the range (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_range(mut self, v: ColorRange) -> Self {
    self.range = v;
    self
  }

  /// Sets the chroma location (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_chroma_location(mut self, v: ChromaLocation) -> Self {
    self.chroma_location = v;
    self
  }

  /// Sets the primaries in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_primaries(&mut self, v: ColorPrimaries) -> &mut Self {
    self.primaries = v;
    self
  }

  /// Sets the transfer in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_transfer(&mut self, v: ColorTransfer) -> &mut Self {
    self.transfer = v;
    self
  }

  /// Sets the matrix in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_matrix(&mut self, v: ColorMatrix) -> &mut Self {
    self.matrix = v;
    self
  }

  /// Sets the range in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_range(&mut self, v: ColorRange) -> &mut Self {
    self.range = v;
    self
  }

  /// Sets the chroma location in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_chroma_location(&mut self, v: ChromaLocation) -> &mut Self {
    self.chroma_location = v;
    self
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn defaults_match_spec() {
    assert!(matches!(ColorMatrix::default(), ColorMatrix::Bt709));
    assert!(matches!(
      ColorPrimaries::default(),
      ColorPrimaries::Unspecified
    ));
    assert!(matches!(
      ColorTransfer::default(),
      ColorTransfer::Unspecified
    ));
    assert!(matches!(ColorRange::default(), ColorRange::Unspecified));
    assert!(matches!(
      ChromaLocation::default(),
      ChromaLocation::Unspecified
    ));
  }

  #[test]
  fn is_variant_helpers_compile_for_each_enum() {
    assert!(ColorMatrix::Bt709.is_bt_709());
    assert!(ColorPrimaries::Bt2020.is_bt_2020());
    assert!(ColorTransfer::SmpteSt2084Pq.is_smpte_st_2084_pq());
    assert!(ColorRange::Full.is_full());
    assert!(ChromaLocation::Center.is_center());
  }

  #[test]
  fn copy_and_eq() {
    let m1 = ColorMatrix::Bt709;
    let m2 = m1; // Copy
    assert_eq!(m1, m2);
  }

  #[test]
  fn color_info_default_is_unspecified_with_bt709_matrix() {
    let ci = ColorInfo::default();
    assert_eq!(ci, ColorInfo::UNSPECIFIED);
    assert!(ci.primaries().is_unspecified());
    assert!(ci.matrix().is_bt_709());
  }

  #[test]
  fn color_info_builders_chain() {
    let ci = ColorInfo::UNSPECIFIED
      .with_primaries(ColorPrimaries::Bt2020)
      .with_transfer(ColorTransfer::SmpteSt2084Pq)
      .with_matrix(ColorMatrix::Bt2020Ncl)
      .with_range(ColorRange::Limited)
      .with_chroma_location(ChromaLocation::Left);
    assert!(ci.primaries().is_bt_2020());
    assert!(ci.transfer().is_smpte_st_2084_pq());
    assert!(ci.matrix().is_bt_2020_ncl());
    assert!(ci.range().is_limited());
    assert!(ci.chroma_location().is_left());
  }

  #[test]
  fn color_info_setters_chain() {
    let mut ci = ColorInfo::UNSPECIFIED;
    ci.set_primaries(ColorPrimaries::Bt709)
      .set_transfer(ColorTransfer::Bt709)
      .set_matrix(ColorMatrix::Bt709)
      .set_range(ColorRange::Limited)
      .set_chroma_location(ChromaLocation::Left);
    assert!(ci.primaries().is_bt_709());
    assert!(ci.range().is_limited());
  }

  #[test]
  fn color_info_const_construction() {
    const CI: ColorInfo = ColorInfo::new(
      ColorPrimaries::Bt709,
      ColorTransfer::Bt709,
      ColorMatrix::Bt709,
      ColorRange::Limited,
      ChromaLocation::Left,
    );
    assert!(CI.matrix().is_bt_709());
  }
}

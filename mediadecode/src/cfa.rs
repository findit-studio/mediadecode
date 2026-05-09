//! Color-filter-array (Bayer) descriptions.

use derive_more::IsVariant;

/// Bayer pattern — which sensor color sits at the top-left of the
/// repeating 2×2 tile.
///
/// In `Bggr` / `Rggb` the green diagonal runs top-left → bottom-right;
/// in `Grbg` / `Gbrg` the green diagonal runs top-right → bottom-left.
/// Each 2×2 cell carries two greens (one on the red row, one on the
/// blue row), one red, and one blue.
///
/// Source: read from the camera's metadata (R3D `ImagerCFA`, BRAW
/// `cfa_pattern`, NRAW SDK accessor). FFmpeg's bayer pixel formats
/// (`AV_PIX_FMT_BAYER_BGGR8` / `RGGB8` / `GRBG8` / `GBRG8` and the
/// `*_16LE` siblings) carry the pattern in the format identifier
/// itself.
///
/// **Scope.** This enum covers the four standard 2×2 Bayer
/// arrangements only. Other CFA families used by modern professional
/// cameras (Quad Bayer / Sony, X-Trans / Fujifilm, RGBW / BMD URSA
/// 12K, Foveon stacked photosites / Sigma, monochrome / Leica) are
/// tracked separately as future variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, IsVariant)]
#[non_exhaustive]
pub enum BayerPattern {
  /// `B G / G R` — top-left is **B**, bottom-right is **R**.
  Bggr,
  /// `R G / G B` — top-left is **R**, bottom-right is **B**.
  Rggb,
  /// `G R / B G` — top-left is **G** (on the red row), top-right is **R**.
  Grbg,
  /// `G B / R G` — top-left is **G** (on the blue row), top-right is **B**.
  Gbrg,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn variants_construct_and_compare() {
    assert_eq!(BayerPattern::Bggr, BayerPattern::Bggr);
    assert_ne!(BayerPattern::Bggr, BayerPattern::Rggb);
  }

  #[test]
  fn is_variant_helpers_work() {
    assert!(BayerPattern::Bggr.is_bggr());
    assert!(!BayerPattern::Bggr.is_rggb());
  }

  #[cfg(feature = "std")]
  #[test]
  fn copy_and_hash() {
    use std::{
      collections::hash_map::DefaultHasher,
      hash::{Hash, Hasher},
    };
    let p = BayerPattern::Grbg;
    let _copy = p; // doesn't move
    let mut h = DefaultHasher::new();
    p.hash(&mut h);
    let _ = h.finish();
  }
}

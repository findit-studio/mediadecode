# mediadecode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the type-and-trait spine for `mediadecode` — a no_std/wasm-friendly generic abstraction layer over real media decoders — per the design at `docs/superpowers/specs/2026-05-08-mediadecode-design.md`.

**Architecture:** Pure types + trait definitions. No decoder implementation in this crate; concrete adapters (`mediadecode-ffmpeg`, etc.) come later. Each public type has private fields with `const fn` getters and `with_*` / `set_*` builders, mirroring the `mediatime` / `colconv` idiom.

**Tech Stack:** Rust 2024 edition, MSRV 1.95, `mediatime`, `bitflags`, `derive_more` (is_variant), `thiserror` (default-features = false). All deps no_std-clean.

**Reference spec:** `docs/superpowers/specs/2026-05-08-mediadecode-design.md`

**Source-of-truth conventions (lifted from spec §4):**
- Private fields. `pub const fn field()` getters; `pub const fn with_field(mut self, v)` consuming builders; `pub const fn set_field(&mut self, v) -> &mut Self` in-place mutators. `const fn` for `Copy` field types; plain `fn` for non-`Copy` (e.g. `extra: A::FrameExtra`, `data: B`).
- Closed enums: `#[non_exhaustive]`, `#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, IsVariant)]` (Default skipped where there's no sensible default).
- `try_new(...)` validating constructor + panicking `new(...)` sibling for types that need validation.
- `thiserror = { default-features = false }` always — emits `core::error::Error` impls (stable since 1.81).
- `cargo test` and `cargo check` are run after every task.

**Working directory throughout:** `/Users/user/Develop/findit-studio/mediadecode`. All commands assume this cwd.

---

## Task 1: Update Cargo manifest

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Replace the entire `Cargo.toml` content**

```toml
[package]
name = "mediadecode"
version = "0.0.0"
edition = "2024"
repository = "https://github.com/findit-ai/mediadecode"
homepage = "https://github.com/findit-ai/mediadecode"
documentation = "https://docs.rs/mediadecode"
description = "Generic, no_std-friendly type-and-trait spine for media decoders (FFmpeg, WebCodecs, R3D, BRAW, ARRIRAW, X-OCN, ProRes RAW, Canon Cinema RAW Light)."
license = "MIT OR Apache-2.0"
rust-version = "1.95.0"
keywords = ["media", "decoder", "video", "audio", "no-std"]
categories = ["multimedia", "multimedia::video", "multimedia::audio", "no-std", "no-std::no-alloc"]

[features]
default = ["std"]
alloc   = []
std     = ["alloc", "mediatime/std"]

serde      = ["dep:serde", "mediatime/serde", "bitflags/serde"]
arbitrary  = ["dep:arbitrary", "mediatime/arbitrary"]
quickcheck = ["dep:quickcheck", "mediatime/quickcheck"]

[dependencies]
mediatime   = { version = "0.1", default-features = false }
bitflags    = { version = "2", default-features = false }
derive_more = { version = "2", default-features = false, features = ["is_variant"] }
thiserror   = { version = "2", default-features = false }

serde      = { version = "1", default-features = false, features = ["derive"], optional = true }
arbitrary  = { version = "1", default-features = false, optional = true }
quickcheck = { version = "1", default-features = false, optional = true }

[dev-dependencies]
criterion = "0.8"
tempfile  = "3"

[[bench]]
path = "benches/foo.rs"
name = "foo"
harness = false

[profile.bench]
opt-level = 3
debug = false
codegen-units = 1
lto = 'thin'
incremental = false
debug-assertions = false
overflow-checks = false
rpath = false

[package.metadata.docs.rs]
all-features = true
rustdoc-args = ["--cfg", "docsrs"]

[lints.rust]
rust_2018_idioms = "warn"
single_use_lifetimes = "warn"
unexpected_cfgs = { level = "warn", check-cfg = [
  'cfg(all_tests)',
  'cfg(tarpaulin)',
] }
```

- [ ] **Step 2: Verify it parses**

Run: `cargo check --no-default-features --features alloc`
Expected: succeeds (lib is empty so far, just confirms manifest + deps resolve)

Run: `cargo check`
Expected: succeeds with default features.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore: update Cargo manifest for mediadecode (MSRV 1.95, edition 2024, deps)

- Rename crate from template-rs to mediadecode
- MSRV 1.95, edition 2024 (matches colconv/hwdecode)
- Add deps: mediatime (re-exported), bitflags, derive_more, thiserror
- Optional serde/arbitrary/quickcheck features pass through to mediatime
- Drop thiserror/default gating; core::error::Error is stable

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: lib.rs scaffolding

**Files:**
- Modify: `src/lib.rs`

- [ ] **Step 1: Replace the entire `src/lib.rs` content**

```rust
//! Generic, no_std-friendly type-and-trait spine for media decoders.
//!
//! This crate provides a unified vocabulary of `Packet` / `Frame` types
//! and `Adapter` / `Decoder` traits that concrete decoder backends
//! (FFmpeg, WebCodecs, RED R3D, Blackmagic BRAW, ARRIRAW, Sony X-OCN,
//! Apple ProRes RAW, Canon Cinema RAW Light, …) implement. No decoder
//! implementation lives here; backend crates depend on this crate and
//! emit the unified types.
//!
//! See `docs/superpowers/specs/2026-05-08-mediadecode-design.md` for
//! the full design.

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

#[cfg(all(not(feature = "std"), feature = "alloc"))]
extern crate alloc as std;

#[cfg(feature = "std")]
extern crate std;

pub mod adapter;
pub mod cfa;
pub mod color;
pub mod decoder;
pub mod frame;
pub mod packet;
pub mod subtitle;

// Re-export the time primitives so consumers don't have to add a
// separate `mediatime` dependency.
pub use mediatime::{TimeRange, Timebase, Timestamp};
```

- [ ] **Step 2: Create empty placeholder modules so `cargo check` compiles**

Run these in one shot:

```bash
for f in adapter cfa color decoder frame packet subtitle; do
  printf '//! Placeholder — populated by later tasks.\n' > "src/${f}.rs"
done
```

- [ ] **Step 3: Verify everything compiles**

Run: `cargo check`
Expected: succeeds.

Run: `cargo check --no-default-features`
Expected: succeeds (core-only).

Run: `cargo check --no-default-features --features alloc`
Expected: succeeds.

- [ ] **Step 4: Commit**

```bash
git add src/lib.rs src/adapter.rs src/cfa.rs src/color.rs src/decoder.rs src/frame.rs src/packet.rs src/subtitle.rs
git commit -m "feat: lib.rs scaffolding with module skeleton and mediatime re-exports

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: cfa.rs — BayerPattern

**Files:**
- Modify: `src/cfa.rs`

- [ ] **Step 1: Write the failing test**

Replace `src/cfa.rs` content with:

```rust
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

    #[test]
    fn copy_and_hash() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let p = BayerPattern::Grbg;
        let _copy = p; // doesn't move
        let mut h = DefaultHasher::new();
        p.hash(&mut h);
        let _ = h.finish();
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --lib cfa`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/cfa.rs
git commit -m "feat(cfa): BayerPattern (copied from colconv::raw)

Four standard 2x2 Bayer arrangements (BGGR/RGGB/GRBG/GBRG).
Copied verbatim from colconv::raw::BayerPattern; both crates carry
this type in parallel until a future colconv migration imports it
from mediadecode.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: color.rs — Closed enums (ColorMatrix, ColorPrimaries, ColorTransfer, ColorRange, ChromaLocation)

**Files:**
- Modify: `src/color.rs`

- [ ] **Step 1: Write the file with all five enums and unit tests**

Replace `src/color.rs` content with:

```rust
//! Color-science enums (H.273 / CICP aligned) and the `ColorInfo`
//! bundle that rides on every `VideoFrame`.

use derive_more::IsVariant;

/// YUV → RGB conversion matrix coefficients.
///
/// Read from `AVFrame.colorspace` (FFmpeg) / `VideoColorSpace.matrix`
/// (WebCodecs) / `kCVImageBufferYCbCrMatrixKey` (CVPixelBuffer
/// attachments). H.273 MatrixCoefficients (Table 4) numbering:
///
/// | `AVCOL_SPC_*`              | Variant      | Note                                    |
/// |---                         |---           |---                                      |
/// | `BT709`                    | `Bt709`      | HDTV default                            |
/// | `BT2020_NCL`               | `Bt2020Ncl`  | UHDTV / HDR10                           |
/// | `SMPTE170M`                | `Bt601`      | NTSC SD; identical coeffs to BT.601     |
/// | `BT470BG`                  | `Bt601`      | PAL/SECAM SD; identical coeffs          |
/// | `SMPTE240M`                | `Smpte240m`  | legacy HD                               |
/// | `FCC`                      | `Fcc`        | legacy NTSC variant                     |
/// | `YCGCO`                    | `YCgCo`      | screen-codec intra / alpha (H.273)      |
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        assert!(matches!(ColorMatrix::default(), ColorMatrix::Bt709));
        assert!(matches!(ColorPrimaries::default(), ColorPrimaries::Unspecified));
        assert!(matches!(ColorTransfer::default(), ColorTransfer::Unspecified));
        assert!(matches!(ColorRange::default(), ColorRange::Unspecified));
        assert!(matches!(ChromaLocation::default(), ChromaLocation::Unspecified));
    }

    #[test]
    fn is_variant_helpers_compile_for_each_enum() {
        assert!(ColorMatrix::Bt709.is_bt709());
        assert!(ColorPrimaries::Bt2020.is_bt2020());
        assert!(ColorTransfer::SmpteSt2084Pq.is_smpte_st2084_pq());
        assert!(ColorRange::Full.is_full());
        assert!(ChromaLocation::Center.is_center());
    }

    #[test]
    fn copy_and_eq() {
        let m1 = ColorMatrix::Bt709;
        let m2 = m1; // Copy
        assert_eq!(m1, m2);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib color`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/color.rs
git commit -m "feat(color): H.273-aligned closed enums (ColorMatrix, ColorPrimaries, ColorTransfer, ColorRange, ChromaLocation)

ColorMatrix copied from colconv::ColorMatrix with #[default] Bt709 added
so ColorInfo can derive Default. The other four are new H.273-style
siblings: ColorPrimaries (Table 2), ColorTransfer (Table 3),
ColorRange (full/limited), ChromaLocation. All #[non_exhaustive],
all derive_more::IsVariant.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: color.rs — ColorInfo bundle

**Files:**
- Modify: `src/color.rs` (append below the enums)

- [ ] **Step 1: Append the `ColorInfo` struct and tests to `src/color.rs`**

Insert this content after the last enum (`ChromaLocation`) and before `#[cfg(test)] mod tests {`:

```rust
/// Bundled color metadata that rides on every [`crate::frame::VideoFrame`].
///
/// Every backend except R3D and BRAW exposes color metadata natively;
/// RAW backends populate from clip-level color science and leave
/// `Unspecified` if absent. `ColorInfo::UNSPECIFIED` is the sensible
/// default for RAW backends that don't carry per-frame color data.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorInfo {
    primaries:       ColorPrimaries,
    transfer:        ColorTransfer,
    matrix:          ColorMatrix,
    range:           ColorRange,
    chroma_location: ChromaLocation,
}

impl ColorInfo {
    /// All-`Unspecified` color info (for `Default` / RAW-backend use).
    /// Matrix defaults to `Bt709` (matches FFmpeg's height-≥-720
    /// fallback for `AVCOL_SPC_UNSPECIFIED`).
    pub const UNSPECIFIED: Self = Self {
        primaries:       ColorPrimaries::Unspecified,
        transfer:        ColorTransfer::Unspecified,
        matrix:          ColorMatrix::Bt709,
        range:           ColorRange::Unspecified,
        chroma_location: ChromaLocation::Unspecified,
    };

    /// Constructs a `ColorInfo` from explicit components.
    #[inline]
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
    #[inline]
    pub const fn primaries(&self) -> ColorPrimaries {
        self.primaries
    }

    /// Returns the transfer characteristics.
    #[inline]
    pub const fn transfer(&self) -> ColorTransfer {
        self.transfer
    }

    /// Returns the YUV→RGB matrix coefficients.
    #[inline]
    pub const fn matrix(&self) -> ColorMatrix {
        self.matrix
    }

    /// Returns the sample range (limited / full).
    #[inline]
    pub const fn range(&self) -> ColorRange {
        self.range
    }

    /// Returns the chroma sample location.
    #[inline]
    pub const fn chroma_location(&self) -> ChromaLocation {
        self.chroma_location
    }

    /// Sets the primaries (consuming builder).
    #[inline]
    pub const fn with_primaries(mut self, v: ColorPrimaries) -> Self {
        self.primaries = v;
        self
    }

    /// Sets the transfer (consuming builder).
    #[inline]
    pub const fn with_transfer(mut self, v: ColorTransfer) -> Self {
        self.transfer = v;
        self
    }

    /// Sets the matrix (consuming builder).
    #[inline]
    pub const fn with_matrix(mut self, v: ColorMatrix) -> Self {
        self.matrix = v;
        self
    }

    /// Sets the range (consuming builder).
    #[inline]
    pub const fn with_range(mut self, v: ColorRange) -> Self {
        self.range = v;
        self
    }

    /// Sets the chroma location (consuming builder).
    #[inline]
    pub const fn with_chroma_location(mut self, v: ChromaLocation) -> Self {
        self.chroma_location = v;
        self
    }

    /// Sets the primaries in place.
    #[inline]
    pub const fn set_primaries(&mut self, v: ColorPrimaries) -> &mut Self {
        self.primaries = v;
        self
    }

    /// Sets the transfer in place.
    #[inline]
    pub const fn set_transfer(&mut self, v: ColorTransfer) -> &mut Self {
        self.transfer = v;
        self
    }

    /// Sets the matrix in place.
    #[inline]
    pub const fn set_matrix(&mut self, v: ColorMatrix) -> &mut Self {
        self.matrix = v;
        self
    }

    /// Sets the range in place.
    #[inline]
    pub const fn set_range(&mut self, v: ColorRange) -> &mut Self {
        self.range = v;
        self
    }

    /// Sets the chroma location in place.
    #[inline]
    pub const fn set_chroma_location(&mut self, v: ChromaLocation) -> &mut Self {
        self.chroma_location = v;
        self
    }
}
```

Add these tests inside the existing `#[cfg(test)] mod tests { ... }` block at the bottom of the file (append before its closing `}`):

```rust
    #[test]
    fn color_info_default_is_unspecified_with_bt709_matrix() {
        let ci = ColorInfo::default();
        assert_eq!(ci, ColorInfo::UNSPECIFIED);
        assert!(ci.primaries().is_unspecified());
        assert!(ci.matrix().is_bt709());
    }

    #[test]
    fn color_info_builders_chain() {
        let ci = ColorInfo::UNSPECIFIED
            .with_primaries(ColorPrimaries::Bt2020)
            .with_transfer(ColorTransfer::SmpteSt2084Pq)
            .with_matrix(ColorMatrix::Bt2020Ncl)
            .with_range(ColorRange::Limited)
            .with_chroma_location(ChromaLocation::Left);
        assert!(ci.primaries().is_bt2020());
        assert!(ci.transfer().is_smpte_st2084_pq());
        assert!(ci.matrix().is_bt2020_ncl());
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
        assert!(ci.primaries().is_bt709());
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
        assert!(CI.matrix().is_bt709());
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib color`
Expected: 7 tests pass (3 from Task 4 + 4 new).

- [ ] **Step 3: Commit**

```bash
git add src/color.rs
git commit -m "feat(color): ColorInfo bundle with const fn getters/builders/setters

ColorInfo wraps the five H.273 enums into a single field on VideoFrame.
UNSPECIFIED const for the default; full set of with_* / set_* builders
following the mediatime idiom. Default impl returns UNSPECIFIED with
ColorMatrix=Bt709 (FFmpeg's height>=720 fallback).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: frame.rs — Rect

**Files:**
- Modify: `src/frame.rs`

- [ ] **Step 1: Replace `src/frame.rs` content with `Rect` plus tests**

```rust
//! Frame types and supporting building blocks.
//!
//! `Rect` and `Plane<B>` are the shared building blocks. The full
//! `VideoFrame` / `AudioFrame` / `SubtitleFrame` types land in later
//! tasks.

/// An axis-aligned integer rectangle.
///
/// Used for `VideoFrame::visible_rect` (FFmpeg crop /
/// WebCodecs `visibleRect` / ProRes RAW `CleanAperture`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    x:      u32,
    y:      u32,
    width:  u32,
    height: u32,
}

impl Rect {
    /// Constructs a `Rect` at `(x, y)` with the given size.
    #[inline]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    /// Returns the X coordinate of the top-left corner.
    #[inline]
    pub const fn x(&self) -> u32 { self.x }

    /// Returns the Y coordinate of the top-left corner.
    #[inline]
    pub const fn y(&self) -> u32 { self.y }

    /// Returns the width.
    #[inline]
    pub const fn width(&self) -> u32 { self.width }

    /// Returns the height.
    #[inline]
    pub const fn height(&self) -> u32 { self.height }

    /// Sets the X coordinate (consuming builder).
    #[inline]
    pub const fn with_x(mut self, x: u32) -> Self { self.x = x; self }
    /// Sets the Y coordinate (consuming builder).
    #[inline]
    pub const fn with_y(mut self, y: u32) -> Self { self.y = y; self }
    /// Sets the width (consuming builder).
    #[inline]
    pub const fn with_width(mut self, w: u32) -> Self { self.width = w; self }
    /// Sets the height (consuming builder).
    #[inline]
    pub const fn with_height(mut self, h: u32) -> Self { self.height = h; self }

    /// Sets the X coordinate in place.
    #[inline]
    pub const fn set_x(&mut self, x: u32) -> &mut Self { self.x = x; self }
    /// Sets the Y coordinate in place.
    #[inline]
    pub const fn set_y(&mut self, y: u32) -> &mut Self { self.y = y; self }
    /// Sets the width in place.
    #[inline]
    pub const fn set_width(&mut self, w: u32) -> &mut Self { self.width = w; self }
    /// Sets the height in place.
    #[inline]
    pub const fn set_height(&mut self, h: u32) -> &mut Self { self.height = h; self }
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
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib frame`
Expected: 5 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/frame.rs
git commit -m "feat(frame): Rect with const fn getters/builders/setters

Used for VideoFrame::visible_rect (FFmpeg crop, WebCodecs visibleRect,
ProRes CleanAperture).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: frame.rs — Plane<B>

**Files:**
- Modify: `src/frame.rs` (append `Plane<B>` after `Rect`'s impl, before the existing `mod tests`)

- [ ] **Step 1: Insert `Plane<B>` after the `impl Rect` block in `src/frame.rs`**

Add this code after the `impl Rect { ... }` block and before the `#[cfg(test)] mod tests {` block:

```rust
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
    data:   B,
    stride: u32,
}

impl<B> Plane<B> {
    /// Constructs a `Plane` from a buffer and a stride.
    #[inline]
    pub const fn new(data: B, stride: u32) -> Self {
        Self { data, stride }
    }

    /// Returns the stride in bytes.
    #[inline]
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// Borrows the underlying buffer.
    #[inline]
    pub const fn data(&self) -> &B {
        &self.data
    }

    /// Mutably borrows the underlying buffer.
    #[inline]
    pub fn data_mut(&mut self) -> &mut B {
        &mut self.data
    }

    /// Consumes the plane and returns the underlying buffer.
    #[inline]
    pub fn into_data(self) -> B {
        self.data
    }

    /// Sets the stride (consuming builder).
    #[inline]
    pub const fn with_stride(mut self, stride: u32) -> Self {
        self.stride = stride;
        self
    }

    /// Sets the stride in place.
    #[inline]
    pub const fn set_stride(&mut self, stride: u32) -> &mut Self {
        self.stride = stride;
        self
    }
}
```

Append these tests inside the existing `mod tests { ... }`:

```rust
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
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib frame`
Expected: 8 tests pass (5 from Rect + 3 from Plane).

- [ ] **Step 3: Commit**

```bash
git add src/frame.rs
git commit -m "feat(frame): Plane<B> generic-over-buffer plane carrier

Plane<B> bounds B at use sites (B: AsRef<[u8]> on Frame), not on
Plane itself, so the type stays usable in const contexts and with
buffer types that don't impl AsRef<[u8]> for unrelated reasons.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: packet.rs — PacketFlags

**Files:**
- Modify: `src/packet.rs`

- [ ] **Step 1: Replace `src/packet.rs` content with `PacketFlags` plus tests**

```rust
//! Compressed `Packet` types and `PacketFlags`.
//!
//! The Packet types proper land in later tasks; this module starts
//! with `PacketFlags` so dependent types can use it.

use bitflags::bitflags;

bitflags! {
    /// Per-packet flags.
    ///
    /// Bit values are the public API:
    /// - `KEY = 0b001` — packet starts a keyframe (FFmpeg `AV_PKT_FLAG_KEY`,
    ///   WebCodecs `'key'`, ProRes RAW absence of
    ///   `kCMSampleAttachmentKey_NotSync`).
    /// - `CORRUPT = 0b010` — packet is known-corrupt (FFmpeg
    ///   `AV_PKT_FLAG_CORRUPT`).
    /// - `DISCARD = 0b100` — packet should be skipped during reconstruction
    ///   (FFmpeg `AV_PKT_FLAG_DISCARD`).
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct PacketFlags: u8 {
        /// Keyframe / sync sample.
        const KEY     = 0b001;
        /// Bitstream-level corruption known.
        const CORRUPT = 0b010;
        /// Demuxer hint: skip this packet.
        const DISCARD = 0b100;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_bits_are_stable() {
        assert_eq!(PacketFlags::KEY.bits(), 0b001);
        assert_eq!(PacketFlags::CORRUPT.bits(), 0b010);
        assert_eq!(PacketFlags::DISCARD.bits(), 0b100);
    }

    #[test]
    fn flags_combine() {
        let f = PacketFlags::KEY | PacketFlags::CORRUPT;
        assert!(f.contains(PacketFlags::KEY));
        assert!(f.contains(PacketFlags::CORRUPT));
        assert!(!f.contains(PacketFlags::DISCARD));
    }

    #[test]
    fn empty_default() {
        assert_eq!(PacketFlags::default(), PacketFlags::empty());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib packet`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/packet.rs
git commit -m "feat(packet): PacketFlags (KEY/CORRUPT/DISCARD bitflags)

Mirrors FFmpeg AV_PKT_FLAG_* with stable bit positions. Other backends
(WebCodecs key/delta, ProRes RAW NotSync attachment) map onto these.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: adapter.rs — three Adapter traits

**Files:**
- Modify: `src/adapter.rs`

- [ ] **Step 1: Replace `src/adapter.rs` content**

```rust
//! Adapter traits — the per-kind backend "vocabulary."
//!
//! A backend implements only the kinds it handles. R3D / BRAW /
//! ARRIRAW / X-OCN / Canon RAW Light implement only [`VideoAdapter`].
//! FFmpeg implements all three. The buffer type is **not** part of
//! these traits — it's a struct generic on `Packet` / `Frame` so the
//! same adapter can be used with different buffer types at different
//! call sites.

use core::fmt::Debug;

/// Backend vocabulary for compressed/decoded **video**.
pub trait VideoAdapter {
    /// Codec identifier (e.g. backend-specific newtype around
    /// FFmpeg `AVCodecID`, WebCodecs codec string, etc.).
    type CodecId: Copy + Eq + Debug;
    /// Pixel format identifier (e.g. backend-specific newtype around
    /// FFmpeg `AVPixelFormat`, WebCodecs `VideoPixelFormat`, RAW
    /// `VideoPixelType`, BRAW `BlackmagicRawResourceFormat`).
    type PixelFormat: Copy + Eq + Debug;
    /// Backend-specific extras carried on every `VideoPacket` (e.g.
    /// FFmpeg side-data, WebCodecs metadata).
    type PacketExtra;
    /// Backend-specific extras carried on every `VideoFrame` (e.g.
    /// HDR mastering display, RAW sensor metadata, picture type).
    type FrameExtra;
}

/// Backend vocabulary for compressed/decoded **audio**.
pub trait AudioAdapter {
    /// Codec identifier.
    type CodecId: Copy + Eq + Debug;
    /// Sample format identifier (e.g. FFmpeg `AVSampleFormat`,
    /// WebCodecs `AudioSampleFormat`).
    type SampleFormat: Copy + Eq + Debug;
    /// Channel layout identifier (FFmpeg `AVChannelLayout`,
    /// WebCodecs raw count, RAW SDK fixed layouts).
    type ChannelLayout: Clone + Eq + Debug;
    /// Backend-specific extras carried on every `AudioPacket`.
    type PacketExtra;
    /// Backend-specific extras carried on every `AudioFrame`.
    type FrameExtra;
}

/// Backend vocabulary for compressed/decoded **subtitles**.
pub trait SubtitleAdapter {
    /// Codec identifier.
    type CodecId: Copy + Eq + Debug;
    /// Backend-specific extras carried on every `SubtitlePacket`.
    type PacketExtra;
    /// Backend-specific extras carried on every `SubtitleFrame`.
    type FrameExtra;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zero-sized "loopback" adapter that implements all three traits
    /// with `()` extras. Proves the traits are object-safe-ish in the
    /// associated-type sense (i.e. they can be implemented).
    pub struct Loopback;

    impl VideoAdapter for Loopback {
        type CodecId = u32;
        type PixelFormat = u32;
        type PacketExtra = ();
        type FrameExtra = ();
    }

    impl AudioAdapter for Loopback {
        type CodecId = u32;
        type SampleFormat = u32;
        type ChannelLayout = u32;
        type PacketExtra = ();
        type FrameExtra = ();
    }

    impl SubtitleAdapter for Loopback {
        type CodecId = u32;
        type PacketExtra = ();
        type FrameExtra = ();
    }

    #[test]
    fn loopback_compiles() {
        // The fact that this test compiles means the three traits
        // are implementable. No runtime assertions necessary.
        fn _video<A: VideoAdapter>() {}
        fn _audio<A: AudioAdapter>() {}
        fn _subtitle<A: SubtitleAdapter>() {}
        _video::<Loopback>();
        _audio::<Loopback>();
        _subtitle::<Loopback>();
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib adapter`
Expected: 1 test passes.

- [ ] **Step 3: Commit**

```bash
git add src/adapter.rs
git commit -m "feat(adapter): VideoAdapter, AudioAdapter, SubtitleAdapter traits

Three independent adapter traits so a video-only backend (R3D, BRAW,
ARRIRAW, X-OCN, Canon RAW Light) implements only what it supports.
No 'type Buffer' associated type — buffer is a struct generic on
Packet/Frame so the same adapter can be used with different buffer
types at different call sites.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: packet.rs — VideoPacket<A, B>

**Files:**
- Modify: `src/packet.rs` (append `VideoPacket` and tests)

- [ ] **Step 1: Insert `VideoPacket<A, B>` after the `bitflags!` block in `src/packet.rs`**

Append after the `bitflags!` macro and before the existing `#[cfg(test)] mod tests {`:

```rust
use crate::Timestamp;
use crate::adapter::VideoAdapter;

/// A compressed video packet.
///
/// Generic over the [`VideoAdapter`] (which contributes
/// `A::PacketExtra`) and the buffer type `B: AsRef<[u8]>`.
///
/// `pts` / `dts` / `duration` are `Option<Timestamp>` because not
/// every backend supplies all three (WebCodecs `EncodedVideoChunk`
/// has no DTS; vendor RAW SDKs that produce packets at all derive
/// timestamps from frame index × fps).
pub struct VideoPacket<A: VideoAdapter, B: AsRef<[u8]>> {
    pts:      Option<Timestamp>,
    dts:      Option<Timestamp>,
    duration: Option<Timestamp>,
    flags:    PacketFlags,
    data:     B,
    extra:    A::PacketExtra,
}

impl<A: VideoAdapter, B: AsRef<[u8]>> VideoPacket<A, B> {
    /// Constructs a `VideoPacket` from `data` and `extra`. All
    /// timestamps default to `None` and flags to empty.
    #[inline]
    pub fn new(data: B, extra: A::PacketExtra) -> Self {
        Self {
            pts: None,
            dts: None,
            duration: None,
            flags: PacketFlags::empty(),
            data,
            extra,
        }
    }

    /// Returns the presentation timestamp.
    #[inline]
    pub const fn pts(&self) -> Option<Timestamp> { self.pts }
    /// Returns the decompression timestamp.
    #[inline]
    pub const fn dts(&self) -> Option<Timestamp> { self.dts }
    /// Returns the packet duration.
    #[inline]
    pub const fn duration(&self) -> Option<Timestamp> { self.duration }
    /// Returns the packet flags.
    #[inline]
    pub const fn flags(&self) -> PacketFlags { self.flags }
    /// Returns the compressed data buffer.
    #[inline]
    pub const fn data(&self) -> &B { &self.data }
    /// Returns the backend-specific extras.
    #[inline]
    pub const fn extra(&self) -> &A::PacketExtra { &self.extra }
    /// Returns a mutable reference to the backend-specific extras.
    #[inline]
    pub fn extra_mut(&mut self) -> &mut A::PacketExtra { &mut self.extra }
    /// Consumes the packet and returns the buffer.
    #[inline]
    pub fn into_data(self) -> B { self.data }
    /// Consumes the packet and returns `(buffer, extras)`.
    #[inline]
    pub fn into_parts(self) -> (B, A::PacketExtra) { (self.data, self.extra) }

    /// Sets the PTS (consuming builder).
    #[inline]
    pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self { self.pts = v; self }
    /// Sets the DTS (consuming builder).
    #[inline]
    pub const fn with_dts(mut self, v: Option<Timestamp>) -> Self { self.dts = v; self }
    /// Sets the duration (consuming builder).
    #[inline]
    pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self { self.duration = v; self }
    /// Sets the flags (consuming builder).
    #[inline]
    pub const fn with_flags(mut self, v: PacketFlags) -> Self { self.flags = v; self }

    /// Sets the PTS in place.
    #[inline]
    pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self { self.pts = v; self }
    /// Sets the DTS in place.
    #[inline]
    pub const fn set_dts(&mut self, v: Option<Timestamp>) -> &mut Self { self.dts = v; self }
    /// Sets the duration in place.
    #[inline]
    pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self { self.duration = v; self }
    /// Sets the flags in place.
    #[inline]
    pub const fn set_flags(&mut self, v: PacketFlags) -> &mut Self { self.flags = v; self }
}
```

Append these tests inside the existing `mod tests { ... }` block:

```rust
    use crate::Timebase;
    use core::num::NonZeroU32;

    struct VLoop;
    impl crate::adapter::VideoAdapter for VLoop {
        type CodecId = u32;
        type PixelFormat = u32;
        type PacketExtra = ();
        type FrameExtra = ();
    }

    fn ms_tb() -> Timebase {
        Timebase::new(1, NonZeroU32::new(1000).unwrap())
    }

    #[test]
    fn video_packet_construct_and_access() {
        let data: &[u8] = &[1, 2, 3];
        let p: VideoPacket<VLoop, &[u8]> = VideoPacket::new(data, ());
        assert_eq!(p.pts(), None);
        assert_eq!(p.flags(), PacketFlags::empty());
        assert_eq!(*p.data(), data);
    }

    #[test]
    fn video_packet_builders_chain() {
        let pts = crate::Timestamp::new(1500, ms_tb());
        let p: VideoPacket<VLoop, &[u8]> = VideoPacket::new(&[][..], ())
            .with_pts(Some(pts))
            .with_flags(PacketFlags::KEY);
        assert_eq!(p.pts(), Some(pts));
        assert!(p.flags().contains(PacketFlags::KEY));
    }

    #[test]
    fn video_packet_into_parts() {
        let p: VideoPacket<VLoop, &[u8]> = VideoPacket::new(&[1u8, 2][..], ());
        let (data, _extra) = p.into_parts();
        assert_eq!(data, &[1, 2]);
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib packet`
Expected: 6 tests pass (3 PacketFlags + 3 VideoPacket).

- [ ] **Step 3: Commit**

```bash
git add src/packet.rs
git commit -m "feat(packet): VideoPacket<A, B> with full accessor/builder/setter surface

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: packet.rs — AudioPacket<A, B>

**Files:**
- Modify: `src/packet.rs` (append `AudioPacket` and tests)

- [ ] **Step 1: Insert `AudioPacket<A, B>` after the `VideoPacket` impl block**

Append after the `impl<A: VideoAdapter, B: AsRef<[u8]>> VideoPacket<A, B> { ... }` block:

```rust
use crate::adapter::AudioAdapter;

/// A compressed audio packet.
pub struct AudioPacket<A: AudioAdapter, B: AsRef<[u8]>> {
    pts:      Option<Timestamp>,
    dts:      Option<Timestamp>,
    duration: Option<Timestamp>,
    flags:    PacketFlags,
    data:     B,
    extra:    A::PacketExtra,
}

impl<A: AudioAdapter, B: AsRef<[u8]>> AudioPacket<A, B> {
    /// Constructs an `AudioPacket` from `data` and `extra`.
    #[inline]
    pub fn new(data: B, extra: A::PacketExtra) -> Self {
        Self {
            pts: None,
            dts: None,
            duration: None,
            flags: PacketFlags::empty(),
            data,
            extra,
        }
    }

    /// Returns the presentation timestamp.
    #[inline]
    pub const fn pts(&self) -> Option<Timestamp> { self.pts }
    /// Returns the decompression timestamp.
    #[inline]
    pub const fn dts(&self) -> Option<Timestamp> { self.dts }
    /// Returns the duration.
    #[inline]
    pub const fn duration(&self) -> Option<Timestamp> { self.duration }
    /// Returns the flags.
    #[inline]
    pub const fn flags(&self) -> PacketFlags { self.flags }
    /// Returns the compressed audio data.
    #[inline]
    pub const fn data(&self) -> &B { &self.data }
    /// Returns the backend extras.
    #[inline]
    pub const fn extra(&self) -> &A::PacketExtra { &self.extra }
    /// Returns a mutable reference to the backend extras.
    #[inline]
    pub fn extra_mut(&mut self) -> &mut A::PacketExtra { &mut self.extra }
    /// Consumes the packet and returns the buffer.
    #[inline]
    pub fn into_data(self) -> B { self.data }
    /// Consumes the packet and returns `(buffer, extras)`.
    #[inline]
    pub fn into_parts(self) -> (B, A::PacketExtra) { (self.data, self.extra) }

    /// Sets the PTS (consuming builder).
    #[inline]
    pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self { self.pts = v; self }
    /// Sets the DTS (consuming builder).
    #[inline]
    pub const fn with_dts(mut self, v: Option<Timestamp>) -> Self { self.dts = v; self }
    /// Sets the duration (consuming builder).
    #[inline]
    pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self { self.duration = v; self }
    /// Sets the flags (consuming builder).
    #[inline]
    pub const fn with_flags(mut self, v: PacketFlags) -> Self { self.flags = v; self }

    /// Sets the PTS in place.
    #[inline]
    pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self { self.pts = v; self }
    /// Sets the DTS in place.
    #[inline]
    pub const fn set_dts(&mut self, v: Option<Timestamp>) -> &mut Self { self.dts = v; self }
    /// Sets the duration in place.
    #[inline]
    pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self { self.duration = v; self }
    /// Sets the flags in place.
    #[inline]
    pub const fn set_flags(&mut self, v: PacketFlags) -> &mut Self { self.flags = v; self }
}
```

Append to the existing `mod tests { ... }`:

```rust
    struct ALoop;
    impl crate::adapter::AudioAdapter for ALoop {
        type CodecId = u32;
        type SampleFormat = u32;
        type ChannelLayout = u32;
        type PacketExtra = ();
        type FrameExtra = ();
    }

    #[test]
    fn audio_packet_round_trip() {
        let data: &[u8] = &[7, 8, 9];
        let p: AudioPacket<ALoop, &[u8]> = AudioPacket::new(data, ())
            .with_flags(PacketFlags::KEY);
        assert_eq!(*p.data(), data);
        assert!(p.flags().contains(PacketFlags::KEY));
        let (recovered, _) = p.into_parts();
        assert_eq!(recovered, data);
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib packet`
Expected: 7 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/packet.rs
git commit -m "feat(packet): AudioPacket<A, B> mirroring VideoPacket shape

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 12: packet.rs — SubtitlePacket<A, B>

**Files:**
- Modify: `src/packet.rs` (append `SubtitlePacket` and tests)

- [ ] **Step 1: Insert `SubtitlePacket<A, B>` after the `AudioPacket` impl block**

Append after the `impl<A: AudioAdapter, B: AsRef<[u8]>> AudioPacket<A, B> { ... }` block:

```rust
use crate::adapter::SubtitleAdapter;

/// A compressed subtitle packet.
pub struct SubtitlePacket<A: SubtitleAdapter, B: AsRef<[u8]>> {
    pts:      Option<Timestamp>,
    duration: Option<Timestamp>,
    flags:    PacketFlags,
    data:     B,
    extra:    A::PacketExtra,
}

impl<A: SubtitleAdapter, B: AsRef<[u8]>> SubtitlePacket<A, B> {
    /// Constructs a `SubtitlePacket` from `data` and `extra`.
    #[inline]
    pub fn new(data: B, extra: A::PacketExtra) -> Self {
        Self {
            pts: None,
            duration: None,
            flags: PacketFlags::empty(),
            data,
            extra,
        }
    }

    /// Returns the presentation timestamp.
    #[inline]
    pub const fn pts(&self) -> Option<Timestamp> { self.pts }
    /// Returns the duration.
    #[inline]
    pub const fn duration(&self) -> Option<Timestamp> { self.duration }
    /// Returns the flags.
    #[inline]
    pub const fn flags(&self) -> PacketFlags { self.flags }
    /// Returns the compressed subtitle data.
    #[inline]
    pub const fn data(&self) -> &B { &self.data }
    /// Returns the backend extras.
    #[inline]
    pub const fn extra(&self) -> &A::PacketExtra { &self.extra }
    /// Returns a mutable reference to the backend extras.
    #[inline]
    pub fn extra_mut(&mut self) -> &mut A::PacketExtra { &mut self.extra }
    /// Consumes the packet and returns the buffer.
    #[inline]
    pub fn into_data(self) -> B { self.data }
    /// Consumes the packet and returns `(buffer, extras)`.
    #[inline]
    pub fn into_parts(self) -> (B, A::PacketExtra) { (self.data, self.extra) }

    /// Sets the PTS (consuming builder).
    #[inline]
    pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self { self.pts = v; self }
    /// Sets the duration (consuming builder).
    #[inline]
    pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self { self.duration = v; self }
    /// Sets the flags (consuming builder).
    #[inline]
    pub const fn with_flags(mut self, v: PacketFlags) -> Self { self.flags = v; self }

    /// Sets the PTS in place.
    #[inline]
    pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self { self.pts = v; self }
    /// Sets the duration in place.
    #[inline]
    pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self { self.duration = v; self }
    /// Sets the flags in place.
    #[inline]
    pub const fn set_flags(&mut self, v: PacketFlags) -> &mut Self { self.flags = v; self }
}
```

Append to the existing `mod tests { ... }`:

```rust
    struct SLoop;
    impl crate::adapter::SubtitleAdapter for SLoop {
        type CodecId = u32;
        type PacketExtra = ();
        type FrameExtra = ();
    }

    #[test]
    fn subtitle_packet_round_trip() {
        let data: &[u8] = b"hi";
        let p: SubtitlePacket<SLoop, &[u8]> = SubtitlePacket::new(data, ());
        assert_eq!(*p.data(), data);
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib packet`
Expected: 8 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/packet.rs
git commit -m "feat(packet): SubtitlePacket<A, B>

No DTS field on SubtitlePacket — subtitle streams have no decode-vs-
presentation reorder.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 13: subtitle.rs — SubtitlePayload + BitmapRegion

**Files:**
- Modify: `src/subtitle.rs`

- [ ] **Step 1: Replace `src/subtitle.rs` content**

```rust
//! Decoded subtitle payload.
//!
//! Mirrors `AVSubtitle`'s text-or-bitmap split. `Text` works under
//! pure `core`; `Bitmap` requires the `alloc` feature because it
//! holds a `Vec<BitmapRegion>` (FFmpeg subtitles can carry many
//! rectangles per frame, so a fixed-size array is impractical).

use core::fmt::Debug;

/// One bitmap subtitle region (rectangle of paletted pixels).
///
/// Mirrors `AVSubtitleRect` for bitmap subtitles. `palette` and
/// `data` use the buffer type `B` so callers can pick the storage.
/// Plane stride and palette length are stored as `u32` for parity
/// with the rest of the crate's geometry conventions.
#[cfg(feature = "alloc")]
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
#[derive(Debug, Clone)]
pub struct BitmapRegion<B: AsRef<[u8]>> {
    x:        u32,
    y:        u32,
    width:    u32,
    height:   u32,
    /// Bytes per row of `data`.
    stride:   u32,
    /// Paletted pixel data; one byte per pixel, indices into `palette`.
    data:     B,
    /// RGBA palette (4 bytes per entry).
    palette:  B,
}

#[cfg(feature = "alloc")]
impl<B: AsRef<[u8]>> BitmapRegion<B> {
    /// Constructs a `BitmapRegion`.
    #[inline]
    pub const fn new(
        x: u32, y: u32, width: u32, height: u32, stride: u32,
        data: B, palette: B,
    ) -> Self {
        Self { x, y, width, height, stride, data, palette }
    }

    /// Returns the X coordinate of the region's top-left.
    #[inline]
    pub const fn x(&self) -> u32 { self.x }
    /// Returns the Y coordinate of the region's top-left.
    #[inline]
    pub const fn y(&self) -> u32 { self.y }
    /// Returns the region's width.
    #[inline]
    pub const fn width(&self) -> u32 { self.width }
    /// Returns the region's height.
    #[inline]
    pub const fn height(&self) -> u32 { self.height }
    /// Returns the stride in bytes.
    #[inline]
    pub const fn stride(&self) -> u32 { self.stride }
    /// Returns the paletted pixel data.
    #[inline]
    pub const fn data(&self) -> &B { &self.data }
    /// Returns the RGBA palette.
    #[inline]
    pub const fn palette(&self) -> &B { &self.palette }
}

/// Decoded subtitle payload — text or bitmap regions.
pub enum SubtitlePayload<B: AsRef<[u8]>> {
    /// Text subtitle (UTF-8 in `text`; ISO 639-2 language tag optional).
    Text {
        /// UTF-8 text payload.
        text:     B,
        /// ISO 639-2/T language tag, or `None` if unspecified.
        language: Option<[u8; 3]>,
    },
    /// Bitmap subtitle — one or more rectangles of paletted pixels.
    /// Available only with the `alloc` feature.
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    Bitmap {
        /// One or more rectangles. FFmpeg subtitles often carry several.
        regions: alloc::vec::Vec<BitmapRegion<B>>,
    },
}

impl<B: AsRef<[u8]> + Debug> Debug for SubtitlePayload<B> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Text { text, language } => f
                .debug_struct("SubtitlePayload::Text")
                .field("text", text)
                .field("language", language)
                .finish(),
            #[cfg(feature = "alloc")]
            Self::Bitmap { regions } => f
                .debug_struct("SubtitlePayload::Bitmap")
                .field("regions", &regions.len())
                .finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_payload_constructs() {
        let p: SubtitlePayload<&[u8]> = SubtitlePayload::Text {
            text: b"hello",
            language: Some(*b"eng"),
        };
        match p {
            SubtitlePayload::Text { text, language } => {
                assert_eq!(text, b"hello");
                assert_eq!(language, Some(*b"eng"));
            }
            #[cfg(feature = "alloc")]
            _ => panic!("unexpected variant"),
        }
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn bitmap_region_construction() {
        let data: &[u8] = &[0; 16];
        let pal:  &[u8] = &[0; 16];
        let r = BitmapRegion::new(10, 20, 4, 4, 4, data, pal);
        assert_eq!(r.x(), 10);
        assert_eq!(r.width(), 4);
        assert_eq!(*r.data(), data);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn bitmap_payload_constructs() {
        let data: &[u8] = &[0; 16];
        let pal:  &[u8] = &[0; 16];
        let p: SubtitlePayload<&[u8]> = SubtitlePayload::Bitmap {
            regions: alloc::vec![BitmapRegion::new(0, 0, 4, 4, 4, data, pal)],
        };
        if let SubtitlePayload::Bitmap { regions } = p {
            assert_eq!(regions.len(), 1);
        } else {
            panic!("unexpected variant");
        }
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib subtitle`
Expected: 3 tests pass (1 unconditional + 2 alloc-gated).

Run: `cargo test --lib subtitle --no-default-features`
Expected: 1 test passes (text only).

Run: `cargo test --lib subtitle --no-default-features --features alloc`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/subtitle.rs
git commit -m "feat(subtitle): SubtitlePayload<B> with alloc-gated Bitmap variant

Text variant works under pure core; Bitmap requires alloc because
FFmpeg subtitles often carry multiple regions per frame and a fixed
sized array would force a max-region-count constant on the API.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 14: frame.rs — VideoFrame<A, B>

**Files:**
- Modify: `src/frame.rs` (append `VideoFrame` after the `Plane<B>` impl block)

- [ ] **Step 1: Insert `VideoFrame<A, B>` after the `impl<B> Plane<B> { ... }` block**

```rust
use crate::Timestamp;
use crate::adapter::VideoAdapter;
use crate::color::ColorInfo;

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
    pts:           Option<Timestamp>,
    duration:      Option<Timestamp>,
    width:         u32,
    height:        u32,
    visible_rect:  Option<Rect>,
    pixel_format:  A::PixelFormat,
    plane_count:   u8,
    planes:        [Plane<B>; 4],
    color:         ColorInfo,
    extra:         A::FrameExtra,
}

impl<A: VideoAdapter, B: AsRef<[u8]>> VideoFrame<A, B> {
    /// Constructs a `VideoFrame`. Timestamps default to `None`,
    /// `visible_rect` to `None`, color to `ColorInfo::UNSPECIFIED`.
    #[inline]
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
    #[inline]
    pub const fn pts(&self) -> Option<Timestamp> { self.pts }
    /// Returns the duration.
    #[inline]
    pub const fn duration(&self) -> Option<Timestamp> { self.duration }
    /// Returns the coded width.
    #[inline]
    pub const fn width(&self) -> u32 { self.width }
    /// Returns the coded height.
    #[inline]
    pub const fn height(&self) -> u32 { self.height }
    /// Returns the visible / clean-aperture rectangle, if any.
    #[inline]
    pub const fn visible_rect(&self) -> Option<Rect> { self.visible_rect }
    /// Returns the pixel format identifier.
    #[inline]
    pub fn pixel_format(&self) -> A::PixelFormat { self.pixel_format }
    /// Returns the populated plane count.
    #[inline]
    pub const fn plane_count(&self) -> u8 { self.plane_count }
    /// Returns the populated planes as a slice.
    #[inline]
    pub fn planes(&self) -> &[Plane<B>] {
        &self.planes[..self.plane_count as usize]
    }
    /// Returns one plane by index, or `None` if out of range.
    #[inline]
    pub fn plane(&self, i: usize) -> Option<&Plane<B>> {
        if i < self.plane_count as usize {
            self.planes.get(i)
        } else {
            None
        }
    }
    /// Returns the color metadata.
    #[inline]
    pub const fn color(&self) -> ColorInfo { self.color }
    /// Returns the backend extras.
    #[inline]
    pub const fn extra(&self) -> &A::FrameExtra { &self.extra }
    /// Returns a mutable reference to the backend extras.
    #[inline]
    pub fn extra_mut(&mut self) -> &mut A::FrameExtra { &mut self.extra }

    /// Sets the PTS (consuming builder).
    #[inline]
    pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self { self.pts = v; self }
    /// Sets the duration (consuming builder).
    #[inline]
    pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self { self.duration = v; self }
    /// Sets the visible rect (consuming builder).
    #[inline]
    pub const fn with_visible_rect(mut self, v: Option<Rect>) -> Self { self.visible_rect = v; self }
    /// Sets the color metadata (consuming builder).
    #[inline]
    pub const fn with_color(mut self, v: ColorInfo) -> Self { self.color = v; self }

    /// Sets the PTS in place.
    #[inline]
    pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self { self.pts = v; self }
    /// Sets the duration in place.
    #[inline]
    pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self { self.duration = v; self }
    /// Sets the visible rect in place.
    #[inline]
    pub const fn set_visible_rect(&mut self, v: Option<Rect>) -> &mut Self { self.visible_rect = v; self }
    /// Sets the color metadata in place.
    #[inline]
    pub const fn set_color(&mut self, v: ColorInfo) -> &mut Self { self.color = v; self }
}
```

Append these tests to the existing `mod tests { ... }` block:

```rust
    use crate::adapter::VideoAdapter;
    use crate::color::{ColorInfo, ColorMatrix};

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
        assert!(f.color().matrix().is_bt709());
        assert_eq!(f.planes().len(), 1);
    }

    #[test]
    fn video_frame_plane_index_clamped() {
        let f: VideoFrame<VLoop, &[u8]> =
            VideoFrame::new(64, 64, 0u32, empty_planes(), 2, ());
        assert!(f.plane(0).is_some());
        assert!(f.plane(1).is_some());
        assert!(f.plane(2).is_none());
        assert!(f.plane(3).is_none());
    }

    #[test]
    fn video_frame_builders_chain() {
        let ci = ColorInfo::UNSPECIFIED.with_matrix(ColorMatrix::Bt2020Ncl);
        let f: VideoFrame<VLoop, &[u8]> =
            VideoFrame::new(64, 64, 0u32, empty_planes(), 1, ())
                .with_color(ci)
                .with_visible_rect(Some(Rect::new(0, 0, 64, 64)));
        assert!(f.color().matrix().is_bt2020_ncl());
        assert!(f.visible_rect().is_some());
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib frame`
Expected: 11 tests pass (5 Rect + 3 Plane + 3 VideoFrame).

- [ ] **Step 3: Commit**

```bash
git add src/frame.rs
git commit -m "feat(frame): VideoFrame<A, B> with full accessor surface

Common fields validated against AVFrame, VideoFrame (WebCodecs),
CVPixelBuffer, and the cinema RAW SDK outputs (R3D, BRAW, ARRIRAW,
X-OCN, Canon RAW Light). Backend-specific HDR/sensor metadata lives
in A::FrameExtra.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 15: frame.rs — AudioFrame<A, B>

**Files:**
- Modify: `src/frame.rs` (append `AudioFrame` after the `VideoFrame` impl block)

- [ ] **Step 1: Insert `AudioFrame<A, B>` after the `impl<A: VideoAdapter, B: AsRef<[u8]>> VideoFrame<A, B> { ... }` block**

```rust
use crate::adapter::AudioAdapter;

/// A decoded audio frame.
///
/// `nb_samples` is **per channel**. `plane_count` is `1` for packed
/// (interleaved) formats and `channel_count` for planar; the
/// `[Plane; 8]` cap mirrors FFmpeg's `AV_NUM_DATA_POINTERS`.
/// Channel counts above 8 surface their extra channels through
/// `A::FrameExtra` (rare in practice).
pub struct AudioFrame<A: AudioAdapter, B: AsRef<[u8]>> {
    pts:            Option<Timestamp>,
    duration:       Option<Timestamp>,
    sample_rate:    u32,
    nb_samples:     u32,
    channel_count:  u8,
    sample_format:  A::SampleFormat,
    channel_layout: A::ChannelLayout,
    plane_count:    u8,
    planes:         [Plane<B>; 8],
    extra:          A::FrameExtra,
}

impl<A: AudioAdapter, B: AsRef<[u8]>> AudioFrame<A, B> {
    /// Constructs an `AudioFrame`.
    #[allow(clippy::too_many_arguments)]
    #[inline]
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
    #[inline]
    pub const fn pts(&self) -> Option<Timestamp> { self.pts }
    /// Returns the duration.
    #[inline]
    pub const fn duration(&self) -> Option<Timestamp> { self.duration }
    /// Returns the sample rate (Hz).
    #[inline]
    pub const fn sample_rate(&self) -> u32 { self.sample_rate }
    /// Returns the per-channel sample count.
    #[inline]
    pub const fn nb_samples(&self) -> u32 { self.nb_samples }
    /// Returns the channel count.
    #[inline]
    pub const fn channel_count(&self) -> u8 { self.channel_count }
    /// Returns the sample format identifier.
    #[inline]
    pub fn sample_format(&self) -> A::SampleFormat { self.sample_format }
    /// Returns the channel layout identifier.
    #[inline]
    pub fn channel_layout(&self) -> &A::ChannelLayout { &self.channel_layout }
    /// Returns the populated plane count.
    #[inline]
    pub const fn plane_count(&self) -> u8 { self.plane_count }
    /// Returns the populated planes as a slice.
    #[inline]
    pub fn planes(&self) -> &[Plane<B>] {
        &self.planes[..self.plane_count as usize]
    }
    /// Returns the backend extras.
    #[inline]
    pub const fn extra(&self) -> &A::FrameExtra { &self.extra }
    /// Returns a mutable reference to the backend extras.
    #[inline]
    pub fn extra_mut(&mut self) -> &mut A::FrameExtra { &mut self.extra }

    /// Sets the PTS (consuming builder).
    #[inline]
    pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self { self.pts = v; self }
    /// Sets the duration (consuming builder).
    #[inline]
    pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self { self.duration = v; self }

    /// Sets the PTS in place.
    #[inline]
    pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self { self.pts = v; self }
    /// Sets the duration in place.
    #[inline]
    pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self { self.duration = v; self }
}
```

Append to the existing `mod tests { ... }`:

```rust
    use crate::adapter::AudioAdapter;

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
            48_000, 1024, 2, /*sf=*/ 0u32, /*layout=*/ 0u32,
            audio_planes(), 2, (),
        );
        assert_eq!(f.sample_rate(), 48_000);
        assert_eq!(f.nb_samples(), 1024);
        assert_eq!(f.channel_count(), 2);
        assert_eq!(f.plane_count(), 2);
        assert_eq!(f.planes().len(), 2);
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib frame`
Expected: 12 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/frame.rs
git commit -m "feat(frame): AudioFrame<A, B>

[Plane; 8] mirrors FFmpeg AV_NUM_DATA_POINTERS. Channel counts above
8 surface extra channels through A::FrameExtra.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 16: frame.rs — SubtitleFrame<A, B>

**Files:**
- Modify: `src/frame.rs` (append `SubtitleFrame` after `AudioFrame`'s impl block)

- [ ] **Step 1: Insert `SubtitleFrame<A, B>` after the `impl<A: AudioAdapter, B: AsRef<[u8]>> AudioFrame<A, B> { ... }` block**

```rust
use crate::adapter::SubtitleAdapter;
use crate::subtitle::SubtitlePayload;

/// A decoded subtitle frame.
pub struct SubtitleFrame<A: SubtitleAdapter, B: AsRef<[u8]>> {
    pts:      Option<Timestamp>,
    duration: Option<Timestamp>,
    payload:  SubtitlePayload<B>,
    extra:    A::FrameExtra,
}

impl<A: SubtitleAdapter, B: AsRef<[u8]>> SubtitleFrame<A, B> {
    /// Constructs a `SubtitleFrame`.
    #[inline]
    pub fn new(payload: SubtitlePayload<B>, extra: A::FrameExtra) -> Self {
        Self { pts: None, duration: None, payload, extra }
    }

    /// Returns the PTS.
    #[inline]
    pub const fn pts(&self) -> Option<Timestamp> { self.pts }
    /// Returns the duration.
    #[inline]
    pub const fn duration(&self) -> Option<Timestamp> { self.duration }
    /// Returns the payload.
    #[inline]
    pub const fn payload(&self) -> &SubtitlePayload<B> { &self.payload }
    /// Returns the backend extras.
    #[inline]
    pub const fn extra(&self) -> &A::FrameExtra { &self.extra }
    /// Returns a mutable reference to the backend extras.
    #[inline]
    pub fn extra_mut(&mut self) -> &mut A::FrameExtra { &mut self.extra }

    /// Sets the PTS (consuming builder).
    #[inline]
    pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self { self.pts = v; self }
    /// Sets the duration (consuming builder).
    #[inline]
    pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self { self.duration = v; self }

    /// Sets the PTS in place.
    #[inline]
    pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self { self.pts = v; self }
    /// Sets the duration in place.
    #[inline]
    pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self { self.duration = v; self }
}
```

Append to `mod tests { ... }`:

```rust
    use crate::adapter::SubtitleAdapter;
    use crate::subtitle::SubtitlePayload;

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
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib frame`
Expected: 13 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/frame.rs
git commit -m "feat(frame): SubtitleFrame<A, B>

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 17: decoder.rs — VideoStreamDecoder + VideoFrameSource

**Files:**
- Modify: `src/decoder.rs`

- [ ] **Step 1: Replace `src/decoder.rs` with the video traits**

```rust
//! Decoder traits — push-style streams (FFmpeg / WebCodecs / ProRes
//! RAW via VTDecompressionSession) and pull-style frame sources
//! (R3D / BRAW / ARRIRAW / X-OCN / Canon RAW Light).

use crate::Timebase;
use crate::Timestamp;
use crate::adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter};
use crate::frame::{AudioFrame, SubtitleFrame, VideoFrame};
use crate::packet::{AudioPacket, SubtitlePacket, VideoPacket};

/// Push-style video decoder. Caller submits compressed packets and
/// drains decoded frames.
///
/// Backends: FFmpeg, WebCodecs, ProRes RAW (VideoToolbox).
pub trait VideoStreamDecoder {
    /// Backend-specific vocabulary.
    type Adapter: VideoAdapter;
    /// Buffer type held by the packets and frames this decoder
    /// produces or accepts.
    type Buffer: AsRef<[u8]>;
    /// Decoder-specific error type.
    type Error;

    /// Submits one compressed packet.
    fn send_packet(
        &mut self,
        packet: &VideoPacket<Self::Adapter, Self::Buffer>,
    ) -> Result<(), Self::Error>;

    /// Drains one decoded frame into `dst`. Backends signal "no
    /// frame ready" via a backend-specific `Error` variant.
    fn receive_frame(
        &mut self,
        dst: &mut VideoFrame<Self::Adapter, Self::Buffer>,
    ) -> Result<(), Self::Error>;

    /// Signals end-of-stream.
    fn send_eof(&mut self) -> Result<(), Self::Error>;

    /// Flushes internal state.
    fn flush(&mut self) -> Result<(), Self::Error>;
}

/// Pull-style video frame source. Caller requests frames by integer
/// index. Clip-level metadata accessible via `clip_meta()`.
///
/// Backends: R3D, BRAW, ARRIRAW, Sony X-OCN, Canon Cinema RAW Light.
pub trait VideoFrameSource {
    /// Backend-specific vocabulary.
    type Adapter: VideoAdapter;
    /// Buffer type for the produced frames.
    type Buffer: AsRef<[u8]>;
    /// Backend-specific clip-level metadata bag (e.g. `R3dClipMeta`,
    /// `ArriClipMeta`). Backends without clip metadata set this to `()`.
    type ClipMeta;
    /// Decoder-specific error type.
    type Error;

    /// Total frame count in the clip.
    fn frame_count(&self) -> u64;
    /// Video frame rate (frames per second as a `Timebase`).
    fn frame_rate(&self) -> Timebase;
    /// Total clip duration.
    fn duration(&self) -> Timestamp;
    /// Backend-specific clip-level metadata.
    fn clip_meta(&self) -> &Self::ClipMeta;

    /// Decodes one frame at `index` into `dst`.
    fn decode_frame(
        &mut self,
        index: u64,
        dst: &mut VideoFrame<Self::Adapter, Self::Buffer>,
    ) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Timebase;
    use core::num::NonZeroU32;

    pub(crate) struct VLoop;
    impl VideoAdapter for VLoop {
        type CodecId = u32;
        type PixelFormat = u32;
        type PacketExtra = ();
        type FrameExtra = ();
    }

    /// Trivial loopback impl — confirms the trait can be implemented.
    pub(crate) struct LoopVideoStream;

    #[derive(Debug)]
    pub(crate) struct LoopError;

    impl VideoStreamDecoder for LoopVideoStream {
        type Adapter = VLoop;
        type Buffer = &'static [u8];
        type Error = LoopError;

        fn send_packet(&mut self, _: &VideoPacket<VLoop, &'static [u8]>)
            -> Result<(), LoopError> { Ok(()) }
        fn receive_frame(&mut self, _: &mut VideoFrame<VLoop, &'static [u8]>)
            -> Result<(), LoopError> { Err(LoopError) }
        fn send_eof(&mut self) -> Result<(), LoopError> { Ok(()) }
        fn flush(&mut self) -> Result<(), LoopError> { Ok(()) }
    }

    pub(crate) struct LoopVideoSource;

    impl VideoFrameSource for LoopVideoSource {
        type Adapter = VLoop;
        type Buffer = &'static [u8];
        type ClipMeta = ();
        type Error = LoopError;

        fn frame_count(&self) -> u64 { 0 }
        fn frame_rate(&self) -> Timebase {
            Timebase::new(30, NonZeroU32::new(1).unwrap())
        }
        fn duration(&self) -> Timestamp {
            Timestamp::new(0, self.frame_rate())
        }
        fn clip_meta(&self) -> &() { &() }
        fn decode_frame(&mut self, _: u64, _: &mut VideoFrame<VLoop, &'static [u8]>)
            -> Result<(), LoopError> { Err(LoopError) }
    }

    #[test]
    fn video_traits_are_implementable() {
        fn _stream<D: VideoStreamDecoder>() {}
        fn _source<D: VideoFrameSource>() {}
        _stream::<LoopVideoStream>();
        _source::<LoopVideoSource>();
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib decoder`
Expected: 1 test passes.

- [ ] **Step 3: Commit**

```bash
git add src/decoder.rs
git commit -m "feat(decoder): VideoStreamDecoder + VideoFrameSource traits

Push/pull split: stream decoders for FFmpeg/WebCodecs/ProRes RAW;
frame sources for cinema RAW SDKs. ClipMeta associated type carries
backend-specific clip-level metadata for the pull-style backends.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 18: decoder.rs — AudioStreamDecoder + AudioFrameSource

**Files:**
- Modify: `src/decoder.rs` (append after the video traits)

- [ ] **Step 1: Insert audio traits after the `VideoFrameSource` trait**

Append after the `pub trait VideoFrameSource { ... }` block, before the `#[cfg(test)] mod tests`:

```rust
/// Push-style audio decoder.
pub trait AudioStreamDecoder {
    /// Backend vocabulary.
    type Adapter: AudioAdapter;
    /// Buffer type.
    type Buffer: AsRef<[u8]>;
    /// Decoder-specific error.
    type Error;
    /// Submits a compressed audio packet.
    fn send_packet(
        &mut self,
        packet: &AudioPacket<Self::Adapter, Self::Buffer>,
    ) -> Result<(), Self::Error>;
    /// Drains a decoded frame.
    fn receive_frame(
        &mut self,
        dst: &mut AudioFrame<Self::Adapter, Self::Buffer>,
    ) -> Result<(), Self::Error>;
    /// Signals EOF.
    fn send_eof(&mut self) -> Result<(), Self::Error>;
    /// Flushes internal state.
    fn flush(&mut self) -> Result<(), Self::Error>;
}

/// Pull-style audio frame source. Caller requests blocks by sample
/// offset.
///
/// Backends: R3D, BRAW (audio in companion track of the same clip).
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
    fn decode_block(
        &mut self,
        sample_offset: u64,
        sample_count: u32,
        dst: &mut AudioFrame<Self::Adapter, Self::Buffer>,
    ) -> Result<(), Self::Error>;
}
```

Append to `mod tests { ... }`:

```rust
    pub(crate) struct ALoop;
    impl AudioAdapter for ALoop {
        type CodecId = u32;
        type SampleFormat = u32;
        type ChannelLayout = u32;
        type PacketExtra = ();
        type FrameExtra = ();
    }

    pub(crate) struct LoopAudioStream;

    impl AudioStreamDecoder for LoopAudioStream {
        type Adapter = ALoop;
        type Buffer = &'static [u8];
        type Error = LoopError;
        fn send_packet(&mut self, _: &AudioPacket<ALoop, &'static [u8]>)
            -> Result<(), LoopError> { Ok(()) }
        fn receive_frame(&mut self, _: &mut AudioFrame<ALoop, &'static [u8]>)
            -> Result<(), LoopError> { Err(LoopError) }
        fn send_eof(&mut self) -> Result<(), LoopError> { Ok(()) }
        fn flush(&mut self) -> Result<(), LoopError> { Ok(()) }
    }

    pub(crate) struct LoopAudioSource;

    impl AudioFrameSource for LoopAudioSource {
        type Adapter = ALoop;
        type Buffer = &'static [u8];
        type ClipMeta = ();
        type Error = LoopError;
        fn sample_count(&self) -> u64 { 0 }
        fn sample_rate(&self) -> u32 { 48_000 }
        fn channel_count(&self) -> u8 { 2 }
        fn clip_meta(&self) -> &() { &() }
        fn decode_block(&mut self, _: u64, _: u32, _: &mut AudioFrame<ALoop, &'static [u8]>)
            -> Result<(), LoopError> { Err(LoopError) }
    }

    #[test]
    fn audio_traits_are_implementable() {
        fn _stream<D: AudioStreamDecoder>() {}
        fn _source<D: AudioFrameSource>() {}
        _stream::<LoopAudioStream>();
        _source::<LoopAudioSource>();
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib decoder`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/decoder.rs
git commit -m "feat(decoder): AudioStreamDecoder + AudioFrameSource

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 19: decoder.rs — SubtitleDecoder

**Files:**
- Modify: `src/decoder.rs` (append after audio traits)

- [ ] **Step 1: Insert `SubtitleDecoder` after the `AudioFrameSource` trait**

Append after the `AudioFrameSource` trait, before `#[cfg(test)] mod tests`:

```rust
/// Push-style subtitle decoder. (No pull-style subtitle decoders
/// exist in the wild — subtitle streams are linear and small.)
pub trait SubtitleDecoder {
    /// Backend vocabulary.
    type Adapter: SubtitleAdapter;
    /// Buffer type.
    type Buffer: AsRef<[u8]>;
    /// Decoder-specific error.
    type Error;
    /// Submits a compressed subtitle packet.
    fn send_packet(
        &mut self,
        packet: &SubtitlePacket<Self::Adapter, Self::Buffer>,
    ) -> Result<(), Self::Error>;
    /// Drains a decoded subtitle frame.
    fn receive_frame(
        &mut self,
        dst: &mut SubtitleFrame<Self::Adapter, Self::Buffer>,
    ) -> Result<(), Self::Error>;
    /// Signals EOF.
    fn send_eof(&mut self) -> Result<(), Self::Error>;
    /// Flushes internal state.
    fn flush(&mut self) -> Result<(), Self::Error>;
}
```

Append to `mod tests { ... }`:

```rust
    pub(crate) struct SLoop;
    impl SubtitleAdapter for SLoop {
        type CodecId = u32;
        type PacketExtra = ();
        type FrameExtra = ();
    }

    pub(crate) struct LoopSubtitleStream;

    impl SubtitleDecoder for LoopSubtitleStream {
        type Adapter = SLoop;
        type Buffer = &'static [u8];
        type Error = LoopError;
        fn send_packet(&mut self, _: &SubtitlePacket<SLoop, &'static [u8]>)
            -> Result<(), LoopError> { Ok(()) }
        fn receive_frame(&mut self, _: &mut SubtitleFrame<SLoop, &'static [u8]>)
            -> Result<(), LoopError> { Err(LoopError) }
        fn send_eof(&mut self) -> Result<(), LoopError> { Ok(()) }
        fn flush(&mut self) -> Result<(), LoopError> { Ok(()) }
    }

    #[test]
    fn subtitle_decoder_is_implementable() {
        fn _decoder<D: SubtitleDecoder>() {}
        _decoder::<LoopSubtitleStream>();
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib decoder`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/decoder.rs
git commit -m "feat(decoder): SubtitleDecoder

Push-only — no random-access subtitle decoders exist in the wild.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 20: tests/loopback.rs — full integration test

**Files:**
- Create: `tests/loopback.rs`
- Delete: `tests/foo.rs` (the template placeholder)

- [ ] **Step 1: Remove the template placeholder test**

Run:
```bash
git rm tests/foo.rs
```

- [ ] **Step 2: Create `tests/loopback.rs`**

```rust
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
    color::{ColorInfo, ColorMatrix, ColorPrimaries, ColorTransfer, ColorRange, ChromaLocation},
    decoder::{
        AudioFrameSource, AudioStreamDecoder, SubtitleDecoder,
        VideoFrameSource, VideoStreamDecoder,
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
    fn send_packet(&mut self, _: &VideoPacket<Loop, &'static [u8]>)
        -> Result<(), Eof> { Ok(()) }
    fn receive_frame(&mut self, _: &mut VideoFrame<Loop, &'static [u8]>)
        -> Result<(), Eof> { Err(Eof) }
    fn send_eof(&mut self) -> Result<(), Eof> { Ok(()) }
    fn flush(&mut self) -> Result<(), Eof> { Ok(()) }
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
    fn frame_count(&self) -> u64 { 0 }
    fn frame_rate(&self) -> Timebase { self.fps }
    fn duration(&self) -> Timestamp { Timestamp::new(self.duration_pts, self.fps) }
    fn clip_meta(&self) -> &() { &() }
    fn decode_frame(&mut self, _: u64, _: &mut VideoFrame<Loop, &'static [u8]>)
        -> Result<(), Eof> { Err(Eof) }
}

pub struct AudioStream;
impl AudioStreamDecoder for AudioStream {
    type Adapter = Loop;
    type Buffer = &'static [u8];
    type Error = Eof;
    fn send_packet(&mut self, _: &AudioPacket<Loop, &'static [u8]>)
        -> Result<(), Eof> { Ok(()) }
    fn receive_frame(&mut self, _: &mut AudioFrame<Loop, &'static [u8]>)
        -> Result<(), Eof> { Err(Eof) }
    fn send_eof(&mut self) -> Result<(), Eof> { Ok(()) }
    fn flush(&mut self) -> Result<(), Eof> { Ok(()) }
}

pub struct AudioSource;
impl AudioFrameSource for AudioSource {
    type Adapter = Loop;
    type Buffer = &'static [u8];
    type ClipMeta = ();
    type Error = Eof;
    fn sample_count(&self) -> u64 { 0 }
    fn sample_rate(&self) -> u32 { 48_000 }
    fn channel_count(&self) -> u8 { 2 }
    fn clip_meta(&self) -> &() { &() }
    fn decode_block(&mut self, _: u64, _: u32, _: &mut AudioFrame<Loop, &'static [u8]>)
        -> Result<(), Eof> { Err(Eof) }
}

pub struct SubtitleStream;
impl SubtitleDecoder for SubtitleStream {
    type Adapter = Loop;
    type Buffer = &'static [u8];
    type Error = Eof;
    fn send_packet(&mut self, _: &SubtitlePacket<Loop, &'static [u8]>)
        -> Result<(), Eof> { Ok(()) }
    fn receive_frame(&mut self, _: &mut SubtitleFrame<Loop, &'static [u8]>)
        -> Result<(), Eof> { Err(Eof) }
    fn send_eof(&mut self) -> Result<(), Eof> { Ok(()) }
    fn flush(&mut self) -> Result<(), Eof> { Ok(()) }
}

#[test]
fn video_stream_round_trip() {
    let mut s = VideoStream;
    let pkt: VideoPacket<Loop, &'static [u8]> = VideoPacket::new(b"compressed", ())
        .with_pts(Some(Timestamp::new(0, Timebase::new(1, NonZeroU32::new(1000).unwrap()))))
        .with_flags(PacketFlags::KEY);
    assert!(s.send_packet(&pkt).is_ok());

    let planes = [
        Plane::new(&b"yyyy"[..], 4),
        Plane::new(&b""[..],     0),
        Plane::new(&b""[..],     0),
        Plane::new(&b""[..],     0),
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
    assert!(dst.color().matrix().is_bt709());
    assert!(s.send_eof().is_ok());
    assert!(s.flush().is_ok());
}

#[test]
fn video_source_round_trip() {
    let fps = Timebase::new(30, NonZeroU32::new(1).unwrap());
    let mut src = VideoSource { fps, duration_pts: 0 };
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
    let mut dst: VideoFrame<Loop, &'static [u8]> =
        VideoFrame::new(64, 64, 0u32, planes, 1, ());
    assert!(src.decode_frame(0, &mut dst).is_err());
}

#[test]
fn audio_stream_round_trip() {
    let mut s = AudioStream;
    let pkt: AudioPacket<Loop, &'static [u8]> = AudioPacket::new(b"compressed", ());
    assert!(s.send_packet(&pkt).is_ok());

    let planes = [
        Plane::new(&b""[..], 0), Plane::new(&b""[..], 0),
        Plane::new(&b""[..], 0), Plane::new(&b""[..], 0),
        Plane::new(&b""[..], 0), Plane::new(&b""[..], 0),
        Plane::new(&b""[..], 0), Plane::new(&b""[..], 0),
    ];
    let mut dst: AudioFrame<Loop, &'static [u8]> = AudioFrame::new(
        48_000, 1024, 2, /*sf=*/ 0u32, /*layout=*/ 0u32, planes, 2, (),
    );
    assert!(s.receive_frame(&mut dst).is_err());
    assert_eq!(dst.sample_rate(), 48_000);
}

#[test]
fn audio_source_metadata() {
    let mut src = AudioSource;
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
```

- [ ] **Step 3: Run the integration tests**

Run: `cargo test --test loopback`
Expected: 5 tests pass.

- [ ] **Step 4: Run the full crate test suite (lib + integration)**

Run: `cargo test`
Expected: all tests pass (lib tests from tasks 3-19 + 5 integration tests).

- [ ] **Step 5: Verify no_std + alloc build still compiles**

Run: `cargo check --no-default-features --features alloc`
Expected: succeeds.

Run: `cargo check --no-default-features`
Expected: succeeds.

- [ ] **Step 6: Verify wasm32-unknown-unknown builds**

Run: `cargo check --no-default-features --features alloc --target wasm32-unknown-unknown`
Expected: succeeds. (If `wasm32-unknown-unknown` target isn't installed, run `rustup target add wasm32-unknown-unknown` first.)

- [ ] **Step 7: Commit**

```bash
git add tests/loopback.rs
git commit -m "test: end-to-end loopback adapter integration test

Implements the three adapter traits and the five decoder traits with
() extras and primitive associated types. Proves the type-and-trait
spine composes — packets accepted, frames flow through generics
resolve, color metadata round-trips through builders. No external
SDK required.

Removes the template-rs placeholder tests/foo.rs.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Self-review (running on the plan above)

**Spec coverage check** (against `docs/superpowers/specs/2026-05-08-mediadecode-design.md`):

| Spec section | Covered by task |
|---|---|
| §1 Purpose / §2 Pipeline context / §3 Licenses | (docs only — covered by spec doc) |
| §4 Conventions (private fields, `with_*`/`set_*`, const fn, `#[non_exhaustive]`) | Tasks 4–19 follow throughout |
| §5 Crate layout | Tasks 1, 2 establish the layout |
| §6.1 Adapter traits | Task 9 |
| §6.2 Packet types + PacketFlags | Tasks 8, 10, 11, 12 |
| §6.3 Frame types + Plane + Rect | Tasks 6, 7, 14, 15, 16 |
| §6.4 Color enums (incl. lifted ColorMatrix + BayerPattern, plus 4 H.273 siblings) + ColorInfo | Tasks 3, 4, 5 |
| §6.5 Decoder traits (push/pull split + audio + subtitle) | Tasks 17, 18, 19 |
| §7 Cargo features (default = std; alloc; std; serde; arbitrary; quickcheck) | Task 1 |
| §8 Testing strategy (unit per type, loopback integration test) | Per-task tests + Task 20 |
| MSRV 1.95 / edition 2024 / re-export mediatime | Task 1, Task 2 |

All spec sections covered.

**Placeholder scan:** No "TBD" / "TODO" / "implement later" in any task. All code is concrete.

**Type consistency check:**
- `VideoAdapter::PacketExtra` / `FrameExtra` used consistently in Tasks 9, 10, 14
- `Plane::new(data, stride)` signature matches across Tasks 7, 14, 15, 20
- `ColorInfo::UNSPECIFIED` const used in Tasks 5, 14, 20
- `Timestamp::new(pts, timebase)` matches `mediatime` (verified against `mediatime/src/lib.rs:240`)
- `PacketFlags::KEY / CORRUPT / DISCARD` consistent in Tasks 8, 10, 20
- All decoder traits use `type Adapter`, `type Buffer: AsRef<[u8]>`, `type Error` consistently in Tasks 17, 18, 19
- `clip_meta(&self) -> &Self::ClipMeta` matches in Tasks 17, 18, 20

No type inconsistencies found.

**Scope check:** Single coherent crate; one focused implementation pass; 20 atomic tasks averaging one file change each.

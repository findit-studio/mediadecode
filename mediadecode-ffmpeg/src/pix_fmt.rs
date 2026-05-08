//! `PixelFormat` newtype around FFmpeg's `AVPixelFormat` discriminant,
//! plus stable constants for the formats this crate produces.
//!
//! `Frame::pix_fmt()` returns a `PixelFormat`; the inner `i32` is the
//! exact value FFmpeg wrote to `AVFrame.format`. The newtype protects
//! against the enum-construction UB an unvalidated cast would invoke
//! (the value FFmpeg wrote may not be in our build's bindgen-generated
//! `AVPixelFormat` discriminant set).
//!
//! Constants below cover both:
//! - **Hardware-decoded outputs** — the `NV*` family (8-bit semi-planar)
//!   and `P0xx`/`P2xx`/`P4xx` family (10/12/16-bit semi-planar) that
//!   VideoToolbox / VAAPI / NVDEC / D3D11VA download into.
//! - **Software-decoded outputs** — the planar `YUVxxxP` family (8-bit
//!   and high-bit-depth), packed RGB / BGR, and high-bit-depth packed
//!   RGB used by `ffmpeg::decoder::Video` for software paths.
//!
//! For values not listed here, build a `PixelFormat` directly from
//! `AVPixelFormat::AV_PIX_FMT_X as i32` — that's exactly the cast we
//! use.
//!
//! ```ignore
//! use mediadecode_ffmpeg::{pix_fmt::PixelFormat, Frame};
//! match frame.pix_fmt() {
//!     PixelFormat::NV12   => /* 8-bit 4:2:0 semi-planar */,
//!     PixelFormat::P010LE => /* 10-bit 4:2:0 semi-planar */,
//!     other               => unimplemented!("pix_fmt {:?}", other),
//! }
//! ```

use core::fmt;

use ffmpeg_next::ffi::AVPixelFormat;

/// Pixel format identifier. Wraps the integer value of an `AVPixelFormat`
/// enum variant; comparisons and storage work without ever transmuting
/// back into the bindgen enum.
#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct PixelFormat(i32);

impl PixelFormat {
  /// Constructs a `PixelFormat` from the raw integer FFmpeg uses for
  /// `AVFrame.format`.
  #[inline]
  pub const fn from_raw(raw: i32) -> Self {
    Self(raw)
  }

  /// Returns the underlying integer (suitable for assignment back into
  /// `AVFrame.format` or comparison against C-side values).
  #[inline]
  pub const fn raw(self) -> i32 {
    self.0
  }

  // --- Sentinel --------------------------------------------------------

  /// Sentinel for "no format" / unset (`AV_PIX_FMT_NONE`, `-1`).
  pub const NONE: Self = Self(AVPixelFormat::AV_PIX_FMT_NONE as i32);

  // --- Semi-planar 8-bit (NV*) — HW download outputs ------------------

  /// 4:2:0, 8-bit, Y plane + interleaved Cb/Cr.
  pub const NV12: Self = Self(AVPixelFormat::AV_PIX_FMT_NV12 as i32);
  /// 4:2:0, 8-bit, Y plane + interleaved Cr/Cb.
  pub const NV21: Self = Self(AVPixelFormat::AV_PIX_FMT_NV21 as i32);
  /// 4:2:2, 8-bit, Y plane + interleaved Cb/Cr.
  pub const NV16: Self = Self(AVPixelFormat::AV_PIX_FMT_NV16 as i32);
  /// 4:4:4, 8-bit, Y plane + interleaved Cb/Cr.
  pub const NV24: Self = Self(AVPixelFormat::AV_PIX_FMT_NV24 as i32);

  // --- Semi-planar high-bit-depth (P0xx / P2xx / P4xx) — HW outputs ---

  /// 4:2:0, 10-bit, semi-planar little-endian.
  pub const P010LE: Self = Self(AVPixelFormat::AV_PIX_FMT_P010LE as i32);
  /// 4:2:0, 10-bit, semi-planar big-endian.
  pub const P010BE: Self = Self(AVPixelFormat::AV_PIX_FMT_P010BE as i32);
  /// 4:2:0, 12-bit, semi-planar little-endian.
  pub const P012LE: Self = Self(AVPixelFormat::AV_PIX_FMT_P012LE as i32);
  /// 4:2:0, 16-bit, semi-planar little-endian.
  pub const P016LE: Self = Self(AVPixelFormat::AV_PIX_FMT_P016LE as i32);
  /// 4:2:2, 10-bit, semi-planar little-endian.
  pub const P210LE: Self = Self(AVPixelFormat::AV_PIX_FMT_P210LE as i32);
  /// 4:2:2, 12-bit, semi-planar little-endian (FFmpeg 5.1+).
  pub const P212LE: Self = Self(AVPixelFormat::AV_PIX_FMT_P212LE as i32);
  /// 4:2:2, 16-bit, semi-planar little-endian.
  pub const P216LE: Self = Self(AVPixelFormat::AV_PIX_FMT_P216LE as i32);
  /// 4:4:4, 10-bit, semi-planar little-endian.
  pub const P410LE: Self = Self(AVPixelFormat::AV_PIX_FMT_P410LE as i32);
  /// 4:4:4, 12-bit, semi-planar little-endian (FFmpeg 5.1+).
  pub const P412LE: Self = Self(AVPixelFormat::AV_PIX_FMT_P412LE as i32);
  /// 4:4:4, 16-bit, semi-planar little-endian.
  pub const P416LE: Self = Self(AVPixelFormat::AV_PIX_FMT_P416LE as i32);

  // --- Planar YUV 8-bit (SW decoder outputs) --------------------------

  /// 4:2:0, 8-bit, planar Y/Cb/Cr.
  pub const YUV420P: Self = Self(AVPixelFormat::AV_PIX_FMT_YUV420P as i32);
  /// 4:2:2, 8-bit, planar Y/Cb/Cr.
  pub const YUV422P: Self = Self(AVPixelFormat::AV_PIX_FMT_YUV422P as i32);
  /// 4:4:4, 8-bit, planar Y/Cb/Cr.
  pub const YUV444P: Self = Self(AVPixelFormat::AV_PIX_FMT_YUV444P as i32);

  // --- Planar YUV high-bit-depth --------------------------------------

  /// 4:2:0, 10-bit planar.
  pub const YUV420P10LE: Self = Self(AVPixelFormat::AV_PIX_FMT_YUV420P10LE as i32);
  /// 4:2:0, 12-bit planar.
  pub const YUV420P12LE: Self = Self(AVPixelFormat::AV_PIX_FMT_YUV420P12LE as i32);
  /// 4:2:0, 16-bit planar.
  pub const YUV420P16LE: Self = Self(AVPixelFormat::AV_PIX_FMT_YUV420P16LE as i32);
  /// 4:2:2, 10-bit planar.
  pub const YUV422P10LE: Self = Self(AVPixelFormat::AV_PIX_FMT_YUV422P10LE as i32);
  /// 4:2:2, 12-bit planar.
  pub const YUV422P12LE: Self = Self(AVPixelFormat::AV_PIX_FMT_YUV422P12LE as i32);
  /// 4:2:2, 16-bit planar.
  pub const YUV422P16LE: Self = Self(AVPixelFormat::AV_PIX_FMT_YUV422P16LE as i32);
  /// 4:4:4, 10-bit planar.
  pub const YUV444P10LE: Self = Self(AVPixelFormat::AV_PIX_FMT_YUV444P10LE as i32);
  /// 4:4:4, 12-bit planar.
  pub const YUV444P12LE: Self = Self(AVPixelFormat::AV_PIX_FMT_YUV444P12LE as i32);
  /// 4:4:4, 16-bit planar.
  pub const YUV444P16LE: Self = Self(AVPixelFormat::AV_PIX_FMT_YUV444P16LE as i32);

  // --- Planar YUVA (with alpha) ---------------------------------------

  /// 4:2:0, 8-bit, planar Y/Cb/Cr/A.
  pub const YUVA420P: Self = Self(AVPixelFormat::AV_PIX_FMT_YUVA420P as i32);
  /// 4:2:2, 8-bit, planar Y/Cb/Cr/A.
  pub const YUVA422P: Self = Self(AVPixelFormat::AV_PIX_FMT_YUVA422P as i32);
  /// 4:4:4, 8-bit, planar Y/Cb/Cr/A.
  pub const YUVA444P: Self = Self(AVPixelFormat::AV_PIX_FMT_YUVA444P as i32);

  // --- Packed RGB 8-bit -----------------------------------------------

  /// 24-bit packed RGB.
  pub const RGB24: Self = Self(AVPixelFormat::AV_PIX_FMT_RGB24 as i32);
  /// 24-bit packed BGR.
  pub const BGR24: Self = Self(AVPixelFormat::AV_PIX_FMT_BGR24 as i32);
  /// 32-bit packed RGBA.
  pub const RGBA: Self = Self(AVPixelFormat::AV_PIX_FMT_RGBA as i32);
  /// 32-bit packed BGRA.
  pub const BGRA: Self = Self(AVPixelFormat::AV_PIX_FMT_BGRA as i32);
  /// 32-bit packed ARGB.
  pub const ARGB: Self = Self(AVPixelFormat::AV_PIX_FMT_ARGB as i32);
  /// 32-bit packed ABGR.
  pub const ABGR: Self = Self(AVPixelFormat::AV_PIX_FMT_ABGR as i32);

  // --- Packed RGB 16-bit ----------------------------------------------

  /// 48-bit packed RGB (3 × 16-bit, little-endian).
  pub const RGB48LE: Self = Self(AVPixelFormat::AV_PIX_FMT_RGB48LE as i32);
  /// 64-bit packed RGBA (4 × 16-bit, little-endian).
  pub const RGBA64LE: Self = Self(AVPixelFormat::AV_PIX_FMT_RGBA64LE as i32);

  // --- Greyscale ------------------------------------------------------

  /// 8-bit greyscale.
  pub const GRAY8: Self = Self(AVPixelFormat::AV_PIX_FMT_GRAY8 as i32);
  /// 16-bit greyscale, little-endian.
  pub const GRAY16LE: Self = Self(AVPixelFormat::AV_PIX_FMT_GRAY16LE as i32);
}

impl fmt::Debug for PixelFormat {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let name = match *self {
      Self::NONE => "NONE",
      Self::NV12 => "NV12",
      Self::NV21 => "NV21",
      Self::NV16 => "NV16",
      Self::NV24 => "NV24",
      Self::P010LE => "P010LE",
      Self::P010BE => "P010BE",
      Self::P012LE => "P012LE",
      Self::P016LE => "P016LE",
      Self::P210LE => "P210LE",
      Self::P212LE => "P212LE",
      Self::P216LE => "P216LE",
      Self::P410LE => "P410LE",
      Self::P412LE => "P412LE",
      Self::P416LE => "P416LE",
      Self::YUV420P => "YUV420P",
      Self::YUV422P => "YUV422P",
      Self::YUV444P => "YUV444P",
      Self::YUV420P10LE => "YUV420P10LE",
      Self::YUV420P12LE => "YUV420P12LE",
      Self::YUV420P16LE => "YUV420P16LE",
      Self::YUV422P10LE => "YUV422P10LE",
      Self::YUV422P12LE => "YUV422P12LE",
      Self::YUV422P16LE => "YUV422P16LE",
      Self::YUV444P10LE => "YUV444P10LE",
      Self::YUV444P12LE => "YUV444P12LE",
      Self::YUV444P16LE => "YUV444P16LE",
      Self::YUVA420P => "YUVA420P",
      Self::YUVA422P => "YUVA422P",
      Self::YUVA444P => "YUVA444P",
      Self::RGB24 => "RGB24",
      Self::BGR24 => "BGR24",
      Self::RGBA => "RGBA",
      Self::BGRA => "BGRA",
      Self::ARGB => "ARGB",
      Self::ABGR => "ABGR",
      Self::RGB48LE => "RGB48LE",
      Self::RGBA64LE => "RGBA64LE",
      Self::GRAY8 => "GRAY8",
      Self::GRAY16LE => "GRAY16LE",
      _ => return write!(f, "PixelFormat({})", self.0),
    };
    write!(f, "PixelFormat::{name}")
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Regression check: if the underlying `AVPixelFormat` discriminants
  /// ever change in `ffmpeg-sys-next`'s bindings, this catches it.
  #[test]
  fn known_constants_match_av_values() {
    assert_eq!(PixelFormat::NV12.raw(), AVPixelFormat::AV_PIX_FMT_NV12 as i32);
    assert_eq!(PixelFormat::P010LE.raw(), AVPixelFormat::AV_PIX_FMT_P010LE as i32);
    assert_eq!(PixelFormat::P416LE.raw(), AVPixelFormat::AV_PIX_FMT_P416LE as i32);
    assert_eq!(PixelFormat::NONE.raw(), -1, "AV_PIX_FMT_NONE must be -1 (FFmpeg ABI sentinel)");
    assert_eq!(PixelFormat::YUV420P.raw(), AVPixelFormat::AV_PIX_FMT_YUV420P as i32);
    assert_eq!(PixelFormat::RGB24.raw(), AVPixelFormat::AV_PIX_FMT_RGB24 as i32);
  }

  #[test]
  fn match_dispatch_compiles() {
    fn classify(v: PixelFormat) -> &'static str {
      match v {
        PixelFormat::NV12 => "nv12",
        PixelFormat::NV21 => "nv21",
        PixelFormat::P010LE => "p010le",
        PixelFormat::P210LE => "p210le",
        PixelFormat::P410LE => "p410le",
        PixelFormat::YUV420P => "yuv420p",
        _ => "other",
      }
    }
    assert_eq!(classify(PixelFormat::NV12), "nv12");
    assert_eq!(classify(PixelFormat::P010LE), "p010le");
    assert_eq!(classify(PixelFormat::YUV420P), "yuv420p");
    assert_eq!(classify(PixelFormat::NONE), "other");
  }

  #[test]
  fn from_raw_round_trips() {
    let p = PixelFormat::from_raw(AVPixelFormat::AV_PIX_FMT_NV12 as i32);
    assert_eq!(p, PixelFormat::NV12);
    assert_eq!(p.raw(), AVPixelFormat::AV_PIX_FMT_NV12 as i32);
  }

  #[test]
  fn debug_uses_name_for_known_formats() {
    assert_eq!(format!("{:?}", PixelFormat::NV12), "PixelFormat::NV12");
    assert_eq!(format!("{:?}", PixelFormat::YUV420P), "PixelFormat::YUV420P");
  }

  #[test]
  fn debug_falls_back_to_raw_for_unknown() {
    let unknown = PixelFormat::from_raw(-99_999);
    assert_eq!(format!("{:?}", unknown), "PixelFormat(-99999)");
  }
}

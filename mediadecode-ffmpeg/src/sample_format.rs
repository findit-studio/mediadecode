//! `SampleFormat` newtype around FFmpeg's `AVSampleFormat` discriminant.
//!
//! Same safety stance as [`crate::pix_fmt::PixelFormat`] — never cast an
//! arbitrary `i32` back into the bindgen `AVSampleFormat` enum (UB when
//! the value isn't in the build's discriminant set). Wrap the integer in
//! `SampleFormat` and dispatch on it via the associated constants below.
//!
//! Each format is one of:
//! - **Packed** (interleaved samples for multi-channel audio in one
//!   plane): `U8`, `S16`, `S32`, `S64`, `FLT`, `DBL`.
//! - **Planar** (one sample buffer per channel): `U8P`, `S16P`, `S32P`,
//!   `S64P`, `FLTP`, `DBLP`. The corresponding `AudioFrame` exposes one
//!   `Plane` per channel rather than a single interleaved buffer.

use core::fmt;

use ffmpeg_next::ffi::AVSampleFormat;

/// Audio sample format identifier.
#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct SampleFormat(i32);

impl SampleFormat {
  /// Constructs a `SampleFormat` from the raw integer FFmpeg uses for
  /// `AVCodecContext::sample_fmt` / `AVFrame::format` (audio).
  #[inline]
  pub const fn from_raw(raw: i32) -> Self {
    Self(raw)
  }

  /// Returns the underlying integer.
  #[inline]
  pub const fn raw(self) -> i32 {
    self.0
  }

  /// Returns `true` if this is a planar (one buffer per channel) format.
  #[inline]
  pub const fn is_planar(self) -> bool {
    matches!(
      self,
      Self::U8P | Self::S16P | Self::S32P | Self::S64P | Self::FLTP | Self::DBLP,
    )
  }

  /// Returns `true` if this is a packed (interleaved) format.
  #[inline]
  pub const fn is_packed(self) -> bool {
    matches!(
      self,
      Self::U8 | Self::S16 | Self::S32 | Self::S64 | Self::FLT | Self::DBL,
    )
  }

  /// Bytes per sample for known formats. `None` for [`Self::NONE`] or
  /// values outside the closed set this newtype enumerates.
  #[inline]
  pub const fn bytes_per_sample(self) -> Option<u32> {
    let bytes = match self {
      Self::U8 | Self::U8P => 1,
      Self::S16 | Self::S16P => 2,
      Self::S32 | Self::S32P | Self::FLT | Self::FLTP => 4,
      Self::S64 | Self::S64P | Self::DBL | Self::DBLP => 8,
      _ => return None,
    };
    Some(bytes)
  }

  // --- Sentinel --------------------------------------------------------

  /// Sentinel for "no format" / unset (`AV_SAMPLE_FMT_NONE`, `-1`).
  pub const NONE: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_NONE as i32);

  // --- Packed (interleaved) --------------------------------------------

  /// Unsigned 8-bit, packed.
  pub const U8: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_U8 as i32);
  /// Signed 16-bit, packed.
  pub const S16: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_S16 as i32);
  /// Signed 32-bit, packed.
  pub const S32: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_S32 as i32);
  /// Signed 64-bit, packed.
  pub const S64: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_S64 as i32);
  /// 32-bit float, packed.
  pub const FLT: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_FLT as i32);
  /// 64-bit double, packed.
  pub const DBL: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_DBL as i32);

  // --- Planar (one buffer per channel) ---------------------------------

  /// Unsigned 8-bit, planar.
  pub const U8P: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_U8P as i32);
  /// Signed 16-bit, planar.
  pub const S16P: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_S16P as i32);
  /// Signed 32-bit, planar.
  pub const S32P: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_S32P as i32);
  /// Signed 64-bit, planar.
  pub const S64P: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_S64P as i32);
  /// 32-bit float, planar.
  pub const FLTP: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_FLTP as i32);
  /// 64-bit double, planar.
  pub const DBLP: Self = Self(AVSampleFormat::AV_SAMPLE_FMT_DBLP as i32);
}

impl fmt::Debug for SampleFormat {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let name = match *self {
      Self::NONE => "NONE",
      Self::U8 => "U8",
      Self::S16 => "S16",
      Self::S32 => "S32",
      Self::S64 => "S64",
      Self::FLT => "FLT",
      Self::DBL => "DBL",
      Self::U8P => "U8P",
      Self::S16P => "S16P",
      Self::S32P => "S32P",
      Self::S64P => "S64P",
      Self::FLTP => "FLTP",
      Self::DBLP => "DBLP",
      _ => return write!(f, "SampleFormat({})", self.0),
    };
    write!(f, "SampleFormat::{name}")
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn known_constants_match_av_values() {
    assert_eq!(
      SampleFormat::S16.raw(),
      AVSampleFormat::AV_SAMPLE_FMT_S16 as i32
    );
    assert_eq!(
      SampleFormat::FLTP.raw(),
      AVSampleFormat::AV_SAMPLE_FMT_FLTP as i32
    );
    assert_eq!(SampleFormat::NONE.raw(), -1);
  }

  #[test]
  fn planar_packed_partition_is_complete() {
    let all_packed = [
      SampleFormat::U8,
      SampleFormat::S16,
      SampleFormat::S32,
      SampleFormat::S64,
      SampleFormat::FLT,
      SampleFormat::DBL,
    ];
    let all_planar = [
      SampleFormat::U8P,
      SampleFormat::S16P,
      SampleFormat::S32P,
      SampleFormat::S64P,
      SampleFormat::FLTP,
      SampleFormat::DBLP,
    ];
    for f in all_packed {
      assert!(f.is_packed());
      assert!(!f.is_planar());
    }
    for f in all_planar {
      assert!(f.is_planar());
      assert!(!f.is_packed());
    }
  }

  #[test]
  fn bytes_per_sample_matches_width() {
    assert_eq!(SampleFormat::U8.bytes_per_sample(), Some(1));
    assert_eq!(SampleFormat::S16.bytes_per_sample(), Some(2));
    assert_eq!(SampleFormat::S32P.bytes_per_sample(), Some(4));
    assert_eq!(SampleFormat::FLTP.bytes_per_sample(), Some(4));
    assert_eq!(SampleFormat::DBL.bytes_per_sample(), Some(8));
    assert_eq!(SampleFormat::NONE.bytes_per_sample(), None);
    assert_eq!(SampleFormat::from_raw(99_999).bytes_per_sample(), None);
  }

  #[test]
  fn debug_uses_name_for_known_formats() {
    assert_eq!(format!("{:?}", SampleFormat::S16), "SampleFormat::S16");
    assert_eq!(format!("{:?}", SampleFormat::FLTP), "SampleFormat::FLTP");
  }

  #[test]
  fn debug_falls_back_to_raw_for_unknown() {
    assert_eq!(
      format!("{:?}", SampleFormat::from_raw(99_999)),
      "SampleFormat(99999)"
    );
  }
}

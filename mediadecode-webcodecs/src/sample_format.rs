//! WebCodecs audio sample format mapping.
//!
//! Mirrors the
//! [`AudioSampleFormat`](https://www.w3.org/TR/webcodecs/#enumdef-audiosampleformat)
//! enum from the WebCodecs spec. Closed enum — non-`web-sys`-known
//! values surface as
//! [`AudioDecodeError::UnsupportedSampleFormat`](crate::error::AudioDecodeError::UnsupportedSampleFormat)
//! at the boundary.

use core::fmt;

/// Audio sample formats produced by WebCodecs `AudioData`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SampleFormat {
  /// 8-bit unsigned integer, interleaved.
  U8,
  /// 16-bit signed integer, interleaved.
  S16,
  /// 32-bit signed integer, interleaved.
  S32,
  /// 32-bit float, interleaved.
  F32,
  /// 8-bit unsigned integer, planar.
  U8Planar,
  /// 16-bit signed integer, planar.
  S16Planar,
  /// 32-bit signed integer, planar.
  S32Planar,
  /// 32-bit float, planar.
  F32Planar,
}

impl SampleFormat {
  /// `true` if samples are stored channel-by-channel (planar)
  /// rather than interleaved.
  pub const fn is_planar(self) -> bool {
    matches!(
      self,
      Self::U8Planar | Self::S16Planar | Self::S32Planar | Self::F32Planar
    )
  }

  /// Bytes per sample, regardless of layout.
  pub const fn bytes_per_sample(self) -> usize {
    match self {
      Self::U8 | Self::U8Planar => 1,
      Self::S16 | Self::S16Planar => 2,
      Self::S32 | Self::S32Planar | Self::F32 | Self::F32Planar => 4,
    }
  }

  /// The WebCodecs spec name (`"u8"`, `"s16"`, `"f32-planar"`, …).
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::U8 => "u8",
      Self::S16 => "s16",
      Self::S32 => "s32",
      Self::F32 => "f32",
      Self::U8Planar => "u8-planar",
      Self::S16Planar => "s16-planar",
      Self::S32Planar => "s32-planar",
      Self::F32Planar => "f32-planar",
    }
  }

  /// Parse from WebCodecs spec name.
  pub fn from_spec_name(s: &str) -> Option<Self> {
    Some(match s {
      "u8" => Self::U8,
      "s16" => Self::S16,
      "s32" => Self::S32,
      "f32" => Self::F32,
      "u8-planar" => Self::U8Planar,
      "s16-planar" => Self::S16Planar,
      "s32-planar" => Self::S32Planar,
      "f32-planar" => Self::F32Planar,
      _ => return None,
    })
  }
}

impl fmt::Display for SampleFormat {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

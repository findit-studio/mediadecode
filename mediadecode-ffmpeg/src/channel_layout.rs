//! `ChannelLayout` — channel count + native-order channel mask.
//!
//! FFmpeg 5.1+ models channel layouts as the richer `AVChannelLayout`
//! (with native / custom / unspec / ambisonic orderings, optional
//! per-channel position lists, and opaque user data). For most real
//! audio — mono / stereo / 5.1 / 7.1 / atmos — the **native** order's
//! channel-position bitmask is enough, and that's what this newtype
//! captures. Exotic per-channel custom layouts can land later as an
//! additional `Order` variant; today the design is intentionally tight.
//!
//! The bitmask uses the `AV_CH_*` constants from FFmpeg's
//! `libavutil/channel_layout.h` (re-exported here for ergonomic
//! construction).

use core::fmt;

use ffmpeg_next::ffi::{
  AV_CH_BACK_CENTER, AV_CH_BACK_LEFT, AV_CH_BACK_RIGHT, AV_CH_FRONT_CENTER, AV_CH_FRONT_LEFT,
  AV_CH_FRONT_LEFT_OF_CENTER, AV_CH_FRONT_RIGHT, AV_CH_FRONT_RIGHT_OF_CENTER, AV_CH_LOW_FREQUENCY,
  AV_CH_LOW_FREQUENCY_2, AV_CH_SIDE_LEFT, AV_CH_SIDE_RIGHT, AV_CH_TOP_BACK_CENTER,
  AV_CH_TOP_BACK_LEFT, AV_CH_TOP_BACK_RIGHT, AV_CH_TOP_CENTER, AV_CH_TOP_FRONT_CENTER,
  AV_CH_TOP_FRONT_LEFT, AV_CH_TOP_FRONT_RIGHT,
};

/// Channel layout — number of channels plus a native-order channel
/// position bitmask.
///
/// `mask == 0` indicates unspecified / unknown order with the channel
/// count still meaningful (matches FFmpeg's `AV_CHANNEL_ORDER_UNSPEC`).
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct ChannelLayout {
  nb_channels: u32,
  mask: u64,
}

impl ChannelLayout {
  /// Constructs a `ChannelLayout` from a channel count and a native-order
  /// position mask. The mask should be a bitwise OR of `AV_CH_*` constants
  /// from FFmpeg, matching the channel count exactly. No validation is
  /// performed here — pass `mask = 0` for unspecified order.
  #[inline]
  pub const fn new(nb_channels: u32, mask: u64) -> Self {
    Self { nb_channels, mask }
  }

  /// Constructs an "unspecified order" layout — channel count is known
  /// but the per-channel meaning isn't.
  #[inline]
  pub const fn unspec(nb_channels: u32) -> Self {
    Self {
      nb_channels,
      mask: 0,
    }
  }

  /// Number of channels.
  #[inline]
  pub const fn nb_channels(&self) -> u32 {
    self.nb_channels
  }

  /// Native-order position bitmask (`AV_CH_*` ORed together), or `0` for
  /// unspecified order.
  #[inline]
  pub const fn mask(&self) -> u64 {
    self.mask
  }

  /// Returns `true` if the channel order is unspecified (mask == 0).
  #[inline]
  pub const fn is_unspec(&self) -> bool {
    self.mask == 0
  }

  // --- Common constants ------------------------------------------------

  /// Mono — single front-center channel.
  pub const MONO: Self = Self::new(1, AV_CH_FRONT_CENTER);

  /// Stereo — front L+R.
  pub const STEREO: Self = Self::new(2, AV_CH_FRONT_LEFT | AV_CH_FRONT_RIGHT);

  /// 2.1 — stereo + LFE.
  pub const SURROUND_2_1: Self = Self::new(
    3,
    AV_CH_FRONT_LEFT | AV_CH_FRONT_RIGHT | AV_CH_LOW_FREQUENCY,
  );

  /// 4.0 — quad (front L+R, back L+R).
  pub const QUAD: Self = Self::new(
    4,
    AV_CH_FRONT_LEFT | AV_CH_FRONT_RIGHT | AV_CH_BACK_LEFT | AV_CH_BACK_RIGHT,
  );

  /// 5.0 — front L+C+R, side L+R.
  pub const SURROUND_5_0: Self = Self::new(
    5,
    AV_CH_FRONT_LEFT
      | AV_CH_FRONT_RIGHT
      | AV_CH_FRONT_CENTER
      | AV_CH_SIDE_LEFT
      | AV_CH_SIDE_RIGHT,
  );

  /// 5.1 — 5.0 + LFE.
  pub const SURROUND_5_1: Self = Self::new(
    6,
    AV_CH_FRONT_LEFT
      | AV_CH_FRONT_RIGHT
      | AV_CH_FRONT_CENTER
      | AV_CH_LOW_FREQUENCY
      | AV_CH_SIDE_LEFT
      | AV_CH_SIDE_RIGHT,
  );

  /// 6.1 — 5.1 + back-center.
  pub const SURROUND_6_1: Self = Self::new(
    7,
    AV_CH_FRONT_LEFT
      | AV_CH_FRONT_RIGHT
      | AV_CH_FRONT_CENTER
      | AV_CH_LOW_FREQUENCY
      | AV_CH_BACK_CENTER
      | AV_CH_SIDE_LEFT
      | AV_CH_SIDE_RIGHT,
  );

  /// 7.1 — front L+C+R, side L+R, back L+R, LFE.
  pub const SURROUND_7_1: Self = Self::new(
    8,
    AV_CH_FRONT_LEFT
      | AV_CH_FRONT_RIGHT
      | AV_CH_FRONT_CENTER
      | AV_CH_LOW_FREQUENCY
      | AV_CH_BACK_LEFT
      | AV_CH_BACK_RIGHT
      | AV_CH_SIDE_LEFT
      | AV_CH_SIDE_RIGHT,
  );

  /// 7.1 wide — front L+C+R, front L-of-center+R-of-center, side L+R, LFE.
  pub const SURROUND_7_1_WIDE: Self = Self::new(
    8,
    AV_CH_FRONT_LEFT
      | AV_CH_FRONT_RIGHT
      | AV_CH_FRONT_CENTER
      | AV_CH_LOW_FREQUENCY
      | AV_CH_FRONT_LEFT_OF_CENTER
      | AV_CH_FRONT_RIGHT_OF_CENTER
      | AV_CH_SIDE_LEFT
      | AV_CH_SIDE_RIGHT,
  );

  /// 9.1.6 Atmos — 7.1 + L/R top + 4 height channels.
  pub const ATMOS_9_1_6: Self = Self::new(
    16,
    AV_CH_FRONT_LEFT
      | AV_CH_FRONT_RIGHT
      | AV_CH_FRONT_CENTER
      | AV_CH_LOW_FREQUENCY
      | AV_CH_BACK_LEFT
      | AV_CH_BACK_RIGHT
      | AV_CH_FRONT_LEFT_OF_CENTER
      | AV_CH_FRONT_RIGHT_OF_CENTER
      | AV_CH_SIDE_LEFT
      | AV_CH_SIDE_RIGHT
      | AV_CH_TOP_FRONT_LEFT
      | AV_CH_TOP_FRONT_CENTER
      | AV_CH_TOP_FRONT_RIGHT
      | AV_CH_TOP_BACK_LEFT
      | AV_CH_TOP_CENTER
      | AV_CH_TOP_BACK_RIGHT,
  );
}

impl fmt::Debug for ChannelLayout {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let name = match *self {
      Self::MONO => "MONO",
      Self::STEREO => "STEREO",
      Self::SURROUND_2_1 => "SURROUND_2_1",
      Self::QUAD => "QUAD",
      Self::SURROUND_5_0 => "SURROUND_5_0",
      Self::SURROUND_5_1 => "SURROUND_5_1",
      Self::SURROUND_6_1 => "SURROUND_6_1",
      Self::SURROUND_7_1 => "SURROUND_7_1",
      Self::SURROUND_7_1_WIDE => "SURROUND_7_1_WIDE",
      Self::ATMOS_9_1_6 => "ATMOS_9_1_6",
      _ => {
        return f
          .debug_struct("ChannelLayout")
          .field("nb_channels", &self.nb_channels)
          .field("mask", &format_args!("{:#x}", self.mask))
          .finish();
      }
    };
    write!(f, "ChannelLayout::{name}")
  }
}

// Suppress the unused-import warnings for `AV_CH_*` constants we don't
// reference in any preset above (kept imported so future preset
// additions don't need to revisit the use list).
#[allow(dead_code)]
const _UNUSED_AV_CH: [u64; 2] = [AV_CH_LOW_FREQUENCY_2, AV_CH_TOP_BACK_CENTER];

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn mono_stereo_basic() {
    assert_eq!(ChannelLayout::MONO.nb_channels(), 1);
    assert_eq!(ChannelLayout::STEREO.nb_channels(), 2);
    assert_eq!(ChannelLayout::SURROUND_5_1.nb_channels(), 6);
    assert_eq!(ChannelLayout::SURROUND_7_1.nb_channels(), 8);
    assert_eq!(ChannelLayout::ATMOS_9_1_6.nb_channels(), 16);
  }

  #[test]
  fn unspec_has_zero_mask() {
    let lay = ChannelLayout::unspec(4);
    assert_eq!(lay.nb_channels(), 4);
    assert_eq!(lay.mask(), 0);
    assert!(lay.is_unspec());
    assert!(!ChannelLayout::STEREO.is_unspec());
  }

  #[test]
  fn equality_is_value_based() {
    assert_eq!(ChannelLayout::STEREO, ChannelLayout::new(2, ChannelLayout::STEREO.mask()));
    assert_ne!(ChannelLayout::STEREO, ChannelLayout::SURROUND_5_1);
  }

  #[test]
  fn debug_names_known_layouts() {
    assert_eq!(format!("{:?}", ChannelLayout::MONO), "ChannelLayout::MONO");
    assert_eq!(format!("{:?}", ChannelLayout::SURROUND_5_1), "ChannelLayout::SURROUND_5_1");
  }

  #[test]
  fn debug_falls_back_to_struct_form_for_unknown() {
    let unknown = ChannelLayout::new(3, 0b111);
    let printed = format!("{unknown:?}");
    assert!(printed.contains("nb_channels: 3"), "got {printed}");
    assert!(printed.contains("0x7"), "got {printed}");
  }

  #[test]
  fn cloned_eq() {
    let a = ChannelLayout::SURROUND_5_1;
    let b = a.clone();
    assert_eq!(a, b);
  }
}

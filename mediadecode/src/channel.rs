//! Audio channel layout types.
//!
//! Four-layer model:
//! 1. [`ChannelLayoutKind`] — high-level "is this stereo / 5.1 / Atmos /
//!    …?" tag, independent of the underlying ordering.
//! 2. [`AudioChannelOrderKind`] — how the channels are ordered
//!    (Native bitmask / Custom per-channel list / Ambisonic / Unspecified),
//!    matching FFmpeg's `AVChannelOrder` taxonomy.
//! 3. [`AudioChannelSpec`] — for a custom-order layout, one entry per
//!    channel: an index, a backend-specific raw id, and an optional label.
//! 4. [`AudioChannelLayout`] — the bundle: order + channel count + known
//!    kind + native bitmask (when applicable) + custom channel list (when
//!    applicable) + free-form description.
//!
//! The two enums work without `alloc`. The two structs require the
//! `alloc` feature because they hold `Vec` / `SmolStr` payloads.

use derive_more::{Display, IsVariant};

/// The kind of channel layout, abstracting the specific layout details
/// into a more general category.
///
/// Roughly mirrors FFmpeg's named-layout set (`AV_CHANNEL_LAYOUT_*`)
/// without committing to that namespace's exact integer values; use
/// [`Self::to_u32`] / [`Self::from_u32`] when you need a stable wire
/// representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, IsVariant)]
#[non_exhaustive]
pub enum ChannelLayoutKind {
  /// Mono channel layout, typically with a single audio channel.
  #[display("mono")]
  Mono,
  /// Stereo channel layout, typically with two audio channels (left and right).
  #[display("stereo")]
  Stereo,
  /// Stereo downmix channel layout, which is a stereo representation of a multi-channel audio layout.
  #[display("stereo downmix")]
  StereoDownmix,
  /// Surround channel layout, typically with three audio channels (left, center, right) and sometimes additional channels for rear or height speakers.
  #[display("surround")]
  Surround,
  /// Quad channel layout, typically with four audio channels (front left, front right, rear left, rear right).
  #[display("quad")]
  Quad,
  /// Hexagonal channel layout, typically with six audio channels arranged in a hexagonal pattern.
  #[display("hexagonal")]
  Hexagonal,
  /// Octagonal channel layout, typically with eight audio channels arranged in an octagonal pattern.
  #[display("octagonal")]
  Octagonal,
  /// Hexadecagonal channel layout, typically with sixteen audio channels arranged in a hexadecagonal pattern.
  #[display("hexadecagonal")]
  Hexadecagonal,
  /// Cube channel layout, typically with eight audio channels arranged in a cube pattern.
  #[display("cube")]
  Cube,
  /// 2.1 channel layout, typically with three audio channels (left, right, and a subwoofer).
  #[display("2.1")]
  Ch2_1,
  /// 2.1 alternative channel layout, which is an alternative representation of the 2.1 channel layout.
  #[display("2.1 alternative")]
  Ch2_1Alt,
  /// 2.2 channel layout, typically with four audio channels (left, right, subwoofer, and an additional channel for height or rear speakers).
  #[display("2.2")]
  Ch2_2,
  /// 3.1 channel layout, typically with four audio channels (left, center, right, and a subwoofer).
  #[display("3.1")]
  Ch3_1,
  /// 3.1.2 channel layout, typically with six audio channels (left, center, right, subwoofer, and two additional channels for height or rear speakers).
  #[display("3.1.2")]
  Ch3_1_2,
  /// 4.0 channel layout, typically with four audio channels (front left, front right, rear left, rear right) without a center channel or subwoofer.
  #[display("4.0")]
  Ch4_0,
  /// 4.1 channel layout, typically with five audio channels (front left, front right, rear left, rear right, and a center channel) without a subwoofer.
  #[display("4.1")]
  Ch4_1,
  /// 5.0 channel layout, typically with five audio channels (front left, front right, center, rear left, rear right) without a subwoofer.
  #[display("5.0")]
  Ch5_0,
  /// 5.0 back channel layout, which is a variation of the 5.0 channel layout with the rear channels positioned behind the listener.
  #[display("5.0 back")]
  Ch5_0Back,
  /// 5.1 channel layout, typically with six audio channels (front left, front right, center, rear left, rear right, and a subwoofer).
  #[display("5.1")]
  Ch5_1,
  /// 5.1 back channel layout, which is a variation of the 5.1 channel layout with the rear channels positioned behind the listener.
  #[display("5.1 back")]
  Ch5_1Back,
  /// 5.1.2 back channel layout, which is a variation of the 5.1 channel layout with two additional channels for height or rear speakers positioned behind the listener.
  #[display("5.1.2 back")]
  Ch5_1_2Back,
  /// 5.1.4 back channel layout, which is a variation of the 5.1 channel layout with four additional channels for height or rear speakers positioned behind the listener.
  #[display("5.1.4 back")]
  Ch5_1_4Back,
  /// 6.0 channel layout, typically with six audio channels (front left, front right, center, rear left, rear right, and an additional channel for height or rear speakers) without a subwoofer.
  #[display("6.0")]
  Ch6_0,
  /// 6.0 front channel layout, which is a variation of the 6.0 channel layout with the additional channel for height or rear speakers positioned in front of the listener.
  #[display("6.0 front")]
  Ch6_0Front,
  /// 6.1 channel layout, typically with seven audio channels (front left, front right, center, rear left, rear right, an additional channel for height or rear speakers, and a subwoofer).
  #[display("6.1")]
  Ch6_1,
  /// 6.1 back channel layout, which is a variation of the 6.1 channel layout with the additional channel for height or rear speakers positioned behind the listener.
  #[display("6.1 back")]
  Ch6_1Back,
  /// 6.1 front channel layout, which is a variation of the 6.1 channel layout with the additional channel for height or rear speakers positioned in front of the listener.
  #[display("6.1 front")]
  Ch6_1Front,
  /// 7.0 channel layout, typically with seven audio channels (front left, front right, center, rear left, rear right, and two additional channels for height or rear speakers) without a subwoofer.
  #[display("7.0")]
  Ch7_0,
  /// 7.0 front channel layout, which is a variation of the 7.0 channel layout with the two additional channels for height or rear speakers positioned in front of the listener.
  #[display("7.0 front")]
  Ch7_0Front,
  /// 7.1 channel layout, typically with eight audio channels (front left, front right, center, rear left, rear right, two additional channels for height or rear speakers, and a subwoofer).
  #[display("7.1")]
  Ch7_1,
  /// 7.1 wide channel layout, which is a variation of the 7.1 channel layout with the two additional channels for height or rear speakers positioned wider than the standard 7.1 layout.
  #[display("7.1 wide")]
  Ch7_1Wide,
  /// 7.1 wide back channel layout, which is a variation of the 7.1 wide channel layout with the two additional channels for height or rear speakers positioned behind the listener.
  #[display("7.1 wide back")]
  Ch7_1WideBack,
  /// 7.1 top back channel layout, which is a variation of the 7.1 channel layout with the two additional channels for height or rear speakers positioned above and behind the listener.
  #[display("7.1 top back")]
  Ch7_1TopBack,
  /// 7.1.2 channel layout, which is a variation of the 7.1 channel layout with two additional channels for height or rear speakers.
  #[display("7.1.2")]
  Ch7_1_2,
  /// 7.1.4 back channel layout, which is a variation of the 7.1 channel layout with four additional channels for height or rear speakers positioned behind the listener.
  #[display("7.1.4 back")]
  Ch7_1_4Back,
  /// 7.2.3 channel layout, which is a variation of the 7.1 channel layout with two additional channels for height or rear speakers and three additional channels for height or rear speakers positioned behind the listener.
  #[display("7.2.3")]
  Ch7_2_3,
  /// 9.1.4 back channel layout, which is a variation of the 7.1 channel layout with two additional channels for height or rear speakers and four additional channels for height or rear speakers positioned behind the listener.
  #[display("9.1.4 back")]
  Ch9_1_4Back,
  /// 22.2 channel layout, typically with twenty-four audio channels arranged in a 22.2 configuration.
  #[display("22.2")]
  Ch22_2,
  /// Unknown channel layout kind, represents any channel layout that does not fit into the predefined categories.
  #[display("unknown")]
  Unknown,
}

impl Default for ChannelLayoutKind {
  #[cfg_attr(not(tarpaulin), inline(always))]
  fn default() -> Self {
    Self::Unknown
  }
}

impl ChannelLayoutKind {
  /// Decode from the stable `u32` representation produced by [`Self::to_u32`].
  /// Unrecognised values map to [`Self::Unknown`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn from_u32(value: u32) -> Self {
    match value {
      1 => Self::Mono,
      2 => Self::Stereo,
      3 => Self::StereoDownmix,
      4 => Self::Surround,
      5 => Self::Quad,
      6 => Self::Hexagonal,
      7 => Self::Octagonal,
      8 => Self::Hexadecagonal,
      9 => Self::Cube,
      10 => Self::Ch2_1,
      11 => Self::Ch2_1Alt,
      12 => Self::Ch2_2,
      13 => Self::Ch3_1,
      14 => Self::Ch3_1_2,
      15 => Self::Ch4_0,
      16 => Self::Ch4_1,
      17 => Self::Ch5_0,
      18 => Self::Ch5_0Back,
      19 => Self::Ch5_1,
      20 => Self::Ch5_1Back,
      21 => Self::Ch5_1_2Back,
      22 => Self::Ch5_1_4Back,
      23 => Self::Ch6_0,
      24 => Self::Ch6_0Front,
      25 => Self::Ch6_1,
      26 => Self::Ch6_1Back,
      27 => Self::Ch6_1Front,
      28 => Self::Ch7_0,
      29 => Self::Ch7_0Front,
      30 => Self::Ch7_1,
      31 => Self::Ch7_1Wide,
      32 => Self::Ch7_1WideBack,
      33 => Self::Ch7_1TopBack,
      34 => Self::Ch7_1_2,
      35 => Self::Ch7_1_4Back,
      36 => Self::Ch7_2_3,
      37 => Self::Ch9_1_4Back,
      38 => Self::Ch22_2,
      _ => Self::Unknown,
    }
  }

  /// Stable wire representation. `0` always means [`Self::Unknown`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn to_u32(self) -> u32 {
    match self {
      Self::Unknown => 0,
      Self::Mono => 1,
      Self::Stereo => 2,
      Self::StereoDownmix => 3,
      Self::Surround => 4,
      Self::Quad => 5,
      Self::Hexagonal => 6,
      Self::Octagonal => 7,
      Self::Hexadecagonal => 8,
      Self::Cube => 9,
      Self::Ch2_1 => 10,
      Self::Ch2_1Alt => 11,
      Self::Ch2_2 => 12,
      Self::Ch3_1 => 13,
      Self::Ch3_1_2 => 14,
      Self::Ch4_0 => 15,
      Self::Ch4_1 => 16,
      Self::Ch5_0 => 17,
      Self::Ch5_0Back => 18,
      Self::Ch5_1 => 19,
      Self::Ch5_1Back => 20,
      Self::Ch5_1_2Back => 21,
      Self::Ch5_1_4Back => 22,
      Self::Ch6_0 => 23,
      Self::Ch6_0Front => 24,
      Self::Ch6_1 => 25,
      Self::Ch6_1Back => 26,
      Self::Ch6_1Front => 27,
      Self::Ch7_0 => 28,
      Self::Ch7_0Front => 29,
      Self::Ch7_1 => 30,
      Self::Ch7_1Wide => 31,
      Self::Ch7_1WideBack => 32,
      Self::Ch7_1TopBack => 33,
      Self::Ch7_1_2 => 34,
      Self::Ch7_1_4Back => 35,
      Self::Ch7_2_3 => 36,
      Self::Ch9_1_4Back => 37,
      Self::Ch22_2 => 38,
    }
  }
}

/// How the channels in an [`AudioChannelLayout`] are ordered.
///
/// Mirrors FFmpeg's `AVChannelOrder`. Stable wire integers are
/// `repr(u32)` and match the `to_u32` / `from_u32` mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum AudioChannelOrderKind {
  /// Channel order is unknown / not communicated by the source.
  #[default]
  Unspecified = 0,
  /// Native order: positions identified by a bitmask of well-known
  /// channel-position bits (see `AV_CH_*` in FFmpeg, or
  /// [`AudioChannelLayout::native_mask`]).
  Native = 1,
  /// Custom order: channels are listed explicitly in
  /// [`AudioChannelLayout::custom_channels`].
  Custom = 2,
  /// Ambisonic order, optionally with an extra non-diegetic stereo
  /// pair (FFmpeg-style).
  Ambisonic = 3,
}

impl AudioChannelOrderKind {
  /// Decode from the stable `u32` representation. Unrecognised values
  /// map to [`Self::Unspecified`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn from_u32(value: u32) -> Self {
    match value {
      1 => Self::Native,
      2 => Self::Custom,
      3 => Self::Ambisonic,
      _ => Self::Unspecified,
    }
  }

  /// Stable wire representation. `0` always means [`Self::Unspecified`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn as_u32(self) -> u32 {
    self as u32
  }
}

// ---------------------------------------------------------------------------
//  Alloc-gated structs (`AudioChannelSpec`, `AudioChannelLayout`).
// ---------------------------------------------------------------------------

#[cfg(any(feature = "alloc", feature = "std"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "alloc", feature = "std"))))]
pub use alloc_only::{AudioChannelLayout, AudioChannelSpec};

#[cfg(any(feature = "alloc", feature = "std"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "alloc", feature = "std"))))]
mod alloc_only {
  use super::{AudioChannelOrderKind, ChannelLayoutKind};
  use smol_str::SmolStr;
  use std::vec::Vec;

  /// One entry in a [`AudioChannelLayout::custom_channels`] list — the
  /// per-channel description for a [`AudioChannelOrderKind::Custom`]
  /// layout.  
  #[derive(Debug, Clone, PartialEq, Eq, Default)]
  pub struct AudioChannelSpec {
    index: u32,
    raw_id: u32,
    label: SmolStr,
  }

  impl AudioChannelSpec {
    /// Constructs an `AudioChannelSpec` with the given channel index
    /// and backend-specific raw id. Label defaults to empty.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn new(index: u32, raw_id: u32) -> Self {
      Self {
        index,
        raw_id,
        label: SmolStr::new_inline(""),
      }
    }

    /// Index of this channel in the layout (0-based).
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn index(&self) -> u32 {
      self.index
    }

    /// Backend-specific channel id (e.g. FFmpeg's `AVChannel` integer).
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn raw_id(&self) -> u32 {
      self.raw_id
    }

    /// Human-readable label, or the empty string if unspecified.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub fn label(&self) -> &str {
      self.label.as_str()
    }

    /// Sets the channel index (consuming builder).
    #[cfg_attr(not(tarpaulin), inline(always))]
    #[must_use]
    pub const fn with_index(mut self, value: u32) -> Self {
      self.set_index(value);
      self
    }

    /// Sets the channel index in place.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn set_index(&mut self, value: u32) -> &mut Self {
      self.index = value;
      self
    }

    /// Sets the raw id (consuming builder).
    #[cfg_attr(not(tarpaulin), inline(always))]
    #[must_use]
    pub const fn with_raw_id(mut self, value: u32) -> Self {
      self.set_raw_id(value);
      self
    }

    /// Sets the raw id in place.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn set_raw_id(&mut self, value: u32) -> &mut Self {
      self.raw_id = value;
      self
    }

    /// Sets the label (consuming builder).
    #[cfg_attr(not(tarpaulin), inline(always))]
    #[must_use]
    pub fn with_label(mut self, value: impl Into<SmolStr>) -> Self {
      self.set_label(value);
      self
    }

    /// Sets the label in place.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub fn set_label(&mut self, value: impl Into<SmolStr>) -> &mut Self {
      self.label = value.into();
      self
    }
  }

  /// Audio channel layout — order + channel count + identification.
  ///
  /// The bundle FFmpeg's `AVChannelLayout` carries through to consumers,
  /// rendered as plain Rust data:
  ///
  /// - [`order`](Self::order) — Native / Custom / Ambisonic / Unspecified.
  /// - [`channels`](Self::channels) — total count.
  /// - [`known_kind`](Self::known_kind) — high-level "is this 5.1 / 7.1 /
  ///   Atmos / …" tag, [`ChannelLayoutKind::Unknown`] when none of the
  ///   well-known shapes match.
  /// - [`native_mask`](Self::native_mask) — `Some(bitmask)` for
  ///   [`AudioChannelOrderKind::Native`] / [`AudioChannelOrderKind::Ambisonic`],
  ///   `None` otherwise.
  /// - [`custom_channels`](Self::custom_channels) — populated for
  ///   [`AudioChannelOrderKind::Custom`] layouts; one [`AudioChannelSpec`]
  ///   per channel.
  /// - [`description`](Self::description) — free-form human-readable
  ///   description (e.g. FFmpeg's `av_channel_layout_describe` output).
  #[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
  #[derive(Debug, Clone, PartialEq, Eq, Default)]
  pub struct AudioChannelLayout {
    order: AudioChannelOrderKind,
    channels: u32,
    known_kind: ChannelLayoutKind,
    native_mask: Option<u64>,
    custom_channels: Vec<AudioChannelSpec>,
    description: SmolStr,
  }

  impl AudioChannelLayout {
    /// Constructs a minimal `AudioChannelLayout` with the given channel
    /// count. All other fields are at their default values
    /// (`Unspecified` / `Unknown` / empty); use the `with_*` builders to
    /// fill them in.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn new(channels: u32) -> Self {
      Self {
        channels,
        order: AudioChannelOrderKind::Unspecified,
        known_kind: ChannelLayoutKind::Unknown,
        native_mask: None,
        custom_channels: Vec::new(),
        description: SmolStr::new_inline(""),
      }
    }

    /// Channel ordering (Native / Custom / Ambisonic / Unspecified).
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn order(&self) -> AudioChannelOrderKind {
      self.order
    }

    /// Total channel count.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn channels(&self) -> u32 {
      self.channels
    }

    /// High-level layout tag, or [`ChannelLayoutKind::Unknown`] if no
    /// well-known shape matches.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn known_kind(&self) -> ChannelLayoutKind {
      self.known_kind
    }

    /// Native-order bitmask of `AV_CH_*` channel positions, when
    /// applicable. `None` for Custom / Unspecified orders or when the
    /// mask is zero.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn native_mask(&self) -> Option<u64> {
      self.native_mask
    }

    /// Per-channel descriptors for [`AudioChannelOrderKind::Custom`]
    /// layouts; empty otherwise.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn custom_channels(&self) -> &[AudioChannelSpec] {
      self.custom_channels.as_slice()
    }

    /// Human-readable description (e.g. `"5.1(side)"`,
    /// `"3 channels (FL+FR+LFE)"`).
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub fn description(&self) -> &str {
      self.description.as_str()
    }

    /// `true` when every field is at its default (zero channels,
    /// `Unspecified` order, `Unknown` kind, no mask, no custom channels,
    /// empty description). Useful as an "uninitialized" sentinel.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub fn is_empty(&self) -> bool {
      self.channels == 0
        && self.order == AudioChannelOrderKind::Unspecified
        && self.known_kind == ChannelLayoutKind::Unknown
        && self.native_mask.is_none()
        && self.custom_channels.is_empty()
        && self.description.is_empty()
    }

    /// Sets the order (consuming builder).
    #[cfg_attr(not(tarpaulin), inline(always))]
    #[must_use]
    pub const fn with_order(mut self, value: AudioChannelOrderKind) -> Self {
      self.set_order(value);
      self
    }

    /// Sets the order in place.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn set_order(&mut self, value: AudioChannelOrderKind) -> &mut Self {
      self.order = value;
      self
    }

    /// Sets the channel count (consuming builder).
    #[cfg_attr(not(tarpaulin), inline(always))]
    #[must_use]
    pub const fn with_channels(mut self, value: u32) -> Self {
      self.set_channels(value);
      self
    }

    /// Sets the channel count in place.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn set_channels(&mut self, value: u32) -> &mut Self {
      self.channels = value;
      self
    }

    /// Sets the high-level layout tag (consuming builder).
    #[cfg_attr(not(tarpaulin), inline(always))]
    #[must_use]
    pub const fn with_known_kind(mut self, value: ChannelLayoutKind) -> Self {
      self.set_known_kind(value);
      self
    }

    /// Sets the high-level layout tag in place.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn set_known_kind(&mut self, value: ChannelLayoutKind) -> &mut Self {
      self.known_kind = value;
      self
    }

    /// Sets the native-order bitmask (consuming builder).
    #[cfg_attr(not(tarpaulin), inline(always))]
    #[must_use]
    pub const fn with_native_mask(mut self, value: Option<u64>) -> Self {
      self.set_native_mask(value);
      self
    }

    /// Sets the native-order bitmask in place.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub const fn set_native_mask(&mut self, value: Option<u64>) -> &mut Self {
      self.native_mask = value;
      self
    }

    /// Sets the custom-order channel list (consuming builder).
    #[cfg_attr(not(tarpaulin), inline(always))]
    #[must_use]
    pub fn with_custom_channels(mut self, value: Vec<AudioChannelSpec>) -> Self {
      self.set_custom_channels(value);
      self
    }

    /// Sets the custom-order channel list in place.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub fn set_custom_channels(&mut self, value: Vec<AudioChannelSpec>) -> &mut Self {
      self.custom_channels = value;
      self
    }

    /// Sets the human-readable description (consuming builder).
    #[cfg_attr(not(tarpaulin), inline(always))]
    #[must_use]
    pub fn with_description(mut self, value: impl Into<SmolStr>) -> Self {
      self.set_description(value);
      self
    }

    /// Sets the human-readable description in place.
    #[cfg_attr(not(tarpaulin), inline(always))]
    pub fn set_description(&mut self, value: impl Into<SmolStr>) -> &mut Self {
      self.description = value.into();
      self
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  // -----------------------------------------------------------------
  //  ChannelLayoutKind
  // -----------------------------------------------------------------

  #[test]
  fn channel_layout_kind_default_is_unknown() {
    assert!(matches!(
      ChannelLayoutKind::default(),
      ChannelLayoutKind::Unknown
    ));
  }

  #[test]
  fn channel_layout_kind_round_trip_u32() {
    let all = [
      ChannelLayoutKind::Unknown,
      ChannelLayoutKind::Mono,
      ChannelLayoutKind::Stereo,
      ChannelLayoutKind::StereoDownmix,
      ChannelLayoutKind::Surround,
      ChannelLayoutKind::Quad,
      ChannelLayoutKind::Hexagonal,
      ChannelLayoutKind::Octagonal,
      ChannelLayoutKind::Hexadecagonal,
      ChannelLayoutKind::Cube,
      ChannelLayoutKind::Ch2_1,
      ChannelLayoutKind::Ch2_1Alt,
      ChannelLayoutKind::Ch2_2,
      ChannelLayoutKind::Ch3_1,
      ChannelLayoutKind::Ch3_1_2,
      ChannelLayoutKind::Ch4_0,
      ChannelLayoutKind::Ch4_1,
      ChannelLayoutKind::Ch5_0,
      ChannelLayoutKind::Ch5_0Back,
      ChannelLayoutKind::Ch5_1,
      ChannelLayoutKind::Ch5_1Back,
      ChannelLayoutKind::Ch5_1_2Back,
      ChannelLayoutKind::Ch5_1_4Back,
      ChannelLayoutKind::Ch6_0,
      ChannelLayoutKind::Ch6_0Front,
      ChannelLayoutKind::Ch6_1,
      ChannelLayoutKind::Ch6_1Back,
      ChannelLayoutKind::Ch6_1Front,
      ChannelLayoutKind::Ch7_0,
      ChannelLayoutKind::Ch7_0Front,
      ChannelLayoutKind::Ch7_1,
      ChannelLayoutKind::Ch7_1Wide,
      ChannelLayoutKind::Ch7_1WideBack,
      ChannelLayoutKind::Ch7_1TopBack,
      ChannelLayoutKind::Ch7_1_2,
      ChannelLayoutKind::Ch7_1_4Back,
      ChannelLayoutKind::Ch7_2_3,
      ChannelLayoutKind::Ch9_1_4Back,
      ChannelLayoutKind::Ch22_2,
    ];
    for kind in all {
      let n = kind.to_u32();
      assert_eq!(
        ChannelLayoutKind::from_u32(n),
        kind,
        "round-trip failed for {kind:?}"
      );
    }
  }

  #[test]
  fn channel_layout_kind_unknown_for_garbage() {
    assert_eq!(
      ChannelLayoutKind::from_u32(99_999),
      ChannelLayoutKind::Unknown
    );
    assert_eq!(ChannelLayoutKind::from_u32(0), ChannelLayoutKind::Unknown);
  }

  // `format!` requires an allocator; gate to alloc-or-std builds.
  #[cfg(any(feature = "alloc", feature = "std"))]
  #[test]
  fn channel_layout_kind_display() {
    assert_eq!(format!("{}", ChannelLayoutKind::Mono), "mono");
    assert_eq!(format!("{}", ChannelLayoutKind::Ch5_1), "5.1");
    assert_eq!(
      format!("{}", ChannelLayoutKind::Ch7_1WideBack),
      "7.1 wide back"
    );
    assert_eq!(format!("{}", ChannelLayoutKind::Unknown), "unknown");
  }

  #[test]
  fn channel_layout_kind_is_variant() {
    assert!(ChannelLayoutKind::Mono.is_mono());
    assert!(!ChannelLayoutKind::Stereo.is_mono());
    assert!(ChannelLayoutKind::Ch5_1.is_ch_5_1());
    assert!(ChannelLayoutKind::Unknown.is_unknown());
  }

  // -----------------------------------------------------------------
  //  AudioChannelOrderKind
  // -----------------------------------------------------------------

  #[test]
  fn order_default_is_unspecified() {
    assert_eq!(
      AudioChannelOrderKind::default(),
      AudioChannelOrderKind::Unspecified
    );
  }

  #[test]
  fn order_round_trip_u32() {
    for o in [
      AudioChannelOrderKind::Unspecified,
      AudioChannelOrderKind::Native,
      AudioChannelOrderKind::Custom,
      AudioChannelOrderKind::Ambisonic,
    ] {
      assert_eq!(AudioChannelOrderKind::from_u32(o.as_u32()), o);
    }
  }

  #[test]
  fn order_unspecified_for_garbage() {
    assert_eq!(
      AudioChannelOrderKind::from_u32(42),
      AudioChannelOrderKind::Unspecified
    );
    assert_eq!(
      AudioChannelOrderKind::from_u32(0),
      AudioChannelOrderKind::Unspecified
    );
  }

  #[test]
  fn order_repr_matches_as_u32() {
    // The repr(u32) discriminants must match what `as_u32` returns.
    assert_eq!(AudioChannelOrderKind::Unspecified as u32, 0);
    assert_eq!(AudioChannelOrderKind::Native as u32, 1);
    assert_eq!(AudioChannelOrderKind::Custom as u32, 2);
    assert_eq!(AudioChannelOrderKind::Ambisonic as u32, 3);
    assert_eq!(AudioChannelOrderKind::Native.as_u32(), 1);
  }

  // -----------------------------------------------------------------
  //  AudioChannelSpec  /  AudioChannelLayout (alloc-gated)
  // -----------------------------------------------------------------

  #[cfg(any(feature = "std", feature = "alloc"))]
  mod alloc_tests {
    use super::*;

    #[test]
    fn spec_construct_and_access() {
      let s = AudioChannelSpec::new(2, 4);
      assert_eq!(s.index(), 2);
      assert_eq!(s.raw_id(), 4);
      assert_eq!(s.label(), "");
    }

    #[test]
    fn spec_builders_chain() {
      let s = AudioChannelSpec::default()
        .with_index(1)
        .with_raw_id(3)
        .with_label("FL");
      assert_eq!(s.index(), 1);
      assert_eq!(s.raw_id(), 3);
      assert_eq!(s.label(), "FL");
    }

    #[test]
    fn spec_setters_chain() {
      let mut s = AudioChannelSpec::default();
      s.set_index(7).set_raw_id(11).set_label("BC");
      assert_eq!(s.index(), 7);
      assert_eq!(s.raw_id(), 11);
      assert_eq!(s.label(), "BC");
    }

    #[test]
    fn layout_default_is_empty() {
      let l = AudioChannelLayout::default();
      assert!(l.is_empty());
      assert_eq!(l.channels(), 0);
      assert_eq!(l.order(), AudioChannelOrderKind::Unspecified);
      assert_eq!(l.known_kind(), ChannelLayoutKind::Unknown);
      assert!(l.native_mask().is_none());
      assert!(l.custom_channels().is_empty());
      assert_eq!(l.description(), "");
    }

    #[test]
    fn layout_new_with_channels_only() {
      let l = AudioChannelLayout::new(6);
      assert!(!l.is_empty()); // channels > 0
      assert_eq!(l.channels(), 6);
    }

    #[test]
    fn layout_builders_chain() {
      let l = AudioChannelLayout::new(6)
        .with_order(AudioChannelOrderKind::Native)
        .with_known_kind(ChannelLayoutKind::Ch5_1)
        .with_native_mask(Some(0x3F))
        .with_description("5.1 side");
      assert_eq!(l.channels(), 6);
      assert_eq!(l.order(), AudioChannelOrderKind::Native);
      assert_eq!(l.known_kind(), ChannelLayoutKind::Ch5_1);
      assert_eq!(l.native_mask(), Some(0x3F));
      assert_eq!(l.description(), "5.1 side");
    }

    #[test]
    fn layout_custom_channels_round_trip() {
      let custom = vec![
        AudioChannelSpec::new(0, 1).with_label("FL"),
        AudioChannelSpec::new(1, 2).with_label("FR"),
      ];
      let l = AudioChannelLayout::new(2)
        .with_order(AudioChannelOrderKind::Custom)
        .with_custom_channels(custom);
      assert_eq!(l.custom_channels().len(), 2);
      assert_eq!(l.custom_channels()[0].label(), "FL");
      assert_eq!(l.custom_channels()[1].label(), "FR");
    }

    #[test]
    fn layout_setters_chain() {
      let mut l = AudioChannelLayout::default();
      l.set_channels(8)
        .set_order(AudioChannelOrderKind::Native)
        .set_known_kind(ChannelLayoutKind::Ch7_1)
        .set_native_mask(Some(0x63F));
      assert_eq!(l.channels(), 8);
      assert!(matches!(l.known_kind(), ChannelLayoutKind::Ch7_1));
    }
  }
}

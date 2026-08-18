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
//!
//! Both enums carry a canonical **lowercase slug**: `as_str` renders it,
//! [`Display`](core::fmt::Display) prints exactly that, and
//! [`FromStr`](core::str::FromStr) reads it back folding ASCII case, so
//! `"5.1-back"` and `"5.1-BACK"` are one value. Case is the whole of the
//! folding — no alias, no trimming, no second spelling for one variant —
//! and the fold allocates nothing, so the door is the same at the
//! no-`alloc` tier. The slug is the text form; `to_u32` / `as_u32` stay
//! the compact numeric one.

use core::str::FromStr;

use derive_more::{Display, IsVariant};
use thiserror::Error;

/// The kind of channel layout, abstracting the specific layout details
/// into a more general category.
///
/// Roughly mirrors FFmpeg's named-layout set (`AV_CHANNEL_LAYOUT_*`)
/// without committing to that namespace's exact integer values; use
/// [`Self::to_u32`] / [`Self::from_u32`] when you need a stable wire
/// representation, [`Self::as_str`] / [`FromStr`] for the text one.
///
/// **Closed set.** A layout this tag cannot name is [`Self::Unknown`],
/// not an owned `Other(…)` escape. The tag classifies an
/// [`AudioChannelLayout`], and that record already keeps the
/// unclassifiable case losslessly — `native_mask`, `custom_channels`
/// and the free-form `description` (FFmpeg's own
/// `av_channel_layout_describe` rendering) — so an escape arm here
/// would duplicate a neighbouring field while costing the enum both
/// `Copy` and its place at the no-`alloc` tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, IsVariant)]
#[display("{}", self.as_str())]
#[non_exhaustive]
pub enum ChannelLayoutKind {
  /// Mono channel layout, typically with a single audio channel.
  Mono,
  /// Stereo channel layout, typically with two audio channels (left and right).
  Stereo,
  /// Stereo downmix channel layout, which is a stereo representation of a multi-channel audio layout.
  StereoDownmix,
  /// Surround channel layout, typically with three audio channels (left, center, right) and sometimes additional channels for rear or height speakers.
  Surround,
  /// Quad channel layout, typically with four audio channels (front left, front right, rear left, rear right).
  Quad,
  /// Hexagonal channel layout, typically with six audio channels arranged in a hexagonal pattern.
  Hexagonal,
  /// Octagonal channel layout, typically with eight audio channels arranged in an octagonal pattern.
  Octagonal,
  /// Hexadecagonal channel layout, typically with sixteen audio channels arranged in a hexadecagonal pattern.
  Hexadecagonal,
  /// Cube channel layout, typically with eight audio channels arranged in a cube pattern.
  Cube,
  /// 2.1 channel layout, typically with three audio channels (left, right, and a subwoofer).
  Ch2_1,
  /// 2.1 alternative channel layout, which is an alternative representation of the 2.1 channel layout.
  Ch2_1Alt,
  /// 2.2 channel layout, typically with four audio channels (left, right, subwoofer, and an additional channel for height or rear speakers).
  Ch2_2,
  /// 3.1 channel layout, typically with four audio channels (left, center, right, and a subwoofer).
  Ch3_1,
  /// 3.1.2 channel layout, typically with six audio channels (left, center, right, subwoofer, and two additional channels for height or rear speakers).
  Ch3_1_2,
  /// 4.0 channel layout, typically with four audio channels (front left, front right, rear left, rear right) without a center channel or subwoofer.
  Ch4_0,
  /// 4.1 channel layout, typically with five audio channels (front left, front right, rear left, rear right, and a center channel) without a subwoofer.
  Ch4_1,
  /// 5.0 channel layout, typically with five audio channels (front left, front right, center, rear left, rear right) without a subwoofer.
  Ch5_0,
  /// 5.0 back channel layout, which is a variation of the 5.0 channel layout with the rear channels positioned behind the listener.
  Ch5_0Back,
  /// 5.1 channel layout, typically with six audio channels (front left, front right, center, rear left, rear right, and a subwoofer).
  Ch5_1,
  /// 5.1 back channel layout, which is a variation of the 5.1 channel layout with the rear channels positioned behind the listener.
  Ch5_1Back,
  /// 5.1.2 back channel layout, which is a variation of the 5.1 channel layout with two additional channels for height or rear speakers positioned behind the listener.
  Ch5_1_2Back,
  /// 5.1.4 back channel layout, which is a variation of the 5.1 channel layout with four additional channels for height or rear speakers positioned behind the listener.
  Ch5_1_4Back,
  /// 6.0 channel layout, typically with six audio channels (front left, front right, center, rear left, rear right, and an additional channel for height or rear speakers) without a subwoofer.
  Ch6_0,
  /// 6.0 front channel layout, which is a variation of the 6.0 channel layout with the additional channel for height or rear speakers positioned in front of the listener.
  Ch6_0Front,
  /// 6.1 channel layout, typically with seven audio channels (front left, front right, center, rear left, rear right, an additional channel for height or rear speakers, and a subwoofer).
  Ch6_1,
  /// 6.1 back channel layout, which is a variation of the 6.1 channel layout with the additional channel for height or rear speakers positioned behind the listener.
  Ch6_1Back,
  /// 6.1 front channel layout, which is a variation of the 6.1 channel layout with the additional channel for height or rear speakers positioned in front of the listener.
  Ch6_1Front,
  /// 7.0 channel layout, typically with seven audio channels (front left, front right, center, rear left, rear right, and two additional channels for height or rear speakers) without a subwoofer.
  Ch7_0,
  /// 7.0 front channel layout, which is a variation of the 7.0 channel layout with the two additional channels for height or rear speakers positioned in front of the listener.
  Ch7_0Front,
  /// 7.1 channel layout, typically with eight audio channels (front left, front right, center, rear left, rear right, two additional channels for height or rear speakers, and a subwoofer).
  Ch7_1,
  /// 7.1 wide channel layout, which is a variation of the 7.1 channel layout with the two additional channels for height or rear speakers positioned wider than the standard 7.1 layout.
  Ch7_1Wide,
  /// 7.1 wide back channel layout, which is a variation of the 7.1 wide channel layout with the two additional channels for height or rear speakers positioned behind the listener.
  Ch7_1WideBack,
  /// 7.1 top back channel layout, which is a variation of the 7.1 channel layout with the two additional channels for height or rear speakers positioned above and behind the listener.
  Ch7_1TopBack,
  /// 7.1.2 channel layout, which is a variation of the 7.1 channel layout with two additional channels for height or rear speakers.
  Ch7_1_2,
  /// 7.1.4 back channel layout, which is a variation of the 7.1 channel layout with four additional channels for height or rear speakers positioned behind the listener.
  Ch7_1_4Back,
  /// 7.2.3 channel layout, which is a variation of the 7.1 channel layout with two additional channels for height or rear speakers and three additional channels for height or rear speakers positioned behind the listener.
  Ch7_2_3,
  /// 9.1.4 back channel layout, which is a variation of the 7.1 channel layout with two additional channels for height or rear speakers and four additional channels for height or rear speakers positioned behind the listener.
  Ch9_1_4Back,
  /// 22.2 channel layout, typically with twenty-four audio channels arranged in a 22.2 configuration.
  Ch22_2,
  /// Unknown channel layout kind, represents any channel layout that does not fit into the predefined categories.
  Unknown,
}

impl Default for ChannelLayoutKind {
  #[cfg_attr(not(tarpaulin), inline(always))]
  fn default() -> Self {
    Self::Unknown
  }
}

impl ChannelLayoutKind {
  /// Every variant, once — the roster [`FromStr`] walks and the
  /// `arbitrary` / `quickcheck` generators sample.
  ///
  /// The slugs themselves live only in [`Self::as_str`]; this list walks
  /// that one table, so a name can never be writable in one direction and
  /// unreadable in the other. `channel_layout_kind_roster_is_complete`
  /// pins that a variant reaching [`Self::to_u32`] reaches this list too.
  const ALL: &'static [Self] = &[
    Self::Mono,
    Self::Stereo,
    Self::StereoDownmix,
    Self::Surround,
    Self::Quad,
    Self::Hexagonal,
    Self::Octagonal,
    Self::Hexadecagonal,
    Self::Cube,
    Self::Ch2_1,
    Self::Ch2_1Alt,
    Self::Ch2_2,
    Self::Ch3_1,
    Self::Ch3_1_2,
    Self::Ch4_0,
    Self::Ch4_1,
    Self::Ch5_0,
    Self::Ch5_0Back,
    Self::Ch5_1,
    Self::Ch5_1Back,
    Self::Ch5_1_2Back,
    Self::Ch5_1_4Back,
    Self::Ch6_0,
    Self::Ch6_0Front,
    Self::Ch6_1,
    Self::Ch6_1Back,
    Self::Ch6_1Front,
    Self::Ch7_0,
    Self::Ch7_0Front,
    Self::Ch7_1,
    Self::Ch7_1Wide,
    Self::Ch7_1WideBack,
    Self::Ch7_1TopBack,
    Self::Ch7_1_2,
    Self::Ch7_1_4Back,
    Self::Ch7_2_3,
    Self::Ch9_1_4Back,
    Self::Ch22_2,
    Self::Unknown,
  ];

  /// Canonical lowercase slug — the text form [`Display`](core::fmt::Display)
  /// prints and [`FromStr`] reads back (`"mono"`, `"5.1"`,
  /// `"7.1-wide-back"`).
  ///
  /// Multi-word names are hyphenated rather than spaced: a slug is meant
  /// to survive a CLI argument, a filename and an environment variable
  /// without quoting.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn as_str(&self) -> &'static str {
    match self {
      Self::Mono => "mono",
      Self::Stereo => "stereo",
      Self::StereoDownmix => "stereo-downmix",
      Self::Surround => "surround",
      Self::Quad => "quad",
      Self::Hexagonal => "hexagonal",
      Self::Octagonal => "octagonal",
      Self::Hexadecagonal => "hexadecagonal",
      Self::Cube => "cube",
      Self::Ch2_1 => "2.1",
      Self::Ch2_1Alt => "2.1-alternative",
      Self::Ch2_2 => "2.2",
      Self::Ch3_1 => "3.1",
      Self::Ch3_1_2 => "3.1.2",
      Self::Ch4_0 => "4.0",
      Self::Ch4_1 => "4.1",
      Self::Ch5_0 => "5.0",
      Self::Ch5_0Back => "5.0-back",
      Self::Ch5_1 => "5.1",
      Self::Ch5_1Back => "5.1-back",
      Self::Ch5_1_2Back => "5.1.2-back",
      Self::Ch5_1_4Back => "5.1.4-back",
      Self::Ch6_0 => "6.0",
      Self::Ch6_0Front => "6.0-front",
      Self::Ch6_1 => "6.1",
      Self::Ch6_1Back => "6.1-back",
      Self::Ch6_1Front => "6.1-front",
      Self::Ch7_0 => "7.0",
      Self::Ch7_0Front => "7.0-front",
      Self::Ch7_1 => "7.1",
      Self::Ch7_1Wide => "7.1-wide",
      Self::Ch7_1WideBack => "7.1-wide-back",
      Self::Ch7_1TopBack => "7.1-top-back",
      Self::Ch7_1_2 => "7.1.2",
      Self::Ch7_1_4Back => "7.1.4-back",
      Self::Ch7_2_3 => "7.2.3",
      Self::Ch9_1_4Back => "9.1.4-back",
      Self::Ch22_2 => "22.2",
      Self::Unknown => "unknown",
    }
  }

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

/// The error [`ChannelLayoutKind`]'s [`FromStr`] returns.
///
/// Opaque and sealed: the rejected input is deliberately not retained.
/// This vocabulary is available at the crate's no-`alloc` tier, where
/// there is nowhere to put an owned copy, and the input is
/// attacker-controlled on any deserialization path. `#[non_exhaustive]`
/// keeps the error constructible here only, so it can grow structure
/// later without breaking callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[error("not a channel-layout-kind name")]
#[non_exhaustive]
pub struct ParseChannelLayoutKindError;

impl FromStr for ChannelLayoutKind {
  type Err = ParseChannelLayoutKindError;

  /// Reads the canonical slug [`Self::as_str`] renders — the exact
  /// inverse of [`Display`](core::fmt::Display).
  ///
  /// The comparison is made against the roster's own slugs with
  /// [`str::eq_ignore_ascii_case`], so nothing is allocated and nothing
  /// is folded into a buffer: `"5.1-back"`, `"5.1-Back"` and
  /// `"5.1-BACK"` are one value, and case is the whole of the folding —
  /// no alias, no trimming.
  ///
  /// # Errors
  ///
  /// Returns [`ParseChannelLayoutKindError`] for any input outside this
  /// closed vocabulary, the empty string included.
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::ALL
      .iter()
      .find(|kind| kind.as_str().eq_ignore_ascii_case(s))
      .copied()
      .ok_or(ParseChannelLayoutKindError)
  }
}

/// How the channels in an [`AudioChannelLayout`] are ordered.
///
/// Mirrors FFmpeg's `AVChannelOrder`. Stable wire integers are
/// `repr(u32)` and match the [`Self::as_u32`] / [`Self::from_u32`]
/// mapping; [`Self::as_str`] / [`FromStr`] are the text form.
///
/// **Closed set.** `AVChannelOrder` is itself a closed taxonomy — every
/// layout FFmpeg can describe is unspecified, native, custom or
/// ambisonic — so there is no vendor space for an escape arm to
/// preserve. A raw discriminant outside the four is a corrupt read, not
/// a value, and decodes to [`Self::Unspecified`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Display)]
#[display("{}", self.as_str())]
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
  /// Every variant, once — the roster [`FromStr`] walks and the
  /// `arbitrary` / `quickcheck` generators sample. The slugs live only
  /// in [`Self::as_str`]; `order_roster_is_complete` pins that a variant
  /// reaching [`Self::from_u32`] reaches this list too.
  const ALL: &'static [Self] = &[
    Self::Unspecified,
    Self::Native,
    Self::Custom,
    Self::Ambisonic,
  ];

  /// Canonical lowercase slug — the text form
  /// [`Display`](core::fmt::Display) prints and [`FromStr`] reads back.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn as_str(&self) -> &'static str {
    match self {
      Self::Unspecified => "unspecified",
      Self::Native => "native",
      Self::Custom => "custom",
      Self::Ambisonic => "ambisonic",
    }
  }

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

/// The error [`AudioChannelOrderKind`]'s [`FromStr`] returns.
///
/// Its own type rather than a shared one: an input that names no channel
/// *order* and an input that names no channel *layout* are different
/// failures, and the type is what says which. Opaque and sealed for the
/// same reasons as [`ParseChannelLayoutKindError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
#[error("not an audio-channel-order name")]
#[non_exhaustive]
pub struct ParseAudioChannelOrderKindError;

impl FromStr for AudioChannelOrderKind {
  type Err = ParseAudioChannelOrderKindError;

  /// Reads the canonical slug [`Self::as_str`] renders — the exact
  /// inverse of [`Display`](core::fmt::Display), folding ASCII case and
  /// nothing else (`"native"`, `"Native"`, `"NATIVE"`).
  ///
  /// # Errors
  ///
  /// Returns [`ParseAudioChannelOrderKindError`] for any input outside
  /// this closed vocabulary, the empty string included. Note that
  /// [`Self::from_u32`] absorbs an unrecognised *code* into
  /// [`Self::Unspecified`] while this door rejects an unrecognised
  /// *name*: a corrupt discriminant read out of FFmpeg memory has no
  /// spelling to fall back on, a misspelled configuration value does.
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::ALL
      .iter()
      .find(|order| order.as_str().eq_ignore_ascii_case(s))
      .copied()
      .ok_or(ParseAudioChannelOrderKindError)
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
    for &kind in ChannelLayoutKind::ALL {
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
      "7.1-wide-back"
    );
    assert_eq!(format!("{}", ChannelLayoutKind::Unknown), "unknown");
    for &kind in ChannelLayoutKind::ALL {
      assert_eq!(
        format!("{kind}"),
        kind.as_str(),
        "display drifted from as_str"
      );
    }
  }

  #[test]
  fn channel_layout_kind_is_variant() {
    assert!(ChannelLayoutKind::Mono.is_mono());
    assert!(!ChannelLayoutKind::Stereo.is_mono());
    assert!(ChannelLayoutKind::Ch5_1.is_ch_5_1());
    assert!(ChannelLayoutKind::Unknown.is_unknown());
  }

  #[test]
  fn channel_layout_kind_slugs() {
    // `FromStr` reads the very table `as_str` writes, so a typo there
    // round-trips happily. This is the independent copy that catches it.
    let table = [
      (ChannelLayoutKind::Mono, "mono"),
      (ChannelLayoutKind::Stereo, "stereo"),
      (ChannelLayoutKind::StereoDownmix, "stereo-downmix"),
      (ChannelLayoutKind::Surround, "surround"),
      (ChannelLayoutKind::Quad, "quad"),
      (ChannelLayoutKind::Hexagonal, "hexagonal"),
      (ChannelLayoutKind::Octagonal, "octagonal"),
      (ChannelLayoutKind::Hexadecagonal, "hexadecagonal"),
      (ChannelLayoutKind::Cube, "cube"),
      (ChannelLayoutKind::Ch2_1, "2.1"),
      (ChannelLayoutKind::Ch2_1Alt, "2.1-alternative"),
      (ChannelLayoutKind::Ch2_2, "2.2"),
      (ChannelLayoutKind::Ch3_1, "3.1"),
      (ChannelLayoutKind::Ch3_1_2, "3.1.2"),
      (ChannelLayoutKind::Ch4_0, "4.0"),
      (ChannelLayoutKind::Ch4_1, "4.1"),
      (ChannelLayoutKind::Ch5_0, "5.0"),
      (ChannelLayoutKind::Ch5_0Back, "5.0-back"),
      (ChannelLayoutKind::Ch5_1, "5.1"),
      (ChannelLayoutKind::Ch5_1Back, "5.1-back"),
      (ChannelLayoutKind::Ch5_1_2Back, "5.1.2-back"),
      (ChannelLayoutKind::Ch5_1_4Back, "5.1.4-back"),
      (ChannelLayoutKind::Ch6_0, "6.0"),
      (ChannelLayoutKind::Ch6_0Front, "6.0-front"),
      (ChannelLayoutKind::Ch6_1, "6.1"),
      (ChannelLayoutKind::Ch6_1Back, "6.1-back"),
      (ChannelLayoutKind::Ch6_1Front, "6.1-front"),
      (ChannelLayoutKind::Ch7_0, "7.0"),
      (ChannelLayoutKind::Ch7_0Front, "7.0-front"),
      (ChannelLayoutKind::Ch7_1, "7.1"),
      (ChannelLayoutKind::Ch7_1Wide, "7.1-wide"),
      (ChannelLayoutKind::Ch7_1WideBack, "7.1-wide-back"),
      (ChannelLayoutKind::Ch7_1TopBack, "7.1-top-back"),
      (ChannelLayoutKind::Ch7_1_2, "7.1.2"),
      (ChannelLayoutKind::Ch7_1_4Back, "7.1.4-back"),
      (ChannelLayoutKind::Ch7_2_3, "7.2.3"),
      (ChannelLayoutKind::Ch9_1_4Back, "9.1.4-back"),
      (ChannelLayoutKind::Ch22_2, "22.2"),
      (ChannelLayoutKind::Unknown, "unknown"),
    ];
    assert_eq!(table.len(), ChannelLayoutKind::ALL.len());
    for (kind, slug) in table {
      assert_eq!(kind.as_str(), slug, "slug mismatch for {kind:?}");
    }
  }

  #[test]
  fn channel_layout_kind_round_trips_through_its_slug() {
    for &kind in ChannelLayoutKind::ALL {
      assert_eq!(
        kind.as_str().parse::<ChannelLayoutKind>(),
        Ok(kind),
        "slug round-trip failed for {kind:?}"
      );
    }
  }

  #[test]
  fn channel_layout_kind_folds_ascii_case() {
    assert_eq!(
      "MONO".parse::<ChannelLayoutKind>(),
      Ok(ChannelLayoutKind::Mono)
    );
    assert_eq!(
      "5.1-Back".parse::<ChannelLayoutKind>(),
      Ok(ChannelLayoutKind::Ch5_1Back)
    );
    assert_eq!(
      "Stereo-DOWNMIX".parse::<ChannelLayoutKind>(),
      Ok(ChannelLayoutKind::StereoDownmix)
    );
  }

  #[test]
  fn channel_layout_kind_rejects_what_it_cannot_name() {
    assert!("".parse::<ChannelLayoutKind>().is_err());
    assert!("atmos".parse::<ChannelLayoutKind>().is_err());
    // Case is the whole of the folding: neither whitespace nor a second
    // spelling of a name is an alias for it.
    assert!("5.1-back ".parse::<ChannelLayoutKind>().is_err());
    assert!("5.1 back".parse::<ChannelLayoutKind>().is_err());
  }

  #[test]
  fn channel_layout_kind_slugs_do_not_collide_under_ascii_folding() {
    // The door folds case, so two slugs equal under folding would make it
    // ambiguous and the earlier roster entry would win in silence. This
    // also pins that no variant is listed twice.
    for (i, a) in ChannelLayoutKind::ALL.iter().enumerate() {
      for b in &ChannelLayoutKind::ALL[i + 1..] {
        assert!(
          !a.as_str().eq_ignore_ascii_case(b.as_str()),
          "{a:?} and {b:?} answer to the same name"
        );
      }
    }
  }

  #[test]
  fn channel_layout_kind_roster_is_complete() {
    // A code is live when it survives the `u32` round trip; every live
    // code's variant has to be on the roster the text door walks, or the
    // value would be writable and unreadable. The scan runs past the
    // highest live code so a variant added tomorrow is covered too.
    let mut live = 0;
    for code in 0..=64u32 {
      let kind = ChannelLayoutKind::from_u32(code);
      if kind.to_u32() == code {
        live += 1;
        assert!(
          ChannelLayoutKind::ALL.contains(&kind),
          "{kind:?} (code {code}) is missing from the roster"
        );
      }
    }
    assert_eq!(
      ChannelLayoutKind::ALL.len(),
      live,
      "the roster holds an entry no wire code decodes to"
    );
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
    for &o in AudioChannelOrderKind::ALL {
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

  #[test]
  fn order_slugs() {
    let table = [
      (AudioChannelOrderKind::Unspecified, "unspecified"),
      (AudioChannelOrderKind::Native, "native"),
      (AudioChannelOrderKind::Custom, "custom"),
      (AudioChannelOrderKind::Ambisonic, "ambisonic"),
    ];
    assert_eq!(table.len(), AudioChannelOrderKind::ALL.len());
    for (order, slug) in table {
      assert_eq!(order.as_str(), slug, "slug mismatch for {order:?}");
    }
  }

  #[cfg(any(feature = "alloc", feature = "std"))]
  #[test]
  fn order_display_is_the_slug() {
    assert_eq!(format!("{}", AudioChannelOrderKind::Ambisonic), "ambisonic");
    for &order in AudioChannelOrderKind::ALL {
      assert_eq!(
        format!("{order}"),
        order.as_str(),
        "display drifted from as_str"
      );
    }
  }

  #[test]
  fn order_round_trips_through_its_slug() {
    for &order in AudioChannelOrderKind::ALL {
      assert_eq!(
        order.as_str().parse::<AudioChannelOrderKind>(),
        Ok(order),
        "slug round-trip failed for {order:?}"
      );
    }
  }

  #[test]
  fn order_folds_ascii_case() {
    assert_eq!(
      "Native".parse::<AudioChannelOrderKind>(),
      Ok(AudioChannelOrderKind::Native)
    );
    assert_eq!(
      "AMBISONIC".parse::<AudioChannelOrderKind>(),
      Ok(AudioChannelOrderKind::Ambisonic)
    );
  }

  #[test]
  fn order_rejects_what_it_cannot_name() {
    // The numeric door absorbs an unknown code into `Unspecified`; the
    // text door refuses an unknown name rather than inventing one.
    assert!("".parse::<AudioChannelOrderKind>().is_err());
    assert!("interleaved".parse::<AudioChannelOrderKind>().is_err());
    assert_eq!(
      AudioChannelOrderKind::from_u32(42),
      AudioChannelOrderKind::Unspecified
    );
  }

  #[test]
  fn order_slugs_do_not_collide_under_ascii_folding() {
    for (i, a) in AudioChannelOrderKind::ALL.iter().enumerate() {
      for b in &AudioChannelOrderKind::ALL[i + 1..] {
        assert!(
          !a.as_str().eq_ignore_ascii_case(b.as_str()),
          "{a:?} and {b:?} answer to the same name"
        );
      }
    }
  }

  #[test]
  fn order_roster_is_complete() {
    let mut live = 0;
    for code in 0..=16u32 {
      let order = AudioChannelOrderKind::from_u32(code);
      if order.as_u32() == code {
        live += 1;
        assert!(
          AudioChannelOrderKind::ALL.contains(&order),
          "{order:?} (code {code}) is missing from the roster"
        );
      }
    }
    assert_eq!(
      AudioChannelOrderKind::ALL.len(),
      live,
      "the roster holds an entry no wire code decodes to"
    );
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

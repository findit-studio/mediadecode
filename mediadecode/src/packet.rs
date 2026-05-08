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

use crate::{Timestamp, adapter::VideoAdapter};

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
  pts: Option<Timestamp>,
  dts: Option<Timestamp>,
  duration: Option<Timestamp>,
  flags: PacketFlags,
  data: B,
  extra: A::PacketExtra,
}

impl<A: VideoAdapter, B: AsRef<[u8]>> VideoPacket<A, B> {
  /// Constructs a `VideoPacket` from `data` and `extra`. All
  /// timestamps default to `None` and flags to empty.
  #[cfg_attr(not(tarpaulin), inline(always))]
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
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pts(&self) -> Option<Timestamp> {
    self.pts
  }
  /// Returns the decompression timestamp.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn dts(&self) -> Option<Timestamp> {
    self.dts
  }
  /// Returns the packet duration.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Returns the packet flags.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn flags(&self) -> PacketFlags {
    self.flags
  }
  /// Returns the compressed data buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data(&self) -> &B {
    &self.data
  }
  /// Returns the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &A::PacketExtra {
    &self.extra
  }
  /// Returns a mutable reference to the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extra_mut(&mut self) -> &mut A::PacketExtra {
    &mut self.extra
  }
  /// Consumes the packet and returns the buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_data(self) -> B {
    self.data
  }
  /// Consumes the packet and returns `(buffer, extras)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (B, A::PacketExtra) {
    (self.data, self.extra)
  }

  /// Sets the PTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the DTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_dts(mut self, v: Option<Timestamp>) -> Self {
    self.dts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }
  /// Sets the flags (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_flags(mut self, v: PacketFlags) -> Self {
    self.flags = v;
    self
  }

  /// Sets the PTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.pts = v;
    self
  }
  /// Sets the DTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_dts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.dts = v;
    self
  }
  /// Sets the duration in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.duration = v;
    self
  }
  /// Sets the flags in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_flags(&mut self, v: PacketFlags) -> &mut Self {
    self.flags = v;
    self
  }
}

use crate::adapter::AudioAdapter;

/// A compressed audio packet.
pub struct AudioPacket<A: AudioAdapter, B: AsRef<[u8]>> {
  pts: Option<Timestamp>,
  dts: Option<Timestamp>,
  duration: Option<Timestamp>,
  flags: PacketFlags,
  data: B,
  extra: A::PacketExtra,
}

impl<A: AudioAdapter, B: AsRef<[u8]>> AudioPacket<A, B> {
  /// Constructs an `AudioPacket` from `data` and `extra`.
  #[cfg_attr(not(tarpaulin), inline(always))]
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
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pts(&self) -> Option<Timestamp> {
    self.pts
  }
  /// Returns the decompression timestamp.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn dts(&self) -> Option<Timestamp> {
    self.dts
  }
  /// Returns the duration.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Returns the flags.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn flags(&self) -> PacketFlags {
    self.flags
  }
  /// Returns the compressed audio data.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data(&self) -> &B {
    &self.data
  }
  /// Returns the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &A::PacketExtra {
    &self.extra
  }
  /// Returns a mutable reference to the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extra_mut(&mut self) -> &mut A::PacketExtra {
    &mut self.extra
  }
  /// Consumes the packet and returns the buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_data(self) -> B {
    self.data
  }
  /// Consumes the packet and returns `(buffer, extras)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (B, A::PacketExtra) {
    (self.data, self.extra)
  }

  /// Sets the PTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the DTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_dts(mut self, v: Option<Timestamp>) -> Self {
    self.dts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }
  /// Sets the flags (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_flags(mut self, v: PacketFlags) -> Self {
    self.flags = v;
    self
  }

  /// Sets the PTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.pts = v;
    self
  }
  /// Sets the DTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_dts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.dts = v;
    self
  }
  /// Sets the duration in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.duration = v;
    self
  }
  /// Sets the flags in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_flags(&mut self, v: PacketFlags) -> &mut Self {
    self.flags = v;
    self
  }
}

use crate::adapter::SubtitleAdapter;

/// A compressed subtitle packet.
pub struct SubtitlePacket<A: SubtitleAdapter, B: AsRef<[u8]>> {
  pts: Option<Timestamp>,
  duration: Option<Timestamp>,
  flags: PacketFlags,
  data: B,
  extra: A::PacketExtra,
}

impl<A: SubtitleAdapter, B: AsRef<[u8]>> SubtitlePacket<A, B> {
  /// Constructs a `SubtitlePacket` from `data` and `extra`.
  #[cfg_attr(not(tarpaulin), inline(always))]
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
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pts(&self) -> Option<Timestamp> {
    self.pts
  }
  /// Returns the duration.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Returns the flags.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn flags(&self) -> PacketFlags {
    self.flags
  }
  /// Returns the compressed subtitle data.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data(&self) -> &B {
    &self.data
  }
  /// Returns the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &A::PacketExtra {
    &self.extra
  }
  /// Returns a mutable reference to the backend extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extra_mut(&mut self) -> &mut A::PacketExtra {
    &mut self.extra
  }
  /// Consumes the packet and returns the buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_data(self) -> B {
    self.data
  }
  /// Consumes the packet and returns `(buffer, extras)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (B, A::PacketExtra) {
    (self.data, self.extra)
  }

  /// Sets the PTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }
  /// Sets the flags (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_flags(mut self, v: PacketFlags) -> Self {
    self.flags = v;
    self
  }

  /// Sets the PTS in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_pts(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.pts = v;
    self
  }
  /// Sets the duration in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.duration = v;
    self
  }
  /// Sets the flags in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_flags(&mut self, v: PacketFlags) -> &mut Self {
    self.flags = v;
    self
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
    let p: AudioPacket<ALoop, &[u8]> = AudioPacket::new(data, ()).with_flags(PacketFlags::KEY);
    assert_eq!(*p.data(), data);
    assert!(p.flags().contains(PacketFlags::KEY));
    let (recovered, _) = p.into_parts();
    assert_eq!(recovered, data);
  }

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
}

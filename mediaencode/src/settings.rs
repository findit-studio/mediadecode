//! What an encoder is opened with.
//!
//! [`EncoderSettings`] holds what every encoder takes — the codec, how it
//! spends bits, the profile and the level — and, as `params`, what only
//! one kind of encoder takes: [`VideoParams`] (the frames' shape, the GOP,
//! the color description and the HDR metadata written into the stream)
//! or [`AudioParams`] (sample rate, sample format, channel layout).
//! [`VideoEncoderSettings`] and [`AudioEncoderSettings`] name the two
//! over an adapter's vocabulary.
//!
//! Every setting is a request. What an encoder cannot honour is the
//! backend's to refuse when it opens the session; nothing here is
//! checked against a particular codec.

use core::num::{NonZeroU32, NonZeroU64};

use mediaspine::{
  Timebase,
  adapter::{AudioAdapter, VideoAdapter},
  color::ColorInfo,
  frame::Dimensions,
  hdr::{ContentLightLevel, MasteringDisplay},
};

/// How an encoder spends bits.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RateControl {
  /// The encoder's own default.
  #[default]
  EncoderDefault,
  /// An average bitrate, in bits per second.
  Bitrate(NonZeroU64),
  /// An average bitrate held under a ceiling, which the encoder enforces
  /// over a decoder buffer of `buffer` bits (the VBV / HRD model).
  Constrained {
    /// The average, in bits per second.
    bitrate: NonZeroU64,
    /// The ceiling, in bits per second.
    max_bitrate: NonZeroU64,
    /// The decoder buffer the ceiling is enforced over, in bits.
    buffer: NonZeroU64,
  },
  /// A constant quality, on the encoder's own scale — a constant rate
  /// factor for `libx264` and `libx265`, where lower is better; a
  /// quality percentage for VideoToolbox, where higher is better. Each
  /// backend states the scale of the encoders it opens.
  Quality(u32),
}

/// A codec profile, as the backend numbers it — each backend documents
/// its numbering.
///
/// The FFmpeg backend's is `AVCodecContext.profile`: the codec's own
/// number (an H.264 stream's `profile_idc`, 100 for High; an HEVC
/// stream's `general_profile_idc`, 2 for Main 10; AAC's audio object
/// type minus one, 1 for LC), with FFmpeg's constraint flags above bit 8
/// — H.264 Constrained Baseline is 66 | 512 = 578, High 10 Intra is
/// 110 | 2048. A width narrower than the backend's numbering would lose
/// the web's commonest H.264 profile, so the number is a `u32`.
///
/// Where settings carry no profile (`None`), the encoder chooses.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Profile(u32);

impl Profile {
  /// Constructs a `Profile` from the backend's number.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(number: u32) -> Self {
    Self(number)
  }
  /// Returns the backend's number.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn get(self) -> u32 {
    self.0
  }
}

/// A codec level, as the backend numbers it — each backend documents
/// its numbering.
///
/// The FFmpeg backend's is `AVCodecContext.level`: an H.264 stream's
/// `level_idc` (41 for level 4.1), an HEVC stream's `general_level_idc`
/// (153 for level 5.1).
///
/// Where settings carry no level (`None`), the encoder chooses.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Level(u32);

impl Level {
  /// Constructs a `Level` from the backend's number.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(number: u32) -> Self {
    Self(number)
  }
  /// Returns the backend's number.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn get(self) -> u32 {
    self.0
  }
}

/// The structure of a video stream's groups of pictures.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Gop {
  keyframe_interval: Option<NonZeroU32>,
  max_b_frames: Option<u32>,
}

impl Gop {
  /// The encoder's own GOP structure.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      keyframe_interval: None,
      max_b_frames: None,
    }
  }

  /// The most frames between two keyframes, or `None` for the
  /// encoder's default.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn keyframe_interval(&self) -> Option<NonZeroU32> {
    self.keyframe_interval
  }
  /// The most consecutive B-frames, or `None` for the encoder's default.
  /// `Some(0)` asks for none: packets come out in presentation order.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_b_frames(&self) -> Option<u32> {
    self.max_b_frames
  }

  /// Sets the keyframe interval (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_keyframe_interval(mut self, value: Option<NonZeroU32>) -> Self {
    self.keyframe_interval = value;
    self
  }
  /// Sets the B-frame limit (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_b_frames(mut self, value: Option<u32>) -> Self {
    self.max_b_frames = value;
    self
  }

  /// Sets the keyframe interval in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_keyframe_interval(&mut self, value: Option<NonZeroU32>) -> &mut Self {
    self.keyframe_interval = value;
    self
  }
  /// Sets the B-frame limit in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_b_frames(&mut self, value: Option<u32>) -> &mut Self {
    self.max_b_frames = value;
    self
  }
}

/// What a video encoder takes in, and what it writes into the stream
/// about the pictures besides the pictures.
///
/// The color description is written as the stream's own (it is passed
/// through, not converted), and the HDR metadata travels as the stream's
/// side data where the codec carries it.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoParams<P> {
  dimensions: Dimensions,
  pixel_format: P,
  timebase: Timebase,
  frame_rate: Option<Timebase>,
  gop: Gop,
  color: ColorInfo,
  mastering_display: Option<MasteringDisplay>,
  content_light_level: Option<ContentLightLevel>,
}

impl<P> VideoParams<P> {
  /// Constructs `VideoParams` for frames of `dimensions` in
  /// `pixel_format`, stamped in `timebase`. The frame rate is unstated,
  /// the GOP the encoder's, the color unspecified, and there is no HDR
  /// metadata.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn new(dimensions: Dimensions, pixel_format: P, timebase: Timebase) -> Self {
    Self {
      dimensions,
      pixel_format,
      timebase,
      frame_rate: None,
      gop: Gop::new(),
      color: ColorInfo::default(),
      mastering_display: None,
      content_light_level: None,
    }
  }

  /// Returns the frames' dimensions.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn dimensions(&self) -> Dimensions {
    self.dimensions
  }
  /// Returns the frames' pixel format, in the backend's vocabulary.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pixel_format(&self) -> &P {
    &self.pixel_format
  }
  /// Returns the timebase the frames' timestamps are expressed in.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn timebase(&self) -> Timebase {
    self.timebase
  }
  /// Returns the frame rate, as a rate-shaped [`Timebase`] (`30000/1001`
  /// for 29.97 fps), or `None` where it is not stated.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn frame_rate(&self) -> Option<Timebase> {
    self.frame_rate
  }
  /// Returns the GOP structure.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn gop(&self) -> Gop {
    self.gop
  }
  /// Returns the color description written into the stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn color(&self) -> &ColorInfo {
    &self.color
  }
  /// Returns the mastering display metadata, if any.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn mastering_display(&self) -> Option<MasteringDisplay> {
    self.mastering_display
  }
  /// Returns the content light level, if any.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn content_light_level(&self) -> Option<ContentLightLevel> {
    self.content_light_level
  }

  /// Sets the frame rate (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_frame_rate(mut self, value: Option<Timebase>) -> Self {
    self.frame_rate = value;
    self
  }
  /// Sets the GOP structure (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_gop(mut self, value: Gop) -> Self {
    self.gop = value;
    self
  }
  /// Sets the color description (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_color(mut self, value: ColorInfo) -> Self {
    self.color = value;
    self
  }
  /// Sets the mastering display metadata (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_mastering_display(mut self, value: Option<MasteringDisplay>) -> Self {
    self.mastering_display = value;
    self
  }
  /// Sets the content light level (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_content_light_level(mut self, value: Option<ContentLightLevel>) -> Self {
    self.content_light_level = value;
    self
  }

  /// Sets the frame rate in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_frame_rate(&mut self, value: Option<Timebase>) -> &mut Self {
    self.frame_rate = value;
    self
  }
  /// Sets the GOP structure in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_gop(&mut self, value: Gop) -> &mut Self {
    self.gop = value;
    self
  }
  /// Sets the color description in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_color(&mut self, value: ColorInfo) -> &mut Self {
    self.color = value;
    self
  }
  /// Sets the mastering display metadata in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_mastering_display(&mut self, value: Option<MasteringDisplay>) -> &mut Self {
    self.mastering_display = value;
    self
  }
  /// Sets the content light level in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_content_light_level(&mut self, value: Option<ContentLightLevel>) -> &mut Self {
    self.content_light_level = value;
    self
  }
}

/// What an audio encoder takes in.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AudioParams<S, L> {
  sample_rate: u32,
  sample_format: S,
  channel_layout: L,
}

impl<S, L> AudioParams<S, L> {
  /// Constructs `AudioParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(sample_rate: u32, sample_format: S, channel_layout: L) -> Self {
    Self {
      sample_rate,
      sample_format,
      channel_layout,
    }
  }

  /// Returns the sample rate, in hertz.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn sample_rate(&self) -> u32 {
    self.sample_rate
  }
  /// Returns the sample format, in the backend's vocabulary.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn sample_format(&self) -> &S {
    &self.sample_format
  }
  /// Returns the channel layout, in the backend's vocabulary.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn channel_layout(&self) -> &L {
    &self.channel_layout
  }
}

/// What an encoder is opened with: the codec, how it spends bits, its
/// profile and level, and the kind's own `params`.
#[derive(Clone, Debug, PartialEq)]
pub struct EncoderSettings<C, K> {
  codec: C,
  rate_control: RateControl,
  profile: Option<Profile>,
  level: Option<Level>,
  params: K,
}

impl<C, K> EncoderSettings<C, K> {
  /// Constructs settings for `codec` with the kind's `params`; rate
  /// control, profile and level are the encoder's defaults.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(codec: C, params: K) -> Self {
    Self {
      codec,
      rate_control: RateControl::EncoderDefault,
      profile: None,
      level: None,
      params,
    }
  }

  /// Returns the codec, in the backend's vocabulary.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> &C {
    &self.codec
  }
  /// Returns how the encoder spends bits.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn rate_control(&self) -> RateControl {
    self.rate_control
  }
  /// Returns the requested profile, or `None` for the encoder's default.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn profile(&self) -> Option<Profile> {
    self.profile
  }
  /// Returns the requested level, or `None` for the encoder's default.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn level(&self) -> Option<Level> {
    self.level
  }
  /// Returns the kind's own parameters.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn params(&self) -> &K {
    &self.params
  }
  /// Returns the kind's own parameters mutably.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn params_mut(&mut self) -> &mut K {
    &mut self.params
  }

  /// Sets the rate control (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_rate_control(mut self, value: RateControl) -> Self {
    self.rate_control = value;
    self
  }
  /// Sets the profile (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_profile(mut self, value: Option<Profile>) -> Self {
    self.profile = value;
    self
  }
  /// Sets the level (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_level(mut self, value: Option<Level>) -> Self {
    self.level = value;
    self
  }

  /// Sets the rate control in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_rate_control(&mut self, value: RateControl) -> &mut Self {
    self.rate_control = value;
    self
  }
  /// Sets the profile in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_profile(&mut self, value: Option<Profile>) -> &mut Self {
    self.profile = value;
    self
  }
  /// Sets the level in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_level(&mut self, value: Option<Level>) -> &mut Self {
    self.level = value;
    self
  }
}

/// The settings a video encoder over adapter `A` is opened with.
pub type VideoEncoderSettings<A> =
  EncoderSettings<<A as VideoAdapter>::CodecId, VideoParams<<A as VideoAdapter>::PixelFormat>>;

/// The settings an audio encoder over adapter `A` is opened with.
pub type AudioEncoderSettings<A> = EncoderSettings<
  <A as AudioAdapter>::CodecId,
  AudioParams<<A as AudioAdapter>::SampleFormat, <A as AudioAdapter>::ChannelLayout>,
>;

#[cfg(test)]
mod tests {
  use core::num::NonZeroI32;

  use super::*;

  struct VLoop;
  impl VideoAdapter for VLoop {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  fn ms_tb() -> Timebase {
    Timebase::new(1, NonZeroI32::new(1000).expect("non-zero"))
  }

  /// What is not asked for is the encoder's: a fresh setting names no
  /// rate, profile, level, frame rate, GOP shape or HDR metadata.
  #[test]
  fn an_unstated_setting_is_the_encoders_default() {
    let settings: VideoEncoderSettings<VLoop> = EncoderSettings::new(
      27,
      VideoParams::new(Dimensions::new(1920, 1080), 0, ms_tb()),
    );
    assert_eq!(settings.rate_control(), RateControl::EncoderDefault);
    assert_eq!((settings.profile(), settings.level()), (None, None));
    let params = settings.params();
    assert_eq!(params.frame_rate(), None);
    assert_eq!(params.gop(), Gop::new());
    assert_eq!(params.color(), &ColorInfo::default());
    assert_eq!(params.mastering_display(), None);
    assert_eq!(params.content_light_level(), None);
  }

  #[test]
  fn the_builders_set_what_they_name() {
    let rate = RateControl::Constrained {
      bitrate: NonZeroU64::new(8_000_000).expect("non-zero"),
      max_bitrate: NonZeroU64::new(12_000_000).expect("non-zero"),
      buffer: NonZeroU64::new(16_000_000).expect("non-zero"),
    };
    let gop = Gop::new()
      .with_keyframe_interval(NonZeroU32::new(48))
      .with_max_b_frames(Some(0));
    let mut settings: VideoEncoderSettings<VLoop> =
      EncoderSettings::new(27, VideoParams::new(Dimensions::new(1280, 720), 0, ms_tb()))
        .with_rate_control(rate)
        .with_profile(Some(Profile::new(100)))
        .with_level(Some(Level::new(41)));
    settings
      .params_mut()
      .set_gop(gop)
      .set_content_light_level(Some(ContentLightLevel::new(1000, 400)));
    assert_eq!(settings.rate_control(), rate);
    assert_eq!(settings.profile().map(Profile::get), Some(100));
    assert_eq!(settings.level().map(Level::get), Some(41));
    assert_eq!(
      settings.params().gop().keyframe_interval(),
      NonZeroU32::new(48)
    );
    assert_eq!(settings.params().gop().max_b_frames(), Some(0));
    assert_eq!(
      settings.params().content_light_level(),
      Some(ContentLightLevel::new(1000, 400))
    );

    let audio: AudioParams<u32, u32> = AudioParams::new(48_000, 8, 3);
    assert_eq!(
      (
        audio.sample_rate(),
        *audio.sample_format(),
        *audio.channel_layout()
      ),
      (48_000, 8, 3)
    );
  }

  /// **A profile number carries the backend's whole numbering.** FFmpeg
  /// puts its constraint flags above bit 8, so the web's commonest H.264
  /// profile — Constrained Baseline — is 578, not 66, and High 10 Intra
  /// is 2158: both round-trip unchanged.
  #[test]
  fn a_profile_carries_ffmpegs_constraint_flags() {
    const CONSTRAINED: u32 = 1 << 9;
    const INTRA: u32 = 1 << 11;
    let constrained_baseline = Profile::new(66 | CONSTRAINED);
    let high_10_intra = Profile::new(110 | INTRA);
    assert_eq!(constrained_baseline.get(), 578);
    assert_eq!(high_10_intra.get(), 2158);
    let settings: VideoEncoderSettings<VLoop> =
      EncoderSettings::new(27, VideoParams::new(Dimensions::new(640, 360), 0, ms_tb()))
        .with_profile(Some(constrained_baseline));
    assert_eq!(settings.profile().map(Profile::get), Some(578));
  }
}

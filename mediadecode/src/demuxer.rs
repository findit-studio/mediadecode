//! The demux tier — the track table, the five-arm packet envelope, and
//! the [`Demuxer`] session face.
//!
//! A container file is a bundle of *tracks*; reading it produces a
//! single interleaved stream of *packets*, each belonging to one track.
//! This module names both halves: [`TrackInfo`] describes a track,
//! [`DemuxedPacket`] delivers one packet with its track coordinate
//! attached, and [`Demuxer`] is the pull session that hands them out.
//!
//! # The session is pull-style
//!
//! The caller owns the loop. [`Demuxer::next_packet`] returns the next
//! packet in interleaved file order and `Ok(None)` at end of file. One
//! packet is in hand at a time, whatever the file's length, and the
//! caller pulls only when it is ready to consume — backpressure with no
//! machinery. This is the same rhythm the decoder faces already keep
//! (the caller schedules; see [`crate::decoder`]).
//!
//! # Construction is not on the trait
//!
//! [`Demuxer`] covers the **opened session** only: [`tracks`],
//! [`next_packet`], [`seek`]. Opening is each backend's own business
//! and each backend's is different — FFmpeg opens from a path or from a
//! `Read + Seek` reader, a WebAssembly container parser opens from a
//! byte slice. Putting a constructor on the trait would force one of
//! those spellings onto all of them, which is exactly what the decoder
//! traits already decline to do.
//!
//! # Not every backend is a demuxer
//!
//! R3D and BRAW are clip-oriented SDKs: they expose frames by index,
//! never packets, so there is nothing for them to demux. They
//! deliberately do **not** implement this trait and join a pipeline one
//! tier up, through [`crate::decoder::VideoFrameSource`] and
//! [`crate::decoder::AudioFrameSource`]. A graph that wants both shapes
//! unifies them at the *frame* tier, not here.
//!
//! [`tracks`]: Demuxer::tracks
//! [`next_packet`]: Demuxer::next_packet
//! [`seek`]: Demuxer::seek

use core::fmt::{self, Debug};

use crate::{
  Timebase, Timestamp,
  adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
  packet::{AudioPacket, PacketFlags, SubtitlePacket, VideoPacket},
};

/// A track's position in the table [`Demuxer::tracks`] returns.
///
/// `TrackIndex(i)` **is** the index of `tracks()[i]` — the coordinate
/// and the table position are the same number by contract, so a
/// consumer that has a packet's track can look up its description
/// without a side map. Backends whose native identifiers are sparse or
/// unordered are responsible for the translation.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackIndex(usize);

impl TrackIndex {
  /// Constructs a `TrackIndex` from a position in the track table.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(index: usize) -> Self {
    Self(index)
  }

  /// Returns the position in the track table.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn get(self) -> usize {
    self.0
  }
}

/// What a track carries.
///
/// A closed roster: these six are every kind a container can present,
/// and [`Unknown`](Self::Unknown) is the honest answer for a track the
/// backend cannot classify rather than an escape hatch for new kinds.
/// Minted here rather than borrowed: `mediaframe` has a track
/// *disposition* vocabulary but no kind vocabulary, and the dependency
/// direction forbids reaching the other way.
///
/// **Cover art is [`Attachment`](Self::Attachment), not
/// [`Video`](Self::Video).** A still image stored in a container's
/// video-shaped slot is an attachment by every property that matters —
/// one sample, no timeline, no motion — so the `Video` arm carries true
/// motion video and nothing else. See [`DemuxedPacket`] for the
/// delivery contract that follows from this.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrackKind {
  /// Motion video.
  Video,
  /// Audio.
  Audio,
  /// Subtitles / captions, text or bitmap.
  Subtitle,
  /// Timed opaque data — timecode, KLV, timed ID3.
  Data,
  /// A file carried inside the container: a font, cover art.
  Attachment,
  /// The backend could not classify this track.
  #[default]
  Unknown,
}

/// Backend vocabulary for the **demux tier**.
///
/// Bundles the three decoding families ([`VideoAdapter`],
/// [`AudioAdapter`], [`SubtitleAdapter`]) that already exist and adds
/// the seats only a demuxer needs: extras for the two packet kinds that
/// have no decoder face, extras for a track-table row, and the text
/// carrier for a track's identity metadata.
///
/// The three families are bound to share this adapter's
/// [`CodecId`](Self::CodecId). A demuxer reads one container, and a
/// container's track table has one codec-identifier column — the same
/// namespace names its video, its audio, its subtitles, and its cover
/// art. Keeping them one type is what lets a consumer take the codec
/// identifier off an [`Attachment`](TrackKind::Attachment) track
/// carrying cover art and hand it to a video decoder.
pub trait DemuxAdapter {
  /// Codec identifier for every track in the table, whatever its kind
  /// (e.g. a newtype around FFmpeg's `AVCodecID`, a WebCodecs codec
  /// string, a container fourcc).
  type CodecId: Copy + Eq + Debug;

  /// Vocabulary for the container's video tracks.
  type Video: VideoAdapter<CodecId = Self::CodecId>;
  /// Vocabulary for the container's audio tracks.
  type Audio: AudioAdapter<CodecId = Self::CodecId>;
  /// Vocabulary for the container's subtitle tracks.
  type Subtitle: SubtitleAdapter<CodecId = Self::CodecId>;

  /// Backend-specific extras carried on every [`DataPacket`].
  type DataExtra;
  /// Backend-specific extras carried on every [`AttachmentPacket`].
  type AttachmentExtra;
  /// Backend-specific extras carried on every [`TrackInfo`] — the place
  /// a backend keeps what the portable row has no seat for (a native
  /// stream index, disposition bits, the container's metadata bag).
  type TrackExtra;

  /// Text carrier for a track's identity metadata — the attachment
  /// filename and MIME type on [`TrackInfo`].
  ///
  /// A seat rather than a fixed string type so the core stays
  /// allocator-free: a backend with compile-time names can bind
  /// `&'static str`, one that reads them out of a container binds an
  /// owned inline string.
  type Text: AsRef<str> + Debug;
}

/// The [`VideoPacket`] a [`DemuxAdapter`] delivers over buffer `D`.
pub type DemuxVideoPacket<E, D> =
  VideoPacket<<<E as DemuxAdapter>::Video as VideoAdapter>::PacketExtra, D>;

/// The [`AudioPacket`] a [`DemuxAdapter`] delivers over buffer `D`.
pub type DemuxAudioPacket<E, D> =
  AudioPacket<<<E as DemuxAdapter>::Audio as AudioAdapter>::PacketExtra, D>;

/// The [`SubtitlePacket`] a [`DemuxAdapter`] delivers over buffer `D`.
pub type DemuxSubtitlePacket<E, D> =
  SubtitlePacket<<<E as DemuxAdapter>::Subtitle as SubtitleAdapter>::PacketExtra, D>;

/// The [`DataPacket`] a [`DemuxAdapter`] delivers over buffer `D`.
pub type DemuxDataPacket<E, D> = DataPacket<<E as DemuxAdapter>::DataExtra, D>;

/// The [`AttachmentPacket`] a [`DemuxAdapter`] delivers over buffer `D`.
pub type DemuxAttachmentPacket<E, D> = AttachmentPacket<<E as DemuxAdapter>::AttachmentExtra, D>;

// ---------------------------------------------------------------------------
//  The two packet types the demux tier adds.
//
//  They live here rather than beside their three siblings in
//  `crate::packet` because they exist only as demux products: this
//  crate has no `DataDecoder` and no `AttachmentDecoder`, so nothing
//  ever hands one of these to a decoder. Their shape still follows
//  `packet.rs` exactly — private fields, `const` accessors, `with_*`
//  consuming builders and `set_*` in-place mutators.
// ---------------------------------------------------------------------------

/// A timed opaque-data packet — timecode, KLV, timed ID3.
///
/// Data tracks are real packet streams: they run along the file's
/// timeline and carry a payload per timestamp. They are never
/// reordered, so — like [`SubtitlePacket`] and for the same reason —
/// there is no DTS seat; a data packet's presentation time is its
/// decode time.
///
/// `Clone` and `Debug` derive directly — see [`VideoPacket`]'s docs
/// for why the per-parameter bound this produces is already precise.
#[derive(Clone, Debug)]
pub struct DataPacket<E, D> {
  pts: Option<Timestamp>,
  duration: Option<Timestamp>,
  flags: PacketFlags,
  data: D,
  extra: E,
}

impl<E, D> DataPacket<E, D> {
  /// Constructs a `DataPacket` from `data` and `extra`. Timestamps
  /// default to `None` and flags to empty.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(data: D, extra: E) -> Self {
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
  /// Returns the payload buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data(&self) -> &D {
    &self.data
  }
  /// Returns the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &E {
    &self.extra
  }
  /// Returns a mutable reference to the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extra_mut(&mut self) -> &mut E {
    &mut self.extra
  }
  /// Consumes the packet and returns the buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_data(self) -> D {
    self.data
  }
  /// Consumes the packet and returns `(buffer, extras)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (D, E) {
    (self.data, self.extra)
  }

  /// Sets the PTS (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_pts(mut self, v: Option<Timestamp>) -> Self {
    self.pts = v;
    self
  }
  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }
  /// Sets the flags (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
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

/// An attachment packet — a whole file carried inside the container.
///
/// **No timestamps.** A font or a cover image is not on the timeline;
/// it is present for the whole file or not at all. Its identity — the
/// filename it was attached under, its MIME type — is not repeated here
/// either: that belongs to the track, and lives on [`TrackInfo`].
/// [`PacketFlags`] is kept because `CORRUPT` still means something for
/// a payload that failed to read.
///
/// `Clone` and `Debug` derive directly — see [`VideoPacket`]'s docs
/// for why the per-parameter bound this produces is already precise.
#[derive(Clone, Debug)]
pub struct AttachmentPacket<E, D> {
  flags: PacketFlags,
  data: D,
  extra: E,
}

impl<E, D> AttachmentPacket<E, D> {
  /// Constructs an `AttachmentPacket` from `data` and `extra`. Flags
  /// default to empty.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(data: D, extra: E) -> Self {
    Self {
      flags: PacketFlags::empty(),
      data,
      extra,
    }
  }

  /// Returns the packet flags.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn flags(&self) -> PacketFlags {
    self.flags
  }
  /// Returns the attached file's bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data(&self) -> &D {
    &self.data
  }
  /// Returns the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &E {
    &self.extra
  }
  /// Returns a mutable reference to the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extra_mut(&mut self) -> &mut E {
    &mut self.extra
  }
  /// Consumes the packet and returns the buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_data(self) -> D {
    self.data
  }
  /// Consumes the packet and returns `(buffer, extras)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (D, E) {
    (self.data, self.extra)
  }

  /// Sets the flags (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_flags(mut self, v: PacketFlags) -> Self {
    self.flags = v;
    self
  }

  /// Sets the flags in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_flags(&mut self, v: PacketFlags) -> &mut Self {
    self.flags = v;
    self
  }
}

// ---------------------------------------------------------------------------
//  The track table.
// ---------------------------------------------------------------------------

/// Payload for [`TrackParams::Video`].
pub struct VideoTrackParams<E: DemuxAdapter> {
  codec: E::CodecId,
  width: u32,
  height: u32,
  pixel_format: <E::Video as VideoAdapter>::PixelFormat,
  frame_rate: Option<Timebase>,
}

impl<E: DemuxAdapter> VideoTrackParams<E> {
  /// Constructs a `VideoTrackParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(
    codec: E::CodecId,
    width: u32,
    height: u32,
    pixel_format: <E::Video as VideoAdapter>::PixelFormat,
    frame_rate: Option<Timebase>,
  ) -> Self {
    Self {
      codec,
      width,
      height,
      pixel_format,
      frame_rate,
    }
  }

  /// Returns the codec identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.codec
  }
  /// Returns the coded width in pixels.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn width(&self) -> u32 {
    self.width
  }
  /// Returns the coded height in pixels.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn height(&self) -> u32 {
    self.height
  }
  /// Returns the pixel format the track declares, in the backend's
  /// vocabulary.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn pixel_format(&self) -> &<E::Video as VideoAdapter>::PixelFormat {
    &self.pixel_format
  }
  /// Returns the average frame rate, as a rate-shaped [`Timebase`]
  /// (`30000/1001` for 29.97 fps), or `None` when the container does
  /// not say.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn frame_rate(&self) -> Option<Timebase> {
    self.frame_rate
  }
}

// `Clone` / `Debug` are hand-written for the same associated-type
// reason as `TrackParams` itself (see its own impls, below): every
// field here that is not a plain `u32` routes through an associated
// type on `E`, and each of those already carries the bound it needs
// on the trait that declares it. `#[derive(Clone)]` would add a flat
// `E: Clone` bound the struct's own fields never ask for.
impl<E: DemuxAdapter> Clone for VideoTrackParams<E> {
  fn clone(&self) -> Self {
    Self {
      codec: self.codec,
      width: self.width,
      height: self.height,
      pixel_format: self.pixel_format.clone(),
      frame_rate: self.frame_rate,
    }
  }
}

impl<E: DemuxAdapter> Debug for VideoTrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("VideoTrackParams")
      .field("codec", &self.codec)
      .field("width", &self.width)
      .field("height", &self.height)
      .field("pixel_format", &self.pixel_format)
      .field("frame_rate", &self.frame_rate)
      .finish()
  }
}

/// Payload for [`TrackParams::Audio`].
pub struct AudioTrackParams<E: DemuxAdapter> {
  codec: E::CodecId,
  sample_rate: u32,
  channel_count: u8,
  sample_format: <E::Audio as AudioAdapter>::SampleFormat,
  channel_layout: <E::Audio as AudioAdapter>::ChannelLayout,
}

impl<E: DemuxAdapter> AudioTrackParams<E> {
  /// Constructs an `AudioTrackParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(
    codec: E::CodecId,
    sample_rate: u32,
    channel_count: u8,
    sample_format: <E::Audio as AudioAdapter>::SampleFormat,
    channel_layout: <E::Audio as AudioAdapter>::ChannelLayout,
  ) -> Self {
    Self {
      codec,
      sample_rate,
      channel_count,
      sample_format,
      channel_layout,
    }
  }

  /// Returns the codec identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.codec
  }
  /// Returns the sample rate in Hz.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn sample_rate(&self) -> u32 {
    self.sample_rate
  }
  /// Returns the channel count.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn channel_count(&self) -> u8 {
    self.channel_count
  }
  /// Returns the sample format the track declares, in the backend's
  /// vocabulary.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn sample_format(&self) -> <E::Audio as AudioAdapter>::SampleFormat {
    self.sample_format
  }
  /// Returns the channel layout the track declares, in the backend's
  /// vocabulary.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn channel_layout(&self) -> &<E::Audio as AudioAdapter>::ChannelLayout {
    &self.channel_layout
  }
}

impl<E: DemuxAdapter> Clone for AudioTrackParams<E> {
  fn clone(&self) -> Self {
    Self {
      codec: self.codec,
      sample_rate: self.sample_rate,
      channel_count: self.channel_count,
      sample_format: self.sample_format,
      channel_layout: self.channel_layout.clone(),
    }
  }
}

impl<E: DemuxAdapter> Debug for AudioTrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AudioTrackParams")
      .field("codec", &self.codec)
      .field("sample_rate", &self.sample_rate)
      .field("channel_count", &self.channel_count)
      .field("sample_format", &self.sample_format)
      .field("channel_layout", &self.channel_layout)
      .finish()
  }
}

/// Payload for [`TrackParams::Subtitle`].
pub struct SubtitleTrackParams<E: DemuxAdapter> {
  codec: E::CodecId,
}

impl<E: DemuxAdapter> SubtitleTrackParams<E> {
  /// Constructs a `SubtitleTrackParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(codec: E::CodecId) -> Self {
    Self { codec }
  }

  /// Returns the codec identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.codec
  }
}

impl<E: DemuxAdapter> Clone for SubtitleTrackParams<E> {
  fn clone(&self) -> Self {
    Self { codec: self.codec }
  }
}

impl<E: DemuxAdapter> Debug for SubtitleTrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SubtitleTrackParams")
      .field("codec", &self.codec)
      .finish()
  }
}

/// Payload for [`TrackParams::Data`].
pub struct DataTrackParams<E: DemuxAdapter> {
  codec: E::CodecId,
}

impl<E: DemuxAdapter> DataTrackParams<E> {
  /// Constructs a `DataTrackParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(codec: E::CodecId) -> Self {
    Self { codec }
  }

  /// Returns the codec identifier.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.codec
  }
}

impl<E: DemuxAdapter> Clone for DataTrackParams<E> {
  fn clone(&self) -> Self {
    Self { codec: self.codec }
  }
}

impl<E: DemuxAdapter> Debug for DataTrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("DataTrackParams")
      .field("codec", &self.codec)
      .finish()
  }
}

/// Payload for [`TrackParams::Attachment`].
pub struct AttachmentTrackParams<E: DemuxAdapter> {
  /// Codec identifier — the font format, or the still image's codec
  /// for cover art.
  codec: E::CodecId,
}

impl<E: DemuxAdapter> AttachmentTrackParams<E> {
  /// Constructs an `AttachmentTrackParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(codec: E::CodecId) -> Self {
    Self { codec }
  }

  /// Returns the codec identifier — the font format, or the still
  /// image's codec for cover art.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.codec
  }
}

impl<E: DemuxAdapter> Clone for AttachmentTrackParams<E> {
  fn clone(&self) -> Self {
    Self { codec: self.codec }
  }
}

impl<E: DemuxAdapter> Debug for AttachmentTrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AttachmentTrackParams")
      .field("codec", &self.codec)
      .finish()
  }
}

/// Payload for [`TrackParams::Unknown`].
pub struct UnknownTrackParams<E: DemuxAdapter> {
  /// Codec identifier, which may itself be the backend's "none".
  codec: E::CodecId,
}

impl<E: DemuxAdapter> UnknownTrackParams<E> {
  /// Constructs an `UnknownTrackParams`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(codec: E::CodecId) -> Self {
    Self { codec }
  }

  /// Returns the codec identifier, which may itself be the backend's
  /// "none".
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    self.codec
  }
}

impl<E: DemuxAdapter> Clone for UnknownTrackParams<E> {
  fn clone(&self) -> Self {
    Self { codec: self.codec }
  }
}

impl<E: DemuxAdapter> Debug for UnknownTrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("UnknownTrackParams")
      .field("codec", &self.codec)
      .finish()
  }
}

/// A track's codec and per-kind parameters.
///
/// The arm **is** the kind: [`TrackInfo::kind`] reads it off this enum
/// rather than storing a second copy that could disagree with the
/// payload beside it.
pub enum TrackParams<E: DemuxAdapter> {
  /// Motion video.
  Video(VideoTrackParams<E>),
  /// Audio.
  Audio(AudioTrackParams<E>),
  /// Subtitles / captions.
  Subtitle(SubtitleTrackParams<E>),
  /// Timed opaque data.
  Data(DataTrackParams<E>),
  /// An attached file — a font, cover art.
  Attachment(AttachmentTrackParams<E>),
  /// A track the backend could not classify.
  Unknown(UnknownTrackParams<E>),
}

impl<E: DemuxAdapter> TrackParams<E> {
  /// Returns the kind this arm describes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> TrackKind {
    match self {
      Self::Video(_) => TrackKind::Video,
      Self::Audio(_) => TrackKind::Audio,
      Self::Subtitle(_) => TrackKind::Subtitle,
      Self::Data(_) => TrackKind::Data,
      Self::Attachment(_) => TrackKind::Attachment,
      Self::Unknown(_) => TrackKind::Unknown,
    }
  }

  /// Returns the codec identifier, whichever arm this is.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn codec(&self) -> E::CodecId {
    match self {
      Self::Video(p) => p.codec(),
      Self::Audio(p) => p.codec(),
      Self::Subtitle(p) => p.codec(),
      Self::Data(p) => p.codec(),
      Self::Attachment(p) => p.codec(),
      Self::Unknown(p) => p.codec(),
    }
  }
}

// `Clone` / `Debug` are hand-written, not derived: a flat
// `#[derive(Clone)]` over the enum's own type parameter `E` would
// demand `E: Clone`, which none of the six payload structs' fields
// need — each already carries the precise bound it requires from the
// trait that declares its associated type. See each payload struct's
// own `Clone` / `Debug` impl, above, for the same reasoning one level
// down.
impl<E: DemuxAdapter> Clone for TrackParams<E> {
  fn clone(&self) -> Self {
    match self {
      Self::Video(p) => Self::Video(p.clone()),
      Self::Audio(p) => Self::Audio(p.clone()),
      Self::Subtitle(p) => Self::Subtitle(p.clone()),
      Self::Data(p) => Self::Data(p.clone()),
      Self::Attachment(p) => Self::Attachment(p.clone()),
      Self::Unknown(p) => Self::Unknown(p.clone()),
    }
  }
}

impl<E: DemuxAdapter> Debug for TrackParams<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Video(p) => f.debug_tuple("Video").field(p).finish(),
      Self::Audio(p) => f.debug_tuple("Audio").field(p).finish(),
      Self::Subtitle(p) => f.debug_tuple("Subtitle").field(p).finish(),
      Self::Data(p) => f.debug_tuple("Data").field(p).finish(),
      Self::Attachment(p) => f.debug_tuple("Attachment").field(p).finish(),
      Self::Unknown(p) => f.debug_tuple("Unknown").field(p).finish(),
    }
  }
}

/// One row of the track table [`Demuxer::tracks`] returns.
///
/// Carries what a consumer needs to decide whether it wants the track
/// and how to open a decoder for it: the kind, the timebase every
/// timestamp on that track is expressed in, the duration when the
/// container knows it, the per-kind codec parameters, and — for
/// attachments — the identity the file was attached under.
///
/// Everything a particular backend knows and this row has no seat for
/// rides [`DemuxAdapter::TrackExtra`].
pub struct TrackInfo<E: DemuxAdapter> {
  timebase: Timebase,
  duration: Option<Timestamp>,
  params: TrackParams<E>,
  filename: Option<E::Text>,
  mime_type: Option<E::Text>,
  extra: E::TrackExtra,
}

impl<E: DemuxAdapter> TrackInfo<E> {
  /// Constructs a `TrackInfo`. Identity metadata defaults to `None`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(timebase: Timebase, params: TrackParams<E>, extra: E::TrackExtra) -> Self {
    Self {
      timebase,
      duration: None,
      params,
      filename: None,
      mime_type: None,
      extra,
    }
  }

  /// Returns the track's kind, read off [`Self::params`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> TrackKind {
    self.params.kind()
  }
  /// Returns the timebase every timestamp on this track is expressed in.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn timebase(&self) -> Timebase {
    self.timebase
  }
  /// Returns the track duration, or `None` when the container does not
  /// carry one.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Returns the per-kind codec parameters.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn params(&self) -> &TrackParams<E> {
    &self.params
  }
  /// Returns the filename an attachment was attached under, when the
  /// container carries one. `None` for every other kind.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn filename(&self) -> Option<&E::Text> {
    self.filename.as_ref()
  }
  /// Returns an attachment's declared MIME type, when the container
  /// carries one. `None` for every other kind.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn mime_type(&self) -> Option<&E::Text> {
    self.mime_type.as_ref()
  }
  /// Returns the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn extra(&self) -> &E::TrackExtra {
    &self.extra
  }
  /// Returns a mutable reference to the backend-specific extras.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn extra_mut(&mut self) -> &mut E::TrackExtra {
    &mut self.extra
  }

  /// Sets the duration (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_duration(mut self, v: Option<Timestamp>) -> Self {
    self.duration = v;
    self
  }
  /// Sets the attachment filename (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_filename(mut self, v: Option<E::Text>) -> Self {
    self.filename = v;
    self
  }
  /// Sets the attachment MIME type (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_mime_type(mut self, v: Option<E::Text>) -> Self {
    self.mime_type = v;
    self
  }

  /// Sets the duration in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_duration(&mut self, v: Option<Timestamp>) -> &mut Self {
    self.duration = v;
    self
  }
  /// Sets the attachment filename in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_filename(&mut self, v: Option<E::Text>) -> &mut Self {
    self.filename = v;
    self
  }
  /// Sets the attachment MIME type in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_mime_type(&mut self, v: Option<E::Text>) -> &mut Self {
    self.mime_type = v;
    self
  }
}

// `Clone` / `Debug` are hand-written for the same reason as
// `TrackParams`'s: `#[derive(Clone)]` would add `E: Clone`, but the
// fields that actually need a bound are `E::TrackExtra` and `E::Text`
// — two associated types `E: Clone` neither implies nor is implied
// by. `TrackParams<E>` itself needs no extra bound (see its own impl,
// just above); `E::Text: Debug` is already guaranteed by
// `DemuxAdapter::Text`'s own trait bound, so only `E::TrackExtra`
// needs naming for `Debug`.
impl<E: DemuxAdapter> Clone for TrackInfo<E>
where
  E::TrackExtra: Clone,
  E::Text: Clone,
{
  fn clone(&self) -> Self {
    Self {
      timebase: self.timebase,
      duration: self.duration,
      params: self.params.clone(),
      filename: self.filename.clone(),
      mime_type: self.mime_type.clone(),
      extra: self.extra.clone(),
    }
  }
}

impl<E: DemuxAdapter> Debug for TrackInfo<E>
where
  E::TrackExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TrackInfo")
      .field("timebase", &self.timebase)
      .field("duration", &self.duration)
      .field("params", &self.params)
      .field("filename", &self.filename)
      .field("mime_type", &self.mime_type)
      .field("extra", &self.extra)
      .finish()
  }
}

// ---------------------------------------------------------------------------
//  The delivery envelope.
// ---------------------------------------------------------------------------

/// Payload for [`DemuxedPacket::Video`].
pub struct VideoTrackPacket<E: DemuxAdapter, D> {
  track: TrackIndex,
  packet: DemuxVideoPacket<E, D>,
}

impl<E: DemuxAdapter, D> VideoTrackPacket<E, D> {
  /// Constructs a `VideoTrackPacket`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(track: TrackIndex, packet: DemuxVideoPacket<E, D>) -> Self {
    Self { track, packet }
  }

  /// Returns the track this packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn track(&self) -> TrackIndex {
    self.track
  }
  /// Returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn packet(&self) -> &DemuxVideoPacket<E, D> {
    &self.packet
  }
  /// Consumes the envelope and returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_packet(self) -> DemuxVideoPacket<E, D> {
    self.packet
  }
  /// Consumes the envelope and returns `(track, packet)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (TrackIndex, DemuxVideoPacket<E, D>) {
    (self.track, self.packet)
  }
}

// `Clone` / `Debug` are hand-written for the same associated-type
// reason as `TrackParams`'s payload structs, above: the bound belongs
// on `<E::Video as VideoAdapter>::PacketExtra`, not on `E` itself,
// which `#[derive]` cannot see.
impl<E, D> Clone for VideoTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Clone,
  <E::Video as VideoAdapter>::PacketExtra: Clone,
{
  fn clone(&self) -> Self {
    Self {
      track: self.track,
      packet: self.packet.clone(),
    }
  }
}

impl<E, D> Debug for VideoTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Debug,
  <E::Video as VideoAdapter>::PacketExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("VideoTrackPacket")
      .field("track", &self.track)
      .field("packet", &self.packet)
      .finish()
  }
}

/// Payload for [`DemuxedPacket::Audio`].
pub struct AudioTrackPacket<E: DemuxAdapter, D> {
  track: TrackIndex,
  packet: DemuxAudioPacket<E, D>,
}

impl<E: DemuxAdapter, D> AudioTrackPacket<E, D> {
  /// Constructs an `AudioTrackPacket`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(track: TrackIndex, packet: DemuxAudioPacket<E, D>) -> Self {
    Self { track, packet }
  }

  /// Returns the track this packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn track(&self) -> TrackIndex {
    self.track
  }
  /// Returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn packet(&self) -> &DemuxAudioPacket<E, D> {
    &self.packet
  }
  /// Consumes the envelope and returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_packet(self) -> DemuxAudioPacket<E, D> {
    self.packet
  }
  /// Consumes the envelope and returns `(track, packet)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (TrackIndex, DemuxAudioPacket<E, D>) {
    (self.track, self.packet)
  }
}

impl<E, D> Clone for AudioTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Clone,
  <E::Audio as AudioAdapter>::PacketExtra: Clone,
{
  fn clone(&self) -> Self {
    Self {
      track: self.track,
      packet: self.packet.clone(),
    }
  }
}

impl<E, D> Debug for AudioTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Debug,
  <E::Audio as AudioAdapter>::PacketExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AudioTrackPacket")
      .field("track", &self.track)
      .field("packet", &self.packet)
      .finish()
  }
}

/// Payload for [`DemuxedPacket::Subtitle`].
pub struct SubtitleTrackPacket<E: DemuxAdapter, D> {
  track: TrackIndex,
  packet: DemuxSubtitlePacket<E, D>,
}

impl<E: DemuxAdapter, D> SubtitleTrackPacket<E, D> {
  /// Constructs a `SubtitleTrackPacket`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(track: TrackIndex, packet: DemuxSubtitlePacket<E, D>) -> Self {
    Self { track, packet }
  }

  /// Returns the track this packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn track(&self) -> TrackIndex {
    self.track
  }
  /// Returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn packet(&self) -> &DemuxSubtitlePacket<E, D> {
    &self.packet
  }
  /// Consumes the envelope and returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_packet(self) -> DemuxSubtitlePacket<E, D> {
    self.packet
  }
  /// Consumes the envelope and returns `(track, packet)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (TrackIndex, DemuxSubtitlePacket<E, D>) {
    (self.track, self.packet)
  }
}

impl<E, D> Clone for SubtitleTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Clone,
  <E::Subtitle as SubtitleAdapter>::PacketExtra: Clone,
{
  fn clone(&self) -> Self {
    Self {
      track: self.track,
      packet: self.packet.clone(),
    }
  }
}

impl<E, D> Debug for SubtitleTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Debug,
  <E::Subtitle as SubtitleAdapter>::PacketExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("SubtitleTrackPacket")
      .field("track", &self.track)
      .field("packet", &self.packet)
      .finish()
  }
}

/// Payload for [`DemuxedPacket::Data`].
pub struct DataTrackPacket<E: DemuxAdapter, D> {
  track: TrackIndex,
  packet: DemuxDataPacket<E, D>,
}

impl<E: DemuxAdapter, D> DataTrackPacket<E, D> {
  /// Constructs a `DataTrackPacket`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(track: TrackIndex, packet: DemuxDataPacket<E, D>) -> Self {
    Self { track, packet }
  }

  /// Returns the track this packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn track(&self) -> TrackIndex {
    self.track
  }
  /// Returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn packet(&self) -> &DemuxDataPacket<E, D> {
    &self.packet
  }
  /// Consumes the envelope and returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_packet(self) -> DemuxDataPacket<E, D> {
    self.packet
  }
  /// Consumes the envelope and returns `(track, packet)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (TrackIndex, DemuxDataPacket<E, D>) {
    (self.track, self.packet)
  }
}

impl<E, D> Clone for DataTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Clone,
  E::DataExtra: Clone,
{
  fn clone(&self) -> Self {
    Self {
      track: self.track,
      packet: self.packet.clone(),
    }
  }
}

impl<E, D> Debug for DataTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Debug,
  E::DataExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("DataTrackPacket")
      .field("track", &self.track)
      .field("packet", &self.packet)
      .finish()
  }
}

/// Payload for [`DemuxedPacket::Attachment`].
pub struct AttachmentTrackPacket<E: DemuxAdapter, D> {
  track: TrackIndex,
  packet: DemuxAttachmentPacket<E, D>,
}

impl<E: DemuxAdapter, D> AttachmentTrackPacket<E, D> {
  /// Constructs an `AttachmentTrackPacket`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(track: TrackIndex, packet: DemuxAttachmentPacket<E, D>) -> Self {
    Self { track, packet }
  }

  /// Returns the track this packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn track(&self) -> TrackIndex {
    self.track
  }
  /// Returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn packet(&self) -> &DemuxAttachmentPacket<E, D> {
    &self.packet
  }
  /// Consumes the envelope and returns the packet.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_packet(self) -> DemuxAttachmentPacket<E, D> {
    self.packet
  }
  /// Consumes the envelope and returns `(track, packet)`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn into_parts(self) -> (TrackIndex, DemuxAttachmentPacket<E, D>) {
    (self.track, self.packet)
  }
}

impl<E, D> Clone for AttachmentTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Clone,
  E::AttachmentExtra: Clone,
{
  fn clone(&self) -> Self {
    Self {
      track: self.track,
      packet: self.packet.clone(),
    }
  }
}

impl<E, D> Debug for AttachmentTrackPacket<E, D>
where
  E: DemuxAdapter,
  D: Debug,
  E::AttachmentExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AttachmentTrackPacket")
      .field("track", &self.track)
      .field("packet", &self.packet)
      .finish()
  }
}

/// One demuxed packet, with the track it came from.
///
/// Five arms — the whole delivery roster. Packets do not carry track
/// coordinates; this envelope does, which is what lets the same
/// [`VideoPacket`] type be handed straight to a decoder without
/// stripping a field the decoder has no use for.
///
/// A track whose kind is [`TrackKind::Unknown`] has no arm, and its
/// packets are therefore never delivered. The roster is closed at five
/// on purpose: a kind nothing can name is a kind nothing can consume.
pub enum DemuxedPacket<E: DemuxAdapter, D> {
  /// A compressed video packet.
  Video(VideoTrackPacket<E, D>),
  /// A compressed audio packet.
  Audio(AudioTrackPacket<E, D>),
  /// A compressed subtitle packet.
  Subtitle(SubtitleTrackPacket<E, D>),
  /// A timed opaque-data packet.
  Data(DataTrackPacket<E, D>),
  /// An attachment payload.
  Attachment(AttachmentTrackPacket<E, D>),
}

impl<E: DemuxAdapter, D> DemuxedPacket<E, D> {
  /// Returns the track this packet belongs to.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn track(&self) -> TrackIndex {
    match self {
      Self::Video(p) => p.track(),
      Self::Audio(p) => p.track(),
      Self::Subtitle(p) => p.track(),
      Self::Data(p) => p.track(),
      Self::Attachment(p) => p.track(),
    }
  }

  /// Returns the kind of track this packet came from.
  ///
  /// Always equal to `demuxer.tracks()[self.track().get()].kind()`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> TrackKind {
    match self {
      Self::Video(_) => TrackKind::Video,
      Self::Audio(_) => TrackKind::Audio,
      Self::Subtitle(_) => TrackKind::Subtitle,
      Self::Data(_) => TrackKind::Data,
      Self::Attachment(_) => TrackKind::Attachment,
    }
  }
}

// `Clone` / `Debug` are hand-written for the same associated-type
// reason as `TrackParams` and `TrackInfo` above. The five payload
// types this enum carries route through `DemuxAdapter` and its three
// per-kind sub-adapters — `<E::Video as VideoAdapter>::PacketExtra`,
// `<E::Audio as AudioAdapter>::PacketExtra`, `<E::Subtitle as
// SubtitleAdapter>::PacketExtra`, `E::DataExtra`, `E::AttachmentExtra`
// — five independent associated types plus the buffer `D`, none of
// which `#[derive]`'s flat `E: Clone` bound would name.
impl<E, D> Clone for DemuxedPacket<E, D>
where
  E: DemuxAdapter,
  D: Clone,
  <E::Video as VideoAdapter>::PacketExtra: Clone,
  <E::Audio as AudioAdapter>::PacketExtra: Clone,
  <E::Subtitle as SubtitleAdapter>::PacketExtra: Clone,
  E::DataExtra: Clone,
  E::AttachmentExtra: Clone,
{
  fn clone(&self) -> Self {
    match self {
      Self::Video(p) => Self::Video(p.clone()),
      Self::Audio(p) => Self::Audio(p.clone()),
      Self::Subtitle(p) => Self::Subtitle(p.clone()),
      Self::Data(p) => Self::Data(p.clone()),
      Self::Attachment(p) => Self::Attachment(p.clone()),
    }
  }
}

impl<E, D> Debug for DemuxedPacket<E, D>
where
  E: DemuxAdapter,
  D: Debug,
  <E::Video as VideoAdapter>::PacketExtra: Debug,
  <E::Audio as AudioAdapter>::PacketExtra: Debug,
  <E::Subtitle as SubtitleAdapter>::PacketExtra: Debug,
  E::DataExtra: Debug,
  E::AttachmentExtra: Debug,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Video(p) => f.debug_tuple("Video").field(p).finish(),
      Self::Audio(p) => f.debug_tuple("Audio").field(p).finish(),
      Self::Subtitle(p) => f.debug_tuple("Subtitle").field(p).finish(),
      Self::Data(p) => f.debug_tuple("Data").field(p).finish(),
      Self::Attachment(p) => f.debug_tuple("Attachment").field(p).finish(),
    }
  }
}

// ---------------------------------------------------------------------------
//  The session face.
// ---------------------------------------------------------------------------

/// An opened container session: the track table, a pull loop, and a seek.
///
/// # Delivery order
///
/// [`next_packet`](Self::next_packet) returns packets in **interleaved
/// file order** — the order the container stores them in, tracks mixed
/// together exactly as written. `Ok(None)` means end of file, and once
/// it is returned it stays returned until a [`seek`](Self::seek) moves
/// the session somewhere else.
///
/// # The attachment contract
///
/// An [`Attachment`](TrackKind::Attachment) track delivers **exactly
/// one packet, before any timed packet**. Attachments are not on the
/// timeline, so a consumer must be able to collect them all before it
/// starts consuming time — a subtitle renderer needs its fonts before
/// the first cue, and a thumbnailer wants the cover art without reading
/// the file to its end. Backends satisfy this by synthesising the
/// packet when the container keeps the payload outside the packet
/// stream (fonts, whose bytes live in the track's codec extradata) and
/// by hoisting the natural one when it exists (cover art, which is a
/// real packet the container stores).
///
/// A track's identity — the filename it was attached under, its MIME
/// type — is on [`TrackInfo`], not repeated on every packet.
///
/// # Seeking
///
/// [`seek`](Self::seek) obeys three laws:
///
/// 1. **It flushes session state.** Anything buffered from before the
///    seek is discarded; the next [`next_packet`](Self::next_packet)
///    reads from the new position.
/// 2. **It lands on the nearest keyframe at or before the target.**
///    Never after. A decoder fed from a landing point past the target
///    would have no reference frame, so "at or before" is a
///    correctness requirement, not a preference — the caller discards
///    the packets between the landing point and the target itself.
/// 3. **Attachments are never replayed.** An attachment already
///    delivered is not delivered again, however many times the session
///    seeks. One attachment track yields one packet for the life of the
///    session — a seek moves the *timeline*, and attachments are not on
///    it.
///
/// # What is not here
///
/// Opening. See the [module docs](self#construction-is-not-on-the-trait).
pub trait Demuxer {
  /// Backend-specific vocabulary.
  type Adapter: DemuxAdapter;
  /// Buffer type held by the packets this session produces.
  type Buffer: AsRef<[u8]>;
  /// Demuxer-specific error type.
  type Error;

  /// Returns the container's track table.
  ///
  /// Position `i` describes [`TrackIndex::new(i)`](TrackIndex::new) —
  /// the coordinate every [`DemuxedPacket`] carries.
  fn tracks(&self) -> &[TrackInfo<Self::Adapter>];

  /// Pulls the next packet in interleaved file order, or `Ok(None)` at
  /// end of file.
  fn next_packet(
    &mut self,
  ) -> Result<Option<DemuxedPacket<Self::Adapter, Self::Buffer>>, Self::Error>;

  /// Seeks to `target`, landing on the nearest keyframe at or before it.
  ///
  /// See the [three laws](Self#seeking) on the trait.
  fn seek(&mut self, target: Timestamp) -> Result<(), Self::Error>;
}

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

  struct ALoop;
  impl AudioAdapter for ALoop {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  struct SLoop;
  impl SubtitleAdapter for SLoop {
    type CodecId = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  struct Loopback;
  impl DemuxAdapter for Loopback {
    type CodecId = u32;
    type Video = VLoop;
    type Audio = ALoop;
    type Subtitle = SLoop;
    type DataExtra = ();
    type AttachmentExtra = ();
    type TrackExtra = ();
    type Text = &'static str;
  }

  fn ms_tb() -> Timebase {
    Timebase::new(1, NonZeroI32::new(1000).expect("non-zero"))
  }

  #[test]
  fn track_index_round_trips() {
    assert_eq!(TrackIndex::new(3).get(), 3);
    assert_eq!(TrackIndex::default(), TrackIndex::new(0));
  }

  #[test]
  fn the_kind_is_read_off_the_params_arm() {
    // The one-source-of-truth property: there is no way to build a
    // `TrackInfo` whose advertised kind disagrees with its payload.
    let rows: [(TrackParams<Loopback>, TrackKind); 6] = [
      (
        TrackParams::Video(VideoTrackParams::new(1, 1920, 1080, 7, None)),
        TrackKind::Video,
      ),
      (
        TrackParams::Audio(AudioTrackParams::new(2, 48_000, 2, 3, 4)),
        TrackKind::Audio,
      ),
      (
        TrackParams::Subtitle(SubtitleTrackParams::new(3)),
        TrackKind::Subtitle,
      ),
      (TrackParams::Data(DataTrackParams::new(4)), TrackKind::Data),
      (
        TrackParams::Attachment(AttachmentTrackParams::new(5)),
        TrackKind::Attachment,
      ),
      (
        TrackParams::Unknown(UnknownTrackParams::new(0)),
        TrackKind::Unknown,
      ),
    ];
    for (params, expected) in rows {
      let codec = params.codec();
      let info = TrackInfo::<Loopback>::new(ms_tb(), params, ());
      assert_eq!(info.kind(), expected);
      assert_eq!(info.params().kind(), expected);
      assert_eq!(info.params().codec(), codec);
    }
  }

  #[test]
  fn track_params_clone_matches_the_original() {
    let original = TrackParams::<Loopback>::Video(VideoTrackParams::new(
      7,
      1920,
      1080,
      3,
      Some(Timebase::new(
        30_000,
        NonZeroI32::new(1001).expect("non-zero"),
      )),
    ));
    let cloned = original.clone();
    assert_eq!(cloned.kind(), original.kind());
    assert_eq!(cloned.codec(), original.codec());
    match (&original, &cloned) {
      (TrackParams::Video(p1), TrackParams::Video(p2)) => {
        assert_eq!(p1.width(), p2.width());
        assert_eq!(p1.height(), p2.height());
        assert_eq!(p1.pixel_format(), p2.pixel_format());
        assert_eq!(p1.frame_rate(), p2.frame_rate());
      }
      _ => panic!("clone changed the arm"),
    }
    assert!(format!("{cloned:?}").contains("Video"));
  }

  #[test]
  fn attachment_identity_lives_on_the_track() {
    let info = TrackInfo::<Loopback>::new(
      ms_tb(),
      TrackParams::Attachment(AttachmentTrackParams::new(9)),
      (),
    )
    .with_filename(Some("Arial.ttf"))
    .with_mime_type(Some("application/x-truetype-font"));
    assert_eq!(info.filename().copied(), Some("Arial.ttf"));
    assert_eq!(
      info.mime_type().copied(),
      Some("application/x-truetype-font")
    );
    assert_eq!(info.duration(), None);
  }

  #[test]
  fn track_info_clone_matches_the_original() {
    let original = TrackInfo::<Loopback>::new(
      ms_tb(),
      TrackParams::Attachment(AttachmentTrackParams::new(9)),
      (),
    )
    .with_filename(Some("Arial.ttf"))
    .with_mime_type(Some("application/x-truetype-font"))
    .with_duration(Some(Timestamp::new(500, ms_tb())));
    let cloned = original.clone();
    assert_eq!(cloned.kind(), original.kind());
    assert_eq!(cloned.timebase(), original.timebase());
    assert_eq!(cloned.duration(), original.duration());
    assert_eq!(cloned.filename(), original.filename());
    assert_eq!(cloned.mime_type(), original.mime_type());
    assert_eq!(cloned.params().codec(), original.params().codec());
    assert!(format!("{cloned:?}").contains("TrackInfo"));
  }

  #[test]
  fn data_packet_follows_the_house_shape() {
    let pts = Timestamp::new(1500, ms_tb());
    let p: DataPacket<(), &[u8]> = DataPacket::new(&b"klv"[..], ())
      .with_pts(Some(pts))
      .with_duration(Some(Timestamp::new(40, ms_tb())))
      .with_flags(PacketFlags::KEY);
    assert_eq!(p.pts(), Some(pts));
    assert_eq!(p.duration(), Some(Timestamp::new(40, ms_tb())));
    assert!(p.flags().contains(PacketFlags::KEY));
    let (data, _) = p.into_parts();
    assert_eq!(data, b"klv");
  }

  #[test]
  fn data_packet_clone_matches_the_original() {
    let pts = Timestamp::new(1500, ms_tb());
    let original: DataPacket<(), &[u8]> = DataPacket::new(&b"klv"[..], ())
      .with_pts(Some(pts))
      .with_duration(Some(Timestamp::new(40, ms_tb())))
      .with_flags(PacketFlags::KEY);
    let cloned = original.clone();
    assert_eq!(cloned.pts(), original.pts());
    assert_eq!(cloned.duration(), original.duration());
    assert_eq!(cloned.flags(), original.flags());
    assert_eq!(cloned.data(), original.data());
    assert!(format!("{cloned:?}").contains("DataPacket"));
  }

  #[test]
  fn an_attachment_packet_has_no_timestamp_seat() {
    let mut p: AttachmentPacket<(), &[u8]> = AttachmentPacket::new(&b"\x00\x01TTF"[..], ());
    assert_eq!(p.flags(), PacketFlags::empty());
    p.set_flags(PacketFlags::CORRUPT);
    assert!(p.flags().contains(PacketFlags::CORRUPT));
    assert_eq!(p.data(), &&b"\x00\x01TTF"[..]);
  }

  #[test]
  fn attachment_packet_clone_matches_the_original() {
    let mut original: AttachmentPacket<(), &[u8]> = AttachmentPacket::new(&b"\x00\x01TTF"[..], ());
    original.set_flags(PacketFlags::CORRUPT);
    let cloned = original.clone();
    assert_eq!(cloned.flags(), original.flags());
    assert_eq!(cloned.data(), original.data());
    assert!(format!("{cloned:?}").contains("AttachmentPacket"));
  }

  #[test]
  fn the_envelope_carries_the_coordinate_and_the_kind() {
    let track = TrackIndex::new(2);
    let packets: [(DemuxedPacket<Loopback, &[u8]>, TrackKind); 5] = [
      (
        DemuxedPacket::Video(VideoTrackPacket::new(track, VideoPacket::new(&[][..], ()))),
        TrackKind::Video,
      ),
      (
        DemuxedPacket::Audio(AudioTrackPacket::new(track, AudioPacket::new(&[][..], ()))),
        TrackKind::Audio,
      ),
      (
        DemuxedPacket::Subtitle(SubtitleTrackPacket::new(
          track,
          SubtitlePacket::new(&[][..], ()),
        )),
        TrackKind::Subtitle,
      ),
      (
        DemuxedPacket::Data(DataTrackPacket::new(track, DataPacket::new(&[][..], ()))),
        TrackKind::Data,
      ),
      (
        DemuxedPacket::Attachment(AttachmentTrackPacket::new(
          track,
          AttachmentPacket::new(&[][..], ()),
        )),
        TrackKind::Attachment,
      ),
    ];
    for (packet, expected) in packets {
      assert_eq!(packet.track(), track);
      assert_eq!(packet.kind(), expected);
    }
  }

  #[test]
  fn demuxed_packet_clone_matches_the_original() {
    let track = TrackIndex::new(2);
    let packets: [DemuxedPacket<Loopback, &[u8]>; 5] = [
      DemuxedPacket::Video(VideoTrackPacket::new(track, VideoPacket::new(&[][..], ()))),
      DemuxedPacket::Audio(AudioTrackPacket::new(track, AudioPacket::new(&[][..], ()))),
      DemuxedPacket::Subtitle(SubtitleTrackPacket::new(
        track,
        SubtitlePacket::new(&[][..], ()),
      )),
      DemuxedPacket::Data(DataTrackPacket::new(track, DataPacket::new(&[][..], ()))),
      DemuxedPacket::Attachment(AttachmentTrackPacket::new(
        track,
        AttachmentPacket::new(&[][..], ()),
      )),
    ];
    for packet in packets {
      let cloned = packet.clone();
      assert_eq!(cloned.track(), packet.track());
      assert_eq!(cloned.kind(), packet.kind());
      assert!(!format!("{cloned:?}").is_empty());
    }
  }

  /// Trivial loopback session — proves the trait is implementable and
  /// that its associated types resolve through the adapter bundle.
  struct LoopDemuxer {
    tracks: [TrackInfo<Loopback>; 1],
    drained: bool,
  }

  #[derive(Debug)]
  struct LoopError;

  impl Demuxer for LoopDemuxer {
    type Adapter = Loopback;
    type Buffer = &'static [u8];
    type Error = LoopError;

    fn tracks(&self) -> &[TrackInfo<Loopback>] {
      &self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<DemuxedPacket<Loopback, &'static [u8]>>, LoopError> {
      if self.drained {
        return Ok(None);
      }
      self.drained = true;
      Ok(Some(DemuxedPacket::Audio(AudioTrackPacket::new(
        TrackIndex::new(0),
        AudioPacket::new(&[][..], ()),
      ))))
    }

    fn seek(&mut self, _target: Timestamp) -> Result<(), LoopError> {
      self.drained = false;
      Ok(())
    }
  }

  #[test]
  fn the_session_face_is_implementable_and_none_means_eof() {
    fn _accepts<D: Demuxer>() {}
    _accepts::<LoopDemuxer>();

    let mut d = LoopDemuxer {
      tracks: [TrackInfo::new(
        ms_tb(),
        TrackParams::Audio(AudioTrackParams::new(1, 48_000, 2, 0, 0)),
        (),
      )],
      drained: false,
    };
    assert_eq!(d.tracks().len(), 1);
    assert_eq!(d.tracks()[0].kind(), TrackKind::Audio);
    assert!(matches!(
      d.next_packet().expect("pull"),
      Some(DemuxedPacket::Audio(_))
    ));
    assert!(d.next_packet().expect("pull").is_none());
    assert!(d.next_packet().expect("pull").is_none(), "EOF is sticky");
    d.seek(Timestamp::new(0, ms_tb())).expect("seek");
    assert!(d.next_packet().expect("pull").is_some());
  }
}

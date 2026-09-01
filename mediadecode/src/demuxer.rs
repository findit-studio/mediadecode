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

use core::{
  fmt::{self, Debug},
  ops::Deref,
};

use derive_more::{IsVariant, TryUnwrap, Unwrap};

use crate::{
  Timebase, Timestamp,
  adapter::{AudioAdapter, SubtitleAdapter, VideoAdapter},
  packet::{AudioPacket, PacketFlags, SubtitlePacket, VideoPacket},
};

// Nothing in this module owns an allocation: the whole demux tier,
// `Demuxer` included, is `core`-only. What a session's track table
// costs is the backend's own choice, made at `Demuxer::TrackHandle`
// — a heap-backed backend binds a refcounted handle, one that parses
// in place binds a borrow.
//
// `alloc` is bound here for the test mock alone, whose table is a
// `Vec`, and is scoped to this module so a reader can see exactly
// what needs the heap and when.
#[cfg(all(test, any(feature = "std", feature = "alloc")))]
extern crate alloc;

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
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, IsVariant)]
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

// `Debug` is hand-written for the same associated-type reason as
// `TrackParams` itself (see its own impl, below): every field here
// that is not a plain `u32` routes through an associated type on
// `E`, and each of those already carries the bound it needs on the
// trait that declares it. `#[derive(Debug)]` would add a flat `E:
// Debug` bound the struct's own fields never ask for.
//
// No `Clone` on this type, `TrackParams`, or `TrackInfo` — see
// `TrackInfo`'s own doc, below, for the message-carrier law that
// keeps it off the whole family.
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
///
/// No `Clone` — see [`TrackInfo`]'s own doc for the message-carrier
/// law that keeps it off this type too.
#[derive(IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
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

// `Debug` is hand-written, not derived: a flat `#[derive(Debug)]`
// over the enum's own type parameter `E` would demand `E: Debug`,
// which none of the six payload structs' fields need — each already
// carries the precise bound it requires from the trait that declares
// its associated type. See each payload struct's own `Debug` impl,
// above, for the same reasoning one level down.
//
// No `Clone`: see `TrackInfo`'s own doc, below, for the
// message-carrier law.
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
/// container knows it, the per-kind codec parameters, the language the
/// container declares for the track, and — for attachments — the
/// identity the file was attached under.
///
/// Everything a particular backend knows and this row has no seat for
/// rides [`DemuxAdapter::TrackExtra`].
///
/// **No `Clone`, on this type or on [`TrackParams`].** The
/// message-carrier law: messages may be `Clone`, but `Clone` is
/// always a refcount bump, never a deep copy — and a track row,
/// backend metadata down to codec parameters, is not cheap to
/// duplicate. A consumer that needs to share a row shares a *handle*
/// on it instead: [`Demuxer::tracks`] hands out
/// [`Demuxer::TrackHandle`]s over rows the session keeps for its
/// whole life, so a row is built once and every consumer after that
/// shares it by refcount.
///
/// The absence is load-bearing, not incidental. It is what leaves
/// `TrackHandle`'s `Clone` bound with no cheap deep-copying carrier
/// to admit: there is no `#[derive(Clone)]` road over a row that has
/// none, and `Box<TrackInfo<_>>` is not `Clone` either. A shared
/// handle or a borrow is what an implementor reaches for; a deep copy
/// would have to be hand-written, field by field, against the law
/// this paragraph states.
pub struct TrackInfo<E: DemuxAdapter> {
  timebase: Timebase,
  duration: Option<Timestamp>,
  params: TrackParams<E>,
  filename: Option<E::Text>,
  mime_type: Option<E::Text>,
  language: Option<E::Text>,
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
      language: None,
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
  /// Returns the language the **container declares** for this track,
  /// exactly as it is written there — `None` where the file declares
  /// none.
  ///
  /// # The file's word, unfolded
  ///
  /// This is a reading, never a reckoning. `None` means the container
  /// said nothing, and a `Some` is the tag as written: an MKV's ISO
  /// 639-2/B `ger`, an MP4's 639-2/T `deu`, a decades-old muxer's `iw`,
  /// a BCP 47 `zh-Hans`, or `und` where a file declares its language
  /// unknown. Each of those is a different string for something a
  /// vocabulary may well call one language, and none of them is
  /// normalised here.
  ///
  /// **Deliberately, and the alternative was measured.** Folding those
  /// spellings together takes registry tables — the IANA subtag
  /// registry for BCP 47 and ISO 639-2's own for the alpha-3 space BCP
  /// 47 does not register — and a crate that owns one is where that
  /// fold belongs. A demux tier that folded early would be a *second*
  /// authority on the same question, disagreeing with the first in
  /// exactly the cases the registries exist for; and a narrower seat —
  /// a three-letter code, say — would have had nowhere to put
  /// `zh-Hans`, which is a tag Matroska really writes. So the seat
  /// carries the declaration whole and leaves the fold downstream,
  /// where one door can apply it once.
  ///
  /// # Every kind, not only subtitles
  ///
  /// Containers tag audio tracks as readily as subtitle ones — a dub
  /// and its captions are the same question asked twice — so the seat
  /// is on the row rather than on one arm of
  /// [`TrackParams`](crate::demuxer::TrackParams).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn language(&self) -> Option<&E::Text> {
    self.language.as_ref()
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
  /// Sets the container's declared language (consuming builder).
  ///
  /// Pass the tag as the container writes it — see
  /// [`language`](Self::language) for why nothing normalises it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_language(mut self, v: Option<E::Text>) -> Self {
    self.language = v;
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
  /// Sets the container's declared language in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_language(&mut self, v: Option<E::Text>) -> &mut Self {
    self.language = v;
    self
  }
}

// `Debug` is hand-written for the same reason as `TrackParams`'s:
// `#[derive(Debug)]` would add `E: Debug`, but the field that
// actually needs a bound is `E::TrackExtra` — `E::Text: Debug` is
// already guaranteed by `DemuxAdapter::Text`'s own trait bound, and
// `TrackParams<E>` needs no extra bound at all (see its own impl,
// above). No `Clone`: see this type's own doc, above, for the
// message-carrier law.
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
      .field("language", &self.language)
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
#[derive(IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
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
/// # Why `Option` and not [`Received`](crate::Received)
///
/// The decoders' three-state answer has a state this face does not:
/// **needs-input cannot happen to a demuxer.** A demuxer is pulled, not
/// fed — there is no caller-supplied input it could be waiting on, so a
/// `NeedsInput` arm here would be a state no backend can ever produce
/// and every consumer would still have to write a `match` arm for.
/// `Ok(None)` is the same fact in the shape that fits: two states, two
/// values, and the packet itself riding in the `Ok` arm rather than
/// into a `dst`.
///
/// What the two faces share is the law, not the type: **a protocol
/// state never travels in `Err`.** End of file is not a failure here
/// for the same reason [`Received::Ended`](crate::Received::Ended) is
/// not one there.
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
/// # The track table
///
/// [`tracks`](Self::tracks) is a **non-destructive** read, and a
/// session holds its table for its whole life: the rows a caller reads
/// before the first pull are the same rows the session classifies
/// every packet against, and they are still there after end of file.
/// Reading the table therefore has no ordering rule at all — before
/// the first packet, between two of them, after EOF, twice, never.
///
/// The table is *shared*, not handed over. `TrackInfo` has no `Clone`
/// (see its own doc for the message-carrier law), so a caller that
/// needs a row beyond a borrow of `&self` — to fan it out, or simply
/// to hold it across the `&mut self` of the pull loop — clones a
/// [`TrackHandle`](Self::TrackHandle) instead.
///
/// This face used to carry a `take_tracks` that *moved* the table out
/// of the session. It is gone, root and branch: a demuxer that has
/// given its table away can no longer say which track a packet
/// belongs to, and the one backend that implemented it classified
/// against the very `Vec` the move emptied — so following the
/// documented order made every packet in a healthy file vanish. A
/// read that costs the session the state it runs on is not a door
/// worth having.
///
/// # What is not here
///
/// Opening. See the [module docs](self#construction-is-not-on-the-trait).
pub trait Demuxer {
  /// Backend-specific vocabulary.
  type Adapter: DemuxAdapter;
  /// Buffer type held by the packets this session produces.
  type Buffer: AsRef<[u8]>;
  /// A shareable handle on one row of the track table.
  ///
  /// The row is read *through* the handle, and a consumer that needs
  /// to keep a row past a borrow of the session clones the handle.
  /// The backend picks the carrier, the same way it picks
  /// [`Buffer`](Self::Buffer): a heap-backed, thread-crossing backend
  /// binds `Arc<TrackInfo<..>>`, a single-threaded one binds `Rc`,
  /// and one whose rows live in memory it already borrows binds
  /// `&TrackInfo<..>` — which is what keeps this whole tier
  /// allocator-free.
  ///
  /// **`Clone` on a handle must be a refcount bump or a copied
  /// borrow, never a deep copy of the row** — the message-carrier law
  /// [`TrackInfo`] states. An implementor is what upholds it; the
  /// bound is what makes upholding it the path of least resistance,
  /// since `TrackInfo` is not `Clone`, `Box<TrackInfo<_>>` therefore
  /// is not either, and no `#[derive(Clone)]` reaches a carrier that
  /// owns a row outright. A deep copy here would have to be
  /// hand-written against the law.
  type TrackHandle: Clone + Deref<Target = TrackInfo<Self::Adapter>>;
  /// Demuxer-specific error type.
  type Error;

  /// Returns the container's track table.
  ///
  /// Position `i` describes [`TrackIndex::new(i)`](TrackIndex::new) —
  /// the coordinate every [`DemuxedPacket`] carries.
  ///
  /// Non-destructive, and callable whenever: see [the track
  /// table](Self#the-track-table) on the trait.
  fn tracks(&self) -> &[Self::TrackHandle];

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

  // `Vec` is not in the prelude in the alloc-without-std tier (the
  // crate is `#![no_std]` there; the crate-root `alloc`-as-`std` alias
  // only makes `std::`-qualified paths resolve, it does not inject
  // prelude items). `format!` and `Rc` need the same bridge.
  // Unconditional whenever this arm runs: the enclosing `alloc`
  // binding above is gated the same way.
  #[cfg(any(feature = "std", feature = "alloc"))]
  use alloc::{format, rc::Rc, vec, vec::Vec};

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

  /// **The language seat carries the declaration, whatever shape it is
  /// in** — and says nothing where the container said nothing.
  ///
  /// The four tags are the four real spellings a container writes for
  /// one question, and the row keeps each verbatim: an MKV's 639-2/B
  /// against an MP4's 639-2/T, a BCP 47 tag with a script subtag, and
  /// the explicit `und` an ISOBMFF writes for a track nobody tagged.
  /// Folding any pair together is a downstream vocabulary's job, and a
  /// row that folded here would have made the pair indistinguishable
  /// before that vocabulary ever saw them.
  #[test]
  fn the_language_seat_keeps_the_containers_own_spelling() {
    let row = |language| {
      TrackInfo::<Loopback>::new(
        ms_tb(),
        TrackParams::Subtitle(SubtitleTrackParams::new(3)),
        (),
      )
      .with_language(language)
    };

    assert_eq!(
      row(None).language().copied(),
      None,
      "a container that declares no language leaves the seat empty",
    );
    for tag in ["ger", "deu", "zh-Hans", "und"] {
      assert_eq!(
        row(Some(tag)).language().copied(),
        Some(tag),
        "{tag} must survive the row unchanged",
      );
    }
    assert_ne!(
      row(Some("ger")).language().copied(),
      row(Some("deu")).language().copied(),
      "two spellings of one language stay two values here; reconciling them belongs to \
       whoever owns the language registry",
    );
  }

  /// The seat has both mutators the row's other identity fields have,
  /// and the in-place one can take a declaration back off a row.
  #[test]
  fn the_language_seat_can_be_set_and_cleared_in_place() {
    let mut info = TrackInfo::<Loopback>::new(
      ms_tb(),
      TrackParams::Audio(AudioTrackParams::new(2, 48_000, 2, 3, 4)),
      (),
    );
    assert_eq!(info.language().copied(), None);

    info.set_language(Some("jpn"));
    assert_eq!(info.language().copied(), Some("jpn"));

    info.set_language(None);
    assert_eq!(info.language().copied(), None);
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

  // `format!` needs an allocator; see the `Vec`/`format!` import note
  // above `VLoop`.
  #[cfg(any(feature = "std", feature = "alloc"))]
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

  // `format!` needs an allocator; see the `Vec`/`format!` import note
  // above `VLoop`.
  #[cfg(any(feature = "std", feature = "alloc"))]
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

  // `format!` needs an allocator; see the `Vec`/`format!` import note
  // above `VLoop`.
  #[cfg(any(feature = "std", feature = "alloc"))]
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

  #[test]
  fn demuxed_packet_carries_the_derived_accessor_face() {
    // `IsVariant` / `Unwrap` / `TryUnwrap` — one arm per derive family,
    // proving the house accessor face rides the enum rather than
    // asserting the shape of every variant.
    let track = TrackIndex::new(0);
    let video: DemuxedPacket<Loopback, &[u8]> =
      DemuxedPacket::Video(VideoTrackPacket::new(track, VideoPacket::new(&[][..], ())));
    assert!(video.is_video());
    assert!(!video.is_audio());
    assert_eq!(video.unwrap_video_ref().track(), track);
    assert!(video.try_unwrap_audio().is_err());
  }

  /// Trivial loopback session — proves the trait is implementable and
  /// that its associated types resolve through the adapter bundle.
  ///
  /// Binds `Rc` rather than `Arc` at the [`Demuxer::TrackHandle`] seat
  /// on purpose: the seat is a backend's choice, and a single-threaded
  /// session should not be made to pay for atomics. `Vec`-backed, so
  /// the mock and the tests that construct it are gated on the
  /// allocating tier — see the `Vec`/`format!` import note above
  /// `VLoop`. [`BorrowedDemuxer`], below, is the same face with no
  /// allocator at all.
  #[cfg(any(feature = "std", feature = "alloc"))]
  struct LoopDemuxer {
    tracks: Vec<Rc<TrackInfo<Loopback>>>,
    drained: bool,
  }

  #[derive(Debug)]
  struct LoopError;

  #[cfg(any(feature = "std", feature = "alloc"))]
  impl Demuxer for LoopDemuxer {
    type Adapter = Loopback;
    type Buffer = &'static [u8];
    type TrackHandle = Rc<TrackInfo<Loopback>>;
    type Error = LoopError;

    fn tracks(&self) -> &[Rc<TrackInfo<Loopback>>] {
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

  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn the_session_face_is_implementable_and_none_means_eof() {
    fn _accepts<D: Demuxer>() {}
    _accepts::<LoopDemuxer>();

    let mut d = LoopDemuxer {
      tracks: vec![Rc::new(TrackInfo::new(
        ms_tb(),
        TrackParams::Audio(AudioTrackParams::new(1, 48_000, 2, 0, 0)),
        (),
      ))],
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

  /// Reading the table is non-destructive, and a handle taken before
  /// the first pull is still the session's own row after EOF.
  ///
  /// This is the unit half of the regression behind
  /// [issue #51](https://github.com/findit-studio/mediadecode/issues/51):
  /// the face this replaced moved the table out of the session, and
  /// the backend that classified packets against that same table
  /// answered a clean end-of-file to every caller who followed the
  /// documented order.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn the_table_survives_the_session_and_the_handles_stay_the_rows() {
    let mut d = LoopDemuxer {
      tracks: vec![
        Rc::new(TrackInfo::new(
          ms_tb(),
          TrackParams::Audio(AudioTrackParams::new(1, 48_000, 2, 0, 0)),
          (),
        )),
        Rc::new(TrackInfo::new(
          ms_tb(),
          TrackParams::Subtitle(SubtitleTrackParams::new(2)),
          (),
        )),
      ],
      drained: false,
    };

    // The documented order: take the handles first, pull afterwards.
    let held: Vec<Rc<TrackInfo<Loopback>>> = d.tracks().to_vec();
    let expected: Vec<TrackKind> = held.iter().map(|t| t.kind()).collect();
    assert_eq!(expected, vec![TrackKind::Audio, TrackKind::Subtitle]);

    let mut pulled = 0;
    while d.next_packet().expect("pull").is_some() {
      pulled += 1;
      assert_eq!(d.tracks().len(), 2, "the table is not spent by a pull");
    }
    assert_eq!(pulled, 1, "the mock's one packet, not an empty session");

    // After EOF the table is the same table, row for row, and the
    // handles taken before the first pull address those very rows.
    assert_eq!(d.tracks().len(), held.len());
    for (before, after) in held.iter().zip(d.tracks()) {
      assert!(Rc::ptr_eq(before, after), "the same row, not a copy");
    }
    assert_eq!(
      d.tracks().iter().map(|t| t.kind()).collect::<Vec<_>>(),
      expected,
    );
    // The rows the handles address are readable, not dangling.
    assert_eq!(held[0].timebase(), ms_tb());
  }

  /// The same session face with **no allocator**: the row handle is a
  /// borrow, so the demux tier stays `core`-only.
  ///
  /// The seat [`Demuxer::TrackHandle`] opens is the reason this
  /// compiles at all — the table's carrier is the backend's choice,
  /// exactly as [`Demuxer::Buffer`] already was. Deliberately outside
  /// the `alloc` gate: if the tier ever grew a hard dependency on the
  /// heap, this mock would stop compiling first.
  struct BorrowedDemuxer<'a> {
    tracks: &'a [&'a TrackInfo<Loopback>],
    drained: bool,
  }

  impl<'a> Demuxer for BorrowedDemuxer<'a> {
    type Adapter = Loopback;
    type Buffer = &'static [u8];
    type TrackHandle = &'a TrackInfo<Loopback>;
    type Error = LoopError;

    fn tracks(&self) -> &[&'a TrackInfo<Loopback>] {
      self.tracks
    }

    fn next_packet(&mut self) -> Result<Option<DemuxedPacket<Loopback, &'static [u8]>>, LoopError> {
      if self.drained {
        return Ok(None);
      }
      self.drained = true;
      Ok(Some(DemuxedPacket::Subtitle(SubtitleTrackPacket::new(
        TrackIndex::new(0),
        SubtitlePacket::new(&[][..], ()),
      ))))
    }

    fn seek(&mut self, _target: Timestamp) -> Result<(), LoopError> {
      self.drained = false;
      Ok(())
    }
  }

  #[test]
  fn a_borrowed_table_binds_the_handle_seat_without_an_allocator() {
    let row = TrackInfo::<Loopback>::new(
      ms_tb(),
      TrackParams::Subtitle(SubtitleTrackParams::new(7)),
      (),
    );
    let rows = [&row];
    let mut d = BorrowedDemuxer {
      tracks: &rows,
      drained: false,
    };

    let held: &TrackInfo<Loopback> = d.tracks()[0];
    assert!(d.next_packet().expect("pull").is_some());
    assert!(d.next_packet().expect("pull").is_none());
    assert_eq!(d.tracks().len(), 1, "the table outlives the pull loop");
    assert_eq!(held.kind(), TrackKind::Subtitle);
    assert!(core::ptr::eq(held, d.tracks()[0]));
  }
}

//! The timeline and its tracks.

use alloc::{string::String, vec::Vec};

use mediatime::{Rate, Timebase, Timestamp};
use serde::{Deserialize, Serialize};

use crate::{Clip, Metadata, Schema, Transition};

/// An edit: tracks of clips at explicit positions, counted at one edit rate.
///
/// Every position on the timeline — a clip's record, a transition's cut, a
/// fade's length — is counted at [`rate`](Self::rate), from the timeline's
/// zero. [`start`](Self::start) is the timecode that zero carries
/// (`01:00:00:00` is 86 400 frames at 24 fps); it labels the timeline and
/// moves nothing on it.
///
/// The value is plain data: building one checks nothing, and
/// `validate` is where a timeline is judged. Tracks stack
/// in order, the first at the bottom.
///
/// On the wire the document's first field is [`schema`](Self::schema), and
/// a field this reader does not know is refused by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
  schema: Schema,
  name: String,
  rate: Rate,
  start: Timestamp,
  metadata: Metadata,
  tracks: Vec<Track>,
}

impl Timeline {
  /// An empty timeline at the edit rate `rate`, its zero labelled with the
  /// timecode zero.
  ///
  /// The start is counted at `rate`; for a rate of zero, which
  /// `validate` refuses, it is counted in `1/1`.
  pub fn new(name: impl Into<String>, rate: Rate) -> Self {
    let ruler = rate.checked_to_timebase().unwrap_or_default();
    Self {
      schema: Schema::V1,
      name: name.into(),
      rate,
      start: Timestamp::new(0, ruler),
      metadata: Metadata::new(),
      tracks: Vec::new(),
    }
  }

  /// Which shape of document this is.
  pub const fn schema(&self) -> Schema {
    self.schema
  }

  /// The timeline's name.
  pub const fn name(&self) -> &str {
    self.name.as_str()
  }

  /// The edit rate every position on the timeline is counted at.
  pub const fn rate(&self) -> Rate {
    self.rate
  }

  /// The timebase positions are counted in — the edit rate's reciprocal —
  /// or `None` when the rate is zero and has none.
  pub const fn edit_timebase(&self) -> Option<Timebase> {
    self.rate.checked_to_timebase()
  }

  /// The timecode the timeline's zero carries.
  pub const fn start(&self) -> Timestamp {
    self.start
  }

  /// The timeline's own notes.
  pub const fn metadata(&self) -> &Metadata {
    &self.metadata
  }

  /// The timeline's own notes, for editing in place.
  pub const fn metadata_mut(&mut self) -> &mut Metadata {
    &mut self.metadata
  }

  /// The tracks, bottom first.
  pub const fn tracks(&self) -> &[Track] {
    self.tracks.as_slice()
  }

  /// The tracks, for editing in place.
  pub const fn tracks_mut(&mut self) -> &mut Vec<Track> {
    &mut self.tracks
  }

  /// Renames the timeline in place.
  pub fn set_name(&mut self, name: impl Into<String>) -> &mut Self {
    self.name = name.into();
    self
  }

  /// Sets the edit rate in place. Nothing is recounted: positions keep their
  /// counts, and `validate` refuses those not counted at
  /// the new rate.
  pub const fn set_rate(&mut self, rate: Rate) -> &mut Self {
    self.rate = rate;
    self
  }

  /// Sets the timecode the timeline's zero carries (consuming builder).
  #[must_use]
  pub fn with_start(mut self, start: Timestamp) -> Self {
    self.start = start;
    self
  }

  /// Sets the timecode the timeline's zero carries, in place.
  pub const fn set_start(&mut self, start: Timestamp) -> &mut Self {
    self.start = start;
    self
  }

  /// Replaces the timeline's notes (consuming builder).
  #[must_use]
  pub fn with_metadata(mut self, metadata: Metadata) -> Self {
    self.metadata = metadata;
    self
  }

  /// Appends `track` on top of the stack (consuming builder).
  #[must_use]
  pub fn with_track(mut self, track: Track) -> Self {
    self.tracks.push(track);
    self
  }
}

/// One layer of a timeline: clips of one kind of medium, in record order,
/// and the transitions between them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
  kind: TrackKind,
  name: String,
  enabled: bool,
  clips: Vec<Clip>,
  transitions: Vec<Transition>,
}

impl Track {
  /// An empty, enabled track of `kind`.
  pub fn new(kind: TrackKind, name: impl Into<String>) -> Self {
    Self {
      kind,
      name: name.into(),
      enabled: true,
      clips: Vec::new(),
      transitions: Vec::new(),
    }
  }

  /// The kind of medium the track carries.
  pub const fn kind(&self) -> TrackKind {
    self.kind
  }

  /// The track's name.
  pub const fn name(&self) -> &str {
    self.name.as_str()
  }

  /// Whether the track plays. A disabled track keeps its clips and is
  /// skipped.
  pub const fn enabled(&self) -> bool {
    self.enabled
  }

  /// The clips, in record order.
  pub const fn clips(&self) -> &[Clip] {
    self.clips.as_slice()
  }

  /// The clips, for editing in place.
  pub const fn clips_mut(&mut self) -> &mut Vec<Clip> {
    &mut self.clips
  }

  /// The transitions between adjacent clips.
  pub const fn transitions(&self) -> &[Transition] {
    self.transitions.as_slice()
  }

  /// The transitions, for editing in place.
  pub const fn transitions_mut(&mut self) -> &mut Vec<Transition> {
    &mut self.transitions
  }

  /// Renames the track in place.
  pub fn set_name(&mut self, name: impl Into<String>) -> &mut Self {
    self.name = name.into();
    self
  }

  /// Sets whether the track plays (consuming builder).
  #[must_use]
  pub fn with_enabled(mut self, enabled: bool) -> Self {
    self.enabled = enabled;
    self
  }

  /// Sets whether the track plays, in place.
  pub const fn set_enabled(&mut self, enabled: bool) -> &mut Self {
    self.enabled = enabled;
    self
  }

  /// Appends `clip` (consuming builder).
  #[must_use]
  pub fn with_clip(mut self, clip: Clip) -> Self {
    self.clips.push(clip);
    self
  }

  /// Appends `transition` (consuming builder).
  #[must_use]
  pub fn with_transition(mut self, transition: Transition) -> Self {
    self.transitions.push(transition);
    self
  }
}

/// The kind of medium a track carries.
///
/// Marked `#[non_exhaustive]`: a later kind joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TrackKind {
  /// Pictures.
  Video,
  /// Sound.
  Audio,
}

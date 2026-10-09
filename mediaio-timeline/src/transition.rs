//! A transition between two adjacent clips.

use mediatime::{Duration, Timestamp};
use serde::{Deserialize, Serialize};

/// A blend across the cut between two adjacent clips of one track.
///
/// [`at`](Self::at) is the cut: the instant where one clip's record ends and
/// the next one's begins. The blend runs from [`in_offset`](Self::in_offset)
/// before the cut to [`out_offset`](Self::out_offset) after it, all counted
/// at the edit rate. The records still abut at the cut — a transition never
/// makes two records overlap. Across the blend each clip plays media its
/// record does not cover, its **handle**:
///
/// - the outgoing clip plays on for `out_offset` past its source range's end;
/// - the incoming clip starts `in_offset` before its source range's start.
///
/// These are OpenTimelineIO's `in_offset` and `out_offset`, with the same
/// meaning. [`validate`](fn@crate::validate) holds each handle inside its
/// clip's available media when that is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
  kind: TransitionKind,
  #[serde(deserialize_with = "crate::wire::timestamp")]
  at: Timestamp,
  #[serde(deserialize_with = "crate::wire::duration")]
  in_offset: Duration,
  #[serde(deserialize_with = "crate::wire::duration")]
  out_offset: Duration,
}

impl Transition {
  /// A transition of `kind` across the cut at `at`, reaching `in_offset`
  /// before it and `out_offset` after it.
  pub const fn new(
    kind: TransitionKind,
    at: Timestamp,
    in_offset: Duration,
    out_offset: Duration,
  ) -> Self {
    Self {
      kind,
      at,
      in_offset,
      out_offset,
    }
  }

  /// A dissolve across the cut at `at`.
  pub const fn dissolve(at: Timestamp, in_offset: Duration, out_offset: Duration) -> Self {
    Self::new(TransitionKind::Dissolve, at, in_offset, out_offset)
  }

  /// What kind of blend this is.
  pub const fn kind(&self) -> TransitionKind {
    self.kind
  }

  /// The cut the transition straddles, on the timeline.
  pub const fn at(&self) -> Timestamp {
    self.at
  }

  /// How far before the cut the blend begins.
  pub const fn in_offset(&self) -> Duration {
    self.in_offset
  }

  /// How far after the cut the blend ends.
  pub const fn out_offset(&self) -> Duration {
    self.out_offset
  }
}

/// The kind of blend a [`Transition`] performs.
///
/// Marked `#[non_exhaustive]`: a later kind (a wipe) joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TransitionKind {
  /// A cross-dissolve: the outgoing picture or sound gives way to the
  /// incoming one across the blend.
  Dissolve,
}

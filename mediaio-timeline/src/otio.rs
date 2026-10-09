//! OpenTimelineIO: export, and the structural self-check.
//!
//! [`to_otio`] writes a valid timeline as OpenTimelineIO JSON for the
//! readers an [`OtioTarget`] names:
//!
//! | model | OpenTimelineIO |
//! |---|---|
//! | [`Timeline`] | `Timeline.1`; `global_start_time` is always written, from [`Timeline::start`], at the edit rate |
//! | its tracks | one `Stack.1` named `tracks`, bottom track first |
//! | [`Track`](crate::Track) | `Track.1`, `kind` `Video` or `Audio`, `enabled` |
//! | [`Clip`](crate::Clip) | `Clip.2` under [`OtioTarget::V0_15Plus`], `Clip.1` under [`OtioTarget::Legacy`]; `enabled` |
//! | its trim | `source_range`: the source range whole, `start_time` and `duration` in the medium's one ruler — by validation, a duration as long as the record |
//! | [`MediaRef`](crate::MediaRef) | `ExternalReference.1` (under `DEFAULT_MEDIA` in `Clip.2`): `target_url` is the locator, `available_range` the available range |
//! | a gap between records | `Gap.1` — derived by [`layout`](fn@crate::layout), never stored |
//! | [`Transition`](crate::Transition) | `Transition.1`, `SMPTE_Dissolve`, its offsets at the edit rate |
//! | a [`Fade`](crate::Fade) | `Transition.1`, `SMPTE_Dissolve` against a gap — a gap of no length where the clip abuts a clip or a track's end |
//! | a clip's [`id`](crate::Clip::id), gain, reel, the medium's rate, a fade's curve, [`Metadata`](crate::Metadata) | `metadata.mediaio`: `id`, `gain_db`, `reel`, `rate`, `fade` and `shape`, `metadata` |
//!
//! Every position on the record side is a whole count at the edit rate. A
//! media-side range (a source range, an available range) is written whole in
//! one ruler: frames of the medium's stated rate when its start and length
//! both land on one, and otherwise ticks of its own timebase.
//!
//! **Every count OpenTimelineIO reads or derives is one it holds exactly.**
//! A `RationalTime` keeps its value in an `f64`, which holds every whole
//! number up to 2^53 and only some past it, so a larger count may be read
//! rounded — a clip placed early, a different stretch of media played. And
//! OpenTimelineIO derives a document's positions in that arithmetic: a sum
//! of two times is carried in the higher of their rates, the other rescaled
//! into it, two rounded operations. So the export holds two things:
//!
//! - every count it writes lies within ±2^53, and a rate is written as the
//!   `f64` nearest it, spelled so it reads back as that very `f64`; a
//!   media-side range whose counts are past 2^53 in both rulers above is
//!   written in the coarsest ruler of a whole number of ticks a second that
//!   holds its start and its length exactly, when its counts there are
//!   within 2^53;
//! - every count OpenTimelineIO derives from the document — each child's
//!   place on its track, from zero in the child's own rate with every item
//!   before it added, and in the timeline; the running end of one walk over
//!   a whole track; each item's visible range, widened by the handles of
//!   the transitions beside it; each track's duration and the stack's, the
//!   longest of them as OpenTimelineIO picks it; the global start added to
//!   each place and each track's end; and the last tick of every range among
//!   them, `end_time_inclusive`, which floors the range's end or takes a
//!   tick off it by a branch OpenTimelineIO takes on its own doubles — is
//!   computed as OpenTimelineIO computes it, operation for operation and
//!   branch for branch, beside its exact value, and must lie within ±2^53
//!   in the ruler OpenTimelineIO carries it in, with OpenTimelineIO's
//!   double less than half a tick from the exact count. A range written in
//!   one ruler ends inclusively where it exactly does: its counts whole,
//!   OpenTimelineIO's arithmetic on them is exact.
//!
//! Where OpenTimelineIO's arithmetic would round a value, the export
//! searches the rulers the clips it is formed from may be written in: each
//! whole number of ticks a second that holds a clip's source range exactly,
//! from the ruler above down to the coarsest such, at most 64 a clip —
//! moving one clip one ruler coarser at a time, the one with the finest
//! ruler first, the coarsest plan last. What no plan it tries holds,
//! [`to_otio`] refuses ([`Refused::NotRepresentable`], naming the value the
//! last plan could not hold): it never writes a document OpenTimelineIO
//! would read rounded.
//!
//! A source range's length is a whole number of edit-rate ticks (validation
//! refuses one that is not), so laying a track's items end to end puts every
//! clip on its record. Where a medium's ruler is not the edit rate, one
//! track's items are counted in more than one rate, and OpenTimelineIO sums
//! those in floating point: a position derived that way is read to the
//! nearest frame, not truncated — held, as above, under half a tick of the
//! exact count.
//!
//! OpenTimelineIO has no word for a clip's id, its gain or a reel, so all
//! three ride in `metadata` — the id so that a reader can tell each clip
//! again, its name and medium being free to repeat. An application that does
//! not read it — DaVinci Resolve among them — applies neither gain nor reel.
//! Available ranges and the start are written as the
//! timeline holds them: a medium read through `mediadecode` starts at zero
//! until the read side exposes the container's timecode, so its available
//! range does not yet carry the camera's timecode.
//!
//! [`validate_json`] reads a document back with the crate's own JSON reader
//! and checks OpenTimelineIO's schema shape — no Python, no OpenTimelineIO
//! install. Reading OpenTimelineIO back into a [`Timeline`] (`from_otio`) is
//! a later row; this module is its home.

use alloc::{string::String, vec::Vec};
use core::fmt;

use mediatime::Rate;

use crate::{ClipAt, EdgeAt, Refusal, Timeline, TransitionAt, layout::layout_valid, validate};

mod check;
mod derive;
mod export;
mod json;

/// Which OpenTimelineIO readers a document is written for.
///
/// Marked `#[non_exhaustive]`: a later target joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OtioTarget {
  /// OpenTimelineIO 0.15 and later: a clip is `Clip.2`, its medium under
  /// `media_references`.
  V0_15Plus,
  /// Readers from before OpenTimelineIO 0.15: a clip is `Clip.1`, with one
  /// `media_reference`.
  Legacy,
}

/// Writes `timeline` as OpenTimelineIO JSON for `target`, indented four
/// spaces a level as OpenTimelineIO's own writer indents, ending in a
/// newline.
///
/// # Errors
///
/// - [`Refused::Validation`], with its refusals, for a timeline that does
///   not [`validate`](fn@validate): its records could not be laid end to
///   end, which is how OpenTimelineIO places items.
/// - [`Refused::NotRepresentable`] for a count OpenTimelineIO would read
///   or derive rounded in every plan of rulers the export tries: one past
///   2^53, where its `f64` no longer holds every whole number, or one its
///   arithmetic would land half a tick off or more (see [the module's
///   docs](self)).
pub fn to_otio(timeline: &Timeline, target: OtioTarget) -> Result<String, Refused> {
  validate(timeline).map_err(Refused::Validation)?;
  let tree = export::timeline(timeline, &layout_valid(timeline), target)
    .map_err(Refused::NotRepresentable)?;
  let mut out = String::new();
  json::write_pretty(&tree, &mut out);
  out.push('\n');
  Ok(out)
}

/// Why [`to_otio`] wrote nothing.
///
/// Marked `#[non_exhaustive]`: a later reason joins as a variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refused {
  /// The timeline does not [`validate`](fn@validate): its refusals, as
  /// `validate` answers them.
  Validation(Vec<Refusal>),
  /// A count OpenTimelineIO would read or derive rounded in every plan of
  /// rulers the export tries — past 2^53, where its `f64` no longer holds
  /// every whole number, or landed half a tick off or more by its
  /// arithmetic: the count the last plan tried could not hold.
  NotRepresentable(NotRepresentable),
}

/// Writes `the timeline does not validate: …` with each refusal, or the
/// count that is not representable.
impl fmt::Display for Refused {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Validation(refusals) => {
        f.write_str("the timeline does not validate")?;
        for (index, refusal) in refusals.iter().enumerate() {
          f.write_str(if index == 0 { ": " } else { "; " })?;
          fmt::Display::fmt(refusal, f)?;
        }
        Ok(())
      }
      Self::NotRepresentable(count) => fmt::Display::fmt(count, f),
    }
  }
}

impl core::error::Error for Refused {}

/// A count OpenTimelineIO would not hold exactly: what
/// [`Refused::NotRepresentable`] carries.
///
/// The count is one the export would write, or one OpenTimelineIO derives
/// from them in its own `f64` arithmetic — a range's end or its last tick,
/// a child's place on its track or in the timeline, a visible range, a
/// track's or the stack's duration, a place counted from the global start.
/// Past 2^53 an `f64` no longer holds every whole number; a derived count
/// can also come out of OpenTimelineIO's rescaling, or its branch on a
/// rescaled double, half a tick off or more. Either way a reader would get,
/// or add up from it, a rounded time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NotRepresentable {
  at: Spot,
  value: i128,
  rate: Rate,
}

impl NotRepresentable {
  /// Where the count sits.
  pub const fn at(&self) -> Spot {
    self.at
  }

  /// The count, in ticks of [`rate`](Self::rate).
  ///
  /// A count the export would write is the count as the timeline holds it:
  /// on the record side ticks of the edit rate; on the media side ticks of
  /// the range's own timebase — its start, its length or its end, the first
  /// past 2^53 — for a range no ruler the export may write it in holds.
  /// A count OpenTimelineIO derives is counted, exactly and to the nearest
  /// tick, in the ruler OpenTimelineIO derives it in: the highest rate among
  /// the times it adds up. A last tick is the exact one — the range's exact
  /// end floored, or less one tick, as the exact duration is fractional or
  /// whole.
  pub const fn value(&self) -> i128 {
    self.value
  }

  /// The rate [`value`](Self::value) counts ticks of.
  pub const fn rate(&self) -> Rate {
    self.rate
  }
}

/// Writes `the place of track 0, child 1 counts 27021597764222973 ticks of
/// 1/3 s, which OpenTimelineIO's f64 does not hold exactly`.
impl fmt::Display for NotRepresentable {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{} counts {} ticks", self.at, self.value)?;
    if let Some(tick) = self.rate.checked_to_timebase() {
      write!(f, " of {tick} s")?;
    }
    f.write_str(", which OpenTimelineIO's f64 does not hold exactly")
  }
}

/// Where in a timeline a count [`to_otio`] would write, or OpenTimelineIO
/// would derive, sits: what [`NotRepresentable`] names.
///
/// Marked `#[non_exhaustive]`: a later counted word joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Spot {
  /// The timeline's start: `global_start_time`.
  Start,
  /// The gap before a clip: its length, written at the edit rate.
  Record(ClipAt),
  /// A clip's source range: its start, its length or its end.
  Source(ClipAt),
  /// The available range of a clip's medium: its start, its length or its
  /// end.
  Available(ClipAt),
  /// A transition's offsets.
  Transition(TransitionAt),
  /// A clip's fade at one edge.
  Fade(EdgeAt),
  /// A child's place as OpenTimelineIO derives it: where it starts or ends
  /// on its track — exclusively, or at its last tick, `end_time_inclusive`
  /// — a sum of the items before it on the way there, the same in the
  /// timeline, or where it starts or ends in one walk over the whole track.
  TrackPosition(ChildAt),
  /// A track's duration, as OpenTimelineIO sums it, or the last tick of the
  /// track's range, which runs that duration from zero; the stack's
  /// duration is the longest of them, and its range the longest track's.
  TrackDuration(usize),
  /// The stack's duration, which OpenTimelineIO picks by comparing the
  /// tracks' durations in `f64`: a shorter track picked where two compare
  /// equal. [`NotRepresentable::value`] is the longest track's duration.
  StackDuration,
  /// An item's visible range: its source range widened by the handles of
  /// the transitions beside it — its start, its duration, its end or its
  /// last tick.
  Visible(ChildAt),
  /// A child's start or end in the timeline, counted from the global start.
  Absolute(ChildAt),
  /// A track's end counted from the global start — exclusively or at its
  /// last tick; the timeline's end, for the longest track.
  AbsoluteEnd(usize),
}

/// Writes `the start`, `the place of track 0, child 2`, and so on.
impl fmt::Display for Spot {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Start => f.write_str("the start"),
      Self::Record(at) => write!(f, "the gap before {at}"),
      Self::Source(at) => write!(f, "the source range of {at}"),
      Self::Available(at) => write!(f, "the available range of {at}"),
      Self::Transition(at) => write!(f, "{at}"),
      Self::Fade(at) => write!(f, "the fade at the {} of {}", at.edge().side(), at.clip()),
      Self::TrackPosition(at) => write!(f, "the place of {at}"),
      Self::TrackDuration(track) => write!(f, "the duration of track {track}"),
      Self::StackDuration => f.write_str("the stack's duration"),
      Self::Visible(at) => write!(f, "the visible range of {at}"),
      Self::Absolute(at) => write!(f, "the place of {at} from the global start"),
      Self::AbsoluteEnd(track) => write!(f, "the end of track {track} from the global start"),
    }
  }
}

/// One child of an exported track, by the index of its track in the
/// timeline and its index among the track's OpenTimelineIO children —
/// items and transitions, in the order they are written: gaps, clips, and
/// a dissolve or a fade's transition between two items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChildAt {
  track: usize,
  child: usize,
}

impl ChildAt {
  /// Child `child` of track `track`.
  pub const fn new(track: usize, child: usize) -> Self {
    Self { track, child }
  }

  /// The track's index in the timeline.
  pub const fn track(&self) -> usize {
    self.track
  }

  /// The child's index among the track's OpenTimelineIO children.
  pub const fn child(&self) -> usize {
    self.child
  }
}

/// Writes `track 0, child 2`.
impl fmt::Display for ChildAt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "track {}, child {}", self.track, self.child)
  }
}

/// Checks that `json` is an OpenTimelineIO timeline `target`'s readers
/// read: well-formed JSON, every object of a schema OpenTimelineIO defines,
/// with exactly the keys those readers require — audited against each
/// schema's `read_from`, per target — the types they read for the keys the
/// export writes, and every transition between two items.
///
/// This is the structural self-check the export is held to — the shape, not
/// the meaning: it does not recompute positions.
pub fn validate_json(json: &str, target: OtioTarget) -> Result<(), Invalid> {
  let root = json::parse(json).map_err(Invalid::Syntax)?;
  check::timeline(&root, target).map_err(Invalid::Shape)
}

/// Why [`validate_json`] refused a document.
///
/// Marked `#[non_exhaustive]`: a later kind of refusal joins as a variant.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Invalid {
  /// The text is not one well-formed JSON value.
  Syntax(Syntax),
  /// The JSON is not the shape OpenTimelineIO reads.
  Shape(Shape),
}

impl fmt::Display for Invalid {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Syntax(syntax) => fmt::Display::fmt(syntax, f),
      Self::Shape(shape) => fmt::Display::fmt(shape, f),
    }
  }
}

impl core::error::Error for Invalid {}

/// Where the JSON grammar was broken, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Syntax {
  offset: usize,
  reason: &'static str,
}

impl Syntax {
  /// The byte offset at which the text stopped being JSON.
  pub const fn offset(&self) -> usize {
    self.offset
  }

  /// What was wrong there.
  pub const fn reason(&self) -> &'static str {
    self.reason
  }
}

/// Writes `byte 12: expected a value`.
impl fmt::Display for Syntax {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "byte {}: {}", self.offset, self.reason)
  }
}

/// Where the document left OpenTimelineIO's shape, and what was expected
/// there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
  path: String,
  expected: &'static str,
}

impl Shape {
  /// Where, as a path from the document's root: `$.tracks.children[0]`.
  pub fn path(&self) -> &str {
    &self.path
  }

  /// What OpenTimelineIO reads there.
  pub const fn expected(&self) -> &'static str {
    self.expected
  }
}

/// Writes `$.tracks.children[0].kind: expected a string`.
impl fmt::Display for Shape {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: expected {}", self.path, self.expected)
  }
}

impl core::error::Error for Shape {}

#[cfg(test)]
mod tests;

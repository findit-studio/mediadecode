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
//! **Every count written is one OpenTimelineIO holds exactly.** A
//! `RationalTime` keeps its value in an `f64`, which holds every whole number
//! up to 2^53 and only some past it, so a larger count may be read rounded —
//! a clip placed early, a different stretch of media played. Every
//! value written lies within ±2^53, and so does the end OpenTimelineIO
//! derives from each range — its start rescaled to its duration's rate, plus
//! the duration, which is then the range's own end — and each place a record
//! starts or ends on its track. A rate is written as the `f64` nearest it,
//! spelled so it reads back as that very `f64`. A media-side range whose
//! counts are past 2^53 in both rulers above is written in the coarsest
//! ruler of a whole number of ticks a second that holds its start and its
//! length exactly, when its counts there are within 2^53. The record side has
//! no other ruler: OpenTimelineIO adds a track's items up at the edit rate,
//! where a count past 2^53 stays past it. What no ruler holds,
//! [`to_otio`] refuses ([`Refused::NotRepresentable`]): it never writes a
//! rounded document.
//!
//! A source range's length is a whole number of edit-rate ticks (validation
//! refuses one that is not), so laying a track's items end to end puts every
//! clip on its record. Where a medium's ruler is not the edit rate, one
//! track's items are counted in more than one rate, and OpenTimelineIO sums
//! those in floating point: a position derived that way is read to the
//! nearest frame, not truncated.
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

use crate::{ClipAt, EdgeAt, Refusal, Timeline, TransitionAt, layout::layout_valid, validate};

mod check;
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
/// - [`Refused::NotRepresentable`] for a count no ruler the export may write
///   it in holds within 2^53, where OpenTimelineIO's `f64` would read it
///   rounded (see [the module's docs](self)).
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
  /// A count past 2^53 in every ruler the export may write it in, where
  /// OpenTimelineIO's `f64` no longer holds every whole number.
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

/// A count past 2^53: what [`Refused::NotRepresentable`] carries.
///
/// The count is one the export would write, or one OpenTimelineIO would
/// derive from them — a range's end, the place a record starts or ends on
/// its track. Past 2^53 an `f64` no longer holds every whole number, so
/// OpenTimelineIO would read it, or what it adds up from it, rounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NotRepresentable {
  at: Spot,
  value: i128,
}

impl NotRepresentable {
  /// Where the count sits.
  pub const fn at(&self) -> Spot {
    self.at
  }

  /// The count past 2^53, as the timeline holds it: on the record side,
  /// ticks of the edit rate; on the media side, ticks of the range's own
  /// timebase — its start, its length or its end, the first past 2^53 — for
  /// a range no ruler the export may write it in holds.
  pub const fn value(&self) -> i128 {
    self.value
  }
}

/// Writes `the record of track 0, clip 0 counts 9007199254740993 ticks: past
/// 2^53, where OpenTimelineIO's f64 no longer holds every whole number`.
impl fmt::Display for NotRepresentable {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{} counts {} ticks: past 2^53, where OpenTimelineIO's f64 no longer holds every whole number",
      self.at, self.value
    )
  }
}

/// Where in a timeline a count [`to_otio`] would write sits: what
/// [`NotRepresentable`] names.
///
/// Marked `#[non_exhaustive]`: a later counted word joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Spot {
  /// The timeline's start: `global_start_time`.
  Start,
  /// A clip's place on its track: where its record starts or ends, or the
  /// gap before it.
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
}

/// Writes `the start`, `the record of track 0, clip 2`, and so on.
impl fmt::Display for Spot {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Start => f.write_str("the start"),
      Self::Record(at) => write!(f, "the record of {at}"),
      Self::Source(at) => write!(f, "the source range of {at}"),
      Self::Available(at) => write!(f, "the available range of {at}"),
      Self::Transition(at) => write!(f, "{at}"),
      Self::Fade(at) => write!(f, "the fade at the {} of {}", at.edge().side(), at.clip()),
    }
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

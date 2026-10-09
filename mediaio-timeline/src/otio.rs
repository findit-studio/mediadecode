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
//! | its trim | `source_range`: `duration` is the record's length at the edit rate, `start_time` the source range's start |
//! | [`MediaRef`](crate::MediaRef) | `ExternalReference.1` (under `DEFAULT_MEDIA` in `Clip.2`): `target_url` is the locator, `available_range` the available range |
//! | a gap between records | `Gap.1` — derived by [`layout`](fn@crate::layout), never stored |
//! | [`Transition`](crate::Transition) | `Transition.1`, `SMPTE_Dissolve`, its offsets at the edit rate |
//! | a [`Fade`](crate::Fade) | `Transition.1`, `SMPTE_Dissolve` against a gap — a gap of no length where the clip abuts a clip or a track's end |
//! | gain, reel, the medium's rate, a fade's curve, [`Metadata`](crate::Metadata) | `metadata.mediaio`: `gain_db`, `reel`, `rate`, `fade` and `shape`, `metadata` |
//!
//! Every position on the record side is a whole count at the edit rate. A
//! media-side time (a source start, an available range) is written in
//! frames of the medium's stated rate when it lands on one, and otherwise in
//! ticks of its own timebase, so every number written is exact.
//!
//! OpenTimelineIO has no word for gain or a reel, so both ride in
//! `metadata`; an application that does not read it — DaVinci Resolve among
//! them — applies neither. Available ranges and the start are written as the
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

use crate::{Refusal, Timeline, layout::layout_valid, validate};

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
/// A timeline that does not [`validate`](fn@validate) is refused with its refusals: its
/// records could not be laid end to end, which is how OpenTimelineIO places
/// items.
pub fn to_otio(timeline: &Timeline, target: OtioTarget) -> Result<String, Vec<Refusal>> {
  validate(timeline)?;
  let tree = export::timeline(timeline, &layout_valid(timeline), target);
  let mut out = String::new();
  json::write_pretty(&tree, &mut out);
  out.push('\n');
  Ok(out)
}

/// Checks that `json` is an OpenTimelineIO timeline `target`'s readers
/// read: well-formed JSON, every object of a schema OpenTimelineIO defines,
/// with the keys its reader requires and the types it reads, and every
/// transition between two items.
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

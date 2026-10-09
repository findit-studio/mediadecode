//! What changed between two timelines, clip by clip.

use alloc::{
  collections::{BTreeMap, btree_map::Entry},
  vec::Vec,
};
use core::{cmp::Ordering, fmt};

use mediatime::{TimeRange, Timestamp};

use crate::{Clip, ClipPair, Fade, Fades, Timeline};

/// What changed from `before` to `after`, clip by clip, per track.
///
/// Tracks are matched by index. Within a track a clip is identified by its
/// [`name`](Clip::name) together with its medium's
/// [`locator`](crate::MediaRef::locator), and the identity must be unique in
/// its track for a match to mean anything. A track of either side that names
/// one identity twice is refused with [`Ambiguous`] rather than matched by
/// occurrence, which would report a clip that stayed put as moved and one
/// that stayed as removed; [`validate`](fn@crate::validate) refuses such a
/// track too ([`Refusal::DuplicateClip`](crate::Refusal::DuplicateClip)). A
/// stable `id` word is reserved for a later schema.
///
/// A clip only in `after` is [`Added`](ChangeKind::Added); one only in
/// `before` is [`Removed`](ChangeKind::Removed). A clip in both is reported
/// once for each way it changed:
///
/// - [`Moved`](ChangeKind::Moved) — its record covers a different stretch
///   of the timeline. Positions are explicit, so a clip moves only when its
///   own record changed, never because a clip before it was trimmed;
/// - [`Retimed`](ChangeKind::Retimed) — its source range covers a different
///   stretch of its medium;
/// - [`Regained`](ChangeKind::Regained) — its gain or its fades changed;
/// - [`EnabledFlipped`](ChangeKind::EnabledFlipped) — it was enabled and is
///   disabled, or the other way round.
///
/// Ranges and lengths compare by the time they cover, so a range recounted
/// in another timebase over the same instants is not a change.
///
/// The changes come in a fixed order: by track, then by where the clip sits
/// — its record in `after`, or in `before` for a removed clip — then by kind
/// in the order above, then by index. So `diff(a, a)` is empty for every `a`
/// it answers, and every run over the same pair answers the same list.
///
/// Only clips are compared. The timeline's own words (name, rate, start,
/// notes), a track's words (kind, name, enabled), transitions, a clip's
/// notes and its medium's words other than the locator are not.
///
/// ```
/// use core::num::NonZeroI32;
///
/// use mediaio_timeline::{ChangeKind, Clip, MediaRef, Rate, TimeRange, Timebase, Timeline, Track, TrackKind, diff};
///
/// let edit = Timebase::new(1, NonZeroI32::new(25).unwrap());
/// let at = |name: &str, start: i64| {
///   Clip::new(name, MediaRef::new(name), TimeRange::new(0, 10, edit), TimeRange::new(start, start + 10, edit))
/// };
/// let before = Timeline::new("t", Rate::FPS_25).with_track(
///   Track::new(TrackKind::Video, "V").with_clip(at("a", 0)).with_clip(at("b", 10)),
/// );
/// let mut after = before.clone();
/// after.tracks_mut()[0].clips_mut()[1].set_record(TimeRange::new(20, 30, edit));
/// let delta = diff(&before, &after).unwrap();
/// assert_eq!(delta.changes().len(), 1);
/// assert_eq!(delta.changes()[0].kind(), ChangeKind::Moved);
/// assert_eq!(delta.changes()[0].after(), Some(1));
/// assert!(diff(&after, &after).unwrap().is_empty());
/// ```
pub fn diff(before: &Timeline, after: &Timeline) -> Result<Delta, Ambiguous> {
  for (side, timeline) in [(Side::Before, before), (Side::After, after)] {
    for (index, track) in timeline.tracks().iter().enumerate() {
      if let Some((earlier, later)) = first_duplicate(track.clips()) {
        return Err(Ambiguous {
          side,
          clips: ClipPair::new(index, earlier, later),
        });
      }
    }
  }
  let mut changes = Vec::new();
  let tracks = before.tracks().len().max(after.tracks().len());
  for track in 0..tracks {
    let clips_before = before
      .tracks()
      .get(track)
      .map_or(&[][..], |track| track.clips());
    let clips_after = after
      .tracks()
      .get(track)
      .map_or(&[][..], |track| track.clips());
    diff_track(track, clips_before, clips_after, &mut changes);
  }
  changes.sort_by_cached_key(|change| {
    (
      change.track,
      change.starts_at(before, after),
      rank(change.kind),
      change.before,
      change.after,
    )
  });
  Ok(Delta { changes })
}

/// The first clip of `clips` with an earlier clip's identity, and the
/// earlier clip: `(earlier, later)`.
fn first_duplicate(clips: &[Clip]) -> Option<(usize, usize)> {
  let mut first = BTreeMap::new();
  for (index, clip) in clips.iter().enumerate() {
    match first.entry(clip.identity()) {
      Entry::Vacant(slot) => {
        slot.insert(index);
      }
      Entry::Occupied(slot) => return Some((*slot.get(), index)),
    }
  }
  None
}

/// Matches `before`'s clips with `after`'s by identity, unique on each side.
fn diff_track(track: usize, before: &[Clip], after: &[Clip], out: &mut Vec<Change>) {
  // Each identity's clip in `after`, waiting to be matched.
  let mut waiting: BTreeMap<(&str, &str), usize> = after
    .iter()
    .enumerate()
    .map(|(index, clip)| (clip.identity(), index))
    .collect();
  let mut matched = alloc::vec![false; after.len()];
  for (index, clip) in before.iter().enumerate() {
    let Some(partner) = waiting.remove(&clip.identity()) else {
      out.push(Change::new(track, ChangeKind::Removed, Some(index), None));
      continue;
    };
    matched[partner] = true;
    let then = clip;
    let now = &after[partner];
    let pair = |kind| Change::new(track, kind, Some(index), Some(partner));
    if !same_span(then.record(), now.record()) {
      out.push(pair(ChangeKind::Moved));
    }
    if !same_span(then.source_range(), now.source_range()) {
      out.push(pair(ChangeKind::Retimed));
    }
    if then.gain() != now.gain() || !same_fades(then.fades(), now.fades()) {
      out.push(pair(ChangeKind::Regained));
    }
    if then.enabled() != now.enabled() {
      out.push(pair(ChangeKind::EnabledFlipped));
    }
  }
  for (index, matched) in matched.into_iter().enumerate() {
    if !matched {
      out.push(Change::new(track, ChangeKind::Added, None, Some(index)));
    }
  }
}

/// Whether two ranges cover the same stretch of time, whatever they are
/// counted in.
fn same_span(a: TimeRange, b: TimeRange) -> bool {
  a.start() == b.start() && a.end() == b.end()
}

fn same_fades(a: Fades, b: Fades) -> bool {
  same_fade(a.in_(), b.in_()) && same_fade(a.out(), b.out())
}

fn same_fade(a: Option<Fade>, b: Option<Fade>) -> bool {
  match (a, b) {
    (None, None) => true,
    (Some(a), Some(b)) => {
      a.shape() == b.shape() && a.duration().cmp_semantic(&b.duration()) == Ordering::Equal
    }
    _ => false,
  }
}

const fn rank(kind: ChangeKind) -> u8 {
  match kind {
    ChangeKind::Added => 0,
    ChangeKind::Removed => 1,
    ChangeKind::Moved => 2,
    ChangeKind::Retimed => 3,
    ChangeKind::Regained => 4,
    ChangeKind::EnabledFlipped => 5,
  }
}

/// Why [`diff`] refused: a track of one side names one clip identity — a
/// name and a locator — twice, so a clip of the other side could be matched
/// with either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ambiguous {
  side: Side,
  clips: ClipPair,
}

impl Ambiguous {
  /// The timeline whose track names the identity twice.
  pub const fn side(&self) -> Side {
    self.side
  }

  /// The track and its first two clips with the identity.
  pub const fn clips(&self) -> ClipPair {
    self.clips
  }
}

/// Writes `before, track 0: clips 0 and 1 share a name and a locator`.
impl fmt::Display for Ambiguous {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{}, track {}: clips {} and {} share a name and a locator",
      match self.side {
        Side::Before => "before",
        Side::After => "after",
      },
      self.clips.track(),
      self.clips.earlier(),
      self.clips.later()
    )
  }
}

impl core::error::Error for Ambiguous {}

/// One of the two timelines [`diff`] compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
  /// The timeline before the change.
  Before,
  /// The timeline after it.
  After,
}

/// The changes [`diff`] found, in its fixed order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Delta {
  changes: Vec<Change>,
}

impl Delta {
  /// The changes, by track, then by where the clip sits, then by kind.
  pub fn changes(&self) -> &[Change] {
    &self.changes
  }

  /// Whether nothing [`diff`] compares changed.
  pub fn is_empty(&self) -> bool {
    self.changes.is_empty()
  }
}

/// One way one clip changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Change {
  track: usize,
  kind: ChangeKind,
  before: Option<usize>,
  after: Option<usize>,
}

impl Change {
  const fn new(
    track: usize,
    kind: ChangeKind,
    before: Option<usize>,
    after: Option<usize>,
  ) -> Self {
    Self {
      track,
      kind,
      before,
      after,
    }
  }

  /// The track's index, in both timelines.
  pub const fn track(&self) -> usize {
    self.track
  }

  /// How the clip changed.
  pub const fn kind(&self) -> ChangeKind {
    self.kind
  }

  /// The clip's index in the track before, or `None` for an added clip.
  pub const fn before(&self) -> Option<usize> {
    self.before
  }

  /// The clip's index in the track after, or `None` for a removed clip.
  pub const fn after(&self) -> Option<usize> {
    self.after
  }

  /// The instant the clip starts at: after the change, or before it for a
  /// removed clip.
  fn starts_at(&self, before: &Timeline, after: &Timeline) -> Option<Timestamp> {
    let (timeline, index) = match self.after {
      Some(index) => (after, index),
      None => (before, self.before?),
    };
    let clip = timeline.tracks().get(self.track)?.clips().get(index)?;
    Some(clip.record().start())
  }
}

/// How a clip changed between two timelines.
///
/// Marked `#[non_exhaustive]`: a later kind of change joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChangeKind {
  /// The clip is new.
  Added,
  /// The clip is gone.
  Removed,
  /// The clip's record changed: it sits elsewhere on the timeline, or covers
  /// more or less of it.
  Moved,
  /// The clip's source range changed: it plays a different stretch of its
  /// medium.
  Retimed,
  /// The clip's gain or fades changed.
  Regained,
  /// The clip was enabled and is disabled, or the other way round.
  EnabledFlipped,
}

#[cfg(test)]
mod tests;

//! Judging a timeline: every refusal by name, with where it is.

use alloc::vec::Vec;
use core::{cmp::Ordering, fmt};

use mediatime::{Duration, ExactSeconds, Rounding, Timebase, Timestamp};

use crate::{Clip, Timeline, Track, Transition, time::span};

/// Judges `timeline`, answering every refusal it earns, or `Ok` when there
/// is none.
///
/// A timeline is valid when:
///
/// - its edit rate is stated — not zero ([`Refusal::RateUnstated`]);
/// - every position on it — the start, each record, each fade, each
///   transition — is counted at the edit rate ([`Refusal::OffEditRate`]);
/// - each clip's medium has a real ruler ([`Refusal::MediaRateUnstated`]);
/// - each record starts at or after the timeline's zero
///   ([`Refusal::RecordBeforeZero`]) and covers some time
///   ([`Refusal::EmptyRecord`]);
/// - each track's clips are in record order ([`Refusal::OutOfOrder`]) and no
///   two of their records overlap ([`Refusal::Overlap`]) — a transition
///   blends across a cut with media outside the records, so it is never an
///   overlap;
/// - each source range runs a whole number of edit-rate ticks
///   ([`Refusal::SourceOffEditRate`]) and its record exactly as long
///   ([`Refusal::DurationMismatch`]) — there is no time-warp in schema 1, so
///   nothing may round a source onto the edit rate;
/// - each source range lies inside its medium's available range when that
///   is known ([`Refusal::OutsideAvailable`]);
/// - each transition sits on a cut two of its track's records share
///   ([`Refusal::OffBoundary`]), alone ([`Refusal::DuplicateTransition`]),
///   with both handles inside the media where that is known
///   ([`Refusal::HandleMissing`]);
/// - no fade sits at a cut a transition already blends
///   ([`Refusal::FadeMeetsTransition`]), and a clip's blends — a fade, or the
///   part of a transition inside it — fit inside it
///   ([`Refusal::BlendsOverrunClip`]): in particular, no fade runs longer
///   than its clip.
///
/// Refusals come back in a fixed order: the timeline's own, then track by
/// track — each clip's, then each transition's, then each clip's blends.
/// The checks that need the edit rate are skipped while it is unstated, and
/// those that read a clip's media ruler are skipped for a clip whose medium
/// has none, so those two defects are not reported again by every check
/// that would read them.
///
/// The time arithmetic is `mediatime`'s, compared exactly across timebases.
pub fn validate(timeline: &Timeline) -> Result<(), Vec<Refusal>> {
  let mut refusals = Vec::new();
  let edit = timeline.edit_timebase();
  match edit {
    None => refusals.push(Refusal::RateUnstated),
    Some(edit) => {
      if timeline.start().timebase() != edit {
        refusals.push(Refusal::OffEditRate(Place::Start));
      }
    }
  }
  for (index, track) in timeline.tracks().iter().enumerate() {
    judge_track(index, track, edit, &mut refusals);
  }
  if refusals.is_empty() {
    Ok(())
  } else {
    Err(refusals)
  }
}

fn judge_track(track_index: usize, track: &Track, edit: Option<Timebase>, out: &mut Vec<Refusal>) {
  let clips = track.clips();
  // The record reaching furthest so far, by index and end: a later record
  // starting before that end overlaps it.
  let mut furthest: Option<(usize, Timestamp)> = None;
  for (index, clip) in clips.iter().enumerate() {
    let at = ClipAt::new(track_index, index);
    let record = clip.record();
    let on_rate = edit.is_none_or(|edit| record.timebase() == edit);
    if !on_rate {
      out.push(Refusal::OffEditRate(Place::Record(at)));
    }
    let media_stated = media_stated(clip);
    if !media_stated {
      out.push(Refusal::MediaRateUnstated(at));
    }
    if record.start() < Timestamp::new(0, record.timebase()) {
      out.push(Refusal::RecordBeforeZero(at));
    }
    if record.start() == record.end() {
      out.push(Refusal::EmptyRecord(at));
    }
    if let Some(previous) = index.checked_sub(1) {
      let earlier = clips[previous].record();
      if record.start() < earlier.start() {
        out.push(Refusal::OutOfOrder(ClipPair::new(
          track_index,
          previous,
          index,
        )));
      } else if let Some((reach, end)) = furthest
        && record.start() < end
        && record.start() < record.end()
      {
        out.push(Refusal::Overlap(ClipPair::new(track_index, reach, index)));
      }
    }
    if furthest.is_none_or(|(_, end)| record.end() > end) {
      furthest = Some((index, record.end()));
    }
    if let Some(edit) = edit
      && media_stated
    {
      // The source's length at the edit rate, exactly: with no time-warp in
      // schema 1 a record cannot absorb a remainder. A length between ticks
      // is the source's own defect; one too long to count at the edit rate
      // matches no record.
      let length = span(clip.source_range());
      let expected = length.checked_rescale_with(edit, Rounding::Exact);
      if expected.is_none() && length.checked_rescale_to(edit).is_some() {
        out.push(Refusal::SourceOffEditRate(at));
      } else if on_rate {
        let found = span(record);
        if expected.is_none_or(|expected| expected.ticks() != found.ticks()) {
          out.push(Refusal::DurationMismatch(Mismatch {
            clip: at,
            expected,
            found,
          }));
        }
      }
    }
    if media_stated
      && let Some(available) = clip.media().available_range()
      && !available.contains(&clip.source_range())
    {
      out.push(Refusal::OutsideAvailable(at));
    }
    if let Some(edit) = edit {
      let fades = clip.fades();
      for (edge, fade) in [(Edge::In, fades.in_()), (Edge::Out, fades.out())] {
        if let Some(fade) = fade
          && fade.duration().timebase() != edit
        {
          out.push(Refusal::OffEditRate(Place::Fade(EdgeAt::new(at, edge))));
        }
      }
    }
  }

  let transitions = track.transitions();
  for (index, transition) in transitions.iter().enumerate() {
    let at = TransitionAt::new(track_index, index);
    if let Some(edit) = edit
      && (transition.at().timebase() != edit
        || transition.in_offset().timebase() != edit
        || transition.out_offset().timebase() != edit)
    {
      out.push(Refusal::OffEditRate(Place::Transition(at)));
    }
    if transitions[..index]
      .iter()
      .any(|earlier| earlier.at() == transition.at())
    {
      out.push(Refusal::DuplicateTransition(at));
    }
    let (Some(outgoing), Some(incoming)) = (
      outgoing_at(clips, transition.at()),
      incoming_at(clips, transition.at()),
    ) else {
      out.push(Refusal::OffBoundary(at));
      continue;
    };
    if !tail_handle_held(&clips[outgoing], transition.out_offset()) {
      out.push(Refusal::HandleMissing(EdgeAt::new(
        ClipAt::new(track_index, outgoing),
        Edge::Out,
      )));
    }
    if !head_handle_held(&clips[incoming], transition.in_offset()) {
      out.push(Refusal::HandleMissing(EdgeAt::new(
        ClipAt::new(track_index, incoming),
        Edge::In,
      )));
    }
  }

  for (index, clip) in clips.iter().enumerate() {
    let at = ClipAt::new(track_index, index);
    let record = clip.record();
    let fades = clip.fades();
    let entering = transition_at(transitions, record.start());
    let leaving = transition_at(transitions, record.end());
    if fades.in_().is_some() && entering.is_some() {
      out.push(Refusal::FadeMeetsTransition(EdgeAt::new(at, Edge::In)));
    }
    if fades.out().is_some() && leaving.is_some() {
      out.push(Refusal::FadeMeetsTransition(EdgeAt::new(at, Edge::Out)));
    }
    // What blends inside the record at each edge: the fade, or the part of a
    // transition on this side of its cut — whichever reaches further.
    let head = longest(
      fades.in_().map(|fade| fade.duration()),
      entering.map(Transition::out_offset),
    );
    let tail = longest(
      fades.out().map(|fade| fade.duration()),
      leaving.map(Transition::in_offset),
    );
    let blends = ExactSeconds::from_duration(head).checked_add(ExactSeconds::from_duration(tail));
    if blends.is_none_or(|blends| blends > ExactSeconds::from_duration(span(record))) {
      out.push(Refusal::BlendsOverrunClip(at));
    }
  }
}

/// Whether the clip's medium has a real ruler: no stated rate of zero, and
/// neither its source range nor its available range counted in a
/// degenerate timebase.
fn media_stated(clip: &Clip) -> bool {
  let media = clip.media();
  media.rate().is_none_or(|rate| rate.num() != 0)
    && clip.source_range().timebase().num() != 0
    && media
      .available_range()
      .is_none_or(|available| available.timebase().num() != 0)
}

/// The clip whose record ends at the cut `at`.
fn outgoing_at(clips: &[Clip], at: Timestamp) -> Option<usize> {
  clips.iter().position(|clip| {
    let record = clip.record();
    record.end() == at && record.start() < record.end()
  })
}

/// The clip whose record starts at the cut `at`.
fn incoming_at(clips: &[Clip], at: Timestamp) -> Option<usize> {
  clips.iter().position(|clip| {
    let record = clip.record();
    record.start() == at && record.start() < record.end()
  })
}

/// The first transition whose cut is at `at`.
fn transition_at(transitions: &[Transition], at: Timestamp) -> Option<&Transition> {
  transitions.iter().find(|transition| transition.at() == at)
}

/// Whether the outgoing clip's medium holds `out_offset` past its source
/// range's end. A medium whose available range is unknown is taken at its
/// word, as its source range is; a sum past `i128` holds nothing.
fn tail_handle_held(clip: &Clip, out_offset: Duration) -> bool {
  if !media_stated(clip) {
    return true;
  }
  let Some(available) = clip.media().available_range() else {
    return true;
  };
  ExactSeconds::from_timestamp(clip.source_range().end())
    .checked_add(ExactSeconds::from_duration(out_offset))
    .is_some_and(|needed| needed <= ExactSeconds::from_timestamp(available.end()))
}

/// Whether the incoming clip's medium holds `in_offset` before its source
/// range's start.
fn head_handle_held(clip: &Clip, in_offset: Duration) -> bool {
  if !media_stated(clip) {
    return true;
  }
  let Some(available) = clip.media().available_range() else {
    return true;
  };
  ExactSeconds::from_timestamp(clip.source_range().start())
    .checked_sub(ExactSeconds::from_duration(in_offset))
    .is_some_and(|needed| ExactSeconds::from_timestamp(available.start()) <= needed)
}

/// The longer of two optional lengths, or a zero length when both are
/// absent.
fn longest(a: Option<Duration>, b: Option<Duration>) -> Duration {
  match (a, b) {
    (Some(a), Some(b)) => match a.cmp_semantic(&b) {
      Ordering::Less => b,
      _ => a,
    },
    (Some(one), None) | (None, Some(one)) => one,
    (None, None) => Duration::new(0, Timebase::default()),
  }
}

/// One defect [`validate`] found, named, with where it is.
///
/// Marked `#[non_exhaustive]`: a later check joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Refusal {
  /// The edit rate is zero: a timeline needs a real ruler to count its
  /// positions in.
  RateUnstated,
  /// A position on the timeline is not counted at the edit rate.
  OffEditRate(Place),
  /// A clip's medium has no real ruler: a stated rate of zero, or a source
  /// or available range counted in a degenerate (`0/den`) timebase.
  MediaRateUnstated(ClipAt),
  /// A record starts before the timeline's zero.
  RecordBeforeZero(ClipAt),
  /// A record covers no time.
  EmptyRecord(ClipAt),
  /// A clip's record starts before an earlier clip's: a track's clips are
  /// kept in record order.
  OutOfOrder(ClipPair),
  /// Two records of one track overlap.
  Overlap(ClipPair),
  /// A source range's length is no whole number of edit-rate ticks. A
  /// record runs exactly as long as its source (schema 1 has no time-warp),
  /// so a source must rescale onto the edit rate exactly.
  SourceOffEditRate(ClipAt),
  /// A record's length is not its source range's, counted at the edit rate.
  DurationMismatch(Mismatch),
  /// A source range runs outside its medium's available range.
  OutsideAvailable(ClipAt),
  /// A transition's cut is not where one of its track's records ends and
  /// another begins.
  OffBoundary(TransitionAt),
  /// A transition sits at a cut an earlier transition already holds.
  DuplicateTransition(TransitionAt),
  /// A clip's medium does not hold the handle a transition plays at that
  /// edge.
  HandleMissing(EdgeAt),
  /// A fade sits at a cut a transition already blends.
  FadeMeetsTransition(EdgeAt),
  /// A clip's blends — its fades, and the parts of transitions inside it —
  /// together run longer than its record.
  BlendsOverrunClip(ClipAt),
}

impl fmt::Display for Refusal {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::RateUnstated => f.write_str("the edit rate is zero"),
      Self::OffEditRate(place) => write!(f, "{place} is not counted at the edit rate"),
      Self::MediaRateUnstated(at) => write!(f, "{at}: the medium has no real ruler"),
      Self::RecordBeforeZero(at) => {
        write!(f, "{at}: the record starts before the timeline's zero")
      }
      Self::EmptyRecord(at) => write!(f, "{at}: the record covers no time"),
      Self::OutOfOrder(pair) => write!(
        f,
        "track {}: clip {} starts before clip {}",
        pair.track, pair.later, pair.earlier
      ),
      Self::Overlap(pair) => write!(
        f,
        "track {}: clip {}'s record overlaps clip {}'s",
        pair.track, pair.later, pair.earlier
      ),
      Self::SourceOffEditRate(at) => write!(
        f,
        "{at}: the source range's length is no whole number of edit-rate ticks"
      ),
      Self::DurationMismatch(mismatch) => {
        write!(
          f,
          "{}: the record runs {} ticks, the source range ",
          mismatch.clip,
          mismatch.found.ticks()
        )?;
        match mismatch.expected {
          Some(expected) => write!(f, "{} at the edit rate", expected.ticks()),
          None => f.write_str("has no count at the edit rate"),
        }
      }
      Self::OutsideAvailable(at) => {
        write!(f, "{at}: the source range runs outside the available range")
      }
      Self::OffBoundary(at) => write!(f, "{at}: the cut is not where two records meet"),
      Self::DuplicateTransition(at) => {
        write!(f, "{at}: an earlier transition holds this cut")
      }
      Self::HandleMissing(at) => write!(
        f,
        "{}: the medium holds no handle for the transition at its {}",
        at.clip,
        at.edge.side()
      ),
      Self::FadeMeetsTransition(at) => write!(
        f,
        "{}: a fade at its {} where a transition blends",
        at.clip,
        at.edge.side()
      ),
      Self::BlendsOverrunClip(at) => {
        write!(
          f,
          "{at}: its fades and transitions run longer than its record"
        )
      }
    }
  }
}

impl core::error::Error for Refusal {}

/// A clip, by the index of its track in the timeline and its index in the
/// track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClipAt {
  track: usize,
  clip: usize,
}

impl ClipAt {
  /// Clip `clip` of track `track`.
  pub const fn new(track: usize, clip: usize) -> Self {
    Self { track, clip }
  }

  /// The track's index in the timeline.
  pub const fn track(&self) -> usize {
    self.track
  }

  /// The clip's index in its track.
  pub const fn clip(&self) -> usize {
    self.clip
  }
}

/// Writes `track 0, clip 2`.
impl fmt::Display for ClipAt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "track {}, clip {}", self.track, self.clip)
  }
}

/// Two clips of one track, by index: the earlier and the later in the
/// track's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClipPair {
  track: usize,
  earlier: usize,
  later: usize,
}

impl ClipPair {
  /// Clips `earlier` and `later` of track `track`.
  pub const fn new(track: usize, earlier: usize, later: usize) -> Self {
    Self {
      track,
      earlier,
      later,
    }
  }

  /// The track's index in the timeline.
  pub const fn track(&self) -> usize {
    self.track
  }

  /// The earlier clip's index in its track.
  pub const fn earlier(&self) -> usize {
    self.earlier
  }

  /// The later clip's index in its track.
  pub const fn later(&self) -> usize {
    self.later
  }
}

/// A transition, by the index of its track in the timeline and its index in
/// the track's transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TransitionAt {
  track: usize,
  transition: usize,
}

impl TransitionAt {
  /// Transition `transition` of track `track`.
  pub const fn new(track: usize, transition: usize) -> Self {
    Self { track, transition }
  }

  /// The track's index in the timeline.
  pub const fn track(&self) -> usize {
    self.track
  }

  /// The transition's index in its track.
  pub const fn transition(&self) -> usize {
    self.transition
  }
}

/// Writes `track 0, transition 1`.
impl fmt::Display for TransitionAt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "track {}, transition {}", self.track, self.transition)
  }
}

/// One of a clip's two edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Edge {
  /// The clip's start: where a fade-in runs, and where a transition into the
  /// clip plays the media before its source range.
  In,
  /// The clip's end: where a fade-out runs, and where a transition out of
  /// the clip plays the media after its source range.
  Out,
}

impl Edge {
  const fn side(self) -> &'static str {
    match self {
      Self::In => "start",
      Self::Out => "end",
    }
  }
}

/// One edge of one clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeAt {
  clip: ClipAt,
  edge: Edge,
}

impl EdgeAt {
  /// The `edge` of `clip`.
  pub const fn new(clip: ClipAt, edge: Edge) -> Self {
    Self { clip, edge }
  }

  /// The clip.
  pub const fn clip(&self) -> ClipAt {
    self.clip
  }

  /// Which edge.
  pub const fn edge(&self) -> Edge {
    self.edge
  }
}

/// A position on the timeline that [`Refusal::OffEditRate`] names.
///
/// Marked `#[non_exhaustive]`: a later positioned word joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Place {
  /// The timeline's start.
  Start,
  /// A clip's record.
  Record(ClipAt),
  /// A clip's fade at one edge.
  Fade(EdgeAt),
  /// A transition's cut or one of its offsets.
  Transition(TransitionAt),
}

/// Writes `the start`, `the record of track 0, clip 2`, and so on.
impl fmt::Display for Place {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Start => f.write_str("the start"),
      Self::Record(at) => write!(f, "the record of {at}"),
      Self::Fade(at) => write!(f, "the fade at the {} of {}", at.edge.side(), at.clip),
      Self::Transition(at) => write!(f, "{at}"),
    }
  }
}

/// A record whose length is not its source range's: what
/// [`Refusal::DurationMismatch`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mismatch {
  clip: ClipAt,
  expected: Option<Duration>,
  found: Duration,
}

impl Mismatch {
  /// The clip.
  pub const fn clip(&self) -> ClipAt {
    self.clip
  }

  /// The source range's length counted at the edit rate — the record's
  /// length the source asks for — or `None` when that count does not fit a
  /// `u64`.
  pub const fn expected(&self) -> Option<Duration> {
    self.expected
  }

  /// The record's length.
  pub const fn found(&self) -> Duration {
    self.found
  }
}

#[cfg(test)]
mod tests;

//! A valid timeline laid out: each track's clips at their records, and the
//! gaps between them.

use alloc::vec::Vec;

use mediatime::TimeRange;

use crate::{Clip, Refusal, Timeline, Track, validate};

/// Lays `timeline` out for export: per track, in record order, each clip at
/// its record and a gap wherever the records leave time uncovered.
///
/// Positions are explicit, so a clip's place is its record, as stored —
/// layout derives nothing about a clip. What it derives are the **gaps**:
/// from the timeline's zero to the first record, and between each record's
/// end and the next one's start. A gap is never stored; it is what an
/// interchange format that places items end to end (OpenTimelineIO) needs
/// written between them. A track ends at its last record: no gap is
/// written after it.
///
/// A timeline that does not validate has no layout; its refusals come back
/// instead.
pub fn layout(timeline: &Timeline) -> Result<Layout<'_>, Vec<Refusal>> {
  validate(timeline)?;
  Ok(layout_valid(timeline))
}

/// [`layout`] of a timeline already validated.
pub(crate) fn layout_valid(timeline: &Timeline) -> Layout<'_> {
  // A valid timeline states its rate, and counts every record in a timebase
  // equal to this one, so record counts and gap counts share one ruler.
  let edit = timeline.edit_timebase().unwrap_or_default();
  let tracks = timeline
    .tracks()
    .iter()
    .map(|track| {
      let mut items = Vec::with_capacity(track.clips().len() * 2);
      let mut covered_to = 0;
      for clip in track.clips() {
        let record = clip.record();
        if record.start_pts() > covered_to {
          items.push(Item::Gap(TimeRange::new(
            covered_to,
            record.start_pts(),
            edit,
          )));
        }
        items.push(Item::Clip(clip));
        covered_to = record.end_pts();
      }
      TrackLayout { track, items }
    })
    .collect();
  Layout { tracks }
}

/// A timeline laid out: one [`TrackLayout`] per track, bottom first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout<'a> {
  tracks: Vec<TrackLayout<'a>>,
}

impl<'a> Layout<'a> {
  /// The tracks, bottom first.
  pub fn tracks(&self) -> &[TrackLayout<'a>] {
    &self.tracks
  }
}

/// One track laid out: its items in record order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackLayout<'a> {
  track: &'a Track,
  items: Vec<Item<'a>>,
}

impl<'a> TrackLayout<'a> {
  /// The track laid out.
  pub const fn track(&self) -> &'a Track {
    self.track
  }

  /// Its clips and gaps, in record order, each starting where the one before
  /// it ends.
  pub fn items(&self) -> &[Item<'a>] {
    &self.items
  }
}

/// One stretch of a laid-out track.
///
/// Marked `#[non_exhaustive]`: a later kind of item joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Item<'a> {
  /// A clip, at its record.
  Clip(&'a Clip),
  /// Time no record covers, counted at the edit rate.
  Gap(TimeRange),
}

impl Item<'_> {
  /// The stretch of the timeline the item covers: a clip's record, or the
  /// gap itself.
  pub const fn record(&self) -> TimeRange {
    match self {
      Self::Clip(clip) => clip.record(),
      Self::Gap(range) => *range,
    }
  }
}

#[cfg(test)]
mod tests;

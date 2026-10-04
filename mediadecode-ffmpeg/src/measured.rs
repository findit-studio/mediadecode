//! The running measurement of a demux walk: the greatest packet end on
//! each track, and whether the walk has finished.
//!
//! [`Measured`] is the state behind
//! [`Demuxer::measured_end`](mediadecode::demuxer::Demuxer::measured_end).
//! It is fed by the pulls the caller already makes — one
//! [`observe`](Measured::observe) per packet delivered — and reads
//! nothing of its own, so there is no seek-to-end probe and no second
//! pass over the file.

use std::collections::TryReserveError;

use mediadecode::{Timebase, Timestamp, demuxer::MeasuredEnd};

/// What a walk has measured so far.
pub(crate) struct Measured {
  /// Per track, the greatest packet end delivered so far, in that
  /// track's own ticks; `None` until a packet carrying a timestamp
  /// arrives.
  ends: Vec<Option<i64>>,
  /// `true` while the walk is one unbroken pass from the first packet:
  /// no seek and no dropped packet since the session opened.
  unbroken: bool,
  /// `true` once an unbroken pass has answered end of file. Never
  /// cleared: the figures are final from then on, and a later seek can
  /// only re-deliver packets that are already counted.
  reached_end: bool,
}

impl Measured {
  /// One slot per track, reserved fallibly — the count is the
  /// container's.
  pub(crate) fn new(tracks: usize) -> Result<Self, TryReserveError> {
    let mut ends = Vec::new();
    ends.try_reserve_exact(tracks)?;
    // Inside the capacity just reserved, so this cannot allocate.
    ends.resize(tracks, None);
    Ok(Self {
      ends,
      unbroken: true,
      reached_end: false,
    })
  }

  /// Folds one delivered packet into its track's figure.
  ///
  /// A packet's end is `pts + duration`, and `pts` alone where it
  /// carries no duration. libavformat derives a duration from the
  /// stream's frame rate or frame size for a packet whose demuxer wrote
  /// none, so a packet reaches here without one only where nothing
  /// could be derived — and a track of such packets measures where its
  /// last packet begins. A packet without a `pts` says nothing about
  /// when it ends and is passed over.
  ///
  /// The sum saturates: a hostile timestamp near `i64::MAX` must stay a
  /// very large figure, not wrap into a small one.
  pub(crate) fn observe(&mut self, track: usize, pts: Option<i64>, duration: i64) {
    let Some(pts) = pts else { return };
    let Some(slot) = self.ends.get_mut(track) else {
      return;
    };
    let end = if duration > 0 {
      pts.saturating_add(duration)
    } else {
      pts
    };
    *slot = Some(slot.map_or(end, |seen| seen.max(end)));
  }

  /// The session answered end of file. The figures are final only if
  /// the pass that got here was unbroken.
  pub(crate) fn end_of_file(&mut self) {
    if self.unbroken {
      self.reached_end = true;
    }
  }

  /// The walk is no longer one unbroken pass from the first packet: a
  /// seek moved it, or a packet was refused and dropped. A figure that
  /// was already final stays final.
  pub(crate) fn break_pass(&mut self) {
    self.unbroken = false;
  }

  /// The figure for `track`, expressed in that track's own `timebase`,
  /// or `None` where nothing has been measured on it.
  pub(crate) fn get(&self, track: usize, timebase: Timebase) -> Option<MeasuredEnd> {
    let ticks = self.ends.get(track).copied().flatten()?;
    Some(MeasuredEnd::new(
      Timestamp::new(ticks, timebase),
      self.reached_end,
    ))
  }
}

#[cfg(test)]
mod tests;

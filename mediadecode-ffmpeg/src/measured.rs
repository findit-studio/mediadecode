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

/// One track's running figure.
#[derive(Clone, Copy)]
enum Figure {
  /// No packet carrying a timestamp has arrived.
  Unseen,
  /// The greatest packet end delivered so far, in the track's own ticks.
  End(i64),
  /// A packet's end did not fit in an `i64`, so the track has no figure
  /// that can be named. Never left again: a later, smaller packet does
  /// not make the greatest end representable.
  Unrepresentable,
}

/// What a walk has measured so far.
pub(crate) struct Measured {
  /// One figure per track.
  ends: Vec<Figure>,
  /// `true` while the walk is one unbroken pass from the first packet:
  /// no seek, no packet read and not delivered, and no end that could
  /// not be represented, since the session opened.
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
    ends.resize(tracks, Figure::Unseen);
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
  /// **An end that does not fit in an `i64` is no end.** Saturating
  /// would record `i64::MAX` as though the file had ended there, and
  /// end of file would then return it as an exact, final figure. The
  /// track answers none from then on, and the pass stops being final
  /// too: the other tracks' figures cannot be called the file's measured
  /// end while one track's is missing.
  pub(crate) fn observe(&mut self, track: usize, pts: Option<i64>, duration: i64) {
    let Some(pts) = pts else { return };
    let Some(slot) = self.ends.get_mut(track) else {
      return;
    };
    let end = if duration > 0 {
      match pts.checked_add(duration) {
        Some(end) => end,
        None => {
          *slot = Figure::Unrepresentable;
          self.unbroken = false;
          return;
        }
      }
    } else {
      pts
    };
    *slot = match *slot {
      Figure::Unseen => Figure::End(end),
      Figure::End(seen) => Figure::End(seen.max(end)),
      Figure::Unrepresentable => Figure::Unrepresentable,
    };
  }

  /// The session answered end of file. The figures are final only if
  /// the pass that got here was unbroken.
  pub(crate) fn end_of_file(&mut self) {
    if self.unbroken {
      self.reached_end = true;
    }
  }

  /// The walk is no longer one unbroken pass from the first packet: a
  /// seek moved it, or a packet was read and not delivered. A figure
  /// that was already final stays final.
  pub(crate) fn break_pass(&mut self) {
    self.unbroken = false;
  }

  /// The figure for `track`, expressed in that track's own `timebase`,
  /// or `None` where nothing has been measured on it or its end cannot
  /// be represented.
  pub(crate) fn get(&self, track: usize, timebase: Timebase) -> Option<MeasuredEnd> {
    match self.ends.get(track)? {
      Figure::End(ticks) => Some(MeasuredEnd::new(
        Timestamp::new(*ticks, timebase),
        self.reached_end,
      )),
      Figure::Unseen | Figure::Unrepresentable => None,
    }
  }
}

#[cfg(test)]
mod tests;

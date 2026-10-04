//! The running measurement of a demux walk: the greatest packet end on
//! each track, and whether the walk covered the file.
//!
//! [`Measured`] is the state behind
//! [`Demuxer::measured_end`](mediadecode::demuxer::Demuxer::measured_end).
//! It is fed by the pulls the caller already makes — one
//! [`observe`](Measured::observe) per timed packet read — and reads
//! nothing of its own, so there is no seek-to-end probe and no second
//! pass over the file.
//!
//! What it can vouch for stops where the session's own reads start:
//! libavformat probes while the container opens, buffers the packets it
//! reads and replays them, but a read error it swallows during probing
//! is not exposed anywhere, so data skipped there is invisible here.

use std::collections::TryReserveError;

use mediadecode::{Timebase, Timestamp, demuxer::MeasuredEnd};

/// One track's running figure.
#[derive(Clone, Copy)]
enum Figure {
  /// No packet carrying a timestamp has been read.
  Unseen,
  /// The greatest packet end read so far, in the track's own ticks.
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
  /// `true` while the walk has skipped nothing since the session opened:
  /// no seek, no packet read and not observed, and no end that could
  /// not be represented.
  unbroken: bool,
  /// `true` once a walk that skipped nothing has answered end of file.
  /// Never cleared: the figures cover the file from then on, and a later
  /// seek can only re-read packets that are already counted.
  walk_complete: bool,
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
      walk_complete: false,
    })
  }

  /// Folds one timed packet into its track's figure.
  ///
  /// A packet's end is `pts + duration`, and `pts` alone where it
  /// carries no duration. libavformat derives a duration from the
  /// stream's frame rate or frame size for a packet whose demuxer wrote
  /// none, so a packet reaches here without one only where nothing
  /// could be derived — and a track of such packets measures where its
  /// last packet begins. A packet without a `pts` says nothing about
  /// when it ends and is passed over.
  ///
  /// The caller observes **before any payload filter**: a timed packet
  /// with no payload is a real endpoint, and a later one is the end.
  ///
  /// **An end that does not fit in an `i64` is no end.** Saturating
  /// would record `i64::MAX` as though the file had ended there, and
  /// end of file would then return it as an exact figure over a
  /// complete walk. The track answers none from then on, and the walk
  /// stops being complete too: the other tracks' figures cannot be
  /// called the file's measured end while one track's is missing.
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

  /// The session answered end of file. The walk is complete only if it
  /// skipped nothing.
  pub(crate) fn end_of_file(&mut self) {
    if self.unbroken {
      self.walk_complete = true;
    }
  }

  /// The walk has skipped something: a seek moved it, or a packet was
  /// read and not observed. A walk that was already complete stays so.
  pub(crate) fn break_walk(&mut self) {
    self.unbroken = false;
  }

  /// The figure for `track`, expressed in that track's own `timebase`,
  /// or `None` where nothing has been measured on it or its end cannot
  /// be represented.
  pub(crate) fn get(&self, track: usize, timebase: Timebase) -> Option<MeasuredEnd> {
    match self.ends.get(track)? {
      Figure::End(ticks) => Some(MeasuredEnd::new(
        Timestamp::new(*ticks, timebase),
        self.walk_complete,
      )),
      Figure::Unseen | Figure::Unrepresentable => None,
    }
  }
}

#[cfg(test)]
mod tests;

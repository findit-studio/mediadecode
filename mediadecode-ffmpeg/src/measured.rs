//! The running measurement of a demux walk: the greatest packet end on
//! each track, whether that end is exact, and whether the walk covered
//! the file.
//!
//! [`Measured`] is the state behind
//! [`Demuxer::measured_end`](mediadecode::demuxer::Demuxer::measured_end).
//! It is fed by the pulls the caller already makes — one
//! [`observe`](Measured::observe) per timed packet read — and reads
//! nothing of its own, so there is no seek-to-end probe and no second
//! pass over the file.
//!
//! The figure is an **endpoint on the track's own timeline**, not a
//! length: turning it into one means subtracting the track's start and
//! honouring presentation edits, which is the composer's job and not
//! this module's.
//!
//! # Lifecycle
//!
//! The walk is *unbroken* from the session's first read until something
//! breaks it: a seek, a packet read and not observed, or an end that
//! cannot be represented. End of file on an unbroken walk **completes**
//! it, and a completed walk is **frozen**: its figures are final, so
//! neither a later [`observe`](Measured::observe) nor a later
//! [`break_walk`](Measured::break_walk) changes anything, through any
//! seek. A walk broken before it completes can never complete in this
//! session; measurement carries on regardless, as a "so far" maximum.
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

/// One track's slot: its figure, and whether every end that fed it was
/// known.
#[derive(Clone, Copy)]
struct Slot {
  figure: Figure,
  /// `true` until a packet on the track could not give an end of its
  /// own — no usable `pts`, or no positive duration. From then on the
  /// figure is a lower bound.
  exact: bool,
}

/// What a walk has measured so far.
pub(crate) struct Measured {
  /// One slot per track.
  ends: Vec<Slot>,
  /// `true` while the walk has skipped nothing since the session opened:
  /// no seek, no packet read and not observed, and no end that could
  /// not be represented.
  unbroken: bool,
  /// `true` once a walk that skipped nothing has answered end of file.
  /// From then on the measurement is frozen.
  walk_complete: bool,
}

impl Measured {
  /// One slot per track, reserved fallibly — the count is the
  /// container's.
  pub(crate) fn new(tracks: usize) -> Result<Self, TryReserveError> {
    let mut ends = Vec::new();
    ends.try_reserve_exact(tracks)?;
    // Inside the capacity just reserved, so this cannot allocate.
    ends.resize(
      tracks,
      Slot {
        figure: Figure::Unseen,
        exact: true,
      },
    );
    Ok(Self {
      ends,
      unbroken: true,
      walk_complete: false,
    })
  }

  /// Folds one timed packet into its track's figure.
  ///
  /// A packet's end is `pts + duration`. A packet whose duration is not
  /// positive — libavformat writes zero for one it does not know, and
  /// `AV_NOPTS_VALUE` is not positive either — ends, as far as the walk
  /// can tell, at its `pts`, and that makes the track's end **inexact**:
  /// a lower bound. A duration libavformat derived from the stream's
  /// frame rate or frame size for a packet whose demuxer wrote none
  /// counts as a duration here, and is only as exact as that derivation.
  ///
  /// A packet **read** without a usable `pts` has no end to place: it
  /// moves no figure, and it makes the track's end inexact too. That
  /// holds whether or not the packet carries a payload and whatever
  /// becomes of it afterwards — delivered, refused or parked.
  ///
  /// The caller observes **before the payload conversion**, which is
  /// what makes both marks independent of its outcome: a timed packet
  /// with no payload is a real endpoint, and a later one is the end.
  ///
  /// **An end that does not fit in an `i64` is no end.** Saturating
  /// would record `i64::MAX` as though the file had ended there, and
  /// end of file would then return it as an exact figure over a
  /// complete walk. The track answers none from then on, and the walk
  /// stops being complete too: the other tracks' figures cannot be
  /// called the file's measured end while one track's is missing.
  ///
  /// A completed walk is frozen and ignores the packet.
  pub(crate) fn observe(&mut self, track: usize, pts: Option<i64>, duration: i64) {
    if self.walk_complete {
      return;
    }
    let Some(slot) = self.ends.get_mut(track) else {
      return;
    };
    let Some(pts) = pts else {
      slot.exact = false;
      return;
    };
    let end = if duration > 0 {
      match pts.checked_add(duration) {
        Some(end) => end,
        None => {
          slot.figure = Figure::Unrepresentable;
          self.unbroken = false;
          return;
        }
      }
    } else {
      slot.exact = false;
      pts
    };
    slot.figure = match slot.figure {
      Figure::Unseen => Figure::End(end),
      Figure::End(seen) => Figure::End(seen.max(end)),
      Figure::Unrepresentable => Figure::Unrepresentable,
    };
  }

  /// The session answered end of file. The walk is complete only if it
  /// skipped nothing, and a completed walk is frozen from here on.
  pub(crate) fn end_of_file(&mut self) {
    if self.unbroken {
      self.walk_complete = true;
    }
  }

  /// The walk has skipped something: a seek moved it, or a packet was
  /// read and not observed. Completion can never be reached in this
  /// session from here, and measurement carries on as a "so far"
  /// maximum. A walk that was already complete is frozen and stays so.
  pub(crate) fn break_walk(&mut self) {
    if self.walk_complete {
      return;
    }
    self.unbroken = false;
  }

  /// The figure for `track`, expressed in that track's own `timebase`,
  /// or `None` where nothing has been measured on it or its end cannot
  /// be represented.
  pub(crate) fn get(&self, track: usize, timebase: Timebase) -> Option<MeasuredEnd> {
    let slot = self.ends.get(track)?;
    match slot.figure {
      Figure::End(ticks) => Some(MeasuredEnd::new(
        Timestamp::new(ticks, timebase),
        self.walk_complete,
        slot.exact,
      )),
      Figure::Unseen | Figure::Unrepresentable => None,
    }
  }
}

#[cfg(test)]
mod tests;

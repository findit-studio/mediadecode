//! Time, as `mediatime` 0.5 counts it.
//!
//! The crate does no time arithmetic of its own. Every comparison, sum and
//! recount is one of `mediatime`'s, exact across timebases unless it names a
//! rounding:
//!
//! | what the crate needs | `mediatime` 0.5 |
//! |---|---|
//! | instants compared across timebases: order, overlap, a cut both records share | `Timestamp`'s `Ord` and `Eq` (`cmp_semantic`) |
//! | a source range inside its available range | `TimeRange::contains` |
//! | a source's length at the edit rate, exactly, or refused | `Duration::checked_rescale_with(_, Rounding::Exact)`, and `checked_rescale_to` to tell a length between ticks from one too long to count |
//! | handles and blends summed across timebases | `ExactSeconds` |
//! | a media-side range in frames only where its start and length both land on one | `checked_rescale_with(_, Rounding::Exact)` |
//! | a rate as OpenTimelineIO's `f64`; a rate of zero refused | `Rate::as_f64`, `Rate::checked_to_timebase`, `Rate::checked_from_timebase` |
//!
//! One road is missing, and is filed as a `mediatime` row rather than built
//! here: a range's exact length as a [`Duration`] — [`span`].

use mediatime::{Duration, TimeRange};

/// `range`'s length, counted in its own timebase.
///
/// `mediatime` 0.5 measures a range as `total_pts` (an `i64` that saturates
/// at `i64::MAX`) or `duration` (a `core::time::Duration`, truncated to the
/// nanosecond), and has no road to its own exact [`Duration`]; that road is
/// filed as a `mediatime` row, `TimeRange::span(&self) -> Duration`. Until
/// it lands this reads `total_pts`, which is never negative, so a range
/// longer than `i64::MAX` ticks — no medium is — measures `i64::MAX`.
pub(crate) const fn span(range: TimeRange) -> Duration {
  Duration::new(range.total_pts() as u64, range.timebase())
}

#[cfg(test)]
mod tests;

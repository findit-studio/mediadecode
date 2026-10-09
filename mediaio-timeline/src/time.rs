//! The one road from a range to its length that this crate needs and
//! `mediatime` 0.5 does not have.

use mediatime::{Duration, TimeRange};

/// `range`'s length, counted in its own timebase.
///
/// `mediatime` 0.5 measures a range as `total_pts` (an `i64` that saturates
/// at `i64::MAX`) or `duration` (a `core::time::Duration`, truncated to the
/// nanosecond), and has no road to its own exact [`Duration`]; that road is
/// filed as a `mediatime` row (`TimeRange::span`). Until it lands this reads
/// `total_pts`, which is never negative, so a range longer than `i64::MAX`
/// ticks — no medium is — measures `i64::MAX`.
pub(crate) const fn span(range: TimeRange) -> Duration {
  Duration::new(range.total_pts() as u64, range.timebase())
}

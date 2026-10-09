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
//! | a media-side range recounted in another ruler that holds it: the coarsest whole-rate one, where its own rulers count past 2^53; a ruler the timeline's operands are counted in, which the export plans a source range in where none of its own rulers writes it, and an available range where none holds it whole, and which its search tries; or a coarser whole rate the search tries | `checked_rescale_with(_, Rounding::Exact)`, which answers only where the ruler holds it |
//! | a rate as OpenTimelineIO's `f64`; a rate of zero refused | `Rate::as_f64`, `Rate::checked_to_timebase`, `Rate::checked_from_timebase` |
//! | a range's length, or none past `i64::MAX` ticks | `Timestamp::checked_signed_duration_since` |
//! | what OpenTimelineIO derives from an exported document, exactly: its sums, the longest track, and a range's last tick | `ExactSeconds` (`from_timestamp`, `checked_add`, `checked_sub`, `Ord`) |
//!
//! Three roads are missing, and are filed as `mediatime` rows rather than
//! built here: a range's exact length as a [`Duration`] — [`span`]; the
//! coarsest timebase of a whole number of ticks a second in which a range's
//! ends both land on a tick, which the OpenTimelineIO export picks meanwhile
//! from the greatest common divisor of the range's counts, handing the
//! recount itself to `mediatime`; and an exact number of seconds counted at
//! a rate, as an exact fraction of a tick — `ExactSeconds` recounts only to
//! a whole tick — which the export forms meanwhile as one product of the two
//! fractions, reduced.
//!
//! The export also computes OpenTimelineIO's own `f64` arithmetic, operation
//! for operation, to find a count OpenTimelineIO would round. That is
//! OpenTimelineIO's arithmetic, mirrored, not the model's: `mediatime` is
//! exact by design and has no road for it.

use mediatime::{Duration, TimeRange};

/// `range`'s length, counted in its own timebase, or `None` for a range
/// longer than `i64::MAX` ticks.
///
/// `mediatime` 0.5 measures a range as `total_pts` (an `i64` that saturates
/// at `i64::MAX`, so two ranges of different lengths can measure alike) or
/// `duration` (a `core::time::Duration`, truncated to the nanosecond), and
/// has no road to its own exact [`Duration`]; that road is filed as a
/// `mediatime` row, `TimeRange::span(&self) -> Duration`. Until it lands
/// this takes the checked difference of the endpoints, which has no answer
/// past `i64::MAX` ticks. No timeline needs such a range: the document's
/// reader and [`validate`](fn@crate::validate) refuse one by name
/// ([`Refusal::RangeTooLong`](crate::Refusal::RangeTooLong)), so a length
/// never stands in for another.
pub(crate) fn span(range: TimeRange) -> Option<Duration> {
  range
    .end()
    .checked_signed_duration_since(&range.start())
    .and_then(|length| u64::try_from(length.ticks()).ok())
    .map(|ticks| Duration::new(ticks, range.timebase()))
}

#[cfg(test)]
mod tests;

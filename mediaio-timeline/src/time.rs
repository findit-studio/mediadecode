//! Time, as `mediatime` 0.5.1 counts it.
//!
//! The crate does no time arithmetic of its own. Every comparison, sum,
//! measure and recount is one of `mediatime`'s, exact across timebases
//! unless it names a rounding:
//!
//! | what the crate needs | `mediatime` 0.5.1 |
//! |---|---|
//! | instants compared across timebases: order, overlap, a cut both records share | `Timestamp`'s `Ord` and `Eq` (`cmp_semantic`) |
//! | a source range inside its available range | `TimeRange::contains` |
//! | a source's length at the edit rate, exactly, or refused | `Duration::checked_rescale_with(_, Rounding::Exact)`, and `checked_rescale_to` to tell a length between ticks from one too long to count |
//! | handles and blends summed across timebases | `ExactSeconds` |
//! | a media-side range in frames only where its start and length both land on one | `checked_rescale_with(_, Rounding::Exact)` |
//! | a media-side range recounted in another ruler that holds it: the coarsest whole-rate one, where its own rulers count past 2^53; a ruler the timeline's operands are counted in, which the export plans a source range in where none of its own rulers writes it, and an available range where none holds it whole, and which its search tries; or a coarser whole rate the search tries | `checked_rescale_with(_, Rounding::Exact)`, which answers only where the ruler holds it |
//! | the coarsest whole-rate ruler a media-side range lands on, whose multiples are every whole rate that does | `TimeRange::coarsest_whole_rate` |
//! | a rate as OpenTimelineIO's `f64`; a rate of zero refused | `Rate::as_f64`, `Rate::checked_to_timebase`, `Rate::checked_from_timebase` |
//! | a range's length, or none past `i64::MAX` ticks | `TimeRange::span`, exact and total; the bound is this crate's ([`span`]) |
//! | what OpenTimelineIO derives from an exported document, exactly: its sums, the longest track, and a range's last tick | `ExactSeconds` (`from_timestamp`, `checked_add`, `checked_sub`, `Ord`) |
//! | a time counted in a ruler as an exact fraction of a tick, which the export holds every derived value by — `ExactSeconds` reads back only to a whole tick | `Rate::checked_count` |
//!
//! The export also computes OpenTimelineIO's own `f64` arithmetic, operation
//! for operation, to find a count OpenTimelineIO would round. That is
//! OpenTimelineIO's arithmetic, mirrored, not the model's: `mediatime` is
//! exact by design and has no road for it.

use mediatime::{Duration, TimeRange};

/// `range`'s length, counted in its own timebase, or `None` for a range
/// longer than `i64::MAX` ticks.
///
/// [`TimeRange::span`] measures every range exactly, up to `u64::MAX`
/// ticks; the bound is this crate's. No timeline needs a longer range: the
/// document's reader and [`validate`](fn@crate::validate) refuse one by name
/// ([`Refusal::RangeTooLong`](crate::Refusal::RangeTooLong)), so a length
/// never stands in for another.
pub(crate) fn span(range: TimeRange) -> Option<Duration> {
  let length = range.span();
  i64::try_from(length.ticks()).is_ok().then_some(length)
}

#[cfg(test)]
mod tests;

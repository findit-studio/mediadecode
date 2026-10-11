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

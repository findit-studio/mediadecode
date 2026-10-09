use core::num::NonZeroI32;

use mediatime::Timebase;

use super::*;

#[test]
fn a_span_is_the_ranges_length_in_its_own_timebase() {
  let movie = Timebase::new(1, NonZeroI32::new(24_000).unwrap());
  assert_eq!(
    span(TimeRange::new(24_024, 120_120, movie)),
    Duration::new(96_096, movie)
  );
  assert!(span(TimeRange::new(-5, -5, movie)).is_zero());
}

#[test]
fn a_span_past_i64_saturates_until_mediatime_counts_it_whole() {
  // The widest range is `u64::MAX` ticks long. `total_pts` saturates at
  // `i64::MAX`; the filed `TimeRange::span` answers the whole length, and
  // this law flips when the crate moves onto it.
  let widest = TimeRange::new(i64::MIN, i64::MAX, Timebase::NANOS);
  assert_eq!(span(widest).ticks(), i64::MAX as u64);
}

use core::num::NonZeroI32;

use mediatime::Timebase;

use super::*;

#[test]
fn a_span_is_the_ranges_length_in_its_own_timebase() {
  let movie = Timebase::new(1, NonZeroI32::new(24_000).unwrap());
  assert_eq!(
    span(TimeRange::new(24_024, 120_120, movie)),
    Some(Duration::new(96_096, movie))
  );
  assert!(span(TimeRange::new(-5, -5, movie)).is_some_and(|length| length.is_zero()));
}

#[test]
fn a_range_longer_than_i64_max_ticks_has_no_span() {
  // `total_pts` saturates at `i64::MAX`, so these two would measure alike
  // although the first is `u64::MAX` ticks long. Neither is a length a
  // timeline needs; the first has none here, the second keeps its own.
  let widest = TimeRange::new(i64::MIN, i64::MAX, Timebase::NANOS);
  let longest = TimeRange::new(0, i64::MAX, Timebase::NANOS);
  assert_eq!(widest.total_pts(), longest.total_pts());
  assert_eq!(span(widest), None);
  assert_eq!(
    span(longest),
    Some(Duration::new(i64::MAX as u64, Timebase::NANOS))
  );
  // One tick past `i64::MAX` is past it.
  assert_eq!(span(TimeRange::new(-1, i64::MAX, Timebase::NANOS)), None);
}

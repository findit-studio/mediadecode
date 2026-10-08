//! The re-exported time primitives, used from outside the crate.
//!
//! A consumer reaches `Timebase`, `Timestamp` and `TimeRange` through
//! this crate rather than through a `mediatime` dependency of its own,
//! so the names the range's own methods answer with have to come
//! through here as well.

use core::num::NonZeroI32;

use mediadecode::{InvertedRange, TimeRange, Timebase, Timestamp};

const MILLIS: Timebase = match NonZeroI32::new(1_000) {
  Some(den) => Timebase::new(1, den),
  None => unreachable!(),
};

/// **A move that would put a range's end before its start is refused
/// by a name this crate exports, and the range stays as it was.** The
/// unchecked `with_start` / `with_end` / `set_start` / `set_end` are gone
/// from `mediatime` 0.5; their checked replacements answer
/// `InvertedRange`, and `with_bounds` / `set_bounds` move both ends at
/// once.
#[test]
fn a_move_that_would_invert_a_range_is_refused_by_a_name_this_crate_exports() {
  let range = TimeRange::new(1_000, 5_000, MILLIS);

  let refused: Result<TimeRange, InvertedRange> = range.try_with_start(6_000);
  let refusal: InvertedRange = refused.expect_err("a start past the end is refused");
  assert_eq!(refusal.to_string(), "time range end must not precede start");

  let mut in_place = range;
  assert!(
    in_place.try_set_end(500).is_err(),
    "an end before the start is refused"
  );
  assert_eq!(
    (in_place.start_pts(), in_place.end_pts()),
    (1_000, 5_000),
    "a refused move leaves the range as it was",
  );

  let moved = range
    .try_with_end(8_000)
    .expect("an end after the start is a range");
  assert_eq!(moved.end(), Timestamp::new(8_000, MILLIS));

  let both = range.with_bounds(9_000, 9_500);
  assert_eq!((both.start_pts(), both.end_pts()), (9_000, 9_500));
}

//! The re-exported time primitives, used from outside the crate.
//!
//! A consumer reaches `Timebase`, `Timestamp` and `TimeRange` through
//! this crate rather than through a `mediatime` dependency of its own,
//! so the names the range's own methods answer with have to come
//! through here as well, and the instants have to be the ones
//! `mediaframe`'s frames carry.

use mediadecode::{InvertedRange, TimeRange, Timebase, Timestamp};
use mediaframe::frame::TimestampedFrame;

/// **A move that would put a range's end before its start is refused
/// by a name this crate exports, and the range stays as it was.**
/// `mediatime` 0.5 moves one end of a `TimeRange` only by a checked
/// method — `try_with_start` / `try_with_end` / `try_set_start` /
/// `try_set_end`, each refusing with `InvertedRange` — and both ends at
/// once by `with_bounds` / `set_bounds`.
#[test]
fn a_move_that_would_invert_a_range_is_refused_by_a_name_this_crate_exports() {
  let range = TimeRange::new(1_000, 5_000, Timebase::MILLIS);

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
  assert_eq!(moved.end(), Timestamp::new(8_000, Timebase::MILLIS));

  let both = range.with_bounds(9_000, 9_500);
  assert_eq!((both.start_pts(), both.end_pts()), (9_000, 9_500));
}

/// **The instants this crate hands out are the ones `mediaframe`'s
/// frames carry.** `mediaframe` 0.12 is built on `mediatime` 0.5, the
/// version these re-exports come from, so the graph holds one
/// `mediatime` and a `TimestampedFrame` takes this crate's `Timestamp`
/// as it is. A `mediaframe` built on another `mediatime` minor would make
/// them two types, and this law would not compile.
#[test]
fn a_timestamp_this_crate_exports_is_the_one_mediaframe_frames_carry() {
  let pts = Timestamp::new(3_003, Timebase::MPEG_90K);
  let duration = Timestamp::new(3_003, Timebase::MPEG_90K);

  let frame = TimestampedFrame::new(())
    .with_pts(pts)
    .with_duration(duration);

  let carried: Option<Timestamp> = frame.pts();
  assert_eq!(carried, Some(pts));
  assert_eq!(frame.duration(), Some(duration));
}

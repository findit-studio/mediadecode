use alloc::vec::Vec;
use core::num::NonZeroI32;

use super::*;
use crate::otio::ChildAt;

fn at(rate: i32, count: i128) -> Time {
  Time::written(count, Ruler::new(Rate::hz(rate)))
}

#[test]
fn a_sum_is_carried_in_the_higher_rate_and_a_tie_keeps_the_left() {
  // `operator+` and `operator-` (`rationalTime.h` 316–339): the operand at
  // the lower rate rescaled to the higher.
  let (one, three) = (at(1, 5), at(3, 2));
  for sum in [one.add(three), three.add(one)] {
    assert_eq!((sum.value, sum.ruler.otio), (17.0, 3.0));
  }
  let difference = one.sub(three);
  assert_eq!((difference.value, difference.ruler.otio), (13.0, 3.0));
  let difference = three.sub(one);
  assert_eq!((difference.value, difference.ruler.otio), (-13.0, 3.0));
  // A range's end: the start rescaled to the duration's rate, plus it
  // (`timeRange.h` 105–108) — here into the lower one.
  let end = Time::end(at(3, 7), at(1, 2));
  assert_eq!((end.value, end.ruler.otio), (2.0 + 7.0 / 3.0, 1.0));
  // Equal rates add the values as they are.
  assert_eq!(at(2, 1).add(at(2, 2)).value, 3.0);
}

#[test]
fn a_rescale_rounds_as_opentimelineio_rounds_it() {
  // Codex round 3: 2^53 - 1 seconds into thirds — the product 3·(2^53 - 1)
  // is past 2^53 and an f64 holds it one short. The exact count says so.
  let sum = at(3, 0).add(at(1, (1 << 53) - 1));
  assert_eq!(sum.value, 27_021_597_764_222_972.0);
  assert_eq!(sum.exact_count(), Some((27_021_597_764_222_973, 1)));
  let refused = sum.hold(Spot::StackDuration).unwrap_err();
  assert_eq!(
    (refused.value(), refused.rate()),
    (27_021_597_764_222_973, Rate::hz(3))
  );
}

#[test]
fn a_value_is_held_within_2_53_and_closer_than_half_a_tick() {
  let spot = Spot::TrackPosition(ChildAt::new(0, 0));
  let two_53 = 1_i128 << 53;
  // Whole counts up to 2^53, both signs.
  for count in [0, 1, -1, two_53 - 1, two_53, -two_53] {
    assert_eq!(at(1, count).hold(spot), Ok(()), "{count}");
  }
  // 2^53 + 1 has no f64; 2^53 + 2 has one, but sums from it would not stay
  // exact: past the bound, both.
  for count in [two_53 + 1, two_53 + 2, -two_53 - 1] {
    assert_eq!(
      at(1, count).hold(spot).map_err(|refused| refused.value()),
      Err(count),
      "{count}"
    );
  }
  // A third of a second at 1/2 s a tick is 2/3 of a tick: OpenTimelineIO's
  // double for it, rounded as it is, is held.
  let third = at(3, 1).rescaled_to(Ruler::new(Rate::hz(2)));
  assert_eq!(third.value, 2.0 / 3.0);
  assert_eq!(third.hold(spot), Ok(()));
  // Refused, a count reads to the nearest tick, half a tick up.
  let half = at(2, 2 * two_53 + 1).rescaled_to(Ruler::new(Rate::hz(1)));
  assert_eq!(
    half.hold(spot).map_err(|refused| refused.value()),
    Err(two_53 + 1)
  );
}

/// Whether `value` lies less than half a tick from `num / den`, in exact
/// integers — for a double of few fraction bits.
fn exactly_near(value: f64, num: i128, den: i128) -> bool {
  if value == 0.0 {
    return 2 * num.abs() < den;
  }
  let bits = value.to_bits();
  let exponent = ((bits >> 52) & 0x7ff) as i32;
  let mantissa = i128::from(bits & ((1 << 52) - 1)) | (1 << 52);
  let mantissa = if value < 0.0 { -mantissa } else { mantissa };
  // value = mantissa · 2^shift
  let shift = exponent - 1075;
  let (value_num, value_den) = if shift >= 0 {
    (mantissa << shift, 1_i128)
  } else {
    (mantissa, 1_i128 << -shift)
  };
  (2 * (value_num * den - num * value_den)).abs() < den * value_den
}

#[test]
fn the_check_never_holds_half_a_tick_and_holds_anything_closer() {
  let mut checked = 0;
  for whole in [0_i128, 1, -3, 1 << 40, (1 << 52) - 7] {
    for den in [1_i128, 2, 3, 7, 1_000] {
      for part in 0..den.min(9) {
        let num = whole * den + part;
        // Doubles on a sixteenth of a tick around the count.
        for sixteenths in -12..=28 {
          let value = whole as f64 + f64::from(sixteenths) / 16.0;
          let held = near(value, whole, part, den);
          // Never a double half a tick off or more.
          assert!(
            !held || exactly_near(value, num, den),
            "{value} against {num}/{den}"
          );
          // Any closer, away from the edge.
          let off = f64::from(sixteenths) / 16.0 - part as f64 / den as f64;
          if whole.abs() <= 1 << 40 && off.abs() < 0.5 - 1e-3 {
            assert!(held, "{value} against {num}/{den}");
          }
          checked += 1;
        }
      }
    }
  }
  assert_eq!(checked, 5 * (1 + 2 + 3 + 7 + 9) * 41);
  // Exactly half a tick off is never held, either way.
  assert!(!near(0.5, 0, 0, 1));
  assert!(!near(-0.5, 0, 0, 1));
  assert!(!near(1.0, 0, 1, 2));
  assert!(!near(0.0, 0, 1, 2));
  assert!(near(0.75, 0, 1, 2));
  // Not a number, or no finite one, is never held.
  assert!(!near(f64::NAN, 0, 0, 1));
  assert!(!near(f64::INFINITY, 0, 0, 1));
}

#[test]
fn a_sum_past_i128_is_refused_with_opentimelineio_s_double() {
  let mut sum = at(1, 1);
  sum.seconds = None;
  let refused = sum.hold(Spot::StackDuration).unwrap_err();
  assert_eq!((refused.value(), refused.rate()), (1, Rate::hz(1)));
}

#[test]
fn the_stack_keeps_the_first_of_two_durations_that_compare_equal() {
  // `std::max` by `operator<` (`stack.cpp` 129–130): the first of two that
  // compare equal in `f64` seconds is kept.
  let j = 1_i128 << 51;
  let (shorter, longer) = (at(3, 3 * j + 1), at(3, 3 * j + 2));
  let start = at(3, 0);
  assert!(!shorter.otio_lt(longer));
  assert_eq!(
    stack(&[shorter, longer], start).map_err(|refused| (refused.at(), refused.value())),
    Err((Spot::StackDuration, 3 * j + 2))
  );
  assert_eq!(stack(&[longer, shorter], start), Ok(()));
  // Tracks of one length in two rulers are one duration, whichever is kept.
  assert_eq!(stack(&[at(1, 2), at(3, 6), at(2, 4)], start), Ok(()));
}

#[test]
fn a_stack_with_no_track_ends_at_the_global_start_counted_at_rate_1() {
  // `TimeRange()` (`stack.cpp` 121–123): no duration, at rate 1, so the
  // range from the global start ends at the global start rescaled to rate 1
  // (`timeRange.h` 105–108). At one frame every two seconds, from 2^52
  // frames it ends at 2^53 seconds, held; from a frame later at 2^53 + 2,
  // and from Codex round 7's 2^53 frames at 2^54 — past the bound, refused.
  let frames =
    |count: i128| Time::written(count, Ruler::new(Rate::fps(1, NonZeroI32::new(2).unwrap())));
  assert_eq!(stack(&[], frames(1 << 52)), Ok(()));
  for (start, end) in [((1 << 52) + 1, (1 << 53) + 2), (1 << 53, 1 << 54)] {
    assert_eq!(
      stack(&[], frames(start)).map_err(|refused| (refused.at(), refused.value(), refused.rate())),
      Err((Spot::TimelineEnd, end, Rate::hz(1)))
    );
  }
  // From zero, the empty timeline's start, nothing is rescaled past it.
  assert_eq!(stack(&Vec::new(), at(25, 0)), Ok(()));
}

#[test]
fn a_last_tick_takes_opentimelineio_s_branch_and_is_held_against_the_exact_ones() {
  let spot = Spot::Visible(ChildAt::new(0, 0));
  let two_53 = 1_i128 << 53;
  // One ruler, whole counts, the end within 2^53 — every range the export
  // writes: the end less one tick past one tick, else the start, exactly.
  for start in [-two_53, 1 - two_53, -1, 0, 1, two_53 - 1000] {
    for length in [0, 1, 2, 3, 1000] {
      let last = Time::end_inclusive(at(3, start), at(3, length));
      let exact = if length > 1 {
        start + length - 1
      } else {
        start
      };
      assert_eq!(
        (last.value, last.exact_count()),
        (exact as f64, Some((exact, 1))),
        "{start} {length}"
      );
      assert_eq!(last.hold(spot), Ok(()));
    }
  }
  // A duration no whole number of ticks — ten thirds of a second in halves,
  // 20/3 of them: the end floored, 6.
  let thirds = at(3, 10).rescaled_to(Ruler::new(Rate::hz(2)));
  let last = Time::end_inclusive(at(2, 0), thirds);
  assert_eq!((last.value, last.exact_count()), (6.0, Some((6, 1))));
  // One tick or less: the start, in its own rate.
  let last = Time::end_inclusive(at(5, 7), at(2, 1));
  assert_eq!(
    (last.value, last.ruler.otio, last.exact_count()),
    (7.0, 5.0, Some((7, 1)))
  );
  // Codex round 4: a duration whose double rounds onto a whole number,
  // which exactly it is not — less one frame, not floored: half a frame
  // early, refused with the exact last frame.
  let start = at(25, 1_099_511_627_816).sub(at(7, 1_125_899_906_843_277));
  let duration = at(25, 360_287_970_189_200)
    .add(at(7, 1_125_899_906_843_277))
    .add(at(7, 1_099_511_627_815));
  assert_eq!(duration.value, 4_385_285_893_300_243.0);
  let last = Time::end_inclusive(start, duration);
  assert_eq!(last.value, 365_314_309_059_211.5);
  assert_eq!(last.exact_count(), Some((365_314_309_059_212, 1)));
  assert_eq!(
    last
      .hold(spot)
      .map_err(|refused| (refused.value(), refused.rate())),
    Err((365_314_309_059_212, Rate::hz(25)))
  );
}

#[test]
fn the_floor_is_std_s() {
  let whole = 4_503_599_627_370_496.0;
  for value in [
    -2.5,
    -1.0,
    -0.5,
    0.0,
    0.5,
    1.0,
    2.5,
    whole - 0.5,
    0.5 - whole,
    whole,
    -whole,
    1e300,
    -1e300,
  ] {
    assert_eq!(floor(value), value.floor(), "{value}");
  }
  assert!(floor(f64::NAN).is_nan());
  assert_eq!(floor(f64::INFINITY), f64::INFINITY);
  assert_eq!(floor(f64::NEG_INFINITY), f64::NEG_INFINITY);
}

use alloc::vec::Vec;

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
  assert!(!shorter.otio_lt(longer));
  assert_eq!(
    stack(&[shorter, longer]).map_err(|refused| (refused.at(), refused.value())),
    Err((Spot::StackDuration, 3 * j + 2))
  );
  assert_eq!(stack(&[longer, shorter]), Ok(()));
  // Tracks of one length in two rulers are one duration, whichever is kept.
  assert_eq!(stack(&[at(1, 2), at(3, 6), at(2, 4)]), Ok(()));
  assert_eq!(stack(&Vec::new()), Ok(()));
}

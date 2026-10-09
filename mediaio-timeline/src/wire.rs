//! Reading the document's time values, strictly.
//!
//! The document stores `mediatime`'s values under `mediatime`'s own field
//! names — `numerator` and `denominator`, `pts`, `ticks`, `start` and `end`,
//! each with its `timebase` — and `mediatime` writes them. Its readers accept
//! a key they do not know and drop it, though, so a later word inside a time
//! value (a `speed` beside a record's `start`) would read as schema 1 with
//! the word gone. Every time value in the document is read here instead:
//! through a local shape under the same names that refuses an unknown key by
//! name, then converted, checked, into `mediatime`'s type — refusing what
//! `mediatime`'s own reader refuses, and a range longer than `i64::MAX`
//! ticks besides.

use core::num::NonZeroI32;

use mediatime::{Duration, Rate, TimeRange, Timebase, Timestamp};
use serde::{Deserialize, Deserializer, de};

use crate::time::span;

/// A [`Timebase`], and a [`Rate`], which is written as the rational it is.
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimebaseWire {
  numerator: i32,
  denominator: NonZeroI32,
}

impl TimebaseWire {
  fn read<E: de::Error>(self) -> Result<Timebase, E> {
    Timebase::try_new(self.numerator, self.denominator).ok_or_else(|| self.refusal())
  }

  fn read_rate<E: de::Error>(self) -> Result<Rate, E> {
    Rate::try_fps(self.numerator, self.denominator).ok_or_else(|| self.refusal())
  }

  /// Why `mediatime` has no such rational, in its own reader's words.
  fn refusal<E: de::Error>(self) -> E {
    E::custom(if self.numerator < 0 {
      "timebase numerator must not be negative"
    } else {
      "timebase denominator must be positive"
    })
  }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimestampWire {
  pts: i64,
  timebase: TimebaseWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DurationWire {
  ticks: u64,
  timebase: TimebaseWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeRangeWire {
  start: i64,
  end: i64,
  timebase: TimebaseWire,
}

impl TimeRangeWire {
  fn read<E: de::Error>(self) -> Result<TimeRange, E> {
    let range = TimeRange::try_new(self.start, self.end, self.timebase.read()?)
      .ok_or_else(|| E::custom("time range end must not precede start"))?;
    match span(range) {
      Some(_) => Ok(range),
      None => Err(E::custom(format_args!(
        "time range [{}, {}) runs longer than i64::MAX ticks",
        range.start_pts(),
        range.end_pts()
      ))),
    }
  }
}

/// Reads a [`Rate`].
pub(crate) fn rate<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Rate, D::Error> {
  TimebaseWire::deserialize(deserializer)?.read_rate()
}

/// Reads a [`Rate`] that may be absent.
pub(crate) fn option_rate<'de, D: Deserializer<'de>>(
  deserializer: D,
) -> Result<Option<Rate>, D::Error> {
  Option::<TimebaseWire>::deserialize(deserializer)?
    .map(TimebaseWire::read_rate)
    .transpose()
}

/// Reads a [`Timestamp`].
pub(crate) fn timestamp<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Timestamp, D::Error> {
  let wire = TimestampWire::deserialize(deserializer)?;
  Ok(Timestamp::new(wire.pts, wire.timebase.read()?))
}

/// Reads a [`Duration`].
pub(crate) fn duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
  let wire = DurationWire::deserialize(deserializer)?;
  Ok(Duration::new(wire.ticks, wire.timebase.read()?))
}

/// Reads a [`TimeRange`], refusing by name one longer than `i64::MAX` ticks
/// of its timebase: no timeline needs one, and its length has no exact count
/// here.
pub(crate) fn time_range<'de, D: Deserializer<'de>>(
  deserializer: D,
) -> Result<TimeRange, D::Error> {
  TimeRangeWire::deserialize(deserializer)?.read()
}

/// [`time_range`] for a range that may be absent.
pub(crate) fn option_time_range<'de, D: Deserializer<'de>>(
  deserializer: D,
) -> Result<Option<TimeRange>, D::Error> {
  Option::<TimeRangeWire>::deserialize(deserializer)?
    .map(TimeRangeWire::read)
    .transpose()
}

//! Reading the document's time ranges.

use mediatime::TimeRange;
use serde::{Deserialize, Deserializer, de};

use crate::time::span;

/// Reads a [`TimeRange`], refusing by name one longer than `i64::MAX` ticks
/// of its timebase: no timeline needs one, and its length has no exact count
/// here.
pub(crate) fn time_range<'de, D: Deserializer<'de>>(
  deserializer: D,
) -> Result<TimeRange, D::Error> {
  measured(TimeRange::deserialize(deserializer)?)
}

/// [`time_range`] for a range that may be absent.
pub(crate) fn option_time_range<'de, D: Deserializer<'de>>(
  deserializer: D,
) -> Result<Option<TimeRange>, D::Error> {
  Option::<TimeRange>::deserialize(deserializer)?
    .map(measured)
    .transpose()
}

fn measured<E: de::Error>(range: TimeRange) -> Result<TimeRange, E> {
  match span(range) {
    Some(_) => Ok(range),
    None => Err(E::custom(format_args!(
      "time range [{}, {}) runs longer than i64::MAX ticks",
      range.start_pts(),
      range.end_pts()
    ))),
  }
}

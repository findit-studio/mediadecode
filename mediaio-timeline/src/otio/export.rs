//! A valid timeline as OpenTimelineIO's object tree.
//!
//! Every object is written with its keys in the order OpenTimelineIO's own
//! writer uses: `OTIO_SCHEMA` first, then each base class's fields before
//! the derived class's.
//!
//! Every count is held within ±2^53 — the whole numbers an `f64` holds
//! exactly — or refused, by where it sits: the first the walk meets.

use alloc::{
  format,
  string::{String, ToString},
  vec,
  vec::Vec,
};
use core::num::NonZeroI32;

use mediatime::{Duration, Rate, Rounding, TimeRange, Timebase};

use super::{NotRepresentable, OtioTarget, Spot, json::Value};
use crate::{
  Clip, ClipAt, Edge, EdgeAt, Fade, FadeShape, Item, Layout, MediaRef, Metadata, Timeline, Track,
  TrackKind, TrackLayout, Transition, TransitionAt, time::span,
};

/// The key under which this crate's own words ride in an object's
/// `metadata`.
const OURS: &str = "mediaio";

/// The key a `Clip.2` keeps its one media reference under.
const DEFAULT_MEDIA: &str = "DEFAULT_MEDIA";

/// 2^53: an `f64` holds every whole number of no greater magnitude, and past
/// it only some — a count there is read rounded.
const EXACT: u128 = 1 << 53;

struct Ruler {
  /// The edit rate, as the `f64` OpenTimelineIO counts in.
  rate: f64,
}

pub(crate) fn timeline(
  timeline: &Timeline,
  laid: &Layout<'_>,
  target: OtioTarget,
) -> Result<Value, NotRepresentable> {
  let ruler = Ruler {
    rate: timeline.rate().as_f64(),
  };
  let start = count(i128::from(timeline.start().pts()), Spot::Start)?;
  let tracks = laid
    .tracks()
    .iter()
    .enumerate()
    .map(|(index, track)| self::track(index, track, &ruler, target))
    .collect::<Result<_, _>>()?;
  Ok(object(vec![
    ("OTIO_SCHEMA", text("Timeline.1")),
    ("metadata", metadata(Vec::new(), timeline.metadata())),
    ("name", text(timeline.name())),
    ("global_start_time", rational_time(ruler.rate, start)),
    (
      "tracks",
      object(vec![
        ("OTIO_SCHEMA", text("Stack.1")),
        ("metadata", Value::Object(Vec::new())),
        ("name", text("tracks")),
        ("source_range", Value::Null),
        ("effects", Value::Array(Vec::new())),
        ("markers", Value::Array(Vec::new())),
        ("enabled", Value::Bool(true)),
        ("children", Value::Array(tracks)),
      ]),
    ),
  ]))
}

fn track(
  index: usize,
  laid: &TrackLayout<'_>,
  ruler: &Ruler,
  target: OtioTarget,
) -> Result<Value, NotRepresentable> {
  let track = laid.track();
  let children = children(index, laid, ruler, target)?;
  Ok(object(vec![
    ("OTIO_SCHEMA", text("Track.1")),
    ("metadata", Value::Object(Vec::new())),
    ("name", text(track.name())),
    ("source_range", Value::Null),
    ("effects", Value::Array(Vec::new())),
    ("markers", Value::Array(Vec::new())),
    ("enabled", Value::Bool(track.enabled())),
    ("children", Value::Array(children)),
    (
      "kind",
      text(match track.kind() {
        TrackKind::Video => "Video",
        TrackKind::Audio => "Audio",
      }),
    ),
  ]))
}

/// A track's children in OpenTimelineIO's order: items end to end, each
/// transition between the two items it joins.
///
/// A fade is a dissolve against a gap: a fade-in after the gap before its
/// clip, a fade-out before the gap after it. Where the clip abuts another
/// clip or a track end, a gap of no length stands in, so the fade always has
/// black (or silence) on its other side and takes no time from a neighbour.
///
/// Each clip's record is held within 2^53 where it starts and ends: those
/// are the sums of the lengths before it, which OpenTimelineIO adds up at
/// the edit rate.
fn children(
  track_index: usize,
  laid: &TrackLayout<'_>,
  ruler: &Ruler,
  target: OtioTarget,
) -> Result<Vec<Value>, NotRepresentable> {
  let track = laid.track();
  let items = laid.items();
  let mut out = Vec::with_capacity(items.len() * 2);
  let mut after_gap = false;
  // The clip the walk reaches next. The layout derives a gap only before a
  // clip, and a gap is named by the clip it leads to.
  let mut next = 0;
  for (index, item) in items.iter().enumerate() {
    let at = ClipAt::new(track_index, next);
    match item {
      Item::Gap(range) => {
        out.push(gap(length_of(*range), ruler, at)?);
        after_gap = true;
      }
      Item::Clip(clip) => {
        next += 1;
        let record = clip.record();
        hold(i128::from(record.start_pts()), Spot::Record(at))?;
        hold(i128::from(record.end_pts()), Spot::Record(at))?;
        let fades = clip.fades();
        if let Some(fade) = fades.in_() {
          if !after_gap {
            out.push(gap(Duration::new(0, Timebase::default()), ruler, at)?);
          }
          out.push(fade_transition(fade, EdgeAt::new(at, Edge::In), ruler)?);
        }
        out.push(self::clip(clip, at, target)?);
        after_gap = false;
        if let Some(fade) = fades.out() {
          out.push(fade_transition(fade, EdgeAt::new(at, Edge::Out), ruler)?);
          if !matches!(items.get(index + 1), Some(Item::Gap(_))) {
            out.push(gap(Duration::new(0, Timebase::default()), ruler, at)?);
            after_gap = true;
          }
        } else if let Some((which, transition)) = leaving(track, clip) {
          let at = TransitionAt::new(track_index, which);
          out.push(dissolve(transition, at, ruler)?);
        }
      }
    }
  }
  Ok(out)
}

/// The transition at the cut where `clip`'s record ends, and its index.
fn leaving<'a>(track: &'a Track, clip: &Clip) -> Option<(usize, &'a Transition)> {
  let end = clip.record().end();
  track
    .transitions()
    .iter()
    .enumerate()
    .find(|(_, transition)| transition.at() == end)
}

fn clip(clip: &Clip, at: ClipAt, target: OtioTarget) -> Result<Value, NotRepresentable> {
  let mut words = vec![("id", text(clip.id().as_str()))];
  if let Some(gain) = clip.gain() {
    words.push(("gain_db", Value::Number(format!("{:?}", gain.db()))));
  }
  let media = clip.media();
  // The source range whole, start and length in the medium's one ruler, so
  // the end OpenTimelineIO derives from it — the start rescaled to the
  // duration's rate, plus the duration — is the stored end. Validation holds
  // the source's length to a whole number of edit-rate ticks, so the same
  // duration is the record's length: the clip fills exactly its record.
  let source_range = media_range(clip.source_range(), media.rate(), Spot::Source(at))?;
  let mut members = vec![
    (
      "OTIO_SCHEMA",
      text(match target {
        OtioTarget::V0_15Plus => "Clip.2",
        OtioTarget::Legacy => "Clip.1",
      }),
    ),
    ("metadata", metadata(words, clip.metadata())),
    ("name", text(clip.name())),
    ("source_range", source_range),
    ("effects", Value::Array(Vec::new())),
    ("markers", Value::Array(Vec::new())),
    ("enabled", Value::Bool(clip.enabled())),
  ];
  let reference = external_reference(media, at, target)?;
  match target {
    OtioTarget::V0_15Plus => {
      members.push(("media_references", object(vec![(DEFAULT_MEDIA, reference)])));
      members.push(("active_media_reference_key", text(DEFAULT_MEDIA)));
    }
    OtioTarget::Legacy => members.push(("media_reference", reference)),
  }
  Ok(object(members))
}

fn external_reference(
  media: &MediaRef,
  at: ClipAt,
  target: OtioTarget,
) -> Result<Value, NotRepresentable> {
  let mut words = Vec::new();
  if let Some(rate) = media.rate() {
    words.push(("rate", text(&rate.to_string())));
  }
  if let Some(reel) = media.reel() {
    words.push(("reel", text(reel)));
  }
  let available = match media.available_range() {
    Some(range) => media_range(range, media.rate(), Spot::Available(at))?,
    None => Value::Null,
  };
  let mut members = vec![
    ("OTIO_SCHEMA", text("ExternalReference.1")),
    ("metadata", metadata(words, &Metadata::new())),
    ("name", text("")),
    ("available_range", available),
  ];
  if let OtioTarget::V0_15Plus = target {
    members.push(("available_image_bounds", Value::Null));
  }
  members.push(("target_url", text(media.locator())));
  Ok(object(members))
}

/// A gap of `length` at the edit rate, before the clip `at`.
fn gap(length: Duration, ruler: &Ruler, at: ClipAt) -> Result<Value, NotRepresentable> {
  let spot = Spot::Record(at);
  Ok(object(vec![
    ("OTIO_SCHEMA", text("Gap.1")),
    ("metadata", Value::Object(Vec::new())),
    ("name", text("")),
    (
      "source_range",
      time_range(
        rational_time(ruler.rate, count(i128::from(length.ticks()), spot)?),
        rational_time(ruler.rate, count(0, spot)?),
      ),
    ),
    ("effects", Value::Array(Vec::new())),
    ("markers", Value::Array(Vec::new())),
    ("enabled", Value::Bool(true)),
  ]))
}

/// A fade as a dissolve: a fade-in runs `duration` after the cut from its
/// gap, a fade-out `duration` before the cut to its gap — inside the clip
/// either way, so it plays no handle.
fn fade_transition(fade: Fade, at: EdgeAt, ruler: &Ruler) -> Result<Value, NotRepresentable> {
  let spot = Spot::Fade(at);
  let length = count(i128::from(fade.duration().ticks()), spot)?;
  let none = count(0, spot)?;
  let (word, in_offset, out_offset) = match at.edge() {
    Edge::In => ("in", none, length),
    Edge::Out => ("out", length, none),
  };
  let shape = match fade.shape() {
    FadeShape::Linear => "linear",
    FadeShape::EqualPower => "equal_power",
  };
  Ok(transition(
    metadata(
      vec![("fade", text(word)), ("shape", text(shape))],
      &Metadata::new(),
    ),
    rational_time(ruler.rate, in_offset),
    rational_time(ruler.rate, out_offset),
  ))
}

fn dissolve(
  transition: &Transition,
  at: TransitionAt,
  ruler: &Ruler,
) -> Result<Value, NotRepresentable> {
  let spot = Spot::Transition(at);
  Ok(self::transition(
    Value::Object(Vec::new()),
    rational_time(
      ruler.rate,
      count(i128::from(transition.in_offset().ticks()), spot)?,
    ),
    rational_time(
      ruler.rate,
      count(i128::from(transition.out_offset().ticks()), spot)?,
    ),
  ))
}

fn transition(metadata: Value, in_offset: Value, out_offset: Value) -> Value {
  object(vec![
    ("OTIO_SCHEMA", text("Transition.1")),
    ("metadata", metadata),
    ("name", text("")),
    ("in_offset", in_offset),
    ("out_offset", out_offset),
    ("transition_type", text("SMPTE_Dissolve")),
  ])
}

/// A media-side range — a source range or an available range — start and
/// length in one ruler, never two, exact, and held: the start, the length and
/// the end they make each within ±2^53. The first ruler that holds it:
///
/// 1. frames of the medium's stated rate, when both land on whole frames;
/// 2. ticks of the range's own timebase;
/// 3. the coarsest ruler of a whole number of ticks a second in which both
///    land on a tick ([`coarsest`]).
///
/// A range none holds is refused with its own count past 2^53.
fn media_range(range: TimeRange, rate: Option<Rate>, at: Spot) -> Result<Value, NotRepresentable> {
  let length = length_of(range);
  let frames = media_ruler(rate).and_then(|(per_second, ruler)| {
    let start = range.start().checked_rescale_with(ruler, Rounding::Exact)?;
    let frames = length.checked_rescale_with(ruler, Rounding::Exact)?;
    Some(Counted::new(per_second, start.pts(), frames.ticks()))
  });
  if let Some(frames) = frames.filter(Counted::held) {
    return Ok(frames.time_range());
  }
  let ticks = Counted::new(
    ticks_per_second(range.timebase()),
    range.start_pts(),
    length.ticks(),
  );
  let Some(value) = ticks.past_exact() else {
    return Ok(ticks.time_range());
  };
  match coarsest(range, length).filter(Counted::held) {
    Some(counted) => Ok(counted.time_range()),
    None => Err(NotRepresentable { at, value }),
  }
}

/// A range counted in one ruler: the ruler's rate, as OpenTimelineIO's
/// `f64`, and the range's start and length in it.
#[derive(Clone, Copy)]
struct Counted {
  per_second: f64,
  start: i128,
  length: i128,
}

impl Counted {
  fn new(per_second: f64, start: i64, length: u64) -> Self {
    Self {
      per_second,
      start: i128::from(start),
      length: i128::from(length),
    }
  }

  /// The first of the start, the length and the end OpenTimelineIO derives
  /// from them (the two added, in this one ruler) past ±2^53.
  fn past_exact(self) -> Option<i128> {
    [self.start, self.length, self.start + self.length]
      .into_iter()
      .find(|&count| !exact(count))
  }

  fn held(&self) -> bool {
    self.past_exact().is_none()
  }

  fn time_range(self) -> Value {
    time_range(
      rational_time(self.per_second, number(self.length)),
      rational_time(self.per_second, number(self.start)),
    )
  }
}

/// The coarsest ruler of a whole number of ticks a second in which `range`
/// starts and runs `length` on whole ticks, both recounted there by
/// `mediatime`'s exact rescale; `None` for a range in a degenerate timebase,
/// which a valid timeline never counts one in.
///
/// `n` ticks of `num/den` seconds are a whole number of ticks of `1/r`
/// seconds exactly when `den` divides `n·num·r`. For the start and the
/// length together that is when `den` divides `g·num·r`, `g` their greatest
/// common divisor, so the least `r` — the coarsest ruler — is
/// `den / gcd(den, g·num)`. A whole rate is an exact `f64`. Choosing the
/// ruler is the one sum here; the recount is `mediatime`'s, which answers
/// only where the ruler holds the range exactly.
fn coarsest(range: TimeRange, length: Duration) -> Option<Counted> {
  let timebase = range.timebase();
  let num = u128::try_from(timebase.num())
    .ok()
    .filter(|&num| num != 0)?;
  let den = u128::try_from(timebase.den().get()).ok()?;
  let common = gcd(
    u128::from(range.start_pts().unsigned_abs()),
    u128::from(length.ticks()),
  );
  let per_second = den / gcd(den, common * num);
  let ruler = Timebase::new(1, NonZeroI32::new(i32::try_from(per_second).ok()?)?);
  let start = range.start().checked_rescale_with(ruler, Rounding::Exact)?;
  let ticks = length.checked_rescale_with(ruler, Rounding::Exact)?;
  Some(Counted::new(per_second as f64, start.pts(), ticks.ticks()))
}

/// Euclid's greatest common divisor; `gcd(n, 0)` is `n`.
fn gcd(mut a: u128, mut b: u128) -> u128 {
  while b != 0 {
    (a, b) = (b, a % b);
  }
  a
}

/// A range's length. Only a valid timeline is exported, and validation
/// holds every range of it within `i64::MAX` ticks, the gaps between its
/// records with them.
fn length_of(range: TimeRange) -> Duration {
  span(range).unwrap_or_default()
}

/// A stated media rate, as OpenTimelineIO's `f64` and as the timebase one
/// frame of it is counted in.
fn media_ruler(rate: Option<Rate>) -> Option<(f64, Timebase)> {
  let rate = rate?;
  Some((rate.as_f64(), rate.checked_to_timebase()?))
}

/// How many ticks of `timebase` pass in a second: the rate OpenTimelineIO
/// writes beside a count in it. A valid timeline counts nothing in a
/// degenerate timebase.
fn ticks_per_second(timebase: Timebase) -> f64 {
  Rate::checked_from_timebase(timebase).map_or(0.0, |rate| rate.as_f64())
}

/// The metadata object: this crate's own `words` and the model's `notes`
/// under [`OURS`], or nothing when there are neither.
fn metadata(mut words: Vec<(&'static str, Value)>, notes: &Metadata) -> Value {
  if !notes.is_empty() {
    let notes = notes
      .iter()
      .map(|(key, value)| (String::from(key), text(value)))
      .collect();
    words.push(("metadata", Value::Object(notes)));
  }
  if words.is_empty() {
    Value::Object(Vec::new())
  } else {
    object(vec![(OURS, object(words))])
  }
}

fn rational_time(rate: f64, value: Value) -> Value {
  object(vec![
    ("OTIO_SCHEMA", text("RationalTime.1")),
    ("rate", Value::Number(format!("{rate:?}"))),
    ("value", value),
  ])
}

fn time_range(duration: Value, start_time: Value) -> Value {
  object(vec![
    ("OTIO_SCHEMA", text("TimeRange.1")),
    ("duration", duration),
    ("start_time", start_time),
  ])
}

/// Whether an `f64` holds `count` exactly: whether it lies within ±2^53.
fn exact(count: i128) -> bool {
  count.unsigned_abs() <= EXACT
}

/// Refuses `value`, as sitting at `at`, where an `f64` would hold it only
/// rounded.
fn hold(value: i128, at: Spot) -> Result<(), NotRepresentable> {
  if exact(value) {
    Ok(())
  } else {
    Err(NotRepresentable { at, value })
  }
}

/// A whole count as OpenTimelineIO's `f64` writes one, `86400.0` — refused
/// where an `f64` would hold it only rounded.
fn count(value: i128, at: Spot) -> Result<Value, NotRepresentable> {
  hold(value, at)?;
  Ok(number(value))
}

/// A whole count written as `86400.0`, from the integer's own digits.
fn number(value: i128) -> Value {
  Value::Number(format!("{value}.0"))
}

fn text(value: &str) -> Value {
  Value::String(String::from(value))
}

fn object(members: Vec<(&'static str, Value)>) -> Value {
  Value::Object(
    members
      .into_iter()
      .map(|(key, value)| (String::from(key), value))
      .collect(),
  )
}

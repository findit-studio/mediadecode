//! A valid timeline as OpenTimelineIO's object tree.
//!
//! Every object is written with its keys in the order OpenTimelineIO's own
//! writer uses: `OTIO_SCHEMA` first, then each base class's fields before
//! the derived class's.

use alloc::{
  format,
  string::{String, ToString},
  vec,
  vec::Vec,
};

use mediatime::{Duration, Rate, Rounding, TimeRange, Timebase, Timestamp};

use super::{OtioTarget, json::Value};
use crate::{
  Clip, Fade, FadeShape, Item, Layout, MediaRef, Metadata, Timeline, Track, TrackKind, TrackLayout,
  Transition, time::span,
};

/// The key under which this crate's own words ride in an object's
/// `metadata`.
const OURS: &str = "mediaio";

/// The key a `Clip.2` keeps its one media reference under.
const DEFAULT_MEDIA: &str = "DEFAULT_MEDIA";

struct Ruler {
  /// The edit rate, as the `f64` OpenTimelineIO counts in.
  rate: f64,
}

pub(crate) fn timeline(timeline: &Timeline, laid: &Layout<'_>, target: OtioTarget) -> Value {
  let ruler = Ruler {
    rate: timeline.rate().as_f64(),
  };
  let tracks = laid
    .tracks()
    .iter()
    .map(|track| self::track(track, &ruler, target))
    .collect();
  object(vec![
    ("OTIO_SCHEMA", text("Timeline.1")),
    ("metadata", metadata(Vec::new(), timeline.metadata())),
    ("name", text(timeline.name())),
    (
      "global_start_time",
      rational_time(ruler.rate, count(timeline.start().pts())),
    ),
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
  ])
}

fn track(laid: &TrackLayout<'_>, ruler: &Ruler, target: OtioTarget) -> Value {
  let track = laid.track();
  object(vec![
    ("OTIO_SCHEMA", text("Track.1")),
    ("metadata", Value::Object(Vec::new())),
    ("name", text(track.name())),
    ("source_range", Value::Null),
    ("effects", Value::Array(Vec::new())),
    ("markers", Value::Array(Vec::new())),
    ("enabled", Value::Bool(track.enabled())),
    ("children", Value::Array(children(laid, ruler, target))),
    (
      "kind",
      text(match track.kind() {
        TrackKind::Video => "Video",
        TrackKind::Audio => "Audio",
      }),
    ),
  ])
}

/// A track's children in OpenTimelineIO's order: items end to end, each
/// transition between the two items it joins.
///
/// A fade is a dissolve against a gap: a fade-in after the gap before its
/// clip, a fade-out before the gap after it. Where the clip abuts another
/// clip or a track end, a gap of no length stands in, so the fade always has
/// black (or silence) on its other side and takes no time from a neighbour.
fn children(laid: &TrackLayout<'_>, ruler: &Ruler, target: OtioTarget) -> Vec<Value> {
  let track = laid.track();
  let items = laid.items();
  let mut out = Vec::with_capacity(items.len() * 2);
  let mut after_gap = false;
  for (index, item) in items.iter().enumerate() {
    match item {
      Item::Gap(range) => {
        out.push(gap(span(*range), ruler));
        after_gap = true;
      }
      Item::Clip(clip) => {
        let fades = clip.fades();
        if let Some(fade) = fades.in_() {
          if !after_gap {
            out.push(gap(Duration::new(0, Timebase::default()), ruler));
          }
          out.push(fade_transition(fade, FadeEdge::In, ruler));
        }
        out.push(self::clip(clip, ruler, target));
        after_gap = false;
        if let Some(fade) = fades.out() {
          out.push(fade_transition(fade, FadeEdge::Out, ruler));
          if !matches!(items.get(index + 1), Some(Item::Gap(_))) {
            out.push(gap(Duration::new(0, Timebase::default()), ruler));
            after_gap = true;
          }
        } else if let Some(transition) = leaving(track, clip) {
          out.push(dissolve(transition, ruler));
        }
      }
    }
  }
  out
}

/// The transition at the cut where `clip`'s record ends.
fn leaving<'a>(track: &'a Track, clip: &Clip) -> Option<&'a Transition> {
  let end = clip.record().end();
  track
    .transitions()
    .iter()
    .find(|transition| transition.at() == end)
}

fn clip(clip: &Clip, ruler: &Ruler, target: OtioTarget) -> Value {
  let mut words = Vec::new();
  if let Some(gain) = clip.gain() {
    words.push(("gain_db", Value::Number(format!("{:?}", gain.db()))));
  }
  let media = clip.media();
  // How long the clip occupies its track is its record's length, at the
  // edit rate; where it starts in its medium is a media-side instant.
  let source_range = time_range(
    rational_time(ruler.rate, count_unsigned(span(clip.record()).ticks())),
    media_time(clip.source_range().start(), media.rate()),
  );
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
  let reference = external_reference(media, target);
  match target {
    OtioTarget::V0_15Plus => {
      members.push(("media_references", object(vec![(DEFAULT_MEDIA, reference)])));
      members.push(("active_media_reference_key", text(DEFAULT_MEDIA)));
    }
    OtioTarget::Legacy => members.push(("media_reference", reference)),
  }
  object(members)
}

fn external_reference(media: &MediaRef, target: OtioTarget) -> Value {
  let mut words = Vec::new();
  if let Some(rate) = media.rate() {
    words.push(("rate", text(&rate.to_string())));
  }
  if let Some(reel) = media.reel() {
    words.push(("reel", text(reel)));
  }
  let available = match media.available_range() {
    Some(range) => media_range(range, media.rate()),
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
  object(members)
}

fn gap(length: Duration, ruler: &Ruler) -> Value {
  object(vec![
    ("OTIO_SCHEMA", text("Gap.1")),
    ("metadata", Value::Object(Vec::new())),
    ("name", text("")),
    (
      "source_range",
      time_range(
        rational_time(ruler.rate, count_unsigned(length.ticks())),
        rational_time(ruler.rate, count(0)),
      ),
    ),
    ("effects", Value::Array(Vec::new())),
    ("markers", Value::Array(Vec::new())),
    ("enabled", Value::Bool(true)),
  ])
}

#[derive(Clone, Copy)]
enum FadeEdge {
  In,
  Out,
}

/// A fade as a dissolve: a fade-in runs `duration` after the cut from its
/// gap, a fade-out `duration` before the cut to its gap — inside the clip
/// either way, so it plays no handle.
fn fade_transition(fade: Fade, edge: FadeEdge, ruler: &Ruler) -> Value {
  let length = count_unsigned(fade.duration().ticks());
  let none = count(0);
  let (word, in_offset, out_offset) = match edge {
    FadeEdge::In => ("in", none, length),
    FadeEdge::Out => ("out", length, none),
  };
  let shape = match fade.shape() {
    FadeShape::Linear => "linear",
    FadeShape::EqualPower => "equal_power",
  };
  transition(
    metadata(
      vec![("fade", text(word)), ("shape", text(shape))],
      &Metadata::new(),
    ),
    rational_time(ruler.rate, in_offset),
    rational_time(ruler.rate, out_offset),
  )
}

fn dissolve(transition: &Transition, ruler: &Ruler) -> Value {
  self::transition(
    Value::Object(Vec::new()),
    rational_time(ruler.rate, count_unsigned(transition.in_offset().ticks())),
    rational_time(ruler.rate, count_unsigned(transition.out_offset().ticks())),
  )
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

/// A media-side instant: in frames of the medium's stated rate when it lands
/// on one, else in ticks of its own timebase — exact either way.
fn media_time(at: Timestamp, rate: Option<Rate>) -> Value {
  if let Some((per_second, ruler)) = media_ruler(rate)
    && let Some(frames) = at.checked_rescale_with(ruler, Rounding::Exact)
  {
    return rational_time(per_second, count(frames.pts()));
  }
  rational_time(ticks_per_second(at.timebase()), count(at.pts()))
}

/// A media-side range, start and length in one ruler: frames of the
/// medium's stated rate when both land on whole frames, else ticks of the
/// range's own timebase.
fn media_range(range: TimeRange, rate: Option<Rate>) -> Value {
  let length = span(range);
  if let Some((per_second, ruler)) = media_ruler(rate)
    && let Some(start) = range.start().checked_rescale_with(ruler, Rounding::Exact)
    && let Some(frames) = length.checked_rescale_with(ruler, Rounding::Exact)
  {
    return time_range(
      rational_time(per_second, count_unsigned(frames.ticks())),
      rational_time(per_second, count(start.pts())),
    );
  }
  let per_second = ticks_per_second(range.timebase());
  time_range(
    rational_time(per_second, count_unsigned(length.ticks())),
    rational_time(per_second, count(range.start_pts())),
  )
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

/// A whole count as OpenTimelineIO's `f64` writes one: `86400.0`. Written
/// from the integer's own digits, so a count past 2^53 keeps every digit.
fn count(value: i64) -> Value {
  Value::Number(format!("{value}.0"))
}

fn count_unsigned(value: u64) -> Value {
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

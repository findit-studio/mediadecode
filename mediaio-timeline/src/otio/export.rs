//! A valid timeline as OpenTimelineIO's object tree.
//!
//! Every object is written with its keys in the order OpenTimelineIO's own
//! writer uses: `OTIO_SCHEMA` first, then each base class's fields before
//! the derived class's.
//!
//! Each track is planned, walked and written. The plan counts every child
//! — each count written within ±2^53, the whole numbers an `f64` holds
//! exactly — and the walk, [`mod@derive`], forms what OpenTimelineIO
//! derives from those counts, in its own arithmetic, holding each. A track
//! the walk refuses is walked again with its clips in the coarsest whole
//! rulers their source ranges allow; a track neither walk holds is refused,
//! with the first value the first walk could not hold.

use alloc::{
  format,
  string::{String, ToString},
  vec,
  vec::Vec,
};
use core::num::NonZeroI32;

use mediatime::{Duration, Rate, Rounding, TimeRange, Timebase};

use super::{
  NotRepresentable, OtioTarget, Spot,
  derive::{self, Child, Ruler, Time},
  json::Value,
};
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

pub(crate) fn timeline(
  timeline: &Timeline,
  laid: &Layout<'_>,
  target: OtioTarget,
) -> Result<Value, NotRepresentable> {
  let edit = Ruler::new(timeline.rate());
  let start = i128::from(timeline.start().pts());
  held(start, edit, Spot::Start)?;
  let global = Time::written(start, edit);
  let mut tracks = Vec::with_capacity(laid.tracks().len());
  let mut durations = Vec::with_capacity(laid.tracks().len());
  for (index, laid) in laid.tracks().iter().enumerate() {
    let (children, duration) = settle(index, plan(index, laid, edit)?, edit, global)?;
    durations.push(duration);
    tracks.push(track(laid.track(), &children, edit, target));
  }
  derive::stack(&durations)?;
  Ok(object(vec![
    ("OTIO_SCHEMA", text("Timeline.1")),
    ("metadata", metadata(Vec::new(), timeline.metadata())),
    ("name", text(timeline.name())),
    ("global_start_time", rational_time(edit, start)),
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

/// One child of a track as the export writes it, counted.
#[derive(Clone, Copy)]
enum Planned<'a> {
  /// A gap of so many edit-rate ticks.
  Gap(u64),
  /// A clip, its source range and its medium's available range counted
  /// each in one ruler.
  Clip {
    clip: &'a Clip,
    source: Counted,
    available: Option<Counted>,
  },
  /// A fade at one edge of a clip, as a dissolve against a gap.
  Fade(Fade, Edge),
  /// A transition between two clips.
  Dissolve(&'a Transition),
}

/// A track's children in OpenTimelineIO's order — items end to end, each
/// transition between the two items it joins — each count written held
/// within ±2^53.
///
/// A fade is a dissolve against a gap: a fade-in after the gap before its
/// clip, a fade-out before the gap after it. Where the clip abuts another
/// clip or a track end, a gap of no length stands in, so the fade always has
/// black (or silence) on its other side and takes no time from a neighbour.
fn plan<'a>(
  track_index: usize,
  laid: &TrackLayout<'a>,
  edit: Ruler,
) -> Result<Vec<Planned<'a>>, NotRepresentable> {
  let track = laid.track();
  let items = laid.items();
  let mut out = Vec::with_capacity(items.len() * 2);
  let mut after_gap = false;
  // The clip the walk reaches next. The layout derives a gap only before a
  // clip, and a gap is named by the clip it leads to.
  let mut next = 0;
  for (index, &item) in items.iter().enumerate() {
    let at = ClipAt::new(track_index, next);
    match item {
      Item::Gap(range) => {
        let length = length_of(range).ticks();
        held(i128::from(length), edit, Spot::Record(at))?;
        out.push(Planned::Gap(length));
        after_gap = true;
      }
      Item::Clip(clip) => {
        next += 1;
        let fades = clip.fades();
        if let Some(fade) = fades.in_() {
          if !after_gap {
            out.push(Planned::Gap(0));
          }
          out.push(fade_planned(fade, EdgeAt::new(at, Edge::In), edit)?);
        }
        out.push(clip_planned(clip, at)?);
        after_gap = false;
        if let Some(fade) = fades.out() {
          out.push(fade_planned(fade, EdgeAt::new(at, Edge::Out), edit)?);
          if !matches!(items.get(index + 1), Some(Item::Gap(_))) {
            out.push(Planned::Gap(0));
            after_gap = true;
          }
        } else if let Some((which, transition)) = leaving(track, clip) {
          let spot = Spot::Transition(TransitionAt::new(track_index, which));
          held(i128::from(transition.in_offset().ticks()), edit, spot)?;
          held(i128::from(transition.out_offset().ticks()), edit, spot)?;
          out.push(Planned::Dissolve(transition));
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

fn fade_planned(fade: Fade, at: EdgeAt, edit: Ruler) -> Result<Planned<'static>, NotRepresentable> {
  held(i128::from(fade.duration().ticks()), edit, Spot::Fade(at))?;
  Ok(Planned::Fade(fade, at.edge()))
}

/// The source range whole, start and length in the medium's one ruler, so
/// the end OpenTimelineIO derives from it — the start rescaled to the
/// duration's rate, plus the duration — is the stored end. Validation holds
/// the source's length to a whole number of edit-rate ticks, so the same
/// duration is the record's length: the clip fills exactly its record.
fn clip_planned(clip: &Clip, at: ClipAt) -> Result<Planned<'_>, NotRepresentable> {
  let media = clip.media();
  let source = media_range(clip.source_range(), media.rate(), Spot::Source(at))?;
  let available = match media.available_range() {
    Some(range) => Some(media_range(range, media.rate(), Spot::Available(at))?),
    None => None,
  };
  Ok(Planned::Clip {
    clip,
    source,
    available,
  })
}

/// The track as planned when the walk holds it, else as planned with each
/// clip in the coarsest whole ruler its source range allows, where that is
/// coarser than its own — the rate OpenTimelineIO carries the track's sums
/// in is the highest among them — when the walk holds that. Answers the
/// children to write and the track's duration as OpenTimelineIO sums it;
/// refuses with the first walk's refusal.
fn settle<'a>(
  index: usize,
  planned: Vec<Planned<'a>>,
  edit: Ruler,
  global: Time,
) -> Result<(Vec<Planned<'a>>, Time), NotRepresentable> {
  let refusal = match derive::track(index, &walked(&planned, edit), global) {
    Ok(duration) => return Ok((planned, duration)),
    Err(refusal) => refusal,
  };
  let mut coarser = planned;
  let mut changed = false;
  for child in &mut coarser {
    if let Planned::Clip { clip, source, .. } = child {
      let range = clip.source_range();
      if let Some(counted) = coarsest(range, length_of(range))
        .filter(|counted| counted.held() && counted.ruler.otio() < source.ruler.otio())
      {
        *source = counted;
        changed = true;
      }
    }
  }
  if changed && let Ok(duration) = derive::track(index, &walked(&coarser, edit), global) {
    return Ok((coarser, duration));
  }
  Err(refusal)
}

/// The planned children as the walk reads them.
fn walked(planned: &[Planned<'_>], edit: Ruler) -> Vec<Child> {
  let ticks = |count: u64| Time::written(i128::from(count), edit);
  planned
    .iter()
    .map(|child| match *child {
      Planned::Gap(length) => Child::Item {
        start: ticks(0),
        duration: ticks(length),
      },
      Planned::Clip { source, .. } => Child::Item {
        start: Time::written(source.start, source.ruler),
        duration: Time::written(source.length, source.ruler),
      },
      Planned::Fade(fade, edge) => {
        let (in_offset, out_offset) = fade_offsets(fade, edge);
        Child::Transition {
          in_offset: ticks(in_offset),
          out_offset: ticks(out_offset),
        }
      }
      Planned::Dissolve(transition) => Child::Transition {
        in_offset: ticks(transition.in_offset().ticks()),
        out_offset: ticks(transition.out_offset().ticks()),
      },
    })
    .collect()
}

/// A fade's offsets in edit-rate ticks: a fade-in runs its length after the
/// cut from its gap, a fade-out its length before the cut to its gap —
/// inside the clip either way, so it plays no handle.
fn fade_offsets(fade: Fade, edge: Edge) -> (u64, u64) {
  let length = fade.duration().ticks();
  match edge {
    Edge::In => (0, length),
    Edge::Out => (length, 0),
  }
}

fn track(track: &Track, children: &[Planned<'_>], edit: Ruler, target: OtioTarget) -> Value {
  let children = children
    .iter()
    .map(|child| match *child {
      Planned::Gap(length) => gap(length, edit),
      Planned::Clip {
        clip,
        source,
        available,
      } => self::clip(clip, source, available, target),
      Planned::Fade(fade, edge) => fade_transition(fade, edge, edit),
      Planned::Dissolve(transition) => dissolve(transition, edit),
    })
    .collect();
  object(vec![
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
  ])
}

fn clip(clip: &Clip, source: Counted, available: Option<Counted>, target: OtioTarget) -> Value {
  let mut words = vec![("id", text(clip.id().as_str()))];
  if let Some(gain) = clip.gain() {
    words.push(("gain_db", Value::Number(format!("{:?}", gain.db()))));
  }
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
    ("source_range", source.time_range()),
    ("effects", Value::Array(Vec::new())),
    ("markers", Value::Array(Vec::new())),
    ("enabled", Value::Bool(clip.enabled())),
  ];
  let reference = external_reference(clip.media(), available, target);
  match target {
    OtioTarget::V0_15Plus => {
      members.push(("media_references", object(vec![(DEFAULT_MEDIA, reference)])));
      members.push(("active_media_reference_key", text(DEFAULT_MEDIA)));
    }
    OtioTarget::Legacy => members.push(("media_reference", reference)),
  }
  object(members)
}

fn external_reference(media: &MediaRef, available: Option<Counted>, target: OtioTarget) -> Value {
  let mut words = Vec::new();
  if let Some(rate) = media.rate() {
    words.push(("rate", text(&rate.to_string())));
  }
  if let Some(reel) = media.reel() {
    words.push(("reel", text(reel)));
  }
  let mut members = vec![
    ("OTIO_SCHEMA", text("ExternalReference.1")),
    ("metadata", metadata(words, &Metadata::new())),
    ("name", text("")),
    (
      "available_range",
      available.map_or(Value::Null, Counted::time_range),
    ),
  ];
  if let OtioTarget::V0_15Plus = target {
    members.push(("available_image_bounds", Value::Null));
  }
  members.push(("target_url", text(media.locator())));
  object(members)
}

/// A gap of `length` edit-rate ticks.
fn gap(length: u64, edit: Ruler) -> Value {
  object(vec![
    ("OTIO_SCHEMA", text("Gap.1")),
    ("metadata", Value::Object(Vec::new())),
    ("name", text("")),
    (
      "source_range",
      time_range(
        rational_time(edit, i128::from(length)),
        rational_time(edit, 0),
      ),
    ),
    ("effects", Value::Array(Vec::new())),
    ("markers", Value::Array(Vec::new())),
    ("enabled", Value::Bool(true)),
  ])
}

/// A fade as a dissolve against its gap.
fn fade_transition(fade: Fade, edge: Edge, edit: Ruler) -> Value {
  let (in_offset, out_offset) = fade_offsets(fade, edge);
  let word = match edge {
    Edge::In => "in",
    Edge::Out => "out",
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
    rational_time(edit, i128::from(in_offset)),
    rational_time(edit, i128::from(out_offset)),
  )
}

fn dissolve(transition: &Transition, edit: Ruler) -> Value {
  self::transition(
    Value::Object(Vec::new()),
    rational_time(edit, i128::from(transition.in_offset().ticks())),
    rational_time(edit, i128::from(transition.out_offset().ticks())),
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
fn media_range(
  range: TimeRange,
  rate: Option<Rate>,
  at: Spot,
) -> Result<Counted, NotRepresentable> {
  let length = length_of(range);
  let frames = rate.and_then(|rate| {
    let ruler = rate.checked_to_timebase()?;
    let start = range.start().checked_rescale_with(ruler, Rounding::Exact)?;
    let frames = length.checked_rescale_with(ruler, Rounding::Exact)?;
    Some(Counted::new(Ruler::new(rate), start.pts(), frames.ticks()))
  });
  if let Some(frames) = frames.filter(Counted::held) {
    return Ok(frames);
  }
  let own = Ruler::new(Rate::checked_from_timebase(range.timebase()).unwrap_or(Rate::hz(0)));
  let ticks = Counted::new(own, range.start_pts(), length.ticks());
  let Some(value) = ticks.past_exact() else {
    return Ok(ticks);
  };
  coarsest(range, length)
    .filter(Counted::held)
    .ok_or(NotRepresentable {
      at,
      value,
      rate: own.exact(),
    })
}

/// A range counted in one ruler: the ruler, and the range's start and
/// length in it.
#[derive(Clone, Copy)]
struct Counted {
  ruler: Ruler,
  start: i128,
  length: i128,
}

impl Counted {
  fn new(ruler: Ruler, start: i64, length: u64) -> Self {
    Self {
      ruler,
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
      rational_time(self.ruler, self.length),
      rational_time(self.ruler, self.start),
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
  let per_second = i32::try_from(den / gcd(den, common * num)).ok()?;
  let ruler = Timebase::new(1, NonZeroI32::new(per_second)?);
  let start = range.start().checked_rescale_with(ruler, Rounding::Exact)?;
  let ticks = length.checked_rescale_with(ruler, Rounding::Exact)?;
  Some(Counted::new(
    Ruler::new(Rate::hz(per_second)),
    start.pts(),
    ticks.ticks(),
  ))
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

/// `count` ticks of `ruler`: its rate written as the `f64` nearest it, in
/// the shortest spelling that reads back as that `f64`, and the count as
/// [`number`] writes one.
fn rational_time(ruler: Ruler, count: i128) -> Value {
  object(vec![
    ("OTIO_SCHEMA", text("RationalTime.1")),
    ("rate", Value::Number(format!("{:?}", ruler.otio()))),
    ("value", number(count)),
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

/// Refuses `value` ticks of `ruler`, as sitting at `at`, where an `f64`
/// would hold the count only rounded.
fn held(value: i128, ruler: Ruler, at: Spot) -> Result<(), NotRepresentable> {
  if exact(value) {
    Ok(())
  } else {
    Err(NotRepresentable {
      at,
      value,
      rate: ruler.exact(),
    })
  }
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

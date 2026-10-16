//! OpenTimelineIO read back into a timeline: the exact inverse of the
//! export, and a refusal by name of everything the timeline does not model.
//!
//! The reader walks the document the export writes — `Timeline.1`, one
//! `Stack.1`, its `Track.1`s, their `Gap.1`s, `Clip.1`s or `Clip.2`s and
//! `Transition.1`s, each clip's `ExternalReference.1`, and `Marker.2`s on
//! tracks and clips — and nothing else: a schema the timeline does not
//! model, or models elsewhere, is refused with its name and where it sits
//! ([`Unreadable::Schema`]); a key, a value or a note of a modelled schema
//! the timeline has no word for is refused by name too
//! ([`Unreadable::Feature`]). Nothing is dropped: a key is read only where
//! it carries nothing — `null`, an empty list, `true` for `enabled` — or
//! what the timeline keeps.
//!
//! A member OpenTimelineIO's schema lets a document leave out reads as
//! OpenTimelineIO 0.17.0 reads it — its reader builds each object with its
//! default constructor and reads such a member only if present — so an
//! absent name reads as empty, an item's absent `enabled` as `true`, and a
//! marker's absent color as GREEN and its comment as empty; a `Clip.1` with
//! no media reference is a clip with no medium, refused by name as a `null`
//! one is. A member whose default is no value at all, where the timeline
//! needs one — the timeline's `global_start_time`, a clip's `source_range`
//! — is refused by name, and so is a gap's absent `source_range`, whose
//! default, `TimeRange()`, is nothing at rate 1: a rate the timeline does
//! not keep, which OpenTimelineIO's sum of the track's duration takes.
//!
//! Every number is read exactly, or refused as [`Unreadable::Inexact`]: a
//! count or a place as the whole number its digits spell
//! ([`json::whole`]), a float as the one whose canonical decimal its text
//! spells ([`json::exact_f64`], [`json::exact_f32`]) — so `2.4e1` reads as
//! `24.0` does, and `16777217` does not read as the `f32` 16777216. Counts
//! are read where the export writes them, and held to the exact words it
//! writes beside them ([`export`](super::export)): each `RationalTime`'s
//! rate must be the `f64` of the exact ruler its word names, bit for bit,
//! and its value a whole count within ±2^53, which an `f64` holds exactly;
//! a media-side range is counted again in the timebase the model counts it
//! in, exactly. An edit-side value is counted in the timebase its word
//! spells, which must be the edit rate's reciprocal by value, and without a
//! word in that reciprocal; the model's notes are pairs, each key once.
//! What OpenTimelineIO derives from the document is held as the export
//! holds it ([`derive`](super::derive)), and refused as
//! [`Unreadable::Inexact`] where its `f64` arithmetic rounds: both ends of
//! every range read, and each track's walk and the stack's.
//! Records are not written: like OpenTimelineIO, the reader lays a track's
//! items end to end from zero, each clip as long as its source range at the
//! edit rate. A track's time ends with its last clip, so a gap of some
//! length after it — which OpenTimelineIO counts in the track's duration —
//! is refused by name ([`Feature::TrailingGap`]). A gap of no length is
//! read only where the export writes one, on a fade's gap side, and as it
//! writes one, its range stated at the edit rate; anywhere else
//! OpenTimelineIO still counts it — its range in its duration, its rate in
//! its track's sum — so it is refused by name too
//! ([`Feature::StandaloneGap`]).
//!
//! Last comes the backstop ([`reproduced`]): the export writes the timeline
//! read, and every `RationalTime` it writes must be the one the document
//! states at the same path, strictly — the same value and the same rate —
//! or the read is refused as inexact there. The checks before it refuse by
//! name what they know; the backstop closes the class of what is read but
//! would be written otherwise — a medium's range counted in a ruler the
//! export would not choose, among them — so the reader accepts exactly the
//! documents whose times the export reproduces.

use core::str::FromStr;
use std::{boxed::Box, vec, vec::Vec};

use mediatime::{Duration, Rate, Rounding, TimeRange, Timebase, Timestamp};
use smol_bytes::{Utf8Bytes, format_utf8bytes};

use super::{
  ChildAt, Feature, Inexact, NotRepresentable, OtioTarget, Shape, Spot, UnmodelledFeature,
  UnmodelledSchema, Unreadable,
  derive::{self, Ruler, Time},
  export::{self, DEFAULT_MEDIA, OURS},
  json::{self, Value},
};
use crate::{
  Clip, ClipId, Edge, Fade, FadeShape, Gain, Marker, MarkerColor, MediaRef, Metadata, Timeline,
  Track, TrackKind, Transition, layout::layout_valid,
};

type Members = [(Utf8Bytes, Value)];

/// Where a value sits, as a path from the document's root:
/// `$.tracks.children[0]`.
#[derive(Clone)]
struct At(Utf8Bytes);

impl At {
  fn root() -> Self {
    Self(Utf8Bytes::from_static("$"))
  }

  fn key(&self, key: &str) -> Self {
    Self(format_utf8bytes!("{}.{key}", self.0))
  }

  fn index(&self, index: usize) -> Self {
    Self(format_utf8bytes!("{}[{index}]", self.0))
  }
}

fn shape(at: &At, expected: &'static str) -> Unreadable {
  Unreadable::Shape(Shape {
    path: at.0.clone(),
    expected,
  })
}

fn unmodelled(at: &At, name: &str) -> Unreadable {
  Unreadable::Schema(UnmodelledSchema::new(Utf8Bytes::from(name), at.0.clone()))
}

fn feature(at: &At, feature: Feature) -> Unreadable {
  Unreadable::Feature(UnmodelledFeature::new(feature, at.0.clone()))
}

fn inexact(at: &At, reason: &'static str) -> Unreadable {
  Unreadable::Inexact(Inexact {
    path: at.0.clone(),
    reason,
  })
}

fn array<'a>(value: &'a Value, at: &At) -> Result<&'a [Value], Unreadable> {
  value.as_array().ok_or_else(|| shape(at, "an array"))
}

fn number<'a>(value: &'a Value, at: &At) -> Result<&'a str, Unreadable> {
  match value {
    Value::Number(text) => Ok(text.as_str()),
    _ => Err(shape(at, "a number")),
  }
}

/// An object of the document, and where it sits.
struct Object<'a> {
  members: &'a Members,
  at: At,
}

impl<'a> Object<'a> {
  fn of(value: &'a Value, at: At) -> Result<Self, Unreadable> {
    match value.as_object() {
      Some(members) => Ok(Self { members, at }),
      None => Err(shape(&at, "an object")),
    }
  }

  fn get(&self, key: &str) -> Option<&'a Value> {
    self
      .members
      .iter()
      .find(|(name, _)| name == key)
      .map(|(_, value)| value)
  }

  fn required(&self, key: &str) -> Result<&'a Value, Unreadable> {
    self
      .get(key)
      .ok_or_else(|| shape(&self.at.key(key), "a required key"))
  }

  /// The object's schema, refused by name where it is not one of `known`.
  fn schema(&self, known: &[&str]) -> Result<&'a str, Unreadable> {
    let name = self
      .get("OTIO_SCHEMA")
      .and_then(Value::as_str)
      .ok_or_else(|| shape(&self.at.key("OTIO_SCHEMA"), "a schema name"))?;
    if known.contains(&name) {
      Ok(name)
    } else {
      Err(unmodelled(&self.at, name))
    }
  }

  /// Refuses the first key not among `known`, by name.
  fn keys(&self, known: &[&str]) -> Result<(), Unreadable> {
    match self
      .members
      .iter()
      .find(|(key, _)| !known.contains(&key.as_str()))
    {
      Some((key, _)) => Err(feature(&self.at.key(key), Feature::Key(key.clone()))),
      None => Ok(()),
    }
  }

  fn string(&self, key: &str) -> Result<Option<&'a str>, Unreadable> {
    match self.get(key) {
      None => Ok(None),
      Some(value) => value
        .as_str()
        .map(Some)
        .ok_or_else(|| shape(&self.at.key(key), "a string")),
    }
  }

  fn boolean(&self, key: &str) -> Result<Option<bool>, Unreadable> {
    match self.get(key) {
      None => Ok(None),
      Some(Value::Bool(flag)) => Ok(Some(*flag)),
      Some(_) => Err(shape(&self.at.key(key), "a boolean")),
    }
  }

  /// The object's name, where the timeline keeps it; absent reads as empty.
  fn name(&self) -> Result<&'a str, Unreadable> {
    Ok(self.string("name")?.unwrap_or(""))
  }

  /// A name where the timeline keeps none: empty, or one of `also`.
  fn no_name(&self, also: &[&str]) -> Result<(), Unreadable> {
    match self.string("name")? {
      None | Some("") => Ok(()),
      Some(name) if also.contains(&name) => Ok(()),
      Some(name) => Err(feature(
        &self.at.key("name"),
        Feature::Name(Utf8Bytes::from(name)),
      )),
    }
  }

  /// Whether the item plays: `enabled`, absent reading as `true`.
  fn enabled(&self) -> Result<bool, Unreadable> {
    Ok(self.boolean("enabled")?.unwrap_or(true))
  }

  /// An item the timeline cannot disable — the stack, a gap, a transition:
  /// `enabled` absent or `true`.
  fn never_disabled(&self) -> Result<(), Unreadable> {
    if self.enabled()? {
      Ok(())
    } else {
      Err(feature(&self.at.key("enabled"), Feature::Disabled))
    }
  }

  /// An item's `color`, a newer reader's word the timeline has none for:
  /// absent or `null`.
  fn no_color(&self) -> Result<(), Unreadable> {
    match self.get("color") {
      None | Some(Value::Null) => Ok(()),
      Some(_) => Err(feature(&self.at.key("color"), Feature::ItemColor)),
    }
  }

  /// A trim on the stack or a track, which the timeline has no word for:
  /// `source_range` absent or `null`.
  fn no_trim(&self) -> Result<(), Unreadable> {
    match self.get("source_range") {
      None | Some(Value::Null) => Ok(()),
      Some(_) => Err(feature(&self.at.key("source_range"), Feature::Trim)),
    }
  }

  /// An item's effects: none. An effect of any schema is one the timeline
  /// does not model, refused with its name.
  fn no_effects(&self) -> Result<(), Unreadable> {
    self.no_schemas("effects")
  }

  /// Markers where the timeline keeps none — on the stack or a gap: none,
  /// each refused as a schema the timeline does not model there.
  fn no_markers(&self) -> Result<(), Unreadable> {
    self.no_schemas("markers")
  }

  fn no_schemas(&self, key: &str) -> Result<(), Unreadable> {
    let Some(value) = self.get(key) else {
      return Ok(());
    };
    let at = self.at.key(key);
    match array(value, &at)?.first() {
      None => Ok(()),
      Some(first) => {
        let at = at.index(0);
        let entry = Object::of(first, at.clone())?;
        let name = entry
          .get("OTIO_SCHEMA")
          .and_then(Value::as_str)
          .ok_or_else(|| shape(&at.key("OTIO_SCHEMA"), "a schema name"))?;
        Err(unmodelled(&at, name))
      }
    }
  }
}

/// This crate's words in an object's `metadata`: the `mediaio` object. Any
/// other key of the metadata is another writer's note, which the timeline
/// does not keep, and is refused by name; so is a word this reader does
/// not know.
struct Words<'a> {
  words: Option<Object<'a>>,
  at: At,
}

impl<'a> Words<'a> {
  /// The words of `owner`, refusing any but `known`.
  fn of(owner: &Object<'a>, known: &[&str]) -> Result<Self, Unreadable> {
    let at = owner.at.key("metadata");
    let ours = at.key(OURS);
    let Some(metadata) = owner.get("metadata") else {
      return Ok(Self {
        words: None,
        at: ours,
      });
    };
    let metadata = Object::of(metadata, at.clone())?;
    if let Some((key, _)) = metadata.members.iter().find(|(key, _)| key != OURS) {
      return Err(feature(&at.key(key), Feature::Metadata(key.clone())));
    }
    let Some(words) = metadata.get(OURS) else {
      return Ok(Self {
        words: None,
        at: ours,
      });
    };
    let words = Object::of(words, ours.clone())?;
    if let Some((key, _)) = words
      .members
      .iter()
      .find(|(key, _)| !known.contains(&key.as_str()))
    {
      return Err(feature(&ours.key(key), Feature::Word(key.clone())));
    }
    Ok(Self {
      words: Some(words),
      at: ours,
    })
  }

  fn get(&self, word: &str) -> Option<&'a Value> {
    self.words.as_ref().and_then(|words| words.get(word))
  }

  fn text(&self, word: &str) -> Result<Option<&'a str>, Unreadable> {
    match self.get(word) {
      None => Ok(None),
      Some(value) => value
        .as_str()
        .map(Some)
        .ok_or_else(|| shape(&self.at.key(word), "a string")),
    }
  }

  fn required_text(&self, word: &str) -> Result<&'a str, Unreadable> {
    self
      .text(word)?
      .ok_or_else(|| shape(&self.at.key(word), "a required word"))
  }

  /// A rate written `num/den`, above zero.
  fn rate(&self, word: &str) -> Result<Option<Rate>, Unreadable> {
    self
      .text(word)?
      .map(|text| rate(text, &self.at.key(word)))
      .transpose()
  }

  /// A gain in decibels: the `f32` its number spells exactly
  /// ([`json::exact_f32`]), finite so.
  fn gain(&self, word: &str) -> Result<Option<Gain>, Unreadable> {
    let Some(value) = self.get(word) else {
      return Ok(None);
    };
    let at = self.at.key(word);
    json::exact_f32(number(value, &at)?)
      .and_then(Gain::from_db)
      .map(Some)
      .ok_or_else(|| inexact(&at, "a gain that is not exactly the f32 it reads as"))
  }

  /// The exact words of a media-side range: the ruler its counts are
  /// written in and the timebase the model counts it in.
  fn exact(&self, word: &str) -> Result<Option<Exact>, Unreadable> {
    let Some(value) = self.get(word) else {
      return Ok(None);
    };
    let at = self.at.key(word);
    let object = Object::of(value, at.clone())?;
    if let Some((key, _)) = object
      .members
      .iter()
      .find(|(key, _)| key != "ruler" && key != "timebase")
    {
      return Err(feature(&at.key(key), Feature::Word(key.clone())));
    }
    let ruler = object
      .string("ruler")?
      .ok_or_else(|| shape(&at.key("ruler"), "a required word"))?;
    let timebase = object
      .string("timebase")?
      .ok_or_else(|| shape(&at.key("timebase"), "a required word"))?;
    let timebase_at = at.key("timebase");
    Ok(Some(Exact {
      ruler: rate(ruler, &at.key("ruler"))?,
      timebase: Timebase::from_str(timebase)
        .ok()
        .filter(|timebase| timebase.num() > 0)
        .ok_or_else(|| shape(&timebase_at, "a timebase above zero, written num/den"))?,
    }))
  }

  fn required_exact(&self, word: &str) -> Result<Exact, Unreadable> {
    self
      .exact(word)?
      .ok_or_else(|| shape(&self.at.key(word), "a required word"))
  }

  /// The model's notes, under `metadata`: an array of `[key, value]`
  /// pairs of strings, each key once — never an object, whose keys a
  /// reader would take for schema words (see the export).
  fn notes(&self) -> Result<Metadata, Unreadable> {
    let Some(value) = self.get("metadata") else {
      return Ok(Metadata::new());
    };
    let at = self.at.key("metadata");
    let mut notes = Metadata::new();
    for (index, pair) in array(value, &at)?.iter().enumerate() {
      let at = at.index(index);
      let Some([Value::String(key), Value::String(note)]) = pair.as_array() else {
        return Err(shape(&at, "a note: a key and a value, both strings"));
      };
      if notes.insert(key.clone(), note.clone()).is_some() {
        return Err(shape(&at.index(0), "a key no earlier note names"));
      }
    }
    Ok(notes)
  }

  /// The timebase an edit-side value is counted in: the one its word
  /// spells — the edit rate's reciprocal by value, spelled otherwise, as
  /// the export writes it — or, with no word, the reciprocal `ticks`
  /// itself. Either counts the same ticks.
  fn timebase(&self, word: &str, ticks: Timebase) -> Result<Timebase, Unreadable> {
    let Some(text) = self.text(word)? else {
      return Ok(ticks);
    };
    Timebase::from_str(text)
      .ok()
      .filter(|timebase| *timebase == ticks)
      .ok_or_else(|| {
        shape(
          &self.at.key(word),
          "the edit rate's timebase, written num/den",
        )
      })
  }
}

/// A rate written `num/den`, above zero.
fn rate(text: &str, at: &At) -> Result<Rate, Unreadable> {
  Rate::from_str(text)
    .ok()
    .filter(|rate| rate.num() > 0)
    .ok_or_else(|| shape(at, "a rate above zero, written num/den"))
}

/// The exact ruler a media-side range is written in, and the timebase the
/// model counts it in.
#[derive(Clone, Copy)]
struct Exact {
  ruler: Rate,
  timebase: Timebase,
}

/// A `RationalTime.1` counted in `ruler`: its rate the `f64` its number
/// spells exactly ([`json::exact_f64`]) and that `f64` the exact ruler's,
/// bit for bit; its value a whole count within ±2^53.
fn count(value: &Value, at: &At, ruler: Ruler) -> Result<i64, Unreadable> {
  let object = Object::of(value, at.clone())?;
  object.schema(&["RationalTime.1"])?;
  object.keys(&["OTIO_SCHEMA", "rate", "value"])?;
  let rate_at = at.key("rate");
  let rate = json::exact_f64(number(object.required("rate")?, &rate_at)?)
    .ok_or_else(|| inexact(&rate_at, "a rate that is not exactly the f64 it reads as"))?;
  if rate.to_bits() != ruler.otio().to_bits() {
    return Err(inexact(
      &rate_at,
      "a rate other than the f64 of the exact ruler it is counted in",
    ));
  }
  let value_at = at.key("value");
  let whole = json::whole(number(object.required("value")?, &value_at)?)
    .ok_or_else(|| inexact(&value_at, "a count that is no whole number"))?;
  if whole.unsigned_abs() > 1 << 53 {
    return Err(inexact(
      &value_at,
      "a count past 2^53, which an f64 does not hold exactly",
    ));
  }
  // Within ±2^53, so within `i64`.
  Ok(whole as i64)
}

/// A count that measures a length: zero or more.
fn length(value: &Value, at: &At, ruler: Ruler) -> Result<u64, Unreadable> {
  u64::try_from(count(value, at, ruler)?)
    .map_err(|_| shape(&at.key("value"), "a length of zero or more"))
}

/// A `TimeRange.1` counted in `ruler`: its start and its length, and both
/// ends OpenTimelineIO derives from them held ([`derive::range_held`]) —
/// its end, the two added in `f64`, and its last tick — as the export holds
/// every range it writes.
fn counts(value: &Value, at: &At, ruler: Ruler) -> Result<(i64, u64), Unreadable> {
  let object = Object::of(value, at.clone())?;
  object.schema(&["TimeRange.1"])?;
  object.keys(&["OTIO_SCHEMA", "duration", "start_time"])?;
  let start = count(object.required("start_time")?, &at.key("start_time"), ruler)?;
  let length = length(object.required("duration")?, &at.key("duration"), ruler)?;
  if !derive::range_held(written(start, ruler), written(length, ruler)) {
    return Err(inexact(
      at,
      "a range whose end, or last tick, OpenTimelineIO's f64 arithmetic rounds",
    ));
  }
  Ok((start, length))
}

/// `count` ticks of `ruler`, as the document writes them, for the
/// arithmetic OpenTimelineIO does on them ([`Time`]).
fn written(count: impl Into<i128>, ruler: Ruler) -> Time {
  Time::written(count.into(), ruler)
}

/// What OpenTimelineIO's `f64` arithmetic rounds in the walk of a track
/// or of the stack at `stack` ([`derive::track`], [`derive::stack`]),
/// refused where it is derived: at a child of a track, at a track, or at
/// the stack.
fn rounded(refusal: &NotRepresentable, stack: &At) -> Unreadable {
  let track = |index: usize| stack.key("children").index(index);
  let child = |at: ChildAt| track(at.track()).key("children").index(at.child());
  match refusal.at() {
    Spot::TrackPosition(at) => inexact(
      &child(at),
      "a place on its track that OpenTimelineIO's f64 arithmetic rounds",
    ),
    Spot::Visible(at) => inexact(
      &child(at),
      "a visible range that OpenTimelineIO's f64 arithmetic rounds",
    ),
    Spot::Absolute(at) => inexact(
      &child(at),
      "a place from the global start that OpenTimelineIO's f64 arithmetic rounds",
    ),
    Spot::TrackDuration(index) => inexact(
      &track(index),
      "a track duration that OpenTimelineIO's f64 arithmetic rounds",
    ),
    Spot::AbsoluteEnd(index) => inexact(
      &track(index),
      "a track's end from the global start that OpenTimelineIO's f64 arithmetic rounds",
    ),
    Spot::StackDuration => inexact(
      stack,
      "tracks whose longest OpenTimelineIO's f64 comparison does not pick",
    ),
    Spot::TimelineEnd => inexact(
      stack,
      "a timeline end from the global start that OpenTimelineIO's f64 arithmetic rounds",
    ),
    // Counts the document writes, which a walk does not refuse.
    Spot::Start
    | Spot::Record(_)
    | Spot::Source(_)
    | Spot::Available(_)
    | Spot::Transition(_)
    | Spot::Fade(_)
    | Spot::Marker(_) => inexact(stack, "a time OpenTimelineIO's f64 arithmetic rounds"),
  }
}

/// The range `start` and `length` ticks of `exact.ruler` make, counted
/// exactly in `exact.timebase`.
fn exact_range(start: i64, length: u64, exact: Exact, at: &At) -> Result<TimeRange, Unreadable> {
  // A rate above zero has a timebase.
  let written = exact.ruler.checked_to_timebase().unwrap_or_default();
  let start = Timestamp::new(start, written)
    .checked_rescale_with(exact.timebase, Rounding::Exact)
    .ok_or_else(|| inexact(at, "a start its timebase does not count exactly"))?;
  let length = Duration::new(length, written)
    .checked_rescale_with(exact.timebase, Rounding::Exact)
    .ok_or_else(|| inexact(at, "a length its timebase does not count exactly"))?;
  let end = i64::try_from(length.ticks())
    .ok()
    .and_then(|ticks| start.pts().checked_add(ticks))
    .ok_or_else(|| inexact(at, "an end past what its timebase counts in an i64"))?;
  Ok(TimeRange::new(start.pts(), end, exact.timebase))
}

/// The range `start` and `length` edit-rate ticks make, from the
/// timeline's zero.
fn edit_range(start: i64, length: u64, edit: Timebase) -> TimeRange {
  // Both lie within ±2^53, so their sum within `i64`.
  TimeRange::new(start, start + length as i64, edit)
}

/// Reads the timeline `root` holds — not yet validated.
pub(crate) fn timeline(root: &Value) -> Result<Timeline, Unreadable> {
  let object = Object::of(root, At::root())?;
  object.schema(&["Timeline.1"])?;
  object.keys(&[
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "global_start_time",
    "tracks",
  ])?;
  let words = Words::of(&object, &["rate", "start", "metadata"])?;
  let rate = words
    .rate("rate")?
    .ok_or_else(|| shape(&words.at.key("rate"), "a required word"))?;
  let edit = Ruler::new(rate);
  let ticks = rate.checked_to_timebase().unwrap_or_default();
  let start_timebase = words.timebase("start", ticks)?;
  let start = count(
    object.required("global_start_time")?,
    &object.at.key("global_start_time"),
    edit,
  )?;
  let stack = Object::of(object.required("tracks")?, object.at.key("tracks"))?;
  let tracks = self::stack(&stack, edit, ticks, written(start, edit))?;
  Ok(
    Timeline::new(object.name()?, rate)
      .with_start(Timestamp::new(start, start_timebase))
      .with_metadata(words.notes()?)
      .with_tracks(tracks),
  )
}

/// The one `Stack.1`, named `tracks` as OpenTimelineIO names it, holding
/// the tracks and nothing the timeline does not keep. Each track is walked
/// as the export walks it, from the global start `global`, and then the
/// stack ([`derive::track`], [`derive::stack`]): what OpenTimelineIO's
/// `f64` arithmetic rounds there is refused ([`rounded`]).
fn stack(
  stack: &Object<'_>,
  edit: Ruler,
  ticks: Timebase,
  global: Time,
) -> Result<Vec<Track>, Unreadable> {
  stack.schema(&["Stack.1"])?;
  stack.keys(&[
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "source_range",
    "effects",
    "markers",
    "enabled",
    "color",
    "children",
  ])?;
  Words::of(stack, &[])?;
  stack.no_name(&["tracks"])?;
  stack.no_trim()?;
  stack.no_effects()?;
  stack.no_markers()?;
  stack.never_disabled()?;
  stack.no_color()?;
  let at = stack.at.key("children");
  let children = array(stack.required("children")?, &at)?;
  let mut tracks = Vec::with_capacity(children.len());
  let mut durations = Vec::with_capacity(children.len());
  for (index, child) in children.iter().enumerate() {
    let (track, walk) = track(&Object::of(child, at.index(index))?, edit, ticks)?;
    let duration =
      derive::track(index, &walk, global).map_err(|refusal| rounded(&refusal, &stack.at))?;
    tracks.push(track);
    durations.push(duration);
  }
  derive::stack(&durations, global).map_err(|refusal| rounded(&refusal, &stack.at))?;
  Ok(tracks)
}

/// A `Track.1`, and its children as OpenTimelineIO's walk reads them.
fn track(
  object: &Object<'_>,
  edit: Ruler,
  ticks: Timebase,
) -> Result<(Track, Vec<derive::Child>), Unreadable> {
  object.schema(&["Track.1"])?;
  object.keys(&[
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "source_range",
    "effects",
    "markers",
    "enabled",
    "color",
    "children",
    "kind",
  ])?;
  Words::of(object, &[])?;
  object.no_trim()?;
  object.no_effects()?;
  object.no_color()?;
  let kind = match object.string("kind")? {
    Some("Video") => TrackKind::Video,
    Some("Audio") => TrackKind::Audio,
    Some(other) => {
      return Err(feature(
        &object.at.key("kind"),
        Feature::TrackKind(Utf8Bytes::from(other)),
      ));
    }
    None => return Err(shape(&object.at.key("kind"), "a required key")),
  };
  let mut track = Track::new(kind, object.name()?).with_enabled(object.enabled()?);
  let Children {
    clips,
    transitions,
    walk,
  } = children(object, edit, ticks)?;
  *track.clips_mut() = clips;
  *track.transitions_mut() = transitions;
  *track.markers_mut() = markers(object, Placed::Edit(edit, ticks))?;
  Ok((track, walk))
}

/// A track's children as read: its clips, each at its record, its
/// transitions, and the children as OpenTimelineIO's walk reads them.
struct Children {
  clips: Vec<Clip>,
  transitions: Vec<Transition>,
  walk: Vec<derive::Child>,
}

/// One child of a track as read, before the track places it.
enum Child {
  /// A gap, so many edit-rate ticks long.
  Gap(u64),
  /// A clip, and its length at the edit rate.
  Clip(Box<Clip>, u64),
  /// A fade at one edge of the clip beside it.
  Fade(Edge, Fade),
  /// A dissolve: its offsets, the timebase of its cut, and its index
  /// among its track's transitions where the document says it.
  Dissolve {
    in_offset: Duration,
    out_offset: Duration,
    at: Timebase,
    order: Option<usize>,
  },
}

/// What kind of child sits at a place of a track: what a transition's
/// neighbours are judged by.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
  Gap,
  Clip,
  Transition,
}

impl Child {
  const fn kind(&self) -> Kind {
    match self {
      Self::Gap(_) => Kind::Gap,
      Self::Clip(..) => Kind::Clip,
      Self::Fade(..) | Self::Dissolve { .. } => Kind::Transition,
    }
  }
}

/// A track's clips, each at its record, and its transitions: its items
/// laid end to end from zero, as OpenTimelineIO places them, each fade on
/// the clip beside it and each dissolve on the cut it straddles. The track
/// ends with its last clip: a gap of some length after it is refused by
/// name, every gap of a track with no clip among them. A gap of no length
/// is read only where the export writes one — on a fade's gap side, beside
/// no other gap — and in the form it writes, its range stated at the edit
/// rate ([`gap`]); it is refused by name anywhere else, as OpenTimelineIO
/// counts it all the same. Answers the children as OpenTimelineIO's walk
/// reads them too, each counted as the document writes it.
fn children(object: &Object<'_>, edit: Ruler, ticks: Timebase) -> Result<Children, Unreadable> {
  let at = object.at.key("children");
  let values = array(object.required("children")?, &at)?;
  let mut read = Vec::with_capacity(values.len());
  let mut walk = Vec::with_capacity(values.len());
  for (index, value) in values.iter().enumerate() {
    let child = Object::of(value, at.index(index))?;
    let (read_as, walked) = match child.schema(&["Gap.1", "Clip.1", "Clip.2", "Transition.1"])? {
      "Gap.1" => {
        let length = gap(&child, edit)?;
        let walked = derive::Child::Item {
          start: written(0, edit),
          duration: written(length, edit),
        };
        (Child::Gap(length), walked)
      }
      "Transition.1" => transition(&child, edit, ticks)?,
      schema => {
        let (clip, length, walked) = clip(&child, schema, ticks)?;
        (Child::Clip(Box::new(clip), length), walked)
      }
    };
    read.push(read_as);
    walk.push(walked);
  }

  let kinds: Vec<Kind> = read.iter().map(Child::kind).collect();
  let fades: Vec<Option<Edge>> = read
    .iter()
    .map(|child| match child {
      Child::Fade(edge, _) => Some(*edge),
      _ => None,
    })
    .collect();
  let last_clip = kinds.iter().rposition(|&kind| kind == Kind::Clip);
  let mut cursor: i64 = 0;
  let mut clips: Vec<Clip> = Vec::new();
  let mut dissolves = Vec::new();
  let mut fade_in = None;
  for (index, child) in read.into_iter().enumerate() {
    let here = at.index(index);
    let before = index
      .checked_sub(1)
      .and_then(|before| kinds.get(before).copied());
    let after = kinds.get(index + 1).copied();
    if kinds[index] == Kind::Transition
      && [before, after]
        .iter()
        .any(|neighbour| neighbour.is_none_or(|kind| kind == Kind::Transition))
    {
      return Err(shape(&here, "a transition between two items"));
    }
    match child {
      Child::Gap(length) => {
        if length > 0 && last_clip.is_none_or(|last| index > last) {
          return Err(feature(&here, Feature::TrailingGap));
        }
        // A gap of no length is read only where the export writes one: on
        // a fade's gap side — before a fade-in, after a fade-out — and
        // beside no other gap. Anywhere else OpenTimelineIO still counts
        // it, while the timeline has nothing there.
        if length == 0 {
          let fade = |at: Option<usize>| at.and_then(|at| fades.get(at).copied().flatten());
          let fade_side = fade(Some(index + 1)) == Some(Edge::In)
            || fade(index.checked_sub(1)) == Some(Edge::Out);
          if !fade_side || before == Some(Kind::Gap) || after == Some(Kind::Gap) {
            return Err(feature(&here, Feature::StandaloneGap));
          }
        }
        cursor = advance(cursor, length, &here)?;
      }
      Child::Clip(mut clip, length) => {
        let end = advance(cursor, length, &here)?;
        // The record's timebase is the one `clip` set: its word's, or the
        // edit rate's reciprocal.
        let timebase = clip.record().timebase();
        clip.set_record(TimeRange::new(cursor, end, timebase));
        if let Some(fade) = fade_in.take() {
          let fades = clip.fades().with_in(Some(fade));
          clip.set_fades(fades);
        }
        clips.push(*clip);
        cursor = end;
      }
      Child::Fade(edge, fade) => {
        // A fade runs between a gap and the clip it fades: a fade-in from
        // the gap before its clip, a fade-out to the gap after it.
        let (gap_side, clip_side) = match edge {
          Edge::In => (before, after),
          Edge::Out => (after, before),
        };
        if gap_side != Some(Kind::Gap) || clip_side != Some(Kind::Clip) {
          return Err(feature(&here, Feature::Fade));
        }
        match edge {
          Edge::In => fade_in = Some(fade),
          Edge::Out => {
            if let Some(clip) = clips.last_mut() {
              let fades = clip.fades().with_out(Some(fade));
              clip.set_fades(fades);
            }
          }
        }
      }
      Child::Dissolve {
        in_offset,
        out_offset,
        at,
        order,
      } => {
        if before != Some(Kind::Clip) || after != Some(Kind::Clip) {
          return Err(feature(&here, Feature::DissolveBesideGap));
        }
        let transition = Transition::dissolve(Timestamp::new(cursor, at), in_offset, out_offset);
        dissolves.push((transition, order, here));
      }
    }
  }
  Ok(Children {
    clips,
    transitions: ordered(dissolves)?,
    walk,
  })
}

/// `cursor` moved on `length` edit-rate ticks.
fn advance(cursor: i64, length: u64, at: &At) -> Result<i64, Unreadable> {
  i64::try_from(length)
    .ok()
    .and_then(|length| cursor.checked_add(length))
    .ok_or_else(|| inexact(at, "a place on its track past i64 edit-rate ticks"))
}

/// A track's dissolves in the order the track keeps them: their cuts'
/// order, where none says its place; else each at the place it says, every
/// place once.
fn ordered(dissolves: Vec<(Transition, Option<usize>, At)>) -> Result<Vec<Transition>, Unreadable> {
  if dissolves.iter().all(|(_, order, _)| order.is_none()) {
    return Ok(
      dissolves
        .into_iter()
        .map(|(dissolve, ..)| dissolve)
        .collect(),
    );
  }
  let mut places = vec![None; dissolves.len()];
  for (dissolve, order, at) in dissolves {
    let place = order
      .filter(|&place| places.get(place).is_some_and(Option::is_none))
      .ok_or_else(|| {
        shape(
          &at.key("metadata").key(OURS).key("order"),
          "a place among the track's transitions, said by every dissolve, each place once",
        )
      })?;
    places[place] = Some(dissolve);
  }
  Ok(places.into_iter().flatten().collect())
}

/// A `Gap.1`'s length at the edit rate: a gap carries nothing else.
///
/// Its `source_range` is required, as a clip's is, and counted at the edit
/// rate exactly — the form the export writes — as every range is
/// ([`counts`]). Without one OpenTimelineIO reads a gap as its
/// constructor's `TimeRange()` (`gap.h` 23 in 0.17.0; `item.cpp` 175 reads
/// the range only if present): nothing, at rate 1. The timeline keeps a
/// gap's length at the edit rate alone, so that rate would be lost, and it
/// joins the track's sum, which carries the higher of two rates
/// (`track.cpp` 111–141; `rationalTime.h` 283–294): a re-export, writing
/// the gap at the edit rate, could change the track's duration strictly.
fn gap(object: &Object<'_>, edit: Ruler) -> Result<u64, Unreadable> {
  object.keys(&[
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "source_range",
    "effects",
    "markers",
    "enabled",
    "color",
  ])?;
  Words::of(object, &[])?;
  object.no_name(&[])?;
  object.no_effects()?;
  object.no_markers()?;
  object.never_disabled()?;
  object.no_color()?;
  let at = object.at.key("source_range");
  let (start, length) = counts(object.required("source_range")?, &at, edit)?;
  if start != 0 {
    return Err(feature(&at.key("start_time"), Feature::GapStart));
  }
  Ok(length)
}

/// A `Transition.1`: a fade, by this crate's words, or a dissolve — and
/// the transition as OpenTimelineIO's walk reads it.
fn transition(
  object: &Object<'_>,
  edit: Ruler,
  ticks: Timebase,
) -> Result<(Child, derive::Child), Unreadable> {
  object.keys(&[
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "in_offset",
    "out_offset",
    "transition_type",
    "enabled",
  ])?;
  object.no_name(&[])?;
  object.never_disabled()?;
  match object.string("transition_type")? {
    Some("SMPTE_Dissolve") => {}
    Some(other) => {
      return Err(feature(
        &object.at.key("transition_type"),
        Feature::TransitionType(Utf8Bytes::from(other)),
      ));
    }
    None => return Err(shape(&object.at.key("transition_type"), "a required key")),
  }
  let in_offset = length(
    object.required("in_offset")?,
    &object.at.key("in_offset"),
    edit,
  )?;
  let out_offset = length(
    object.required("out_offset")?,
    &object.at.key("out_offset"),
    edit,
  )?;
  let walked = derive::Child::Transition {
    in_offset: written(in_offset, edit),
    out_offset: written(out_offset, edit),
  };
  let words = Words::of(
    object,
    &[
      "fade",
      "shape",
      "timebase",
      "order",
      "at",
      "in_offset",
      "out_offset",
    ],
  )?;
  // A word of the other kind of transition is one this reader does not
  // know there.
  let foreign = |names: &[&'static str]| -> Result<(), Unreadable> {
    match names.iter().find(|name| words.get(name).is_some()) {
      Some(name) => Err(feature(
        &words.at.key(name),
        Feature::Word(Utf8Bytes::from_static(name)),
      )),
      None => Ok(()),
    }
  };
  let Some(fade) = words.text("fade")? else {
    foreign(&["shape", "timebase"])?;
    let order = words
      .get("order")
      .map(|value| {
        let at = words.at.key("order");
        json::whole(number(value, &at)?)
          .and_then(|order| usize::try_from(order).ok())
          .ok_or_else(|| shape(&at, "a place among the track's transitions"))
      })
      .transpose()?;
    let dissolve = Child::Dissolve {
      in_offset: Duration::new(in_offset, words.timebase("in_offset", ticks)?),
      out_offset: Duration::new(out_offset, words.timebase("out_offset", ticks)?),
      at: words.timebase("at", ticks)?,
      order,
    };
    return Ok((dissolve, walked));
  };
  foreign(&["order", "at", "in_offset", "out_offset"])?;
  let edge = match fade {
    "in" => Edge::In,
    "out" => Edge::Out,
    _ => return Err(shape(&words.at.key("fade"), "in or out")),
  };
  let shape_word = words.required_text("shape")?;
  let curve = match shape_word {
    "linear" => FadeShape::Linear,
    "equal_power" => FadeShape::EqualPower,
    _ => return Err(shape(&words.at.key("shape"), "linear or equal_power")),
  };
  // A fade runs inside its clip: it plays nothing of the gap's side.
  let (inside, gap_side) = match edge {
    Edge::In => (out_offset, in_offset),
    Edge::Out => (in_offset, out_offset),
  };
  if gap_side != 0 {
    return Err(feature(&object.at, Feature::Fade));
  }
  let timebase = words.timebase("timebase", ticks)?;
  Ok((
    Child::Fade(edge, Fade::new(Duration::new(inside, timebase), curve)),
    walked,
  ))
}

/// A `Clip.1` or a `Clip.2`, its length at the edit rate — its record is
/// the track's to place — and the clip as OpenTimelineIO's walk reads it:
/// its source range, counted in the ruler the document writes it in.
fn clip(
  object: &Object<'_>,
  schema: &str,
  ticks: Timebase,
) -> Result<(Clip, u64, derive::Child), Unreadable> {
  let references: &[&str] = match schema {
    "Clip.2" => &["media_references", "active_media_reference_key"],
    _ => &["media_reference"],
  };
  let mut known = vec![
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "source_range",
    "effects",
    "markers",
    "enabled",
    "color",
  ];
  known.extend_from_slice(references);
  object.keys(&known)?;
  let words = Words::of(object, &["id", "gain_db", "source", "record", "metadata"])?;
  let id = words.required_text("id")?;
  let record = words.timebase("record", ticks)?;
  let exact = words.required_exact("source")?;
  object.no_effects()?;
  object.no_color()?;
  let at = object.at.key("source_range");
  let ruler = Ruler::new(exact.ruler);
  let (start, length) = counts(object.required("source_range")?, &at, ruler)?;
  let walked = derive::Child::Item {
    start: written(start, ruler),
    duration: written(length, ruler),
  };
  let source = exact_range(start, length, exact, &at)?;
  // With no time-warp, a clip runs exactly as long as its source.
  let length = source
    .span()
    .checked_rescale_with(ticks, Rounding::Exact)
    .ok_or_else(|| {
      inexact(
        &at,
        "a source range that is no whole number of edit-rate ticks",
      )
    })?
    .ticks();
  let mut clip = Clip::new(
    ClipId::new(id),
    object.name()?,
    media(object, schema)?,
    source,
    // The track places the record; its timebase is the clip's own.
    TimeRange::new(0, 0, record),
  )
  .with_enabled(object.enabled()?)
  .with_gain(words.gain("gain_db")?)
  .with_metadata(words.notes()?);
  *clip.markers_mut() = markers(object, Placed::Exact)?;
  Ok((clip, length, walked))
}

/// The one medium a clip reads: `Clip.1`'s `media_reference`, or the one
/// reference of a `Clip.2`, under `DEFAULT_MEDIA` and active.
fn media(object: &Object<'_>, schema: &str) -> Result<MediaRef, Unreadable> {
  if schema != "Clip.2" {
    // OpenTimelineIO upgrades a `Clip.1` to a `Clip.2`, and one whose
    // `media_reference` is absent or `null` to a `MissingReference`
    // (`typeRegistry.cpp` 99–117 in 0.17.0): a clip with no medium.
    let at = object.at.key("media_reference");
    return match object.get("media_reference") {
      None | Some(Value::Null) => Err(feature(&at, Feature::NoMedia)),
      Some(value) => reference(value, at),
    };
  }
  let at = object.at.key("media_references");
  let references = Object::of(object.required("media_references")?, at.clone())?;
  let active = object
    .string("active_media_reference_key")?
    .ok_or_else(|| {
      shape(
        &object.at.key("active_media_reference_key"),
        "a required key",
      )
    })?;
  match references.members {
    [] => Err(feature(&at, Feature::NoMedia)),
    [(key, value)] if key == DEFAULT_MEDIA && active == DEFAULT_MEDIA => {
      reference(value, at.key(DEFAULT_MEDIA))
    }
    _ => Err(feature(&at, Feature::MediaReferences)),
  }
}

/// An `ExternalReference.1`: the medium's locator, and what is known of it.
fn reference(value: &Value, at: At) -> Result<MediaRef, Unreadable> {
  let object = Object::of(value, at)?;
  object.schema(&["ExternalReference.1"])?;
  object.keys(&[
    "OTIO_SCHEMA",
    "metadata",
    "name",
    "available_range",
    "available_image_bounds",
    "target_url",
  ])?;
  let words = Words::of(&object, &["rate", "reel", "available"])?;
  object.no_name(&[])?;
  match object.get("available_image_bounds") {
    None | Some(Value::Null) => {}
    Some(_) => {
      return Err(feature(
        &object.at.key("available_image_bounds"),
        Feature::ImageBounds,
      ));
    }
  }
  let locator = object
    .string("target_url")?
    .ok_or_else(|| shape(&object.at.key("target_url"), "a required key"))?;
  let at = object.at.key("available_range");
  let available = match object.get("available_range") {
    None | Some(Value::Null) => {
      if words.get("available").is_some() {
        return Err(shape(
          &words.at.key("available"),
          "no available word, as the reference has no available range",
        ));
      }
      None
    }
    Some(range) => {
      let exact = words.required_exact("available")?;
      let (start, length) = counts(range, &at, Ruler::new(exact.ruler))?;
      Some(exact_range(start, length, exact, &at)?)
    }
  };
  Ok(
    MediaRef::new(locator)
      .with_available_range(available)
      .with_rate(words.rate("rate")?)
      .with_reel(words.text("reel")?.map(Utf8Bytes::from)),
  )
}

/// Where a marker's range is counted.
#[derive(Clone, Copy)]
enum Placed {
  /// A track's: at the edit rate, whose exact rate and timebase are these.
  Edit(Ruler, Timebase),
  /// A clip's: in the exact ruler its words name, then in its timebase.
  Exact,
}

/// The `Marker.2`s of an item: a track's, or a clip's.
fn markers(object: &Object<'_>, placed: Placed) -> Result<Vec<Marker>, Unreadable> {
  let Some(value) = object.get("markers") else {
    return Ok(Vec::new());
  };
  let at = object.at.key("markers");
  array(value, &at)?
    .iter()
    .enumerate()
    .map(|(index, entry)| {
      let marker = Object::of(entry, at.index(index))?;
      marker.schema(&["Marker.2"])?;
      marker.keys(&[
        "OTIO_SCHEMA",
        "metadata",
        "name",
        "color",
        "marked_range",
        "comment",
      ])?;
      let words = Words::of(
        &marker,
        match placed {
          Placed::Edit(..) => &["timebase"],
          Placed::Exact => &["range"],
        },
      )?;
      let color = match marker.string("color")? {
        None => MarkerColor::Green,
        Some(name) => MarkerColor::from_otio_name(name).ok_or_else(|| {
          feature(
            &marker.at.key("color"),
            Feature::MarkerColor(Utf8Bytes::from(name)),
          )
        })?,
      };
      let range_at = marker.at.key("marked_range");
      let range = marker.required("marked_range")?;
      let range = match placed {
        Placed::Edit(edit, ticks) => {
          let (start, length) = counts(range, &range_at, edit)?;
          edit_range(start, length, words.timebase("timebase", ticks)?)
        }
        Placed::Exact => {
          let exact = words.required_exact("range")?;
          let (start, length) = counts(range, &range_at, Ruler::new(exact.ruler))?;
          exact_range(start, length, exact, &range_at)?
        }
      };
      Ok(
        Marker::new(marker.name()?, range)
          .with_color(color)
          .with_comment(marker.string("comment")?.unwrap_or("")),
      )
    })
    .collect()
}

/// The reader's backstop: every time the document states is one the
/// export writes, where and as the export writes it for the timeline read
/// from it.
///
/// The export writes `timeline` again, and each `RationalTime` it writes —
/// the global start, each gap's and clip's range, each transition's
/// offsets, each medium's available range, each marker's range — is
/// compared with the one `root` states at the same path, strictly, as
/// OpenTimelineIO's `strictly_equal` compares (`rationalTime.h` 101–104 in
/// 0.17.0): its value and its rate, each the same `f64`. A clip's medium is
/// compared where the document's own clip schema keeps it. The first time
/// that differs, that the document does not state, or that it states and
/// the export does not write, is refused as inexact; a timeline the export
/// cannot write, at the document's root.
///
/// The checks before it refuse by name what they know. This closes the
/// class none of them names — a time read, and counted again, in a form
/// the export does not write it in, such as a medium's range in a ruler of
/// the document's choosing — so that the reader accepts exactly the
/// documents whose times the export reproduces.
pub(crate) fn reproduced(root: &Value, timeline: &Timeline) -> Result<(), Unreadable> {
  let written = export::timeline(timeline, &layout_valid(timeline), OtioTarget::V0_15Plus)
    .map_err(|_| {
      inexact(
        &At::root(),
        "a timeline whose times the export cannot write back",
      )
    })?;
  times_match(&written, Some(root), &At::root())
}

/// The `RationalTime` `written` and the one `stated`, held strictly equal;
/// or, within any other value, each time `written` holds against the one
/// `stated` holds at the same place, and no time `stated` holds that
/// `written` does not ([`reproduced`]).
fn times_match(written: &Value, stated: Option<&Value>, at: &At) -> Result<(), Unreadable> {
  let differs = || {
    inexact(
      at,
      "a time the export does not write back as stated: another value or rate",
    )
  };
  match written {
    Value::Object(members) => {
      let schema = schema_of(written);
      if schema == Some("RationalTime.1") {
        return match stated {
          Some(stated) if same_time(written, stated) => Ok(()),
          _ => Err(differs()),
        };
      }
      let stated_members = stated.and_then(Value::as_object).unwrap_or_default();
      // A `Clip.1` keeps its medium under `media_reference`, where the
      // export's `Clip.2` keeps it under `media_references`, by its key.
      let legacy_clip = schema == Some("Clip.2") && stated.and_then(schema_of) == Some("Clip.1");
      for (key, value) in members {
        if legacy_clip && key == "media_references" {
          let medium = member(value, DEFAULT_MEDIA).unwrap_or(&Value::Null);
          let at = at.key("media_reference");
          times_match(medium, find(stated_members, "media_reference"), &at)?;
        } else {
          times_match(value, find(stated_members, key), &at.key(key))?;
        }
      }
      // A time the document states where the export writes none.
      for (key, value) in stated_members {
        let written_there = if legacy_clip && key == "media_reference" {
          find(members, "media_references")
        } else {
          find(members, key)
        };
        if written_there.is_none() && carries_time(value) {
          return Err(inexact(
            &at.key(key),
            "a time the export does not write: none is written there",
          ));
        }
      }
      Ok(())
    }
    Value::Array(items) => {
      let stated_items = stated.and_then(Value::as_array).unwrap_or_default();
      for (index, item) in items.iter().enumerate() {
        times_match(item, stated_items.get(index), &at.index(index))?;
      }
      match stated_items
        .iter()
        .enumerate()
        .skip(items.len())
        .find(|(_, item)| carries_time(item))
      {
        Some((index, _)) => Err(inexact(
          &at.index(index),
          "a time the export does not write: none is written there",
        )),
        None => Ok(()),
      }
    }
    _ => match stated {
      Some(stated) if carries_time(stated) => Err(differs()),
      _ => Ok(()),
    },
  }
}

/// Whether two `RationalTime`s are strictly equal: the same `f64` value and
/// the same `f64` rate, read exactly from their numbers.
fn same_time(written: &Value, stated: &Value) -> bool {
  let read = |time: &Value, key: &str| {
    member(time, key).and_then(|number| match number {
      Value::Number(text) => json::exact_f64(text.as_str()),
      _ => None,
    })
  };
  schema_of(stated) == Some("RationalTime.1")
    && ["value", "rate"]
      .iter()
      .all(|key| matches!((read(written, key), read(stated, key)), (Some(a), Some(b)) if a == b))
}

/// Whether `value` holds a `RationalTime` anywhere.
fn carries_time(value: &Value) -> bool {
  match value {
    Value::Object(members) => {
      schema_of(value) == Some("RationalTime.1")
        || members.iter().any(|(_, value)| carries_time(value))
    }
    Value::Array(items) => items.iter().any(carries_time),
    _ => false,
  }
}

/// The schema an object names, if it is one that names one.
fn schema_of(value: &Value) -> Option<&str> {
  member(value, "OTIO_SCHEMA").and_then(Value::as_str)
}

/// The member `key` of `value`, if it is an object that has one.
fn member<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
  value.as_object().and_then(|members| find(members, key))
}

/// The member `key` among `members`.
fn find<'a>(members: &'a Members, key: &str) -> Option<&'a Value> {
  members
    .iter()
    .find(|(name, _)| name == key)
    .map(|(_, value)| value)
}

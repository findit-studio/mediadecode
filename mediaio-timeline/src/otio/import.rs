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
//! Counts are read where the export writes them, and held to the exact
//! words it writes beside them ([`export`](super::export)): each
//! `RationalTime`'s rate must be the `f64` of the exact ruler its word
//! names, bit for bit, and its value a whole count within ±2^53, which an
//! `f64` holds exactly; a media-side range is counted again in the timebase
//! the model counts it in, exactly ([`Unreadable::Inexact`] otherwise).
//! Records are not written: like OpenTimelineIO, the reader lays a track's
//! items end to end from zero, each clip as long as its source range at the
//! edit rate.

use core::str::FromStr;
use std::{boxed::Box, vec, vec::Vec};

use mediatime::{Duration, Rate, Rounding, TimeRange, Timebase, Timestamp};
use smol_bytes::{Utf8Bytes, format_utf8bytes};

use super::{
  Feature, Inexact, Shape, UnmodelledFeature, UnmodelledSchema, Unreadable,
  derive::Ruler,
  export::{DEFAULT_MEDIA, OURS},
  json::{self, Value},
};
use crate::{
  Clip, ClipId, Edge, Fade, FadeShape, Gain, Marker, MarkerColor, MediaRef, Metadata, Timeline,
  Track, TrackKind, Transition,
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
  Unreadable::Schema(UnmodelledSchema {
    name: Utf8Bytes::from(name),
    path: at.0.clone(),
  })
}

fn feature(at: &At, feature: Feature) -> Unreadable {
  Unreadable::Feature(UnmodelledFeature {
    feature,
    path: at.0.clone(),
  })
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

  /// A gain in decibels, finite.
  fn gain(&self, word: &str) -> Result<Option<Gain>, Unreadable> {
    let Some(value) = self.get(word) else {
      return Ok(None);
    };
    let at = self.at.key(word);
    number(value, &at)?
      .parse::<f32>()
      .ok()
      .and_then(Gain::from_db)
      .map(Some)
      .ok_or_else(|| shape(&at, "a finite gain in decibels"))
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

  /// The model's notes: an object of strings, under `metadata`.
  fn notes(&self) -> Result<Metadata, Unreadable> {
    let Some(value) = self.get("metadata") else {
      return Ok(Metadata::new());
    };
    let at = self.at.key("metadata");
    let notes = Object::of(value, at.clone())?;
    notes
      .members
      .iter()
      .map(|(key, note)| match note {
        Value::String(text) => Ok((key.clone(), text.clone())),
        _ => Err(shape(&at.key(key), "a string")),
      })
      .collect()
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

/// A `RationalTime.1` counted in `ruler`: its rate the `f64` of the exact
/// ruler, bit for bit, and its value a whole count within ±2^53.
fn count(value: &Value, at: &At, ruler: Ruler) -> Result<i64, Unreadable> {
  let object = Object::of(value, at.clone())?;
  object.schema(&["RationalTime.1"])?;
  object.keys(&["OTIO_SCHEMA", "rate", "value"])?;
  let rate_at = at.key("rate");
  let rate = number(object.required("rate")?, &rate_at)?
    .parse::<f64>()
    .map_err(|_| shape(&rate_at, "a number"))?;
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

/// A `TimeRange.1` counted in `ruler`: its start and its length.
fn counts(value: &Value, at: &At, ruler: Ruler) -> Result<(i64, u64), Unreadable> {
  let object = Object::of(value, at.clone())?;
  object.schema(&["TimeRange.1"])?;
  object.keys(&["OTIO_SCHEMA", "duration", "start_time"])?;
  let start = count(object.required("start_time")?, &at.key("start_time"), ruler)?;
  let length = length(object.required("duration")?, &at.key("duration"), ruler)?;
  Ok((start, length))
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
  let words = Words::of(&object, &["rate", "metadata"])?;
  let rate = words
    .rate("rate")?
    .ok_or_else(|| shape(&words.at.key("rate"), "a required word"))?;
  let edit = Ruler::new(rate);
  let ticks = rate.checked_to_timebase().unwrap_or_default();
  let start = count(
    object.required("global_start_time")?,
    &object.at.key("global_start_time"),
    edit,
  )?;
  let stack = Object::of(object.required("tracks")?, object.at.key("tracks"))?;
  let tracks = self::stack(&stack, edit, ticks)?;
  Ok(
    Timeline::new(object.name()?, rate)
      .with_start(Timestamp::new(start, ticks))
      .with_metadata(words.notes()?)
      .with_tracks(tracks),
  )
}

/// The one `Stack.1`, named `tracks` as OpenTimelineIO names it, holding
/// the tracks and nothing the timeline does not keep.
fn stack(stack: &Object<'_>, edit: Ruler, ticks: Timebase) -> Result<Vec<Track>, Unreadable> {
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
  array(stack.required("children")?, &at)?
    .iter()
    .enumerate()
    .map(|(index, child)| track(&Object::of(child, at.index(index))?, edit, ticks))
    .collect()
}

fn track(object: &Object<'_>, edit: Ruler, ticks: Timebase) -> Result<Track, Unreadable> {
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
  let (clips, transitions) = children(object, edit, ticks)?;
  *track.clips_mut() = clips;
  *track.transitions_mut() = transitions;
  *track.markers_mut() = markers(object, Placed::Edit(edit, ticks))?;
  Ok(track)
}

/// One child of a track as read, before the track places it.
enum Child {
  /// A gap, so many edit-rate ticks long.
  Gap(u64),
  /// A clip, and its length at the edit rate.
  Clip(Box<Clip>, u64),
  /// A fade at one edge of the clip beside it.
  Fade(Edge, Fade),
  /// A dissolve, its offsets at the edit rate, and its index among its
  /// track's transitions where the document says it.
  Dissolve(u64, u64, Option<usize>),
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
      Self::Fade(..) | Self::Dissolve(..) => Kind::Transition,
    }
  }
}

/// A track's clips, each at its record, and its transitions: its items
/// laid end to end from zero, as OpenTimelineIO places them, each fade on
/// the clip beside it and each dissolve on the cut it straddles.
fn children(
  object: &Object<'_>,
  edit: Ruler,
  ticks: Timebase,
) -> Result<(Vec<Clip>, Vec<Transition>), Unreadable> {
  let at = object.at.key("children");
  let values = array(object.required("children")?, &at)?;
  let mut read = Vec::with_capacity(values.len());
  for (index, value) in values.iter().enumerate() {
    let child = Object::of(value, at.index(index))?;
    read.push(
      match child.schema(&["Gap.1", "Clip.1", "Clip.2", "Transition.1"])? {
        "Gap.1" => Child::Gap(gap(&child, edit)?),
        "Transition.1" => transition(&child, edit, ticks)?,
        schema => {
          let (clip, length) = clip(&child, schema, ticks)?;
          Child::Clip(Box::new(clip), length)
        }
      },
    );
  }

  let kinds: Vec<Kind> = read.iter().map(Child::kind).collect();
  let mut cursor: i64 = 0;
  let mut clips: Vec<Clip> = Vec::new();
  let mut dissolves = Vec::new();
  let mut fade_in = None;
  for (index, child) in read.into_iter().enumerate() {
    let here = at.index(index);
    let before = index.checked_sub(1).and_then(|before| kinds.get(before).copied());
    let after = kinds.get(index + 1).copied();
    if kinds[index] == Kind::Transition
      && [before, after]
        .iter()
        .any(|neighbour| neighbour.is_none_or(|kind| kind == Kind::Transition))
    {
      return Err(shape(&here, "a transition between two items"));
    }
    match child {
      Child::Gap(length) => cursor = advance(cursor, length, &here)?,
      Child::Clip(mut clip, length) => {
        let end = advance(cursor, length, &here)?;
        clip.set_record(TimeRange::new(cursor, end, ticks));
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
      Child::Dissolve(in_offset, out_offset, order) => {
        if before != Some(Kind::Clip) || after != Some(Kind::Clip) {
          return Err(feature(&here, Feature::DissolveBesideGap));
        }
        let transition = Transition::dissolve(
          Timestamp::new(cursor, ticks),
          Duration::new(in_offset, ticks),
          Duration::new(out_offset, ticks),
        );
        dissolves.push((transition, order, here));
      }
    }
  }
  Ok((clips, ordered(dissolves)?))
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
    return Ok(dissolves.into_iter().map(|(dissolve, ..)| dissolve).collect());
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

/// A `Transition.1`: a fade, by this crate's words, or a dissolve.
fn transition(object: &Object<'_>, edit: Ruler, ticks: Timebase) -> Result<Child, Unreadable> {
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
  let words = Words::of(object, &["fade", "shape", "order"])?;
  let Some(fade) = words.text("fade")? else {
    if words.get("shape").is_some() {
      return Err(feature(
        &words.at.key("shape"),
        Feature::Word(Utf8Bytes::from_static("shape")),
      ));
    }
    let order = words
      .get("order")
      .map(|value| {
        let at = words.at.key("order");
        json::whole(number(value, &at)?)
          .and_then(|order| usize::try_from(order).ok())
          .ok_or_else(|| shape(&at, "a place among the track's transitions"))
      })
      .transpose()?;
    return Ok(Child::Dissolve(in_offset, out_offset, order));
  };
  if words.get("order").is_some() {
    return Err(feature(
      &words.at.key("order"),
      Feature::Word(Utf8Bytes::from_static("order")),
    ));
  }
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
  Ok(Child::Fade(
    edge,
    Fade::new(Duration::new(inside, ticks), curve),
  ))
}

/// A `Clip.1` or a `Clip.2`, and its length at the edit rate; its record is
/// the track's to place.
fn clip(object: &Object<'_>, schema: &str, ticks: Timebase) -> Result<(Clip, u64), Unreadable> {
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
  let words = Words::of(object, &["id", "gain_db", "source", "metadata"])?;
  let id = words.required_text("id")?;
  let exact = words.required_exact("source")?;
  object.no_effects()?;
  object.no_color()?;
  let at = object.at.key("source_range");
  let (start, length) = counts(object.required("source_range")?, &at, Ruler::new(exact.ruler))?;
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
    TimeRange::new(0, 0, ticks),
  )
  .with_enabled(object.enabled()?)
  .with_gain(words.gain("gain_db")?)
  .with_metadata(words.notes()?);
  *clip.markers_mut() = markers(object, Placed::Exact)?;
  Ok((clip, length))
}

/// The one medium a clip reads: `Clip.1`'s `media_reference`, or the one
/// reference of a `Clip.2`, under `DEFAULT_MEDIA` and active.
fn media(object: &Object<'_>, schema: &str) -> Result<MediaRef, Unreadable> {
  if schema != "Clip.2" {
    let at = object.at.key("media_reference");
    return match object.required("media_reference")? {
      Value::Null => Err(feature(&at, Feature::NoMedia)),
      value => reference(value, at),
    };
  }
  let at = object.at.key("media_references");
  let references = Object::of(object.required("media_references")?, at.clone())?;
  let active = object
    .string("active_media_reference_key")?
    .ok_or_else(|| shape(&object.at.key("active_media_reference_key"), "a required key"))?;
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
          Placed::Edit(..) => &[],
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
          edit_range(start, length, ticks)
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

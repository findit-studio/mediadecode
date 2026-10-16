//! OpenTimelineIO's schema shape, checked over a value the crate's own
//! reader produced.
//!
//! # Which keys are required
//!
//! Each schema's `read_from` in OpenTimelineIO's C++ core decides which keys
//! a document must carry: `reader.read` fails on a missing key, and
//! `reader.read_if_present` does not. The check requires exactly the keys
//! the `reader.read` calls take, for every reader of its target — releases
//! 0.15 to 0.18.1 and `main` (`00c22fa`) for [`OtioTarget::V0_15Plus`]; for
//! [`OtioTarget::Legacy`], the C++ readers of 0.12 to 0.14.1 (the Python
//! ones before them require less). Lines are `main`'s under
//! `src/opentimelineio/` unless a release is named:
//!
//! | schema | required | where |
//! |---|---|---|
//! | `Timeline.1` | `tracks` | `timeline.cpp:30` |
//! | `Stack.1` | `children` | `composition.cpp:199` |
//! | `Track.1` | `kind`, `children` | `track.cpp:41` (0.14.1: `:31`), `composition.cpp:199` |
//! | `Clip.2` | `media_references`, `active_media_reference_key` | `clip.cpp:145–148` |
//! | `Clip.1` | `media_reference` under `Legacy`; nothing under `V0_15Plus` | 0.14.1 `clip.cpp:34`, 0.12 `clip.cpp:28`; a later reader upgrades a `Clip.1`, reading a reference left out as missing (`typeRegistry.cpp:159–177`) |
//! | `Gap.1` | — | an item's keys only |
//! | `Transition.1` | `in_offset`, `out_offset`, `transition_type` | `transition.cpp:35–37` |
//! | `ExternalReference.1` | `target_url` | `externalReference.cpp:23` |
//! | `Marker.2` | `marked_range` | `marker.cpp:28` (0.15: `:26`, 0.14.1: `:23`, 0.12: `:20`) |
//! | `Effect.1`, `TimeEffect.1` | `effect_name` | `effect.cpp:25` (0.15: `:23`, 0.14.1: `:20`, 0.12: `:17`) |
//! | `LinearTimeWarp.1`, `FreezeFrame.1` | `time_scalar`, then an effect's | `linearTimeWarp.cpp:23` (0.14.1: `:20`, 0.12: `:17`) |
//! | `RationalTime.1` | `rate`, `value` | `deserialization.cpp:578–583` |
//! | `TimeRange.1` | `start_time`, `duration` | `deserialization.cpp:585–591` |
//! | `TimeTransform.1` | `offset`, `rate`, `scale` | `deserialization.cpp:602–610` |
//! | `V2d.1` | `x`, `y` | `deserialization.cpp:621–627`, from 0.15 |
//! | `Box2d.1` | `min`, `max` | `deserialization.cpp:628–635`, from 0.15 |
//! | `Color.1` | `name`, `r`, `g`, `b`, `a` | `deserialization.cpp:593–600`, from 0.18 |
//!
//! A reader that does not decode a value schema — one before 0.15 a `V2d.1`
//! or a `Box2d.1`, one before 0.18 a `Color.1` — keeps the dictionary as an
//! object of a schema it does not know (below); the check holds it to the
//! newer readers' rules for every target.
//!
//! Everything else these schemas read is read only if present: `metadata`
//! and `name` on each (`serializableObjectWithMetadata.cpp:21–22`), an
//! item's `source_range`, `effects`, `markers` and `enabled`
//! (`item.cpp:185–188`), a timeline's `global_start_time`
//! (`timeline.cpp:31`), a media reference's `available_range` and
//! `available_image_bounds` (`mediaReference.cpp:30–33`), a marker's
//! `color` and, from 0.16, its `comment` (`marker.cpp:29`; readers
//! before 0.16 keep a `comment` as a field they do not know, and write it
//! back).
//!
//! # Every dictionary
//!
//! A reader converts each dictionary as its parser closes it, wherever it
//! sits (`deserialization.cpp:156–190`): one without `OTIO_SCHEMA` stays a
//! dictionary, and one carrying it becomes an object of the schema it names
//! (`:564–568`), which that schema's own `read_from` then reads
//! (`serializableObject.h:222–235`). So a dictionary in metadata, under a
//! key no reader reads, or nested inside either, refuses the document when
//! its schema's reader would. The check walks every dictionary alike and
//! holds each one carrying `OTIO_SCHEMA` to that schema's rules, wherever it
//! sits. Metadata itself is a dictionary, never a schema's object.
//!
//! A schema whose rules the check does not know is refused by name, at its
//! `OTIO_SCHEMA`. In one case that is stricter than a reader: a name no
//! reader registers becomes an `UnknownSchema` (`typeRegistry.cpp:260–290`
//! in 0.17.0), which a reader keeps where the document leaves the type
//! open. The check does not vouch for an object it cannot read, and the
//! export writes none.
//!
//! # What a value is held to
//!
//! A value present is held to the type the reader takes, and in places to
//! more — a string where a reader would take `null` as empty, a positive,
//! finite rate, a length of zero or more, an active media key among the
//! references, every transition between two items, every marker a
//! `Marker.2` — the one marker schema the readers 0.12 to 0.18.1 know —
//! every effect one of the four effect schemas, and, for readers from
//! 0.15, a media reference's image bounds a `Box2d.1`. Keys this crate
//! never writes and no reader of the target requires are walked but not
//! typed: a newer reader's `color` on an item or `enabled` on a transition
//! or an effect, a generator's parameters.

use std::format;

use smol_bytes::{Utf8Bytes, format_utf8bytes};

use super::{OtioTarget, Shape, json::Value};

type Members = [(Utf8Bytes, Value)];

/// The keys every schema with metadata reads.
const NAMED: &[&str] = &["metadata", "name"];

/// The keys every item reads, beside [`NAMED`].
const ITEM: &[&str] = &["source_range", "effects", "markers", "enabled"];

const MEDIA_REFERENCES: &[&str] = &[
  "ExternalReference.1",
  "MissingReference.1",
  "GeneratorReference.1",
  "ImageSequenceReference.1",
];

const EFFECTS: &[&str] = &[
  "Effect.1",
  "TimeEffect.1",
  "LinearTimeWarp.1",
  "FreezeFrame.1",
];

pub(crate) fn timeline(root: &Value, target: OtioTarget) -> Result<(), Shape> {
  timeline_at(root, "$", target)
}

fn timeline_at(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Timeline.1"], "the schema Timeline.1")?;
  named(members, path, target)?;
  optional(members, path, "global_start_time", |value, at| {
    nullable(value, at, |value, at| rational_time(value, at, target))
  })?;
  let (tracks, at) = required(members, path, "tracks")?;
  stack(tracks, &at, target)?;
  others(
    members,
    path,
    target,
    &[NAMED, &["global_start_time", "tracks"]],
  )
}

fn stack(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Stack.1"], "the schema Stack.1")?;
  item(members, path, target)?;
  let (children, at) = required(members, path, "children")?;
  for (index, child) in array(children, &at)?.iter().enumerate() {
    composable(child, &format!("{at}[{index}]"), target, false)?;
  }
  others(members, path, target, &[NAMED, ITEM, &["children"]])
}

fn track(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Track.1"], "the schema Track.1")?;
  item(members, path, target)?;
  let (kind, at) = required(members, path, "kind")?;
  string(kind, &at)?;
  let (children, at) = required(members, path, "children")?;
  let children = array(children, &at)?;
  let is_transition = |index: usize| {
    children
      .get(index)
      .and_then(Value::as_object)
      .and_then(|members| member(members, "OTIO_SCHEMA"))
      .and_then(Value::as_str)
      == Some("Transition.1")
  };
  for (index, child) in children.iter().enumerate() {
    let here = format!("{at}[{index}]");
    composable(child, &here, target, true)?;
    if is_transition(index)
      && (index == 0
        || index + 1 == children.len()
        || is_transition(index - 1)
        || is_transition(index + 1))
    {
      return Err(shape(&here, "a transition between two items"));
    }
  }
  others(members, path, target, &[NAMED, ITEM, &["kind", "children"]])
}

fn composable(value: &Value, path: &str, target: OtioTarget, in_track: bool) -> Result<(), Shape> {
  let members = object(value, path)?;
  let name = schema_name(members, path)?;
  match name {
    "Track.1" => track(value, path, target),
    "Stack.1" => stack(value, path, target),
    "Gap.1" => {
      item(members, path, target)?;
      others(members, path, target, &[NAMED, ITEM])
    }
    "Clip.1" => clip1(members, path, target),
    "Clip.2" if target == OtioTarget::V0_15Plus => clip2(members, path, target),
    "Clip.2" => Err(shape(
      &format!("{path}.OTIO_SCHEMA"),
      "Clip.1, the only clip readers before OpenTimelineIO 0.15 know",
    )),
    "Transition.1" if in_track => transition(members, path, target),
    _ => Err(shape(
      &format!("{path}.OTIO_SCHEMA"),
      if in_track {
        "a schema a track holds"
      } else {
        "a schema a stack holds"
      },
    )),
  }
}

fn clip1(members: &Members, path: &str, target: OtioTarget) -> Result<(), Shape> {
  item(members, path, target)?;
  let reference =
    |value: &Value, at: &str| nullable(value, at, |value, at| media_reference(value, at, target));
  match target {
    OtioTarget::V0_15Plus => optional(members, path, "media_reference", reference)?,
    OtioTarget::Legacy => {
      let (value, at) = required(members, path, "media_reference")?;
      reference(value, &at)?;
    }
  }
  others(members, path, target, &[NAMED, ITEM, &["media_reference"]])
}

fn clip2(members: &Members, path: &str, target: OtioTarget) -> Result<(), Shape> {
  item(members, path, target)?;
  let (references, at) = required(members, path, "media_references")?;
  let references = object(references, &at)?;
  for (key, reference) in references {
    media_reference(reference, &format!("{at}.{key}"), target)?;
  }
  let (active, at) = required(members, path, "active_media_reference_key")?;
  let active = string_value(active, &at)?;
  if member(references, active).is_none() {
    return Err(shape(&at, "a key of media_references"));
  }
  others(
    members,
    path,
    target,
    &[
      NAMED,
      ITEM,
      &["media_references", "active_media_reference_key"],
    ],
  )
}

fn media_reference(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, MEDIA_REFERENCES, "a media reference schema")?;
  named(members, path, target)?;
  optional(members, path, "available_range", |value, at| {
    nullable(value, at, |value, at| time_range(value, at, target))
  })?;
  // Read from 0.15 as a `Box2d`; kept before as a key the reader does not
  // know.
  optional(members, path, "available_image_bounds", |value, at| {
    nullable(value, at, |value, at| match target {
      OtioTarget::V0_15Plus => box2d(value, at, target),
      OtioTarget::Legacy => {
        object(value, at)?;
        contents(value, at, target)
      }
    })
  })?;
  let external = schema_name(members, path)? == "ExternalReference.1";
  if external {
    let (url, at) = required(members, path, "target_url")?;
    string(url, &at)?;
  }
  let url: &[&str] = if external { &["target_url"] } else { &[] };
  others(
    members,
    path,
    target,
    &[NAMED, &["available_range", "available_image_bounds"], url],
  )
}

fn transition(members: &Members, path: &str, target: OtioTarget) -> Result<(), Shape> {
  named(members, path, target)?;
  let (in_offset, at) = required(members, path, "in_offset")?;
  length(in_offset, &at, target)?;
  let (out_offset, at) = required(members, path, "out_offset")?;
  length(out_offset, &at, target)?;
  let (kind, at) = required(members, path, "transition_type")?;
  string(kind, &at)?;
  others(
    members,
    path,
    target,
    &[NAMED, &["in_offset", "out_offset", "transition_type"]],
  )
}

/// The fields every item carries: a name and metadata, a trim, effects,
/// markers, and whether it plays.
fn item(members: &Members, path: &str, target: OtioTarget) -> Result<(), Shape> {
  named(members, path, target)?;
  optional(members, path, "source_range", |value, at| {
    nullable(value, at, |value, at| time_range(value, at, target))
  })?;
  optional(members, path, "effects", |value, at| {
    for (index, entry) in array(value, at)?.iter().enumerate() {
      effect(entry, &format!("{at}[{index}]"), target)?;
    }
    Ok(())
  })?;
  optional(members, path, "markers", |value, at| {
    for (index, entry) in array(value, at)?.iter().enumerate() {
      marker(entry, &format!("{at}[{index}]"), target)?;
    }
    Ok(())
  })?;
  optional(members, path, "enabled", |value, at| match value {
    Value::Bool(_) => Ok(()),
    _ => Err(shape(at, "a boolean")),
  })
}

/// A `Marker.2`: its name and metadata, its color and comment strings, and
/// the range it marks, which every reader requires.
fn marker(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Marker.2"], "the schema Marker.2")?;
  named(members, path, target)?;
  optional(members, path, "color", string)?;
  let (range, at) = required(members, path, "marked_range")?;
  time_range(range, &at, target)?;
  optional(members, path, "comment", string)?;
  others(
    members,
    path,
    target,
    &[NAMED, &["color", "marked_range", "comment"]],
  )
}

/// An effect: its name and metadata, the `effect_name` every reader
/// requires, and a time warp's `time_scalar`. An item holds its effects as
/// `Effect`s, so every reader refuses another schema there too.
fn effect(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, EFFECTS, "an effect schema")?;
  named(members, path, target)?;
  let (name, at) = required(members, path, "effect_name")?;
  string(name, &at)?;
  let warp = matches!(
    schema_name(members, path)?,
    "LinearTimeWarp.1" | "FreezeFrame.1"
  );
  if warp {
    let (scalar, at) = required(members, path, "time_scalar")?;
    number(scalar, &at)?;
  }
  let scalar: &[&str] = if warp { &["time_scalar"] } else { &[] };
  others(members, path, target, &[NAMED, &["effect_name"], scalar])
}

fn named(members: &Members, path: &str, target: OtioTarget) -> Result<(), Shape> {
  optional(members, path, "metadata", |value, at| {
    dictionary(value, at, target)
  })?;
  optional(members, path, "name", string)
}

/// Metadata: a dictionary every reader takes as one, never a schema's
/// object. Its values are walked as any others are ([`contents`]).
fn dictionary(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = object(value, path)?;
  if member(members, "OTIO_SCHEMA").is_some() {
    return Err(shape(
      &format!("{path}.OTIO_SCHEMA"),
      "no schema: metadata is a dictionary",
    ));
  }
  contents(value, path, target)
}

/// A value where the document leaves the type open — metadata, a key no
/// reader reads, and what either holds. A reader decodes every dictionary
/// there all the same: a plain one stays a dictionary, whose values are
/// walked in turn, and one carrying `OTIO_SCHEMA` is an object of that
/// schema ([`any_schema`]).
fn contents(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  match value {
    Value::Array(entries) => {
      for (index, entry) in entries.iter().enumerate() {
        contents(entry, &format!("{path}[{index}]"), target)?;
      }
      Ok(())
    }
    Value::Object(members) => {
      if member(members, "OTIO_SCHEMA").is_some() {
        return any_schema(value, path, target);
      }
      for (key, entry) in members {
        contents(entry, &format!("{path}.{key}"), target)?;
      }
      Ok(())
    }
    _ => Ok(()),
  }
}

/// A dictionary carrying `OTIO_SCHEMA` where the document leaves the type
/// open: held to that schema's rules, or refused by name where the check
/// does not know them.
fn any_schema(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = object(value, path)?;
  match schema_name(members, path)? {
    "Timeline.1" => timeline_at(value, path, target),
    "Stack.1" | "Track.1" | "Gap.1" | "Clip.1" | "Clip.2" => composable(value, path, target, false),
    "Transition.1" => transition(members, path, target),
    "Marker.2" => marker(value, path, target),
    name if MEDIA_REFERENCES.contains(&name) => media_reference(value, path, target),
    name if EFFECTS.contains(&name) => effect(value, path, target),
    "RationalTime.1" => rational_time(value, path, target),
    "TimeRange.1" => time_range(value, path, target),
    "TimeTransform.1" => time_transform(value, path, target),
    "V2d.1" => v2d(value, path, target),
    "Box2d.1" => box2d(value, path, target),
    "Color.1" => color(value, path, target),
    _ => Err(shape(
      &format!("{path}.OTIO_SCHEMA"),
      "a schema whose rules the check knows",
    )),
  }
}

/// The members a schema's reader leaves unread, given the keys it reads: a
/// reader keeps them as the object's own, decoded as any others are
/// ([`contents`]).
fn others(
  members: &Members,
  path: &str,
  target: OtioTarget,
  read: &[&[&str]],
) -> Result<(), Shape> {
  for (key, value) in members {
    let key = key.as_str();
    if key != "OTIO_SCHEMA" && !read.iter().any(|keys| keys.contains(&key)) {
      contents(value, &format!("{path}.{key}"), target)?;
    }
  }
  Ok(())
}

fn time_range(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["TimeRange.1"], "the schema TimeRange.1")?;
  let (duration, at) = required(members, path, "duration")?;
  length(duration, &at, target)?;
  let (start, at) = required(members, path, "start_time")?;
  rational_time(start, &at, target)?;
  others(members, path, target, &[&["duration", "start_time"]])
}

/// A `RationalTime.1` that measures a length: no shorter than nothing.
fn length(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  rational_time(value, path, target)?;
  let members = object(value, path)?;
  match member(members, "value").and_then(Value::as_f64) {
    Some(value) if value >= 0.0 => Ok(()),
    _ => Err(shape(&format!("{path}.value"), "a length of zero or more")),
  }
}

fn rational_time(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(
    value,
    path,
    &["RationalTime.1"],
    "the schema RationalTime.1",
  )?;
  let (rate, at) = required(members, path, "rate")?;
  match rate.as_f64() {
    Some(rate) if rate.is_finite() && rate > 0.0 => {}
    _ => return Err(shape(&at, "a positive, finite rate")),
  }
  let (count, at) = required(members, path, "value")?;
  number(count, &at)?;
  others(members, path, target, &[&["rate", "value"]])
}

fn time_transform(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(
    value,
    path,
    &["TimeTransform.1"],
    "the schema TimeTransform.1",
  )?;
  let (offset, at) = required(members, path, "offset")?;
  rational_time(offset, &at, target)?;
  for key in ["rate", "scale"] {
    let (value, at) = required(members, path, key)?;
    number(value, &at)?;
  }
  others(members, path, target, &[&["offset", "rate", "scale"]])
}

fn v2d(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["V2d.1"], "the schema V2d.1")?;
  for key in ["x", "y"] {
    let (value, at) = required(members, path, key)?;
    number(value, &at)?;
  }
  others(members, path, target, &[&["x", "y"]])
}

fn box2d(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Box2d.1"], "the schema Box2d.1")?;
  for key in ["min", "max"] {
    let (corner, at) = required(members, path, key)?;
    v2d(corner, &at, target)?;
  }
  others(members, path, target, &[&["min", "max"]])
}

fn color(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Color.1"], "the schema Color.1")?;
  let (name, at) = required(members, path, "name")?;
  string(name, &at)?;
  for key in ["r", "g", "b", "a"] {
    let (value, at) = required(members, path, key)?;
    number(value, &at)?;
  }
  others(members, path, target, &[&["name", "r", "g", "b", "a"]])
}

fn schema<'a>(
  value: &'a Value,
  path: &str,
  allowed: &[&str],
  expected: &'static str,
) -> Result<&'a Members, Shape> {
  let members = object(value, path)?;
  let name = schema_name(members, path)?;
  if allowed.contains(&name) {
    Ok(members)
  } else {
    Err(shape(&format!("{path}.OTIO_SCHEMA"), expected))
  }
}

fn schema_name<'a>(members: &'a Members, path: &str) -> Result<&'a str, Shape> {
  member(members, "OTIO_SCHEMA")
    .and_then(Value::as_str)
    .ok_or_else(|| shape(&format!("{path}.OTIO_SCHEMA"), "a schema name"))
}

fn required<'a>(
  members: &'a Members,
  path: &str,
  key: &str,
) -> Result<(&'a Value, Utf8Bytes), Shape> {
  let at = format_utf8bytes!("{path}.{key}");
  match member(members, key) {
    Some(value) => Ok((value, at)),
    None => Err(shape(&at, "a required key")),
  }
}

fn optional(
  members: &Members,
  path: &str,
  key: &str,
  check: impl Fn(&Value, &str) -> Result<(), Shape>,
) -> Result<(), Shape> {
  match member(members, key) {
    Some(value) => check(value, &format!("{path}.{key}")),
    None => Ok(()),
  }
}

fn nullable(
  value: &Value,
  path: &str,
  check: impl Fn(&Value, &str) -> Result<(), Shape>,
) -> Result<(), Shape> {
  match value {
    Value::Null => Ok(()),
    value => check(value, path),
  }
}

fn member<'a>(members: &'a Members, key: &str) -> Option<&'a Value> {
  members
    .iter()
    .find(|(name, _)| name == key)
    .map(|(_, value)| value)
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a Members, Shape> {
  value.as_object().ok_or_else(|| shape(path, "an object"))
}

fn array<'a>(value: &'a Value, path: &str) -> Result<&'a [Value], Shape> {
  value.as_array().ok_or_else(|| shape(path, "an array"))
}

fn string(value: &Value, path: &str) -> Result<(), Shape> {
  string_value(value, path).map(drop)
}

fn string_value<'a>(value: &'a Value, path: &str) -> Result<&'a str, Shape> {
  value.as_str().ok_or_else(|| shape(path, "a string"))
}

/// A finite number: what a reader takes for a `double`.
fn number(value: &Value, path: &str) -> Result<(), Shape> {
  match value.as_f64() {
    Some(number) if number.is_finite() => Ok(()),
    _ => Err(shape(path, "a finite number")),
  }
}

fn shape(path: &str, expected: &'static str) -> Shape {
  Shape {
    path: Utf8Bytes::from(path),
    expected,
  }
}

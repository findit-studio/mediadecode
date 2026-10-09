//! OpenTimelineIO's schema shape, checked over a value the crate's own
//! reader produced.
//!
//! Each schema's required keys are the ones OpenTimelineIO's reader cannot
//! do without; its other keys may be absent, but where present must have
//! their type. A key OpenTimelineIO does not define is left alone, as its
//! reader keeps one.

use alloc::{format, string::String};

use super::{OtioTarget, Shape, json::Value};

type Members = [(String, Value)];

pub(crate) fn timeline(root: &Value, target: OtioTarget) -> Result<(), Shape> {
  let path = "$";
  let members = schema(root, path, &["Timeline.1"], "the schema Timeline.1")?;
  named(members, path)?;
  optional(members, path, "global_start_time", |value, at| {
    nullable(value, at, rational_time)
  })?;
  let (tracks, at) = required(members, path, "tracks")?;
  stack(tracks, &at, target)
}

fn stack(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Stack.1"], "the schema Stack.1")?;
  item(members, path)?;
  let (children, at) = required(members, path, "children")?;
  for (index, child) in array(children, &at)?.iter().enumerate() {
    composable(child, &format!("{at}[{index}]"), target, false)?;
  }
  Ok(())
}

fn track(value: &Value, path: &str, target: OtioTarget) -> Result<(), Shape> {
  let members = schema(value, path, &["Track.1"], "the schema Track.1")?;
  item(members, path)?;
  optional(members, path, "kind", string)?;
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
  Ok(())
}

fn composable(value: &Value, path: &str, target: OtioTarget, in_track: bool) -> Result<(), Shape> {
  let members = object(value, path)?;
  let name = schema_name(members, path)?;
  match name {
    "Track.1" => track(value, path, target),
    "Stack.1" => stack(value, path, target),
    "Gap.1" => item(members, path),
    "Clip.1" => {
      item(members, path)?;
      optional(members, path, "media_reference", |value, at| {
        nullable(value, at, media_reference)
      })
    }
    "Clip.2" if target == OtioTarget::V0_15Plus => clip2(members, path),
    "Clip.2" => Err(shape(
      &format!("{path}.OTIO_SCHEMA"),
      "Clip.1, the only clip readers before OpenTimelineIO 0.15 know",
    )),
    "Transition.1" if in_track => transition(members, path),
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

fn clip2(members: &Members, path: &str) -> Result<(), Shape> {
  item(members, path)?;
  let (references, at) = required(members, path, "media_references")?;
  let references = object(references, &at)?;
  for (key, reference) in references {
    media_reference(reference, &format!("{at}.{key}"))?;
  }
  let (active, at) = required(members, path, "active_media_reference_key")?;
  let active = string_value(active, &at)?;
  if member(references, active).is_none() {
    return Err(shape(&at, "a key of media_references"));
  }
  Ok(())
}

fn media_reference(value: &Value, path: &str) -> Result<(), Shape> {
  let members = schema(
    value,
    path,
    &[
      "ExternalReference.1",
      "MissingReference.1",
      "GeneratorReference.1",
      "ImageSequenceReference.1",
    ],
    "a media reference schema",
  )?;
  named(members, path)?;
  optional(members, path, "available_range", |value, at| {
    nullable(value, at, time_range)
  })?;
  optional(members, path, "available_image_bounds", |value, at| {
    nullable(value, at, |value, at| object(value, at).map(drop))
  })?;
  if schema_name(members, path)? == "ExternalReference.1" {
    let (url, at) = required(members, path, "target_url")?;
    string(url, &at)?;
  }
  Ok(())
}

fn transition(members: &Members, path: &str) -> Result<(), Shape> {
  named(members, path)?;
  let (in_offset, at) = required(members, path, "in_offset")?;
  length(in_offset, &at)?;
  let (out_offset, at) = required(members, path, "out_offset")?;
  length(out_offset, &at)?;
  let (kind, at) = required(members, path, "transition_type")?;
  string(kind, &at)
}

/// The fields every item carries: a name and metadata, a trim, effects,
/// markers, and whether it plays.
fn item(members: &Members, path: &str) -> Result<(), Shape> {
  named(members, path)?;
  optional(members, path, "source_range", |value, at| {
    nullable(value, at, time_range)
  })?;
  optional(members, path, "effects", schema_objects)?;
  optional(members, path, "markers", schema_objects)?;
  optional(members, path, "enabled", |value, at| match value {
    Value::Bool(_) => Ok(()),
    _ => Err(shape(at, "a boolean")),
  })
}

fn named(members: &Members, path: &str) -> Result<(), Shape> {
  optional(members, path, "metadata", |value, at| {
    object(value, at).map(drop)
  })?;
  optional(members, path, "name", string)
}

fn time_range(value: &Value, path: &str) -> Result<(), Shape> {
  let members = schema(value, path, &["TimeRange.1"], "the schema TimeRange.1")?;
  let (duration, at) = required(members, path, "duration")?;
  length(duration, &at)?;
  let (start, at) = required(members, path, "start_time")?;
  rational_time(start, &at)
}

/// A `RationalTime.1` that measures a length: no shorter than nothing.
fn length(value: &Value, path: &str) -> Result<(), Shape> {
  rational_time(value, path)?;
  let members = object(value, path)?;
  match member(members, "value").and_then(Value::as_f64) {
    Some(value) if value >= 0.0 => Ok(()),
    _ => Err(shape(&format!("{path}.value"), "a length of zero or more")),
  }
}

fn rational_time(value: &Value, path: &str) -> Result<(), Shape> {
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
  match count.as_f64() {
    Some(count) if count.is_finite() => Ok(()),
    _ => Err(shape(&at, "a finite number")),
  }
}

/// An array of objects that each name their schema: effects and markers,
/// whose own fields this check does not read.
fn schema_objects(value: &Value, path: &str) -> Result<(), Shape> {
  for (index, entry) in array(value, path)?.iter().enumerate() {
    let at = format!("{path}[{index}]");
    schema_name(object(entry, &at)?, &at)?;
  }
  Ok(())
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

fn required<'a>(members: &'a Members, path: &str, key: &str) -> Result<(&'a Value, String), Shape> {
  let at = format!("{path}.{key}");
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

fn shape(path: &str, expected: &'static str) -> Shape {
  Shape {
    path: String::from(path),
    expected,
  }
}

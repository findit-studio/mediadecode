//! The reader: what the export writes reads back exactly, and what the
//! timeline does not model is refused by name, with where it sits.

use std::{string::String, vec, vec::Vec};

use smol_bytes::{Utf8Bytes, Utf8BytesMut};

use super::{
  edit,
  json::{self, Value},
};
use crate::{
  Clip, ClipAt, ClipId, Duration, Fade, FadeShape, Fades, Gain, IdClash, Marker, MarkerColor,
  MediaRef, Metadata, Rate, Refusal, TimeRange, Timebase, Timeline, Timestamp, Track, TrackKind,
  Transition,
  otio::{
    Feature, Inexact, Invalid, OtioTarget, Shape, UnmodelledFeature, UnmodelledSchema, Unreadable,
    from_otio, to_otio, validate_json,
  },
};

/// One track at 24 fps: a 12-frame gap; `a` at [12, 36), fading in over 6
/// frames, at -3 dB, with a marker; a 2 + 2-frame dissolve at 36 into `b`
/// at [36, 60). The track carries a red marker over its first second.
fn sample() -> Timeline {
  let medium = |name: &str| {
    MediaRef::new(smol_bytes::format_utf8bytes!("file:///{name}.mov"))
      .with_available_range(Some(TimeRange::new(0, 100, edit())))
      .with_rate(Some(Rate::FPS_24))
  };
  let a = Clip::new(
    ClipId::new("a"),
    "a",
    medium("a"),
    TimeRange::new(10, 34, edit()),
    TimeRange::new(12, 36, edit()),
  )
  .with_gain(Gain::from_db(-3.0))
  .with_fades(Fades::new().with_in(Some(Fade::new(Duration::new(6, edit()), FadeShape::Linear))))
  .with_marker(Marker::new("n", TimeRange::new(10, 12, edit())));
  let b = Clip::new(
    ClipId::new("b"),
    "b",
    medium("b"),
    TimeRange::new(10, 34, edit()),
    TimeRange::new(36, 60, edit()),
  );
  Timeline::new("sample", Rate::FPS_24).with_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(a)
      .with_clip(b)
      .with_transition(Transition::dissolve(
        Timestamp::new(36, edit()),
        Duration::new(2, edit()),
        Duration::new(2, edit()),
      ))
      .with_marker(Marker::new("m", TimeRange::new(0, 24, edit())).with_color(MarkerColor::Red)),
  )
}

/// The sample's first track, as the export lays it out:
/// `[gap, fade in, a, dissolve, b]`.
const TRACK: &str = "tracks.children.0";
const GAP: &str = "tracks.children.0.children.0";
const FADE_IN: &str = "tracks.children.0.children.1";
const A: &str = "tracks.children.0.children.2";
const DISSOLVE: &str = "tracks.children.0.children.3";
const B: &str = "tracks.children.0.children.4";
const A_MEDIA: &str = "tracks.children.0.children.2.media_references.DEFAULT_MEDIA";

/// The value at the dotted `path` inside `value`: object keys, and array
/// indices as numbers.
fn at<'a>(value: &'a mut Value, path: &str) -> &'a mut Value {
  path
    .split('.')
    .filter(|step| !step.is_empty())
    .fold(value, |value, step| match value {
      Value::Object(members) => {
        &mut members
          .iter_mut()
          .find(|(key, _)| key == step)
          .unwrap_or_else(|| panic!("no {step}"))
          .1
      }
      Value::Array(items) => &mut items[step.parse::<usize>().unwrap()],
      _ => panic!("{step} inside a scalar"),
    })
}

/// Sets `key` of the object at `path` to `new`, in place or appended.
fn set(value: &mut Value, path: &str, key: &str, new: Value) {
  let Value::Object(members) = at(value, path) else {
    panic!("{path} is no object")
  };
  match members.iter_mut().find(|(name, _)| name == key) {
    Some((_, old)) => *old = new,
    None => members.push((Utf8Bytes::from(key), new)),
  }
}

/// Takes `key` out of the object at `path`.
fn remove(value: &mut Value, path: &str, key: &str) {
  let Value::Object(members) = at(value, path) else {
    panic!("{path} is no object")
  };
  members.retain(|(name, _)| name != key);
}

fn text(value: &str) -> Value {
  Value::String(Utf8Bytes::from(value))
}

fn number(value: &str) -> Value {
  Value::Number(Utf8Bytes::from(value))
}

fn object(members: Vec<(&str, Value)>) -> Value {
  Value::Object(
    members
      .into_iter()
      .map(|(key, value)| (Utf8Bytes::from(key), value))
      .collect(),
  )
}

/// The sample written for `target`, edited, written out again and read.
fn read_edited(target: OtioTarget, edit: impl FnOnce(&mut Value)) -> Result<Timeline, Unreadable> {
  let mut root = json::parse(&to_otio(&sample(), target).unwrap()).unwrap();
  edit(&mut root);
  let mut out = Utf8BytesMut::new();
  json::write_pretty(&root, &mut out);
  from_otio(&out.freeze_shared())
}

/// [`read_edited`] for `V0_15Plus` readers.
fn read(edit: impl FnOnce(&mut Value)) -> Result<Timeline, Unreadable> {
  read_edited(OtioTarget::V0_15Plus, edit)
}

/// `$.` and the dotted path, as the reader writes one.
fn path(dotted: &str) -> String {
  let mut path = String::from("$");
  for step in dotted.split('.').filter(|step| !step.is_empty()) {
    if step.bytes().all(|byte| byte.is_ascii_digit()) {
      path.push('[');
      path.push_str(step);
      path.push(']');
    } else {
      path.push('.');
      path.push_str(step);
    }
  }
  path
}

fn schema(name: &str, dotted: &str) -> Unreadable {
  Unreadable::Schema(UnmodelledSchema::new(
    Utf8Bytes::from(name),
    Utf8Bytes::from(path(dotted)),
  ))
}

fn feature(feature: Feature, dotted: &str) -> Unreadable {
  Unreadable::Feature(UnmodelledFeature::new(
    feature,
    Utf8Bytes::from(path(dotted)),
  ))
}

fn shape(dotted: &str, expected: &'static str) -> Unreadable {
  Unreadable::Shape(Shape {
    path: Utf8Bytes::from(path(dotted)),
    expected,
  })
}

fn inexact(dotted: &str, reason: &'static str) -> Unreadable {
  Unreadable::Inexact(Inexact {
    path: Utf8Bytes::from(path(dotted)),
    reason,
  })
}

#[test]
fn what_the_export_writes_reads_back_exactly() {
  let sample = sample();
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    let text = to_otio(&sample, target).unwrap();
    assert_eq!(from_otio(&text), Ok(sample.clone()), "{target:?}");
  }
}

#[test]
fn what_carries_nothing_reads_as_nothing() {
  // A newer reader's `color` left unset, a transition `enabled`, empty
  // words, the stack unnamed, and a comment and a color left out: none
  // carries anything the timeline would lose.
  let read = read(|root| {
    set(root, A, "color", Value::Null);
    set(root, DISSOLVE, "enabled", Value::Bool(true));
    set(
      root,
      GAP,
      "metadata",
      object(vec![("mediaio", object(vec![]))]),
    );
    set(root, "tracks", "name", text(""));
    let marker = &format!("{TRACK}.markers.0");
    remove(root, marker, "comment");
    set(root, marker, "color", text("red"));
  });
  assert_eq!(read, Ok(sample()));
  // A marker with no color is OpenTimelineIO's default, green.
  let green = self::read(|root| remove(root, &format!("{TRACK}.markers.0"), "color")).unwrap();
  assert_eq!(green.tracks()[0].markers()[0].color(), MarkerColor::Green);
}

/// Every schema the export writes: an object carrying `OTIO_SCHEMA` is
/// one of these, or a reader would take a plain dictionary for an object.
const SCHEMAS: [&str; 11] = [
  "Timeline.1",
  "Stack.1",
  "Track.1",
  "Clip.1",
  "Clip.2",
  "Gap.1",
  "Transition.1",
  "ExternalReference.1",
  "Marker.2",
  "RationalTime.1",
  "TimeRange.1",
];

/// Whether every dictionary in `value` OpenTimelineIO reads as a plain one
/// is one: no object below a `metadata` member carries `OTIO_SCHEMA`, and
/// every object that does names a schema the export writes.
fn plain_where_it_should_be(value: &Value, in_metadata: bool) -> bool {
  match value {
    Value::Object(members) => {
      let schema = members
        .iter()
        .find(|(key, _)| key == "OTIO_SCHEMA")
        .map(|(_, schema)| schema);
      let schema_ok = match schema {
        None => true,
        Some(Value::String(name)) => !in_metadata && SCHEMAS.contains(&name.as_str()),
        Some(_) => false,
      };
      schema_ok
        && members
          .iter()
          .all(|(key, member)| plain_where_it_should_be(member, in_metadata || key == "metadata"))
    }
    Value::Array(items) => items
      .iter()
      .all(|item| plain_where_it_should_be(item, in_metadata)),
    _ => true,
  }
}

#[test]
fn a_note_keyed_as_otio_keys_its_objects_is_written_as_a_pair_and_reads_back() {
  // The keys OpenTimelineIO's reader looks for (`deserialization.cpp`
  // 561–573, 627–629 in 0.17.0), the crate's own word names, and an empty
  // key: none may become a member of an object.
  let notes = || {
    Metadata::new()
      .with("OTIO_SCHEMA", "Marker.2")
      .with("OTIO_REF_ID", "a reference")
      .with("metadata", "a note")
      .with("mediaio", "another")
      .with("", "an empty key")
  };
  let mut timeline = sample().with_metadata(notes());
  *timeline.tracks_mut()[0].clips_mut()[0].metadata_mut() = notes();
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    let written = to_otio(&timeline, target).unwrap();
    assert_eq!(validate_json(&written, target), Ok(()), "{target:?}");
    let root = json::parse(&written).unwrap();
    assert!(
      plain_where_it_should_be(&root, false),
      "{target:?}: a note keyed as a schema is an object: {written}"
    );
    assert_eq!(from_otio(&written), Ok(timeline.clone()), "{target:?}");
  }
  // The pairs, as written.
  let mut root = json::parse(&to_otio(&timeline, OtioTarget::V0_15Plus).unwrap()).unwrap();
  let pairs = at(&mut root, "metadata.mediaio.metadata");
  let Value::Array(pairs) = pairs else {
    panic!("the notes are an array: {pairs:?}")
  };
  assert!(pairs.contains(&Value::Array(vec![text("OTIO_SCHEMA"), text("Marker.2")])));
}

/// The sample written for `target`, edited, written out again and held to
/// the self-check: where it refuses, and what it expects there.
fn checked_edited(
  target: OtioTarget,
  edit: impl FnOnce(&mut Value),
) -> Result<(), (String, &'static str)> {
  let mut root = json::parse(&to_otio(&sample(), target).unwrap()).unwrap();
  edit(&mut root);
  let mut out = Utf8BytesMut::new();
  json::write_pretty(&root, &mut out);
  validate_json(&out.freeze_shared(), target).map_err(|invalid| match invalid {
    Invalid::Shape(shape) => (String::from(shape.path()), shape.expected()),
    other => panic!("{other:?}"),
  })
}

#[test]
fn the_self_check_decodes_every_dictionary_as_a_reader_does() {
  // A reader decodes every dictionary as its parser closes it, wherever it
  // sits, one carrying `OTIO_SCHEMA` as an object of that schema
  // (`deserialization.cpp` 561–573 in 0.17.0), so the check walks them all.
  let refused = |path: &str, expected: &'static str| Err((String::from(path), expected));
  let schema = |name: &str, mut members: Vec<(&str, Value)>| {
    members.insert(0, ("OTIO_SCHEMA", text(name)));
    object(members)
  };
  let rational = || {
    schema(
      "RationalTime.1",
      vec![("rate", number("24.0")), ("value", number("1.0"))],
    )
  };
  let studio = |value: Value| move |root: &mut Value| set(root, "metadata", "studio", value);
  let clip = "$.tracks.children[0].children[2]";
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    // Round 4's plant N1: the notes written as an object. One keyed
    // `OTIO_SCHEMA: Marker.2` is a marker to every reader, which refuses it
    // for the range it lacks.
    assert_eq!(
      checked_edited(target, |root| set(
        root,
        "metadata.mediaio",
        "metadata",
        schema("Marker.2", vec![])
      )),
      refused("$.metadata.mediaio.metadata.marked_range", "a required key"),
      "{target:?}",
    );
    // N1's document whole: its note keyed `metadata` is a string, which the
    // marker reads as its metadata.
    assert_eq!(
      checked_edited(target, |root| set(
        root,
        "metadata.mediaio",
        "metadata",
        object(vec![
          ("", text("an empty key")),
          ("OTIO_REF_ID", text("a reference")),
          ("OTIO_SCHEMA", text("Marker.2")),
          ("mediaio", text("another")),
          ("metadata", text("a note")),
        ])
      )),
      refused("$.metadata.mediaio.metadata.metadata", "an object"),
      "{target:?}",
    );
    // A plain dictionary passes however deep, and so does an object of a
    // schema the check knows, held to its rules.
    let plain = object(vec![
      (
        "crew",
        Value::Array(vec![
          object(vec![("name", text("x"))]),
          number("1"),
          Value::Null,
        ]),
      ),
      ("start", rational()),
    ]);
    assert_eq!(checked_edited(target, studio(plain)), Ok(()), "{target:?}");
    // A schema whose rules the check does not know is refused by name: one
    // no reader registers, which a reader keeps as an `UnknownSchema`; one
    // whose registered constructor is a fatal error (`typeRegistry.cpp`
    // 46–55 in 0.17.0); a version past the registered one, and a name
    // without a version, both of which a reader refuses too.
    for name in ["Studio.1", "UnknownSchema.1", "Marker.3", "Marker"] {
      assert_eq!(
        checked_edited(target, studio(schema(name, vec![]))),
        refused(
          "$.metadata.studio.OTIO_SCHEMA",
          "a schema whose rules the check knows"
        ),
        "{target:?} {name}",
      );
    }
    // Metadata is a dictionary, never a schema's object.
    assert_eq!(
      checked_edited(target, |root| set(
        root,
        "metadata",
        "OTIO_SCHEMA",
        text("Marker.2")
      )),
      refused(
        "$.metadata.OTIO_SCHEMA",
        "no schema: metadata is a dictionary"
      ),
      "{target:?}",
    );
    // In a clip's metadata, inside an array; and under a key no reader
    // reads, which a reader keeps as the clip's own.
    assert_eq!(
      checked_edited(target, |root| set(
        root,
        &format!("{A}.metadata"),
        "studio",
        Value::Array(vec![schema("TimeRange.1", vec![])])
      )),
      refused(
        &format!("{clip}.metadata.studio[0].duration"),
        "a required key"
      ),
      "{target:?}",
    );
    assert_eq!(
      checked_edited(target, |root| set(
        root,
        A,
        "studio",
        schema("Marker.2", vec![])
      )),
      refused(&format!("{clip}.studio.marked_range"), "a required key"),
      "{target:?}",
    );
    // An item holds its effects as `Effect`s, each with the keys its reader
    // requires.
    let effects =
      |entry: Value| move |root: &mut Value| set(root, A, "effects", Value::Array(vec![entry]));
    assert_eq!(
      checked_edited(target, effects(schema("Effect.1", vec![]))),
      refused(&format!("{clip}.effects[0].effect_name"), "a required key"),
      "{target:?}",
    );
    assert_eq!(
      checked_edited(
        target,
        effects(schema("LinearTimeWarp.1", vec![("effect_name", text("w"))]))
      ),
      refused(&format!("{clip}.effects[0].time_scalar"), "a required key"),
      "{target:?}",
    );
    assert_eq!(
      checked_edited(target, effects(schema("Marker.2", vec![]))),
      refused(
        &format!("{clip}.effects[0].OTIO_SCHEMA"),
        "an effect schema"
      ),
      "{target:?}",
    );
    let frozen = schema(
      "FreezeFrame.1",
      vec![
        ("effect_name", text("f")),
        ("time_scalar", number("0.0")),
        ("metadata", object(vec![("at", rational())])),
      ],
    );
    assert_eq!(
      checked_edited(target, effects(frozen)),
      Ok(()),
      "{target:?}"
    );
    // The value schemas a reader decodes, each with every key it fetches.
    let red = || {
      schema(
        "Color.1",
        vec![
          ("name", text("red")),
          ("r", number("1.0")),
          ("g", number("0")),
          ("b", number("0")),
          ("a", number("1")),
        ],
      )
    };
    let point = |keys: &[&str]| {
      schema(
        "V2d.1",
        keys.iter().map(|key| (*key, number("0.5"))).collect(),
      )
    };
    let warp = schema(
      "TimeTransform.1",
      vec![
        ("offset", rational()),
        ("rate", number("24.0")),
        ("scale", number("1.0")),
      ],
    );
    for value in [red(), point(&["x", "y"]), warp] {
      assert_eq!(checked_edited(target, studio(value)), Ok(()), "{target:?}");
    }
    let mut unlit = red();
    remove(&mut unlit, "", "a");
    assert_eq!(
      checked_edited(target, studio(unlit)),
      refused("$.metadata.studio.a", "a required key"),
      "{target:?}",
    );
    assert_eq!(
      checked_edited(target, studio(point(&["x"]))),
      refused("$.metadata.studio.y", "a required key"),
      "{target:?}",
    );
  }
  // From 0.15 a reader reads a media reference's image bounds as a
  // `Box2d`; before, as a key it does not know.
  let bounds =
    |value: Value| move |root: &mut Value| set(root, A_MEDIA, "available_image_bounds", value);
  assert_eq!(
    checked_edited(OtioTarget::V0_15Plus, bounds(object(vec![]))),
    refused(
      &format!("{clip}.media_references.DEFAULT_MEDIA.available_image_bounds.OTIO_SCHEMA"),
      "a schema name"
    ),
  );
  let corner = || schema("V2d.1", vec![("x", number("0")), ("y", number("0"))]);
  let framed = schema("Box2d.1", vec![("min", corner()), ("max", corner())]);
  assert_eq!(
    checked_edited(OtioTarget::V0_15Plus, bounds(framed)),
    Ok(())
  );
  assert_eq!(
    checked_edited(OtioTarget::Legacy, |root| set(
      root,
      &format!("{A}.media_reference"),
      "available_image_bounds",
      object(vec![])
    )),
    Ok(()),
  );
}

/// Where the sample's clip `a` keeps its medium, for `target`.
fn a_media(target: OtioTarget) -> String {
  match target {
    OtioTarget::V0_15Plus => String::from(A_MEDIA),
    OtioTarget::Legacy => format!("{A}.media_reference"),
  }
}

/// The checker's path for the dotted `path`: `$.`, and each index in
/// brackets.
fn checked_path(path: &str) -> String {
  let mut out = String::from("$");
  for step in path.split('.').filter(|step| !step.is_empty()) {
    if step.bytes().all(|byte| byte.is_ascii_digit()) {
      out.push_str(&format!("[{step}]"));
    } else {
      out.push('.');
      out.push_str(step);
    }
  }
  out
}

#[test]
fn a_dictionary_of_media_references_carrying_otio_schema_is_refused_by_name() {
  // Round 5: a reader decodes `media_references` as it closes it, and a
  // member named `OTIO_SCHEMA` makes it an object of a schema, its
  // `OTIO_SCHEMA` fetched as a string — here an object, which it refuses.
  let external = object(vec![
    ("OTIO_SCHEMA", text("ExternalReference.1")),
    ("target_url", text("file:///other.mov")),
  ]);
  assert_eq!(
    checked_edited(OtioTarget::V0_15Plus, |root| set(
      root,
      &format!("{A}.media_references"),
      "OTIO_SCHEMA",
      external
    )),
    Err((
      checked_path(&format!("{A}.media_references.OTIO_SCHEMA")),
      "no schema: media_references is a dictionary"
    )),
  );
}

#[test]
fn a_generator_reference_is_held_to_its_readers_members() {
  // `generatorReference.cpp` 26–27 in 0.17.0: `generator_kind`, a string,
  // and `parameters`, a dictionary, both required.
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    let media = a_media(target);
    let at = |key: &str| checked_path(&format!("{media}.{key}"));
    let generator = |members: Vec<(&str, Value)>| {
      let mut members = members;
      members.insert(0, ("OTIO_SCHEMA", text("GeneratorReference.1")));
      object(members)
    };
    let refused = |reference: Value| {
      checked_edited(target, |root| {
        let (parent, key) = media.rsplit_once('.').expect("a dotted path");
        set(root, parent, key, reference);
      })
    };
    // Round 5's counterexample: a generator on a reference's members alone.
    assert_eq!(
      refused(generator(vec![])),
      Err((at("generator_kind"), "a required key")),
      "{target:?}",
    );
    assert_eq!(
      refused(generator(vec![("generator_kind", text("solid"))])),
      Err((at("parameters"), "a required key")),
      "{target:?}",
    );
    assert_eq!(
      refused(generator(vec![
        ("generator_kind", number("1")),
        ("parameters", object(vec![])),
      ])),
      Err((at("generator_kind"), "a string")),
      "{target:?}",
    );
    assert_eq!(
      refused(generator(vec![
        ("generator_kind", text("solid")),
        ("parameters", text("red")),
      ])),
      Err((at("parameters"), "an object")),
      "{target:?}",
    );
    assert_eq!(
      refused(generator(vec![
        ("generator_kind", text("solid")),
        (
          "parameters",
          object(vec![("OTIO_SCHEMA", text("Marker.2"))]),
        ),
      ])),
      Err((
        at("parameters.OTIO_SCHEMA"),
        "no schema: parameters is a dictionary"
      )),
      "{target:?}",
    );
    // Its parameters are walked as any dictionary is.
    assert_eq!(
      refused(generator(vec![
        ("generator_kind", text("solid")),
        (
          "parameters",
          object(vec![(
            "fill",
            object(vec![("OTIO_SCHEMA", text("Marker.2"))]),
          )]),
        ),
      ])),
      Err((at("parameters.fill.marked_range"), "a required key")),
      "{target:?}",
    );
    assert_eq!(
      refused(generator(vec![
        ("generator_kind", text("solid")),
        ("parameters", object(vec![("color", text("red"))])),
      ])),
      Ok(()),
      "{target:?}",
    );
  }
}

#[test]
fn an_image_sequence_reference_is_held_to_its_readers_members() {
  // `imageSequenceReference.cpp` 195–233 in 0.17.0: three strings, three
  // `int64_t`s, a `double` and a policy of three, each required.
  let sequence = || {
    vec![
      ("OTIO_SCHEMA", text("ImageSequenceReference.1")),
      ("target_url_base", text("file:///frames/")),
      ("name_prefix", text("a.")),
      ("name_suffix", text(".exr")),
      ("start_frame", number("1001")),
      ("frame_step", number("1")),
      ("rate", number("24")),
      ("frame_zero_padding", number("4")),
      ("missing_frame_policy", text("hold")),
    ]
  };
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    let media = a_media(target);
    let at = |key: &str| checked_path(&format!("{media}.{key}"));
    let checked = |members: Vec<(&str, Value)>| {
      checked_edited(target, |root| {
        let (parent, key) = media.rsplit_once('.').expect("a dotted path");
        set(root, parent, key, object(members));
      })
    };
    assert_eq!(checked(sequence()), Ok(()), "{target:?}");
    // Each required member, left out.
    for (index, (key, _)) in sequence().into_iter().enumerate().skip(1) {
      let mut members = sequence();
      members.remove(index);
      assert_eq!(
        checked(members),
        Err((at(key), "a required key")),
        "{target:?} {key}",
      );
    }
    // Each held to its type.
    let with = |key: &str, value: Value| {
      let mut members = sequence();
      for member in &mut members {
        if member.0 == key {
          member.1 = value.clone();
        }
      }
      checked(members)
    };
    let whole = "a whole number, without a fraction or an exponent";
    for spelling in ["1001.0", "1.001e3", "-0"] {
      assert_eq!(
        with("start_frame", number(spelling)),
        Err((at("start_frame"), whole)),
        "{target:?} {spelling}",
      );
    }
    assert_eq!(
      with("frame_step", text("1")),
      Err((at("frame_step"), whole)),
      "{target:?}",
    );
    assert_eq!(
      with("frame_zero_padding", number("4.5")),
      Err((at("frame_zero_padding"), whole)),
      "{target:?}",
    );
    assert_eq!(with("rate", number("23.976")), Ok(()), "{target:?}");
    assert_eq!(
      with("rate", text("24")),
      Err((at("rate"), "a finite number")),
      "{target:?}",
    );
    assert_eq!(
      with("name_prefix", number("5")),
      Err((at("name_prefix"), "a string")),
      "{target:?}",
    );
    for policy in ["error", "black", "hold"] {
      assert_eq!(
        with("missing_frame_policy", text(policy)),
        Ok(()),
        "{target:?} {policy}"
      );
    }
    assert_eq!(
      with("missing_frame_policy", text("skip")),
      Err((at("missing_frame_policy"), "error, black or hold")),
      "{target:?}",
    );
  }
}

#[test]
fn a_missing_reference_reads_a_references_members_alone() {
  // `missingReference.cpp` 28 in 0.17.0: nothing of its own.
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    let media = a_media(target);
    let checked = |members: Vec<(&str, Value)>| {
      checked_edited(target, |root| {
        let (parent, key) = media.rsplit_once('.').expect("a dotted path");
        set(root, parent, key, object(members));
      })
    };
    assert_eq!(
      checked(vec![("OTIO_SCHEMA", text("MissingReference.1"))]),
      Ok(()),
      "{target:?}",
    );
    assert_eq!(
      checked(vec![
        ("OTIO_SCHEMA", text("MissingReference.1")),
        ("name", number("5")),
      ]),
      Err((checked_path(&format!("{media}.name")), "a string")),
      "{target:?}",
    );
  }
}

#[test]
fn notes_that_are_not_pairs_of_strings_each_key_once_are_refused_by_name() {
  let notes = |root: &mut Value, value: Value| {
    set(root, "metadata.mediaio", "metadata", value);
  };
  let pair = |key: &str, value: &str| Value::Array(vec![text(key), text(value)]);
  assert_eq!(
    read(|root| notes(root, object(vec![("project", text("law"))]))),
    Err(shape("metadata.mediaio.metadata", "an array")),
  );
  assert_eq!(
    read(|root| notes(
      root,
      Value::Array(vec![Value::Array(vec![text("only a key")])])
    )),
    Err(shape(
      "metadata.mediaio.metadata.0",
      "a note: a key and a value, both strings"
    )),
  );
  assert_eq!(
    read(|root| {
      notes(
        root,
        Value::Array(vec![Value::Array(vec![text("key"), number("1")])]),
      )
    }),
    Err(shape(
      "metadata.mediaio.metadata.0",
      "a note: a key and a value, both strings"
    )),
  );
  assert_eq!(
    read(|root| notes(root, Value::Array(vec![pair("key", "a"), pair("key", "b")]))),
    Err(shape(
      "metadata.mediaio.metadata.1.0",
      "a key no earlier note names"
    )),
  );
  assert_eq!(
    read(|root| notes(root, Value::Array(vec![pair("key", "a")]))),
    Ok(sample().with_metadata(Metadata::new().with("key", "a"))),
  );
}

/// `num/den`, as a test spells a timebase.
fn timebase(num: i32, den: i32) -> Timebase {
  Timebase::new(num, core::num::NonZeroI32::new(den).expect("non-zero"))
}

/// The sample at a rate of `48/2` — 24 a second, spelled unreduced — whose
/// edit-side values spell its reciprocal `2/48` otherwise: the start in
/// `1/24`, `a`'s record in `4/96` and its fade in `3/72`, the dissolve's cut
/// in `1/24`, its offsets in `2/48` (the reciprocal's own spelling) and
/// `5/120`, and the track's marker in `1/24`.
fn respelled() -> Timeline {
  let rate = Rate::fps(48, core::num::NonZeroI32::new(2).expect("non-zero"));
  let mut timeline = sample();
  timeline.set_rate(rate);
  timeline.set_start(Timestamp::new(0, timebase(1, 24)));
  let track = &mut timeline.tracks_mut()[0];
  let a = &mut track.clips_mut()[0];
  a.set_record(TimeRange::new(12, 36, timebase(4, 96)));
  let fades = a.fades().with_in(Some(Fade::new(
    Duration::new(6, timebase(3, 72)),
    FadeShape::Linear,
  )));
  a.set_fades(fades);
  track.clips_mut()[1].set_record(TimeRange::new(36, 60, timebase(2, 48)));
  *track.transitions_mut() = vec![Transition::dissolve(
    Timestamp::new(36, timebase(1, 24)),
    Duration::new(2, timebase(2, 48)),
    Duration::new(2, timebase(5, 120)),
  )];
  track.markers_mut()[0].set_range(TimeRange::new(0, 24, timebase(1, 24)));
  timeline
}

/// A timebase's spelling, as `(num, den)`.
fn spelling(timebase: Timebase) -> (i32, i32) {
  (timebase.num(), timebase.den().get())
}

#[test]
fn an_edit_side_value_keeps_its_timebases_spelling() {
  let timeline = respelled();
  assert_eq!(
    crate::validate(&timeline),
    Ok(()),
    "the premise: it validates"
  );
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    let written = to_otio(&timeline, target).unwrap();
    let read = from_otio(&written).unwrap();
    assert_eq!(read, timeline, "{target:?}");
    assert_eq!(spelling(read.start().timebase()), (1, 24), "the start");
    let track = &read.tracks()[0];
    let a = &track.clips()[0];
    assert_eq!(spelling(a.record().timebase()), (4, 96), "a's record");
    assert_eq!(
      spelling(a.fades().in_().expect("a's fade").duration().timebase()),
      (3, 72),
      "a's fade"
    );
    assert_eq!(
      spelling(track.clips()[1].record().timebase()),
      (2, 48),
      "b's record"
    );
    let dissolve = &track.transitions()[0];
    assert_eq!(spelling(dissolve.at().timebase()), (1, 24), "the cut");
    assert_eq!(spelling(dissolve.in_offset().timebase()), (2, 48), "in");
    assert_eq!(spelling(dissolve.out_offset().timebase()), (5, 120), "out");
    assert_eq!(
      spelling(track.markers()[0].range().timebase()),
      (1, 24),
      "the marker"
    );
    #[cfg(feature = "serde")]
    assert_eq!(
      serde_json::to_string(&read).unwrap(),
      serde_json::to_string(&timeline).unwrap(),
      "{target:?}: the stored document, byte for byte",
    );
  }
  // The words are written only where a spelling differs from the
  // reciprocal's.
  let mut root = json::parse(&to_otio(&timeline, OtioTarget::V0_15Plus).unwrap()).unwrap();
  assert_eq!(at(&mut root, "metadata.mediaio.start"), &text("1/24"));
  assert_eq!(
    at(&mut root, &format!("{A}.metadata.mediaio.record")),
    &text("4/96")
  );
  assert_eq!(
    at(&mut root, &format!("{FADE_IN}.metadata.mediaio.timebase")),
    &text("3/72")
  );
  let words = at(&mut root, &format!("{DISSOLVE}.metadata.mediaio"));
  let Value::Object(words) = words else {
    panic!("the dissolve's words: {words:?}")
  };
  let keys: Vec<&str> = words.iter().map(|(key, _)| key.as_str()).collect();
  assert_eq!(
    keys,
    ["at", "out_offset"],
    "in_offset spells the reciprocal"
  );
  let b_words = at(&mut root, &format!("{B}.metadata.mediaio"));
  let Value::Object(b_words) = b_words else {
    panic!("b's words: {b_words:?}")
  };
  assert!(
    b_words.iter().all(|(key, _)| key != "record"),
    "b's record spells the reciprocal"
  );
  assert_eq!(
    at(
      &mut root,
      &format!("{TRACK}.markers.0.metadata.mediaio.timebase")
    ),
    &text("1/24")
  );
}

#[test]
fn a_timebase_word_off_the_edit_rate_is_refused_by_name() {
  assert_eq!(
    read(|root| set(root, "metadata.mediaio", "start", text("1/25"))),
    Err(shape(
      "metadata.mediaio.start",
      "the edit rate's timebase, written num/den"
    )),
  );
  assert_eq!(
    read(|root| set(root, &format!("{A}.metadata.mediaio"), "record", text("x"))),
    Err(shape(
      &format!("{A}.metadata.mediaio.record"),
      "the edit rate's timebase, written num/den"
    )),
  );
  // An equal timebase spelled otherwise reads, in its own spelling.
  let equal = read(|root| {
    set(
      root,
      &format!("{A}.metadata.mediaio"),
      "record",
      text("2/48"),
    )
  })
  .expect("an equal timebase");
  assert_eq!(
    spelling(equal.tracks()[0].clips()[0].record().timebase()),
    (2, 48)
  );
  // A word of the other kind of transition.
  assert_eq!(
    read_edited(OtioTarget::V0_15Plus, |root| {
      set(
        &mut *root,
        &format!("{DISSOLVE}.metadata"),
        "mediaio",
        object(vec![]),
      );
      set(
        root,
        &format!("{DISSOLVE}.metadata.mediaio"),
        "timebase",
        text("1/24"),
      );
    }),
    Err(feature(
      Feature::Word("timebase".into()),
      &format!("{DISSOLVE}.metadata.mediaio.timebase")
    )),
  );
  assert_eq!(
    read(|root| set(
      root,
      &format!("{FADE_IN}.metadata.mediaio"),
      "at",
      text("1/24")
    )),
    Err(feature(
      Feature::Word("at".into()),
      &format!("{FADE_IN}.metadata.mediaio.at")
    )),
  );
}

// Members OpenTimelineIO 0.17.0's schema lets a document leave out read as
// its reader reads them: it builds each object with its default
// constructor (`typeRegistry.h` 57–65) and reads such a member only if
// present. One law per member.

#[test]
fn an_absent_name_reads_as_empty() {
  // `serializableObjectWithMetadata.cpp` 22: an empty name.
  let (track_marker, clip_marker) = (format!("{TRACK}.markers.0"), format!("{A}.markers.0"));
  let read = read(|root| {
    for path in [
      "",
      "tracks",
      TRACK,
      GAP,
      FADE_IN,
      A,
      DISSOLVE,
      B,
      A_MEDIA,
      track_marker.as_str(),
      clip_marker.as_str(),
    ] {
      remove(root, path, "name");
    }
  });
  let mut nameless = sample();
  nameless.set_name("");
  let track = &mut nameless.tracks_mut()[0];
  track.set_name("");
  track.markers_mut()[0].set_name("");
  for clip in track.clips_mut() {
    clip.set_name("");
  }
  track.clips_mut()[0].markers_mut()[0].set_name("");
  assert_eq!(read, Ok(nameless));
}

#[test]
fn absent_metadata_reads_as_no_note_and_a_needed_word_is_refused_by_name() {
  // `serializableObjectWithMetadata.cpp` 21: an empty dictionary — where
  // the timeline needs none of its words, nothing.
  let track_marker = format!("{TRACK}.markers.0");
  assert_eq!(
    read(|root| {
      for path in ["tracks", TRACK, GAP, DISSOLVE, track_marker.as_str()] {
        remove(root, path, "metadata");
      }
    }),
    Ok(sample()),
  );
  // Where it needs one, the word is refused by name.
  assert_eq!(
    read(|root| remove(root, "", "metadata")),
    Err(shape("metadata.mediaio.rate", "a required word")),
  );
  assert_eq!(
    read(|root| remove(root, A, "metadata")),
    Err(shape(
      &format!("{A}.metadata.mediaio.id"),
      "a required word"
    )),
  );
}

#[test]
fn an_items_absent_source_range_is_no_trim_a_gap_of_no_length_or_refused_for_a_clip() {
  // `item.cpp` 175: none — no trim, on the stack and a track.
  assert_eq!(
    read(|root| {
      remove(root, "tracks", "source_range");
      remove(root, TRACK, "source_range");
    }),
    Ok(sample()),
  );
  // A gap's is its constructor's `TimeRange()` (`gap.h` 23): no length.
  // The export writes one after a fade-out at the track's end.
  let mut faded = sample();
  let b = &mut faded.tracks_mut()[0].clips_mut()[1];
  let fades = b
    .fades()
    .with_out(Some(Fade::new(Duration::new(4, edit()), FadeShape::Linear)));
  b.set_fades(fades);
  let mut root = json::parse(&to_otio(&faded, OtioTarget::V0_15Plus).unwrap()).unwrap();
  remove(&mut root, &format!("{TRACK}.children.6"), "source_range");
  let mut out = Utf8BytesMut::new();
  json::write_pretty(&root, &mut out);
  assert_eq!(from_otio(&out.freeze_shared()), Ok(faded));
  // A clip's is none, which OpenTimelineIO plays as its medium's whole
  // available range — a range the document's words do not count.
  assert_eq!(
    read(|root| remove(root, A, "source_range")),
    Err(shape(&format!("{A}.source_range"), "a required key")),
  );
}

#[test]
fn absent_effects_and_markers_read_as_none() {
  // `item.cpp` 176–177: none.
  assert_eq!(
    read(|root| {
      for path in ["tracks", TRACK, GAP, A, B] {
        remove(root, path, "effects");
      }
      for path in ["tracks", GAP, B] {
        remove(root, path, "markers");
      }
    }),
    Ok(sample()),
  );
}

#[test]
fn an_items_absent_enabled_reads_as_enabled() {
  // `item.cpp` 178; `item.h` 33: `true`.
  assert_eq!(
    read(|root| {
      for path in ["tracks", TRACK, GAP, A, B] {
        remove(root, path, "enabled");
      }
    }),
    Ok(sample()),
  );
}

#[test]
fn a_markers_absent_color_reads_and_writes_back_as_green() {
  // `marker.cpp` 27 reads the color only if present, over the default
  // constructor's `Color::green` (`marker.h` 37–42). Python's binding
  // defaults its own constructor to RED, which no reader runs.
  let read = read(|root| {
    remove(root, &format!("{TRACK}.markers.0"), "color");
    remove(root, &format!("{A}.markers.0"), "color");
  })
  .unwrap();
  let track = &read.tracks()[0];
  assert_eq!(track.markers()[0].color(), MarkerColor::Green);
  assert_eq!(track.clips()[0].markers()[0].color(), MarkerColor::Green);
  let mut root = json::parse(&to_otio(&read, OtioTarget::V0_15Plus).unwrap()).unwrap();
  assert_eq!(
    at(&mut root, &format!("{TRACK}.markers.0.color")),
    &text("GREEN"),
  );
}

#[test]
fn a_markers_absent_comment_reads_as_empty() {
  // `marker.cpp` 29; `marker.h` 42: empty.
  let mut commented = sample();
  commented.tracks_mut()[0].markers_mut()[0].set_comment("a note");
  let mut root = json::parse(&to_otio(&commented, OtioTarget::V0_15Plus).unwrap()).unwrap();
  remove(&mut root, &format!("{TRACK}.markers.0"), "comment");
  let mut out = Utf8BytesMut::new();
  json::write_pretty(&root, &mut out);
  assert_eq!(from_otio(&out.freeze_shared()), Ok(sample()));
}

#[test]
fn a_references_absent_ranges_read_as_none() {
  // `mediaReference.cpp` 30–33: none — the available range, its word
  // with it, and the image bounds.
  let read = read(|root| {
    remove(root, A_MEDIA, "available_range");
    remove(root, &format!("{A_MEDIA}.metadata.mediaio"), "available");
    remove(root, A_MEDIA, "available_image_bounds");
  })
  .unwrap();
  let media = read.tracks()[0].clips()[0].media();
  assert_eq!(media.available_range(), None);
  let mut expected = sample();
  let a = &mut expected.tracks_mut()[0].clips_mut()[0];
  let medium = a.media().clone().with_available_range(None);
  a.set_media(medium);
  assert_eq!(read, expected);
}

#[test]
fn an_absent_global_start_time_is_refused_by_name() {
  // `timeline.cpp` 31; `timeline.h` 28: none — no time, where the
  // timeline always starts somewhere.
  assert_eq!(
    read(|root| remove(root, "", "global_start_time")),
    Err(shape("global_start_time", "a required key")),
  );
}

#[test]
fn a_clip_1_without_a_media_reference_is_a_clip_with_no_medium() {
  // OpenTimelineIO upgrades it to a `Clip.2` holding a `MissingReference`
  // (`typeRegistry.cpp` 99–117), as it does a `null` one.
  assert_eq!(
    read_edited(OtioTarget::Legacy, |root| remove(
      root,
      A,
      "media_reference"
    )),
    Err(feature(Feature::NoMedia, &format!("{A}.media_reference"))),
  );
}

#[test]
fn a_member_opentimelineio_requires_is_refused_by_name() {
  // What 0.17.0 reads with `reader.read` and so refuses a document
  // without: a track's kind (`track.cpp` 34), a transition's offsets and
  // type (`transition.cpp` 33–35), a marker's range (`marker.cpp` 28), a
  // stack's and a track's children (`composition.cpp` 191), a reference's
  // URL (`externalReference.cpp` 23), a `Clip.2`'s references
  // (`clip.cpp` 141–144), a time's rate and value and a range's start
  // and duration (`deserialization.cpp` 578, 585–586).
  let cases: Vec<(String, &str)> = vec![
    (TRACK.into(), "kind"),
    (DISSOLVE.into(), "in_offset"),
    (DISSOLVE.into(), "out_offset"),
    (DISSOLVE.into(), "transition_type"),
    (format!("{TRACK}.markers.0"), "marked_range"),
    ("tracks".into(), "children"),
    (TRACK.into(), "children"),
    (A_MEDIA.into(), "target_url"),
    (A.into(), "media_references"),
    (A.into(), "active_media_reference_key"),
    (format!("{GAP}.source_range.duration"), "rate"),
    (format!("{GAP}.source_range.duration"), "value"),
    (format!("{GAP}.source_range"), "start_time"),
    (format!("{GAP}.source_range"), "duration"),
  ];
  for (path, key) in cases {
    assert_eq!(
      read(|root| remove(root, &path, key)),
      Err(shape(&format!("{path}.{key}"), "a required key")),
      "{path}.{key}",
    );
  }
}

#[test]
fn a_schema_the_timeline_does_not_model_is_refused_with_where_it_sits() {
  let effect = |name: &str| object(vec![("OTIO_SCHEMA", text(name))]);
  for name in ["Effect.1", "LinearTimeWarp.1", "FreezeFrame.1"] {
    assert_eq!(
      read(|root| set(root, A, "effects", Value::Array(vec![effect(name)]))),
      Err(schema(name, &format!("{A}.effects.0"))),
    );
  }
  // A stack inside a track, a clip directly in the stack.
  let nested = object(vec![
    ("OTIO_SCHEMA", text("Stack.1")),
    ("children", Value::Array(vec![])),
  ]);
  assert_eq!(
    read(|root| {
      let Value::Array(children) = at(root, &format!("{TRACK}.children")) else {
        unreachable!()
      };
      children.push(nested);
    }),
    Err(schema("Stack.1", &format!("{TRACK}.children.5"))),
  );
  assert_eq!(
    read(|root| {
      let clip = at(root, A).clone();
      let Value::Array(tracks) = at(root, "tracks.children") else {
        unreachable!()
      };
      tracks.push(clip);
    }),
    Err(schema("Clip.2", "tracks.children.1")),
  );
  // A reference other than an external one, a marker of another version,
  // a marker where the timeline keeps none, and a root that is no timeline.
  assert_eq!(
    read(|root| set(
      root,
      &format!("{A}.media_references"),
      "DEFAULT_MEDIA",
      effect("MissingReference.1")
    )),
    Err(schema("MissingReference.1", A_MEDIA)),
  );
  assert_eq!(
    read(|root| set(
      root,
      &format!("{A}.markers.0"),
      "OTIO_SCHEMA",
      text("Marker.3")
    )),
    Err(schema("Marker.3", &format!("{A}.markers.0"))),
  );
  assert_eq!(
    read(|root| {
      let marker = at(root, &format!("{TRACK}.markers.0")).clone();
      set(root, GAP, "markers", Value::Array(vec![marker]));
    }),
    Err(schema("Marker.2", &format!("{GAP}.markers.0"))),
  );
  assert_eq!(
    read(|root| set(root, "", "OTIO_SCHEMA", text("SerializableCollection.1"))),
    Err(schema("SerializableCollection.1", "")),
  );
}

/// An edit of a written document, for a law that tries many.
type Edit = Box<dyn FnOnce(&mut Value)>;

#[test]
fn a_feature_the_timeline_has_no_word_for_is_refused_by_name() {
  let cases: Vec<(Edit, Unreadable)> = vec![
    (
      Box::new(|root| set(root, A, "speed", number("2.0"))),
      feature(Feature::Key("speed".into()), &format!("{A}.speed")),
    ),
    (
      Box::new(|root| {
        set(
          root,
          &format!("{A}.metadata"),
          "Resolve_OTIO",
          object(vec![]),
        )
      }),
      feature(
        Feature::Metadata("Resolve_OTIO".into()),
        &format!("{A}.metadata.Resolve_OTIO"),
      ),
    ),
    (
      Box::new(|root| set(root, &format!("{A}.metadata.mediaio"), "speed", number("2"))),
      feature(
        Feature::Word("speed".into()),
        &format!("{A}.metadata.mediaio.speed"),
      ),
    ),
    (
      Box::new(|root| set(root, GAP, "name", text("black"))),
      feature(Feature::Name("black".into()), &format!("{GAP}.name")),
    ),
    (
      Box::new(|root| set(root, "tracks", "name", text("Video Tracks"))),
      feature(Feature::Name("Video Tracks".into()), "tracks.name"),
    ),
    (
      Box::new(|root| set(root, TRACK, "kind", text("Data"))),
      feature(Feature::TrackKind("Data".into()), &format!("{TRACK}.kind")),
    ),
    (
      Box::new(|root| set(root, DISSOLVE, "transition_type", text("SMPTE_Wipe"))),
      feature(
        Feature::TransitionType("SMPTE_Wipe".into()),
        &format!("{DISSOLVE}.transition_type"),
      ),
    ),
    (
      Box::new(|root| {
        let range = at(root, &format!("{GAP}.source_range")).clone();
        set(root, TRACK, "source_range", range);
      }),
      feature(Feature::Trim, &format!("{TRACK}.source_range")),
    ),
    (
      Box::new(|root| set(root, GAP, "enabled", Value::Bool(false))),
      feature(Feature::Disabled, &format!("{GAP}.enabled")),
    ),
    (
      Box::new(|root| set(root, DISSOLVE, "enabled", Value::Bool(false))),
      feature(Feature::Disabled, &format!("{DISSOLVE}.enabled")),
    ),
    (
      Box::new(|root| set(root, A, "color", text("RED"))),
      feature(Feature::ItemColor, &format!("{A}.color")),
    ),
    (
      Box::new(|root| set(root, &format!("{TRACK}.markers.0"), "color", text("LILAC"))),
      feature(
        Feature::MarkerColor("LILAC".into()),
        &format!("{TRACK}.markers.0.color"),
      ),
    ),
    (
      Box::new(|root| {
        set(
          root,
          A_MEDIA,
          "available_image_bounds",
          object(vec![("OTIO_SCHEMA", text("Box2d.1"))]),
        )
      }),
      feature(
        Feature::ImageBounds,
        &format!("{A_MEDIA}.available_image_bounds"),
      ),
    ),
    (
      Box::new(|root| {
        let reference = at(root, A_MEDIA).clone();
        set(
          root,
          &format!("{A}.media_references"),
          "high_res",
          reference,
        );
      }),
      feature(Feature::MediaReferences, &format!("{A}.media_references")),
    ),
    (
      Box::new(|root| {
        set(
          root,
          &format!("{GAP}.source_range.start_time"),
          "value",
          number("5.0"),
        )
      }),
      feature(Feature::GapStart, &format!("{GAP}.source_range.start_time")),
    ),
    (
      // The fade's words on the dissolve: a fade between two clips.
      Box::new(|root| {
        let words = at(root, &format!("{FADE_IN}.metadata")).clone();
        set(root, DISSOLVE, "metadata", words);
      }),
      feature(Feature::Fade, DISSOLVE),
    ),
    (
      // The fade's words taken off: a dissolve from the gap.
      Box::new(|root| set(root, FADE_IN, "metadata", object(vec![]))),
      feature(Feature::DissolveBesideGap, FADE_IN),
    ),
  ];
  for (index, (edit, refused)) in cases.into_iter().enumerate() {
    assert_eq!(read(edit), Err(refused), "case {index}");
  }
  // A `Clip.1` with no reference at all.
  assert_eq!(
    read_edited(OtioTarget::Legacy, |root| set(
      root,
      A,
      "media_reference",
      Value::Null
    )),
    Err(feature(Feature::NoMedia, &format!("{A}.media_reference"))),
  );
}

#[test]
fn a_gap_of_some_length_after_the_last_clip_is_refused() {
  // The sample's leading gap, `length` frames long.
  let gap = |root: &mut Value, length: &str| {
    let mut gap = at(root, GAP).clone();
    set(&mut gap, "source_range.duration", "value", number(length));
    gap
  };
  let push = |root: &mut Value, dotted: &str, child: Value| {
    let Value::Array(children) = at(root, dotted) else {
      unreachable!()
    };
    children.push(child);
  };
  // Ten frames after `b`, the track's last clip, which OpenTimelineIO
  // counts in the track's duration.
  assert_eq!(
    read(|root| {
      let trailing = gap(root, "10.0");
      push(root, &format!("{TRACK}.children"), trailing);
    }),
    Err(feature(
      Feature::TrailingGap,
      &format!("{TRACK}.children.5")
    )),
  );
  // A track of one gap, ten frames long, where the timeline's track would
  // hold no time.
  assert_eq!(
    read(|root| {
      let mut track = at(root, TRACK).clone();
      let only = gap(root, "10.0");
      set(&mut track, "", "children", Value::Array(vec![only]));
      set(&mut track, "", "markers", Value::Array(vec![]));
      push(root, "tracks.children", track);
    }),
    Err(feature(
      Feature::TrailingGap,
      "tracks.children.1.children.0"
    )),
  );
  // A gap of no length there carries nothing.
  assert_eq!(
    read(|root| {
      let empty = gap(root, "0.0");
      push(root, &format!("{TRACK}.children"), empty);
    }),
    Ok(sample()),
  );
  // The export writes one after a fade-out at a track's end, and reads it
  // back.
  let mut faded = sample();
  let b = &mut faded.tracks_mut()[0].clips_mut()[1];
  let fades = b.fades().with_out(Some(Fade::new(
    Duration::new(4, edit()),
    FadeShape::EqualPower,
  )));
  b.set_fades(fades);
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    let written = to_otio(&faded, target).unwrap();
    let mut root = json::parse(&written).unwrap();
    // `[gap, fade in, a, dissolve, b, fade out, gap]`.
    let last = format!("{TRACK}.children.6");
    assert_eq!(
      at(&mut root, &format!("{last}.OTIO_SCHEMA")),
      &text("Gap.1"),
      "{target:?}"
    );
    assert_eq!(
      at(&mut root, &format!("{last}.source_range.duration.value")),
      &number("0.0"),
      "{target:?}"
    );
    assert_eq!(from_otio(&written), Ok(faded.clone()), "{target:?}");
  }
}

#[test]
fn a_count_or_rate_that_does_not_land_exactly_is_refused() {
  assert_eq!(
    read(|root| set(
      root,
      "global_start_time",
      "rate",
      number("24.000000000000004")
    )),
    Err(inexact(
      "global_start_time.rate",
      "a rate other than the f64 of the exact ruler it is counted in"
    )),
  );
  assert_eq!(
    read(|root| set(
      root,
      &format!("{GAP}.source_range.duration"),
      "value",
      number("12.5")
    )),
    Err(inexact(
      &format!("{GAP}.source_range.duration.value"),
      "a count that is no whole number"
    )),
  );
  assert_eq!(
    read(|root| set(
      root,
      &format!("{A}.source_range.start_time"),
      "value",
      number("9007199254740994.0")
    )),
    Err(inexact(
      &format!("{A}.source_range.start_time.value"),
      "a count past 2^53, which an f64 does not hold exactly"
    )),
  );
  // 47 ticks of 1/48 s: half an edit-rate tick short of 24.
  assert_eq!(
    read(|root| {
      set(
        root,
        &format!("{A}.metadata.mediaio"),
        "source",
        object(vec![("ruler", text("48/1")), ("timebase", text("1/48"))]),
      );
      for (key, value) in [("start_time", "20.0"), ("duration", "47.0")] {
        let time = format!("{A}.source_range.{key}");
        set(root, &time, "rate", number("48.0"));
        set(root, &time, "value", number(value));
      }
    }),
    Err(inexact(
      &format!("{A}.source_range"),
      "a source range that is no whole number of edit-rate ticks"
    )),
  );
}

/// 2^53: past it an `f64` holds only some whole counts.
const LIMIT: i64 = 1 << 53;

/// `value` as the export writes a count.
fn count(value: i64) -> Value {
  number(&format!("{value}.0"))
}

/// Sets the start and the duration of the range at `range`.
fn set_range(root: &mut Value, range: &str, start: i64, duration: i64) {
  set(root, &format!("{range}.start_time"), "value", count(start));
  set(root, &format!("{range}.duration"), "value", count(duration));
}

#[test]
fn a_range_whose_end_opentimelineio_rounds_is_refused() {
  const ROUNDS: &str = "a range whose end, or last tick, OpenTimelineIO's f64 arithmetic rounds";
  // A track's marker, at the edit rate: one tick ending at 2^53 reads; one
  // tick on, OpenTimelineIO's sum rounds 2^53 + 1 back to 2^53.
  let marker = |start: i64| {
    read(move |root| set_range(root, &format!("{TRACK}.markers.0.marked_range"), start, 1))
  };
  assert_eq!(
    marker(LIMIT - 1).unwrap().tracks()[0].markers()[0].range(),
    TimeRange::new(LIMIT - 1, LIMIT, edit()),
  );
  assert_eq!(
    marker(LIMIT),
    Err(inexact(&format!("{TRACK}.markers.0.marked_range"), ROUNDS)),
  );
  // A clip's marker, in the ruler its word names.
  let clip_marker = |start: i64| {
    read(move |root| set_range(root, &format!("{A}.markers.0.marked_range"), start, 2))
  };
  assert_eq!(
    clip_marker(LIMIT - 2).unwrap().tracks()[0].clips()[0].markers()[0].range(),
    TimeRange::new(LIMIT - 2, LIMIT, edit()),
  );
  assert_eq!(
    clip_marker(LIMIT - 1),
    Err(inexact(&format!("{A}.markers.0.marked_range"), ROUNDS)),
  );
  // `b`'s source range, its medium's available range left unknown so the
  // source may lie anywhere.
  let source = |start: i64| {
    read(move |root| {
      let media = format!("{B}.media_references.DEFAULT_MEDIA");
      set(root, &media, "available_range", Value::Null);
      remove(root, &format!("{media}.metadata.mediaio"), "available");
      set_range(root, &format!("{B}.source_range"), start, 24);
    })
  };
  assert_eq!(
    source(LIMIT - 24).unwrap().tracks()[0].clips()[1].source_range(),
    TimeRange::new(LIMIT - 24, LIMIT, edit()),
  );
  assert_eq!(
    source(LIMIT - 23),
    Err(inexact(&format!("{B}.source_range"), ROUNDS)),
  );
  // An available range.
  assert_eq!(
    read(|root| set_range(root, &format!("{A_MEDIA}.available_range"), LIMIT - 99, 100)),
    Err(inexact(&format!("{A_MEDIA}.available_range"), ROUNDS)),
  );
}

#[test]
fn what_opentimelineio_derives_from_a_track_is_held_as_the_export_holds_it() {
  // The leading gap so long that `b` ends at 2^53: read, every record
  // moved on with it.
  let gap =
    |length: i64| read(move |root| set_range(root, &format!("{GAP}.source_range"), 0, length));
  assert_eq!(
    gap(LIMIT - 48).unwrap().tracks()[0].clips()[1].record(),
    TimeRange::new(LIMIT - 24, LIMIT, edit()),
  );
  // One frame longer, `b` ends at 2^53 + 1 on its track, which
  // OpenTimelineIO's sum rounds.
  assert_eq!(
    gap(LIMIT - 47),
    Err(inexact(
      B,
      "a place on its track that OpenTimelineIO's f64 arithmetic rounds"
    )),
  );
  // `a`'s source so late that the dissolve's handle after it reaches past
  // 2^53: its visible range.
  assert_eq!(
    read(|root| {
      let start = format!("{A}.source_range.start_time");
      set(root, &start, "value", count(LIMIT - 25));
    }),
    Err(inexact(
      A,
      "a visible range that OpenTimelineIO's f64 arithmetic rounds"
    )),
  );
  // A global start so late that `b` ends past 2^53 from it.
  assert_eq!(
    read(|root| set(root, "global_start_time", "value", count(LIMIT - 50))),
    Err(inexact(
      B,
      "a place from the global start that OpenTimelineIO's f64 arithmetic rounds"
    )),
  );
}

#[test]
fn every_number_is_read_exactly_or_refused() {
  const GAIN: &str = "a gain that is not exactly the f32 it reads as";
  const RATE: &str = "a rate that is not exactly the f64 it reads as";
  let gain_at = format!("{A}.metadata.mediaio.gain_db");
  let gain = |spelled: &'static str| {
    read(move |root| {
      set(
        root,
        &format!("{A}.metadata.mediaio"),
        "gain_db",
        number(spelled),
      )
    })
  };
  // Exactly an f32, and other spellings of the sample's own -3 dB.
  assert_eq!(
    gain("16777216").unwrap().tracks()[0].clips()[0].gain(),
    Gain::from_db(16_777_216.0),
  );
  for spelled in ["-3", "-3e0", "-0.3E1", "-30e-1"] {
    assert_eq!(gain(spelled), Ok(sample()), "{spelled}");
  }
  // Rounded to its nearest f32 (2^24 + 1 reads as 2^24), past the largest,
  // and below the smallest.
  for spelled in ["16777217", "-3.0000001", "1e39", "1e-50"] {
    assert_eq!(gain(spelled), Err(inexact(&gain_at, GAIN)), "{spelled}");
  }

  // A RationalTime's rate, the same way: any spelling of the f64 the
  // export writes, and no other number.
  let rate = |spelled: &'static str| {
    read(move |root| set(root, "global_start_time", "rate", number(spelled)))
  };
  for spelled in ["24", "2.4e1", "2400E-2", "24.000"] {
    assert_eq!(rate(spelled), Ok(sample()), "{spelled}");
  }
  for spelled in ["24.0000000000000001", "1e309", "1e-400"] {
    assert_eq!(
      rate(spelled),
      Err(inexact("global_start_time.rate", RATE)),
      "{spelled}"
    );
  }
}

#[test]
fn a_word_missing_or_misshapen_is_refused_by_name() {
  assert_eq!(
    read(|root| remove(root, &format!("{A}.metadata.mediaio"), "id")),
    Err(shape(
      &format!("{A}.metadata.mediaio.id"),
      "a required word"
    )),
  );
  assert_eq!(
    read(|root| remove(root, "metadata.mediaio", "rate")),
    Err(shape("metadata.mediaio.rate", "a required word")),
  );
  assert_eq!(
    read(|root| set(root, "metadata.mediaio", "rate", text("23.976"))),
    Err(shape(
      "metadata.mediaio.rate",
      "a rate above zero, written num/den"
    )),
  );
  assert_eq!(
    read(|root| remove(root, &format!("{A_MEDIA}.metadata.mediaio"), "available")),
    Err(shape(
      &format!("{A_MEDIA}.metadata.mediaio.available"),
      "a required word"
    )),
  );
  // One dissolve may say its place, which is the first; no other place.
  assert_eq!(
    read(|root| set(
      root,
      &format!("{DISSOLVE}.metadata"),
      "mediaio",
      object(vec![("order", number("0"))])
    )),
    Ok(sample()),
  );
  assert_eq!(
    read(|root| set(
      root,
      &format!("{DISSOLVE}.metadata"),
      "mediaio",
      object(vec![("order", number("1"))])
    )),
    Err(shape(
      &format!("{DISSOLVE}.metadata.mediaio.order"),
      "a place among the track's transitions, said by every dissolve, each place once"
    )),
  );
  // A transition at a track's edge.
  assert_eq!(
    read(|root| {
      let Value::Array(children) = at(root, &format!("{TRACK}.children")) else {
        unreachable!()
      };
      children.remove(0);
    }),
    Err(shape(
      &format!("{TRACK}.children.0"),
      "a transition between two items"
    )),
  );
}

#[test]
fn text_that_is_not_json_and_a_timeline_that_does_not_validate_are_refused() {
  assert!(matches!(from_otio("{"), Err(Unreadable::Syntax(_))));
  // `b` under `a`'s id: read whole, then refused as the timeline is.
  assert_eq!(
    read(|root| set(root, &format!("{B}.metadata.mediaio"), "id", text("a"))),
    Err(Unreadable::Validation(vec![Refusal::DuplicateClipId(
      IdClash::new(ClipAt::new(0, 0), ClipAt::new(0, 1))
    )])),
  );
}

#[test]
fn reader_refusals_say_what_and_where() {
  let shown = |refused: Unreadable| std::string::ToString::to_string(&refused);
  assert_eq!(
    shown(schema("Effect.1", &format!("{A}.effects.0"))),
    "$.tracks.children[0].children[2].effects[0]: the schema Effect.1, which the timeline does not model there"
  );
  assert_eq!(
    shown(feature(
      Feature::TrackKind("Data".into()),
      &format!("{TRACK}.kind")
    )),
    "$.tracks.children[0].kind: the track kind Data, where the timeline knows Video and Audio"
  );
  assert_eq!(
    shown(inexact(
      "global_start_time.value",
      "a count that is no whole number"
    )),
    "$.global_start_time.value: a count that is no whole number"
  );
  assert_eq!(
    shown(Unreadable::Validation(vec![Refusal::RateUnstated])),
    "the timeline read does not validate: the edit rate is zero"
  );
}

#[test]
fn every_marker_color_reads_back_in_any_case() {
  for color in MarkerColor::ALL {
    let mut timeline = sample();
    timeline.tracks_mut()[0].markers_mut()[0].set_color(color);
    let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
    assert_eq!(from_otio(&text).as_ref(), Ok(&timeline), "{color}");
    let lower = text.as_str().replace(color.otio_name(), &color.to_string());
    let read = from_otio(&lower).unwrap();
    assert_eq!(read.tracks()[0].markers()[0].color(), color, "{lower}");
  }
  assert_eq!(
    MarkerColor::from_otio_name("Green"),
    Some(MarkerColor::Green)
  );
  assert_eq!(MarkerColor::from_otio_name("GRAY"), None);
}

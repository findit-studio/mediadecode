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
  MediaRef, Rate, Refusal, TimeRange, Timeline, Timestamp, Track, TrackKind, Transition,
  otio::{
    Feature, Inexact, OtioTarget, Shape, UnmodelledFeature, UnmodelledSchema, Unreadable,
    from_otio, to_otio,
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

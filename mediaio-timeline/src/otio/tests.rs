use alloc::{format, string::String, vec::Vec};
use core::num::NonZeroI32;

use super::{json::Value, *};
use crate::{
  Clip, Duration, Fade, FadeShape, Fades, MediaRef, Rate, TimeRange, Timebase, Track, TrackKind,
  Transition,
};

fn tb(num: i32, den: i32) -> Timebase {
  Timebase::new(num, NonZeroI32::new(den).unwrap())
}

fn edit() -> Timebase {
  tb(1, 24)
}

fn clip(name: &str, start: i64, end: i64) -> Clip {
  Clip::new(
    name,
    MediaRef::new(format!("file:///{name}.mov")),
    TimeRange::new(0, end - start, edit()),
    TimeRange::new(start, end, edit()),
  )
}

fn fade(frames: u64) -> Option<Fade> {
  Some(Fade::new(Duration::new(frames, edit()), FadeShape::Linear))
}

fn one_track(track: Track) -> Timeline {
  Timeline::new("t", Rate::FPS_24).with_track(track)
}

/// The schema names of the first track's children, in order, with each
/// gap's and transition's lengths: `Gap(0)`, `In(0, 6)`, `Clip`.
fn children(timeline: &Timeline) -> Vec<String> {
  let text = to_otio(timeline, OtioTarget::V0_15Plus).unwrap();
  validate_json(&text, OtioTarget::V0_15Plus).unwrap();
  let root = json::parse(&text).unwrap();
  let get = |value: &Value, key: &str| -> Value {
    value
      .as_object()
      .unwrap()
      .iter()
      .find(|(k, _)| k == key)
      .unwrap()
      .1
      .clone()
  };
  let frames = |time: &Value| get(time, "value").as_f64().unwrap();
  let tracks = get(&root, "tracks");
  let first = get(&tracks, "children").as_array().unwrap()[0].clone();
  get(&first, "children")
    .as_array()
    .unwrap()
    .iter()
    .map(|child| match get(child, "OTIO_SCHEMA").as_str().unwrap() {
      "Gap.1" => format!(
        "Gap({})",
        frames(&get(&get(child, "source_range"), "duration"))
      ),
      "Transition.1" => format!(
        "Dissolve({}, {})",
        frames(&get(child, "in_offset")),
        frames(&get(child, "out_offset"))
      ),
      _ => String::from(get(child, "name").as_str().unwrap()),
    })
    .collect()
}

#[test]
fn a_fade_out_at_a_tracks_end_dissolves_to_a_gap_of_no_length() {
  let timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, 24).with_fades(Fades::new().with_out(fade(6)))),
  );
  assert_eq!(children(&timeline), ["a", "Dissolve(6, 0)", "Gap(0)"]);
}

#[test]
fn a_fade_in_at_a_tracks_start_dissolves_from_a_gap_of_no_length() {
  let timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, 24).with_fades(Fades::new().with_in(fade(6)))),
  );
  assert_eq!(children(&timeline), ["Gap(0)", "Dissolve(0, 6)", "a"]);
}

#[test]
fn a_fade_beside_a_derived_gap_dissolves_against_that_gap() {
  let timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, 24).with_fades(Fades::new().with_out(fade(6))))
      .with_clip(clip("b", 30, 54).with_fades(Fades::new().with_in(fade(6)))),
  );
  assert_eq!(
    children(&timeline),
    ["a", "Dissolve(6, 0)", "Gap(6)", "Dissolve(0, 6)", "b"]
  );
}

#[test]
fn a_fade_out_meeting_a_fade_in_shares_one_gap_of_no_length() {
  let timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, 24).with_fades(Fades::new().with_out(fade(6))))
      .with_clip(clip("b", 24, 48).with_fades(Fades::new().with_in(fade(6)))),
  );
  assert_eq!(
    children(&timeline),
    ["a", "Dissolve(6, 0)", "Gap(0)", "Dissolve(0, 6)", "b"]
  );
}

#[test]
fn a_dissolve_sits_between_the_two_clips_it_joins() {
  // `b` plays from the start of its medium, so the dissolve's four frames
  // before the cut have no head handle until its source moves on.
  let mut a = clip("a", 0, 24);
  a.set_source_range(TimeRange::new(10, 34, edit()));
  let mut b = clip("b", 24, 48).with_enabled(false);
  b.media_mut()
    .set_available_range(Some(TimeRange::new(0, 100, edit())));
  let mut timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(a)
      .with_clip(b)
      .with_clip(clip("c", 48, 72))
      .with_transition(Transition::dissolve(
        crate::Timestamp::new(24, edit()),
        Duration::new(4, edit()),
        Duration::new(2, edit()),
      )),
  );
  assert_eq!(
    to_otio(&timeline, OtioTarget::V0_15Plus),
    Err(alloc::vec![Refusal::HandleMissing(crate::EdgeAt::new(
      crate::ClipAt::new(0, 1),
      crate::Edge::In
    ))])
  );
  timeline.tracks_mut()[0].clips_mut()[1].set_source_range(TimeRange::new(10, 34, edit()));
  assert_eq!(children(&timeline), ["a", "Dissolve(4, 2)", "b", "c"]);
}

#[test]
fn a_timeline_that_does_not_validate_is_refused_with_its_refusals() {
  let timeline = one_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, 24))
      .with_clip(clip("b", 12, 36)),
  );
  assert_eq!(
    to_otio(&timeline, OtioTarget::Legacy),
    Err(alloc::vec![Refusal::Overlap(crate::ClipPair::new(0, 0, 1))])
  );
}

/// The first clip's `source_range` as written: `(rate, start, duration)`,
/// after checking that its start and its duration share that rate.
fn source_range(timeline: &Timeline) -> (f64, f64, f64) {
  let text = to_otio(timeline, OtioTarget::Legacy).unwrap();
  let root = json::parse(&text).unwrap();
  let get = |value: &Value, key: &str| -> Value {
    value
      .as_object()
      .unwrap()
      .iter()
      .find(|(k, _)| k == key)
      .unwrap()
      .1
      .clone()
  };
  let track = get(&get(&root, "tracks"), "children").as_array().unwrap()[0].clone();
  let clip = get(&track, "children").as_array().unwrap()[0].clone();
  let range = get(&clip, "source_range");
  let (start, duration) = (get(&range, "start_time"), get(&range, "duration"));
  let rate = get(&duration, "rate").as_f64().unwrap();
  assert_eq!(
    get(&start, "rate").as_f64(),
    Some(rate),
    "a range written in two rates"
  );
  (
    rate,
    get(&start, "value").as_f64().unwrap(),
    get(&duration, "value").as_f64().unwrap(),
  )
}

/// One clip `a` playing `source` of a medium at `rate`, at the record
/// [0, `frames`) of a timeline at `edit`.
fn one_source(edit: Rate, frames: i64, source: TimeRange, rate: Option<Rate>) -> Timeline {
  let mut a = clip("a", 0, frames);
  a.set_source_range(source);
  a.set_record(TimeRange::new(
    0,
    frames,
    edit.checked_to_timebase().unwrap(),
  ));
  a.media_mut().set_rate(rate);
  Timeline::new("t", edit).with_track(Track::new(TrackKind::Video, "V").with_clip(a))
}

#[test]
fn a_source_range_is_written_whole_in_the_mediums_ruler() {
  // A second of 48 kHz sound from sample 1000, under 24 fps: the start and
  // the length in samples, so the range OpenTimelineIO reads ends at 49 000,
  // where the stored range ends — not at a length counted in frames.
  let sound = one_source(
    Rate::FPS_24,
    24,
    TimeRange::new(1000, 49_000, tb(1, 48_000)),
    Some(Rate::hz(48_000)),
  );
  assert_eq!(source_range(&sound), (48_000.0, 1000.0, 48_000.0));
  // On frames of the medium's stated rate: both in frames.
  let movie = one_source(
    Rate::FPS_23_976,
    24,
    TimeRange::new(1001, 1001 + 24_024, tb(1, 24_000)),
    Some(Rate::FPS_23_976),
  );
  assert_eq!(source_range(&movie), (24_000.0 / 1001.0, 1.0, 24.0));
}

#[test]
fn a_source_range_off_the_mediums_frames_is_written_in_ticks_start_and_length() {
  // The start is half a frame in at 24 fps: both in ticks.
  let between = one_source(
    Rate::FPS_24,
    24,
    TimeRange::new(500, 500 + 24_000, tb(1, 24_000)),
    Some(Rate::FPS_24),
  );
  assert_eq!(source_range(&between), (24_000.0, 500.0, 24_000.0));
  // The start lands on a frame of the medium's 25 fps, the length — three
  // frames at 24, 3.125 at 25 — does not: both in ticks, never one of each.
  let start_only = one_source(
    Rate::FPS_24,
    3,
    TimeRange::new(3600, 3600 + 11_250, tb(1, 90_000)),
    Some(Rate::FPS_25),
  );
  assert_eq!(source_range(&start_only), (90_000.0, 3600.0, 11_250.0));
  // No stated rate: ticks.
  let unstated = one_source(
    Rate::FPS_24,
    24,
    TimeRange::new(1000, 1000 + 24_000, tb(1, 24_000)),
    None,
  );
  assert_eq!(source_range(&unstated), (24_000.0, 1000.0, 24_000.0));
}

#[test]
fn a_source_off_the_edit_rate_is_not_exported() {
  // [0, 1) at 48 per second is half a frame at 24 fps: a record of one frame
  // would select twice the stored media, so the timeline is refused.
  let half = one_source(Rate::FPS_24, 1, TimeRange::new(0, 1, tb(1, 48)), None);
  assert_eq!(
    to_otio(&half, OtioTarget::V0_15Plus),
    Err(alloc::vec![Refusal::SourceOffEditRate(crate::ClipAt::new(
      0, 0
    ))])
  );
}

#[test]
fn names_with_quotes_and_control_characters_are_escaped() {
  let timeline = one_track(Track::new(TrackKind::Video, "V \"1\"\n").with_clip(clip("a\\b", 0, 1)));
  let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  assert!(text.contains(r#""name": "V \"1\"\n""#), "{text}");
  assert!(text.contains(r#""name": "a\\b""#), "{text}");
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
}

#[test]
fn an_empty_timeline_exports_an_empty_stack() {
  let text = to_otio(&Timeline::new("empty", Rate::FPS_25), OtioTarget::V0_15Plus).unwrap();
  assert!(text.contains(r#""children": []"#), "{text}");
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  assert!(text.ends_with("}\n"));
}

fn shape_refusal(text: &str, target: OtioTarget) -> (String, &'static str) {
  match validate_json(text, target) {
    Err(Invalid::Shape(shape)) => (String::from(shape.path()), shape.expected()),
    other => panic!("{other:?}"),
  }
}

/// A one-clip document, valid for both targets once `clip` is filled in.
fn document(clip: &str) -> String {
  format!(
    r#"{{"OTIO_SCHEMA": "Timeline.1", "tracks": {{"OTIO_SCHEMA": "Stack.1", "children": [
      {{"OTIO_SCHEMA": "Track.1", "kind": "Video", "children": [{clip}]}}]}}}}"#
  )
}

const RT: &str = r#"{"OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 1.0}"#;

fn gap() -> String {
  format!(
    r#"{{"OTIO_SCHEMA": "Gap.1", "source_range": {{"OTIO_SCHEMA": "TimeRange.1", "duration": {RT}, "start_time": {RT}}}}}"#
  )
}

fn dissolve() -> String {
  format!(
    r#"{{"OTIO_SCHEMA": "Transition.1", "in_offset": {RT}, "out_offset": {RT}, "transition_type": "SMPTE_Dissolve"}}"#
  )
}

#[test]
fn the_self_check_accepts_the_minimal_shapes() {
  assert_eq!(validate_json(&document(&gap()), OtioTarget::Legacy), Ok(()));
  let clip1 = r#"{"OTIO_SCHEMA": "Clip.1", "media_reference": {"OTIO_SCHEMA": "ExternalReference.1", "target_url": "a.mov"}}"#;
  assert_eq!(validate_json(&document(clip1), OtioTarget::Legacy), Ok(()));
  assert_eq!(
    validate_json(&document(clip1), OtioTarget::V0_15Plus),
    Ok(())
  );
  let between = format!("{}, {}, {}", gap(), dissolve(), gap());
  assert_eq!(
    validate_json(&document(&between), OtioTarget::Legacy),
    Ok(())
  );
}

#[test]
fn the_self_check_refuses_by_path() {
  let target = OtioTarget::V0_15Plus;
  assert_eq!(
    shape_refusal(r#"{"OTIO_SCHEMA": "Stack.1"}"#, target),
    (String::from("$.OTIO_SCHEMA"), "the schema Timeline.1")
  );
  assert_eq!(
    shape_refusal(r#"{"OTIO_SCHEMA": "Timeline.1"}"#, target),
    (String::from("$.tracks"), "a required key")
  );
  assert_eq!(
    shape_refusal(
      &document(r#"{"OTIO_SCHEMA": "Gap.1", "enabled": "yes"}"#),
      target
    ),
    (
      String::from("$.tracks.children[0].children[0].enabled"),
      "a boolean"
    )
  );
  let rate_zero = gap().replace("\"rate\": 24.0", "\"rate\": 0.0");
  assert_eq!(
    shape_refusal(&document(&rate_zero), target),
    (
      String::from("$.tracks.children[0].children[0].source_range.duration.rate"),
      "a positive, finite rate"
    )
  );
  let negative = gap().replacen("\"value\": 1.0", "\"value\": -1.0", 1);
  assert_eq!(
    shape_refusal(&document(&negative), target),
    (
      String::from("$.tracks.children[0].children[0].source_range.duration.value"),
      "a length of zero or more"
    )
  );
  assert_eq!(
    shape_refusal(&document(r#"{"OTIO_SCHEMA": "Clip.3"}"#), target),
    (
      String::from("$.tracks.children[0].children[0].OTIO_SCHEMA"),
      "a schema a track holds"
    )
  );
}

#[test]
fn a_track_without_kind_is_refused_for_both_targets() {
  // Every reader takes `kind` with `reader.read`, which fails on a key left
  // out (track.cpp).
  let kindless = document(&gap()).replace(r#""kind": "Video", "#, "");
  assert!(!kindless.contains("kind"));
  for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
    assert_eq!(
      shape_refusal(&kindless, target),
      (String::from("$.tracks.children[0].kind"), "a required key")
    );
  }
}

#[test]
fn a_clip_1_without_its_media_reference_is_refused_for_legacy_readers_only() {
  // Readers before 0.15 read `media_reference` with `reader.read`; later
  // ones upgrade a `Clip.1` and take a reference left out as missing.
  let bare = document(r#"{"OTIO_SCHEMA": "Clip.1"}"#);
  assert_eq!(
    shape_refusal(&bare, OtioTarget::Legacy),
    (
      String::from("$.tracks.children[0].children[0].media_reference"),
      "a required key"
    )
  );
  assert_eq!(validate_json(&bare, OtioTarget::V0_15Plus), Ok(()));
  // A null reference is read, as an empty one.
  let null = document(r#"{"OTIO_SCHEMA": "Clip.1", "media_reference": null}"#);
  assert_eq!(validate_json(&null, OtioTarget::Legacy), Ok(()));
}

/// The keys each schema's reader requires, per target: the audit in
/// `check`'s docs, transcribed.
fn required_by_readers(schema: &str, target: OtioTarget) -> &'static [&'static str] {
  match (schema, target) {
    ("Timeline.1", _) => &["tracks"],
    ("Stack.1", _) => &["children"],
    ("Track.1", _) => &["kind", "children"],
    ("Clip.2", _) => &["media_references", "active_media_reference_key"],
    ("Clip.1", OtioTarget::Legacy) => &["media_reference"],
    ("Clip.1" | "Gap.1", _) => &[],
    ("Transition.1", _) => &["in_offset", "out_offset", "transition_type"],
    ("ExternalReference.1", _) => &["target_url"],
    ("RationalTime.1", _) => &["rate", "value"],
    ("TimeRange.1", _) => &["start_time", "duration"],
    _ => panic!("no audit for {schema}"),
  }
}

/// One step from a value to one inside it.
#[derive(Clone)]
enum Step {
  Key(String),
  At(usize),
}

/// Every object naming a schema inside `value`: its path as the check
/// writes one, the steps to it, its schema, and its keys.
fn schema_objects(
  value: &Value,
  path: &str,
  steps: &[Step],
  out: &mut Vec<(String, Vec<Step>, String, Vec<String>)>,
) {
  match value {
    Value::Object(members) => {
      if let Some(schema) = members
        .iter()
        .find(|(key, _)| key == "OTIO_SCHEMA")
        .and_then(|(_, schema)| schema.as_str())
      {
        let keys = members
          .iter()
          .map(|(key, _)| key.clone())
          .filter(|key| key != "OTIO_SCHEMA")
          .collect();
        out.push((
          String::from(path),
          steps.to_vec(),
          String::from(schema),
          keys,
        ));
      }
      for (key, member) in members {
        let mut deeper = steps.to_vec();
        deeper.push(Step::Key(key.clone()));
        schema_objects(member, &format!("{path}.{key}"), &deeper, out);
      }
    }
    Value::Array(items) => {
      for (index, item) in items.iter().enumerate() {
        let mut deeper = steps.to_vec();
        deeper.push(Step::At(index));
        schema_objects(item, &format!("{path}[{index}]"), &deeper, out);
      }
    }
    _ => {}
  }
}

/// `root` with `key` taken out of the object `steps` lead to, written out.
fn without(root: &Value, steps: &[Step], key: &str) -> String {
  let mut root = root.clone();
  let mut at = &mut root;
  for step in steps {
    at = match (step, at) {
      (Step::Key(name), Value::Object(members)) => {
        &mut members
          .iter_mut()
          .find(|(member, _)| member == name)
          .unwrap()
          .1
      }
      (Step::At(index), Value::Array(items)) => &mut items[*index],
      _ => unreachable!(),
    };
  }
  let Value::Object(members) = at else {
    unreachable!()
  };
  members.retain(|(member, _)| member != key);
  let mut text = String::new();
  json::write_pretty(&root, &mut text);
  text
}

#[test]
fn the_self_check_requires_exactly_what_the_targets_readers_require() {
  // Each key of each object of each golden taken out in turn: refused where
  // the target's readers require it, read where they do not. Fewer under
  // Miri, which interprets slowly.
  let every = if cfg!(miri) { 23 } else { 1 };
  for (target, golden) in [
    (
      OtioTarget::V0_15Plus,
      include_str!("../../tests/golden/law.v0_15.otio"),
    ),
    (
      OtioTarget::Legacy,
      include_str!("../../tests/golden/law.legacy.otio"),
    ),
  ] {
    let root = json::parse(golden).unwrap();
    let mut objects = Vec::new();
    schema_objects(&root, "$", &[], &mut objects);
    let cases = objects.iter().flat_map(|(path, steps, schema, keys)| {
      keys.iter().map(move |key| (path, steps, schema, key))
    });
    let mut checked = 0;
    for (path, steps, schema, key) in cases.step_by(every) {
      let verdict = validate_json(&without(&root, steps, key), target);
      if required_by_readers(schema, target).contains(&key.as_str()) {
        assert_eq!(
          verdict,
          Err(Invalid::Shape(Shape {
            path: format!("{path}.{key}"),
            expected: "a required key",
          })),
          "{target:?}: {schema} at {path} without {key}"
        );
      } else {
        assert_eq!(
          verdict,
          Ok(()),
          "{target:?}: {schema} at {path} without {key}"
        );
      }
      checked += 1;
    }
    assert!(checked > 0);
  }
}

#[test]
fn the_self_check_holds_transitions_between_two_items() {
  let target = OtioTarget::Legacy;
  for (children, at) in [
    (format!("{}, {}", dissolve(), gap()), 0),
    (format!("{}, {}", gap(), dissolve()), 1),
    (
      format!("{}, {}, {}, {}", gap(), dissolve(), dissolve(), gap()),
      1,
    ),
  ] {
    assert_eq!(
      shape_refusal(&document(&children), target),
      (
        format!("$.tracks.children[0].children[{at}]"),
        "a transition between two items"
      )
    );
  }
}

#[test]
fn the_self_check_holds_the_targets_clip_schema() {
  let clip2 = r#"{"OTIO_SCHEMA": "Clip.2", "media_references": {"DEFAULT_MEDIA": {"OTIO_SCHEMA": "MissingReference.1"}}, "active_media_reference_key": "DEFAULT_MEDIA"}"#;
  assert_eq!(
    validate_json(&document(clip2), OtioTarget::V0_15Plus),
    Ok(())
  );
  assert_eq!(
    shape_refusal(&document(clip2), OtioTarget::Legacy),
    (
      String::from("$.tracks.children[0].children[0].OTIO_SCHEMA"),
      "Clip.1, the only clip readers before OpenTimelineIO 0.15 know"
    )
  );
  let dangling = clip2.replace(
    r#""active_media_reference_key": "DEFAULT_MEDIA""#,
    r#""active_media_reference_key": "OTHER""#,
  );
  assert_eq!(
    shape_refusal(&document(&dangling), OtioTarget::V0_15Plus),
    (
      String::from("$.tracks.children[0].children[0].active_media_reference_key"),
      "a key of media_references"
    )
  );
  let no_url = clip2.replace("MissingReference.1", "ExternalReference.1");
  assert_eq!(
    shape_refusal(&document(&no_url), OtioTarget::V0_15Plus),
    (
      String::from("$.tracks.children[0].children[0].media_references.DEFAULT_MEDIA.target_url"),
      "a required key"
    )
  );
}

#[test]
fn a_document_that_is_not_json_is_refused_as_syntax() {
  assert_eq!(
    validate_json("{\"OTIO_SCHEMA\": ", OtioTarget::V0_15Plus),
    Err(Invalid::Syntax(Syntax {
      offset: 16,
      reason: "expected a value"
    }))
  );
  assert_eq!(
    alloc::string::ToString::to_string(&validate_json("[", OtioTarget::Legacy).unwrap_err()),
    "byte 1: expected a value"
  );
}

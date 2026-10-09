use alloc::{format, string::String, vec::Vec};
use core::num::NonZeroI32;

use super::{json::Value, *};
use crate::{
  Clip, Duration, Fade, FadeShape, Fades, MediaRef, Rate, TimeRange, Timebase, Timestamp, Track,
  TrackKind, Transition,
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
    Err(Refused::Validation(alloc::vec![Refusal::HandleMissing(
      crate::EdgeAt::new(crate::ClipAt::new(0, 1), crate::Edge::In)
    )]))
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
    Err(Refused::Validation(alloc::vec![Refusal::Overlap(
      crate::ClipPair::new(0, 0, 1)
    )]))
  );
}

fn member(value: &Value, key: &str) -> Value {
  value
    .as_object()
    .unwrap()
    .iter()
    .find(|(k, _)| k == key)
    .unwrap()
    .1
    .clone()
}

/// The first clip's `source_range` as written: `(rate, start, duration)`,
/// after checking that its start and its duration share that rate.
fn source_range(timeline: &Timeline) -> (f64, f64, f64) {
  rate_start_duration(&member(&first_clip(timeline), "source_range"))
}

/// The first clip's medium's `available_range` as written, as
/// [`source_range`] reads one.
fn available_range(timeline: &Timeline) -> (f64, f64, f64) {
  let reference = member(&first_clip(timeline), "media_reference");
  rate_start_duration(&member(&reference, "available_range"))
}

/// The first track's first clip, written for `Legacy` readers.
fn first_clip(timeline: &Timeline) -> Value {
  let text = to_otio(timeline, OtioTarget::Legacy).unwrap();
  let root = json::parse(&text).unwrap();
  let track = member(&member(&root, "tracks"), "children")
    .as_array()
    .unwrap()[0]
    .clone();
  member(&track, "children").as_array().unwrap()[0].clone()
}

fn rate_start_duration(range: &Value) -> (f64, f64, f64) {
  let (start, duration) = (member(range, "start_time"), member(range, "duration"));
  let rate = member(&duration, "rate").as_f64().unwrap();
  assert_eq!(
    member(&start, "rate").as_f64(),
    Some(rate),
    "a range written in two rates"
  );
  (
    rate,
    member(&start, "value").as_f64().unwrap(),
    member(&duration, "value").as_f64().unwrap(),
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
    Err(Refused::Validation(alloc::vec![
      Refusal::SourceOffEditRate(crate::ClipAt::new(0, 0))
    ]))
  );
}

const TWO_53: i64 = 1 << 53;

/// Where `to_otio` refuses `timeline` as not representable, and the count.
fn not_representable(timeline: &Timeline) -> (Spot, i128) {
  match to_otio(timeline, OtioTarget::V0_15Plus) {
    Err(Refused::NotRepresentable(count)) => (count.at(), count.value()),
    other => panic!("{other:?}"),
  }
}

/// One clip `a` at the record [`start`, `end`) of a timeline at one frame a
/// second, playing as long a stretch of a medium counted in whole seconds.
fn at_one_fps(start: i64, end: i64) -> Timeline {
  let second = tb(1, 1);
  one_track_at(
    Rate::hz(1),
    Clip::new(
      "a",
      MediaRef::new("file:///a.mov"),
      TimeRange::new(0, end - start, second),
      TimeRange::new(start, end, second),
    ),
  )
}

fn one_track_at(rate: Rate, clip: Clip) -> Timeline {
  Timeline::new("t", rate).with_track(Track::new(TrackKind::Video, "V").with_clip(clip))
}

#[test]
fn a_count_past_2_53_is_refused_rather_than_read_rounded() {
  // An f64 holds every whole number up to 2^53 and only some past it:
  // OpenTimelineIO reads 2^53 + 1 as 2^53.
  assert_eq!(
    "9007199254740993.0".parse::<f64>(),
    Ok(9_007_199_254_740_992.0)
  );
  let a = Spot::Record(ClipAt::new(0, 0));
  let past = i128::from(TWO_53) + 1;
  // Codex's case: a clip at [2^53 + 1, 2^53 + 2) on a timeline at one frame
  // a second. Its leading gap of 2^53 + 1 frames would read as 2^53, placing
  // the clip a second early.
  assert_eq!(
    not_representable(&at_one_fps(TWO_53 + 1, TWO_53 + 2)),
    (a, past)
  );
  // A gap of 2^53 and a clip of one: each count is held, but the record ends
  // at their sum, 2^53 + 1, where OpenTimelineIO adds them up.
  assert_eq!(
    not_representable(&at_one_fps(TWO_53, TWO_53 + 1)),
    (a, past)
  );
  // The start, at the edit rate.
  let early = at_one_fps(0, 1).with_start(Timestamp::new(-TWO_53 - 1, tb(1, 1)));
  assert_eq!(not_representable(&early), (Spot::Start, -past));
  // Up to 2^53 itself is written, and reads back whole.
  assert_eq!(
    children(&at_one_fps(TWO_53 - 1, TWO_53)),
    ["Gap(9007199254740991)", "a"]
  );
}

#[test]
fn a_media_range_whose_end_is_past_2_53_is_refused() {
  // The source range [2^53 - 1, 2^53 + 1): its start and its length are
  // held, the end OpenTimelineIO adds up from them is not, and in whole
  // seconds no coarser whole ruler holds it.
  let second = tb(1, 1);
  let mut timeline = at_one_fps(0, 2);
  timeline.tracks_mut()[0].clips_mut()[0].set_source_range(TimeRange::new(
    TWO_53 - 1,
    TWO_53 + 1,
    second,
  ));
  assert_eq!(
    not_representable(&timeline),
    (Spot::Source(ClipAt::new(0, 0)), i128::from(TWO_53) + 1)
  );
  // An available range 2^53 + 1 seconds long.
  let mut timeline = at_one_fps(0, 2);
  timeline.tracks_mut()[0].clips_mut()[0]
    .media_mut()
    .set_available_range(Some(TimeRange::new(0, TWO_53 + 1, second)));
  assert_eq!(
    not_representable(&timeline),
    (Spot::Available(ClipAt::new(0, 0)), i128::from(TWO_53) + 1)
  );
}

#[test]
fn a_media_range_its_ticks_cannot_hold_is_written_in_the_coarsest_whole_ruler_that_can() {
  // A medium stamped in nanoseconds since 1970: one second of it from
  // 1 700 000 000 s starts at tick 1.7e18, past 2^53, and has no stated rate
  // to count frames in. Whole seconds hold it.
  let nanos = Timebase::NANOS;
  let second = 1_000_000_000;
  let epoch = 1_700_000_000 * second;
  let mut timeline = one_source(
    Rate::FPS_25,
    25,
    TimeRange::new(epoch, epoch + second, nanos),
    None,
  );
  timeline.tracks_mut()[0].clips_mut()[0]
    .media_mut()
    .set_available_range(Some(TimeRange::new(epoch, epoch + 10 * second, nanos)));
  assert_eq!(source_range(&timeline), (1.0, 1_700_000_000.0, 1.0));
  assert_eq!(available_range(&timeline), (1.0, 1_700_000_000.0, 10.0));
  // Starting between seconds, on a millisecond: milliseconds.
  let off = epoch + 123_000_000;
  let mut timeline = one_source(
    Rate::FPS_25,
    25,
    TimeRange::new(off, off + second, nanos),
    None,
  );
  assert_eq!(
    source_range(&timeline),
    (1000.0, 1_700_000_000_123.0, 1000.0)
  );
  // Read back, the counts land on the stored ends, exactly.
  let stored = timeline.tracks()[0].clips()[0].source_range();
  let (rate, start, duration) = source_range(&timeline);
  let nanoseconds = |count: f64| count as i128 * i128::from(second) / rate as i128;
  assert_eq!(nanoseconds(start), i128::from(stored.start_pts()));
  assert_eq!(nanoseconds(start + duration), i128::from(stored.end_pts()));
  // A stated rate whose frames it does not land on changes nothing.
  timeline.tracks_mut()[0].clips_mut()[0]
    .media_mut()
    .set_rate(Some(Rate::FPS_29_97));
  assert_eq!(
    source_range(&timeline),
    (1000.0, 1_700_000_000_123.0, 1000.0)
  );
}

#[test]
fn a_media_range_no_ruler_holds_is_refused() {
  // One nanosecond off a whole microsecond: no whole ruler coarser than its
  // own holds the start, and its own counts it past 2^53.
  let start = 1_700_000_000_123_456_789;
  let timeline = one_source(
    Rate::FPS_25,
    25,
    TimeRange::new(start, start + 1_000_000_000, Timebase::NANOS),
    None,
  );
  assert_eq!(
    not_representable(&timeline),
    (Spot::Source(ClipAt::new(0, 0)), i128::from(start))
  );
}

/// Every object inside `value` naming `schema`.
fn named<'a>(value: &'a Value, schema: &str, out: &mut Vec<&'a [(String, Value)]>) {
  match value {
    Value::Object(members) => {
      if members
        .iter()
        .any(|(key, name)| key == "OTIO_SCHEMA" && name.as_str() == Some(schema))
      {
        out.push(members);
      }
      for (_, member) in members {
        named(member, schema, out);
      }
    }
    Value::Array(items) => items.iter().for_each(|item| named(item, schema, out)),
    _ => {}
  }
}

/// The text a JSON number was written as.
fn spelled<'a>(members: &'a [(String, Value)], key: &str) -> &'a str {
  match members.iter().find(|(k, _)| k == key) {
    Some((_, Value::Number(text))) => text,
    other => panic!("{key}: {other:?}"),
  }
}

#[test]
fn every_count_in_the_goldens_is_one_an_f64_holds_exactly() {
  for golden in [
    include_str!("../../tests/golden/law.v0_15.otio"),
    include_str!("../../tests/golden/law.legacy.otio"),
  ] {
    let root = json::parse(golden).unwrap();
    let mut times = Vec::new();
    named(&root, "RationalTime.1", &mut times);
    // The start; per clip its source and available ranges; per gap its range;
    // per transition its two offsets.
    assert_eq!(times.len(), 1 + 3 * 4 + 2 * 2 + 3 * 2);
    for time in &times {
      // A whole count within ±2^53, written as `<digits>.0`, which an f64
      // reads back as exactly that count.
      let value = spelled(time, "value");
      let digits = value.strip_suffix(".0").unwrap();
      let count: i128 = digits.parse().unwrap();
      assert!(count.unsigned_abs() <= 1 << 53, "{value}");
      assert_eq!(value.parse::<f64>().unwrap() as i128, count, "{value}");
      // The rate spelled as the shortest text of one f64, which reads back as
      // that very f64.
      let rate = spelled(time, "rate");
      let read = rate.parse::<f64>().unwrap();
      assert!(read.is_finite() && read > 0.0, "{rate}");
      assert_eq!(format!("{read:?}"), rate);
    }
    let mut ranges = Vec::new();
    named(&root, "TimeRange.1", &mut ranges);
    assert_eq!(ranges.len(), 3 * 2 + 2);
    for range in ranges {
      let time = |key: &str| match range.iter().find(|(k, _)| k == key) {
        Some((_, Value::Object(members))) => members.as_slice(),
        other => panic!("{key}: {other:?}"),
      };
      let (start, duration) = (time("start_time"), time("duration"));
      // One ruler, so the end is the two counts added, within 2^53 too.
      assert_eq!(spelled(start, "rate"), spelled(duration, "rate"));
      let count = |members| -> i128 {
        spelled(members, "value")
          .strip_suffix(".0")
          .unwrap()
          .parse()
          .unwrap()
      };
      assert!((count(start) + count(duration)).unsigned_abs() <= 1 << 53);
    }
  }
}

#[test]
fn a_refusal_to_export_says_why() {
  let shown = |refused: Refused| alloc::string::ToString::to_string(&refused);
  assert_eq!(
    shown(Refused::NotRepresentable(NotRepresentable {
      at: Spot::Record(ClipAt::new(0, 0)),
      value: 9_007_199_254_740_993,
    })),
    "the record of track 0, clip 0 counts 9007199254740993 ticks: past 2^53, where \
     OpenTimelineIO's f64 no longer holds every whole number"
  );
  assert_eq!(
    shown(Refused::Validation(alloc::vec![
      Refusal::RateUnstated,
      Refusal::EmptyRecord(ClipAt::new(0, 1)),
    ])),
    "the timeline does not validate: the edit rate is zero; track 0, clip 1: the record covers \
     no time"
  );
  let spot = |at: Spot| alloc::string::ToString::to_string(&at);
  assert_eq!(spot(Spot::Start), "the start");
  assert_eq!(
    spot(Spot::Available(ClipAt::new(1, 2))),
    "the available range of track 1, clip 2"
  );
  assert_eq!(
    spot(Spot::Fade(EdgeAt::new(ClipAt::new(0, 3), crate::Edge::Out))),
    "the fade at the end of track 0, clip 3"
  );
  assert_eq!(
    spot(Spot::Transition(TransitionAt::new(0, 1))),
    "track 0, transition 1"
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

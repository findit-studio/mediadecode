//! The stored document: its wire names, its round trip, and what a reader
//! refuses by name.

use core::num::NonZeroI32;

use mediaio_timeline::{
  Clip, Duration, Fade, FadeShape, Fades, Gain, MediaRef, Metadata, Rate, Schema, TimeRange,
  Timebase, Timeline, Timestamp, Track, TrackKind, Transition,
};

fn tb(num: i32, den: i32) -> Timebase {
  Timebase::new(num, NonZeroI32::new(den).unwrap())
}

/// Reads a document from its text.
///
/// Through `serde_json::from_reader`, whose reader counts lines and columns
/// as it goes. `from_str` finds an error's line afterwards with `memchr`,
/// which aligns its reads by address: Miri's symbolic alignment check, run
/// by the CI's Miri lanes, cannot follow that and stops on the read (it did
/// on powerpc64, where `memchr` takes its portable word-at-a-time path).
/// These laws read errors, so they read without `memchr`.
fn read(text: &str) -> serde_json::Result<Timeline> {
  serde_json::from_reader(text.as_bytes())
}

/// Every word of the model, present once. The document need not validate:
/// it pins the wire names, not an edit.
fn sample() -> Timeline {
  let edit = tb(1001, 24_000);
  let media = tb(1, 24_000);
  Timeline::new("doc", Rate::FPS_23_976)
    .with_start(Timestamp::new(86_400, edit))
    .with_metadata(Metadata::new().with("project", "law"))
    .with_track(
      Track::new(TrackKind::Video, "V1")
        .with_clip(
          Clip::new(
            "a",
            MediaRef::new("file:///media/a.mov")
              .with_available_range(Some(TimeRange::new(0, 240_240, media)))
              .with_rate(Some(Rate::FPS_23_976))
              .with_reel(Some("A001".into())),
            TimeRange::new(24_024, 120_120, media),
            TimeRange::new(0, 96, edit),
          )
          .with_gain(Gain::from_db(-6.0))
          .with_fades(Fades::new().with_out(Some(Fade::new(
            Duration::new(24, edit),
            FadeShape::EqualPower,
          )))),
        )
        .with_transition(Transition::dissolve(
          Timestamp::new(96, edit),
          Duration::new(12, edit),
          Duration::new(12, edit),
        )),
    )
}

/// Predicted by hand from the field declarations before the first run.
const SAMPLE_PRETTY: &str = r#"{
  "schema": 1,
  "name": "doc",
  "rate": {
    "numerator": 24000,
    "denominator": 1001
  },
  "start": {
    "pts": 86400,
    "timebase": {
      "numerator": 1001,
      "denominator": 24000
    }
  },
  "metadata": {
    "project": "law"
  },
  "tracks": [
    {
      "kind": "video",
      "name": "V1",
      "enabled": true,
      "clips": [
        {
          "name": "a",
          "media": {
            "locator": "file:///media/a.mov",
            "available_range": {
              "start": 0,
              "end": 240240,
              "timebase": {
                "numerator": 1,
                "denominator": 24000
              }
            },
            "rate": {
              "numerator": 24000,
              "denominator": 1001
            },
            "reel": "A001"
          },
          "source_range": {
            "start": 24024,
            "end": 120120,
            "timebase": {
              "numerator": 1,
              "denominator": 24000
            }
          },
          "record": {
            "start": 0,
            "end": 96,
            "timebase": {
              "numerator": 1001,
              "denominator": 24000
            }
          },
          "enabled": true,
          "gain": -6.0,
          "fades": {
            "in": null,
            "out": {
              "duration": {
                "ticks": 24,
                "timebase": {
                  "numerator": 1001,
                  "denominator": 24000
                }
              },
              "shape": "equal_power"
            }
          },
          "metadata": {}
        }
      ],
      "transitions": [
        {
          "kind": "dissolve",
          "at": {
            "pts": 96,
            "timebase": {
              "numerator": 1001,
              "denominator": 24000
            }
          },
          "in_offset": {
            "ticks": 12,
            "timebase": {
              "numerator": 1001,
              "denominator": 24000
            }
          },
          "out_offset": {
            "ticks": 12,
            "timebase": {
              "numerator": 1001,
              "denominator": 24000
            }
          }
        }
      ]
    }
  ]
}"#;

#[test]
fn the_wire_names_are_stable() {
  assert_eq!(
    serde_json::to_string_pretty(&sample()).unwrap(),
    SAMPLE_PRETTY
  );
}

#[test]
fn a_document_round_trips() {
  let timeline = sample();
  let text = serde_json::to_string(&timeline).unwrap();
  let back = read(&text).unwrap();
  assert_eq!(back, timeline);
  assert_eq!(serde_json::to_string(&back).unwrap(), text);
}

#[test]
fn the_schema_is_the_first_field_written() {
  let text = serde_json::to_string(&sample()).unwrap();
  assert!(text.starts_with(r#"{"schema":1,"#), "{text}");
  assert_eq!(sample().schema(), Schema::V1);
}

#[test]
fn an_unknown_schema_is_refused_by_name_before_the_body_is_read() {
  // A later schema may reshape every field after its number: the refusal
  // names the schema, not the first field this reader cannot parse.
  let doc = r#"{"schema": 2, "name": 7, "tracks": "a shape schema 1 never had"}"#;
  let error = read(doc).unwrap_err().to_string();
  assert!(
    error.contains("unknown timeline schema 2: this reader knows schema 1"),
    "{error}"
  );
}

#[test]
fn a_word_this_reader_does_not_know_is_refused_by_name() {
  // `speed` is reserved for a later schema; schema 1 has no such word.
  let text = serde_json::to_string(&sample()).unwrap().replacen(
    r#""enabled":true,"gain""#,
    r#""enabled":true,"speed":2.0,"gain""#,
    1,
  );
  assert!(text.contains(r#""speed":2.0"#));
  let error = read(&text).unwrap_err().to_string();
  assert!(error.contains("unknown field `speed`"), "{error}");
}

#[test]
fn a_word_inside_a_record_this_reader_does_not_know_is_refused_by_name() {
  // A later schema's `speed` beside a record's `start` must not read as
  // schema 1 with the word dropped.
  let text = serde_json::to_string(&sample()).unwrap().replacen(
    r#""record":{"start":0,"#,
    r#""record":{"start":0,"speed":2.0,"#,
    1,
  );
  assert!(text.contains(r#""speed":2.0"#));
  let error = read(&text).unwrap_err().to_string();
  assert!(error.contains("unknown field `speed`"), "{error}");
}

/// Every time value the sample carries, by JSON pointer, its timebase among
/// them.
const TIME_VALUES: [&str; 18] = [
  "/rate",
  "/start",
  "/start/timebase",
  "/tracks/0/clips/0/media/available_range",
  "/tracks/0/clips/0/media/available_range/timebase",
  "/tracks/0/clips/0/media/rate",
  "/tracks/0/clips/0/source_range",
  "/tracks/0/clips/0/source_range/timebase",
  "/tracks/0/clips/0/record",
  "/tracks/0/clips/0/record/timebase",
  "/tracks/0/clips/0/fades/out/duration",
  "/tracks/0/clips/0/fades/out/duration/timebase",
  "/tracks/0/transitions/0/at",
  "/tracks/0/transitions/0/at/timebase",
  "/tracks/0/transitions/0/in_offset",
  "/tracks/0/transitions/0/in_offset/timebase",
  "/tracks/0/transitions/0/out_offset",
  "/tracks/0/transitions/0/out_offset/timebase",
];

#[test]
fn every_time_value_refuses_a_word_it_does_not_know() {
  for pointer in TIME_VALUES {
    let mut doc = serde_json::to_value(sample()).unwrap();
    doc
      .pointer_mut(pointer)
      .and_then(serde_json::Value::as_object_mut)
      .unwrap_or_else(|| panic!("{pointer} is no object"))
      .insert("speed".into(), serde_json::json!(2.0));
    let error = match serde_json::from_value::<Timeline>(doc) {
      Ok(_) => panic!("{pointer}: read with the word dropped"),
      Err(error) => error.to_string(),
    };
    assert!(
      error.contains("unknown field `speed`"),
      "{pointer}: {error}"
    );
  }
}

#[test]
fn a_time_value_reads_only_what_mediatime_could_hold() {
  let refused = |pointer: &str, key: &str, value: serde_json::Value| {
    let mut doc = serde_json::to_value(sample()).unwrap();
    doc.pointer_mut(pointer).unwrap()[key] = value;
    serde_json::from_value::<Timeline>(doc)
      .unwrap_err()
      .to_string()
  };
  for pointer in ["/rate", "/tracks/0/clips/0/record/timebase"] {
    let error = refused(pointer, "numerator", serde_json::json!(-1));
    assert!(
      error.contains("timebase numerator must not be negative"),
      "{pointer}: {error}"
    );
    let error = refused(pointer, "denominator", serde_json::json!(-24));
    assert!(
      error.contains("timebase denominator must be positive"),
      "{pointer}: {error}"
    );
    let error = refused(pointer, "denominator", serde_json::json!(0));
    assert!(error.contains("nonzero"), "{pointer}: {error}");
  }
  let error = refused("/tracks/0/clips/0/record", "end", serde_json::json!(-1));
  assert!(
    error.contains("time range end must not precede start"),
    "{error}"
  );
}

#[test]
fn a_required_word_left_out_is_refused_by_name() {
  let text =
    serde_json::to_string(&sample())
      .unwrap()
      .replacen(r#""enabled":true,"gain""#, r#""gain""#, 1);
  let error = read(&text).unwrap_err().to_string();
  assert!(error.contains("missing field `enabled`"), "{error}");
}

#[test]
fn an_optional_word_left_out_reads_as_absent() {
  let text = serde_json::to_string(&sample())
    .unwrap()
    .replacen(r#","reel":"A001""#, "", 1)
    .replacen(r#""gain":-6.0,"#, "", 1);
  let back = read(&text).unwrap();
  let clip = &back.tracks()[0].clips()[0];
  assert_eq!(clip.gain(), None);
  assert_eq!(clip.media().reel(), None);
  assert_eq!(clip.media().rate(), Some(Rate::FPS_23_976));
}

#[test]
fn a_metadata_key_named_twice_is_refused_by_name() {
  let text = serde_json::to_string(&sample()).unwrap().replacen(
    r#""metadata":{"project":"law"}"#,
    r#""metadata":{"project":"law","project":"other"}"#,
    1,
  );
  let error = read(&text).unwrap_err().to_string();
  assert!(
    error.contains("duplicate metadata key `project`"),
    "{error}"
  );
}

#[test]
fn a_gain_that_is_not_finite_is_refused() {
  // 1e300 has no f32: it reads as infinity, which no gain is.
  let text =
    serde_json::to_string(&sample())
      .unwrap()
      .replacen(r#""gain":-6.0"#, r#""gain":1e300"#, 1);
  let error = read(&text).unwrap_err().to_string();
  assert!(error.contains("gain inf dB is not finite"), "{error}");
}

#[test]
fn a_range_longer_than_i64_max_ticks_is_refused_by_the_reader() {
  for (range, path) in [
    ("record", "/tracks/0/clips/0/record"),
    ("source_range", "/tracks/0/clips/0/source_range"),
    ("available_range", "/tracks/0/clips/0/media/available_range"),
  ] {
    let mut doc = serde_json::to_value(sample()).unwrap();
    let end = doc.pointer(path).unwrap()["end"].clone();
    doc.pointer_mut(path).unwrap()["start"] = serde_json::json!(i64::MIN);
    let error = serde_json::from_value::<Timeline>(doc)
      .unwrap_err()
      .to_string();
    let named = format!(
      "time range [{}, {end}) runs longer than i64::MAX ticks",
      i64::MIN
    );
    assert!(error.contains(&named), "{range}: {error}");
  }
  // A range of exactly `i64::MAX` ticks reads.
  let mut doc = serde_json::to_value(sample()).unwrap();
  doc["tracks"][0]["clips"][0]["record"]["end"] = serde_json::json!(i64::MAX);
  let back: Timeline = serde_json::from_value(doc).unwrap();
  assert_eq!(back.tracks()[0].clips()[0].record().end_pts(), i64::MAX);
}

#[test]
fn a_gain_is_finite_by_construction() {
  assert_eq!(Gain::from_db(-6.0).map(Gain::db), Some(-6.0));
  assert_eq!(Gain::from_db(f32::NAN), None);
  assert_eq!(Gain::from_db(f32::INFINITY), None);
  assert_eq!(Gain::from_db(f32::NEG_INFINITY), None);
  assert_eq!(Gain::UNITY.db(), 0.0);
}

#[test]
fn metadata_iterates_in_key_order_whatever_the_insertion_order() {
  let metadata = Metadata::new().with("b", "2").with("a", "1");
  let entries: Vec<_> = metadata.iter().collect();
  assert_eq!(entries, [("a", "1"), ("b", "2")]);
  assert_eq!(metadata, [("a", "1"), ("b", "2")].into_iter().collect());
  assert_eq!(metadata.get("a"), Some("1"));
  assert_eq!(metadata.len(), 2);
}

#[test]
fn a_new_timeline_counts_its_start_at_the_edit_rate() {
  let timeline = Timeline::new("t", Rate::FPS_25);
  assert_eq!(timeline.start().timebase(), tb(1, 25));
  assert_eq!(timeline.edit_timebase(), Some(tb(1, 25)));
  // A rate of zero has no timebase; the start falls back to 1/1.
  let zero = Timeline::new("t", Rate::hz(0));
  assert_eq!(zero.edit_timebase(), None);
  assert_eq!(zero.start().timebase(), Timebase::default());
}

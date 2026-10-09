//! What OpenTimelineIO derives from an exported document: refused where its
//! arithmetic would round, written where it does not — and read back, in
//! that arithmetic written again here from OpenTimelineIO's C++, to prove
//! it.

use super::*;

/// OpenTimelineIO's `RationalTime` as it computes, for reading a document
/// back: `value_rescaled_to` (`opentime/rationalTime.h` 70–75), `operator+`
/// (316–326), `operator-` (329–339) and `TimeRange::end_time_exclusive`
/// (`timeRange.h` 105–108), at `main` `00c22fa`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Read {
  value: f64,
  rate: f64,
}

impl Read {
  fn at(self, rate: f64) -> f64 {
    if rate == self.rate {
      self.value
    } else {
      self.value * rate / self.rate
    }
  }

  fn plus(self, other: Self) -> Self {
    if self.rate < other.rate {
      Self {
        value: self.at(other.rate) + other.value,
        rate: other.rate,
      }
    } else {
      Self {
        value: other.at(self.rate) + self.value,
        rate: self.rate,
      }
    }
  }

  fn minus(self, other: Self) -> Self {
    if self.rate < other.rate {
      Self {
        value: self.at(other.rate) - other.value,
        rate: other.rate,
      }
    } else {
      Self {
        value: self.value - other.at(self.rate),
        rate: self.rate,
      }
    }
  }

  fn end(start: Self, duration: Self) -> Self {
    duration.plus(Self {
      value: start.at(duration.rate),
      rate: duration.rate,
    })
  }

  /// `TimeRange::end_time_inclusive` (`timeRange.h` 88–102): past one
  /// tick, the end floored where the duration is not whole, else the end
  /// less one tick; else the start.
  fn end_inclusive(start: Self, duration: Self) -> Self {
    let end = Self::end(start, duration);
    let start_there = Self {
      value: start.at(duration.rate),
      rate: duration.rate,
    };
    if end.minus(start_there).value > 1.0 {
      if duration.value != duration.value.floor() {
        Self {
          value: end.value.floor(),
          rate: end.rate,
        }
      } else {
        end.minus(Self {
          value: 1.0,
          rate: duration.rate,
        })
      }
    } else {
      start
    }
  }
}

/// A `RationalTime.1` object, read.
fn read(time: &Value) -> Read {
  Read {
    value: member(time, "value").as_f64().unwrap(),
    rate: member(time, "rate").as_f64().unwrap(),
  }
}

/// One child of a track, read: an item's source range, or a transition's
/// offsets.
#[derive(Debug, Clone, Copy)]
enum Kid {
  Item { start: Read, duration: Read },
  Transition { in_offset: Read, out_offset: Read },
}

impl Kid {
  fn duration(self) -> Read {
    match self {
      Self::Item { duration, .. } => duration,
      Self::Transition {
        in_offset,
        out_offset,
      } => in_offset.plus(out_offset),
    }
  }
}

/// Track `track` of `text`, its children read.
fn kids(text: &str, track: usize) -> Vec<Kid> {
  let root = json::parse(text).unwrap();
  let tracks = member(&member(&root, "tracks"), "children");
  let track = tracks.as_array().unwrap()[track].clone();
  member(&track, "children")
    .as_array()
    .unwrap()
    .iter()
    .map(
      |child| match member(child, "OTIO_SCHEMA").as_str().unwrap() {
        "Transition.1" => Kid::Transition {
          in_offset: read(&member(child, "in_offset")),
          out_offset: read(&member(child, "out_offset")),
        },
        _ => {
          let range = member(child, "source_range");
          Kid::Item {
            start: read(&member(&range, "start_time")),
            duration: read(&member(&range, "duration")),
          }
        }
      },
    )
    .collect()
}

/// `Track::range_of_child_at_index` (`track.cpp` 51–92): from zero in the
/// child's own rate, every item before it added; a transition starts its
/// `in_offset` earlier. The child's start and end.
fn place(kids: &[Kid], index: usize) -> (Read, Read) {
  let length = kids[index].duration();
  let mut start = Read {
    value: 0.0,
    rate: length.rate,
  };
  for kid in &kids[..index] {
    if let Kid::Item { duration, .. } = kid {
      start = start.plus(*duration);
    }
  }
  if let Kid::Transition { in_offset, .. } = kids[index] {
    start = start.minus(in_offset);
  }
  (start, Read::end(start, length))
}

/// `Track::available_range` (`track.cpp` 117–148): from zero at rate 1,
/// every item added.
fn track_duration(kids: &[Kid]) -> Read {
  kids.iter().fold(
    Read {
      value: 0.0,
      rate: 1.0,
    },
    |sum, kid| match kid {
      Kid::Item { duration, .. } => sum.plus(*duration),
      Kid::Transition { .. } => sum,
    },
  )
}

/// `Item::visible_range` (`item.cpp` 60–85): the source range widened by
/// the `in_offset` of a transition before the item and the `out_offset` of
/// one after it. Its start and duration.
fn visible_range(kids: &[Kid], index: usize) -> (Read, Read) {
  let Kid::Item {
    mut start,
    mut duration,
  } = kids[index]
  else {
    panic!("child {index} is no item");
  };
  if let Some(Kid::Transition { in_offset, .. }) = index.checked_sub(1).map(|before| kids[before]) {
    start = start.minus(in_offset);
    duration = duration.plus(in_offset);
  }
  if let Some(Kid::Transition { out_offset, .. }) = kids.get(index + 1) {
    duration = duration.plus(*out_offset);
  }
  (start, duration)
}

/// The visible range's start and end.
fn visible(kids: &[Kid], index: usize) -> (Read, Read) {
  let (start, duration) = visible_range(kids, index);
  (start, Read::end(start, duration))
}

/// A clip `id` playing `source` at `record`, of a medium with no stated
/// rate: its source range is written in its own timebase's ticks, or a
/// coarser whole ruler.
fn placed(id: &str, source: TimeRange, record: TimeRange) -> Clip {
  Clip::new(
    ClipId::new(id),
    id,
    MediaRef::new(format!("file:///{id}")),
    source,
    record,
  )
}

/// Why `to_otio` refuses `timeline`: where, the count and its ruler.
fn refused(timeline: &Timeline) -> (Spot, i128, Rate) {
  match to_otio(timeline, OtioTarget::V0_15Plus) {
    Err(Refused::NotRepresentable(count)) => (count.at(), count.value(), count.rate()),
    other => panic!("{other:?}"),
  }
}

fn video(clips: impl IntoIterator<Item = Clip>) -> Track {
  clips
    .into_iter()
    .fold(Track::new(TrackKind::Video, "V"), Track::with_clip)
}

fn second() -> Timebase {
  tb(1, 1)
}

/// Codex round 3's first case: a timeline at one frame a second, a gap of
/// 2^53 - 1 seconds, then a clip one second long whose medium counts thirds
/// of a second, from `third` thirds.
fn after_a_long_gap(third: i64) -> Timeline {
  Timeline::new("t", Rate::hz(1)).with_track(video([placed(
    "a",
    TimeRange::new(third, third + 3, tb(1, 3)),
    TimeRange::new(TWO_53 - 1, TWO_53, second()),
  )]))
}

#[test]
fn a_clip_whose_ruler_is_finer_than_the_track_before_it_is_placed_where_opentimelineio_places_it() {
  // OpenTimelineIO starts the clip's place at zero in the clip's own rate,
  // three a second, and adds the gap there: 3 · (2^53 - 1), which an f64
  // holds as one less — the clip a third of a second early. Every count
  // written is within 2^53. From a third past a second no whole ruler
  // coarser than thirds holds the clip: refused, at the clip, the track's
  // second child, with the count OpenTimelineIO would round.
  let thirds = 3 * (i128::from(TWO_53) - 1);
  assert_eq!(
    refused(&after_a_long_gap(4)),
    (Spot::TrackPosition(ChildAt::new(0, 1)), thirds, Rate::hz(3))
  );
  let gap = Read {
    value: (TWO_53 - 1) as f64,
    rate: 1.0,
  };
  let reads = Read {
    value: 0.0,
    rate: 3.0,
  }
  .plus(gap);
  assert_eq!(reads.value as i128, thirds - 1);
}

#[test]
fn a_clip_a_coarser_whole_ruler_holds_is_written_in_it_and_placed_exactly() {
  // From a whole second, whole seconds hold the clip: it is written in them,
  // the track is one ruler, and OpenTimelineIO places the clip at 2^53 - 1
  // and ends it at 2^53, exactly.
  let timeline = after_a_long_gap(3);
  let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let kids = kids(&text, 0);
  let Kid::Item { start, duration } = kids[1] else {
    panic!("{kids:?}");
  };
  assert_eq!(
    (start, duration),
    (
      Read {
        value: 1.0,
        rate: 1.0
      },
      Read {
        value: 1.0,
        rate: 1.0
      }
    )
  );
  let seconds = |value: i64| Read {
    value: value as f64,
    rate: 1.0,
  };
  assert_eq!(place(&kids, 1), (seconds(TWO_53 - 1), seconds(TWO_53)));
  assert_eq!(track_duration(&kids), seconds(TWO_53));
}

#[test]
fn a_mixed_rate_track_whose_derived_places_are_whole_is_written_and_read_back_exactly() {
  // At 25 fps: a 10-frame gap; `a` in frames of its 50 fps medium; `b` in
  // 48 kHz samples; another 10-frame gap; `c` in milliseconds. OpenTimelineIO
  // carries each place in the highest rate before it, where each count is
  // whole: every clip lands on its record, exactly, and the track ends at
  // its last record.
  let edit = tb(1, 25);
  let a = placed(
    "a",
    TimeRange::new(100, 200, tb(1, 50)),
    TimeRange::new(10, 60, edit),
  );
  let mut a = a;
  a.media_mut().set_rate(Some(Rate::FPS_50));
  let b = placed(
    "b",
    TimeRange::new(48_000, 144_000, tb(1, 48_000)),
    TimeRange::new(60, 110, edit),
  );
  let c = placed(
    "c",
    TimeRange::new(1_000, 3_000, Timebase::MILLIS),
    TimeRange::new(120, 170, edit),
  );
  let timeline = Timeline::new("t", Rate::FPS_25)
    .with_start(Timestamp::new(90_000, edit))
    .with_track(video([a, b, c]));
  let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
  let kids = kids(&text, 0);
  assert_eq!(kids.len(), 5);
  // Each clip's place, in the rate OpenTimelineIO carries it in: its record
  // counted there, with no remainder.
  for (child, rate, frames) in [(1, 50.0, 10), (2, 48_000.0, 60), (4, 48_000.0, 120)] {
    let (start, _) = place(&kids, child);
    assert_eq!(
      start,
      Read {
        value: frames as f64 * rate / 25.0,
        rate
      },
      "child {child}"
    );
  }
  assert_eq!(
    track_duration(&kids),
    Read {
      value: 170.0 * 1_920.0,
      rate: 48_000.0
    }
  );
}

#[test]
fn the_goldens_read_back_put_every_clip_on_its_record() {
  // The law timeline, mixed 23.976 fps frames and 48 kHz samples: read back
  // as OpenTimelineIO reads it, `b` starts at frame 96 and `a-sound` at
  // sample 48 048, frame 24 — exactly.
  for golden in [
    include_str!("../../../tests/golden/law.v0_15.otio"),
    include_str!("../../../tests/golden/law.legacy.otio"),
  ] {
    let ntsc = Rate::FPS_23_976.as_f64();
    let video = kids(golden, 0);
    assert_eq!(
      place(&video, 2).0,
      Read {
        value: 96.0,
        rate: ntsc
      }
    );
    let audio = kids(golden, 1);
    assert_eq!(
      place(&audio, 2).0,
      Read {
        value: 48_048.0,
        rate: 48_000.0
      }
    );
    assert_eq!(
      track_duration(&audio),
      Read {
        value: 240_240.0,
        rate: 48_000.0
      }
    );
  }
}

#[test]
fn a_count_opentimelineio_rescales_a_tick_off_is_refused_though_within_2_53() {
  // At 3 fps, `a` plays v thirds of a second, then `b`, whose medium counts
  // ninths. OpenTimelineIO places `b` at zero in ninths and adds `a` there:
  // v · 9 / 3, its product past 2^54, where an f64 steps by four — 9v
  // rounds by two, and `b` lands a ninth of a second early. Every count,
  // the product's quotient among them, is within 2^53.
  let v: i64 = 2_001_599_834_386_890;
  let timeline = Timeline::new("t", Rate::hz(3)).with_track(video([
    placed(
      "a",
      TimeRange::new(1, 1 + v, tb(1, 3)),
      TimeRange::new(0, v, tb(1, 3)),
    ),
    placed(
      "b",
      TimeRange::new(1, 10, tb(1, 9)),
      TimeRange::new(v, v + 3, tb(1, 3)),
    ),
  ]));
  assert_eq!(
    refused(&timeline),
    (
      Spot::TrackPosition(ChildAt::new(0, 1)),
      3 * i128::from(v),
      Rate::hz(9)
    )
  );
  let reads = Read {
    value: 0.0,
    rate: 9.0,
  }
  .plus(Read {
    value: v as f64,
    rate: 3.0,
  });
  assert_eq!(reads.value as i128, 3 * i128::from(v) - 1);
}

/// A clip `a` one second long in thirds of a second, from a third past a
/// second — no coarser whole ruler holds it — then a gap of `gap` seconds and
/// a clip `b` of `seconds`, at one frame a second from `start`.
fn thirds_then_seconds(gap: i64, seconds: i64, start: i64) -> Timeline {
  let at = 1 + gap;
  Timeline::new("t", Rate::hz(1))
    .with_start(Timestamp::new(start, second()))
    .with_track(video([
      placed(
        "a",
        TimeRange::new(4, 7, tb(1, 3)),
        TimeRange::new(0, 1, second()),
      ),
      placed(
        "b",
        TimeRange::new(0, seconds, second()),
        TimeRange::new(at, at + seconds, second()),
      ),
    ]))
}

#[test]
fn a_track_whose_duration_opentimelineio_sums_past_2_53_is_refused() {
  // `a` in thirds puts the track's sum in thirds: the places are within
  // 2^53 — `b` starts at 3 + 3·g thirds — but the track's duration adds
  // `b`'s five seconds as fifteen thirds, past it.
  let gap = 3_002_399_751_580_329;
  assert_eq!(
    refused(&thirds_then_seconds(gap, 5, 0)),
    (
      Spot::TrackDuration(0),
      3 * i128::from(1 + gap + 5),
      Rate::hz(3)
    )
  );
}

/// Track `0` of length `first` thirds of a second and track `1` of
/// `second`, at 3 fps.
fn two_tracks(first: i64, second: i64) -> Timeline {
  let third = tb(1, 3);
  let track = |id: &str, length: i64| {
    video([placed(
      id,
      TimeRange::new(0, length, third),
      TimeRange::new(0, length, third),
    )])
  };
  Timeline::new("t", Rate::hz(3))
    .with_track(track("a", first))
    .with_track(track("b", second))
}

#[test]
fn a_stack_whose_longest_track_opentimelineio_cannot_tell_from_a_shorter_one_is_refused() {
  // Two tracks a third of a second apart near 2^51 seconds, where an f64
  // steps by halves: OpenTimelineIO compares their durations as seconds,
  // reads both as the same, and keeps the first — the shorter one, so the
  // timeline would end a third of a second early. The other way round it
  // keeps the longer, and the export is written.
  let j = 1_i64 << 51;
  let (shorter, longer) = (3 * j + 1, 3 * j + 2);
  assert_eq!(shorter as f64 / 3.0, longer as f64 / 3.0);
  assert_eq!(
    refused(&two_tracks(shorter, longer)),
    (Spot::StackDuration, i128::from(longer), Rate::hz(3))
  );
  assert!(to_otio(&two_tracks(longer, shorter), OtioTarget::V0_15Plus).is_ok());
}

/// Codex round 3's second case: at one frame a second, `a` plays one second
/// of `outgoing` and `b` one second of `incoming`, with a dissolve between
/// them reaching `into` before the cut and `out` after it; no available
/// range is known, so validation holds no handle.
fn handles(outgoing: i64, incoming: i64, into: u64, out: u64) -> Timeline {
  let second = second();
  Timeline::new("t", Rate::hz(1)).with_track(
    video([
      placed(
        "a",
        TimeRange::new(outgoing, outgoing + 1, second),
        TimeRange::new(0, 1, second),
      ),
      placed(
        "b",
        TimeRange::new(incoming, incoming + 1, second),
        TimeRange::new(1, 2, second),
      ),
    ])
    .with_transition(Transition::dissolve(
      Timestamp::new(1, second),
      Duration::new(into, second),
      Duration::new(out, second),
    )),
  )
}

#[test]
fn a_visible_range_a_transitions_handle_widens_past_2_53_is_refused() {
  // The outgoing source [2^53 - 1, 2^53) plays on a second past its end:
  // OpenTimelineIO's visible range of `a` ends at 2^53 + 1, read as 2^53.
  let past = i128::from(TWO_53) + 1;
  assert_eq!(
    refused(&handles(TWO_53 - 1, 0, 0, 1)),
    (Spot::Visible(ChildAt::new(0, 0)), past, Rate::hz(1))
  );
  // The incoming source from -2^53 starts a second before it: -2^53 - 1.
  assert_eq!(
    refused(&handles(0, -TWO_53, 1, 0)),
    (Spot::Visible(ChildAt::new(0, 2)), -past, Rate::hz(1))
  );
}

#[test]
fn a_visible_range_inside_the_bound_is_written_and_read_back_exactly() {
  // One second earlier the handle ends `a`'s visible range at 2^53 itself,
  // and the incoming one starts at -2^53: both read back exactly.
  let text = to_otio(&handles(TWO_53 - 2, 0, 0, 1), OtioTarget::V0_15Plus).unwrap();
  let seconds = |value: i64| Read {
    value: value as f64,
    rate: 1.0,
  };
  assert_eq!(
    visible(&kids(&text, 0), 0),
    (seconds(TWO_53 - 2), seconds(TWO_53))
  );
  let text = to_otio(&handles(0, 1 - TWO_53, 1, 0), OtioTarget::V0_15Plus).unwrap();
  assert_eq!(
    visible(&kids(&text, 0), 2),
    (seconds(-TWO_53), seconds(2 - TWO_53))
  );
}

/// Codex round 3's third case: at one frame a second from `start`, one
/// clip one second long.
fn one_second_from(start: i64) -> Timeline {
  let second = second();
  Timeline::new("t", Rate::hz(1))
    .with_start(Timestamp::new(start, second))
    .with_track(video([placed(
      "a",
      TimeRange::new(0, 1, second),
      TimeRange::new(0, 1, second),
    )]))
}

#[test]
fn a_place_from_the_global_start_past_2_53_is_refused() {
  // From 2^53 the clip ends at 2^53 + 1 — the global start, written within
  // 2^53, plus the record, which an adapter adds up and rounds.
  assert_eq!(
    refused(&one_second_from(TWO_53)),
    (
      Spot::Absolute(ChildAt::new(0, 0)),
      i128::from(TWO_53) + 1,
      Rate::hz(1)
    )
  );
}

#[test]
fn a_global_start_whose_end_stays_within_2_53_is_written_and_read_back_exactly() {
  let text = to_otio(&one_second_from(TWO_53 - 1), OtioTarget::V0_15Plus).unwrap();
  let root = json::parse(&text).unwrap();
  let start = read(&member(&root, "global_start_time"));
  let kids = kids(&text, 0);
  let (_, ends) = place(&kids, 0);
  // `operator+`, and a range of the duration from the start, as toucan,
  // raven and the FCP XML adapter run one.
  let end = Read {
    value: TWO_53 as f64,
    rate: 1.0,
  };
  assert_eq!(start.plus(ends), end);
  assert_eq!(Read::end(start, track_duration(&kids)), end);
}

#[test]
fn a_tracks_end_from_the_global_start_past_2_53_is_refused() {
  // From one second, `a` in thirds then `b` until 2^53 - 2 thirds: every
  // place is within 2^53, and so is the track's duration — but a range of
  // it from the global start ends at 2^53 + 1 thirds.
  let seconds = 3_002_399_751_580_329;
  assert_eq!(
    refused(&thirds_then_seconds(0, seconds, 1)),
    (Spot::AbsoluteEnd(0), i128::from(TWO_53) + 1, Rate::hz(3))
  );
}

/// Codex round 4's first case: at 7 fps, `a`, then `b` — 360 287 970 189 200
/// frames of a 25 fps medium from frame 1 099 511 627 816 — then `c`, a
/// dissolve reaching `head` ticks into `a` before the cut into `b`, and one
/// reaching 1 099 511 627 815 ticks into `c` after the cut out of `b`. No
/// available range is known, so validation holds no handle; no clip has a
/// coarser ruler.
fn widened(head: u64) -> Timeline {
  let seven = tb(1, 7);
  let (a, c) = (1_125_899_906_843_277_i64, 1_099_511_627_815_i64);
  let (from, frames) = (1_099_511_627_816_i64, 360_287_970_189_200_i64);
  let b = frames * 7 / 25;
  Timeline::new("t", Rate::hz(7)).with_track(
    video([
      placed(
        "a",
        TimeRange::new(1, 1 + a, seven),
        TimeRange::new(0, a, seven),
      ),
      placed(
        "b",
        TimeRange::new(from, from + frames, tb(1, 25)),
        TimeRange::new(a, a + b, seven),
      ),
      placed(
        "c",
        TimeRange::new(1, 1 + c, seven),
        TimeRange::new(a + b, a + b + c, seven),
      ),
    ])
    .with_transition(Transition::dissolve(
      Timestamp::new(a, seven),
      Duration::new(head, seven),
      Duration::new(0, seven),
    ))
    .with_transition(Transition::dissolve(
      Timestamp::new(a + b, seven),
      Duration::new(0, seven),
      Duration::new(c.unsigned_abs(), seven),
    )),
  )
}

#[test]
fn a_last_tick_opentimelineio_derives_half_a_tick_off_is_refused() {
  // `b`'s visible range starts the first dissolve's reach before its source
  // and runs on through the second: OpenTimelineIO carries it in frames of
  // 25 fps, every sum rounded, its start, duration and end each within half
  // a frame of exact. But its duration rounds onto a whole number of frames,
  // which exactly it is not, so `end_time_inclusive` takes the end less one
  // frame instead of the end floored — half a frame early.
  let head = 1_125_899_906_843_277;
  assert_eq!(
    refused(&widened(head)),
    (
      Spot::Visible(ChildAt::new(0, 2)),
      365_314_309_059_212,
      Rate::hz(25)
    )
  );
  // OpenTimelineIO's arithmetic on the counts the export would write.
  let frames = |value: f64| Read { value, rate: 25.0 };
  let ticks = |value: f64| Read { value, rate: 7.0 };
  let start = frames(1_099_511_627_816.0).minus(ticks(head as f64));
  let duration = frames(360_287_970_189_200.0)
    .plus(ticks(head as f64))
    .plus(ticks(1_099_511_627_815.0));
  assert_eq!(duration.value, 4_385_285_893_300_243.0);
  assert_eq!(Read::end(start, duration).value, 365_314_309_059_212.5);
  assert_eq!(
    Read::end_inclusive(start, duration),
    frames(365_314_309_059_211.5)
  );
  // Exactly, the reach before the cut cancels: the range ends at
  // (7 · (from + frames) + 25 · out) / 7 frames, and its duration is no
  // whole number of frames, so its last is that end floored.
  let end = 7 * (1_099_511_627_816_i128 + 360_287_970_189_200) + 25 * 1_099_511_627_815;
  assert_eq!(end, 2_557_200_163_414_487);
  assert_eq!(end.div_euclid(7), 365_314_309_059_212);
}

#[test]
fn a_last_tick_one_tick_inside_is_written_and_read_back_exactly() {
  // A tick less reach before the cut, and OpenTimelineIO's visible duration
  // of `b` is no whole number of frames either: it floors the end, onto the
  // exact last frame.
  let text = to_otio(&widened(1_125_899_906_843_276), OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let (start, duration) = visible_range(&kids(&text, 0), 2);
  assert_eq!(duration.value, 4_385_285_893_300_239.5);
  assert_eq!(
    Read::end_inclusive(start, duration),
    Read {
      value: 365_314_309_059_212.0,
      rate: 25.0
    }
  );
}

/// The generator's seed 132, its track 1, alone: at 29.97 fps from frame
/// 106 560, an 18-frame gap, `a` in 48 kHz samples, a dissolve, then `b` in
/// ticks of 1/90 000 s — neither with a coarser ruler.
fn from_frame_106_560() -> Timeline {
  let edit = tb(1001, 30_000);
  Timeline::new("t", Rate::FPS_29_97)
    .with_start(Timestamp::new(106_560, edit))
    .with_track(
      video([
        placed(
          "a",
          TimeRange::new(69, 136_205, tb(1, 48_000)),
          TimeRange::new(18, 103, edit),
        ),
        placed(
          "b",
          TimeRange::new(45_196, 132_283, tb(1, 90_000)),
          TimeRange::new(103, 132, edit),
        ),
      ])
      .with_transition(Transition::dissolve(
        Timestamp::new(103, edit),
        Duration::new(15, edit),
        Duration::new(9, edit),
      )),
    )
}

#[test]
fn a_tracks_last_tick_from_the_global_start_opentimelineio_reads_late_is_refused() {
  // Exactly the track is 396 396 ticks of 1/90 000 s — a frame is 3003 of
  // them — but OpenTimelineIO adds its frames up in samples first, where
  // they are no whole number, and lands an ulp short of whole. So its range
  // from the global start, as OpenTimelineIO's tools form one, ends
  // inclusively at its end floored — and that end rounds up onto
  // 320 396 076, a tick past the last.
  assert_eq!(
    refused(&from_frame_106_560()),
    (Spot::AbsoluteEnd(0), 320_396_075, Rate::hz(90_000))
  );
  let frames = |value: f64| Read {
    value,
    rate: Rate::FPS_29_97.as_f64(),
  };
  let duration = [
    frames(18.0),
    Read {
      value: 136_136.0,
      rate: 48_000.0,
    },
    Read {
      value: 87_087.0,
      rate: 90_000.0,
    },
  ]
  .into_iter()
  .fold(
    Read {
      value: 0.0,
      rate: 1.0,
    },
    Read::plus,
  );
  assert_eq!(duration.value, f64::from_bits(396_396.0_f64.to_bits() - 1));
  assert_eq!(
    Read::end_inclusive(frames(106_560.0), duration),
    Read {
      value: 320_396_076.0,
      rate: 90_000.0
    }
  );
  // Exactly: from 106 560 · 3003 ticks, 396 396 long, a whole number —
  // the end less one.
  assert_eq!(106_560 * 3003 + 396_396 - 1, 320_396_075);
}

/// Codex round 4's `v`: thirds of a second, its triple a whole number of
/// ninths within 2^53, its ninefold past 2^54.
const V: i64 = 2_001_599_834_386_890;

/// Codex round 4's second case and its kin: at 3 fps, `a` plays `V` thirds
/// of a second from a third of a second into a medium counted in `a_rate`
/// ticks a second, then `b` a third of a second from a ninth into one
/// counted in `b_rate`.
fn shared(a_rate: i32, b_rate: i32) -> Timeline {
  let (a, b, third) = (i64::from(a_rate), i64::from(b_rate), tb(1, 3));
  Timeline::new("t", Rate::hz(3)).with_track(video([
    placed(
      "a",
      TimeRange::new(a / 3, a / 3 + V * a / 3, tb(1, a_rate)),
      TimeRange::new(0, V, third),
    ),
    placed(
      "b",
      TimeRange::new(b / 9, b / 9 + b / 3, tb(1, b_rate)),
      TimeRange::new(V, V + 1, third),
    ),
  ]))
}

/// `shared`'s track exported and read back: both clips written in ninths
/// of a second, and each where its record puts it, exactly.
fn in_ninths(timeline: &Timeline) {
  let text = to_otio(timeline, OtioTarget::V0_15Plus).unwrap();
  assert_eq!(validate_json(&text, OtioTarget::V0_15Plus), Ok(()));
  let kids = kids(&text, 0);
  let ninths = |value: i64| Read {
    value: value as f64,
    rate: 9.0,
  };
  let written: Vec<(Read, Read)> = kids
    .iter()
    .map(|kid| match *kid {
      Kid::Item { start, duration } => (start, duration),
      Kid::Transition { .. } => panic!("{kids:?}"),
    })
    .collect();
  assert_eq!(
    written,
    [(ninths(3), ninths(3 * V)), (ninths(1), ninths(3))]
  );
  assert_eq!(place(&kids, 1), (ninths(3 * V), ninths(3 * V + 3)));
  assert_eq!(track_duration(&kids), ninths(3 * V + 3));
}

#[test]
fn clips_held_only_in_a_ruler_they_share_are_written_in_it() {
  // Codex round 4's case: `a` in twelfths, `b` in 27ths. OpenTimelineIO
  // carries `b`'s place in 27ths, 9v, past 2^53; in their coarsest rulers,
  // thirds and ninths, it rescales v thirds into ninths as v · 9 / 3, a
  // product past 2^54 that rounds: a ninth short. Ninths hold both, and
  // `a` from a third of a second is a whole number of them too.
  in_ninths(&shared(12, 27));
}

#[test]
fn a_clip_only_a_ruler_between_its_own_and_its_coarsest_holds_is_written_in_it() {
  // `b` in ninths from the start: `a` in its own twelfths carries `b`'s end
  // through 36v / 12, in sixths and in its coarsest, thirds, through 18v / 6
  // and 9v / 3 — products past 2^54 that round. Only ninths, between its
  // own ruler and its coarsest, hold it.
  in_ninths(&shared(12, 9));
}

#[test]
fn a_clip_is_written_coarser_while_its_neighbour_keeps_its_own_ruler() {
  // `a` in ninths, `b` in 27ths: `b` in its coarsest, ninths, holds, and
  // `a` keeps its own ninths — written in its coarsest, thirds, too, it
  // would carry `b`'s place through 9v / 3 again.
  in_ninths(&shared(9, 27));
}

#[test]
fn a_track_no_ruler_holds_is_refused_with_the_last_plan_the_search_walked() {
  // At 1 fps, a gap of 2^53 seconds, then `a`, one second of a medium
  // counted in halves. In halves OpenTimelineIO places `a` 2^54 halves in,
  // past 2^53; in seconds, its coarsest ruler and its last, it starts at
  // 2^53 and ends past it — the refusal the search ends on.
  let timeline = Timeline::new("t", Rate::hz(1)).with_track(video([placed(
    "a",
    TimeRange::new(0, 2, tb(1, 2)),
    TimeRange::new(TWO_53, TWO_53 + 1, second()),
  )]));
  assert_eq!(
    refused(&timeline),
    (
      Spot::TrackPosition(ChildAt::new(0, 1)),
      i128::from(TWO_53) + 1,
      Rate::hz(1)
    )
  );
}

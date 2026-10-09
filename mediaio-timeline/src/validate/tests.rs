//! A law per refusal: a valid baseline, one defect planted in it, and
//! exactly that defect's refusal back.

use core::num::NonZeroI32;

use super::*;
use crate::{
  Clip, Fade, FadeShape, Fades, MediaRef, Rate, TimeRange, Timeline, Track, TrackKind, Transition,
};

fn tb(num: i32, den: i32) -> Timebase {
  Timebase::new(num, NonZeroI32::new(den).unwrap())
}

fn edit() -> Timebase {
  tb(1, 24)
}

fn frames(n: u64) -> Duration {
  Duration::new(n, edit())
}

fn cut(n: i64) -> Timestamp {
  Timestamp::new(n, edit())
}

fn rec(start: i64, end: i64) -> TimeRange {
  TimeRange::new(start, end, edit())
}

fn fade(n: u64) -> Option<Fade> {
  Some(Fade::new(frames(n), FadeShape::Linear))
}

/// Two tracks. V1: `a` [0, 48) and `b` [48, 96) with a dissolve of 6 + 6
/// frames at 48, both media holding ten seconds at 24 fps. A1: `m` [24, 72),
/// two seconds of 48 kHz sound with 12-frame fades at both edges.
fn baseline() -> Timeline {
  let video = |name: &str| {
    MediaRef::new(alloc::format!("file:///{name}.mov"))
      .with_available_range(Some(TimeRange::new(0, 240, edit())))
      .with_rate(Some(Rate::FPS_24))
  };
  Timeline::new("law", Rate::FPS_24)
    .with_track(
      Track::new(TrackKind::Video, "V1")
        .with_clip(Clip::new(
          "a",
          video("a"),
          TimeRange::new(24, 72, edit()),
          rec(0, 48),
        ))
        .with_clip(Clip::new(
          "b",
          video("b"),
          TimeRange::new(48, 96, edit()),
          rec(48, 96),
        ))
        .with_transition(Transition::dissolve(cut(48), frames(6), frames(6))),
    )
    .with_track(
      Track::new(TrackKind::Audio, "A1").with_clip(
        Clip::new(
          "m",
          MediaRef::new("file:///m.wav")
            .with_available_range(Some(TimeRange::new(0, 480_000, tb(1, 48_000))))
            .with_rate(Some(Rate::hz(48_000))),
          TimeRange::new(48_000, 144_000, tb(1, 48_000)),
          rec(24, 72),
        )
        .with_fades(Fades::new().with_in(fade(12)).with_out(fade(12))),
      ),
    )
}

fn clip_mut(timeline: &mut Timeline, track: usize, clip: usize) -> &mut Clip {
  &mut timeline.tracks_mut()[track].clips_mut()[clip]
}

fn refusals(timeline: &Timeline) -> Vec<Refusal> {
  validate(timeline).err().unwrap_or_default()
}

#[test]
fn the_baseline_is_valid() {
  assert_eq!(validate(&baseline()), Ok(()));
}

#[test]
fn a_rate_of_zero_is_refused() {
  let mut timeline = baseline();
  timeline.set_rate(Rate::hz(0));
  assert_eq!(refusals(&timeline), [Refusal::RateUnstated]);
}

#[test]
fn a_start_off_the_edit_rate_is_refused() {
  let timeline = baseline().with_start(Timestamp::new(0, Timebase::MILLIS));
  assert_eq!(refusals(&timeline), [Refusal::OffEditRate(Place::Start)]);
}

#[test]
fn a_record_off_the_edit_rate_is_refused() {
  // The same stretch of time, counted in half-frames.
  let mut timeline = baseline();
  clip_mut(&mut timeline, 0, 0).set_record(TimeRange::new(0, 96, tb(1, 48)));
  assert_eq!(
    refusals(&timeline),
    [Refusal::OffEditRate(Place::Record(ClipAt::new(0, 0)))]
  );
}

#[test]
fn a_fade_off_the_edit_rate_is_refused() {
  let mut timeline = baseline();
  let fades = Fades::new()
    .with_in(Some(Fade::new(
      Duration::new(24, tb(1, 48)),
      FadeShape::Linear,
    )))
    .with_out(fade(12));
  clip_mut(&mut timeline, 1, 0).set_fades(fades);
  assert_eq!(
    refusals(&timeline),
    [Refusal::OffEditRate(Place::Fade(EdgeAt::new(
      ClipAt::new(1, 0),
      Edge::In
    )))]
  );
}

#[test]
fn a_transition_off_the_edit_rate_is_refused() {
  let mut timeline = baseline();
  timeline.tracks_mut()[0].transitions_mut()[0] =
    Transition::dissolve(cut(48), Duration::new(12, tb(1, 48)), frames(6));
  assert_eq!(
    refusals(&timeline),
    [Refusal::OffEditRate(Place::Transition(TransitionAt::new(
      0, 0
    )))]
  );
}

#[test]
fn a_media_rate_of_zero_is_refused() {
  let mut timeline = baseline();
  clip_mut(&mut timeline, 0, 0)
    .media_mut()
    .set_rate(Some(Rate::hz(0)));
  assert_eq!(
    refusals(&timeline),
    [Refusal::MediaRateUnstated(ClipAt::new(0, 0))]
  );
}

#[test]
fn a_source_range_in_a_degenerate_timebase_is_refused() {
  let mut timeline = baseline();
  clip_mut(&mut timeline, 1, 0).set_source_range(TimeRange::new(48_000, 144_000, tb(0, 48_000)));
  assert_eq!(
    refusals(&timeline),
    [Refusal::MediaRateUnstated(ClipAt::new(1, 0))]
  );
}

#[test]
fn a_record_before_the_zero_is_refused() {
  let mut timeline = baseline();
  clip_mut(&mut timeline, 1, 0).set_record(rec(-24, 24));
  assert_eq!(
    refusals(&timeline),
    [Refusal::RecordBeforeZero(ClipAt::new(1, 0))]
  );
}

#[test]
fn an_empty_record_is_refused() {
  let mut timeline = baseline();
  timeline.tracks_mut()[1].clips_mut().push(Clip::new(
    "e",
    MediaRef::new("file:///e.wav"),
    TimeRange::new(0, 0, tb(1, 48_000)),
    rec(100, 100),
  ));
  assert_eq!(
    refusals(&timeline),
    [Refusal::EmptyRecord(ClipAt::new(1, 1))]
  );
}

#[test]
fn clips_out_of_record_order_are_refused() {
  let mut timeline = baseline();
  timeline.tracks_mut()[0].clips_mut().swap(0, 1);
  assert_eq!(
    refusals(&timeline),
    [Refusal::OutOfOrder(ClipPair::new(0, 0, 1))]
  );
}

#[test]
fn overlapping_records_are_refused() {
  let mut timeline = baseline();
  timeline.tracks_mut()[1].clips_mut().push(Clip::new(
    "n",
    MediaRef::new("file:///n.mov"),
    TimeRange::new(0, 24, edit()),
    rec(60, 84),
  ));
  assert_eq!(
    refusals(&timeline),
    [Refusal::Overlap(ClipPair::new(1, 0, 1))]
  );
}

#[test]
fn an_overlap_with_a_record_further_back_than_the_last_is_refused() {
  // `long` reaches past `short`, so `late` overlaps `long` although it
  // clears `short`, the clip just before it.
  let mut timeline = Timeline::new("t", Rate::FPS_24);
  let clip = |name: &str, start: i64, end: i64| {
    Clip::new(
      name,
      MediaRef::new(name),
      TimeRange::new(0, end - start, edit()),
      rec(start, end),
    )
  };
  timeline.tracks_mut().push(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("long", 0, 100))
      .with_clip(clip("short", 10, 20))
      .with_clip(clip("late", 30, 40)),
  );
  assert_eq!(
    refusals(&timeline),
    [
      Refusal::Overlap(ClipPair::new(0, 0, 1)),
      Refusal::Overlap(ClipPair::new(0, 0, 2)),
    ]
  );
}

#[test]
fn a_record_longer_than_its_source_is_refused() {
  let mut timeline = baseline();
  clip_mut(&mut timeline, 0, 0).set_source_range(TimeRange::new(24, 73, edit()));
  let [Refusal::DurationMismatch(mismatch)] = refusals(&timeline)[..] else {
    panic!("{:?}", refusals(&timeline));
  };
  assert_eq!(mismatch.clip(), ClipAt::new(0, 0));
  assert_eq!(mismatch.expected(), Some(frames(49)));
  assert_eq!(mismatch.found(), frames(48));
}

/// One clip playing `source` at `record`, on a 24 fps timeline.
fn one_clip(source: TimeRange, record: TimeRange) -> Timeline {
  Timeline::new("t", Rate::FPS_24).with_track(
    Track::new(TrackKind::Audio, "A").with_clip(Clip::new(
      "s",
      MediaRef::new("s.wav"),
      source,
      record,
    )),
  )
}

#[test]
fn a_source_off_the_edit_rate_is_refused_whatever_its_record() {
  // One tick at 48 per second is half a frame at 24 fps: no record holds it
  // exactly, and with no time-warp nothing may round it to one frame.
  for record in [rec(0, 1), rec(0, 0), rec(0, 2)] {
    let refused = refusals(&one_clip(TimeRange::new(0, 1, tb(1, 48)), record));
    assert!(
      refused.contains(&Refusal::SourceOffEditRate(ClipAt::new(0, 0))),
      "{record:?}: {refused:?}"
    );
    assert!(
      !refused
        .iter()
        .any(|refusal| matches!(refusal, Refusal::DurationMismatch(_))),
      "{refused:?}"
    );
  }
  // 2002 samples at 48 kHz are 1.001 frames, 3000 are 1.5: neither is a
  // whole frame, and neither rounds to one.
  for samples in [2002, 3000] {
    assert_eq!(
      refusals(&one_clip(
        TimeRange::new(0, samples, tb(1, 48_000)),
        rec(0, 1)
      )),
      [Refusal::SourceOffEditRate(ClipAt::new(0, 0))]
    );
  }
  // 2000 samples are one frame exactly.
  assert_eq!(
    validate(&one_clip(TimeRange::new(0, 2000, tb(1, 48_000)), rec(0, 1))),
    Ok(())
  );
}

#[test]
fn a_source_outside_its_available_range_is_refused() {
  let mut timeline = baseline();
  clip_mut(&mut timeline, 0, 0)
    .media_mut()
    .set_available_range(Some(TimeRange::new(30, 240, edit())));
  assert_eq!(
    refusals(&timeline),
    [Refusal::OutsideAvailable(ClipAt::new(0, 0))]
  );
}

#[test]
fn a_source_with_no_available_range_is_taken_at_its_word() {
  let mut timeline = baseline();
  clip_mut(&mut timeline, 0, 0)
    .media_mut()
    .set_available_range(None);
  assert_eq!(validate(&timeline), Ok(()));
}

#[test]
fn a_transition_off_a_cut_is_refused() {
  let mut timeline = baseline();
  timeline.tracks_mut()[0].transitions_mut()[0] =
    Transition::dissolve(cut(47), frames(6), frames(6));
  assert_eq!(
    refusals(&timeline),
    [Refusal::OffBoundary(TransitionAt::new(0, 0))]
  );
}

#[test]
fn a_second_transition_at_one_cut_is_refused() {
  let mut timeline = baseline();
  let again = timeline.tracks()[0].transitions()[0];
  timeline.tracks_mut()[0].transitions_mut().push(again);
  assert_eq!(
    refusals(&timeline),
    [Refusal::DuplicateTransition(TransitionAt::new(0, 1))]
  );
}

#[test]
fn an_outgoing_clip_without_its_tail_handle_is_refused() {
  // `a` plays to 72 and the dissolve needs 6 more: 78 > 75.
  let mut timeline = baseline();
  clip_mut(&mut timeline, 0, 0)
    .media_mut()
    .set_available_range(Some(TimeRange::new(0, 75, edit())));
  assert_eq!(
    refusals(&timeline),
    [Refusal::HandleMissing(EdgeAt::new(
      ClipAt::new(0, 0),
      Edge::Out
    ))]
  );
}

#[test]
fn an_incoming_clip_without_its_head_handle_is_refused() {
  // `b` plays from 48 and the dissolve needs 6 before: 42 < 45.
  let mut timeline = baseline();
  clip_mut(&mut timeline, 0, 1)
    .media_mut()
    .set_available_range(Some(TimeRange::new(45, 240, edit())));
  assert_eq!(
    refusals(&timeline),
    [Refusal::HandleMissing(EdgeAt::new(
      ClipAt::new(0, 1),
      Edge::In
    ))]
  );
}

#[test]
fn handles_are_compared_exactly_across_timebases() {
  // The tail handle needs 72 + 6 = 78 frames: 3.25 s, which is 156 000
  // ticks at 48 kHz exactly. One tick short is refused.
  let mut timeline = baseline();
  let a = clip_mut(&mut timeline, 0, 0);
  a.media_mut()
    .set_available_range(Some(TimeRange::new(0, 156_000, tb(1, 48_000))));
  assert_eq!(validate(&timeline), Ok(()));
  clip_mut(&mut timeline, 0, 0)
    .media_mut()
    .set_available_range(Some(TimeRange::new(0, 155_999, tb(1, 48_000))));
  assert_eq!(
    refusals(&timeline),
    [Refusal::HandleMissing(EdgeAt::new(
      ClipAt::new(0, 0),
      Edge::Out
    ))]
  );
}

#[test]
fn a_fade_where_a_transition_blends_is_refused() {
  let mut timeline = baseline();
  clip_mut(&mut timeline, 0, 1).set_fades(Fades::new().with_in(fade(6)));
  assert_eq!(
    refusals(&timeline),
    [Refusal::FadeMeetsTransition(EdgeAt::new(
      ClipAt::new(0, 1),
      Edge::In
    ))]
  );
}

#[test]
fn a_fade_out_where_a_transition_blends_is_refused() {
  let mut timeline = baseline();
  clip_mut(&mut timeline, 0, 0).set_fades(Fades::new().with_out(fade(6)));
  assert_eq!(
    refusals(&timeline),
    [Refusal::FadeMeetsTransition(EdgeAt::new(
      ClipAt::new(0, 0),
      Edge::Out
    ))]
  );
}

#[test]
fn a_fade_longer_than_its_clip_is_refused() {
  let mut timeline = baseline();
  clip_mut(&mut timeline, 1, 0).set_fades(Fades::new().with_in(fade(49)));
  assert_eq!(
    refusals(&timeline),
    [Refusal::BlendsOverrunClip(ClipAt::new(1, 0))]
  );
}

#[test]
fn fades_that_cross_inside_their_clip_are_refused() {
  // 30 + 30 frames of fade in a 48-frame clip; 24 + 24 just fit.
  let mut timeline = baseline();
  clip_mut(&mut timeline, 1, 0).set_fades(Fades::new().with_in(fade(24)).with_out(fade(24)));
  assert_eq!(validate(&timeline), Ok(()));
  clip_mut(&mut timeline, 1, 0).set_fades(Fades::new().with_in(fade(30)).with_out(fade(30)));
  assert_eq!(
    refusals(&timeline),
    [Refusal::BlendsOverrunClip(ClipAt::new(1, 0))]
  );
}

#[test]
fn a_transition_reaching_past_its_outgoing_clip_is_refused() {
  // The dissolve starts 50 frames before the cut, in a 48-frame clip. `b`
  // starts its source at 60, so its 50-frame head handle is there.
  let mut timeline = baseline();
  clip_mut(&mut timeline, 0, 1).set_source_range(TimeRange::new(60, 108, edit()));
  timeline.tracks_mut()[0].transitions_mut()[0] =
    Transition::dissolve(cut(48), frames(50), frames(6));
  assert_eq!(
    refusals(&timeline),
    [Refusal::BlendsOverrunClip(ClipAt::new(0, 0))]
  );
}

#[test]
fn every_refusal_comes_back_at_once_in_the_documented_order() {
  let mut timeline = baseline().with_start(Timestamp::new(0, Timebase::MILLIS));
  clip_mut(&mut timeline, 0, 0)
    .media_mut()
    .set_available_range(Some(TimeRange::new(30, 75, edit())));
  clip_mut(&mut timeline, 1, 0).set_fades(Fades::new().with_in(fade(49)));
  assert_eq!(
    refusals(&timeline),
    [
      Refusal::OffEditRate(Place::Start),
      Refusal::OutsideAvailable(ClipAt::new(0, 0)),
      Refusal::HandleMissing(EdgeAt::new(ClipAt::new(0, 0), Edge::Out)),
      Refusal::BlendsOverrunClip(ClipAt::new(1, 0)),
    ]
  );
}

#[test]
fn refusals_name_what_they_refuse() {
  let shown = |refusal: Refusal| alloc::string::ToString::to_string(&refusal);
  assert_eq!(shown(Refusal::RateUnstated), "the edit rate is zero");
  assert_eq!(
    shown(Refusal::SourceOffEditRate(ClipAt::new(0, 2))),
    "track 0, clip 2: the source range's length is no whole number of edit-rate ticks"
  );
  assert_eq!(
    shown(Refusal::Overlap(ClipPair::new(1, 0, 2))),
    "track 1: clip 2's record overlaps clip 0's"
  );
  assert_eq!(
    shown(Refusal::HandleMissing(EdgeAt::new(
      ClipAt::new(0, 1),
      Edge::In
    ))),
    "track 0, clip 1: the medium holds no handle for the transition at its start"
  );
  assert_eq!(
    shown(Refusal::OffEditRate(Place::Fade(EdgeAt::new(
      ClipAt::new(1, 0),
      Edge::Out
    )))),
    "the fade at the end of track 1, clip 0 is not counted at the edit rate"
  );
}

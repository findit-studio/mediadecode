use core::num::NonZeroI32;

use super::*;
use crate::{
  ClipPair, Duration, Fade, FadeShape, Gain, MediaRef, Rate, Timebase, Track, TrackKind,
};

fn edit() -> Timebase {
  Timebase::new(1, NonZeroI32::new(25).unwrap())
}

fn clip(name: &str, start: i64, end: i64) -> Clip {
  Clip::new(
    name,
    MediaRef::new(alloc::format!("file:///{name}.mov")),
    TimeRange::new(100, 100 + end - start, edit()),
    TimeRange::new(start, end, edit()),
  )
}

/// V: `a` [0, 10), `b` [10, 20), `c` [30, 40). A: `d` [0, 20).
fn before() -> Timeline {
  Timeline::new("t", Rate::FPS_25)
    .with_track(
      Track::new(TrackKind::Video, "V")
        .with_clip(clip("a", 0, 10))
        .with_clip(clip("b", 10, 20))
        .with_clip(clip("c", 30, 40)),
    )
    .with_track(Track::new(TrackKind::Audio, "A").with_clip(clip("d", 0, 20)))
}

fn clip_mut(timeline: &mut Timeline, track: usize, clip: usize) -> &mut Clip {
  &mut timeline.tracks_mut()[track].clips_mut()[clip]
}

fn kinds(delta: &Delta) -> Vec<(usize, ChangeKind, Option<usize>, Option<usize>)> {
  delta
    .changes()
    .iter()
    .map(|change| {
      (
        change.track(),
        change.kind(),
        change.before(),
        change.after(),
      )
    })
    .collect()
}

#[test]
fn a_timeline_against_itself_has_no_change() {
  let timeline = before();
  assert!(diff(&timeline, &timeline).unwrap().is_empty());
}

#[test]
fn a_new_clip_is_added() {
  let mut after = before();
  after.tracks_mut()[0].clips_mut().push(clip("e", 50, 60));
  assert_eq!(
    kinds(&diff(&before(), &after).unwrap()),
    [(0, ChangeKind::Added, None, Some(3))]
  );
}

#[test]
fn a_clip_gone_is_removed() {
  let mut after = before();
  after.tracks_mut()[0].clips_mut().remove(1);
  assert_eq!(
    kinds(&diff(&before(), &after).unwrap()),
    [(0, ChangeKind::Removed, Some(1), None)]
  );
}

#[test]
fn a_clip_whose_record_changed_is_moved() {
  let mut after = before();
  clip_mut(&mut after, 0, 2).set_record(TimeRange::new(25, 35, edit()));
  assert_eq!(
    kinds(&diff(&before(), &after).unwrap()),
    [(0, ChangeKind::Moved, Some(2), Some(2))]
  );
}

#[test]
fn a_clip_whose_source_changed_is_retimed() {
  let mut after = before();
  clip_mut(&mut after, 0, 0).set_source_range(TimeRange::new(110, 120, edit()));
  assert_eq!(
    kinds(&diff(&before(), &after).unwrap()),
    [(0, ChangeKind::Retimed, Some(0), Some(0))]
  );
}

#[test]
fn a_clip_whose_gain_or_fades_changed_is_regained() {
  let mut after = before();
  clip_mut(&mut after, 1, 0).set_gain(Gain::from_db(-3.0));
  clip_mut(&mut after, 0, 1)
    .set_fades(Fades::new().with_out(Some(Fade::new(Duration::new(5, edit()), FadeShape::Linear))));
  assert_eq!(
    kinds(&diff(&before(), &after).unwrap()),
    [
      (0, ChangeKind::Regained, Some(1), Some(1)),
      (1, ChangeKind::Regained, Some(0), Some(0)),
    ]
  );
  // A fade's curve is part of it.
  let mut curved = after.clone();
  clip_mut(&mut curved, 0, 1).set_fades(Fades::new().with_out(Some(Fade::new(
    Duration::new(5, edit()),
    FadeShape::EqualPower,
  ))));
  assert_eq!(
    kinds(&diff(&after, &curved).unwrap()),
    [(0, ChangeKind::Regained, Some(1), Some(1))]
  );
}

#[test]
fn a_clip_switched_off_or_on_is_enabled_flipped() {
  let mut after = before();
  clip_mut(&mut after, 0, 1).set_enabled(false);
  assert_eq!(
    kinds(&diff(&before(), &after).unwrap()),
    [(0, ChangeKind::EnabledFlipped, Some(1), Some(1))]
  );
  assert_eq!(
    kinds(&diff(&after, &before()).unwrap()),
    [(0, ChangeKind::EnabledFlipped, Some(1), Some(1))]
  );
}

#[test]
fn a_clip_changed_every_way_is_reported_once_per_way_in_kind_order() {
  let mut after = before();
  let b = clip_mut(&mut after, 0, 1);
  b.set_record(TimeRange::new(20, 30, edit()));
  b.set_source_range(TimeRange::new(0, 10, edit()));
  b.set_gain(Gain::from_db(-6.0));
  b.set_enabled(false);
  assert_eq!(
    kinds(&diff(&before(), &after).unwrap()),
    [
      (0, ChangeKind::Moved, Some(1), Some(1)),
      (0, ChangeKind::Retimed, Some(1), Some(1)),
      (0, ChangeKind::Regained, Some(1), Some(1)),
      (0, ChangeKind::EnabledFlipped, Some(1), Some(1)),
    ]
  );
}

#[test]
fn trimming_one_clip_moves_no_other() {
  // `a` loses its last five frames; `b` keeps its record, so it is not
  // moved: positions are explicit.
  let mut after = before();
  let a = clip_mut(&mut after, 0, 0);
  a.set_record(TimeRange::new(0, 5, edit()));
  a.set_source_range(TimeRange::new(100, 105, edit()));
  assert_eq!(
    kinds(&diff(&before(), &after).unwrap()),
    [
      (0, ChangeKind::Moved, Some(0), Some(0)),
      (0, ChangeKind::Retimed, Some(0), Some(0)),
    ]
  );
}

#[test]
fn a_clip_is_identified_by_its_name_and_its_locator() {
  // Same name, another medium: the old clip is removed, a new one added.
  let mut after = before();
  clip_mut(&mut after, 0, 2).set_media(MediaRef::new("file:///elsewhere.mov"));
  assert_eq!(
    kinds(&diff(&before(), &after).unwrap()),
    [
      (0, ChangeKind::Added, None, Some(2)),
      (0, ChangeKind::Removed, Some(2), None),
    ]
  );
  // A renamed clip is another clip too.
  let mut renamed = before();
  clip_mut(&mut renamed, 0, 2).set_name("c2");
  assert_eq!(
    kinds(&diff(&before(), &renamed).unwrap()),
    [
      (0, ChangeKind::Added, None, Some(2)),
      (0, ChangeKind::Removed, Some(2), None),
    ]
  );
}

#[test]
fn a_track_naming_one_clip_twice_is_ambiguous() {
  // `a` twice before — at 0 from source frame 100, and at 20 from source
  // frame 0 — and only the second after, unchanged. Matching by occurrence
  // would pair the first with it and report the first moved and retimed and
  // the second removed; nothing says which `a` remains.
  let mut first = clip("a", 0, 10);
  first.set_source_range(TimeRange::new(100, 110, edit()));
  let mut second = clip("a", 20, 30);
  second.set_source_range(TimeRange::new(0, 10, edit()));
  let twice = Timeline::new("t", Rate::FPS_25)
    .with_track(Track::new(TrackKind::Audio, "A").with_clip(clip("d", 0, 20)))
    .with_track(
      Track::new(TrackKind::Video, "V")
        .with_clip(first)
        .with_clip(second),
    );
  let mut once = twice.clone();
  once.tracks_mut()[1].clips_mut().remove(0);
  let ambiguous = diff(&twice, &once).unwrap_err();
  assert_eq!(ambiguous.side(), Side::Before);
  assert_eq!(ambiguous.clips(), ClipPair::new(1, 0, 1));
  assert_eq!(
    alloc::string::ToString::to_string(&ambiguous),
    "before, track 1: clips 0 and 1 share a name and a locator"
  );
  // On the other side, too, and the side before is named first.
  assert_eq!(diff(&once, &twice).unwrap_err().side(), Side::After);
  assert_eq!(diff(&twice, &twice).unwrap_err().side(), Side::Before);
  // One name with two media is two clips, and so is one medium under two
  // names.
  let mut renamed = twice.clone();
  clip_mut(&mut renamed, 1, 1).set_name("a2");
  let mut moved = twice;
  clip_mut(&mut moved, 1, 1).set_media(MediaRef::new("file:///elsewhere.mov"));
  for timeline in [renamed, moved] {
    assert!(diff(&timeline, &timeline).unwrap().is_empty());
  }
}

#[test]
fn a_track_only_one_side_has_is_all_added_or_all_removed() {
  let mut after = before();
  after
    .tracks_mut()
    .push(Track::new(TrackKind::Audio, "A2").with_clip(clip("x", 0, 5)));
  assert_eq!(
    kinds(&diff(&before(), &after).unwrap()),
    [(2, ChangeKind::Added, None, Some(0))]
  );
  assert_eq!(
    kinds(&diff(&after, &before()).unwrap()),
    [(2, ChangeKind::Removed, Some(0), None)]
  );
}

#[test]
fn a_range_recounted_in_another_timebase_is_not_a_change() {
  let mut after = before();
  let half = Timebase::new(1, NonZeroI32::new(50).unwrap());
  clip_mut(&mut after, 0, 0).set_record(TimeRange::new(0, 20, half));
  clip_mut(&mut after, 0, 0).set_source_range(TimeRange::new(200, 220, half));
  assert!(diff(&before(), &after).unwrap().is_empty());
}

#[test]
fn changes_come_by_track_then_by_where_the_clip_sits() {
  let mut after = before();
  // Track 0: remove `a` (at 0), add `e` at 45, move `c` to 50.
  after.tracks_mut()[0].clips_mut().remove(0);
  after.tracks_mut()[0].clips_mut().push(clip("e", 45, 50));
  clip_mut(&mut after, 0, 1).set_record(TimeRange::new(50, 60, edit()));
  // Track 1: `d` switched off.
  clip_mut(&mut after, 1, 0).set_enabled(false);
  assert_eq!(
    kinds(&diff(&before(), &after).unwrap()),
    [
      (0, ChangeKind::Removed, Some(0), None),
      (0, ChangeKind::Added, None, Some(2)),
      (0, ChangeKind::Moved, Some(2), Some(1)),
      (1, ChangeKind::EnabledFlipped, Some(0), Some(0)),
    ]
  );
}

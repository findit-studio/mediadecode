use core::num::NonZeroI32;

use super::*;
use crate::{ClipId, Duration, Fade, FadeShape, Gain, MediaRef, Rate, Timebase, Track, TrackKind};

fn edit() -> Timebase {
  Timebase::new(1, NonZeroI32::new(25).unwrap())
}

/// The clip `name`, its id its name too.
fn clip(name: &str, start: i64, end: i64) -> Clip {
  Clip::new(
    ClipId::new(name),
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
  b.set_name("b2");
  b.set_media(MediaRef::new("file:///b2.mov"));
  assert_eq!(
    kinds(&diff(&before(), &after).unwrap()),
    [
      (0, ChangeKind::Moved, Some(1), Some(1)),
      (0, ChangeKind::Retimed, Some(1), Some(1)),
      (0, ChangeKind::Regained, Some(1), Some(1)),
      (0, ChangeKind::EnabledFlipped, Some(1), Some(1)),
      (0, ChangeKind::Renamed, Some(1), Some(1)),
      (0, ChangeKind::Relinked, Some(1), Some(1)),
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
fn a_clip_is_identified_by_its_id() {
  // Renamed, it is the same clip.
  let mut renamed = before();
  clip_mut(&mut renamed, 0, 2).set_name("c2");
  assert_eq!(
    kinds(&diff(&before(), &renamed).unwrap()),
    [(0, ChangeKind::Renamed, Some(2), Some(2))]
  );
  // Pointed at another medium, the same clip.
  let mut relinked = before();
  clip_mut(&mut relinked, 0, 2).set_media(MediaRef::new("file:///elsewhere.mov"));
  assert_eq!(
    kinds(&diff(&before(), &relinked).unwrap()),
    [(0, ChangeKind::Relinked, Some(2), Some(2))]
  );
  // Under another id, the same name and medium are another clip.
  let mut reminted = before();
  clip_mut(&mut reminted, 0, 2).set_id(ClipId::new("c, again"));
  assert_eq!(
    kinds(&diff(&before(), &reminted).unwrap()),
    [
      (0, ChangeKind::Added, None, Some(2)),
      (0, ChangeKind::Removed, Some(2), None),
    ]
  );
}

#[test]
fn two_placements_of_one_medium_are_two_clips() {
  // Codex's case: `a` placed twice — at 0 from source frame 100, and at 20
  // from source frame 0 — under the ids a0 and a1, and only a1 after,
  // unchanged. By name and locator nothing says which `a` remains, and by
  // occurrence a0 would be reported moved and retimed and a1 removed. By id,
  // a0 is removed and nothing else changed.
  let mut first = clip("a", 0, 10);
  first.set_id(ClipId::new("a0"));
  first.set_source_range(TimeRange::new(100, 110, edit()));
  let mut second = clip("a", 20, 30);
  second.set_id(ClipId::new("a1"));
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
  assert_eq!(
    kinds(&diff(&twice, &once).unwrap()),
    [(1, ChangeKind::Removed, Some(0), None)]
  );
  assert_eq!(
    kinds(&diff(&once, &twice).unwrap()),
    [(1, ChangeKind::Added, None, Some(0))]
  );
  assert!(diff(&twice, &twice).unwrap().is_empty());
}

#[test]
fn a_timeline_carrying_one_id_twice_is_ambiguous() {
  // `a`'s id on `d` too, on the other track: a clip `a` of the other side
  // could be matched with either.
  let mut twice = before();
  clip_mut(&mut twice, 1, 0).set_id(ClipId::new("a"));
  let ambiguous = diff(&twice, &before()).unwrap_err();
  assert_eq!(ambiguous.side(), Side::Before);
  assert_eq!(
    ambiguous.clips(),
    IdClash::new(ClipAt::new(0, 0), ClipAt::new(1, 0))
  );
  assert_eq!(
    alloc::string::ToString::to_string(&ambiguous),
    "before: track 1, clip 0 has the id of track 0, clip 0"
  );
  // On the other side, too, and the side before is named first.
  assert_eq!(diff(&before(), &twice).unwrap_err().side(), Side::After);
  assert_eq!(diff(&twice, &twice).unwrap_err().side(), Side::Before);
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

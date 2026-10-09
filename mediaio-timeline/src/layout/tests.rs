use core::num::NonZeroI32;

use super::*;
use crate::{ClipId, MediaRef, Rate, Timebase, TrackKind};

fn edit() -> Timebase {
  Timebase::new(1, NonZeroI32::new(25).unwrap())
}

fn clip(name: &str, start: i64, end: i64) -> Clip {
  Clip::new(
    ClipId::new(name),
    name,
    MediaRef::new(name),
    TimeRange::new(0, end - start, edit()),
    TimeRange::new(start, end, edit()),
  )
}

#[test]
fn gaps_are_derived_between_records_and_clips_keep_theirs() {
  let timeline = Timeline::new("t", Rate::FPS_25)
    .with_track(
      Track::new(TrackKind::Video, "V")
        .with_clip(clip("a", 0, 10))
        .with_clip(clip("b", 10, 20))
        .with_clip(clip("c", 35, 40)),
    )
    .with_track(Track::new(TrackKind::Audio, "A").with_clip(clip("d", 5, 15)))
    .with_track(Track::new(TrackKind::Audio, "empty"));
  let laid = layout(&timeline).unwrap();
  let records = |track: usize| -> Vec<(bool, TimeRange)> {
    laid.tracks()[track]
      .items()
      .iter()
      .map(|item| (matches!(item, Item::Gap(_)), item.record()))
      .collect()
  };
  let r = |start, end| TimeRange::new(start, end, edit());
  assert_eq!(
    records(0),
    [
      (false, r(0, 10)),
      (false, r(10, 20)),
      (true, r(20, 35)),
      (false, r(35, 40))
    ]
  );
  assert_eq!(records(1), [(true, r(0, 5)), (false, r(5, 15))]);
  assert!(records(2).is_empty());
  assert_eq!(laid.tracks()[1].track().name(), "A");
  let Item::Clip(first) = laid.tracks()[0].items()[0] else {
    panic!("a clip first");
  };
  assert!(core::ptr::eq(first, &timeline.tracks()[0].clips()[0]));
}

#[test]
fn a_timeline_that_does_not_validate_has_no_layout() {
  let timeline = Timeline::new("t", Rate::FPS_25).with_track(
    Track::new(TrackKind::Video, "V")
      .with_clip(clip("a", 0, 10))
      .with_clip(clip("b", 5, 15)),
  );
  assert_eq!(
    layout(&timeline),
    Err(alloc::vec![Refusal::Overlap(crate::ClipPair::new(0, 0, 1))])
  );
}

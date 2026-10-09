//! The law: a two-track, three-clip timeline with a dissolve and fades
//! validates, lays out with the right gaps, exports to OpenTimelineIO the
//! self-check accepts and that matches the hand-predicted golden byte for
//! byte, round-trips as a document, and diffs every kind of change.

mod common;

use mediaio_timeline::{
  ChangeKind, ClipId, Delta, Gain, Item, TimeRange, Timeline, diff, layout,
  otio::{OtioTarget, to_otio, validate_json},
  validate,
};

const GOLDEN_V0_15: &str = include_str!("golden/law.v0_15.otio");
const GOLDEN_LEGACY: &str = include_str!("golden/law.legacy.otio");

/// Each laid-out track as `(is a gap, start, end)` in edit-rate frames.
fn items(timeline: &Timeline) -> Vec<Vec<(bool, i64, i64)>> {
  layout(timeline)
    .unwrap()
    .tracks()
    .iter()
    .map(|track| {
      track
        .items()
        .iter()
        .map(|item| {
          let record = item.record();
          (
            matches!(item, Item::Gap(_)),
            record.start_pts(),
            record.end_pts(),
          )
        })
        .collect()
    })
    .collect()
}

#[test]
fn the_law_timeline_validates_lays_out_and_exports_its_golden() {
  let law = common::law();
  assert_eq!(validate(&law), Ok(()));
  assert_eq!(
    items(&law),
    [
      vec![(false, 0, 96), (false, 96, 144)],
      vec![(true, 0, 24), (false, 24, 120)],
    ]
  );
  for (target, golden) in [
    (OtioTarget::V0_15Plus, GOLDEN_V0_15),
    (OtioTarget::Legacy, GOLDEN_LEGACY),
  ] {
    let text = to_otio(&law, target).unwrap();
    assert_eq!(validate_json(&text, target), Ok(()));
    assert!(text == golden, "{target:?} differs from its golden");
  }
}

#[test]
fn the_law_timeline_round_trips_as_a_document() {
  let law = common::law();
  let text = serde_json::to_string_pretty(&law).unwrap();
  assert_eq!(serde_json::from_str::<Timeline>(&text).unwrap(), law);
}

fn kinds(delta: &Delta) -> Vec<(usize, ChangeKind)> {
  delta
    .changes()
    .iter()
    .map(|change| (change.track(), change.kind()))
    .collect()
}

#[test]
fn every_kind_of_change_to_the_law_timeline_is_reported() {
  let law = common::law();
  let edit = common::tb(1001, 24_000);
  let changed = |edit_fn: &dyn Fn(&mut Timeline)| {
    let mut after = law.clone();
    edit_fn(&mut after);
    kinds(&diff(&law, &after).unwrap())
  };
  assert_eq!(
    changed(&|t| {
      // `a` placed again, on the sound track: another clip, with its own id.
      let clip = t.tracks()[0].clips()[0].clone();
      t.tracks_mut()[1].clips_mut().push({
        let mut c = clip;
        c.set_id(ClipId::new("clip-4"));
        c.set_record(TimeRange::new(120, 216, edit));
        c
      });
    }),
    [(1, ChangeKind::Added)]
  );
  assert_eq!(
    changed(&|t| {
      t.tracks_mut()[0].clips_mut().remove(1);
    }),
    [(0, ChangeKind::Removed)]
  );
  assert_eq!(
    changed(&|t| {
      t.tracks_mut()[1].clips_mut()[0].set_record(TimeRange::new(30, 126, edit));
    }),
    [(1, ChangeKind::Moved)]
  );
  assert_eq!(
    changed(&|t| {
      t.tracks_mut()[0].clips_mut()[0].set_source_range(TimeRange::new(
        25_025,
        121_121,
        common::tb(1, 24_000),
      ));
    }),
    [(0, ChangeKind::Retimed)]
  );
  assert_eq!(
    changed(&|t| {
      t.tracks_mut()[1].clips_mut()[0].set_gain(Gain::from_db(-3.0));
    }),
    [(1, ChangeKind::Regained)]
  );
  assert_eq!(
    changed(&|t| {
      t.tracks_mut()[0].clips_mut()[1].set_enabled(false);
    }),
    [(0, ChangeKind::EnabledFlipped)]
  );
  assert_eq!(
    changed(&|t| {
      t.tracks_mut()[0].clips_mut()[0].set_name("a, take 2");
    }),
    [(0, ChangeKind::Renamed)]
  );
  assert_eq!(
    changed(&|t| {
      t.tracks_mut()[1].clips_mut()[0]
        .media_mut()
        .set_locator("file:///media/a-mixed.wav");
    }),
    [(1, ChangeKind::Relinked)]
  );
}

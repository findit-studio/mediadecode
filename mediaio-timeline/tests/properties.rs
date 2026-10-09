//! Seeded properties: valid timelines built at random, from fixed seeds, so
//! every run checks the same few hundred and a failure names its seed.

mod common;

use mediaio_timeline::{
  ChangeKind, Clip, Duration, Fade, FadeShape, Fades, Gain, Item, MediaRef, Rate, TimeRange,
  Timebase, Timeline, Timestamp, Track, TrackKind, Transition, diff, layout,
  otio::{OtioTarget, to_otio, validate_json},
  validate,
};
use mediatime::Rounding;

/// Seeds checked per property; fewer under Miri, which interprets slowly.
const SEEDS: u64 = if cfg!(miri) { 4 } else { 256 };

/// SplitMix64: a fixed seed gives a fixed sequence, on every platform.
struct Seeded(u64);

impl Seeded {
  fn next(&mut self) -> u64 {
    self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = self.0;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
  }

  /// A number in `low..=high`.
  fn within(&mut self, low: u64, high: u64) -> u64 {
    low + self.next() % (high - low + 1)
  }

  /// `true` one time in `odds`.
  fn one_in(&mut self, odds: u64) -> bool {
    self.next().is_multiple_of(odds)
  }

  fn pick<T: Copy>(&mut self, items: &[T]) -> T {
    items[self.next() as usize % items.len()]
  }
}

const RATES: [Rate; 8] = [
  Rate::FPS_23_976,
  Rate::FPS_24,
  Rate::FPS_25,
  Rate::FPS_29_97,
  Rate::FPS_30,
  Rate::FPS_50,
  Rate::FPS_59_94,
  Rate::FPS_60,
];

/// The rulers media are counted in — each finer than any frame above, so a
/// record's length always survives the trip through one and back.
fn media_rulers() -> [Timebase; 5] {
  [
    common::tb(1, 24_000),
    common::tb(1, 48_000),
    common::tb(1, 44_100),
    common::tb(1, 90_000),
    Timebase::MILLIS,
  ]
}

struct Planned {
  gap: u64,
  length: u64,
  /// The transition into this clip from the one before, `(in, out)`.
  entering: Option<(u64, u64)>,
}

/// A valid timeline, built from `seed`.
fn timeline(seed: u64) -> Timeline {
  let mut rng = Seeded(seed);
  let rate = rng.pick(&RATES);
  let edit = rate.checked_to_timebase().unwrap();
  let frames = |n: u64| Duration::new(n, edit);
  let mut timeline = Timeline::new(format!("seed {seed}"), rate)
    .with_start(Timestamp::new(rng.within(0, 200_000) as i64, edit));
  for track_index in 0..rng.within(1, 3) {
    let kind = if rng.one_in(2) {
      TrackKind::Video
    } else {
      TrackKind::Audio
    };
    let count = rng.within(0, 6);
    let mut plan: Vec<Planned> = Vec::new();
    for index in 0..count {
      let length = rng.within(6, 90);
      let gap = if rng.one_in(2) { 0 } else { rng.within(1, 40) };
      let entering = (index > 0 && gap == 0 && rng.one_in(2)).then(|| {
        let before = plan[index as usize - 1].length;
        (rng.within(0, before / 3), rng.within(0, length / 3))
      });
      plan.push(Planned {
        gap,
        length,
        entering,
      });
    }
    let mut track = Track::new(kind, format!("T{track_index}"));
    let mut cursor = 0;
    for (index, planned) in plan.iter().enumerate() {
      let leaving = plan.get(index + 1).and_then(|next| next.entering);
      let start = cursor + planned.gap;
      cursor = start + planned.length;
      let ruler = rng.pick(&media_rulers());
      let ticks = |length: Duration, rounding| {
        length
          .checked_rescale_with(ruler, rounding)
          .unwrap()
          .ticks()
      };
      let source_length = ticks(frames(planned.length), Rounding::Nearest);
      assert_eq!(
        Duration::new(source_length, ruler)
          .checked_rescale_to(edit)
          .unwrap()
          .ticks(),
        planned.length,
        "seed {seed}: the generator's media ruler cannot hold a frame"
      );
      // Handles: as much media before and after the source range as the
      // transitions on either side play, and a little more.
      let head = planned
        .entering
        .map_or(0, |(into, _)| ticks(frames(into), Rounding::Ceil));
      let tail = leaving.map_or(0, |(_, out)| ticks(frames(out), Rounding::Ceil));
      let available_start = rng.within(0, 100) as i64;
      let source_start = available_start + head as i64 + rng.within(0, 500) as i64;
      let source_end = source_start + source_length as i64;
      let available_end = source_end + tail as i64 + rng.within(0, 500) as i64;
      let mut media = MediaRef::new(format!("file:///{track_index}/{index}"));
      if !rng.one_in(4) {
        media.set_available_range(Some(TimeRange::new(available_start, available_end, ruler)));
      }
      if rng.one_in(2) {
        media.set_rate(Some(rng.pick(&[rate, Rate::hz(48_000), Rate::FPS_25])));
      }
      if rng.one_in(3) {
        media.set_reel(Some(format!("R{index}")));
      }
      let mut fades = Fades::new();
      if planned.entering.is_none() && rng.one_in(3) {
        fades.set_in(Some(Fade::new(
          frames(rng.within(1, planned.length / 3)),
          FadeShape::Linear,
        )));
      }
      if leaving.is_none() && rng.one_in(3) {
        fades.set_out(Some(Fade::new(
          frames(rng.within(1, planned.length / 3)),
          FadeShape::EqualPower,
        )));
      }
      let gain = rng
        .one_in(3)
        .then(|| Gain::from_db(-(rng.within(0, 240) as f32) / 10.0).unwrap());
      track.clips_mut().push(
        Clip::new(
          format!("c{index}"),
          media,
          TimeRange::new(source_start, source_end, ruler),
          TimeRange::new(start as i64, cursor as i64, edit),
        )
        .with_gain(gain)
        .with_fades(fades)
        .with_enabled(!rng.one_in(8)),
      );
      if let Some((into, out)) = planned.entering {
        track.transitions_mut().push(Transition::dissolve(
          Timestamp::new(start as i64, edit),
          frames(into),
          frames(out),
        ));
      }
    }
    timeline.tracks_mut().push(track);
  }
  timeline
}

#[test]
fn validate_accepts_what_the_generator_builds() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    assert_eq!(validate(&timeline), Ok(()), "seed {seed}");
  }
}

#[test]
fn layout_tiles_each_track_from_zero_and_its_gaps_never_overlap() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    let laid = layout(&timeline).unwrap();
    for (track, laid) in timeline.tracks().iter().zip(laid.tracks()) {
      let mut covered_to = Timestamp::new(0, timeline.edit_timebase().unwrap());
      let mut clips = 0;
      for item in laid.items() {
        let record = item.record();
        // Each item starts where the one before it ends: nothing overlaps,
        // and no time is left out.
        assert_eq!(record.start(), covered_to, "seed {seed}");
        assert!(record.start() < record.end(), "seed {seed}: an empty item");
        if let Item::Clip(clip) = item {
          assert!(core::ptr::eq(*clip, &track.clips()[clips]), "seed {seed}");
          clips += 1;
        }
        covered_to = record.end();
      }
      assert_eq!(clips, track.clips().len(), "seed {seed}");
    }
  }
}

#[test]
fn a_timeline_against_itself_has_no_change() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    assert!(diff(&timeline, &timeline).is_empty(), "seed {seed}");
  }
}

#[test]
fn diff_names_exactly_the_change_made() {
  for seed in 0..SEEDS {
    let before = timeline(seed);
    let Some(track) = before.tracks().iter().position(|t| !t.clips().is_empty()) else {
      continue;
    };
    let clip = (seed as usize) % before.tracks()[track].clips().len();
    let mut after = before.clone();
    let edited = &mut after.tracks_mut()[track].clips_mut()[clip];
    let enabled = edited.enabled();
    edited.set_enabled(!enabled);
    let changes = diff(&before, &after);
    let changes: Vec<_> = changes
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
      .collect();
    assert_eq!(
      changes,
      [(track, ChangeKind::EnabledFlipped, Some(clip), Some(clip))],
      "seed {seed}"
    );
  }
}

#[test]
fn a_document_round_trips() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    let text = serde_json::to_string(&timeline).unwrap();
    let back: Timeline = serde_json::from_str(&text).unwrap();
    assert_eq!(back, timeline, "seed {seed}");
  }
}

#[test]
fn the_export_passes_the_self_check_for_both_targets() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    for target in [OtioTarget::V0_15Plus, OtioTarget::Legacy] {
      let text = to_otio(&timeline, target).unwrap();
      assert_eq!(validate_json(&text, target), Ok(()), "seed {seed}");
    }
  }
}

/// OpenTimelineIO places a track's items end to end; walk them as it does
/// and the clips land on their records, frame for frame.
#[test]
fn the_export_places_every_clip_on_its_record() {
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    let rate = timeline.rate().as_f64();
    let text = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
    let root: serde_json::Value = serde_json::from_str(&text).unwrap();
    let tracks = root["tracks"]["children"].as_array().unwrap();
    for (track, exported) in timeline.tracks().iter().zip(tracks) {
      let mut at = 0.0;
      let mut placed = Vec::new();
      for child in exported["children"].as_array().unwrap() {
        let schema = child["OTIO_SCHEMA"].as_str().unwrap();
        if schema == "Transition.1" {
          continue;
        }
        let duration = &child["source_range"]["duration"];
        assert_eq!(duration["rate"].as_f64().unwrap(), rate, "seed {seed}");
        if schema == "Clip.2" {
          placed.push(at);
        }
        at += duration["value"].as_f64().unwrap();
      }
      let records: Vec<f64> = track
        .clips()
        .iter()
        .map(|clip| clip.record().start_pts() as f64)
        .collect();
      assert_eq!(placed, records, "seed {seed}");
    }
  }
}

/// The properties above are only as strong as what the generator builds:
/// across the seeds it reaches every word of the model, often.
#[test]
fn the_generator_reaches_every_word() {
  #[derive(Default, Debug)]
  struct Seen {
    tracks: [usize; 2],
    clips: usize,
    gaps: usize,
    abutting: usize,
    transitions: usize,
    fades_in: usize,
    fades_out: usize,
    gains: usize,
    disabled: usize,
    unknown_available: usize,
    media_rates: usize,
    reels: usize,
    rulers: std::collections::BTreeSet<String>,
    rates: std::collections::BTreeSet<String>,
  }
  let mut seen = Seen::default();
  for seed in 0..SEEDS {
    let timeline = timeline(seed);
    seen.rates.insert(timeline.rate().to_string());
    for (track, laid) in timeline
      .tracks()
      .iter()
      .zip(layout(&timeline).unwrap().tracks())
    {
      seen.tracks[usize::from(track.kind() == TrackKind::Audio)] += 1;
      seen.transitions += track.transitions().len();
      seen.gaps += laid
        .items()
        .iter()
        .filter(|item| matches!(item, Item::Gap(_)))
        .count();
      seen.abutting += track
        .clips()
        .windows(2)
        .filter(|pair| pair[0].record().end() == pair[1].record().start())
        .count();
      for clip in track.clips() {
        seen.clips += 1;
        seen.fades_in += usize::from(clip.fades().in_().is_some());
        seen.fades_out += usize::from(clip.fades().out().is_some());
        seen.gains += usize::from(clip.gain().is_some());
        seen.disabled += usize::from(!clip.enabled());
        seen.unknown_available += usize::from(clip.media().available_range().is_none());
        seen.media_rates += usize::from(clip.media().rate().is_some());
        seen.reels += usize::from(clip.media().reel().is_some());
        seen
          .rulers
          .insert(clip.source_range().timebase().to_string());
      }
    }
  }
  if cfg!(miri) {
    return;
  }
  let enough = |count: usize, what: &str| assert!(count >= 40, "{what}: {count} in {seen:?}");
  enough(seen.tracks[0], "video tracks");
  enough(seen.tracks[1], "audio tracks");
  enough(seen.clips, "clips");
  enough(seen.gaps, "gaps");
  enough(seen.abutting, "abutting clips");
  enough(seen.transitions, "transitions");
  enough(seen.fades_in, "fades in");
  enough(seen.fades_out, "fades out");
  enough(seen.gains, "gains");
  enough(seen.disabled, "disabled clips");
  enough(seen.unknown_available, "unknown available ranges");
  enough(seen.media_rates, "media rates");
  enough(seen.reels, "reels");
  assert_eq!(seen.rulers.len(), media_rulers().len(), "{seen:?}");
  assert_eq!(seen.rates.len(), RATES.len(), "{seen:?}");
}

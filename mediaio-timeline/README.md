<div align="center">
<h1>mediaio-timeline</h1>
</div>
<div align="center">

The editing timeline as data: tracks, clips at explicit record ranges, gain,
fades and dissolves — with validation that refuses by name, a diff, and
OpenTimelineIO export. Pure Rust, `no_std` + `alloc`, no FFmpeg.

[<img alt="github" src="https://img.shields.io/badge/github-findit--studio/mediadecode-8da0cb?style=for-the-badge&logo=Github" height="22">][Github-url]
<img alt="LoC" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fal8n%2F327b2a8aef9003246e45c6e47fe63937%2Fraw%2Fmediaio-timeline" height="22">
[<img alt="Build" src="https://img.shields.io/github/actions/workflow/status/findit-studio/mediadecode/ci-core.yml?logo=Github-Actions&style=for-the-badge" height="22">][CI-url]
[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-mediaio--timeline-66c2a5?style=for-the-badge&labelColor=555555" height="20">][doc-url]
[<img alt="crates.io" src="https://img.shields.io/crates/v/mediaio-timeline?style=for-the-badge" height="22">][crates-url]
<img alt="license" src="https://img.shields.io/badge/License-Apache%202.0/MIT-blue.svg?style=for-the-badge" height="22">

</div>

## The model

| type | what it is |
|---|---|
| `Timeline` | an edit: `name`, the edit `rate`, the timecode `start` its zero carries, `metadata`, and `tracks` stacked bottom first — a `schema 1` document |
| `Track` | one layer: `kind` (`Video` or `Audio`), `name`, `enabled`, `clips` in record order, and the `transitions` between them |
| `Clip` | a stretch of one medium at an explicit position: `name`, `media`, `source_range` (in the medium's timebase), `record` (at the edit rate), `enabled`, `gain`, `fades`, `metadata` |
| `MediaRef` | the medium: its `locator`, and when known its `available_range`, `rate` and `reel` |
| `Transition` | a `Dissolve` across the cut `at`, reaching `in_offset` before it and `out_offset` after it — OpenTimelineIO's offsets |
| `Fades`, `Fade` | a fade at a clip's start (`in`) and end (`out`): a `duration` at the edit rate and a `shape`, `Linear` or `EqualPower` |
| `Gain` | decibels, finite by construction |
| `Metadata` | an ordered map of strings — notes the model carries and never reads |

The time words — `Rate`, `Timebase`, `Timestamp`, `Duration`, `TimeRange` —
are [`mediatime`](https://crates.io/crates/mediatime)'s, re-exported.

### Positions are explicit

A clip's `record` is stored, not derived from the clips before it: trimming
one clip moves no other, and a diff calls a clip moved only when its own
record changed. Every position on the record side — a record, the start, a
transition's cut and offsets, a fade's length — is counted at the edit rate,
from the timeline's zero. `start` is the timecode that zero carries
(`01:00:00:00` is 86 400 frames at 24 fps); it labels the timeline and moves
nothing. With no time-warp in schema 1, a record runs exactly as long as its
source range, rescaled to the edit rate to the nearest tick.

Gaps are never stored. `layout` derives them — from the zero to the first
record, and between records — for formats that place items end to end.

## Validation

`validate(&Timeline) -> Result<(), Vec<Refusal>>` answers every defect at
once, in a fixed order, each naming the clip, pair of clips, edge or
transition it is about:

| refusal | the defect |
|---|---|
| `RateUnstated` | the edit rate is zero |
| `OffEditRate` | the start, a record, a fade or a transition not counted at the edit rate |
| `MediaRateUnstated` | a stated media rate of zero, or a source or available range in a degenerate timebase |
| `RecordBeforeZero` | a record starting before the timeline's zero |
| `EmptyRecord` | a record covering no time |
| `OutOfOrder` | a track's clips out of record order |
| `Overlap` | two records of one track overlapping — a transition blends with media outside the records, so it is never one |
| `DurationMismatch` | a record not its source's length at the edit rate |
| `OutsideAvailable` | a source range outside its medium's available range, when that is known |
| `OffBoundary` | a transition not on a cut where one record ends and the next begins |
| `DuplicateTransition` | a second transition on one cut |
| `HandleMissing` | the outgoing clip's medium lacks `out_offset` after its source range, or the incoming clip's lacks `in_offset` before it |
| `FadeMeetsTransition` | a fade where a transition already blends |
| `BlendsOverrunClip` | a clip's fades and the parts of transitions inside it running longer than the clip — a fade longer than its clip among them |

Building a timeline checks nothing: the value is plain data, and `validate`
is where it is judged.

## Diff

`diff(&before, &after) -> Delta` reports, per track in record order, each
clip `Added`, `Removed`, `Moved` (its record changed), `Retimed` (its source
range changed), `Regained` (its gain or fades changed) or `EnabledFlipped`.
Tracks match by index; a clip is identified by its `name` together with its
medium's `locator` (an `id` word is reserved for a later schema), and a pair
used twice matches in order. Ranges compare by the time they cover. The
order is fixed, so `diff(a, a)` is empty. Only clips are compared: the
timeline's and a track's own words, transitions, and a clip's notes are not.

## OpenTimelineIO

`otio::to_otio(&Timeline, OtioTarget)` writes a valid timeline as
OpenTimelineIO JSON, keys in OpenTimelineIO's own writer order. A timeline
that does not validate is refused with its refusals.

| model | OpenTimelineIO |
|---|---|
| `Timeline` | `Timeline.1`; `global_start_time` always written, from `start`, at the edit rate |
| its tracks | one `Stack.1` named `tracks` |
| `Track` | `Track.1`: `kind` `Video` or `Audio`, `enabled` |
| `Clip` | `Clip.2` for `OtioTarget::V0_15Plus` (OpenTimelineIO 0.15 and later), `Clip.1` for `OtioTarget::Legacy`; `enabled` |
| a clip's trim | `source_range`: `duration` is the record's length at the edit rate, `start_time` the source range's start |
| `MediaRef` | `ExternalReference.1` (under `DEFAULT_MEDIA` in a `Clip.2`): `target_url` is the locator, `available_range` the available range |
| a gap between records | `Gap.1`, derived by `layout` |
| `Transition` | `Transition.1`, `SMPTE_Dissolve`, offsets at the edit rate |
| a fade | `Transition.1`, `SMPTE_Dissolve` against a gap — of no length where the clip abuts a clip or the track's end |
| gain, reel, the medium's rate, a fade's curve, `Metadata` | `metadata.mediaio`: `gain_db`, `reel`, `rate`, `fade` and `shape`, `metadata` |

Record-side times are whole counts at the edit rate. A media-side time is
written in frames of the medium's stated rate where it lands on one, else in
ticks of its own timebase, so every number written is exact — and walking
the exported items end to end lands every clip on its record.

OpenTimelineIO has no word for gain or a reel: both ride in `metadata`, and
an application that does not read it — DaVinci Resolve among them — applies
neither. Available ranges and the start are written as the timeline holds
them: a medium read through `mediadecode` starts at zero until the read side
exposes the container's timecode, so its available range does not yet carry
the camera's.

`otio::validate_json(&str, OtioTarget)` is the structural self-check the
export is held to: the crate's own strict JSON reader, then OpenTimelineIO's
schema shape — required keys and their types, media references, the
target's clip schema, every transition between two items. No Python and no
OpenTimelineIO install; CI runs it on the goldens.

## The stored document

With `serde`, a timeline is a document whose first field is `schema` (`1`).
A reader refuses by name an unknown schema, a field it does not know, a
required field left out, a metadata key named twice and a gain that is not
finite. An optional word left out reads as absent.

## Time

The crate does no time arithmetic of its own: every comparison, sum and
recount is `mediatime`'s, exact across timebases unless it names a
rounding. The one road `mediatime` 0.5 lacks — a range's exact length as a
`Duration` — is a `mediatime` row.

## Not here

- **Time-warp.** `speed` is reserved for a later schema, and a schema 1
  reader refuses it by name.
- **Reading OpenTimelineIO back** (`from_otio`) — a later row; the `otio`
  module is its home.
- **Rendering.** The compositor and the contact sheet are `mediaio-render`'s,
  the mixer `mediaencode`'s.
- **A clip `id`** — reserved; clips are identified by name and locator.
- **The container's timecode** in available ranges — a `mediadecode` row.

## Example

```rust
use core::num::NonZeroI32;

use mediaio_timeline::{
  Clip, ClipPair, Duration, Fade, FadeShape, Fades, MediaRef, Rate, Refusal, TimeRange,
  Timebase, Timeline, Timestamp, Track, TrackKind, Transition, TransitionAt,
  otio::{OtioTarget, to_otio, validate_json},
  validate,
};

let edit = Timebase::new(1, NonZeroI32::new(25).unwrap());
let frames = |n| Duration::new(n, edit);
let shot = |name: &str, start: i64| {
  Clip::new(
    name,
    MediaRef::new(format!("file:///media/{name}.mov"))
      .with_available_range(Some(TimeRange::new(0, 250, edit))),
    TimeRange::new(50, 100, edit),
    TimeRange::new(start, start + 50, edit),
  )
};

// Two shots with a ten-frame dissolve between them; the second fades out.
let timeline = Timeline::new("cut", Rate::FPS_25).with_track(
  Track::new(TrackKind::Video, "V1")
    .with_clip(shot("a", 0))
    .with_clip(shot("b", 50).with_fades(
      Fades::new().with_out(Some(Fade::new(frames(10), FadeShape::Linear))),
    ))
    .with_transition(Transition::dissolve(
      Timestamp::new(50, edit),
      frames(5),
      frames(5),
    )),
);
assert_eq!(validate(&timeline), Ok(()));

let otio = to_otio(&timeline, OtioTarget::V0_15Plus).unwrap();
assert_eq!(validate_json(&otio, OtioTarget::V0_15Plus), Ok(()));

// Pulled ten frames early, `b` overlaps `a` and leaves the dissolve off
// any cut: both refused, by name.
let mut early = timeline.clone();
early.tracks_mut()[0].clips_mut()[1].set_record(TimeRange::new(40, 90, edit));
assert_eq!(
  validate(&early),
  Err(vec![
    Refusal::Overlap(ClipPair::new(0, 0, 1)),
    Refusal::OffBoundary(TransitionAt::new(0, 0)),
  ])
);
```

## Requirements

- Rust ≥ **1.95**, edition 2024.
- `#![no_std]`, needing only `alloc`: a timeline owns its names, lists and
  maps. The crate has no features; `serde` and `mediatime` are taken without
  their `std`, and nothing links FFmpeg.

## License

`mediaio-timeline` is under the terms of both the MIT license and the
Apache License (Version 2.0).

See [LICENSE-APACHE](https://github.com/findit-studio/mediadecode/blob/main/LICENSE-APACHE),
[LICENSE-MIT](https://github.com/findit-studio/mediadecode/blob/main/LICENSE-MIT)
for details.

Copyright (c) 2026 FinDIT Studio authors.

[Github-url]: https://github.com/findit-studio/mediadecode
[CI-url]: https://github.com/findit-studio/mediadecode/actions/workflows/ci-core.yml
[doc-url]: https://docs.rs/mediaio-timeline
[crates-url]: https://crates.io/crates/mediaio-timeline

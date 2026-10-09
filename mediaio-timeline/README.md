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
| `Clip` | a stretch of one medium at an explicit position: its `id`, `name`, `media`, `source_range` (in the medium's timebase), `record` (at the edit rate), `enabled`, `gain`, `fades`, `metadata` |
| `ClipId` | a clip's identity: a string its creator mints, once per placement, unique in the timeline |
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
nothing. With no time-warp in schema 1, a source range runs a whole number of
edit-rate ticks and its record exactly as long: nothing rounds a source onto
the edit rate.

Gaps are never stored. `layout` derives them — from the zero to the first
record, and between records — for formats that place items end to end.

### A clip is its id

A clip is identified by its `id`, which its creator mints — findit's edit
plan mints one per placement — and which no other clip of the timeline
carries. Its name and its medium may repeat: a cut back to a shot, or a
loop of it, places one medium twice, under two ids, and a medium on a video
and an audio track is two clips with two ids. `diff` matches clips by the
id, so a clip renamed or pointed at another medium is still the same clip.

## Validation

`validate(&Timeline) -> Result<(), Vec<Refusal>>` answers every defect at
once, in a fixed order, each naming the clip, pair of clips, range, edge or
transition it is about:

| refusal | the defect |
|---|---|
| `RateUnstated` | the edit rate is zero |
| `OffEditRate` | the start, a record, a fade or a transition not counted at the edit rate |
| `MediaRateUnstated` | a stated media rate of zero, or a source or available range in a degenerate timebase |
| `RangeTooLong` | a record, source range or available range longer than `i64::MAX` ticks — no timeline needs one, and its length has no exact count |
| `RecordBeforeZero` | a record starting before the timeline's zero |
| `EmptyRecord` | a record covering no time |
| `OutOfOrder` | a track's clips out of record order |
| `Overlap` | two records of one track overlapping — a transition blends with media outside the records, so it is never one |
| `EmptyClipId` | a clip whose id is empty |
| `DuplicateClipId` | a clip carrying the id of an earlier clip of the timeline, on its track or another — the identity `diff` matches by |
| `SourceOffEditRate` | a source range whose length is no whole number of edit-rate ticks |
| `DurationMismatch` | a record not exactly its source's length at the edit rate |
| `OutsideAvailable` | a source range outside its medium's available range, when that is known |
| `OffBoundary` | a transition not on a cut where one record ends and the next begins |
| `DuplicateTransition` | a second transition on one cut |
| `HandleMissing` | the outgoing clip's medium lacks `out_offset` after its source range, or the incoming clip's lacks `in_offset` before it |
| `FadeMeetsTransition` | a fade where a transition already blends |
| `BlendsOverrunClip` | a clip's fades and the parts of transitions inside it running longer than the clip — a fade longer than its clip among them |

Building a timeline checks nothing: the value is plain data, and `validate`
is where it is judged.

## Diff

`diff(&before, &after) -> Result<Delta, Ambiguous>` reports, per track in
record order, each clip `Added`, `Removed`, `Moved` (its record changed),
`Retimed` (its source range changed), `Regained` (its gain or fades
changed), `EnabledFlipped`, `Renamed` or `Relinked` (its medium's locator
changed). Tracks match by index, and a clip by its `id` alone: a clip that
changes track is removed from one and added to the other. A side carrying
one id on two clips — a timeline `validate` would refuse — is refused as
`Ambiguous`, naming the side and the two clips, rather than matched by
occurrence. Ranges compare by the time they cover. The order is fixed, so
`diff(a, a)` is empty for every `a` it answers. Only clips are compared: the
timeline's and a track's own words, transitions, and a clip's notes are
not.

## OpenTimelineIO

`otio::to_otio(&Timeline, OtioTarget) -> Result<String, otio::Refused>`
writes a valid timeline as OpenTimelineIO JSON, keys in OpenTimelineIO's own
writer order. A timeline that does not validate is refused with its
refusals (`Refused::Validation`); a count OpenTimelineIO cannot hold exactly
is refused by name (`Refused::NotRepresentable`, below).

| model | OpenTimelineIO |
|---|---|
| `Timeline` | `Timeline.1`; `global_start_time` always written, from `start`, at the edit rate |
| its tracks | one `Stack.1` named `tracks` |
| `Track` | `Track.1`: `kind` `Video` or `Audio`, `enabled` |
| `Clip` | `Clip.2` for `OtioTarget::V0_15Plus` (OpenTimelineIO 0.15 and later), `Clip.1` for `OtioTarget::Legacy`; `enabled` |
| a clip's trim | `source_range`: the source range whole, `start_time` and `duration` in the medium's one ruler — by the exactness rule, a duration as long as the record |
| `MediaRef` | `ExternalReference.1` (under `DEFAULT_MEDIA` in a `Clip.2`): `target_url` is the locator, `available_range` the available range |
| a gap between records | `Gap.1`, derived by `layout` |
| `Transition` | `Transition.1`, `SMPTE_Dissolve`, offsets at the edit rate |
| a fade | `Transition.1`, `SMPTE_Dissolve` against a gap — of no length where the clip abuts a clip or the track's end |
| a clip's `id`, gain, reel, the medium's rate, a fade's curve, `Metadata` | `metadata.mediaio`: `id`, `gain_db`, `reel`, `rate`, `fade` and `shape`, `metadata` |

Record-side times are whole counts at the edit rate. A media-side range — a
source range, an available range — is written whole in one ruler: frames of
the medium's stated rate where its start and length both land on one, else
ticks of its own timebase. The end OpenTimelineIO derives from a range (its
start rescaled to its duration's rate, plus the duration) is the range's own
end, held within ±2^53 as every count it derives is (below). Because a
source range runs a whole number of edit-rate ticks, walking the exported
items end to end lands every clip on its record. Where a medium's ruler is
not the edit rate, one track's items are counted in more than one rate, and
OpenTimelineIO sums those in floating point: a position derived that way is
read to the nearest frame, not truncated — the export holds it under half a
tick of the exact count (below).

**Every count OpenTimelineIO reads or derives is one it holds exactly.** A
`RationalTime` keeps its value in an `f64`, which holds every whole number
up to 2^53 and only some past it, and OpenTimelineIO derives a document's
positions in that arithmetic: a sum of two times is carried in the higher
of their two rates, the other rescaled into it — two rounded operations. The
export holds both:

- **what it writes.** Every count lies within ±2^53, and a rate is the `f64`
  nearest it, spelled so it reads back as that very `f64`. A media-side
  range its own rulers count past 2^53 — a medium stamped in nanoseconds
  since 1970 — is written in the coarsest ruler of a whole number of ticks a
  second that holds its start and its length, whole seconds or
  milliseconds, when its counts there are within 2^53. Of those rulers a
  range is written in the first whose counts end it within ±2^53 too; where
  none does, an available range is refused, and a source range is written
  in the first that writes its start and its length, its end left to the
  walk and to the search below.
- **what OpenTimelineIO derives from it**, computed operation for operation
  and branch for branch as OpenTimelineIO computes it, beside its exact
  value: the end of each clip's source range; each child's place on its
  track — from zero in that child's own rate, every item before it added,
  as `range_of_child_at_index` sums it, and as one walk over the whole
  track carries it — and in the timeline; each item's visible range, its
  source range widened by the handles of the transitions beside it; each
  track's duration, and the stack's, the longest of them as OpenTimelineIO
  picks it; the global start added to each place and to each track's end,
  as OpenTimelineIO's own tools add it; each child's range from the global
  start — the moved start and the child's own duration, as OpenTimelineIO
  moves a child's range into its parent's, so its end rescales the moved
  start into the duration's rate, a rounding of its own; for a timeline
  with no track, its range from the global start, which OpenTimelineIO
  gives no duration at rate 1, so it ends at the global start counted in
  seconds; and the last tick of every one of those ranges —
  `end_time_inclusive`, which floors a range's end where the duration's
  double has a fraction and takes a tick off it where it has none, so a
  duration whose double rounds onto a whole number, or off one, can move
  the last tick by up to a tick.
  Each must lie within ±2^53 in the ruler OpenTimelineIO carries it in, and
  OpenTimelineIO's double less than half a tick from the exact count — read
  to the nearest tick, it is the exact count. On one ruler nothing is
  rescaled, and the double is the count itself: a range the export writes
  in one ruler ends inclusively where it exactly does.

**Exact, or refused.** The export's guarantee is soundness: what it writes,
OpenTimelineIO reads back exactly, and a timeline it cannot so write is
refused by name. Where OpenTimelineIO would round a value, the export
writes the clips the value is formed from in other rulers that hold their
source ranges exactly and walks the timeline again — a search, bounded by
contract. A clip's rulers, in the order the search tries them:

1. its own: frames of its medium's stated rate, ticks of its source's
   timebase, or the coarsest whole rate that holds the range;
2. every ruler the timeline's operands are counted in that holds the range
   — the edit rate, at which the global start, the gaps and the transitions
   are written, the rate 1 OpenTimelineIO sums a track's duration from, and
   every clip's planned ruler — finer or coarser than its own, all of them,
   finest first;
3. the 64 finest whole numbers of ticks a second below its own that hold
   the range and that no operand is counted in, finest first;
4. the 64 coarsest of those, finest first, less any already listed;
5. its own again, where every other is finer.

So where OpenTimelineIO would rescale a clip into the edit rate or into a
neighbour's ruler, the search can write the clip in that ruler itself. It
moves one clip to the next ruler of its list at a time — of the clips the
refused value is formed from, the one with the finest ruler — and walks
again, so clips can meet on a ruler they share, or one move while its
neighbour keeps its own. A clip never moves back, so for `n` clips and `d`
distinct operand rulers the search ends within `1 + n · (d + 127)` walks.

What no plan the search walks holds is refused,
`Refused::NotRepresentable`, naming where (`otio::Spot`; a child of an
exported track is an `otio::ChildAt`), the count the last plan could not
hold and the ruler it is counted in — and, for a count OpenTimelineIO
derives, a source range's end among them, what the search tried:
`otio::RulerSearch`, its bands (`otio::RulerBand`) and its walks. So a
refusal says the search was bounded. Refused before any walk, naming no
search: a count written at the edit rate, which no ruler of the search
changes; a source range none of its own rulers writes, its start or its
length past 2^53 in each, which leaves the search no plan to start from;
an available range none of them holds whole, which keeps its plan's
ruler. A timeline that only a ruler outside the bands, or a plan of
rulers the moves do not reach, would hold is refused by this contract,
never written to be read rounded: a complete search would try every
holding ruler of every clip together, a product space with no closed form
for OpenTimelineIO's double rounding. Where the bound has been met, counts
lie near 2^53 ticks of a clip's ruler — at the rates media run at,
positions thousands of years in: 2^53 ticks are some 1 500 years even at
192 kHz. The rounding a real timeline meets is a rate or a rescale no `f64`
holds exactly — NTSC's 30000/1001, or frames of one rate counted in
another — and the half-tick hold settles it: written where
OpenTimelineIO's double lands within half a tick of the exact count,
refused by name where it does not.

OpenTimelineIO has no word for a clip's id, its gain or a reel: all three
ride in `metadata` — the id so a reader can tell each clip again — and an
application that does not read it — DaVinci Resolve among them — applies
neither gain nor reel. Available ranges and the start are written as the timeline holds
them: a medium read through `mediadecode` starts at zero until the read side
exposes the container's timecode, so its available range does not yet carry
the camera's.

`otio::validate_json(&str, OtioTarget)` is the structural self-check the
export is held to: the crate's own strict JSON reader, then OpenTimelineIO's
schema shape — exactly the keys the target's readers require (audited
against each schema's `read_from`, per target: `Track.1` needs its `kind`,
and a reader before 0.15 a `Clip.1`'s `media_reference`), the types of the
keys the export writes, media references, the target's clip schema, every
transition between two items. No Python and no OpenTimelineIO install; CI
runs it on the goldens.

## The stored document

With `serde`, a timeline is a document whose first field is `schema` (`1`).
A reader refuses by name an unknown schema, a field it does not know — inside
a time value too, a record's or a rate's — a required field left out (a
clip's `id` among them), a metadata key named twice, a gain that is not
finite and a range longer than `i64::MAX` ticks. Time values are written under `mediatime`'s own field names
and read through the crate's strict shapes of them. An optional word left
out reads as absent.

## Time

The crate does no time arithmetic of its own: every comparison, sum and
recount is `mediatime`'s, exact across timebases unless it names a
rounding. Three roads `mediatime` 0.5 lacks are `mediatime` rows: a range's
exact length as a `Duration` — meanwhile a range is measured by the checked
difference of its ends, and one longer than `i64::MAX` ticks is refused —
the coarsest whole-rate timebase holding a range, which the OpenTimelineIO
export picks meanwhile from the greatest common divisor of the range's
counts, the recount itself `mediatime`'s; and an exact number of seconds
counted at a rate as an exact fraction, which the export forms meanwhile as
one product of two fractions. The export also computes OpenTimelineIO's own
floating-point arithmetic, operation for operation, to find a count
OpenTimelineIO would round: that arithmetic is OpenTimelineIO's, not the
model's.

## Not here

- **Time-warp.** `speed` is reserved for a later schema, and a schema 1
  reader refuses it by name.
- **Reading OpenTimelineIO back** (`from_otio`) — a later row; the `otio`
  module is its home.
- **Rendering.** The compositor and the contact sheet are `mediaio-render`'s,
  the mixer `mediaencode`'s.
- **The container's timecode** in available ranges — a `mediadecode` row.

## Example

```rust
use core::num::NonZeroI32;

use mediaio_timeline::{
  Clip, ClipId, ClipPair, Duration, Fade, FadeShape, Fades, MediaRef, Rate, Refusal, TimeRange,
  Timebase, Timeline, Timestamp, Track, TrackKind, Transition, TransitionAt,
  otio::{OtioTarget, to_otio, validate_json},
  validate,
};

let edit = Timebase::new(1, NonZeroI32::new(25).unwrap());
let frames = |n| Duration::new(n, edit);
let shot = |name: &str, start: i64| {
  Clip::new(
    ClipId::new(format!("{name}-1")),
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

# Changelog

All notable changes to the [`mediaio-timeline`](https://crates.io/crates/mediaio-timeline)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Its versions are its own: they do not move with `mediadecode`'s.

## [Unreleased]

The crate's first release, as 0.1.0.

### Added

- **The editing timeline as data, as a schema 1 document.** `Timeline`
  (`name`, the edit `rate`, the timecode `start` its zero carries,
  `metadata`, `tracks`), `Track` (`kind`: `Video` or `Audio`, `name`,
  `enabled`, `clips`, `transitions`), `Clip` (its `id`, `name`, `media`,
  `source_range` in the medium's timebase, `record` at the edit rate,
  `enabled`, `gain`, `fades`, `metadata`), `ClipId` (a clip's identity: a
  string its creator mints once per placement, unique in the timeline;
  names and media may repeat), `MediaRef` (`locator`, and when known
  `available_range`, `rate`, `reel`), `Transition` (a `Dissolve` at a cut,
  with OpenTimelineIO's `in_offset` and `out_offset`), `Fades` and `Fade`
  (`Linear` or `EqualPower`), `Gain` (finite decibels) and `Metadata` (an
  ordered string map). Pure Rust on `mediatime` 0.5 and `serde`; `no_std` +
  `alloc`; no FFmpeg.

  Positions are explicit: a clip's record is stored, never derived from the
  clips before it, so trimming one clip moves no other. With no time-warp in
  schema 1 a source range runs a whole number of edit-rate ticks and its
  record exactly as long; `speed` is reserved for a later schema.

  On the wire the document's first field is `schema`; an unknown schema, a
  field this reader does not know (inside a time value too), a required
  field left out (a clip's `id` among them), a metadata key named twice, a
  gain that is not finite and a range longer than `i64::MAX` ticks are
  refused by name. Time values are written under `mediatime`'s field names
  and read through the crate's own strict shapes of them.

- **`validate`**, refusing by name: `RateUnstated`, `OffEditRate`,
  `MediaRateUnstated`, `RangeTooLong`, `RecordBeforeZero`, `EmptyRecord`,
  `OutOfOrder`, `Overlap`, `EmptyClipId`, `DuplicateClipId`,
  `SourceOffEditRate`, `DurationMismatch`, `OutsideAvailable`,
  `OffBoundary`, `DuplicateTransition`, `HandleMissing`,
  `FadeMeetsTransition` and `BlendsOverrunClip`, each naming where
  (`ClipAt`, `ClipPair`, `IdClash`, `RangeAt` with `ClipRange`, `EdgeAt`,
  `TransitionAt`, `Place`, `Mismatch`). All at once, in a fixed order.

- **`layout`**: a valid timeline's clips at their records and the gaps
  between them, derived for export.

- **`diff`**: per track in record order, each clip `Added`, `Removed`,
  `Moved`, `Retimed`, `Regained`, `EnabledFlipped`, `Renamed` or
  `Relinked`, a clip matched by its id alone — a side carrying one id on two
  clips is refused as `Ambiguous` (its `Side` and `IdClash`) rather than
  matched by occurrence; `diff(a, a)` is empty.

- **`otio::to_otio`**: OpenTimelineIO JSON for `OtioTarget::V0_15Plus`
  (`Clip.2`) or `OtioTarget::Legacy` (`Clip.1`) — `global_start_time`
  always written, gaps from the layout, a clip's source range whole in its
  medium's one ruler, a dissolve as `SMPTE_Dissolve`, a fade as a dissolve
  against a gap, a clip's id, gain and reel in `metadata` (DaVinci Resolve
  applies neither gain nor reel). Every count written lies within ±2^53,
  where OpenTimelineIO's `f64` holds every whole number; a media-side range
  its own rulers count past that is written in the coarsest whole-rate
  ruler that holds it, and of its own rulers in the first that ends it
  within ±2^53 too — a source range none ends is written in one that
  writes its start and its length, its end left to the walk and the
  search; a source range none of them writes, in the first ruler the
  timeline's operands are counted in that does, finest first; and an
  available range none of them ends, in the first of those rulers that
  ends it too, its end held as it is planned (each one at a fractional
  rate: every whole rate that lands on a range is a multiple of the
  coarsest). Every count OpenTimelineIO derives from the document —
  the end of each clip's source range; each child's place on its track, from zero
  in the child's own rate, and in the timeline; each item's visible range
  with its neighbouring transitions' handles; each track's duration and the
  stack's, the longest as OpenTimelineIO picks it; the global start added
  to each place and each track's end; each child's range from the global
  start, the moved start and the child's own duration, as OpenTimelineIO
  moves a range into its parent's; a timeline with no track's range from
  the global start, which OpenTimelineIO gives no duration at rate 1; the
  last tick of every one of those ranges, `end_time_inclusive`, branch for
  branch — is computed in OpenTimelineIO's own arithmetic beside its exact
  value, and held within ±2^53 in the ruler OpenTimelineIO carries it in,
  its double less than half a tick from the exact count. Exact, or
  refused: where a derived value would round, a
  search bounded by contract writes the clips it is formed from in other
  rulers that hold their source ranges — a clip's plan's, then every ruler
  the timeline's operands are counted in (the edit rate, 1, every clip's
  planned ruler), never capped, then the 64 finest and the 64 coarsest
  whole rates below its plan's — one clip one ruler at a time, the finest
  first, within `1 + n · (d + 127)` walks for `n` clips and `d` operand
  rulers. Every range ends where it is stored, and the exported items, laid
  end to end, put every clip on its record. Refuses with `otio::Refused`:
  `Validation` with `validate`'s refusals, or `NotRepresentable` — an
  `otio::Spot` (`otio::ChildAt` for a child of an exported track,
  `TimelineEnd` for the end of a timeline with no track), the count and
  the rate it is counted at, and for a count OpenTimelineIO derives, a
  source range's end among them, what the search tried,
  `otio::RulerSearch`: its bands (`otio::RulerBand`) and its walks — where
  no plan the search tries holds a count, naming the last plan's. A count
  written at the edit rate is refused before any walk, naming no search; a
  source range no ruler of the bands writes, and an available range none
  of them holds whole — none of its own, none the timeline's operands are
  counted in — are refused before any walk too, naming the bands and no
  walk. A timeline only a ruler outside the bands would hold is refused by
  that contract, not misread.

- **`otio::validate_json`**: the structural self-check — a strict JSON
  reader and OpenTimelineIO's schema shape, requiring exactly the keys each
  target's readers require — with no Python dependency.

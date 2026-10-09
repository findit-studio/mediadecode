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
  applies neither gain nor reel). Every count written, every range's end
  and every place a record starts or ends lies within ±2^53, where
  OpenTimelineIO's `f64` holds every whole number; a media-side range its
  own rulers count past that is written in the coarsest whole-rate ruler
  that holds it. Every range ends where it is stored, and the exported
  items, laid end to end, put every clip on its record. Refuses with
  `otio::Refused`: `Validation` with `validate`'s refusals, or
  `NotRepresentable` (an `otio::Spot` and the count) where no ruler holds a
  count.

- **`otio::validate_json`**: the structural self-check — a strict JSON
  reader and OpenTimelineIO's schema shape, requiring exactly the keys each
  target's readers require — with no Python dependency.

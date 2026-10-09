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
  `enabled`, `clips`, `transitions`), `Clip` (`name`, `media`,
  `source_range` in the medium's timebase, `record` at the edit rate,
  `enabled`, `gain`, `fades`, `metadata`), `MediaRef` (`locator`, and when
  known `available_range`, `rate`, `reel`), `Transition` (a `Dissolve` at a
  cut, with OpenTimelineIO's `in_offset` and `out_offset`), `Fades` and
  `Fade` (`Linear` or `EqualPower`), `Gain` (finite decibels) and
  `Metadata` (an ordered string map). Pure Rust on `mediatime` 0.5 and
  `serde`; `no_std` + `alloc`; no FFmpeg.

  Positions are explicit: a clip's record is stored, never derived from the
  clips before it, so trimming one clip moves no other. With no time-warp in
  schema 1 a record runs as long as its source, rescaled to the edit rate to
  the nearest tick; `speed` is reserved for a later schema.

  On the wire the document's first field is `schema`; an unknown schema, a
  field this reader does not know, a required field left out, a metadata key
  named twice and a gain that is not finite are refused by name.

- **`validate`**, refusing by name: `RateUnstated`, `OffEditRate`,
  `MediaRateUnstated`, `RecordBeforeZero`, `EmptyRecord`, `OutOfOrder`,
  `Overlap`, `DurationMismatch`, `OutsideAvailable`, `OffBoundary`,
  `DuplicateTransition`, `HandleMissing`, `FadeMeetsTransition` and
  `BlendsOverrunClip`, each naming where (`ClipAt`, `ClipPair`, `EdgeAt`,
  `TransitionAt`, `Place`, `Mismatch`). All at once, in a fixed order.

- **`layout`**: a valid timeline's clips at their records and the gaps
  between them, derived for export.

- **`diff`**: per track in record order, each clip `Added`, `Removed`,
  `Moved`, `Retimed`, `Regained` or `EnabledFlipped`, a clip identified by
  its name and its medium's locator; `diff(a, a)` is empty.

- **`otio::to_otio`**: OpenTimelineIO JSON for `OtioTarget::V0_15Plus`
  (`Clip.2`) or `OtioTarget::Legacy` (`Clip.1`) — `global_start_time`
  always written, gaps from the layout, a dissolve as `SMPTE_Dissolve`, a
  fade as a dissolve against a gap, gain and reel in `metadata` (which
  DaVinci Resolve does not apply). Every number written is exact, and the
  exported items, laid end to end, put every clip on its record.

- **`otio::validate_json`**: the structural self-check — a strict JSON
  reader and OpenTimelineIO's schema shape — with no Python dependency.

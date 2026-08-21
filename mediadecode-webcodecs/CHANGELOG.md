# Changelog

All notable changes to the [`mediadecode-webcodecs`](https://crates.io/crates/mediadecode-webcodecs)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The backend-agnostic core it adapts has its own log at
[`mediadecode/CHANGELOG.md`](../mediadecode/CHANGELOG.md).

## [Unreleased]

### Changed (BREAKING)

- **`AudioAdapter::ChannelLayout` binds
  `mediaframe::audio::ChannelLayoutDescription`.** `mediadecode::channel`
  is gone (see
  [`mediadecode`](../mediadecode/CHANGELOG.md#unreleased)) and the
  channel vocabulary lives in `mediaframe::audio` now. The adapter's
  conduct is unchanged: WebCodecs reports a channel count and no layout
  name, so `boundary::empty_audio_frame` and the decode path still
  build `ChannelLayoutDescription::new(N)` — the count with an
  `Unspecified` order — and `default()` is still the zero-channel
  placeholder.

- **`mediaframe` is a direct `wasm32` dependency**, pinned with
  `alloc`: the audio household is compiled only at the alloc-or-std
  tier.

## [0.5.0]

Tracks `mediadecode` 0.5.0, which crosses `mediaframe` 0.4 → 0.5 — a
breaking minor in a public dependency whose `PixelFormat` and `color`
types this adapter re-exports. See
[`mediadecode` 0.5.0](../mediadecode/CHANGELOG.md#050). No adapter
source line moved and no adapter behaviour changes.

## [0.4.0] - 2026-08-19

Tracks `mediadecode` 0.4.0, which crosses `mediatime` 0.1 → 0.3 and
`mediaframe` 0.1 → 0.3 — two breaking minors each. See
[`mediadecode` 0.4.0](../mediadecode/CHANGELOG.md#040). No adapter
behaviour changes.

### Changed (BREAKING)

- **The empty destination frame's pixel format is now
  `PixelFormat::None`.** `boundary::empty_video_frame` filled the
  placeholder with `PixelFormat::Unknown(0)`; mediaframe 0.3 struck
  that variant, and `None` is its named "no format yet" member (and
  the `Default`) — which is what the placeholder always meant. The
  adapter overwrites it on every successful decode, as before.
- **`WebCodecsVideoFrame::format` and `::color` are no longer
  `const`** and return clones. `mediaframe::PixelFormat` and
  `mediaframe::color::Info` lost `Copy` in 0.3. Both signatures are
  otherwise unchanged.
- **`Timebase` construction is signed.** `mediatime` 0.2 made
  `Timebase`'s numerator and denominator `i32` / `NonZeroI32`; the
  crate's two `MICROS` constants and the wasm integration tests move
  from `NonZeroU32` to `NonZeroI32`.

### Changed

- **`mediadecode` dep**: bumped to `0.4`.
- `video::expected_plane_layout` (private) takes `&PixelFormat`.
- README: the install snippet said the crate was unpublished and
  pinned `"0.0"`; it has been on crates.io since 0.3.1. Now `"0.4"`.

[0.4.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-webcodecs-v0.4.0

## [0.3.1] - 2026-06-14

Version-tracking release
([#10](https://github.com/findit-studio/mediadecode/pull/10)). No adapter
source changed.

### Changed

- **`mediadecode` dep**: bumped to `0.3.1`, which adds `Clone` to
  `AudioFrame` — see
  [`mediadecode` 0.3.1](../mediadecode/CHANGELOG.md#031---2026-06-14).

[0.3.1]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-webcodecs-v0.3.1

## [0.3.0] - 2026-06-07

Tracks `mediadecode` 0.3.0, which flips the shared vocabulary crate from
`videoframe` 0.2 to `mediaframe` 0.1
([#7](https://github.com/findit-studio/mediadecode/pull/7),
[#8](https://github.com/findit-studio/mediadecode/pull/8)). No adapter source
changed — but the vocabulary this crate re-exports through its frame
types is a different crate's now, so the release is breaking for its
consumers all the same. See
[`mediadecode` 0.3.0](../mediadecode/CHANGELOG.md#030---2026-06-07).

### Changed (BREAKING)

- **`mediadecode` dep**: bumped to `0.3`.

### Changed

- Version bumped to 0.3.0 with the rest of the workspace.

[0.3.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-webcodecs-v0.3.0

## [0.2.0] - 2026-05-15

Tracks the `mediadecode` 0.2.0 / `videoframe` 0.2 cutover
([#5](https://github.com/findit-studio/mediadecode/pull/5)).

### Changed

- The `PixelFormat::Unknown` boundary fallback in
  `webcodecs_pixel_format_to_mediadecode` preserves the raw
  WebCodecs identifier via `PixelFormat::Unknown(raw as u32)`
  instead of collapsing to a unit variant.

### Added

- `#[must_use]` added to every `with_*` consuming builder method.
- New `tests/native_stub.rs` — verifies the crate compiles to an
  empty stub on non-wasm32 targets and that no wasm-only names
  leak through. Closes
  [issue #4 — finding 4](https://github.com/findit-studio/mediadecode/issues/4).

[0.2.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-webcodecs-v0.2.0

## [0.0.0] - 2026-05-15

Placeholder publish — the name is reserved and the shape is scaffolded;
there is no public API yet
([#3](https://github.com/findit-studio/mediadecode/pull/3)).

### Added

- Crate scaffolded: workspace member, `wasm32`-gated `web-sys`
  dependency surface, design spec captured in
  `docs/superpowers/specs/2026-05-09-webcodecs-design.md`.
  Public API lands in a subsequent release.

[0.0.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-webcodecs-v0.0.0

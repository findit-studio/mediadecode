# Changelog

All notable changes to the [`mediadecode`](https://crates.io/crates/mediadecode)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The sibling FFmpeg adapter has its own log at
[`mediadecode-ffmpeg/CHANGELOG.md`](../mediadecode-ffmpeg/CHANGELOG.md).

## [Unreleased]

## [0.2.0] - 2026-05-15

The shared pixel-vocabulary layer (`color`, `cfa`, `pixel_format`,
frame primitives) now lives in the dedicated
[`videoframe`](https://crates.io/crates/videoframe) crate, so colconv,
mediadecode, and scenesdetect share a single canonical definition of
these types. mediadecode keeps the decoder-output story (timestamped
frames + per-backend extras) — pixel and color vocabulary are
re-exports.

### Changed (BREAKING)

- **`PixelFormat::Unknown` shape**: now `Unknown(u32)` (tuple variant
  carrying the raw wire identifier) instead of the prior unit
  variant. Lossless round-trip via `from_u32` / `to_u32`. Callers
  matching the variant must switch from `PixelFormat::Unknown` to
  `PixelFormat::Unknown(_)` (or `Unknown(raw)` if the raw value is
  useful). Boundary adapters (`mediadecode-ffmpeg`,
  `mediadecode-webcodecs`) have been updated to preserve the raw
  FFmpeg / WebCodecs identifier through the cast.
- **`FrameError` variants** are now newtype-tuple form wrapping
  payload structs (matches the convention in
  [`videoframe`](https://crates.io/crates/videoframe)). Affected
  variants: `TooManyVideoPlanes`, `TooManyAudioPlanes`. Callers
  destructuring `Err(FrameError::TooManyVideoPlanes { plane_count })`
  must switch to `Err(FrameError::TooManyVideoPlanes(p))` and call
  `p.plane_count()`. The payload structs
  ([`frame::TooManyVideoPlanes`](https://docs.rs/mediadecode/0.2/mediadecode/frame/struct.TooManyVideoPlanes.html),
  [`frame::TooManyAudioPlanes`](https://docs.rs/mediadecode/0.2/mediadecode/frame/struct.TooManyAudioPlanes.html))
  carry the same `plane_count: u8` and expose it via a
  `pub const fn plane_count(&self) -> u8` accessor. Both variants
  also carry `#[from]`, so `impl From<TooManyVideoPlanes> for FrameError`
  / `impl From<TooManyAudioPlanes> for FrameError` are auto-generated
  — inner helpers returning `Result<_, TooManyVideoPlanes>` can be
  `?`-propagated directly into `FrameError`.
- **`PixelFormat` enum body**: now sourced from
  [`videoframe::pixel_format::PixelFormat`](https://docs.rs/videoframe/0.2/videoframe/pixel_format/enum.PixelFormat.html)
  and covers **every** FFmpeg `n8.1` `AVPixelFormat` slug (~270 variants,
  closed against FFmpeg's vendored slug list via `cargo xtask check`)
  plus cinema-RAW additions. The previously-shipped subset (NV12, P010
  / P012 / P016, P210 / P212 / P216, P410 / P412 / P416, YUV420P, RGB24,
  …) is a strict subset of the new set, so most existing match arms
  still resolve; matches that relied on the enum being closed at the
  prior list will need updating (FFmpeg-derived sources now feed
  variants like `Yuv411p`, `Yuv410p`, `Yuv440p`, `Y210`, `V210`,
  `Xv36`, `Vuya`, `Bayer*`, `Xyz12`, etc.).

### Changed

- **`mediadecode::color::*`** (`ColorMatrix`, `ColorPrimaries`,
  `ColorTransfer`, `ColorRange`, `ChromaLocation`, `ColorInfo`,
  `DcpTargetGamut`) now re-export from `videoframe::color::*`. Public
  import paths (`mediadecode::color::ColorMatrix`, etc.) keep
  resolving — no source-level break for consumers.
- **`mediadecode::cfa::BayerPattern`** re-exports from
  `videoframe::frame::BayerPattern` (videoframe 0.2 dropped its
  separate `cfa` module; the type lives under `frame::bayer` and is
  re-exported via `frame::*`).
- **`mediadecode::frame::{Dimensions, Rect, Plane}`** re-export from
  `videoframe::frame::*`. The structural primitives are now the
  canonical videoframe definitions; the type identity is
  cross-crate-equal so values can flow without conversion.
- **Decoder-output types unchanged.** `VideoFrame<P, E, D>`,
  `AudioFrame<S, C, E, D>`, `SubtitleFrame<E, D>` remain in
  mediadecode — they carry timestamp + backend-extras, which sit
  above the pure pixel-vocabulary layer.

### Added

- **`videoframe`** as a new required dep (`videoframe = "0.2"`).
  Enabled with `features = ["frame"]` so every per-family pixel-format
  borrow type is available to downstream consumers.
- **`#[must_use]`** on every consuming `with_*` builder method
  across frame / packet / subtitle types. Catches accidental
  discards of the returned value at compile time.
- **`VideoFrame::try_new`** / **`AudioFrame::try_new`** —
  panic-free constructors returning `Result<Self, FrameError>`.
  The existing `new` constructors keep their panicking behavior
  for `const fn` / statically-known call sites; `try_new` is for
  runtime-checked callers (e.g. backend adapters validating
  decoder output). Pairs the `new` / `try_new` convention the
  rest of the crate already follows
  (`Plane::new` / `Plane::try_new`, `*_empty` / `try_*_empty`,
  …).
- **`mediadecode::frame::FrameError`** — enum capturing the
  validation failures the `try_new` constructors can surface
  (`TooManyVideoPlanes` / `TooManyAudioPlanes`). `non_exhaustive`,
  `IsVariant`, `thiserror::Error`.

### Fixed

- **`plane_count` validated against the fixed plane-array
  capacity.** `VideoFrame::new` asserts `plane_count <= 4`,
  `AudioFrame::new` asserts `plane_count <= 8`. Previously,
  out-of-range values would panic later inside `planes()` /
  `samples()`; now they fail-fast at construction.
  Closes [issue #4 — finding 1](https://github.com/Findit-AI/mediadecode/issues/4).

[0.2.0]: https://github.com/findit-ai/mediadecode/releases/tag/mediadecode-v0.2.0

## [0.1.0] - 2026-05-09

Initial public release.

### Added

- **Core enums.** `PixelFormat` (closed enum covering CPU and HW-tile
  formats: NV12, P010 / P012 / P016, P210 / P212 / P216, P410 / P412 /
  P416, YUV420P, RGB24, …), `SampleFormat`, `AudioChannelLayout`, and
  `BayerPattern` for RAW.
- **Color metadata.** H.273-aligned `ColorMatrix`, `ColorPrimaries`,
  `ColorTransfer`, `ColorRange`, `ChromaLocation`, plus the bundled
  `ColorInfo` type with `const fn` getters / `with_*` builders /
  `set_*` mutators.
- **Generic packet types.** `VideoPacket<A, B>`, `AudioPacket<A, B>`,
  `SubtitlePacket<A, B>` with the `PacketFlags` bitflags
  (`KEY` / `CORRUPT` / `DISCARD`).
- **Generic frame types.** `VideoFrame<A, B>`, `AudioFrame<A, B>`,
  `SubtitleFrame<A, B>`, alongside the `Plane<B>` plane carrier, the
  `Rect` rectangle, and the alloc-gated `SubtitlePayload<B>::Bitmap`
  variant.
- **Adapter traits.** `VideoAdapter`, `AudioAdapter`,
  `SubtitleAdapter` — fix the `extras` and `buffer` types for a
  whole pipeline once.
- **Decoder traits.** `VideoStreamDecoder`, `AudioStreamDecoder`,
  `SubtitleStreamDecoder` (push-style `send_packet` / `receive_frame`
  / `send_eof` / `flush` shape) plus `VideoFrameSource` /
  `AudioFrameSource`.
- **Time primitives.** `Timebase`, `Timestamp`, `TimeRange` re-exported
  from [`mediatime`](https://crates.io/crates/mediatime) so consumers
  don't need a separate dependency.
- **API style.** All public fields private; access via `field()`
  getters, consuming `with_field(value)` builders, and `set_field`
  mutators returning `&mut Self`. `const fn` everywhere the type
  allows. Panicking constructors paired with fallible `try_*`
  counterparts.
- **`no_std` core.** Builds without `std` or `alloc`; opt-in `alloc` /
  `std` features. Errors via `thiserror` over the stable
  `core::error::Error`, so `Error` impls survive
  `--no-default-features`.
- **Optional features.** `serde`, `arbitrary`, `quickcheck` (each
  forwards to `mediatime`'s matching feature).

[0.1.0]: https://github.com/findit-ai/mediadecode/releases/tag/mediadecode-v0.1.0

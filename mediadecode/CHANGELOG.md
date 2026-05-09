# Changelog

All notable changes to the [`mediadecode`](https://crates.io/crates/mediadecode)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The sibling FFmpeg adapter has its own log at
[`mediadecode-ffmpeg/CHANGELOG.md`](../mediadecode-ffmpeg/CHANGELOG.md).

## [Unreleased]

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

[Unreleased]: https://github.com/findit-ai/mediadecode/compare/mediadecode-v0.1.0...HEAD
[0.1.0]: https://github.com/findit-ai/mediadecode/releases/tag/mediadecode-v0.1.0

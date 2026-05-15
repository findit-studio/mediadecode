# Changelog

All notable changes to the [`mediadecode-ffmpeg`](https://crates.io/crates/mediadecode-ffmpeg)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The backend-agnostic core it adapts has its own log at
[`mediadecode/CHANGELOG.md`](../mediadecode/CHANGELOG.md).

## [Unreleased]

## [0.2.0] - 2026-05-15

Tracks `mediadecode` 0.2.0. The pixel-vocabulary types
(`PixelFormat`, color enums, frame primitives) now live in the
`videoframe` crate and are re-exported through `mediadecode`; this
release adapts the FFmpeg boundary to the new `PixelFormat::Unknown(u32)`
shape and updates the type aliases the crate re-exports.

### Changed (BREAKING)

- **`PixelFormat::Unknown` shape**: re-exported `PixelFormat` is now
  `Unknown(u32)` (tuple variant) instead of the prior unit variant
  — see [`mediadecode` 0.2.0](../mediadecode/CHANGELOG.md#020---2026-05-15).
- **FFmpeg boundary fallback** now preserves the raw `AVPixelFormat`
  identifier through `PixelFormat::Unknown(raw as u32)` instead of
  collapsing to a bare `Unknown`. Round-trips losslessly via
  `PixelFormat::{from_u32, to_u32}`.
- **Type aliases reshape**: `VideoFrame`, `AudioFrame`,
  `SubtitleFrame`, `VideoPacket`, `AudioPacket`, `SubtitlePacket`
  inherit the upstream `PixelFormat` shape change. Downstream
  callers matching on `Unknown` in destination frames need to
  switch to `Unknown(_)`.
- **`Error` enum variants** are now newtype-tuple form wrapping
  payload structs (matches the convention in
  [`videoframe`](https://crates.io/crates/videoframe)). Affected
  variants: `HwDeviceInitFailed`, `AllBackendsFailed`,
  `FallbackFailed`. Pure tuple variants (`Ffmpeg`, `NoCodec`,
  `BackendUnsupportedByCodec`) unchanged. Callers destructuring
  `Err(Error::AllBackendsFailed { attempts, .. })` must switch to
  `Err(Error::AllBackendsFailed(p))` and call `p.attempts()` /
  `p.unconsumed_packets()`. Owning-move paths for the rescued
  packets are preserved via `p.into_unconsumed_packets()` /
  `p.into_parts()`, so non-seekable callers can still relinquish
  the `Vec<Packet>` without cloning. The hand-written `Debug` that
  printed `[N packets]` (because `ffmpeg_next::Packet` has no
  `Debug`) now lives on the payload structs.
  All three new variants also carry `#[from]`, joining `Ffmpeg`
  which already had it — so `impl From<HwDeviceInitFailed> for Error`,
  `impl From<AllBackendsFailed> for Error`, and
  `impl From<FallbackFailed> for Error` are auto-generated, and
  helpers returning `Result<_, HwDeviceInitFailed>` etc. can be
  `?`-propagated into `Result<_, Error>` directly.

### Changed

- **`mediadecode` dep**: bumped to `0.2`.
- Boundary mapping in `pixel_format_from_ffmpeg` and the
  side-data conversion paths updated to the new
  `PixelFormat::Unknown(u32)` shape (17 fallback / assertion /
  default-frame sites across `mediadecode-ffmpeg` and
  `mediadecode-webcodecs`).

### Added

- **`Debug` impl for `Frame`** — manual `core::fmt::Debug` impl
  showing dimensions / format so the only public type previously
  without `Debug` is now printable.
  Closes [issue #4 — finding 2](https://github.com/Findit-AI/mediadecode/issues/4).
- **`#[must_use]`** on every consuming `with_*` builder method
  across the crate's public surface.
  Closes [issue #4 — finding 3](https://github.com/Findit-AI/mediadecode/issues/4).

[0.1.0]: https://github.com/findit-ai/mediadecode/releases/tag/mediadecode-ffmpeg-v0.1.0
[0.2.0]: https://github.com/findit-ai/mediadecode/releases/tag/mediadecode-ffmpeg-v0.2.0

## [0.1.0] - 2026-05-09

Initial public release.

### Added

- **Adapter type.** `Ffmpeg` zero-sized type implementing
  `mediadecode::adapter::VideoAdapter`, `AudioAdapter`, and
  `SubtitleAdapter`.
- **Buffer.** `FfmpegBuffer` — zero-copy refcounted view over an
  `AVBufferRef`, with `empty` / `from_packet` / `from_plane`
  constructors and panic-free `try_*` counterparts.
- **Video decoder.** `FfmpegVideoStreamDecoder` mirrors
  `ffmpeg::decoder::Video`'s `send_packet` / `receive_frame` shape and
  auto-probes the host's HW backends — VideoToolbox on Apple,
  VAAPI / CUDA on Linux, D3D11VA / CUDA on Windows — falling through
  to a software decoder when none open. `open_with(_, _, Backend::…)`
  pins a specific backend (no probe).
- **Audio decoder.** `FfmpegAudioStreamDecoder` over
  `ffmpeg::decoder::Audio`, producing zero-copy `AudioFrame`s.
- **Subtitle decoder.** `FfmpegSubtitleStreamDecoder` over the legacy
  synchronous `ffmpeg::decoder::Subtitle::decode` API, bridged into the
  trait's `send_packet` / `receive_frame` shape.
- **Type aliases.** `VideoPacket`, `AudioPacket`, `SubtitlePacket`,
  `VideoFrame`, `AudioFrame`, `SubtitleFrame` — pre-parameterized with
  this crate's adapter, buffer, and extras types.
- **Boundary helpers.** `video_packet_from_ffmpeg`,
  `audio_packet_from_ffmpeg`, `subtitle_packet_from_ffmpeg` — convert a
  borrowed `ffmpeg::Packet` into the matching `mediadecode` packet
  without copying the compressed payload. Empty-frame builders
  `empty_video_frame`, `empty_audio_frame`, `empty_subtitle_frame`
  produce well-formed destinations for `receive_frame`.
- **Recovery.** `VideoDecodeError::AllBackendsFailed { unconsumed_packets, .. }`
  carries any packets the decoder had already accepted from the
  demuxer when every backend is exhausted, so non-seekable callers
  (live streams, pipes, network sources) can replay them through their
  own software decoder without re-demuxing.

### Safety

The FFmpeg FFI surface is hardened against malformed or
version-skewed decoder output:

- All bindgen enum reads go through `addr_of!` + `read_unaligned` to
  avoid creating invalid Rust enum values from raw memory.
- `AVFrameSideDataType` values are mapped through an explicit
  whitelist of known `AV_FRAME_DATA_*` constants — never `transmute`d.
- `CStr::from_ptr` calls are replaced with a bounded
  `bounded_cstr_bytes` helper that searches at most
  `SUBTITLE_MAX_TEXT_BYTES_PER_RECT + 1` bytes for a NUL terminator.
- Signed counts (`AVFrame.nb_side_data`, `AVSubtitle.num_rects`, …)
  are clamped to non-negative values before any `as usize` cast,
  preventing OOB walks under corrupt input.
- Side-data and subtitle conversions enforce caps on entries and total
  bytes (`SIDE_DATA_MAX_ENTRIES`, `SIDE_DATA_MAX_TOTAL_BYTES`,
  `HW_COPY_SIDE_DATA_MAX_*`, `SUBTITLE_MAX_*`).
- `send_packet` consumes the demuxer packet only after the probe
  rescue records it, so a non-seekable caller can rebuild the input
  stream from `unconsumed_packets` on `AllBackendsFailed`.
- `cpu_frame_bytes` sizes against the underlying `AVBufferRef.size`
  rather than `linesize × plane_height_for(AVFrame.height)`, so
  cropped or heavily aligned streams report correct byte counts.


# mediadecode — design spec

**Status:** draft, awaiting user review
**Date:** 2026-05-08
**Authors:** uqio + Claude

## 1. Purpose

`mediadecode` is a generic, no_std-friendly, wasm-friendly Rust crate that
provides a unified type vocabulary above real media decoders (FFmpeg,
WebCodecs, RED R3D, Blackmagic BRAW, ARRIRAW, Sony X-OCN, Apple ProRes RAW,
Canon Cinema RAW Light, …). Its scope is **types and traits only** — no
decoder implementation lives in this crate. Concrete decoders are
downstream adapter crates (`mediadecode-ffmpeg`, `mediadecode-webcodecs`,
`mediadecode-r3d`, `mediadecode-braw`, …) that implement the traits defined
here while emitting the unified data types.

## 2. Pipeline context

```
[decoder backend]  -> mediadecode::VideoFrame<A, B>
                       |
                       v dispatch by VideoFrame::pixel_format()
[downcast]         -> colconv::{Yuv420pFrame, Nv12Frame, BayerFrame, …}<'a>
                       |
                       v with a colconv::PixelSink
[converter]        -> scenesdetect::{LumaFrame, RgbFrame, HsvFrame}<'a>
                       |
                       v
[detector]         -> scenesdetect::{histogram, phash, threshold, content,
                                     adaptive, keyframe} events
```

Mediadecode's job is the leftmost arrow — turn whatever the backend hands
back into a single, neutral envelope downstream tooling can dispatch off of.

## 3. License and crate boundaries

- `mediadecode`: MIT OR Apache-2.0
- `colconv`: GPL-3.0-or-later — its kernels and the per-format `*Frame<'a>`
  types live there; `mediadecode` does **not** depend on it.
- `mediatime`: MIT OR Apache-2.0; mediadecode re-exports its public
  `Timebase` / `Timestamp` / `TimeRange`.
- `scenesdetect`: MIT OR Apache-2.0; sits downstream of `colconv`.

**Architectural constraint:** mediadecode imports no other crate from the
findit-studio workspace except `mediatime`. This keeps mediadecode usable
in non-GPL pipelines.

## 4. Conventions

These mirror `mediatime` and `colconv`:

- All struct fields are `private`. Every public type exposes:
  - `pub const fn field(&self) -> T` — getter (`const fn` whenever possible)
  - `pub const fn with_field(mut self, v: T) -> Self` — consuming builder
    (`const fn` for `Copy` field types; plain `fn` for non-`Copy`)
  - `pub const fn set_field(&mut self, v: T) -> &mut Self` — in-place
    mutator returning `&mut Self` for chaining
- `#[derive(Debug, Clone, Copy)]` where it makes sense; closed enums are
  `#[non_exhaustive]` (no `Reserved(u8)` escape hatch — `non_exhaustive`
  serves the same purpose).
- Validating constructors are `try_new(...) -> Result<Self, FooError>`,
  with a panicking sibling `new(...) -> Self`.
- `derive_more::IsVariant` on every public enum that warrants it.
- `thiserror`-derived error enums per backend; **mediadecode does not
  define a crate-level error type** (each adapter picks its own).
- MSRV **1.95**, edition **2024**.

## 5. Crate layout

```
mediadecode/
  Cargo.toml
  src/
    lib.rs        // no_std boilerplate, re-exports
    adapter.rs    // VideoAdapter, AudioAdapter, SubtitleAdapter
    packet.rs     // VideoPacket, AudioPacket, SubtitlePacket, PacketFlags
    frame.rs      // VideoFrame, AudioFrame, SubtitleFrame, Plane, Rect
    color.rs      // ColorInfo + ColorPrimaries/ColorTransfer/ColorMatrix/
                  //   ColorRange/ChromaLocation
    cfa.rs        // BayerPattern (copied verbatim from colconv::raw)
    subtitle.rs   // SubtitlePayload, BitmapRegion (alloc-gated)
    decoder.rs    // {Video,Audio}{StreamDecoder,FrameSource}, SubtitleDecoder
```

## 6. Type-and-trait spine

### 6.1 Adapter traits

A backend impl sets the *vocabulary* (codec ids, format ids, side-data /
extras shapes) for each media kind it handles. Buffer is **not** part of
this trait — it's a struct generic on Packet/Frame.

```rust
pub trait VideoAdapter {
    type CodecId:     Copy + Eq + core::fmt::Debug;
    type PixelFormat: Copy + Eq + core::fmt::Debug;
    type PacketExtra;
    type FrameExtra;
}

pub trait AudioAdapter {
    type CodecId:       Copy + Eq + core::fmt::Debug;
    type SampleFormat:  Copy + Eq + core::fmt::Debug;
    type ChannelLayout: Clone + Eq + core::fmt::Debug;
    type PacketExtra;
    type FrameExtra;
}

pub trait SubtitleAdapter {
    type CodecId:    Copy + Eq + core::fmt::Debug;
    type PacketExtra;
    type FrameExtra;
}
```

A backend that handles only video impls only `VideoAdapter`. RED R3D /
ARRIRAW / Sony X-OCN / Canon Cinema RAW Light are video-only here.

### 6.2 Packet types

```rust
bitflags::bitflags! {
    pub struct PacketFlags: u8 {
        const KEY     = 0b001;
        const CORRUPT = 0b010;
        const DISCARD = 0b100;
    }
}

pub struct VideoPacket<A: VideoAdapter, B: AsRef<[u8]>> {
    pts:      Option<Timestamp>,
    dts:      Option<Timestamp>,
    duration: Option<Timestamp>,
    flags:    PacketFlags,
    data:     B,
    extra:    A::PacketExtra,
}
// AudioPacket<A: AudioAdapter, B>, SubtitlePacket<A: SubtitleAdapter, B>
//   — same shape minus media-specific fields.
```

Common-fields rationale (validated against AVPacket, EncodedVideoChunk,
CMSampleBuffer): `pts`/`dts`/`duration` exist on every packet-bearing
backend; `flags::KEY` covers FFmpeg `AV_PKT_FLAG_KEY`, WebCodecs
`'key'`/`'delta'`, ProRes RAW `kCMSampleAttachmentKey_NotSync`. Everything
else (FFmpeg side-data, ProRes attachments, WebCodecs metadata) goes in
`extra`. R3D/BRAW/ARRIRAW/X-OCN/Canon RAW Light are frame-by-index — they
do not produce `Packet`s.

### 6.3 Frame types

```rust
pub struct Plane<B> { data: B, stride: u32 }

pub struct Rect { x: u32, y: u32, width: u32, height: u32 }

pub struct VideoFrame<A: VideoAdapter, B: AsRef<[u8]>> {
    pts:           Option<Timestamp>,
    duration:      Option<Timestamp>,
    width:         u32,                 // coded
    height:        u32,                 // coded
    visible_rect:  Option<Rect>,        // FFmpeg crop / WebCodecs visibleRect / ProRes CleanAperture
    pixel_format:  A::PixelFormat,
    plane_count:   u8,
    planes:        [Plane<B>; 4],       // NV12=2, YUV420P=3, YUVA=4, Bayer CFA=1, packed RGB=1
    color:         ColorInfo,
    extra:         A::FrameExtra,
}

pub struct AudioFrame<A: AudioAdapter, B: AsRef<[u8]>> {
    pts:            Option<Timestamp>,
    duration:       Option<Timestamp>,
    sample_rate:    u32,
    nb_samples:     u32,            // per channel
    channel_count:  u8,
    sample_format:  A::SampleFormat,
    channel_layout: A::ChannelLayout,
    plane_count:    u8,             // 1 packed, channel_count planar (≤ 8)
    planes:         [Plane<B>; 8],  // FFmpeg AV_NUM_DATA_POINTERS = 8
    extra:          A::FrameExtra,
}

pub struct SubtitleFrame<A: SubtitleAdapter, B: AsRef<[u8]>> {
    pts:      Option<Timestamp>,
    duration: Option<Timestamp>,
    payload:  SubtitlePayload<B>,
    extra:    A::FrameExtra,
}

pub enum SubtitlePayload<B: AsRef<[u8]>> {
    Text { text: B, language: Option<[u8; 3]> /* ISO 639-2 */ },
    #[cfg(feature = "alloc")]
    Bitmap { regions: alloc::vec::Vec<BitmapRegion<B>> },
}
```

Common-fields rationale (validated against AVFrame, WebCodecs VideoFrame,
CVPixelBuffer, R3D output, BRAW IBlackmagicRawProcessedImage, ARRIRAW SDK,
Canon CRX/LibRaw, Sony RAW Viewer / Nablet AMA outputs):

| Field on `VideoFrame` | Justification |
|---|---|
| `pts` / `duration` | All push-style backends carry them; pull-style backends derive from frame index + frame rate. |
| `width` / `height` | Universal. |
| `visible_rect` | FFmpeg crop, WebCodecs visibleRect, ProRes CleanAperture; cinema RAW SDKs typically set it to `None`. |
| `pixel_format` | Caller-selected on RAW SDKs; backend-supplied on FFmpeg/WebCodecs/ProRes. Always backend-typed. |
| `plane_count` + `[Plane; 4]` | Covers every realistic format: NV12=2, YUV420P=3, YUVA=4, packed RGB / Bayer CFA = 1. |
| `color` | All backends except R3D/BRAW expose it natively. RAW backends populate from clip-level color science (LogC3/LogC4, S-Log3, Canon Log) and leave `Unspecified` if absent. |
| `extra` | HDR mastering display, RAW sensor metadata, picture type, interlace, codec-specific fields (DV RPU, AV1 OBUs). |

`AudioFrame::planes` capped at 8 mirrors FFmpeg's `AV_NUM_DATA_POINTERS`;
larger channel counts go via `extra`. RAW SDK audio (R3D 1/2/4 channels, BRAW
≤16) fits comfortably.

### 6.4 Color and CFA enums

Copied verbatim from `colconv` (which still owns its own copy until a future
flag-day migrates it to import from mediadecode):

- `ColorMatrix` — Bt601 / Bt709 / Bt2020Ncl / Smpte240m / Fcc / YCgCo
- `BayerPattern` — Bggr / Rggb / Grbg / Gbrg

New H.273-aligned siblings authored fresh in mediadecode:

- `ColorPrimaries` — Bt709, Unspecified, Bt470M, Bt470Bg, Smpte170M,
  Smpte240M, Film, Bt2020, SmpteSt428, SmpteRp431, SmpteEg432, Ebu3213E
- `ColorTransfer` — Bt709, Unspecified, Bt470M, Bt470Bg, Smpte170M,
  Smpte240M, Linear, Log100, Log316, Iec6196624, Bt1361Ecg, Iec6196621,
  Bt2020_10Bit, Bt2020_12Bit, SmpteSt2084Pq, SmpteSt428, AribStdB67Hlg
- `ColorRange` — Unspecified, Limited, Full
- `ChromaLocation` — Unspecified, Left, Center, TopLeft, Top, BottomLeft,
  Bottom

All `#[non_exhaustive]`, all `derive(IsVariant)`, all `Copy + Eq + Hash + Debug`.

### 6.5 Decoder traits — push / pull split

Cinema RAW SDKs (R3D, BRAW, ARRIRAW, X-OCN, Canon RAW Light) are
random-access by frame index with rich clip-level metadata. FFmpeg /
WebCodecs / ProRes (via VTDecompressionSession) are push-style. Modeling
both shapes with one trait would distort one or the other; we use two.

```rust
pub trait VideoStreamDecoder {
    type Adapter: VideoAdapter;
    type Buffer:  AsRef<[u8]>;
    type Error;
    fn send_packet(&mut self, p: &VideoPacket<Self::Adapter, Self::Buffer>)
        -> Result<(), Self::Error>;
    fn receive_frame(&mut self, dst: &mut VideoFrame<Self::Adapter, Self::Buffer>)
        -> Result<(), Self::Error>;
    fn send_eof(&mut self) -> Result<(), Self::Error>;
    fn flush(&mut self)    -> Result<(), Self::Error>;
}

pub trait VideoFrameSource {
    type Adapter:  VideoAdapter;
    type Buffer:   AsRef<[u8]>;
    type ClipMeta;          // backend-specific, e.g. R3dClipMeta
    type Error;
    fn frame_count(&self) -> u64;
    fn frame_rate(&self) -> Timebase;
    fn duration(&self)   -> Timestamp;
    fn clip_meta(&self)  -> &Self::ClipMeta;
    fn decode_frame(
        &mut self,
        index: u64,
        dst: &mut VideoFrame<Self::Adapter, Self::Buffer>,
    ) -> Result<(), Self::Error>;
}

pub trait AudioStreamDecoder { /* mirror of VideoStreamDecoder */ }
pub trait AudioFrameSource   { /* mirror with sample-offset block reads */ }
pub trait SubtitleDecoder    { /* push-only — no pull-style subtitle decoders */ }
```

Backend mapping:

| Backend | `VideoStreamDecoder` | `VideoFrameSource` | `AudioStreamDecoder` | `AudioFrameSource` | `SubtitleDecoder` |
|---|---|---|---|---|---|
| FFmpeg | ✅ | — | ✅ | — | ✅ |
| WebCodecs | ✅ | — | ✅ | — | — |
| ProRes RAW (VideoToolbox) | ✅ | — | — | — | — |
| R3D | — | ✅ | — | ✅ | — |
| BRAW | — | ✅ | — | ✅ | — |
| ARRIRAW | — | ✅ | — | — | — |
| Sony X-OCN | — | ✅ | — | — | — |
| Canon RAW Light | — | ✅ | — | — | — |

No `Send` / `Sync` bound on the traits. No crate-level error type. No async
variant in v1; if needed later, it will be feature-gated.

## 7. Cargo features

```toml
[features]
default = ["std"]
alloc   = []
std     = ["alloc", "mediatime/std"]
serde       = ["dep:serde", "mediatime/serde", "bitflags/serde"]
arbitrary   = ["dep:arbitrary", "mediatime/arbitrary"]
quickcheck  = ["dep:quickcheck", "mediatime/quickcheck"]
```

`thiserror = { version = "2", default-features = false }` is unconditional —
since Rust 1.81 stabilized `core::error::Error`, `thiserror`'s default-off
mode emits `core::error::Error` impls that compile in `core`-only builds.
There is no need to gate it behind `std`.

| Item | `core` | `+alloc` | `+std` |
|---|---|---|---|
| All non-bitmap-subtitle types and traits | ✅ | ✅ | ✅ |
| `Timebase`/`Timestamp`/`TimeRange` (re-exports) | ✅ | ✅ | ✅ |
| `core::error::Error` impls (via thiserror) | ✅ | ✅ | ✅ |
| `SubtitlePayload::Bitmap` (alloc-gated) | ❌ | ✅ | ✅ |
| `std`-only conveniences (currently none required) | ❌ | ❌ | ✅ |

### no_std story

- `core`-only build works; bitmap subtitles are unavailable.
- `+alloc` suits `wasm32-unknown-unknown`.
- `+std` is the host default.

### Wasm story

- `wasm32-unknown-unknown --no-default-features --features alloc`: works.
- `wasm32-wasi*` with `std`: works.
- A future `mediadecode-webcodecs` adapter would target
  `wasm32-unknown-unknown` and call `VideoDecoder` / `AudioDecoder` via
  `web-sys`.

### No platform `cfg` in the core crate

All target-conditional code lives in adapter crates downstream.

## 8. Testing strategy

- Unit tests on every accessor / `with_*` / `set_*` mirroring mediatime's
  coverage style.
- `quickcheck` arbitrary impls behind the `quickcheck` feature for round-
  tripping via serde when enabled.
- Doctests on every public type's primary constructor.
- Integration tests in `tests/` exercising a tiny in-crate "loopback
  adapter" (a zero-sized type that impls `VideoAdapter` with `()` extras)
  — proves the trait machinery composes without an external decoder.
- No external SDK is required to run mediadecode's own test suite. Real
  backend tests live in the adapter crates.

## 9. Out of scope (v1)

- Concrete decoder implementations (separate adapter crates).
- Demuxer abstractions.
- Color conversion (lives in `colconv`).
- Async decoder traits.
- Frame pooling / arena allocators.
- A unified codec id / pixel format vocabulary across backends — the
  associated-type approach intentionally avoids forcing one.

## 10. Open questions deferred to implementation

- Final error-variant set per adapter (the FFmpeg adapter for example will
  likely model `AllBackendsFailed` similar to `hwdecode::Error`).
- Whether the FFmpeg adapter ships its `CodecId` newtype with `pub const`
  exhaustive constants for every well-known codec, or just the
  popular ones.
- The exact shape of each backend's `ClipMeta` struct — to be designed
  per adapter, against the per-SDK metadata vocabularies in the research
  reports.

## 11. Migration / coexistence notes

- Bumping mediadecode's MSRV from 1.73 to 1.95 is acceptable: every
  consumer in the workspace (colconv 1.95, hwdecode 1.95, scenesdetect
  1.85) already meets it.
- `colconv::ColorMatrix` and `colconv::raw::BayerPattern` will remain in
  colconv for the duration of colconv's active development. Mediadecode's
  copies are intentionally identical; both crates must not drift. A
  future flag-day will switch colconv's definitions to `pub use
  mediadecode::{ColorMatrix, BayerPattern};`.

<div align="center">
<h1>mediadecode</h1>
</div>
<div align="center">

Generic, `no_std`-friendly type-and-trait spine for media decoders.

[<img alt="github" src="https://img.shields.io/badge/github-findit--ai/mediadecode-8da0cb?style=for-the-badge&logo=Github" height="22">][Github-url]
<img alt="LoC" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fal8n%2F327b2a8aef9003246e45c6e47fe63937%2Fraw%2Fmediadecode" height="22">
[<img alt="Build" src="https://img.shields.io/github/actions/workflow/status/findit-studio/mediadecode/ci-core.yml?logo=Github-Actions&style=for-the-badge" height="22">][CI-url]
[<img alt="codecov" src="https://img.shields.io/codecov/c/gh/findit-studio/mediadecode?style=for-the-badge&logo=codecov" height="22">][codecov-url]

[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-mediadecode-66c2a5?style=for-the-badge&labelColor=555555&logo=data:image/svg+xml;base64,PHN2ZyByb2xlPSJpbWciIHhtbG5zPSJodHRwOi8vd3d3LnczLm9yZy8yMDAwL3N2ZyIgdmlld0JveD0iMCAwIDUxMiA1MTIiPjxwYXRoIGZpbGw9IiNmNWY1ZjUiIGQ9Ik00ODguNiAyNTAuMkwzOTIgMjE0VjEwNS41YzAtMTUtOS4zLTI4LjQtMjMuNC0zMy43bC0xMDAtMzcuNWMtOC4xLTMuMS0xNy4xLTMuMS0yNS4zIDBsLTEwMCAzNy41Yy0xNC4xIDUuMy0yMy40IDE4LjctMjMuNCAzMy43VjIxNGwtOTYuNiAzNi4yQzkuMyAyNTUuNSAwIDI2OC45IDAgMjgzLjlWMzk0YzAgMTMuNiA3LjcgMjYuMSAxOS45IDMyLjJsMTAwIDUwYzEwLjEgNS4xIDIyLjEgNS4xIDMyLjIgMGwxMDMuOS01MiAxMDMuOSA1MmMxMC4xIDUuMSAyMi4xIDUuMSAzMi4yIDBsMTAwLTUwYzEyLjItNi4xIDE5LjktMTguNiAxOS45LTMyLjJWMjgzLjljMC0xNS05LjMtMjguNC0yMy40LTMzLjd6TTM1OCAyMTQuOGwtODUgMzEuOXYtNjguMmw4NS0zN3Y3My4zek0xNTQgMTA0LjFsMTAyLTM4LjIgMTAyIDM4LjJ2LjZsLTEwMiA0MS40LTEwMi00MS40di0uNnptODQgMjkxLjFsLTg1IDQyLjV2LTc5LjFsODUtMzguOHY3NS40em0wLTExMmwtMTAyIDQxLjQtMTAyLTQxLjR2LS42bDEwMi0zOC4yIDEwMiAzOC4ydi42em0yNDAgMTEybC04NSA0Mi41di03OS4xbDg1LTM4Ljh2NzUuNHptMC0xMTJsLTEwMiA0MS40LTEwMi00MS40di0uNmwxMDItMzguMiAxMDIgMzguMnYuNnoiPjwvcGF0aD48L3N2Zz4K" height="20">][doc-url]
[<img alt="crates.io" src="https://img.shields.io/crates/v/mediadecode?style=for-the-badge&logo=data:image/svg+xml;base64,PD94bWwgdmVyc2lvbj0iMS4wIiBlbmNvZGluZz0iaXNvLTg4NTktMSI/Pg0KPCEtLSBHZW5lcmF0b3I6IEFkb2JlIElsbHVzdHJhdG9yIDE5LjAuMCwgU1ZHIEV4cG9ydCBQbHVnLUluIC4gU1ZHIFZlcnNpb246IDYuMDAgQnVpbGQgMCkgIC0tPg0KPHN2ZyB2ZXJzaW9uPSIxLjEiIGlkPSJMYXllcl8xIiB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHhtbG5zOnhsaW5rPSJodHRwOi8vd3d3LnczLm9yZy8xOTk5L3hsaW5rIiB4PSIwcHgiIHk9IjBweCINCgkgdmlld0JveD0iMCAwIDUxMiA1MTIiIHhtbDpzcGFjZT0icHJlc2VydmUiPg0KPGc+DQoJPGc+DQoJCTxwYXRoIGQ9Ik0yNTYsMEwzMS41MjgsMTEyLjIzNnYyODcuNTI4TDI1Niw1MTJsMjI0LjQ3Mi0xMTIuMjM2VjExMi4yMzZMMjU2LDB6IE0yMzQuMjc3LDQ1Mi41NjRMNzQuOTc0LDM3Mi45MTNWMTYwLjgxDQoJCQlsMTU5LjMwMyw3OS42NTFWNDUyLjU2NHogTTEwMS44MjYsMTI1LjY2MkwyNTYsNDguNTc2bDE1NC4xNzQsNzcuMDg3TDI1NiwyMDIuNzQ5TDEwMS44MjYsMTI1LjY2MnogTTQzNy4wMjYsMzcyLjkxMw0KCQkJbC0xNTkuMzAzLDc5LjY1MVYyNDAuNDYxbDE1OS4zMDMtNzkuNjUxVjM3Mi45MTN6IiBmaWxsPSIjRkZGIi8+DQoJPC9nPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPC9zdmc+DQo=" height="22">][crates-url]
[<img alt="crates.io" src="https://img.shields.io/crates/d/mediadecode?color=critical&logo=data:image/svg+xml;base64,PD94bWwgdmVyc2lvbj0iMS4wIiBzdGFuZGFsb25lPSJubyI/PjwhRE9DVFlQRSBzdmcgUFVCTElDICItLy9XM0MvL0RURCBTVkcgMS4xLy9FTiIgImh0dHA6Ly93d3cudzMub3JnL0dyYXBoaWNzL1NWRy8xLjEvRFREL3N2ZzExLmR0ZCI+PHN2ZyB0PSIxNjQ1MTE3MzMyOTU5IiBjbGFzcz0iaWNvbiIgdmlld0JveD0iMCAwIDEwMjQgMTAyNCIgdmVyc2lvbj0iMS4xIiB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHAtaWQ9IjM0MjEiIGRhdGEtc3BtLWFuY2hvci1pZD0iYTMxM3guNzc4MTA2OS4wLmkzIiB3aWR0aD0iNDgiIGhlaWdodD0iNDgiIHhtbG5zOnhsaW5rPSJodHRwOi8vd3d3LnczLm9yZy8xOTk5L3hsaW5rIj48ZGVmcz48c3R5bGUgdHlwZT0idGV4dC9jc3MiPjwvc3R5bGU+PC9kZWZzPjxwYXRoIGQ9Ik00NjkuMzEyIDU3MC4yNHYtMjU2aDg1LjM3NnYyNTZoMTI4TDUxMiA3NTYuMjg4IDM0MS4zMTIgNTcwLjI0aDEyOHpNMTAyNCA2NDAuMTI4QzEwMjQgNzgyLjkxMiA5MTkuODcyIDg5NiA3ODcuNjQ4IDg5NmgtNTEyQzEyMy45MDQgODk2IDAgNzYxLjYgMCA1OTcuNTA0IDAgNDUxLjk2OCA5NC42NTYgMzMxLjUyIDIyNi40MzIgMzAyLjk3NiAyODQuMTYgMTk1LjQ1NiAzOTEuODA4IDEyOCA1MTIgMTI4YzE1Mi4zMiAwIDI4Mi4xMTIgMTA4LjQxNiAzMjMuMzkyIDI2MS4xMkM5NDEuODg4IDQxMy40NCAxMDI0IDUxOS4wNCAxMDI0IDY0MC4xOTJ6IG0tMjU5LjItMjA1LjMxMmMtMjQuNDQ4LTEyOS4wMjQtMTI4Ljg5Ni0yMjIuNzItMjUyLjgtMjIyLjcyLTk3LjI4IDAtMTgzLjA0IDU3LjM0NC0yMjQuNjQgMTQ3LjQ1NmwtOS4yOCAyMC4yMjQtMjAuOTI4IDIuOTQ0Yy0xMDMuMzYgMTQuNC0xNzguMzY4IDEwNC4zMi0xNzguMzY4IDIxNC43MiAwIDExNy45NTIgODguODMyIDIxNC40IDE5Ni45MjggMjE0LjRoNTEyYzg4LjMyIDAgMTU3LjUwNC03NS4xMzYgMTU3LjUwNC0xNzEuNzEyIDAtODguMDY0LTY1LjkyLTE2NC45MjgtMTQ0Ljk2LTE3MS43NzZsLTI5LjUwNC0yLjU2LTUuODg4LTMwLjk3NnoiIGZpbGw9IiNmZmZmZmYiIHAtaWQ9IjM0MjIiIGRhdGEtc3BtLWFuY2hvci1pZD0iYTMxM3guNzc4MTA2OS4wLmkwIiBjbGFzcz0iIj48L3BhdGg+PC9zdmc+&style=for-the-badge" height="22">][crates-url]
<img alt="license" src="https://img.shields.io/badge/License-Apache%202.0/MIT-blue.svg?style=for-the-badge&fontColor=white&logoColor=f5c076&logo=data:image/svg+xml;base64,PCFET0NUWVBFIHN2ZyBQVUJMSUMgIi0vL1czQy8vRFREIFNWRyAxLjEvL0VOIiAiaHR0cDovL3d3dy53My5vcmcvR3JhcGhpY3MvU1ZHLzEuMS9EVEQvc3ZnMTEuZHRkIj4KDTwhLS0gVXBsb2FkZWQgdG86IFNWRyBSZXBvLCB3d3cuc3ZncmVwby5jb20sIFRyYW5zZm9ybWVkIGJ5OiBTVkcgUmVwbyBNaXhlciBUb29scyAtLT4KPHN2ZyBmaWxsPSIjZmZmZmZmIiBoZWlnaHQ9IjgwMHB4IiB3aWR0aD0iODAwcHgiIHZlcnNpb249IjEuMSIgaWQ9IkNhcGFfMSIgeG1sbnM9Imh0dHA6Ly93d3cudzMub3JnLzIwMDAvc3ZnIiB4bWxuczp4bGluaz0iaHR0cDovL3d3dy53My5vcmcvMTk5OS94bGluayIgdmlld0JveD0iMCAwIDI3Ni43MTUgMjc2LjcxNSIgeG1sOnNwYWNlPSJwcmVzZXJ2ZSIgc3Ryb2tlPSIjZmZmZmZmIj4KDTwhLS0gVXBsb2FkZWQgdG86IFNWRyBSZXBvLCB3d3cuc3ZncmVwby5jb20sIFRyYW5zZm9ybWVkIGJ5OiBTVkcgUmVwbyBNaXhlciBUb29scyAtLT4KPGcgaWQ9IlNWR1JlcG9fYmdDYXJyaWVyIiBzdHJva2Utd2lkdGg9IjAiLz4KDTxnIGlkPSJTVkdSZXBvX3RyYWNlckNhcnJpZXIiIHN0cm9rZS1saW5lY2FwPSJyb3VuZCIgc3Ryb2tlLWxpbmVqb2luPSJyb3VuZCIvPgoNPGcgaWQ9IlNWR1JlcG9faWNvbkNhcnJpZXIiPiA8Zz4gPHBhdGggZD0iTTEzOC4zNTcsMEM2Mi4wNjYsMCwwLDYyLjA2NiwwLDEzOC4zNTdzNjIuMDY2LDEzOC4zNTcsMTM4LjM1NywxMzguMzU3czEzOC4zNTctNjIuMDY2LDEzOC4zNTctMTM4LjM1NyBTMjE0LjY0OCwwLDEzOC4zNTcsMHogTTEzOC4zNTcsMjU4LjcxNUM3MS45OTIsMjU4LjcxNSwxOCwyMDQuNzIzLDE4LDEzOC4zNTdTNzEuOTkyLDE4LDEzOC4zNTcsMTggczEyMC4zNTcsNTMuOTkyLDEyMC4zNTcsMTIwLjM1N1MyMDQuNzIzLDI1OC43MTUsMTM4LjM1NywyNTguNzE1eiIvPiA8cGF0aCBkPSJNMTk0Ljc5OCwxNjAuOTAzYy00LjE4OC0yLjY3Ny05Ljc1My0xLjQ1NC0xMi40MzIsMi43MzJjLTguNjk0LDEzLjU5My0yMy41MDMsMjEuNzA4LTM5LjYxNCwyMS43MDggYy0yNS45MDgsMC00Ni45ODUtMjEuMDc4LTQ2Ljk4NS00Ni45ODZzMjEuMDc3LTQ2Ljk4Niw0Ni45ODUtNDYuOTg2YzE1LjYzMywwLDMwLjIsNy43NDcsMzguOTY4LDIwLjcyMyBjMi43ODIsNC4xMTcsOC4zNzUsNS4yMDEsMTIuNDk2LDIuNDE4YzQuMTE4LTIuNzgyLDUuMjAxLTguMzc3LDIuNDE4LTEyLjQ5NmMtMTIuMTE4LTE3LjkzNy0zMi4yNjItMjguNjQ1LTUzLjg4Mi0yOC42NDUgYy0zNS44MzMsMC02NC45ODUsMjkuMTUyLTY0Ljk4NSw2NC45ODZzMjkuMTUyLDY0Ljk4Niw2NC45ODUsNjQuOTg2YzIyLjI4MSwwLDQyLjc1OS0xMS4yMTgsNTQuNzc4LTMwLjAwOSBDMjAwLjIwOCwxNjkuMTQ3LDE5OC45ODUsMTYzLjU4MiwxOTQuNzk4LDE2MC45MDN6Ii8+IDwvZz4gPC9nPgoNPC9zdmc+" height="22">

</div>

The backend-agnostic core of the [`mediadecode`](https://github.com/findit-studio/mediadecode)
workspace. Defines the unified `Packet` / `Frame` types,
`VideoAdapter` / `AudioAdapter` / `SubtitleAdapter` traits, the
matching push-style `*StreamDecoder` traits that concrete decoder
backends implement, and the two tiers on either side of them: the
`Demuxer` session that produces packets and the `AudioResampler` seam
that reshapes decoded audio.

This crate ships **no decoder code** and **no FFmpeg dependency**.
It's `no_std`-clean (with optional `alloc` / `std` features) and zero
heavy deps — downstream crates (`colconv`, `scenesdetect`, …) program
against this vocabulary regardless of which backend produced the
bytes. Adapter implementations live in sibling crates such as
[`mediadecode-ffmpeg`](../mediadecode-ffmpeg).

## What's in the box

- **Pixel and sample formats** — `PixelFormat` (~270 variants
  covering every FFmpeg `n9.0` `AVPixelFormat` slug plus cinema-RAW
  additions; sourced from
  [`videoframe`](https://crates.io/crates/videoframe) and re-exported
  here so consumers keep their `mediadecode::PixelFormat` import).
  `Unknown(u32)` preserves the raw wire identifier for lossless
  round-trip via `from_u32` / `to_u32`. H.273-aligned color enums
  (`ColorMatrix`, `ColorPrimaries`, `ColorTransfer`, `ColorRange`,
  `ChromaLocation`) and `BayerPattern` for RAW are similarly
  re-exported from `videoframe`.
- **Generic packet / frame types** — `VideoPacket<A, B>`,
  `AudioPacket<A, B>`, `SubtitlePacket<A, B>`, `VideoFrame<A, B>`,
  `AudioFrame<A, B>`, `SubtitleFrame<A, B>` and `ImageFrame<A, B>`
  parameterized over an adapter's per-item **extras** type `A` and
  **buffer** type `B`. `Plane<B>` is the generic plane carrier.
  `ImageFrame` is the still-image household: no `pts`, no `duration`,
  because a still is not on the timeline — the same fact
  `AttachmentPacket` states on the packet side.
- **The D-seat amputation contract** — the one law a backend's buffer
  type `B` must obey: owned, `Send + Sync`, cheap to clone (a refcount
  bump), with no backend-internal lifetime crossing the seam. This
  crate names no carrier and pins no bound past `AsRef<[u8]>`; the
  contract is written out in full on the `adapter` module.
- **Adapter traits** — `VideoAdapter`, `AudioAdapter`,
  `SubtitleAdapter`, `ImageAdapter`. A backend implements these on a
  zero-sized type to fix `A` and `B` once for the whole pipeline.
- **Decoder traits** — `VideoStreamDecoder`, `AudioStreamDecoder`,
  `SubtitleDecoder`, `ImageDecoder`. The two `*Stream*` faces are
  push-style (`send_packet` / `receive_frame` / `send_eof` / `flush`),
  mirroring FFmpeg's decoder API while staying backend-agnostic; the
  other two are not, and their names say so — a subtitle cue and a
  still image each come out of exactly the packet that went in.
- **The demux tier** — `Demuxer`, the pull session over an opened
  container (`tracks()` / `next_packet()` / `seek()`), the five-arm
  `DemuxedPacket` envelope, the `TrackInfo` / `TrackParams` /
  `TrackKind` table, and the `DemuxAdapter` vocabulary bundle. Opening
  is each backend's own, so the trait covers only the opened session.
  A session keeps its track table for life: `tracks()` is a
  non-destructive read that hands out `TrackHandle`s — the backend's
  own row carrier, `Arc` / `Rc` / a plain borrow — so a consumer that
  needs a row past the pull loop clones a refcount rather than taking
  the table away from the session that classifies against it. A row
  carries the identity the container declares about a track, its
  `language` among it — as the file wrote it, unfolded, because the
  registries that reconcile an MKV's `ger` with an MP4's `deu` belong
  to whoever owns the language vocabulary.
- **The resample seam** — `AudioResampler`, the `AudioStreamDecoder`
  push pair one tier along, converting rate, sample format and channel
  layout between a source spec read off `TrackInfo` and a target spec
  that is always the caller's options.
- **Time primitives** — re-exported `Timebase` / `Timestamp` /
  `TimeRange` from [`mediatime`](https://crates.io/crates/mediatime),
  so consumers don't need a separate dep.

## API style

Mirrors the [`mediatime`](https://crates.io/crates/mediatime) idioms
the rest of the findit-studio workspace uses:

- All public fields are private; access is via `field()` getters,
  consuming `with_field(value)` builders, and in-place
  `set_field(value)` mutators that return `&mut Self`.
- `const fn` everywhere the field type allows.
- Panicking constructors come with `try_*` fallible counterparts
  (`empty` / `try_empty`, `clone` / `try_clone`, …).
- Errors via [`thiserror`](https://crates.io/crates/thiserror) over
  the stable `core::error::Error`, so failures still implement the
  `Error` trait under `--no-default-features`.

## Cargo features

| Feature      | Default | Effect                                                        |
| ------------ | :-----: | ------------------------------------------------------------- |
| `std`        |   yes   | Enable the standard library and `mediatime/default`.          |
| `alloc`      |    —    | Enable owning collections (`Vec`, `String`) without `std`.    |
| `serde`      |    —    | Every type below on the wire, plus `mediatime`.               |
| `arbitrary`  |    —    | `Arbitrary` impls for fuzzing, same coverage.                 |
| `quickcheck` |    —    | The same coverage again, for `quickcheck`.                    |

The three optional matrices cover the same type, and its wire shape
follows what it *is*:

| Type | Tier | serde wire shape |
| ---- | ---- | ---------------- |
| `packet::PacketFlags`            | any     | the raw bits, as a number                   |

A **bit set** travels as a number, because every bit pattern is
meaningful and bits this build has no constant for still have to survive
the round trip.

The audio channel-layout vocabulary this crate used to own —
`ChannelLayoutKind`, `AudioChannelOrderKind`, `AudioChannelSpec`,
`AudioChannelLayout` — now lives in
[`mediaframe::audio`](https://docs.rs/mediaframe/latest/mediaframe/audio/),
whose wire shapes are documented there. `AudioFrame`'s channel-layout
parameter is generic, so this crate names no channel type at all.

`no_std` builds: disable defaults and pick `alloc` if you need
`Vec`-backed payloads:

```toml
[dependencies]
mediadecode = { version = "0.8", default-features = false, features = ["alloc"] }
```

## Usage

This crate defines the surface; concrete decoding happens in adapter
crates. A backend-agnostic consumer programs against the traits, and
both faces answer states: `send_packet` answers `Sent` — *accepted* or
*must drain* — and `receive_frame` answers `Received` — *a frame*,
*needs input*, or *ended*. `Err` means a fault and nothing else:

```rust,no_run
use mediadecode::{
  Received, Sent,
  adapter::VideoAdapter,
  decoder::VideoStreamDecoder,
  frame::VideoFrame,
  packet::VideoPacket,
};

type Frame<D> = VideoFrame<
  <<D as VideoStreamDecoder>::Adapter as VideoAdapter>::PixelFormat,
  <<D as VideoStreamDecoder>::Adapter as VideoAdapter>::FrameExtra,
  <D as VideoStreamDecoder>::Buffer,
>;

/// Feeds one packet and delivers every frame it made ready.
/// Answers `true` once the stream is over.
fn decode_one<D: VideoStreamDecoder>(
  decoder: &mut D,
  packet: &VideoPacket<
    <D::Adapter as VideoAdapter>::PacketExtra,
    D::Buffer,
  >,
  dst: &mut Frame<D>,
  mut on_frame: impl FnMut(&Frame<D>),
) -> Result<bool, D::Error> {
  // Offer until the decoder takes it. `MustDrain` promises nothing was
  // consumed, so the *same* packet is re-offered after a drain.
  loop {
    // `?` gives up on a real failure — and only on a real failure.
    match decoder.send_packet(packet)? {
      Sent::Accepted => break,
      Sent::MustDrain => {
        while let Received::Frame = decoder.receive_frame(dst)? {
          on_frame(dst);
        }
      }
    }
  }
  loop {
    match decoder.receive_frame(dst)? {
      Received::Frame => on_frame(dst),
      Received::NeedsInput => return Ok(false),
      Received::Ended => return Ok(true),
    }
  }
}
```

The compiler is what makes this correct: a consumer that forgets
end-of-stream does not compile, a receive-side failure can no longer be
mistaken for a drained decoder, and back pressure no longer has to be
guessed at by offering every packet twice.

For an end-to-end example using the FFmpeg adapter, see
[`mediadecode-ffmpeg`](../mediadecode-ffmpeg).

## Build requirements

- Rust ≥ **1.95**, edition 2024.
- No system dependencies — `mediadecode` is FFmpeg-free and builds
  anywhere Rust does, including `no_std` targets with optional
  `alloc`.

## License

`mediadecode` is under the terms of both the MIT license and the
Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT) for details.

Copyright (c) 2026 FinDIT Studio authors.

[Github-url]: https://github.com/findit-studio/mediadecode
[CI-url]: https://github.com/findit-studio/mediadecode/actions/workflows/ci.yml
[codecov-url]: https://app.codecov.io/gh/findit-studio/mediadecode/
[doc-url]: https://docs.rs/mediadecode
[crates-url]: https://crates.io/crates/mediadecode

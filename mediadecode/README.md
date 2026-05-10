<div align="center">
<h1>mediadecode</h1>
</div>
<div align="center">

Generic, `no_std`-friendly type-and-trait spine for media decoders.

[<img alt="github" src="https://img.shields.io/badge/github-findit--ai/mediadecode-8da0cb?style=for-the-badge&logo=Github" height="22">][Github-url]
<img alt="LoC" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fal8n%2F327b2a8aef9003246e45c6e47fe63937%2Fraw%2Fmediadecode" height="22">
[<img alt="Build" src="https://img.shields.io/github/actions/workflow/status/findit-ai/mediadecode/ci.yml?logo=Github-Actions&style=for-the-badge" height="22">][CI-url]
[<img alt="codecov" src="https://img.shields.io/codecov/c/gh/findit-ai/mediadecode?style=for-the-badge&logo=codecov" height="22">][codecov-url]

[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-mediadecode-66c2a5?style=for-the-badge&labelColor=555555&logo=data:image/svg+xml;base64,PHN2ZyByb2xlPSJpbWciIHhtbG5zPSJodHRwOi8vd3d3LnczLm9yZy8yMDAwL3N2ZyIgdmlld0JveD0iMCAwIDUxMiA1MTIiPjxwYXRoIGZpbGw9IiNmNWY1ZjUiIGQ9Ik00ODguNiAyNTAuMkwzOTIgMjE0VjEwNS41YzAtMTUtOS4zLTI4LjQtMjMuNC0zMy43bC0xMDAtMzcuNWMtOC4xLTMuMS0xNy4xLTMuMS0yNS4zIDBsLTEwMCAzNy41Yy0xNC4xIDUuMy0yMy40IDE4LjctMjMuNCAzMy43VjIxNGwtOTYuNiAzNi4yQzkuMyAyNTUuNSAwIDI2OC45IDAgMjgzLjlWMzk0YzAgMTMuNiA3LjcgMjYuMSAxOS45IDMyLjJsMTAwIDUwYzEwLjEgNS4xIDIyLjEgNS4xIDMyLjIgMGwxMDMuOS01MiAxMDMuOSA1MmMxMC4xIDUuMSAyMi4xIDUuMSAzMi4yIDBsMTAwLTUwYzEyLjItNi4xIDE5LjktMTguNiAxOS45LTMyLjJWMjgzLjljMC0xNS05LjMtMjguNC0yMy40LTMzLjd6TTM1OCAyMTQuOGwtODUgMzEuOXYtNjguMmw4NS0zN3Y3My4zek0xNTQgMTA0LjFsMTAyLTM4LjIgMTAyIDM4LjJ2LjZsLTEwMiA0MS40LTEwMi00MS40di0uNnptODQgMjkxLjFsLTg1IDQyLjV2LTc5LjFsODUtMzguOHY3NS40em0wLTExMmwtMTAyIDQxLjQtMTAyLTQxLjR2LS42bDEwMi0zOC4yIDEwMiAzOC4ydi42em0yNDAgMTEybC04NSA0Mi41di03OS4xbDg1LTM4Ljh2NzUuNHptMC0xMTJsLTEwMiA0MS40LTEwMi00MS40di0uNmwxMDItMzguMiAxMDIgMzguMnYuNnoiPjwvcGF0aD48L3N2Zz4K" height="20">][doc-url]
[<img alt="crates.io" src="https://img.shields.io/crates/v/mediadecode?style=for-the-badge&logo=data:image/svg+xml;base64,PD94bWwgdmVyc2lvbj0iMS4wIiBlbmNvZGluZz0iaXNvLTg4NTktMSI/Pg0KPCEtLSBHZW5lcmF0b3I6IEFkb2JlIElsbHVzdHJhdG9yIDE5LjAuMCwgU1ZHIEV4cG9ydCBQbHVnLUluIC4gU1ZHIFZlcnNpb246IDYuMDAgQnVpbGQgMCkgIC0tPg0KPHN2ZyB2ZXJzaW9uPSIxLjEiIGlkPSJMYXllcl8xIiB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHhtbG5zOnhsaW5rPSJodHRwOi8vd3d3LnczLm9yZy8xOTk5L3hsaW5rIiB4PSIwcHgiIHk9IjBweCINCgkgdmlld0JveD0iMCAwIDUxMiA1MTIiIHhtbDpzcGFjZT0icHJlc2VydmUiPg0KPGc+DQoJPGc+DQoJCTxwYXRoIGQ9Ik0yNTYsMEwzMS41MjgsMTEyLjIzNnYyODcuNTI4TDI1Niw1MTJsMjI0LjQ3Mi0xMTIuMjM2VjExMi4yMzZMMjU2LDB6IE0yMzQuMjc3LDQ1Mi41NjRMNzQuOTc0LDM3Mi45MTNWMTYwLjgxDQoJCQlsMTU5LjMwMyw3OS42NTFWNDUyLjU2NHogTTEwMS44MjYsMTI1LjY2MkwyNTYsNDguNTc2bDE1NC4xNzQsNzcuMDg3TDI1NiwyMDIuNzQ5TDEwMS44MjYsMTI1LjY2MnogTTQzNy4wMjYsMzcyLjkxMw0KCQkJbC0xNTkuMzAzLDc5LjY1MVYyNDAuNDYxbDE1OS4zMDMtNzkuNjUxVjM3Mi45MTN6IiBmaWxsPSIjRkZGIi8+DQoJPC9nPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPGc+DQo8L2c+DQo8Zz4NCjwvZz4NCjxnPg0KPC9nPg0KPC9zdmc+DQo=" height="22">][crates-url]
[<img alt="crates.io" src="https://img.shields.io/crates/d/mediadecode?color=critical&logo=data:image/svg+xml;base64,PD94bWwgdmVyc2lvbj0iMS4wIiBzdGFuZGFsb25lPSJubyI/PjwhRE9DVFlQRSBzdmcgUFVCTElDICItLy9XM0MvL0RURCBTVkcgMS4xLy9FTiIgImh0dHA6Ly93d3cudzMub3JnL0dyYXBoaWNzL1NWRy8xLjEvRFREL3N2ZzExLmR0ZCI+PHN2ZyB0PSIxNjQ1MTE3MzMyOTU5IiBjbGFzcz0iaWNvbiIgdmlld0JveD0iMCAwIDEwMjQgMTAyNCIgdmVyc2lvbj0iMS4xIiB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHAtaWQ9IjM0MjEiIGRhdGEtc3BtLWFuY2hvci1pZD0iYTMxM3guNzc4MTA2OS4wLmkzIiB3aWR0aD0iNDgiIGhlaWdodD0iNDgiIHhtbG5zOnhsaW5rPSJodHRwOi8vd3d3LnczLm9yZy8xOTk5L3hsaW5rIj48ZGVmcz48c3R5bGUgdHlwZT0idGV4dC9jc3MiPjwvc3R5bGU+PC9kZWZzPjxwYXRoIGQ9Ik00NjkuMzEyIDU3MC4yNHYtMjU2aDg1LjM3NnYyNTZoMTI4TDUxMiA3NTYuMjg4IDM0MS4zMTIgNTcwLjI0aDEyOHpNMTAyNCA2NDAuMTI4QzEwMjQgNzgyLjkxMiA5MTkuODcyIDg5NiA3ODcuNjQ4IDg5NmgtNTEyQzEyMy45MDQgODk2IDAgNzYxLjYgMCA1OTcuNTA0IDAgNDUxLjk2OCA5NC42NTYgMzMxLjUyIDIyNi40MzIgMzAyLjk3NiAyODQuMTYgMTk1LjQ1NiAzOTEuODA4IDEyOCA1MTIgMTI4YzE1Mi4zMiAwIDI4Mi4xMTIgMTA4LjQxNiAzMjMuMzkyIDI2MS4xMkM5NDEuODg4IDQxMy40NCAxMDI0IDUxOS4wNCAxMDI0IDY0MC4xOTJ6IG0tMjU5LjItMjA1LjMxMmMtMjQuNDQ4LTEyOS4wMjQtMTI4Ljg5Ni0yMjIuNzItMjUyLjgtMjIyLjcyLTk3LjI4IDAtMTgzLjA0IDU3LjM0NC0yMjQuNjQgMTQ3LjQ1NmwtOS4yOCAyMC4yMjQtMjAuOTI4IDIuOTQ0Yy0xMDMuMzYgMTQuNC0xNzguMzY4IDEwNC4zMi0xNzguMzY4IDIxNC43MiAwIDExNy45NTIgODguODMyIDIxNC40IDE5Ni45MjggMjE0LjRoNTEyYzg4LjMyIDAgMTU3LjUwNC03NS4xMzYgMTU3LjUwNC0xNzEuNzEyIDAtODguMDY0LTY1LjkyLTE2NC45MjgtMTQ0Ljk2LTE3MS43NzZsLTI5LjUwNC0yLjU2LTUuODg4LTMwLjk3NnoiIGZpbGw9IiNmZmZmZmYiIHAtaWQ9IjM0MjIiIGRhdGEtc3BtLWFuY2hvci1pZD0iYTMxM3guNzc4MTA2OS4wLmkwIiBjbGFzcz0iIj48L3BhdGg+PC9zdmc+&style=for-the-badge" height="22">][crates-url]
<img alt="license" src="https://img.shields.io/badge/License-Apache%202.0/MIT-blue.svg?style=for-the-badge&fontColor=white&logoColor=f5c076&logo=data:image/svg+xml;base64,PCFET0NUWVBFIHN2ZyBQVUJMSUMgIi0vL1czQy8vRFREIFNWRyAxLjEvL0VOIiAiaHR0cDovL3d3dy53My5vcmcvR3JhcGhpY3MvU1ZHLzEuMS9EVEQvc3ZnMTEuZHRkIj4KDTwhLS0gVXBsb2FkZWQgdG86IFNWRyBSZXBvLCB3d3cuc3ZncmVwby5jb20sIFRyYW5zZm9ybWVkIGJ5OiBTVkcgUmVwbyBNaXhlciBUb29scyAtLT4KPHN2ZyBmaWxsPSIjZmZmZmZmIiBoZWlnaHQ9IjgwMHB4IiB3aWR0aD0iODAwcHgiIHZlcnNpb249IjEuMSIgaWQ9IkNhcGFfMSIgeG1sbnM9Imh0dHA6Ly93d3cudzMub3JnLzIwMDAvc3ZnIiB4bWxuczp4bGluaz0iaHR0cDovL3d3dy53My5vcmcvMTk5OS94bGluayIgdmlld0JveD0iMCAwIDI3Ni43MTUgMjc2LjcxNSIgeG1sOnNwYWNlPSJwcmVzZXJ2ZSIgc3Ryb2tlPSIjZmZmZmZmIj4KDTwhLS0gVXBsb2FkZWQgdG86IFNWRyBSZXBvLCB3d3cuc3ZncmVwby5jb20sIFRyYW5zZm9ybWVkIGJ5OiBTVkcgUmVwbyBNaXhlciBUb29scyAtLT4KPGcgaWQ9IlNWR1JlcG9fYmdDYXJyaWVyIiBzdHJva2Utd2lkdGg9IjAiLz4KDTxnIGlkPSJTVkdSZXBvX3RyYWNlckNhcnJpZXIiIHN0cm9rZS1saW5lY2FwPSJyb3VuZCIgc3Ryb2tlLWxpbmVqb2luPSJyb3VuZCIvPgoNPGcgaWQ9IlNWR1JlcG9faWNvbkNhcnJpZXIiPiA8Zz4gPHBhdGggZD0iTTEzOC4zNTcsMEM2Mi4wNjYsMCwwLDYyLjA2NiwwLDEzOC4zNTdzNjIuMDY2LDEzOC4zNTcsMTM4LjM1NywxMzguMzU3czEzOC4zNTctNjIuMDY2LDEzOC4zNTctMTM4LjM1NyBTMjE0LjY0OCwwLDEzOC4zNTcsMHogTTEzOC4zNTcsMjU4LjcxNUM3MS45OTIsMjU4LjcxNSwxOCwyMDQuNzIzLDE4LDEzOC4zNTdTNzEuOTkyLDE4LDEzOC4zNTcsMTggczEyMC4zNTcsNTMuOTkyLDEyMC4zNTcsMTIwLjM1N1MyMDQuNzIzLDI1OC43MTUsMTM4LjM1NywyNTguNzE1eiIvPiA8cGF0aCBkPSJNMTk0Ljc5OCwxNjAuOTAzYy00LjE4OC0yLjY3Ny05Ljc1My0xLjQ1NC0xMi40MzIsMi43MzJjLTguNjk0LDEzLjU5My0yMy41MDMsMjEuNzA4LTM5LjYxNCwyMS43MDggYy0yNS45MDgsMC00Ni45ODUtMjEuMDc4LTQ2Ljk4NS00Ni45ODZzMjEuMDc3LTQ2Ljk4Niw0Ni45ODUtNDYuOTg2YzE1LjYzMywwLDMwLjIsNy43NDcsMzguOTY4LDIwLjcyMyBjMi43ODIsNC4xMTcsOC4zNzUsNS4yMDEsMTIuNDk2LDIuNDE4YzQuMTE4LTIuNzgyLDUuMjAxLTguMzc3LDIuNDE4LTEyLjQ5NmMtMTIuMTE4LTE3LjkzNy0zMi4yNjItMjguNjQ1LTUzLjg4Mi0yOC42NDUgYy0zNS44MzMsMC02NC45ODUsMjkuMTUyLTY0Ljk4NSw2NC45ODZzMjkuMTUyLDY0Ljk4Niw2NC45ODUsNjQuOTg2YzIyLjI4MSwwLDQyLjc1OS0xMS4yMTgsNTQuNzc4LTMwLjAwOSBDMjAwLjIwOCwxNjkuMTQ3LDE5OC45ODUsMTYzLjU4MiwxOTQuNzk4LDE2MC45MDN6Ii8+IDwvZz4gPC9nPgoNPC9zdmc+" height="22">

</div>

The backend-agnostic core of the [`mediadecode`](https://github.com/findit-ai/mediadecode)
workspace. Defines the unified `Packet` / `Frame` types,
`VideoAdapter` / `AudioAdapter` / `SubtitleAdapter` traits, and the
matching push-style `*StreamDecoder` traits that concrete decoder
backends implement.

This crate ships **no decoder code** and **no FFmpeg dependency**.
It's `no_std`-clean (with optional `alloc` / `std` features) and zero
heavy deps — downstream crates (`colconv`, `scenesdetect`, …) program
against this vocabulary regardless of which backend produced the
bytes. Adapter implementations live in sibling crates such as
[`mediadecode-ffmpeg`](../mediadecode-ffmpeg).

## What's in the box

- **Pixel and sample formats** — `PixelFormat` (closed enum covering
  CPU and HW-tile formats: NV12, P010/P012/P016, P210/P212/P216,
  P410/P412/P416, YUV420P, RGB24, …) and the H.273-aligned color
  enums `ColorMatrix`, `ColorPrimaries`, `ColorTransfer`,
  `ColorRange`, `ChromaLocation`, plus `BayerPattern` for RAW.
- **Generic packet / frame types** — `VideoPacket<A, B>`,
  `AudioPacket<A, B>`, `SubtitlePacket<A, B>`, `VideoFrame<A, B>`,
  `AudioFrame<A, B>`, `SubtitleFrame<A, B>` parameterized over an
  adapter's per-item **extras** type `A` and **buffer** type `B`.
  `Plane<B>` is the generic plane carrier.
- **Adapter traits** — `VideoAdapter`, `AudioAdapter`,
  `SubtitleAdapter`. A backend implements these on a zero-sized
  type to fix `A` and `B` once for the whole pipeline.
- **Decoder traits** — `VideoStreamDecoder`, `AudioStreamDecoder`,
  `SubtitleStreamDecoder`. Push-style `send_packet` /
  `receive_frame` / `send_eof` / `flush` shape; mirrors FFmpeg's
  decoder API while staying backend-agnostic.
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
| `serde`      |    —    | `Serialize` / `Deserialize` impls (forwards to `mediatime`).  |
| `arbitrary`  |    —    | `Arbitrary` impls for fuzzing.                                |
| `quickcheck` |    —    | `Arbitrary` impls for `quickcheck`.                           |

`no_std` builds: disable defaults and pick `alloc` if you need
`Vec`-backed payloads:

```toml
[dependencies]
mediadecode = { version = "0.0.0", default-features = false, features = ["alloc"] }
```

## Usage

This crate defines the surface; concrete decoding happens in adapter
crates. A backend-agnostic consumer programs against the traits:

```rust,no_run
use mediadecode::{
  decoder::VideoStreamDecoder,
  frame::VideoFrame,
  packet::VideoPacket,
};

fn decode_one<D: VideoStreamDecoder>(
  decoder: &mut D,
  packet: &VideoPacket<
    <D::Adapter as mediadecode::adapter::VideoAdapter>::PacketExtra,
    D::Buffer,
  >,
  dst: &mut VideoFrame<
    <D::Adapter as mediadecode::adapter::VideoAdapter>::PixelFormat,
    <D::Adapter as mediadecode::adapter::VideoAdapter>::FrameExtra,
    D::Buffer,
  >,
) -> Result<(), D::Error> {
  decoder.send_packet(packet)?;
  decoder.receive_frame(dst)
}
```

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

[Github-url]: https://github.com/findit-ai/mediadecode
[CI-url]: https://github.com/findit-ai/mediadecode/actions/workflows/ci.yml
[codecov-url]: https://app.codecov.io/gh/findit-ai/mediadecode/
[doc-url]: https://docs.rs/mediadecode
[crates-url]: https://crates.io/crates/mediadecode

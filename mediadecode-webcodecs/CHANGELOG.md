# Changelog

All notable changes to the [`mediadecode-webcodecs`](https://crates.io/crates/mediadecode-webcodecs)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The backend-agnostic core it adapts has its own log at
[`mediadecode/CHANGELOG.md`](../mediadecode/CHANGELOG.md).

## [Unreleased]

- Crate scaffolded: workspace member, `wasm32`-gated `web-sys`
  dependency surface, design spec captured in
  `docs/superpowers/specs/2026-05-09-webcodecs-design.md`.
  Public API lands in a subsequent release.
- Tracks the `mediadecode` 0.2.0 / `videoframe` 0.2 cutover: the
  `PixelFormat::Unknown` boundary fallback in
  `webcodecs_pixel_format_to_mediadecode` preserves the raw
  WebCodecs identifier via `PixelFormat::Unknown(raw as u32)`
  instead of collapsing to a unit variant.
- `#[must_use]` added to every `with_*` consuming builder method.
- New `tests/native_stub.rs` — verifies the crate compiles to an
  empty stub on non-wasm32 targets and that no wasm-only names
  leak through. Closes
  [issue #4 — finding 4](https://github.com/Findit-AI/mediadecode/issues/4).

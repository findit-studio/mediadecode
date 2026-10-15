# Changelog

All notable changes to the [`mediaencode`](https://crates.io/crates/mediaencode)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The family releases in lockstep.

## [0.19.0] - 2026-10-15

### Added

- **The write faces of the mediaio family**, `no_std` with no allocator
  required. `VideoEncoder` and `AudioEncoder` take raw frames and give
  packets in the push rhythm the decoders keep (`Sent`, `Received`),
  with `send_eof` draining to `Ended`, `flush` answering
  `Flushed::Reset` or `Flushed::Refused`, `force_keyframe` and
  `frame_size`. `Muxer` takes the per-kind track packets a demuxer
  delivers, and `finish(self)` ends the session.
- `EncoderSettings` with `VideoParams` / `AudioParams`, `RateControl`,
  `Profile`, `Level` and `Gop`.
- The write errors: `EncodeError` / `EncodeRefusal` and `MuxError` /
  `MuxRefusal` — the refusals every backend names the same way, beside
  its own faults.

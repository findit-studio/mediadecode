# Changelog

All notable changes to the [`mediaio-ffmpeg`](https://crates.io/crates/mediaio-ffmpeg)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The family — `mediadecode`, `mediadecode-ffmpeg`, `mediadecode-webcodecs`,
`mediaencode`, `mediaencode-ffmpeg` and `mediaio` — releases in lockstep.

## [0.19.0] - 2026-10-15

### Added

- **The FFmpeg base of the mediaio family**: the crossings between
  FFmpeg's memory and the `mediaspine` vocabulary, moved out of
  `mediadecode-ffmpeg` (which re-exports them): the carriers and their
  operations, packet and frame conversion, codec tickets and
  identifiers, containers, limits, the AVFrame crossing, the allocator
  judge, and the FFmpeg resampler behind `resample`, with `Ffmpeg` and
  its adapter impls.
- **The crossing's contract.** Every function whose soundness rests on
  something its caller proves — a pointer's liveness, an extent, a claim
  about who else can write a buffer, a geometry a frame must keep — is
  an `unsafe fn` with a `# Safety` section; what a consumer holds is safe
  to use without any of that.
- `ParametersError`, what the codec-parameter copies answer, and
  `CrossingError`, what the AVFrame allocators, `Frame::empty` and the
  resampler's `Resample` arm carry.
- `try_clone_packet`: a deep copy of a packet that checks both
  `av_packet_ref` and `av_packet_make_writable`.
- A `test-support` feature (off by default): the fault-subprocess
  harness, the allocation cap and the callback-state hooks.

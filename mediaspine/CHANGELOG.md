# Changelog

All notable changes to the [`mediaspine`](https://crates.io/crates/mediaspine)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Its versions are its own: they do not move with the rest of the family's.

## [0.1.0] - 2026-10-15

### Added

- **The shared vocabulary of the mediaio family**, carved out of
  `mediadecode` 0.18 unchanged but for one name: `frame`, `packet`
  (`PacketFlags` with `DISPOSABLE`), `subtitle`, `rhythm`, the demuxer's
  types with `DemuxAdapter`, the adapter traits, the resampler face, and
  the `pixel_format`, `color` and `cfa` re-exports from `mediaframe`.
  `mediadecode` re-exports all of it under its old paths.
- **`Received::Produced`**, the answer that delivers output — a frame
  from a decoder or a resampler, a packet from an encoder. It was
  `Received::Frame` in `mediadecode`.
- **`hdr`**: `MasteringDisplay` and `ContentLightLevel`, lifted from
  `mediadecode-ffmpeg`'s extras so a write backend can take them too.
- **`track::TrackDescription`**: what a muxer needs to open one output
  track — the timebase, the codec and its parameters (`TrackParams`),
  optional extradata and the backend's side data.
- `no_std` with no allocator required; `alloc`, `std`, `serde`,
  `arbitrary` and `quickcheck` as in `mediadecode`.

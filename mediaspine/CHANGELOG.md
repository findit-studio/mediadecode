# Changelog

All notable changes to the [`mediaspine`](https://crates.io/crates/mediaspine)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Its versions are its own: they do not move with the rest of the family's.

## [0.1.0] - 2026-10-16

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
  optional extradata and the backend's side data, and the portable
  track metadata the read side states: the declared language, and for
  an attachment track its file (`AttachedFile` — the payload the
  demuxer delivered, its filename and MIME type). An attachment track
  carries exactly one file, by construction: `TrackDescription::attachment`
  takes one, and `TrackDescription::new` refuses an attachment's
  parameters by name (`AttachmentWithoutFile`).
- **`TrackDescription::from_track_info`**: the lossless conversion
  from a demuxed `TrackInfo` and the payload delivered for it, for a
  remux. It **borrows** the row, so a remux describes each track from
  the handle `Demuxer::tracks` hands out and the session keeps its rows;
  the portable fields are copied explicitly. What only the read side
  states (the library's duration, the backend's extras) stays on the
  row. A row that does not fit — an attachment without its payload, a
  payload for another kind, an attachment's identity claimed by another
  kind — is refused by name, the row's kind named and the payload and
  side data handed back (`TrackMismatch`, `MismatchReason`).
- **`TrackParams::duplicate`**: a field-by-field copy of a track's
  parameters, by name — the message-carrier law keeps `Clone` off the
  type, and a consumer that needs parameters of its own spells the copy
  out.
- **`DemuxAdapter::Text` is `Clone`**: a copy of `&'static str` or a
  refcount bump of a shared string, how the conversion takes a row's
  text from a row it borrows.
- `no_std` with no allocator required; `alloc`, `std`, `serde`,
  `arbitrary` and `quickcheck` as in `mediadecode`.

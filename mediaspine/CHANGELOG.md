# Changelog

All notable changes to the [`mediaspine`](https://crates.io/crates/mediaspine)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Its versions are its own: they do not move with the rest of the family's.

## [0.1.1] - unreleased

### Added

- **`nal`: what an H.264 or HEVC packet's NAL units say about its
  picture.** `NalStream`, built from a track's configuration record —
  FFmpeg's own test for an `avcC` or `hvcC` and its length size, else
  Annex B — reads each packet's NAL unit headers (`read`) and answers
  `Verdict::Disposable` only where the bitstream proves that nothing
  references the packet's picture and nothing beside it is needed later:
  - H.264: at least one slice, every slice of `nal_ref_idc` 0, none an
    IDR, and nothing beside them but SEI, access unit delimiters and
    filler data;
  - HEVC: every slice a sub-layer non-reference type of layer 0 at the
    highest temporal sub-layer any parameter set has declared, learned
    from every SPS and VPS and the record's `numTemporalLayers`, and
    nothing beside them but delimiters, filler data and SEI. Unlike
    FFmpeg's own `AVDISCARD_NONREF`, which skips sub-layer non-reference
    pictures at every sub-layer, it never marks one a higher sub-layer
    may reference.

  Anything else keeps the packet, with the reason (`KeptReason`): a
  reference or random-access picture, a parameter set or an end, an
  extension's NAL unit, a picture below the highest sub-layer or before
  any SPS, and a packet that does not parse. `NalCodec` names the two
  codecs read; every other codec is unclassified.

### Changed

- **`PacketFlags::DISPOSABLE` is the container's mark or the
  bitstream's**: nothing references this picture. Its contract is
  unchanged — absent means unknown — and a demuxer that reads the
  bitstream sets it on more H.264 and HEVC packets than the container
  marks.

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

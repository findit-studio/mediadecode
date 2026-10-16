# Changelog

All notable changes to the [`mediaencode-ffmpeg`](https://crates.io/crates/mediaencode-ffmpeg)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The family releases in lockstep.

## [Unreleased]

### Added

- **The muxer**: `FfmpegMuxer` (view lane) and `FfmpegOwnedMuxer` (owned
  lane) implement `Muxer` over libavformat for any container it names
  (`ContainerName`). `open` takes the sink, the container, the track
  table with each track's `CodecTicket`, the chapters and the packet
  limits, refuses by name what the container cannot take
  (`MuxOpenError`) and reports what it drops (`MuxReport`, `Uncarried`).
  Matroska keeps attachments with their filename and MIME type, MP4 keeps
  cover art; the report follows each track's road, naming the flags an
  attachment loses in Matroska and the filename, MIME type, language and
  flags a cover loses in MP4. Writes judge presentation
  order on the counts libavformat is handed, and refuse a strict
  container's repeated decode time, and a time the stream cannot count —
  one that lands on `i64::MIN`, libavformat's no-timestamp value, among
  them — by the muxer's own name (`FfmpegMuxRefusal`, through
  `MuxError::Backend`), handing the packet back; faults name their stage
  (`MuxFault`), a sink's panic included.
- **The packet-rule table.** Every rule libavformat applies on the
  packet path in Matroska, WebM and MP4, at FFmpeg 9.0.1, is refused by
  name before libavformat is handed the packet, reported at open, or
  passed through. Refused, the packet handed back untouched: a negative
  duration; MP4's duration, decode gap and composition offset past
  `i32::MAX` ticks, the gap judged in `i128`; a reordering stream's
  packet without a presentation time; a time before the earliest the
  container writes; an MP4 `mov_text` cue out of order; an unmarked key
  frame of a codec of key frames alone (`FfmpegMuxRefusal`); and a
  packet's bytes or side data the container would refuse or write
  otherwise — an empty packet it drops, AAC in ADTS headers, partial PCM,
  AV1 OBUs or WavPack blocks that do not parse, MP4 skip-samples below
  nothing, Matroska's new FLAC STREAMINFO of another length, a ProRes
  packet off its atom, a WebVTT cue that would not read back, an AC-3
  frame MP4 would drop or refuse (`FfmpegMuxRefusal::Payload`,
  `PayloadRule`). A track whose configuration the container needs and
  the track lacks is refused at open (`MuxOpenError::ConfigurationMissing`).
  Reported at open (`Uncarried::Rewritten`, `Rewrite`): length prefixes
  for Annex B, filtered OBUs, MP4's joined E-AC-3, `mov_text` cue ends and
  joined TTML paragraphs, sync from the bitstream, durations the container
  does not keep, Matroska's stripped WavPack headers. The automatic
  bitstream filters are off in these containers.
- The encoders arrive in the following releases.

## [0.19.0] - 2026-10-16

### Added

- **The encoder-by-name catalogue**, the gate every FFmpeg encoder will
  be opened through: `lookup` checks a closed catalogue and the license
  gate without asking FFmpeg anything, and `encoder_by_name` then asks
  the linked libavcodec, answering an `Admitted` encoder or an
  `EncoderRefusal` by name.
- **The `gpl` feature, off by default.** Without it the GPL encoders
  (`libx264`, `libx264rgb`, `libx265`) are refused by name before
  libavcodec is asked for them. Linking a GPL FFmpeg makes a binary GPL
  whatever this feature says; the README states it.
- The muxer and the encoders arrive in the following releases.

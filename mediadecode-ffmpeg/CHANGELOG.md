# Changelog

All notable changes to the [`mediadecode-ffmpeg`](https://crates.io/crates/mediadecode-ffmpeg)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The backend-agnostic core it adapts has its own log at
[`mediadecode/CHANGELOG.md`](../mediadecode/CHANGELOG.md).

## [Unreleased]

### Added

- **`FfmpegDemuxer` — `mediadecode`'s demux tier over `libavformat`.**
  Opens from a path (`open`) or from any `Read + Seek` byte source
  through a custom `AVIOContext` (`open_reader`). `Seek` is mandatory
  and not negotiable: MP4 routinely puts `moov` at the end, so a reader
  that cannot go backwards cannot be probed at all — and the face's seek
  law would be unimplementable.

  The track table is built once at open, in stream order, so
  `TrackIndex(i)` and `AVStream.index` are the same number by
  construction. Each row carries the stream's `Parameters`, which is
  what opens a decoder for it — a deep copy with no tie back to the
  format context, so a decoder outlives the demuxer that named it.

  **Three normalizations happen here, and they are all about
  attachments.** libavformat presents cover art as a *video* stream
  carrying `AV_DISPOSITION_ATTACHED_PIC`; this layer maps it to
  `TrackKind::Attachment`, so the `Video` arm is true motion video and
  nothing else. A font's bytes never appear in the packet stream at all
  — an `AVMEDIA_TYPE_ATTACHMENT` stream produces no packets and the
  payload lives in codec extradata — so the packet is synthesized at
  open. And cover art's real packet, which libavformat parks in
  `AVStream.attached_pic` *and* some demuxers also emit, is hoisted at
  open and its duplicate dropped. Both kinds are queued before a single
  `av_read_frame` has run, which is what makes "exactly one packet,
  before any timed packet" true rather than aspirational.

  `seek` converts the target to `AV_TIME_BASE` units and seeks over the
  window `[i64::MIN, target]` — FFmpeg's backward convention, landing on
  the nearest keyframe at or before the target. It clears only the EOF
  latch this layer set itself, leaving a genuine sticky I/O error
  intact, and does not touch the attachment bookkeeping: an attachment
  already handed out is never handed out again, and one not yet handed
  out is still owed.

  A track whose kind is `Unknown` has no delivery arm and its packets
  are not delivered; a corrupt packet is skipped and the read resumed,
  since `AVERROR_INVALIDDATA` is not latched into the `AVIOContext`.

- **Boundary helpers that carry a timebase.**
  `video_packet_from_ffmpeg_in`, `audio_packet_from_ffmpeg_in`,
  `subtitle_packet_from_ffmpeg_in` and `data_packet_from_ffmpeg_in` take
  the stream's timebase and stamp it onto every timestamp. An
  `AVPacket`'s integers are ticks in a timebase the packet does not
  carry, so the existing four-argument-less helpers stamp `1/1` and
  leave the caller to remember what the ticks meant; a demuxer holds the
  track table and has no reason to forget. The originals are unchanged
  and now delegate. `attachment_packet_from_ffmpeg` joins them for
  cover-art payloads, which have no timestamps to carry.

- **`DataPacketExtra`, `AttachmentPacketExtra`, `TrackExtra`** — the
  demux tier's `*Extra` carriers, and `impl DemuxAdapter for Ffmpeg`
  binding them. `AttachmentPacketExtra::synthesized` records whether a
  payload came from a real packet or was built out of codec extradata,
  which is the first thing to check when an attachment looks wrong.
  `TrackExtra::disposition` is the raw `AV_DISPOSITION_*` bit set rather
  than `ffmpeg_next`'s `Disposition`, whose `from_bits_truncate` would
  drop bits this build has no constant for.

## [0.6.0] - 2026-08-21

### Added

- **A layout that falls off the constant table now gets one more
  chance: FFmpeg's own name for it.** `channel_layout_from_ffmpeg`
  keeps its constant-arm table as the first and authoritative rung —
  a mapped constant is answered there, byte for byte, and renders
  nothing and parses nothing. Only on a miss does the adapter ask
  `av_channel_layout_describe` what FFmpeg calls the layout and read
  that word through `mediaframe::audio::ChannelLayout`'s `FromStr`,
  the total door the vocabulary already exposes.

  A named variant wins; anything the vocabulary does not name still
  lands on the absent sentinel with FFmpeg's rendering in `text()`.
  The rendering never rides `known_kind`'s `Other` escape, so
  "unrecognised" stays one value rather than one per spelling.

  What the rung reaches that the table cannot: `binaural`, `5.1.2`
  and `9.1.6` — all three named by `ChannelLayout`, none of them
  minted as an `ffmpeg_next` constant — plus FFmpeg 9's own `5.1.4`,
  whose side-surround mask `ffmpeg_sys_next`'s bundled
  `channel_layout_fixed.h` misses, its
  `AV_CH_LAYOUT_5POINT1POINT4_BACK` still carrying FFmpeg 8's
  back-surround formula. And, from here on, any layout a later
  FFmpeg adds that the vocabulary already names, with no edit in
  this crate: FFmpeg speaks the name, the vocabulary reads the word,
  one source.

  On the `ChannelLayoutDescription` path the layout is described
  once and that single rendering feeds both `known_kind` and
  `text`, so the two can never answer from different FFmpeg calls.

### Changed (BREAKING)

- **`channel_layout`'s conversions produce `mediaframe::audio` types,
  and are named after them.** `mediadecode::channel` is gone (see
  [`mediadecode`](../mediadecode/CHANGELOG.md#unreleased)); the
  vocabulary lives upstream now, and the `Kind` ornament is buried with
  the enum it named.

  | was | is | returns |
  | --- | -- | ------- |
  | `channel_layout_kind_from_ffmpeg` | `channel_layout_from_ffmpeg` | `ChannelLayout` |
  | `audio_channel_order_kind_from_ffmpeg` | `channel_order_from_ffmpeg` | `ChannelOrder` |
  | `audio_channel_order_kind_from_raw` | `channel_order_from_raw` | `ChannelOrder` |
  | `audio_channel_layout_from_ffmpeg` | `channel_layout_description_from_ffmpeg` | `ChannelLayoutDescription` |
  | `audio_channel_layout_from_raw_ptr` | `channel_layout_description_from_raw_ptr` | `ChannelLayoutDescription` |

  The bodies are unchanged: same raw-pointer discipline (`order` is
  read as `i32` before any `AVChannelOrder` value is formed), same
  union guards, same `av_channel_layout_describe` buffer growth, same
  UTF-8-lossy label decoding. Only the target vocabulary moves.
  FFmpeg's own `ChannelLayout` is imported as `AvChannelLayout` inside
  the module so the plain name belongs to what the functions produce.

- **`AudioAdapter::ChannelLayout` and the `AudioFrame` alias bind
  `mediaframe::audio::ChannelLayoutDescription`.** A caller that stored
  a decoded layout in a type of its own re-spells that type; a caller
  that only passed frames along needs nothing. The layout's *name* now
  arrives as `ChannelLayout` rather than `ChannelLayoutKind`, the
  absent case is `ChannelLayout::default()` (the `Other("")` sentinel)
  rather than `ChannelLayoutKind::Unknown`, and FFmpeg's rendering is
  read back with `text()` where it was `description()`.

- **Three layout arms answer to different idents, and one is gone.**
  Upstream's idents follow FFmpeg's constants except where a constant
  names an arrangement no reader would recognise, so `SURROUND` is
  `Ch3_0`, `AV_CH_LAYOUT_2_1` is `Ch3_0Back` and `AV_CH_LAYOUT_2_2` is
  `QuadSide`. The `_7POINT1_TOP_BACK` arm is deleted outright:
  `ffmpeg-next` defines that constant as an alias of
  `AV_CH_LAYOUT_5POINT1POINT2_BACK`, which this table already matches
  twelve arms earlier, so the arm could never be reached and the
  variant it produced does not exist upstream.

  Layouts the table still cannot name — `BINAURAL`, `_5POINT1POINT2`
  (the side one) and `_9POINT1POINT6`, all three named by
  `mediaframe::audio::ChannelLayout` — have no `ffmpeg_next`
  `ChannelLayout` constant to match against. The describe rung above
  reaches them all the same; without it they would keep arriving as
  the absent sentinel with FFmpeg's spelling preserved in `text()`,
  exactly as before.

- **`mediaframe` is a direct dependency**, pinned with `alloc`: the
  audio household is compiled only at the alloc-or-std tier.

## [0.5.0]

Tracks `mediadecode` 0.5.0, which crosses `mediaframe` 0.4 → 0.5 — a
breaking minor. `mediaframe` is a public dependency of the core crate
and its `PixelFormat` and `color` types appear in this adapter's own
signatures (`convert`, `pixdesc`, `frame`), so this adapter's
re-exported surface moves with it. See
[`mediadecode` 0.5.0](../mediadecode/CHANGELOG.md#050).

Nothing about the FFmpeg boundary's behaviour changes, and no source
line moved. The version bump is the public-dependency crossing, not a
change of conduct: `mediaframe` 0.5 adds no pixel-format or colour
variant, so this crate's exhaustive maps over `PixelFormat` — which
would have been `E0004` had one appeared — are unchanged, and the
FFmpeg constant tables still round-trip.

Two upstream conveniences arrive for free: the re-exported vocabularies
carry `ROSTER`, and `BayerPattern` is closed, so a downstream matching
it can drop its wildcard arm.

## [0.4.0] - 2026-08-19

### Changed (BREAKING)

- **`convert::ConvertError::UnsupportedPixelFormat` names the format
  again.** It was `UnsupportedPixelFormat(PixelFormat)`; it is now a
  struct variant carrying `format`, `raw` and `name`. The `format`
  field is the old payload unchanged — still `PixelFormat::None` for
  the fall-through, because this restores the *diagnostic*, not the
  `Unknown(u32)` variant mediaframe 0.3 struck. What is restored is
  everything the message lost with it:
  - `raw: i32` — the `AVFrame.format` integer exactly as FFmpeg wrote
    it, present unconditionally.
  - `name: Option<SmolStr>` — FFmpeg's own name for that integer, from
    `av_get_pix_fmt_name`, or `None` when libavutil has no descriptor
    for it.

  The rendered message goes from `unsupported pixel format None` back
  to `unsupported pixel format None (AVPixelFormat <n> =
  "videotoolbox_vld")`, and to `… (AVPixelFormat 99999, unnamed by
  libavutil)` where there is no name. This supersedes the note under
  0.4.0 below, which recorded the message losing the raw integer.

  Callers matching `ConvertError::UnsupportedPixelFormat(pf)` move to
  `ConvertError::UnsupportedPixelFormat { format, .. }`.

  The lookup does not weaken the crate's FFI stance. `av_get_pix_fmt_name`
  is redeclared with a plain `c_int` parameter rather than the bindgen
  enum, so an integer outside our build's discriminant set is never
  turned into an `AVPixelFormat` — which is the whole reason the
  fall-through exists in the first place. Two tests pin that libavutil
  answers such integers with null rather than misbehaving.

Tracks `mediadecode` 0.4.0, which crosses `mediatime` 0.1 → 0.3 and
`mediaframe` 0.1 → 0.3 — two breaking minors each. Both are public
dependencies of the core crate, so this adapter's re-exported
signatures move with them. See
[`mediadecode` 0.4.0](../mediadecode/CHANGELOG.md#040). This release
also crosses `ffmpeg-next` 8.1 → 9. Nothing about the FFmpeg
boundary's *behaviour* changes: the same raw integers are accepted,
the same formats are deliverable, and the same frames are rejected.

### Changed (BREAKING)

- **`ffmpeg-next` 8.1 → 9**, tracking FFmpeg 9. `ffmpeg_next::Packet`,
  `Frame`, `Error`, `decoder::Audio` and `decoder::Subtitle` appear in
  this crate's public signatures, so `ffmpeg-next` is a public
  dependency and downstream crates must move to the same major.
  No adapter source changed: the bump is spelling-clean, and the
  boundary's behaviour is unaffected. What it buys is the
  `ffmpeg_9_0` code path — `ffmpeg-sys-next` 8.1's version table
  topped out at `ffmpeg_8_1` and gated 9.0 off even when linked
  against a 9.x system library. Two surfaces moved in FFmpeg 9 but do
  not reach this crate: capability queries (`Audio`/`Video`'s
  `rates` / `formats` / `channel_layouts`) now read
  `avcodec_get_supported_config` instead of the codec struct's
  fields, and none of them are called here; and the codec-id
  vocabulary dropped `V308` / `V408` / `V410` while adding
  `WEBP_ANIM` / `APPLE_APAC`, which `CodecId` absorbs as a
  `#[repr(transparent)]` `i32` with a fall-through `Debug` arm. The
  pixel formats this crate already mapped to `None` for want of an
  `AV_PIX_FMT_*` constant (`V210`, `V410Le`, `Yuva420p12Le`,
  `Yuva444p14Le`) are still absent in 9, so their fall-through
  stands.
- **The pixel-format fall-through is now `PixelFormat::None`.**
  `boundary::from_av_pixel_format` returned
  `PixelFormat::Unknown(raw as u32)` for a raw `AVFrame.format`
  integer with no mapping; mediaframe 0.3 struck that variant, so the
  raw integer no longer rides along in the returned value.
  `PixelFormat::None` is a *named* member of the vocabulary (FFmpeg's
  own `AV_PIX_FMT_NONE`, and the `Default`), which is also now the
  exact answer for `AV_PIX_FMT_NONE` itself. Callers matching
  `PixelFormat::Unknown(_)` switch to `PixelFormat::None`; callers
  that read the payload have the raw integer at the call site, where
  `is_hardware_pix_fmt` already reads it. Rejection is unchanged —
  `pixdesc::to_av_pixel_format`, `pixdesc::is_deliverable`,
  `frame::is_supported_cpu_pix_fmt` and both geometry tables refuse
  the fall-through exactly as before. `boundary::empty_video_frame` /
  `try_empty_video_frame` fill the same placeholder.
- **`Timebase` construction is signed.** `mediatime` 0.2 made
  `Timebase`'s numerator and denominator `i32` / `NonZeroI32`, so the
  README example, `examples/decode_via_trait.rs`, and the three
  integration tests drop their `as u32` casts on
  `ffmpeg::Rational::numerator()` /
  `.denominator()` — which are C `int`s to begin with. A stream
  declaring a negative numerator now panics in `Timebase::new`
  instead of wrapping to a huge `u32`.

### Changed

- **`mediadecode` dep**: bumped to `0.4`.
- Crate-internal pixel-format predicates and tables now borrow rather
  than consume, since `mediaframe::PixelFormat` is no longer `Copy`:
  `pixdesc::{is_deliverable, plane_geometry, to_av_pixel_format}`,
  `frame::{is_supported_cpu_pix_fmt, plane_row_bytes_for,
  plane_height_for}` and `convert::{is_yuvj, map_range_for}` take
  `&PixelFormat`. All are `pub(crate)` or private — no public
  signature moves.
- `convert::ConvertError::UnsupportedPixelFormat` still carries the
  format by value; the two construction sites move it in after the
  borrowing checks, so no clone was introduced. Its rendered message
  changes for the fall-through case, from
  `unsupported pixel format Unknown(119)` to
  `unsupported pixel format None` — the raw integer is no longer in
  that log line.

[0.4.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.4.0

## [0.3.3] - 2026-06-25

Every byte-addressable CPU pixel format FFmpeg can produce now decodes
and delivers ([#15](https://github.com/findit-studio/mediadecode/pull/15)).
The hand-maintained per-format geometry table is gone from the decode
path; libavutil's own descriptor is the authority.

### Added

- **`pixdesc` module.** Per-plane geometry — visible row bytes and row
  count — is derived from `av_image_fill_linesizes` and
  `av_image_fill_plane_sizes` for the exact `(format, width, height)`,
  so it is correct by construction for any format FFmpeg can describe
  rather than for the formats someone remembered to tabulate. The
  safety stance is unchanged: an `AVPixelFormat` is never constructed
  from a runtime integer, only mapped from a recognised `PixelFormat`
  to a compile-time `AV_PIX_FMT_*` constant.

### Fixed

- **The YUVJ family decodes.** `yuvj420p` / `yuvj422p` / `yuvj440p` /
  `yuvj444p` / `yuvj411p` previously fell through to `Unknown` and were
  rejected at convert; they now map, deliver, and carry their JPEG
  range on `ColorInfo::range`. This is the real-world fix for MJPEG and
  JPEG-range footage.
- **`boundary::from_av_pixel_format` covers 251 formats**, up from 63.

### Changed

- **Deliberate exclusions**, by descriptor flag rather than by omission:
  Bayer mosaics (a demosaic question, not a geometry one), GPU surface
  formats (the hardware path transfers to a CPU format first), paletted,
  monochrome and sub-byte-packed RGB. All are rejected up front by
  `pixdesc::is_deliverable` with a layout the crate can state, instead
  of being read as `linesize × height` bytes of guesswork.
- Four formats have no constant in the linked `ffmpeg-sys-next` 8.1 and
  so still fall through: `V210`, `V410LE`, `YUVA420P12LE`,
  `YUVA444P14LE`.

> Downstream note carried from the PR: this crate now delivers formats
> the colconv resample layer did not yet handle at the time, which
> decode and then fail at resample. That is not a regression — they
> failed earlier, at decode, before this release — and closing the gap
> is colconv-side work.

[0.3.3]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.3.3

## [0.3.2] - 2026-06-24

Hardware decode that collapses **after** the probe has committed now
falls back to software instead of failing the stream
([#13](https://github.com/findit-studio/mediadecode/pull/13)).

### Added

- **`FallbackOrigin`** (`Probe` / `PostCommit`) on the
  `AllBackendsFailed` payload, with `AllBackendsFailed::origin()` and
  `AllBackendsFailed::new_post_commit()`. The wrapper routes its
  software-fallback replay on this explicit signal rather than inferring
  the origin from whether `unconsumed_packets` is empty — both origins
  can be empty (a probe-era failure on the very first packet has no
  history to surface either), so emptiness cannot tell them apart.
  Conflating the two made a probe-era first-packet cap trip look
  post-commit, and the packet could be dropped in silence.

### Fixed

- **Runtime HW failure no longer ends the stream.** When the committed
  backend fails after the probe has collapsed, the decoder opens a
  software decoder cold, forwards the failing call's packet (or EOF),
  and resyncs at the next keyframe — a bounded, logged gap rather than
  a dead stream. Probe-era failures keep the previous behaviour: the
  buffered history is replayed and the current packet routed on.

[0.3.2]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.3.2

## [0.3.1] - 2026-06-14

Additive release ([#10](https://github.com/findit-studio/mediadecode/pull/10)).

### Added

- **`Clone` on the decode-path error types** — `Error`,
  `AllBackendsFailed`, `FallbackFailed`, `AudioDecodeError` and
  `ConvertError` — so a consumer can forward one error event to several
  per-stream subscribers. Every payload was already cheaply clonable:
  `ffmpeg_next::Error` is `Copy`, `ffmpeg_next::Packet` is `Clone`, and
  `Backend` is `Copy`. `mediadecode::AudioFrame` gains `Clone` in the
  same release — see
  [`mediadecode` 0.3.1](../mediadecode/CHANGELOG.md#031---2026-06-14).

[0.3.1]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.3.1

## [0.3.0] - 2026-06-07

Tracks `mediadecode` 0.3.0, which flips the shared vocabulary crate from
`videoframe` 0.2 to `mediaframe` 0.1
([#7](https://github.com/findit-studio/mediadecode/pull/7),
[#8](https://github.com/findit-studio/mediadecode/pull/8)). See
[`mediadecode` 0.3.0](../mediadecode/CHANGELOG.md#030---2026-06-07) for
what moved in the vocabulary itself.

### Changed (BREAKING)

- **`mediadecode` dep**: bumped to `0.3`. The re-exported vocabulary
  types are `mediaframe`'s now, so this adapter's type aliases and
  signatures carry the new identity.
- **Two colour-transfer mappings are renamed**, tracking upstream:
  `ColorTransfer::Bt470M` → `Gamma22` and `Bt470Bg` → `Gamma28`. The
  FFmpeg wire mapping is untouched — the same `AVCOL_TRC_GAMMA22` /
  `AVCOL_TRC_GAMMA28` land on the same values under their new spelling.

### Changed

- Version bumped to 0.3.0 with the rest of the workspace.

[0.3.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.3.0

## [0.2.0] - 2026-05-15

Tracks `mediadecode` 0.2.0. The pixel-vocabulary types
(`PixelFormat`, color enums, frame primitives) now live in the
`videoframe` crate and are re-exported through `mediadecode`; this
release adapts the FFmpeg boundary to the new `PixelFormat::Unknown(u32)`
shape and updates the type aliases the crate re-exports.

### Changed (BREAKING)

- **`PixelFormat::Unknown` shape**: re-exported `PixelFormat` is now
  `Unknown(u32)` (tuple variant) instead of the prior unit variant
  — see [`mediadecode` 0.2.0](../mediadecode/CHANGELOG.md#020---2026-05-15).
- **FFmpeg boundary fallback** now preserves the raw `AVPixelFormat`
  identifier through `PixelFormat::Unknown(raw as u32)` instead of
  collapsing to a bare `Unknown`. Round-trips losslessly via
  `PixelFormat::{from_u32, to_u32}`.
- **Type aliases reshape**: `VideoFrame`, `AudioFrame`,
  `SubtitleFrame`, `VideoPacket`, `AudioPacket`, `SubtitlePacket`
  inherit the upstream `PixelFormat` shape change. Downstream
  callers matching on `Unknown` in destination frames need to
  switch to `Unknown(_)`.
- **`Error` enum variants** are now newtype-tuple form wrapping
  payload structs (matches the convention in
  [`videoframe`](https://crates.io/crates/videoframe)). Affected
  variants: `HwDeviceInitFailed`, `AllBackendsFailed`,
  `FallbackFailed`. Pure tuple variants (`Ffmpeg`, `NoCodec`,
  `BackendUnsupportedByCodec`) unchanged. Callers destructuring
  `Err(Error::AllBackendsFailed { attempts, .. })` must switch to
  `Err(Error::AllBackendsFailed(p))` and call `p.attempts()` /
  `p.unconsumed_packets()`. Owning-move paths for the rescued
  packets are preserved via `p.into_unconsumed_packets()` /
  `p.into_parts()`, so non-seekable callers can still relinquish
  the `Vec<Packet>` without cloning. The hand-written `Debug` that
  printed `[N packets]` (because `ffmpeg_next::Packet` has no
  `Debug`) now lives on the payload structs.
  All three new variants also carry `#[from]`, joining `Ffmpeg`
  which already had it — so `impl From<HwDeviceInitFailed> for Error`,
  `impl From<AllBackendsFailed> for Error`, and
  `impl From<FallbackFailed> for Error` are auto-generated, and
  helpers returning `Result<_, HwDeviceInitFailed>` etc. can be
  `?`-propagated into `Result<_, Error>` directly.

### Changed

- **`mediadecode` dep**: bumped to `0.2`.
- Boundary mapping in `pixel_format_from_ffmpeg` and the
  side-data conversion paths updated to the new
  `PixelFormat::Unknown(u32)` shape (17 fallback / assertion /
  default-frame sites across `mediadecode-ffmpeg` and
  `mediadecode-webcodecs`).

### Added

- **`Debug` impl for `Frame`** — manual `core::fmt::Debug` impl
  showing dimensions / format so the only public type previously
  without `Debug` is now printable.
  Closes [issue #4 — finding 2](https://github.com/findit-studio/mediadecode/issues/4).
- **`#[must_use]`** on every consuming `with_*` builder method
  across the crate's public surface.
  Closes [issue #4 — finding 3](https://github.com/findit-studio/mediadecode/issues/4).

[0.1.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.1.0
[0.2.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-ffmpeg-v0.2.0

## [0.1.0] - 2026-05-09

Initial public release.

### Added

- **Adapter type.** `Ffmpeg` zero-sized type implementing
  `mediadecode::adapter::VideoAdapter`, `AudioAdapter`, and
  `SubtitleAdapter`.
- **Buffer.** `FfmpegBuffer` — zero-copy refcounted view over an
  `AVBufferRef`, with `empty` / `from_packet` / `from_plane`
  constructors and panic-free `try_*` counterparts.
- **Video decoder.** `FfmpegVideoStreamDecoder` mirrors
  `ffmpeg::decoder::Video`'s `send_packet` / `receive_frame` shape and
  auto-probes the host's HW backends — VideoToolbox on Apple,
  VAAPI / CUDA on Linux, D3D11VA / CUDA on Windows — falling through
  to a software decoder when none open. `open_with(_, _, Backend::…)`
  pins a specific backend (no probe).
- **Audio decoder.** `FfmpegAudioStreamDecoder` over
  `ffmpeg::decoder::Audio`, producing zero-copy `AudioFrame`s.
- **Subtitle decoder.** `FfmpegSubtitleStreamDecoder` over the legacy
  synchronous `ffmpeg::decoder::Subtitle::decode` API, bridged into the
  trait's `send_packet` / `receive_frame` shape.
- **Type aliases.** `VideoPacket`, `AudioPacket`, `SubtitlePacket`,
  `VideoFrame`, `AudioFrame`, `SubtitleFrame` — pre-parameterized with
  this crate's adapter, buffer, and extras types.
- **Boundary helpers.** `video_packet_from_ffmpeg`,
  `audio_packet_from_ffmpeg`, `subtitle_packet_from_ffmpeg` — convert a
  borrowed `ffmpeg::Packet` into the matching `mediadecode` packet
  without copying the compressed payload. Empty-frame builders
  `empty_video_frame`, `empty_audio_frame`, `empty_subtitle_frame`
  produce well-formed destinations for `receive_frame`.
- **Recovery.** `VideoDecodeError::AllBackendsFailed { unconsumed_packets, .. }`
  carries any packets the decoder had already accepted from the
  demuxer when every backend is exhausted, so non-seekable callers
  (live streams, pipes, network sources) can replay them through their
  own software decoder without re-demuxing.

### Safety

The FFmpeg FFI surface is hardened against malformed or
version-skewed decoder output:

- All bindgen enum reads go through `addr_of!` + `read_unaligned` to
  avoid creating invalid Rust enum values from raw memory.
- `AVFrameSideDataType` values are mapped through an explicit
  whitelist of known `AV_FRAME_DATA_*` constants — never `transmute`d.
- `CStr::from_ptr` calls are replaced with a bounded
  `bounded_cstr_bytes` helper that searches at most
  `SUBTITLE_MAX_TEXT_BYTES_PER_RECT + 1` bytes for a NUL terminator.
- Signed counts (`AVFrame.nb_side_data`, `AVSubtitle.num_rects`, …)
  are clamped to non-negative values before any `as usize` cast,
  preventing OOB walks under corrupt input.
- Side-data and subtitle conversions enforce caps on entries and total
  bytes (`SIDE_DATA_MAX_ENTRIES`, `SIDE_DATA_MAX_TOTAL_BYTES`,
  `HW_COPY_SIDE_DATA_MAX_*`, `SUBTITLE_MAX_*`).
- `send_packet` consumes the demuxer packet only after the probe
  rescue records it, so a non-seekable caller can rebuild the input
  stream from `unconsumed_packets` on `AllBackendsFailed`.
- `cpu_frame_bytes` sizes against the underlying `AVBufferRef.size`
  rather than `linesize × plane_height_for(AVFrame.height)`, so
  cropped or heavily aligned streams report correct byte counts.


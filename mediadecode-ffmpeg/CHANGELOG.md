# Changelog

All notable changes to the [`mediadecode-ffmpeg`](https://crates.io/crates/mediadecode-ffmpeg)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The backend-agnostic core it adapts has its own log at
[`mediadecode/CHANGELOG.md`](../mediadecode/CHANGELOG.md).

## [Unreleased]

## [0.8.0] - 2026-08-24

### Added

- **`FfmpegDemuxer` implements `Demuxer::take_tracks`**, the
  owned-tracks door `mediadecode`'s demux tier gained in place of a
  `TrackInfo` / `TrackParams` `Clone` — see
  [`mediadecode`'s CHANGELOG](../mediadecode/CHANGELOG.md) for the
  message-carrier law behind that redirect. The implementation is
  `mem::take` on the session's own `Vec<TrackInfo<Ffmpeg>>`, built once
  at `open` and untouched until a caller takes it.

  `TrackExtra` does not gain `Clone` either, for the same law. An
  interim version of this entry hand-wrote one through the existing
  checked `try_clone`, to satisfy the same channel bound, and it came
  back out — a full `avcodec_parameters_copy` is not the refcount bump
  a message's `Clone` is required to be. `try_clone` and
  `clone_parameters` are unaffected: they remain the checked copy this
  type offers the one caller that genuinely wants an owned duplicate,
  and `Default` remains absent for the reason it always was — there is
  no checked substitute for `Parameters::default()` to route through.

### Changed (BREAKING)

- **Every struct-shaped enum variant across this crate's errors is now
  a tuple variant wrapping a named payload struct**, following
  `mediadecode`'s own `TrackParams` / `DemuxedPacket` /
  `SubtitlePayload` sweep and the shape this crate's own `Error`
  (`HwDeviceInitFailed`, `AllBackendsFailed`, `FallbackFailed`) and
  `FrameError` already used. A struct variant has no nameable type of
  its own to return from `is_<variant>` / `unwrap_<variant>` /
  `try_unwrap_<variant>`, and traps its fields instead of giving them a
  reusable, documented, accessor-bearing home.

  - **`ResampleError`** — all twelve struct arms (`SourceChanged`,
    `PlaneCount`, `SampleCount`, `UnsupportedRate`,
    `UnsupportedFormat`, `UnsupportedLayout`, `TooManyPlanes`,
    `TimestampOutOfRange`, `ChannelDropped`, `RematrixUnsupported`,
    `TimestampOverflow`, `OutputBuffer`) move to `Variant(Payload)`.
    `Again`, `AfterEof`, `Resample(Error)` and `QueueAlloc` are
    unchanged (already unit or newtype).
  - **`PacketBufferError`** — all eight arms (`Refcount`, `Bounds`,
    `SideDataEntries`, `SideDataArray`, `SideDataPayload`,
    `SideDataBytes`, `UnrepresentableFlags`, `SideDataAlloc`) move the
    same way; every payload keeps the enum's own `Copy + Clone + Debug
    + PartialEq + Eq`.
  - **`DemuxError`** — `AttachmentAlloc`, `ParametersMissing`,
    `ParametersAlloc`, `ParametersCopy`, `PacketBuffer`, `ReaderPanic`
    move; `Ffmpeg(#[from] ffmpeg_next::Error)` is unchanged.
  - **`boundary::PacketBuildError`** — `UnknownSideData` and
    `SideDataAlloc` move. This crate already had *two* same-named
    `SideDataAlloc` payloads in scope at this file (the read-side one
    on `PacketBufferError`, imported here, and this write-side one,
    native to this module) — the import is aliased
    `SideDataAlloc as BufferSideDataAlloc`; the native struct keeps the
    bare name.
  - **`convert::ConvertError`** — `UnsupportedPixelFormat`,
    `InvalidPlaneLayout`, `BufferAcquireFailed` move (found during the
    sweep, not in the original census — this enum already violated the
    same rule). This enum hand-writes `Display` rather than using
    `thiserror`; the extracted payloads keep that idiom (their own
    `impl Display`), and the enum's `Display` delegates to it per
    variant.
  - **`video::VideoDecodeError::PostCommitNeverResynced`** — also
    found during the sweep — moves to
    `PostCommitNeverResynced(PostCommitNeverResynced)`. `Decode` and
    `Convert` were already newtype variants.

  Every extracted payload is `thiserror`-derived where the enum already
  was (`#[error(transparent)]` on the wrapper, `#[from]`, the original
  message moved verbatim onto the payload's own `#[error("...")]`) or
  hand-`Display` where the enum already was (`ConvertError`) — no
  crate's error idiom changed, only the variant shape. A match that
  used to destructure fields now binds the payload and reads it through
  accessors, e.g. `Err(ResampleError::PlaneCount { expected, found })`
  becomes `Err(ResampleError::PlaneCount(p))` with `p.expected()` /
  `p.found()`.

  **The accessor face rides the reshape, same as `mediadecode`'s.**
  `ResampleError`, `PacketBufferError`, `DemuxError`,
  `boundary::PacketBuildError`, `convert::ConvertError`,
  `video::VideoDecodeError`, `audio::AudioDecodeError`,
  `subtitle::SubtitleDecodeError`, and `error::Error` — the crate's
  full error taxonomy, not just the arms the reshape sweep touched —
  now derive `derive_more::{IsVariant, Unwrap, TryUnwrap}`, with
  `#[unwrap(ref, ref_mut)]` and `#[try_unwrap(ref, ref_mut)]` — every
  arm answers `is_<variant>()`,
  `unwrap_<variant>()` / `_ref()` / `_mut()`, and
  `try_unwrap_<variant>()` / `_ref()` / `_mut()`. `Backend`,
  `PictureType`, `resampler::SpecEnd`, and `error::FallbackOrigin` gain
  `IsVariant` too; `FallbackOrigin`'s hand-written `is_post_commit()`
  and `ResampleError`'s hand-written `is_again()` are gone — the
  derive answers both now, `&self`-receiver in place of the old
  by-value one (transparent at every call site, all of which already
  used method-call syntax). This crate did not carry a `derive_more`
  dependency before this entry; it does now, with the `is_variant` /
  `unwrap` / `try_unwrap` features (no `display` — this crate's error
  types keep `thiserror` / hand-`Display`, unchanged).

## [0.7.0] - 2026-08-23

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
  open and its duplicate dropped. Every attachment track leaves the open
  with exactly one packet queued — or the open fails — before a single
  `av_read_frame` has run, which is what makes "exactly one packet,
  before any timed packet" a property of the construction rather than a
  promise the pull loop has to keep. A stream that declares cover art
  and parks no payload (a state this build's demuxers do not produce:
  `ff_add_attached_pic` sets the disposition and fills the packet in one
  call) still gets its one packet, empty and marked `synthesized`,
  rather than a place in a queue of packets that may never arrive.

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

  **A panicking reader is an error, not an abort.** libavformat drives
  an `open_reader` byte source from `extern "C"` callbacks, where a
  panic cannot unwind and terminates the process. Every call into the
  caller's reader runs under `catch_unwind`: the panic is latched,
  reported to C as an ordinary I/O error, and surfaced as
  `DemuxError::ReaderPanic` with the panic's own message from the next
  `open_reader`, `next_packet` or `seek`. The session is terminal from
  there. The caught payload is described and then deliberately
  forgotten: `panic_any` takes any `Send` value and safe code can give
  it a `Drop` that panics, and dropping it after the catch would send
  that second panic out of `read`/`seek` and into the `extern "C"`
  callback — the abort this guard exists to prevent, reached through the
  guard itself (measured: SIGABRT). Leaking one value on a path that has
  already made the session terminal is the cheaper half of that trade by
  a distance.

  **Container metadata is read as bytes, not trusted as text.**
  `ffmpeg_next`'s `DictionaryRef::get` builds its `&str` with
  `from_utf8_unchecked`, and FFmpeg does not validate demuxed metadata
  as UTF-8; a track's `filename` / `mimetype` therefore go through
  `av_dict_get` and a bounded walk, with invalid bytes replaced rather
  than trusted.

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

- **`FfmpegResampler` — `mediadecode`'s resample seam over
  `swresample`.** `FfmpegResampler::new(source, target)` takes both
  specs as [`ResampleSpec`]s and neither is inferred. The source is read
  off a demuxed track (`ResampleSpec::from_parameters`, the
  "source from `TrackInfo`" path) or off the opened decoder
  (`ResampleSpec::from_decoder`); the target is the caller's options.

  **Output timestamps are counted, not computed.** The timeline is
  anchored on the first *input* timestamp and advanced by the number of
  samples actually produced, so no arithmetic depends on how many
  samples a given `swr_convert_frame` happened to yield and the frames
  drained after `send_eof` continue the same line. The tail is real:
  the 44.1 kHz → 16 kHz lane pins that `send_eof` still has a frame to
  give, which is the difference between a file's last tens of
  milliseconds surviving and not.

  **A frame whose rate, sample format or channel layout is not the
  source spec is refused by name** (`ResampleError::SourceChanged`), and
  "nothing ready yet" is `ResampleError::Again` — the same mechanism
  `AudioStreamDecoder::receive_frame` uses, one tier along.
  `send_frame` after `send_eof` is refused too rather than silently
  accepted; `flush` is the way back to a reusable resampler.

  Two things this layer had to learn about FFmpeg to be correct, both
  recorded where they are relied on. A WAV without a
  `WAVE_FORMAT_EXTENSIBLE` channel mask genuinely declares **no**
  layout, and FFmpeg reports that unspecified layout in the codec
  parameters, in the codec context and on every decoded frame —
  substituting a default would make the source spec disagree with the
  frames it describes and refuse all of them. But `swr_init` *does*
  substitute a default internally, and then compares every frame handed
  to it against that one — so the frames this layer stages carry the
  post-init layout while the mid-stream check keeps comparing against
  the declared one. Custom and ambisonic layouts are refused outright:
  both keep a heap-allocated channel map that a `Copy` layout wrapper
  cannot own safely, and a silent approximation would be worse than a
  refusal. `from_parameters` / `from_decoder` answer `None` for them,
  and — because `ResampleSpec::new` is `const` and total —
  `FfmpegResampler::new` is the choke point that refuses them by name
  (`ResampleError::UnsupportedLayout`), along with a rate of zero or
  past `c_int`, `AV_SAMPLE_FMT_NONE`, and a planar spec with more
  channels than a frame has plane slots
  (`ResampleError::TooManyPlanes` — planar 22.2 is twenty-four planes
  against the model's eight, unusable as a source because no frame
  could carry it and worse as a target because the failure would land
  after `swr` had consumed the input). Nothing hazardous, and nothing
  unusable, reaches `swr` or a staged `AVFrame` whichever route a spec
  came in by.

  **The pair is checked too, not just each end.** Two individually
  valid layouts can still lose whole channels between them: `swr` mixes
  the channel positions its rematrix table knows and processes the rest
  of the input as though it were absent. Measured against the linked
  FFmpeg 9 with a tone isolated in each source channel, packed
  22.2 → mono drops fifteen of twenty-four and `cube` → stereo drops two
  of *eight* — so channel count is not the rule. The rule is asked of
  FFmpeg: `swr_build_matrix2` builds the matrix it would use, and a pair
  where any source channel reaches no output is refused
  (`ResampleError::ChannelDropped`), as is one FFmpeg will not matrix at
  all (`ResampleError::RematrixUnsupported`). LFE's mix level is forced
  non-zero for the question, so FFmpeg's deliberate downmix policy —
  which leaves LFE out — is not mistaken for a channel it cannot carry.
  Accepting such a conversion knowingly needs an explicit mix-matrix
  seat on the spec; until that exists, refusal is the honest answer.

  The pair judged is the **effective** one — the layouts `swr` is
  configured with, resolved before the check rather than after it. An
  unspecified layout becomes FFmpeg's default for its channel count, and
  twenty-four unspecified channels resolve to exactly the 22.2 whose
  explicit conversion is refused, so judging the declared pair left that
  routing a door. This is the second half of the crate's two-layout
  bookkeeping and it composes with the first: the **declared** layout is
  what decoded frames carry and stays the yardstick for the mid-stream
  refusal (*is this frame the stream I was built for?*), while the
  **effective** layout is what `swr`, every staged `AVFrame` and now
  this check use (*what will `swr` actually do?*). A maskless WAV — one
  channel resolving to mono, two to stereo — keeps converting exactly as
  before.

  **Nothing that can fail is left on the far side of the conversion.**
  Every resource a converted frame needs — the output frame, one
  refcounted view per plane, a placeholder for every unused slot, and
  the room in the ready queue — is acquired *before*
  `swr_convert_frame` touches a sample, on the ordinary path and on the
  EOF drain alike; the views are taken at full capacity and trimmed
  afterwards by a shrink that allocates nothing. The step that turns a
  converted frame into a `mediadecode` one is consequently infallible by
  signature, which is what makes the property hold rather than
  hold-for-now: a failure after `swr` has consumed input leaves a
  session no caller can act on, since retrying feeds the same samples
  twice and continuing loses them.

  **State is committed after the work it describes succeeds.** A frame
  refused for its geometry does not anchor or advance the output
  timeline; timestamps are counted with checked arithmetic and an
  overflow is `ResampleError::TimestampOverflow` — raised *before*
  `swr` consumes the frame, checked against the most samples the call
  could produce, so a refused conversion leaves a session the caller can
  still use (the check used to run after the conversion, which left the
  filter's history moved, an output frame built and dropped, and the
  timeline anchored where every later frame overflowed too) — rather
  than `i64::MAX` repeated; the anchor itself is rescaled with the checked rung before
  anything is staged, so a timestamp that cannot land on the output
  timeline is `ResampleError::TimestampOutOfRange` with the resampler
  untouched, rather than a clamp — a positive clamp used to surface only
  after `swr` had consumed the input, and a negative one landed on
  `i64::MIN`, which is `AV_NOPTS_VALUE`, erasing the timestamp instead
  of reporting it; plane geometry is settled in checked arithmetic *before* any
  allocation is sized from a frame header, and every allocation is
  checked (`frame::Audio::new` dereferences `av_frame_alloc` unchecked
  and discards `av_frame_get_buffer`'s return, so this crate allocates
  its audio frames through a local helper that does neither). `flush`
  rebuilds the `swr` context rather than draining it: a drain that gave
  up and a drain that finished are indistinguishable from outside, and a
  reset that reports success must not leave the previous stream's tail
  inside the filter.

- **`resample` feature, on by default.** `FfmpegResampler`,
  `ResampleError`, `ResampleSpec`, `SpecEnd` and the
  `ffmpeg-next/software-resampling` link they need move behind it, so a
  decode-only consumer building `--no-default-features --features std`
  drops `libswresample` from the link line entirely rather than merely
  not calling into it. The core `mediadecode::resampler` trait module
  stays ungated — it adds no dependency of its own, backend or
  otherwise, over what the crate already compiles unconditionally.

- **`SampleFormat::to_ffmpeg` / `SampleFormat::from_ffmpeg`** — the
  direction this newtype was missing. A raw sample-format integer read
  out of a container cannot be cast back into the bindgen enum to reach
  FFmpeg's safe API, so `to_ffmpeg` matches it against compile-time
  constants instead, exactly as `boundary::from_av_pixel_format` comes
  the other way.

- **`DataPacketExtra`, `AttachmentPacketExtra`, `TrackExtra`** — the
  demux tier's `*Extra` carriers, and `impl DemuxAdapter for Ffmpeg`
  binding them. `AttachmentPacketExtra::synthesized` records whether a
  payload came from a real packet or was built out of codec extradata,
  which is the first thing to check when an attachment looks wrong.
  `TrackExtra::disposition` is the raw `AV_DISPOSITION_*` bit set rather
  than `ffmpeg_next`'s `Disposition`, whose `from_bits_truncate` would
  drop bits this build has no constant for.

### Fixed

- **A sparse thumbnail track is a timed track, not one attachment.**
  Classification tested `AV_DISPOSITION_ATTACHED_PIC` alone, and FFmpeg
  documents `AV_DISPOSITION_TIMED_THUMBNAILS` — "the stream is sparse,
  and contains thumbnail images, often corresponding to chapter
  markers" — as *only ever* used together with it. Such a stream was
  therefore read as cover art: the exactly-once attachment contract
  queued the parked copy and the delivery loop dropped every timestamped
  thumbnail after it. The two bits are now read off the raw
  `AVStream.disposition` (`ffmpeg_next`'s `Disposition` mints no
  `TIMED_THUMBNAILS` constant, and its `from_bits_truncate` drops what
  it cannot name — which is how the distinction went missing), and a
  timed-thumbnail stream goes to the **`Video`** arm. That does not
  bend "cover art is an attachment, not video": the reason behind that
  ruling is that a still with no timeline must not look like a motion
  track, and this stream *is* on the timeline — sparse video, with a
  codec id, a frame size and packets that carry timestamps.

- **A packet's side data arrives with the packet, and a side-data-only
  packet is no longer mistaken for an empty marker.** The `*Extra`
  carriers have always documented a `side_data` seat and the conversion
  never filled one; measured on this repository's own generated corpus,
  every container carries at least one packet with real side data
  (`AV_PKT_DATA_SKIP_SAMPLES` — the encoder-delay trim an MP3 or AAC
  stream must be cut by), so that data was dropped at the boundary on
  every file. Worse, a packet with **no body** and only side data —
  FFmpeg's shape for `AV_PKT_DATA_NEW_EXTRADATA` and for a parameter
  change — read as "empty, skip it", so a decoder could be left running
  on parameters the container had already replaced, with nothing said.

  All four timed arms now collect side data — **whole, or not at all**.
  The collection is bounded (64 entries or as many as this build names,
  whichever is larger; 256 KiB; `try_reserve_exact`), and every bound is
  an **error**, not a truncation: a packet whose side data cannot be
  carried complete is refused by name
  (`PacketBufferError::SideDataEntries` / `SideDataBytes` /
  `SideDataAlloc`, surfacing as `DemuxError::PacketBuffer`) rather than
  delivered missing the entries a decoder acts on. Truncating would put
  the original defect back twice over: a body-bearing packet reaching
  the codec without its `NEW_EXTRADATA`, and a side-data-only packet
  losing its only content and vanishing as an empty marker. The arms
  also deliver a side-data-only packet with an owned empty buffer.

  The same rule reaches the pointers, not only the caps: a count is
  judged before the array it describes, so a malformed or over-cap count
  is refused whether or not the array happens to be null
  (`SideDataArray` names the missing array), and an entry declaring
  bytes it does not carry is refused (`SideDataPayload`) rather than
  read as an empty entry that charges the budget nothing. A zero-size
  entry is still a marker and still welcome.
  `SubtitlePacketExtra` and `DataPacketExtra` gained the seat the other
  two already had. `AttachmentPacketExtra` deliberately did not: an
  attachment is its bytes, so a packet carrying none carries no
  attachment, and that arm still answers `Ok(None)`.

  **And the reverse direction reattaches it**, which is what makes the
  capture worth anything: the three `ffmpeg_packet_from_*_packet`
  helpers are what the trait decoders hand to the codec, and they
  rebuilt a packet from body and timestamps alone. Measured end to end
  on `cover.mp3`: with the trim reattached the decoder returns 88 200
  samples — exactly the two seconds the file holds — and without it
  89 856, the encoder's padding included. A side-data type this build of
  FFmpeg does not name is refused
  (`PacketBuildError::UnknownSideData`) rather than dropped or handed to
  C as an invalid discriminant.

- **Every packet flag survives both directions.** Forward conversion
  went through `ffmpeg_next`'s `Packet::flags()`, whose `Flags` bit set
  names `KEY` and `CORRUPT` and truncates the rest away before this
  crate sees them; the reverse rebuilt only those two. `PacketFlags` is
  a bit set whose documented lossless door is `from_bits_retain`, so
  both directions were breaking its contract — and losing
  `AV_PKT_FLAG_DISCARD`, which tells a consumer to decode a packet and
  throw its output away, makes preroll output look like something to
  keep. `AVPacket.flags` is now read and written as the raw integer it
  is, so `DISCARD`, `TRUSTED`, `DISPOSABLE` and the bits nothing names
  yet all round-trip. A compile-time assertion states that every flag
  this build names fits the byte `PacketFlags` carries; a packet
  carrying one that does not is refused
  (`PacketBufferError::UnrepresentableFlags`) rather than delivered a
  bit short. The hoisted cover-art packet reads its flags through the
  same reader: it is built by hand from `AVStream.attached_pic` rather
  than through the boundary conversion, and it used to be built with
  none at all — FFmpeg marks an attached picture `AV_PKT_FLAG_KEY`, so
  every cover this crate delivered arrived saying it was not a
  keyframe. A synthesized attachment still carries no flags, because
  no packet was parked to read them from.

- **`TrackExtra` no longer derives `Clone` or `Default`, and the
  decoder handoff is checked.** Both derives went through
  `ffmpeg_next`'s `Clone` / `Default` for `Parameters`, so copying a
  track row from safe public code reached the same unchecked
  allocation — measured, a SIGSEGV — and the documented handoff was
  `parameters().clone()`, which is that same clone. `Clone` cannot
  report a failure, so the type does not implement it:
  `TrackExtra::try_clone` is the row copy with an answer, and
  `TrackExtra::clone_parameters` is the handoff that opens a decoder.
  `parameters()` still lends the parameters for inspection —
  `ResampleSpec::from_parameters` reads them — but nothing in this crate
  asks a caller to clone them unchecked any more.

  **And a `TrackExtra` cannot exist over parameters that were never
  allocated.** `Parameters::new()` / `Default` hand back a null-backed
  value when `avcodec_parameters_alloc` failed and report nothing, so
  checking only the destination of a copy left the source trusted: once
  the allocator recovered, `avcodec_parameters_copy(out, NULL)`
  dereferenced null from safe public code. `TrackExtra::new` is
  therefore fallible and refuses one
  (`DemuxError::ParametersMissing`), which gives the type a non-null
  invariant from birth, and the copier checks its source as well —
  belt and braces, because the invariant is a promise and the check is
  a fact. `ResampleSpec::from_parameters` and `from_decoder` grew the
  same guard: the first used to ask `medium()` on the way in, which
  dereferences the pointer inside ffmpeg-next before any code of this
  crate runs.

- **A track's codec parameters are copied with both fallible steps
  checked.** `ffmpeg_next`'s `Clone` for `Parameters` checks neither:
  `Parameters::new` does not test `avcodec_parameters_alloc` for null
  and `clone_from` dereferences it immediately — measured, that is a
  SIGSEGV under a failed allocation — while the copy's return value is
  discarded, so a partial copy (a failed extradata allocation, say)
  produced parameters that look complete and open a decoder wrong.
  Every track in the table goes through this, so it is a whole session
  built on parameters that are not the file's. A local helper checks
  both legs and reports `DemuxError::ParametersAlloc` /
  `ParametersCopy`; a partial copy is freed on the way out.

### Changed (BREAKING)

- **Wrapping a packet's payload answers `Result<Option<_>>`.**
  `FfmpegBuffer::from_packet`, `video_packet_from_ffmpeg`,
  `audio_packet_from_ffmpeg`, `subtitle_packet_from_ffmpeg`, the four
  `*_packet_from_ffmpeg_in` variants and `attachment_packet_from_ffmpeg`
  now return `Result<Option<_>, PacketBufferError>`.

  `Ok(None)` is a packet that carries no payload — the empty marker some
  demuxers emit, and the only thing a pull loop may skip. An `Err` is a
  payload that *is* there and could not be carried: `av_buffer_ref`
  refusing a reference under memory pressure, or a packet whose payload
  does not lie inside its own buffer. The single `None` these used to
  share made the second look like the first, so a demuxer under memory
  pressure dropped real compressed bytes and read on as though the file
  had said so. `FfmpegDemuxer::next_packet` surfaces the failure as
  `DemuxError::PacketBuffer`, naming the stream.

  Call sites that skipped `None` add one `?` or an `expect`; nothing
  else changes.

- **The three `ffmpeg_packet_from_*_packet` helpers answer
  `Result<Packet, PacketBuildError>`.** They can now fail for a reason
  `ffmpeg_next::Error` cannot spell: a side-data entry whose type this
  build of FFmpeg does not name, or whose allocation failed.
  `Error::PacketBuild` carries it into the decoder error types.

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


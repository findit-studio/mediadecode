# Changelog

All notable changes to the [`mediadecode`](https://crates.io/crates/mediadecode)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The sibling FFmpeg adapter has its own log at
[`mediadecode-ffmpeg/CHANGELOG.md`](../mediadecode-ffmpeg/CHANGELOG.md).

## [Unreleased]

### Added

- **The three optional matrices reach every type that owns a wire
  shape.** `channel::AudioChannelSpec`, `channel::AudioChannelLayout`
  and `packet::PacketFlags` gain `serde`, `arbitrary` and `quickcheck`
  impls; the matrices had covered only the two `channel` vocabularies.
  The two records travel as a map of their accessor names (with the
  vocabularies inside them still as slugs); `PacketFlags` travels as
  its raw bits, because a bit set has no name to spell and
  `from_bits_retain` has to carry bits this build has no constant for
  (FFmpeg's `AV_PKT_FLAG_TRUSTED` / `_DISPOSABLE`). Same reasoning, and
  the same wire, as `mediaframe::TrackDisposition`.
- **Feature wiring**: `serde` now enables `smol_str?/serde`, `alloc`
  enables `serde?/alloc` and `std` enables `serde?/std` — the records'
  `Vec` and `SmolStr` fields need them. `arbitrary`'s existing
  `smol_str?/arbitrary` is live for the first time.

### Removed

- **`serde` no longer enables `bitflags/serde`.** It was inert
  (bitflags 2 routes serde through a derive placed inside the
  `bitflags!` body, which `PacketFlags` does not carry), and its wire
  shape is a flag grammar — `"KEY | CORRUPT"` — in any human-readable
  format, which is not the shape `PacketFlags` takes.

## [0.4.0]

Both public dependencies cross **two** breaking minors at once:
`mediatime` 0.1 → 0.3 and `mediaframe` 0.1 → 0.3. Neither is an
internal detail — `mediatime::{Timebase, Timestamp, TimeRange}` and
eleven `mediaframe` types are re-exported as mediadecode's own public
surface and appear in its signatures — so a consumer holding a
`mediatime 0.1` / `mediaframe 0.1` value no longer type-checks against
this release. The upstream notes are the authority
([mediatime](https://github.com/findit-studio/mediatime/blob/main/CHANGELOG.md),
[mediaframe](https://github.com/findit-studio/mediaframe/blob/main/CHANGELOG.md));
what follows is only what changes **here**.

### Changed (BREAKING)

- **`mediatime` 0.1 → 0.3.** `Timebase`'s numerator and denominator
  are now signed (`u32 → i32`, `NonZeroU32 → NonZeroI32`), matching
  FFmpeg's `AVRational`; `Timebase::new` panics on a negative
  numerator or denominator, with `try_new` as the fallible form.
  Every `Timebase::new(n, NonZeroU32::new(d).unwrap())` call site
  becomes `Timebase::new(n, NonZeroI32::new(d).unwrap())`. mediatime
  0.2 → 0.3 additionally deleted the bare rescale ladder
  (`rescale_pts` / `rescale` / `duration_to_pts` → `checked_*` /
  `saturating_*`), corrected the rounding to `AV_ROUND_NEAR_INF` and
  moved `frames_to_duration` onto the new `Rate` type — **mediadecode
  calls none of those**, so nothing here moves for them; consumers
  that call them through mediadecode's re-export do.
- **`mediaframe` 0.1 → 0.3.** `PixelFormat::Unknown(u32)` is struck —
  the same numeric escape goes from eleven coded vocabularies in all.
  `PixelFormat::None` — a **named** member (FFmpeg's own
  `AV_PIX_FMT_NONE`, and the `Default`) — is what the adapter crates
  now produce where they used to produce `Unknown(raw as u32)`; the
  raw integer no longer rides along. `from_u32` returns
  `Option<Self>` and `to_u32` returns `Option<u32>`. The open
  extension arm mediaframe offers instead is `Other(SmolStr)`, which
  lives behind mediaframe's `alloc` feature; mediadecode pins
  mediaframe at the no-alloc tier (`default-features = false`,
  `features = ["frame"]` — unchanged from 0.1), so the re-exported
  vocabularies are **closed** here.
- **`VideoAdapter::PixelFormat` is now `Clone + Eq + Debug`**, was
  `Copy + Eq + Debug`. mediaframe 0.3 dropped `Copy` from the ten
  coded enums (the `Other` arm is heap-capable), so a backend binding
  `mediaframe::PixelFormat` could not satisfy the old bound. Relaxing
  a bound is free for implementors; consumers that relied on
  `A::PixelFormat: Copy` need a `.clone()` or a borrow.
  `AudioAdapter::ChannelLayout` was already `Clone` for the same
  reason (`AudioChannelLayout` carries an owned description) — this
  brings the two into line.
- **`VideoFrame::color` is no longer `const`** and returns a clone.
  `mediaframe::color::Info` lost `Copy` in 0.3. The signature is
  unchanged (`fn color(&self) -> ColorInfo`), matching what
  mediaframe's own `Info` accessors did with the same problem.
- **`VideoFrame::with_color` and `VideoFrame::set_color` are no longer
  `const`.** Assigning the field drops the previous `ColorInfo`, and
  mediaframe 0.3's `Info` acquires a destructor as soon as
  mediaframe's `alloc` feature is on — a const destructor is not
  evaluable (`E0493`). This is **not** conditional on how mediadecode
  pins mediaframe: Cargo unifies features across the whole graph, so
  any other crate depending on `mediaframe` with its defaults turns
  `alloc` on for this build too. The `const` therefore cannot be kept
  at either tier. Signatures are otherwise unchanged; only `const`
  evaluation of these two setters is lost.

### Changed

- Version bumped to 0.4.0. The sibling adapters move to 0.4.0 with it.
- **Fifteen `channel::ChannelLayoutKind` renderings move**
  ([#19](https://github.com/findit-studio/mediadecode/pull/19)). The
  multi-word names are hyphenated rather than spaced, so `Display` (and
  the new `as_str`) now print `"stereo-downmix"`, `"5.1-back"`,
  `"7.1-wide-back"` where they printed `"stereo downmix"`, `"5.1 back"`,
  `"7.1 wide back"`. Affected: `StereoDownmix`, `Ch2_1Alt`, `Ch5_0Back`,
  `Ch5_1Back`, `Ch5_1_2Back`, `Ch5_1_4Back`, `Ch6_0Front`, `Ch6_1Back`,
  `Ch6_1Front`, `Ch7_0Front`, `Ch7_1Wide`, `Ch7_1WideBack`,
  `Ch7_1TopBack`, `Ch7_1_4Back`, `Ch9_1_4Back`. The other 24 slugs are
  unchanged. A slug has to survive a CLI argument, a filename and an
  environment variable without quoting, and the sibling crates spell
  every multi-word slug this way. **No alias was kept**: the old spaced
  spelling is now a parse error, because one value has one name.

### Added

- **A text form for the two `channel` vocabularies**
  ([#19](https://github.com/findit-studio/mediadecode/pull/19)).
  `ChannelLayoutKind` and `AudioChannelOrderKind` gain a `const fn
  as_str` returning a canonical lowercase slug, `FromStr` reading it
  back, and one error type each — `ParseChannelLayoutKindError` /
  `ParseAudioChannelOrderKindError`, both `#[non_exhaustive]` unit
  structs that deliberately do not retain the rejected input.
  `AudioChannelOrderKind` also gains `Display`, which it did not have.
  The door folds ASCII case and nothing else (`"5.1-BACK"` parses,
  `"5.1-back "` does not) and allocates nothing, so it works at the
  no-`alloc` tier where both enums live. The numeric doors (`to_u32` /
  `as_u32` / `from_u32`) are unchanged and stay the compact form.
- **`serde` / `arbitrary` / `quickcheck` for those two vocabularies**
  ([#19](https://github.com/findit-studio/mediadecode/pull/19)). Before
  this, all three features compiled and no type in the crate
  implemented anything. serde carries them as their slug rather than
  their `u32` code: an unrecognised name is a deserialization error,
  where an unrecognised code would decode to `Unknown` / `Unspecified`
  and invent a value. Both generators choose uniformly from the
  variant roster rather than decoding an arbitrary `u32`, which would
  have spent the whole budget on the fall-through variant.

### Not affected

- `mediaframe::frame::Rational` widening to `i64` / `NonZeroI64`
  (mediaframe 0.2) has **no** site here: mediadecode names no
  `Rational`, `SampleAspectRatio` or `FrameRate`, and
  `mediadecode-ffmpeg`'s `Rational` is `ffmpeg_next::Rational`.
- The retired shared `mediaframe::parse::ParseError`, the new
  `KernelMatrix` / `KernelGamut` kernel selectors, and mediaframe's
  serde/buffa number → slug wire move have no site here either:
  mediadecode parses none of those vocabularies, calls no conversion
  kernel, and its `serde` feature does not reach mediaframe.

[0.4.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-v0.4.0

## [0.3.1] - 2026-06-14

Additive release ([#10](https://github.com/findit-studio/mediadecode/pull/10)).

### Added

- **`Clone` on `AudioFrame`.** The decode → resample pipelines that
  consume this crate fan one decoded audio frame out to several
  renditions (a 16 kHz and a 48 kHz resampler, say), which needs the
  frame itself to be clonable. Nothing about the clone is expensive by
  construction: an `AudioFrame`'s planes are the adapter's buffer type,
  and for the FFmpeg adapter that clone is an `av_buffer_ref` refcount
  bump. The sibling adapter's error types gain `Clone` in the same
  release — see
  [`mediadecode-ffmpeg` 0.3.1](../mediadecode-ffmpeg/CHANGELOG.md#031---2026-06-14).

[0.3.1]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-v0.3.1

## [0.3.0] - 2026-06-07

The shared vocabulary crate is renamed: `videoframe` 0.2 became
`mediaframe` 0.1 when its charter broadened from pixel/frame to all
media-stream vocabulary, and the old `videoframe` 0.x line is yanked.
mediadecode flips in lockstep
([#7](https://github.com/findit-studio/mediadecode/pull/7),
[#8](https://github.com/findit-studio/mediadecode/pull/8)). Because the
re-exported types *are* mediadecode's public surface, a rename upstream
is a break here even where this crate's own spellings do not move.

### Changed (BREAKING)

- **`videoframe` 0.2 → `mediaframe` 0.1.** Every re-export in
  `color`, `cfa`, `pixel_format` and `frame` now resolves to a
  `mediaframe` type. The import paths callers write are unchanged
  (`mediadecode::color::ColorMatrix`, `mediadecode::PixelFormat`, …),
  but the **type identity** behind them is a different crate, so a
  value obtained from `videoframe` 0.2 no longer type-checks here.
- **`frame::Plane::data()` is renamed `data_ref()`**, tracking
  mediaframe's `_ref` getter-suffix convention.
- **The `Color*` names are now aliases.** Upstream renamed
  `Color{Matrix,Primaries,Transfer,Range,Info}` to
  `{Matrix,Primaries,Transfer,DynamicRange,Info}`; mediadecode keeps
  the disambiguated spellings as re-export aliases
  (`DynamicRange as ColorRange`, `Info as ColorInfo`, …) so its own
  surface and its consumers stay source-compatible. Callers naming the
  upstream types directly see the new names.
- **`ColorMatrix::default()` is `Unspecified`**, was `Bt709` — an
  upstream default that this crate re-exports rather than defines.
- **`ColorTransfer::Bt470M` / `Bt470Bg` are renamed `Gamma22` /
  `Gamma28`**, again upstream. The wire mapping is untouched: the same
  H.273 codes, and the FFmpeg adapter still maps `AVCOL_TRC_GAMMA22` /
  `AVCOL_TRC_GAMMA28` to them.

### Changed

- Version bumped to 0.3.0 — pre-1.0 SemVer puts a breaking change in
  the minor. All three workspace members move together.

[0.3.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-v0.3.0

## [0.2.0] - 2026-05-15

The shared pixel-vocabulary layer (`color`, `cfa`, `pixel_format`,
frame primitives) now lives in the dedicated
[`videoframe`](https://crates.io/crates/videoframe) crate, so colconv,
mediadecode, and scenesdetect share a single canonical definition of
these types. mediadecode keeps the decoder-output story (timestamped
frames + per-backend extras) — pixel and color vocabulary are
re-exports.

### Changed (BREAKING)

- **`PixelFormat::Unknown` shape**: now `Unknown(u32)` (tuple variant
  carrying the raw wire identifier) instead of the prior unit
  variant. Lossless round-trip via `from_u32` / `to_u32`. Callers
  matching the variant must switch from `PixelFormat::Unknown` to
  `PixelFormat::Unknown(_)` (or `Unknown(raw)` if the raw value is
  useful). Boundary adapters (`mediadecode-ffmpeg`,
  `mediadecode-webcodecs`) have been updated to preserve the raw
  FFmpeg / WebCodecs identifier through the cast.
- **`FrameError` variants** are now newtype-tuple form wrapping
  payload structs (matches the convention in
  [`videoframe`](https://crates.io/crates/videoframe)). Affected
  variants: `TooManyVideoPlanes`, `TooManyAudioPlanes`. Callers
  destructuring `Err(FrameError::TooManyVideoPlanes { plane_count })`
  must switch to `Err(FrameError::TooManyVideoPlanes(p))` and call
  `p.plane_count()`. The payload structs
  ([`frame::TooManyVideoPlanes`](https://docs.rs/mediadecode/0.2/mediadecode/frame/struct.TooManyVideoPlanes.html),
  [`frame::TooManyAudioPlanes`](https://docs.rs/mediadecode/0.2/mediadecode/frame/struct.TooManyAudioPlanes.html))
  carry the same `plane_count: u8` and expose it via a
  `pub const fn plane_count(&self) -> u8` accessor. Both variants
  also carry `#[from]`, so `impl From<TooManyVideoPlanes> for FrameError`
  / `impl From<TooManyAudioPlanes> for FrameError` are auto-generated
  — inner helpers returning `Result<_, TooManyVideoPlanes>` can be
  `?`-propagated directly into `FrameError`.
- **`PixelFormat` enum body**: now sourced from
  [`videoframe::pixel_format::PixelFormat`](https://docs.rs/videoframe/0.2/videoframe/pixel_format/enum.PixelFormat.html)
  and covers **every** FFmpeg `n8.1` `AVPixelFormat` slug (~270 variants,
  closed against FFmpeg's vendored slug list via `cargo xtask check`)
  plus cinema-RAW additions. The previously-shipped subset (NV12, P010
  / P012 / P016, P210 / P212 / P216, P410 / P412 / P416, YUV420P, RGB24,
  …) is a strict subset of the new set, so most existing match arms
  still resolve; matches that relied on the enum being closed at the
  prior list will need updating (FFmpeg-derived sources now feed
  variants like `Yuv411p`, `Yuv410p`, `Yuv440p`, `Y210`, `V210`,
  `Xv36`, `Vuya`, `Bayer*`, `Xyz12`, etc.).

### Changed

- **`mediadecode::color::*`** (`ColorMatrix`, `ColorPrimaries`,
  `ColorTransfer`, `ColorRange`, `ChromaLocation`, `ColorInfo`,
  `DcpTargetGamut`) now re-export from `videoframe::color::*`. Public
  import paths (`mediadecode::color::ColorMatrix`, etc.) keep
  resolving — no source-level break for consumers.
- **`mediadecode::cfa::BayerPattern`** re-exports from
  `videoframe::frame::BayerPattern` (videoframe 0.2 dropped its
  separate `cfa` module; the type lives under `frame::bayer` and is
  re-exported via `frame::*`).
- **`mediadecode::frame::{Dimensions, Rect, Plane}`** re-export from
  `videoframe::frame::*`. The structural primitives are now the
  canonical videoframe definitions; the type identity is
  cross-crate-equal so values can flow without conversion.
- **Decoder-output types unchanged.** `VideoFrame<P, E, D>`,
  `AudioFrame<S, C, E, D>`, `SubtitleFrame<E, D>` remain in
  mediadecode — they carry timestamp + backend-extras, which sit
  above the pure pixel-vocabulary layer.

### Added

- **`videoframe`** as a new required dep (`videoframe = "0.2"`).
  Enabled with `features = ["frame"]` so every per-family pixel-format
  borrow type is available to downstream consumers.
- **`#[must_use]`** on every consuming `with_*` builder method
  across frame / packet / subtitle types. Catches accidental
  discards of the returned value at compile time.
- **`VideoFrame::try_new`** / **`AudioFrame::try_new`** —
  panic-free constructors returning `Result<Self, FrameError>`.
  The existing `new` constructors keep their panicking behavior
  for `const fn` / statically-known call sites; `try_new` is for
  runtime-checked callers (e.g. backend adapters validating
  decoder output). Pairs the `new` / `try_new` convention the
  rest of the crate already follows
  (`Plane::new` / `Plane::try_new`, `*_empty` / `try_*_empty`,
  …).
- **`mediadecode::frame::FrameError`** — enum capturing the
  validation failures the `try_new` constructors can surface
  (`TooManyVideoPlanes` / `TooManyAudioPlanes`). `non_exhaustive`,
  `IsVariant`, `thiserror::Error`.

### Fixed

- **`plane_count` validated against the fixed plane-array
  capacity.** `VideoFrame::new` asserts `plane_count <= 4`,
  `AudioFrame::new` asserts `plane_count <= 8`. Previously,
  out-of-range values would panic later inside `planes()` /
  `samples()`; now they fail-fast at construction.
  Closes [issue #4 — finding 1](https://github.com/findit-studio/mediadecode/issues/4).

[0.2.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-v0.2.0

## [0.1.0] - 2026-05-09

Initial public release.

### Added

- **Core enums.** `PixelFormat` (closed enum covering CPU and HW-tile
  formats: NV12, P010 / P012 / P016, P210 / P212 / P216, P410 / P412 /
  P416, YUV420P, RGB24, …), `SampleFormat`, `AudioChannelLayout`, and
  `BayerPattern` for RAW.
- **Color metadata.** H.273-aligned `ColorMatrix`, `ColorPrimaries`,
  `ColorTransfer`, `ColorRange`, `ChromaLocation`, plus the bundled
  `ColorInfo` type with `const fn` getters / `with_*` builders /
  `set_*` mutators.
- **Generic packet types.** `VideoPacket<A, B>`, `AudioPacket<A, B>`,
  `SubtitlePacket<A, B>` with the `PacketFlags` bitflags
  (`KEY` / `CORRUPT` / `DISCARD`).
- **Generic frame types.** `VideoFrame<A, B>`, `AudioFrame<A, B>`,
  `SubtitleFrame<A, B>`, alongside the `Plane<B>` plane carrier, the
  `Rect` rectangle, and the alloc-gated `SubtitlePayload<B>::Bitmap`
  variant.
- **Adapter traits.** `VideoAdapter`, `AudioAdapter`,
  `SubtitleAdapter` — fix the `extras` and `buffer` types for a
  whole pipeline once.
- **Decoder traits.** `VideoStreamDecoder`, `AudioStreamDecoder`,
  `SubtitleStreamDecoder` (push-style `send_packet` / `receive_frame`
  / `send_eof` / `flush` shape) plus `VideoFrameSource` /
  `AudioFrameSource`.
- **Time primitives.** `Timebase`, `Timestamp`, `TimeRange` re-exported
  from [`mediatime`](https://crates.io/crates/mediatime) so consumers
  don't need a separate dependency.
- **API style.** All public fields private; access via `field()`
  getters, consuming `with_field(value)` builders, and `set_field`
  mutators returning `&mut Self`. `const fn` everywhere the type
  allows. Panicking constructors paired with fallible `try_*`
  counterparts.
- **`no_std` core.** Builds without `std` or `alloc`; opt-in `alloc` /
  `std` features. Errors via `thiserror` over the stable
  `core::error::Error`, so `Error` impls survive
  `--no-default-features`.
- **Optional features.** `serde`, `arbitrary`, `quickcheck` (each
  forwards to `mediatime`'s matching feature).

[0.1.0]: https://github.com/findit-studio/mediadecode/releases/tag/mediadecode-v0.1.0

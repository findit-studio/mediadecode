# Changelog

All notable changes to the [`mediadecode`](https://crates.io/crates/mediadecode)
crate are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The sibling FFmpeg adapter has its own log at
[`mediadecode-ffmpeg/CHANGELOG.md`](../mediadecode-ffmpeg/CHANGELOG.md).

## [Unreleased]

## [0.8.0] - 2026-08-24

### Added

- **`Debug` across the demux tier; `Clone` where it stays cheap.**
  `TrackParams` and `TrackInfo` gain `Debug` (previously absent),
  hand-written and bounded on what their fields actually need
  (`E::TrackExtra`, `E::Text`, and each per-kind payload struct's own
  associated types) rather than on `E` itself, which `#[derive(Debug)]`
  cannot see past. `VideoPacket`, `AudioPacket`, `SubtitlePacket`,
  `DataPacket`, and `AttachmentPacket` derive `Clone` + `Debug` directly
  — their fields are the generic parameters themselves, so the derive's
  bound is already precise — and `DemuxedPacket` gets the same pair
  hand-written, for the same associated-type reason, since its five
  arms carry exactly those packet types.

  `TrackParams` and `TrackInfo` do **not** get `Clone`. An interim
  version of this entry gave them one — a real allocation-and-copy per
  clone, minted to satisfy a channel bound — and it violated the
  message-carrier law recovered from the frozen desktop tree: messages
  may be `Clone`, but `Clone` is always a refcount bump, never a deep
  copy. That impl is gone. In its place, `Demuxer` gains
  **`take_tracks`**, the owned-tracks door: the first call moves the
  whole track table out of the session; `tracks()` answers the empty
  slice afterward. The intended caller takes the table once, right
  after opening a session and before pulling any packet, and wraps
  each row in `Arc` for fan-out — one allocation per track, ever,
  rather than a deep copy per consumer.

### Changed (BREAKING)

- **Every struct-shaped enum variant is now a tuple variant wrapping a
  named payload struct** — `TrackParams`'s six arms, `DemuxedPacket`'s
  five arms, and `SubtitlePayload`'s two arms. The house rule this
  crate otherwise already followed everywhere else (see `FrameError`,
  which has always shaped its variants this way): a struct variant has
  no nameable type of its own, so it cannot answer
  `is_<variant>`/`unwrap_<variant>`/`try_unwrap_<variant>`, and its
  fields are trapped instead of being a reusable, documented,
  accessor-bearing type.

  - `TrackParams::Video { codec, width, height, pixel_format,
    frame_rate }` → `TrackParams::Video(VideoTrackParams<E>)`, and the
    same shape for `Audio` → `AudioTrackParams`, `Subtitle` →
    `SubtitleTrackParams`, `Data` → `DataTrackParams`, `Attachment` →
    `AttachmentTrackParams`, `Unknown` → `UnknownTrackParams`. Each
    payload has private fields, a positional `new(...)`, and a `const`
    accessor per field (`codec()`, `width()`, `pixel_format()`, …),
    matching whichever of `Copy`/`Clone`-only the field's own type is.

  - `DemuxedPacket::Video { track, packet }` →
    `DemuxedPacket::Video(VideoTrackPacket<E, D>)`, and the same shape
    for `Audio` → `AudioTrackPacket`, `Subtitle` →
    `SubtitleTrackPacket`, `Data` → `DataTrackPacket`, `Attachment` →
    `AttachmentTrackPacket`. Named `<Kind>TrackPacket` rather than
    reusing `VideoPacket` etc. — those names are already taken by the
    packet type nested *inside* the envelope. Each payload carries
    `track()`, `packet()` (borrowed), `into_packet()` and
    `into_parts()` (owned) — the enum's own `track()` / `kind()`
    accessors are unchanged and now read through these.

  - `SubtitlePayload::Text { text, language }` →
    `SubtitlePayload::Text(subtitle::Text<B>)`, and `Bitmap { regions
    }` → `SubtitlePayload::Bitmap(subtitle::Bitmap<B>)`.

  A match that used to destructure the struct fields now binds the
  payload and reads it through accessors:

  ```rust
  // Before
  match params { TrackParams::Video { width, height, .. } => .. }
  // After
  match params { TrackParams::Video(p) => (p.width(), p.height()) }
  ```

  **The accessor face rides the reshape.** `TrackParams`,
  `DemuxedPacket`, and `SubtitlePayload` now derive
  `derive_more::{IsVariant, Unwrap, TryUnwrap}`, with
  `#[unwrap(ref, ref_mut)]` and `#[try_unwrap(ref, ref_mut)]` on each
  enum — every arm answers `is_<variant>()`,
  `unwrap_<variant>()` / `_ref()` / `_mut()`, and
  `try_unwrap_<variant>()` / `_ref()` / `_mut()`. This is exactly what
  the reshape above was for: a struct variant has no nameable payload
  type to return from `unwrap_<variant>`, so the face could not exist
  until the fields moved into named structs. `TrackKind` gains
  `IsVariant` too. The `derive_more` dependency row picks up the
  `unwrap` / `try_unwrap` features alongside the existing `display` /
  `is_variant`.

## [0.7.0] - 2026-08-23

### Added

- **The demux tier — `mediadecode::demuxer`.** Until now the spine
  started at the decoder: something else had to open the container, name
  its tracks and hand over packets. That something is now a trait.

  - **`Demuxer`** — the opened-session face: `tracks()`, `next_packet()`,
    `seek(target)`. The session is **pull-style**; the caller owns the
    loop, holds one packet at a time whatever the file's length, and
    pulls only when it is ready. `Ok(None)` is end of file.

    Opening is deliberately **not** on the trait. FFmpeg opens from a
    path or a `Read + Seek` reader, a WebAssembly container parser opens
    from bytes — the same reason the decoder traits carry no
    constructor. And not every backend is a demuxer at all: R3D and BRAW
    are clip-oriented SDKs with no packets to hand out, so they keep
    joining a pipeline one tier up through `VideoFrameSource` /
    `AudioFrameSource`.

  - **`DemuxedPacket<E, D>`** — five arms (`Video` / `Audio` /
    `Subtitle` / `Data` / `Attachment`), each `{ track, packet }`. The
    packet types stay exactly what a decoder already accepts; the
    envelope carries the track coordinate, so nothing has to be stripped
    before `send_packet`.

  - **`DataPacket<E, D>`** and **`AttachmentPacket<E, D>`** — the two
    packet kinds that had no type because they have no decoder. A data
    packet is timed and never reordered, so like `SubtitlePacket` it has
    no DTS seat. An attachment carries **no timestamps at all**: a font
    or a cover image is not on the timeline.

  - **`TrackKind`** — the closed roster `{ Video, Audio, Subtitle, Data,
    Attachment, Unknown }`, minted here because `mediaframe` has a track
    *disposition* vocabulary but no kind vocabulary and the dependency
    direction forbids reaching the other way. **Cover art is
    `Attachment`, not `Video`** — one sample, no timeline, no motion —
    so the `Video` arm carries true motion video and nothing else.

  - **`TrackInfo<E>` / `TrackParams<E>` / `TrackIndex`** — the track
    table. `TrackIndex(i)` *is* the position of `tracks()[i]`, so a
    packet's coordinate needs no side map. `TrackInfo::kind()` is read
    off the `TrackParams` arm rather than stored beside it: there is no
    way to build a row whose advertised kind disagrees with its payload.
    Attachment identity — the filename it was attached under, its MIME
    type — lives on the row, not repeated on every packet.

  - **`DemuxAdapter`** — the demux tier's vocabulary, bundling the three
    existing adapter families and adding what only a demuxer needs:
    `DataExtra`, `AttachmentExtra`, `TrackExtra`, and a `Text` carrier
    for identity metadata (a seat, not a fixed string type, so the core
    stays allocator-free). The three families are bound to share the
    bundle's `CodecId`: a demuxer reads one container, and a container's
    track table has one codec-identifier column.

  Two contracts are stated at the face and binding on every
  implementation. **An attachment track delivers exactly one packet,
  before any timed packet** — synthesised when the container keeps the
  payload outside the packet stream (fonts, whose bytes live in codec
  extradata), hoisted when it is a real packet (cover art). And **`seek`
  obeys three laws**: it flushes session state; it lands on the nearest
  keyframe *at or before* the target, never after, because a decoder
  started past the target has no reference frame; and attachments are
  never replayed, however many times the session seeks.

- **The resample seam — `mediadecode::resampler`.** `AudioResampler`:
  `send_frame` / `receive_frame` / `send_eof` / `flush`, the
  `AudioStreamDecoder` push pair one tier along, with "nothing ready
  yet" signalled the same way — a backend-specific `Error` variant out
  of `receive_frame`.

  Three contract lines ride the face. **Both specs are explicit at
  construction**: the source is read off the track's `TrackInfo`, the
  target is the caller's — 16 kHz mono for a speech model, 48 kHz for an
  audio-event one, from the same file at the same time. The target is
  options, never a constant. **A mid-stream format change is a named
  refusal**: the face does not silently reconfigure, because that would
  resample the two halves of a stream on different terms and hand back
  one unbroken timeline built out of them. And **EOF drains the
  conversion tail** — a rate converter holds tens of milliseconds inside
  its filter, and skipping the drain loses the end of every file. Output
  timestamps are kept by delay-compensated accounting, so the drained
  tail continues the same timeline.

  No FFmpeg in the shape: `swresample` is merely the first
  implementation, and a pure-Rust polyphase resampler fits the same
  face.

## [0.6.0] - 2026-08-21

### Removed (BREAKING)

- **The `channel` module is gone, whole.** `ChannelLayoutKind`,
  `AudioChannelOrderKind`, `AudioChannelSpec`, `AudioChannelLayout`,
  their two parse errors (`ParseChannelLayoutKindError`,
  `ParseAudioChannelOrderKindError`) and the `serde` / `arbitrary` /
  `quickcheck` matrices that covered them are deleted, not moved and
  not deprecated. The vocabulary's house is `mediaframe::audio`, which
  now carries all four:

  | was (`mediadecode::channel`) | is (`mediaframe::audio`) |
  | ---------------------------- | ------------------------ |
  | `ChannelLayoutKind` (39 named + `Unknown`) | `ChannelLayout` (43 named + `Other(SmolStr)`) |
  | `AudioChannelOrderKind`      | `ChannelOrder`           |
  | `AudioChannelSpec`           | `ChannelSpec`            |
  | `AudioChannelLayout`         | `ChannelLayoutDescription` |

  **Nothing is re-exported.** `AudioFrame`'s channel-layout parameter
  `C` is generic and always was, so this crate never named a channel
  type in a signature and needs none now; a consumer that wants one
  adds `mediaframe` and spells `mediaframe::audio::*`. Both adapters in
  this workspace did exactly that.

  Renamings ride along inside the moved types. `ChannelLayout`'s slugs
  are FFmpeg's own, taken from `channel_layout_map[]` rather than from
  the constant names, so several spellings change: `"5.0"` / `"5.1"` /
  `"7.1(wide)"` are the **back**-speaker layouts and the side ones are
  qualified `"5.0(side)"` / `"5.1(side)"` / `"7.1(wide-side)"`;
  `"5.1.4"`, `"7.1.4"` and `"9.1.4"` carry no `(back)` suffix even
  though their constants do; `StereoDownmix` is `"downmix"`.
  `ChannelLayoutDescription` renames the free-text field `description`
  to `text` (`text()` / `with_text()` / `set_text()`), and its
  `known_kind()` returns `&ChannelLayout` — borrowed, because the
  escape arm costs the type `Copy` — with `ChannelLayout::default()`
  (the `Other("")` sentinel) where `ChannelLayoutKind::Unknown` used to
  sit.

  Four defects leave with the table that held them rather than being
  fixed in place:

  - `Ch7_1Wide` and `Ch7_1WideBack` carried each other's slugs. FFmpeg
    gives the unqualified `"7.1(wide)"` to the **back** layout and
    qualifies the side one; this crate had it the other way round.
  - `Ch7_1TopBack` named nothing. `AV_CH_LAYOUT_7POINT1_TOP_BACK` is a
    `#define` alias of `AV_CH_LAYOUT_5POINT1POINT2_BACK`, matched
    earlier in this workspace's own FFmpeg adapter, so the variant was
    unreachable — a value writable through `as_str` / `to_u32` and
    produced by nothing.
  - `Ch2_2`'s doc promised "left, right, subwoofer, and an additional
    channel"; `AV_CH_LAYOUT_2_2`'s mask is FL+FR+SL+SR — no LFE and no
    centre. Upstream spells it `QuadSide`, after its `"quad(side)"`
    slug, for that reason.
  - `Ch5_1`'s doc described rear speakers while the adapter mapped it
    from `AV_CH_LAYOUT_5POINT1`, which is the side layout. The two
    sentences could not both be true.

  Also gone: the runtime roster check that could not see any of this.
  It walked `from_u32`, so a variant with an `as_str` and a `to_u32`
  but no `from_u32` arm stayed invisible to it — which is exactly the
  shape `Ch7_1TopBack` had. Upstream's `roster!` pins its list with a
  compile-time exhaustive match instead.

- **`smol_str` is no longer a dependency.** It was reachable only from
  the two deleted records, so the optional dep and its four feature
  edges (`alloc`'s `smol_str`, `std`'s `smol_str/default`, `serde`'s
  `smol_str?/serde`, `arbitrary`'s `smol_str?/arbitrary`) go with them.
  A build that turned on `mediadecode/alloc` to get `smol_str` into the
  graph now names it directly.

### Changed (BREAKING)

- **`mediaframe` 0.5 → 0.6**, at the same pin as before
  (`default-features = false`, `frame` — the no-alloc tier). This is
  the release that carries the channel household, and `mediaframe` is a
  public dependency here (`color`, `pixel_format`, `cfa` and `frame`
  are re-export modules), so a downstream still spelling `mediaframe`
  0.5 gets two versions of it in one graph. Bump in lockstep.

  Note the tier: `mediaframe::audio` is compiled only at `mediaframe`'s
  alloc-or-std tier, and this crate's pin does not enable it. A
  consumer naming `mediaframe::audio::*` turns on `mediaframe/alloc`
  (or `std`) on its own dependency edge, as both adapters in this
  workspace do.

## [0.5.0]

Tracks `mediaframe` 0.4 → 0.5, a breaking minor. No `mediadecode`
source line changed: the whole diff is the pin, the version and this
note. It is still breaking, because `mediaframe` is a **public**
dependency — `color`, `pixel_format`, `cfa` and `frame` are re-export
modules — so a downstream that still spells `mediaframe` 0.4 gets
`expected ColorMatrix, found Matrix`, with the compiler adding "there
are multiple different versions of crate `mediaframe` in the
dependency graph". Bump `mediaframe` in lockstep and that error goes
away.

### Changed (BREAKING)

- **`mediaframe` 0.4 → 0.5**, at the same pin as before
  (`default-features = false`, `frame` — the no-alloc tier). Upstream
  breaks on three counts; here is where each one lands.

  - **`FromStr::Err` is `Infallible` at the alloc / std tier** for ten
    vocabularies, six of which this crate re-exports
    (`pixel_format::PixelFormat` and `color`'s `Matrix`, `Primaries`,
    `Transfer`, `DynamicRange`, `ChromaLocation`). At *this* crate's
    pin nothing moves — the parse still returns
    `ParsePixelFormatError` / `ParseMatrixError`, because the escape
    arm those errors exist for is `alloc`-gated and this crate does
    not enable `alloc` on `mediaframe`. But the tier is not this
    crate's to fix: any dependency anywhere in the graph that turns on
    `mediaframe/alloc` unifies the feature and flips the re-exported
    associated type to `core::convert::Infallible`. Code that names
    the old error through a `mediadecode::` path — a `match` arm, a
    `From` impl, an annotated binding — moves the way upstream
    describes; code that only propagated it needs nothing.
  - **`subtitle::TrackOrigin` opened** (no longer `Copy`, `as_str` no
    longer `const fn -> &'static str`, `to_u32` returns `Option<u32>`,
    both wire forms changed) — **not reachable from here**.
    `mediaframe`'s `subtitle` module is compiled only at the `alloc`
    tier, and this crate neither enables it nor re-exports it. The
    `to_u32` and `Parse*Error` names that do appear in this crate are
    its own — `channel::ChannelLayoutKind`, `channel::AudioChannelOrderKind`
    — and are untouched.
  - **`subtitle::Format::PgsSub` merged into `HdmvPgs`** — likewise
    not reachable; this crate names neither.

### Added

- **`ROSTER` on the re-exported open vocabularies.**
  `mediadecode::pixel_format::PixelFormat` and the five re-exported
  `color` enums gain `pub const ROSTER: &'static [Self]`, upstream's
  declaration-order list of the named variants, excluding the open
  escape. It is available at this crate's no-alloc pin. Consumers that
  were hand-copying one of these vocabularies can read the list
  instead.

### Changed

- **`cfa::BayerPattern` is closed.** Upstream removed
  `#[non_exhaustive]`, so a downstream matching
  `mediadecode::cfa::BayerPattern` may now drop its wildcard arm and
  get a completeness proof from the compiler. Existing matches keep
  compiling; this only removes a restriction.

## [0.4.0] - 2026-08-19

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

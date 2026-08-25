//! The **amputation seam**: FFmpeg's bytes leave here, copied once,
//! as Rust-owned memory.
//!
//! An `AVPacket`'s payload and an `AVFrame`'s planes both live in
//! `AVBufferRef`s — FFmpeg's own refcounted allocations. Through 0.8
//! this crate handed those out directly, wrapped in an `FfmpegBuffer`
//! whose `AsRef<[u8]>` pointed straight into libavcodec's memory. That
//! type is gone. Every byte that crosses this boundary is now copied
//! into an [`FfmpegBytes`], which is what the core's
//! [D-seat amputation contract][law] requires: owned, `Send + Sync`,
//! clone-is-a-refcount-bump, and with no FFmpeg lifetime riding along.
//!
//! What is left in this module is everything the copy still has to
//! *judge*. A packet's payload has to be proved to lie inside the
//! buffer that owns it before a byte of it is read, its side data has
//! to be carried whole or refused, and its flags have to fit the
//! portable set — so [`PacketBufferError`] and its payload structs
//! outlive the buffer type they were written for. The bounds check in
//! particular matters *more* now, not less: 0.8 formed a view over the
//! claimed range, 0.9 reads it.
//!
//! # The one thing the amputation costs
//!
//! The `Arc<[u8]>` behind [`FfmpegBytes`] has no fallible constructor
//! on stable Rust, so the copy itself aborts on allocation failure
//! rather than returning an error.
//! Everything that bounds *how much* can be asked for — the side-data
//! entry and byte caps, the plane-geometry checks — is unchanged and
//! still runs before any allocation, so a hostile stream cannot reach
//! that abort by demanding memory; only a genuinely exhausted
//! allocator can.
//!
//! [`payload_of`] is where the per-packet half of that bounding lives:
//! every packet body this crate copies passes through it, and it
//! refuses an over-budget claim before reading a byte. See
//! [`crate::limits`] for the budgets and their defaults.
//!
//! # The funnel's accounting
//!
//! Every [`FfmpegBytes`] in this crate is built by
//! [`FfmpegBytes::copy_from_slice`], [`FfmpegBytes::from_rows`] or
//! [`FfmpegBytes::empty`], and **every one of those call sites is
//! bounded before it allocates**. The table is kept here, beside the
//! constructors, so that a new exit has to answer the question the
//! existing ones already answered — the discipline is inherited by
//! being written down where the next author will be standing.
//!
//! Three review rounds found bypasses that each looked like an
//! exception: a plane path with no ceiling, an attachment whose
//! payload was copied by `avcodec_parameters_copy` before its budget
//! was charged, a resampler amplifying a small input into a huge
//! output, and then `coded_side_data` — a third heap seat on the same
//! wholesale parameter copy, where a MOV `prof` atom puts an ICC
//! profile. None of them were exceptions. They were rows nobody had
//! written down.
//!
//! The third one is why the table below has a section it did not need
//! at first. `FfmpegBytes` is not the only place this crate copies
//! attacker-sized bytes: `AVCodecParameters` has heap seats of its own,
//! and the wholesale FFI copy that used to duplicate them took every
//! one — including any this crate had never enumerated. That copy is
//! gone; see
//! [`bounded_clone_parameters`](crate::extras::bounded_clone_parameters).
//!
//! | construction site | what it carries | what bounds it |
//! |---|---|---|
//! | [`payload_of`] | any packet's payload | its `budget` argument — [`PacketLimits::max_packet_bytes`](crate::PacketLimits::max_packet_bytes) for timed packets, [`DemuxLimits::max_attachment_bytes`](crate::DemuxLimits::max_attachment_bytes) for attachments — judged against the declared `size` before a byte is read |
//! | `convert::copy_out_planes`, tight stride | one video or image plane | [`FrameLimits::max_pixels`](crate::FrameLimits::max_pixels) and [`FrameLimits::max_frame_bytes`](crate::FrameLimits::max_frame_bytes), both in a judge-pass that runs before any plane is allocated; `max_pixels` also reaches libavcodec |
//! | `convert::copy_out_planes`, padded stride | one compacted plane, via [`FfmpegBytes::from_rows`] | the same pre-pass |
//! | `convert::av_frame_to_audio_frame` | one audio plane | `max_frame_bytes`, checked over `plane_bytes × plane_count` before the loop |
//! | `convert::collect_side_data` | one frame side-data entry | `SIDE_DATA_MAX_ENTRIES` (64) and `SIDE_DATA_MAX_TOTAL_BYTES` (256 KiB), plus `try_reserve_exact` |
//! | `boundary::packet_side_data` | one packet side-data entry | the same two caps, as refusals rather than truncation |
//! | `convert::av_subtitle_to_subtitle_frame`, text | concatenated cue text | `SUBTITLE_MAX_TEXT_BYTES_PER_RECT` (64 KiB), `SUBTITLE_MAX_TEXT_TOTAL_BYTES` (256 KiB), `SUBTITLE_MAX_RECTS` (64) |
//! | …, bitmap | one paletted rect | `SUBTITLE_MAX_BITMAP_BYTES_PER_RECT` (16 MiB), `SUBTITLE_MAX_BITMAP_TOTAL_BYTES` (32 MiB), `SUBTITLE_MAX_RECTS` |
//! | …, palette | an RGBA palette | structurally fixed at 256 × 4 bytes by the format |
//! | `demuxer::extradata_payload` | a synthesized attachment (a font) | `demuxer::admit_attachments`, which charges every attachment in the file — per-attachment **and** aggregate — before the track loop allocates anything; re-checked here against the per-attachment ceiling |
//! | `demuxer::attached_pic_payload` | a hoisted cover-art packet | the same admission pass, then `payload_of`'s budget |
//! | `resampler::finish_output` | one converted audio plane | `FfmpegResampler::check_output_bytes`, against `max_frame_bytes`, run before the output `AVFrame` is allocated |
//! | every [`FfmpegBytes::empty`] site | nothing | structurally zero: placeholder plane slots, a payload-less packet, a null palette, a marker side-data entry |
//!
//! # The other heap this crate copies
//!
//! `AVCodecParameters` is not an [`FfmpegBytes`] and never passes
//! through this module, but it is the same class of exposure — three
//! heap seats, all sized by the file — so its rows belong in the same
//! accounting.
//!
//! | construction site | what it carries | what bounds it |
//! |---|---|---|
//! | [`bounded_clone_parameters`](crate::extras::bounded_clone_parameters), `extradata` | SPS/PPS and codec headers | [`DemuxLimits::max_codec_parameter_bytes`](crate::DemuxLimits::max_codec_parameter_bytes), measured by `measure_parameters` before the copy |
//! | …, `coded_side_data` | the descriptor array and each entry's payload — a MOV `prof` atom's ICC profile among them | the same seat, counting the array as well as the payloads |
//! | …, `ch_layout` custom map | a channel map | the same seat; the one FFmpeg call left on this path (`av_channel_layout_copy`) copies exactly this field, at a size measured first |
//! | `demuxer::admit_streams` | nothing — it only measures | runs over **every** stream before the track loop clones anything, and charges the whole-file [`max_total_codec_parameter_bytes`](crate::DemuxLimits::max_total_codec_parameter_bytes) |
//! | `decoder::build_codec_context` → `avcodec_parameters_to_context` | the same three seats, copied *into* an `AVCodecContext` | **the choke point**: measured and admitted against [`DecoderLimits::max_codec_parameter_bytes`](crate::DecoderLimits::max_codec_parameter_bytes) right there. Every decoder in this crate opens through this function — the four session `open`s, the HW probe's `build_state`, its per-backend advances, the software fallback — and none of them reaches `avcodec_parameters_to_context` any other way |
//! | `image::FfmpegImageDecoder::decode` → `boundary::try_packet_copy` | the caller's compressed bytes, duplicated into an `AVPacket` | [`DecoderLimits::max_image_input_bytes`](crate::DecoderLimits::max_image_input_bytes), defaulting to the attachment family so the direct road is no more permissive than the demuxed one |
//! | `boundary::ffmpeg_packet_from_{video,audio,subtitle}_packet` → `try_packet_copy` | the caller's compressed bytes, duplicated into an `AVPacket` — **the send leg** | [`DecoderLimits::max_packet_bytes`](crate::DecoderLimits::max_packet_bytes), judged before the allocation. The same seat the receive leg (`payload_of`) judges, so a byte count refused coming out of a container is refused going into a decoder |
//! | still `pal8` palette plane | a fixed `AVPALETTE_SIZE` run | the **format**, not a seat: 256 × `AV_PIX_FMT_RGB32`, always, with no number a file gets to choose |
//!
//! # The rule
//!
//! **A carrier whose size comes from a file is bounded by a seat in
//! [`crate::limits`]; a carrier whose size is a property of a format is
//! bounded by that format.** There is no third kind, and a site that
//! looks like one has not been thought about yet.
//!
//! And the corollary the third round bought: **no code path hands
//! attacker-sized data to a wholesale FFI copy** — a copy that
//! duplicates every field of a struct duplicates the fields nobody
//! enumerated, which is a budget bypass that arrives with the next
//! FFmpeg release rather than with the next commit.
//!
//! # The substrate's knobs, and where this crate stops
//!
//! Everything above is **tier one** of the [resource governance
//! contract][gov]: allocations this crate makes itself, each bounded by
//! a named seat or by a format. This table is that tier's proof.
//!
//! Tier two is the other half — FFmpeg's own resource knobs, set at
//! every point libavcodec and libavformat offer one. They bound
//! allocations this crate does not make and could not otherwise see:
//!
//! | knob | where it is set | what it bounds |
//! |---|---|---|
//! | `AVCodecContext.max_pixels` | every opened decoder | the caller's pixel limit, **verbatim**, applied by `ff_set_dimensions` to the raw dimensions. Extent, not cost: what a frame *costs* is the byte judge's question, two rows down |
//! | the `get_format` coded-dims ask | the hardware road | the **pool's own declared extent**, asked of `avcodec_get_hw_frames_parameters` before the pool is initialised — `max_pixels` is applied to the *display* dims, which a cropped stream can make 2000x smaller |
//! | the `get_format` byte judge | the hardware road | the **pool's** cost, priced through [`crate::footprint`] against `max_frame_bytes`. **Fails closed**: a pool that will not declare its dimensions and layout is a pool that cannot be judged, and the codec-alignment fallback that used to stand in could answer *smaller* than the pool it was standing in for |
//! | the `get_buffer2` byte judge | every software decode | what the allocator will actually take for this frame — pictures and audio both, priced through [`crate::footprint`] against the caller's own `max_frame_bytes`, carried in the codec context's callback state |
//! | the pre-transfer judge | every `av_hwframe_transfer_data` | the CPU destination a hardware download allocates, priced at the frames-context pool dims — folding **every** candidate format FFmpeg may pick, priceable or not, since FFmpeg does the picking |
//! | `probesize` / `formatprobesize` | both demux entrypoints | what the format probe and stream analysis may consume |
//! | `max_streams` | both demux entrypoints | the `AVStream` array a header can conjure |
//! | the `AVIOContext` byte meter | the **reader** demux entrypoint | total bytes libavformat is handed, hard — past the budget the reader stops answering |
//!
//! Two of those knobs used to carry *translated* byte ceilings —
//! `max_pixels` as `min(the caller's limit, bytes / 16)` and
//! `max_samples` as `bytes / 8` — so that the byte budget could bite
//! before libavcodec allocated. Both translations charged every stream
//! the worst format in existence, and both over-refused ordinary media:
//! a 1920x1080 `yuv420p` frame under a 4 MiB budget, a 6-channel `s16`
//! frame under 64 KiB. They are gone. The byte budget is enforced by
//! the `get_buffer2` judge, which is *itself* a pre-allocation seat —
//! `get_buffer2` **is** the allocation — and prices the frame's real
//! format at its real dimensions. An exact judge at the allocation
//! beats an approximate one before it.
//!
//! Where a layout cannot be priced at all, these judges charge
//! [`crate::footprint::video_frame_bytes_upper_bound`] — the same
//! dimension alignment and per-plane overhead at the widest per-pixel
//! rate the census finds — rather than a bare `w * h * rate`, which
//! omits both and could land *below* the accurate path it was standing
//! in for. A conservative fallback that can under-state is not
//! conservative.
//!
//! **These are defense in depth, not a proof.** Each bounds what it was
//! built to bound; together they cover every interposition point FFmpeg
//! exposes, which is not the same as covering FFmpeg.
//!
//! ## What the demux seats cannot reach, and why they exist anyway
//!
//! `avformat_open_input` and `avformat_find_stream_info` build the
//! attached picture, the extradata and the coded side data out of the
//! file themselves. The attachment and parameter seats in the table
//! above therefore measure this crate's *copies* of buffers libavformat
//! has already allocated — too late, by construction, to have prevented
//! the original.
//!
//! A parser cannot allocate from bytes it was never handed, so the
//! input is bounded instead: that is what the probe knobs and the byte
//! meter are for. What is **not** bounded is allocation *amplification*
//! inside a parser — a container can describe, in a handful of bytes, a
//! structure whose in-memory form is far larger, and nothing outside
//! libavformat can observe it happen. Bounding that output is the
//! substrate's own hardening territory; FFmpeg keeps `max_streams`,
//! `max_index_size` and `max_picture_buffer` for it, and this crate
//! sets the first.
//!
//! The hard meter also does not reach the **path** entrypoint: it needs
//! an `AVIOContext` this crate owns, and a path is opened by
//! libavformat's own protocol layer. The probe knobs still apply there;
//! a caller who wants the meter on a file opens it as a reader.
//!
//! That gap is **tier three**, and it is named rather than hedged: see
//! the [contract][gov] for the boundary and for the OS-level instrument
//! a deployment needing a hard memory bound puts underneath all of
//! this. This crate is not a hypervisor for FFmpeg, and its seats
//! compose with that instrument rather than replacing it.
//!
//! [gov]: mediadecode::adapter#the-resource-governance-contract
//!
//! And the capstone, which is what every seat in this table is finally
//! for: **a judge must dominate the allocator's arithmetic, not the
//! payload's.** A budget compared against what the bytes weigh is not a
//! budget on what will be spent — see [`crate::footprint`] for the
//! measured gap and for the two judges that were caught paying it.
//!
//! And the corollary the ninth bought, which is about *whether* to
//! carry at all rather than how much: **a payload that carries
//! addresses instead of bytes is uncarriable.** `AV_PKT_FLAG_TRUSTED`
//! marks one — the wrapped-`AVFrame` producers use it for a body that
//! is an `AVFrame` pointer structure — and copying it mints a carrier
//! that passes every property this table exists to guarantee and
//! dangles the moment its source drops. It is refused on both legs
//! ([`payload_of`] and the reverse builders), because either alone
//! leaves the loop open. See [`TrustedPayload`].
//!
//! And the corollary the seventh bought, about the *inputs* to every
//! guard above rather than the guards themselves: **a number a file
//! chooses is judged or refused, never clipped.** A seat that bounds a
//! byte product still trusts the fields the product is computed from,
//! so a clamped sample count or channel count does not trip any budget
//! — it produces a smaller, plausible frame that no ceiling has any
//! reason to stop. Two of those were live on the audio path (a floored
//! negative `nb_samples`, a channel count clipped to `u8::MAX`), and
//! both turned a malformed header into a well-formed-looking frame,
//! which is strictly worse than an error. The audio road now carries no
//! lossy clamp; the one floor left, `sample_rate`, is censused at its
//! site with the reason it is metadata and sizes nothing.
//!
//! [law]: mediadecode::adapter#the-d-seat-amputation-contract

use std::{
  fmt,
  sync::{Arc, OnceLock},
};

use derive_more::{IsVariant, TryUnwrap, Unwrap};

/// The bytes every packet and frame this crate produces are carried in.
///
/// Owned, `Send + Sync`, `'static`, and clone-is-a-refcount-bump: the
/// core's [D-seat amputation contract][law], satisfied. Nothing inside
/// reaches back into libavcodec.
///
/// # Why it is opaque
///
/// The obvious spelling was the bare `Arc<[u8]>` this type wraps, and
/// 0.9.0's first cut used it. It is opaque for one reason, and the
/// reason is not aesthetics:
///
/// **`Arc<[u8]>` is one storage strategy, and it is not going to be the
/// only one.** Every exit currently allocates, copies, and frees per
/// frame; a decode loop at 4K is asking the global allocator for eight
/// megabytes sixty times a second and handing it back. The recorded
/// answer is a plane pool — reusable slabs handed out at the boundary
/// and returned when the last consumer drops them
/// ([issue #35](https://github.com/findit-studio/mediadecode/issues/35)).
/// A pooled slab is a different carrier with the same contract: still
/// owned, still `Send + Sync`, still refcount-cloned, still holding no
/// FFmpeg lifetime.
///
/// If the carrier were `Arc<[u8]>` in the public aliases, adding the
/// pool would change the type of every frame and every packet in the
/// crate — a breaking release for a change consumers cannot observe.
/// Behind this newtype it is a new arm of a **private** enum: no
/// signature moves, no consumer recompiles differently, and the
/// `AsRef<[u8]>` a consumer actually programs against is unchanged.
/// That extension point *is* this type's justification for existing.
///
/// The enum has exactly one arm today. It gains the second when the
/// pool is built and not before — this codebase does not carry members
/// nothing can produce.
///
/// [law]: mediadecode::adapter#the-d-seat-amputation-contract
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct FfmpegBytes(Inner);

/// The storage behind [`FfmpegBytes`]. **Private, and the point.**
///
/// One arm today; see the type's own docs for the arm that is coming
/// and why it can arrive without a breaking release.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Inner {
  /// A refcounted slice, allocated by the copy at the boundary.
  Shared(Arc<[u8]>),
}

impl Default for Inner {
  #[inline]
  fn default() -> Self {
    Self::Shared(shared_empty())
  }
}

impl FfmpegBytes {
  /// Copies `bytes` into a fresh carrier.
  ///
  /// **The copy site.** Every exit in this crate lands here or on
  /// [`Self::empty`], so "one copy at the boundary" is a property of
  /// one constructor rather than a promise thirty call sites keep —
  /// and it is the one place a future pooled arm has to be taught
  /// about.
  ///
  /// Public because the reverse direction needs it: a consumer
  /// building a packet to feed back into a decoder has bytes and needs
  /// a carrier, and the alternative is an opaque type nobody outside
  /// this crate can construct.
  ///
  /// A zero-length copy lands on the shared empty allocation rather
  /// than minting its own.
  #[inline]
  pub fn copy_from_slice(bytes: &[u8]) -> Self {
    if bytes.is_empty() {
      return Self::empty();
    }
    Self(Inner::Shared(Arc::from(bytes)))
  }

  /// The zero-length carrier, shared.
  ///
  /// Placeholder plane slots and payload-less packets are frequent — a
  /// video frame allocates four slots and populates one to three of
  /// them — and each would otherwise be its own `Arc` header
  /// allocation. One empty allocation for the process, cloned by
  /// refcount, instead.
  #[inline]
  pub fn empty() -> Self {
    Self(Inner::Shared(shared_empty()))
  }

  /// Builds a carrier of `rows * row_bytes` bytes by writing each row
  /// in turn — **one allocation, no staging buffer**.
  ///
  /// This is the road a padded plane takes. FFmpeg lays such a plane
  /// out `linesize` bytes per row while only the first `row_bytes` of
  /// each are the decoder's output, so the copy has to be row-wise and
  /// the destination is contiguous. The obvious spelling — build a
  /// `Vec`, then `Arc::from` it — allocates the whole plane **twice**
  /// and copies it twice, so a 250 MiB frame peaks at 750 MiB counting
  /// FFmpeg's own. Writing the rows straight into
  /// `Arc::new_uninit_slice` leaves the unavoidable 2×: FFmpeg's plane
  /// and ours.
  ///
  /// `row(i)` must answer a slice of exactly `row_bytes`; a shorter or
  /// longer one is a bug in the caller's geometry and panics rather
  /// than leaving the tail of the allocation uninitialised. That
  /// assertion is what discharges the initialisation contract for the
  /// `assume_init` below: the loop visits every row, each row fills its
  /// full width, and `rows * row_bytes` is the whole allocation.
  ///
  /// Crate-internal: the public face is [`Self::copy_from_slice`], and
  /// this shape only makes sense to a caller that already holds a
  /// strided picture.
  ///
  /// # Panics
  ///
  /// If `rows * row_bytes` overflows `usize`, or if `row(i)` answers a
  /// slice that is not `row_bytes` long. Callers reach this only after
  /// the geometry has been validated and the total checked against
  /// [`FrameLimits`](crate::FrameLimits), so both are unreachable from
  /// input.
  pub(crate) fn from_rows<'a>(
    rows: usize,
    row_bytes: usize,
    mut row: impl FnMut(usize) -> &'a [u8],
  ) -> Option<Self> {
    let len = rows.checked_mul(row_bytes)?;
    if len == 0 {
      return Some(Self::empty());
    }
    let mut uninit = Arc::<[u8]>::new_uninit_slice(len);
    {
      let slots =
        Arc::get_mut(&mut uninit).expect("the allocation was made here and has not been shared");
      for index in 0..rows {
        let source = row(index);
        if source.len() != row_bytes {
          // A length that arrives from a caller is an input, not a
          // promise: refuse rather than copy `row_bytes` out of a
          // shorter slice. The half-built `Arc` drops with this
          // return, and every byte of it is still `MaybeUninit`.
          return None;
        }
        let start = index * row_bytes;
        // `MaybeUninit<u8>` has the same layout as `u8`, so the source
        // slice can be viewed as one and copied wholesale.
        let destination = &mut slots[start..start + row_bytes];
        // SAFETY: `&[u8]` and `&[MaybeUninit<u8>]` have identical
        // layout, and the cast is read-only on the source side.
        let source: &[core::mem::MaybeUninit<u8>] = unsafe {
          core::slice::from_raw_parts(
            source.as_ptr().cast::<core::mem::MaybeUninit<u8>>(),
            row_bytes,
          )
        };
        destination.copy_from_slice(source);
      }
    }
    // SAFETY: the loop above wrote every one of the `rows * row_bytes`
    // slots — `rows` iterations, each filling exactly `row_bytes`
    // consecutive bytes starting at `index * row_bytes`, with the
    // length of each source row checked before the copy and the whole
    // gather abandoned if one disagreed. Nothing in the allocation is
    // left uninitialised on this road.
    Some(Self(Inner::Shared(unsafe { uninit.assume_init() })))
  }

  /// The bytes, as a slice.
  ///
  /// The same answer [`AsRef::as_ref`] gives; inherent so a caller
  /// reaching through a `&FfmpegBytes` does not have to name the trait.
  #[inline]
  pub fn as_slice(&self) -> &[u8] {
    match &self.0 {
      Inner::Shared(bytes) => bytes,
    }
  }

  /// Number of bytes carried.
  #[inline]
  pub fn len(&self) -> usize {
    self.as_slice().len()
  }

  /// `true` when this carries no bytes.
  #[inline]
  pub fn is_empty(&self) -> bool {
    self.as_slice().is_empty()
  }

  /// `true` when both handles name the same allocation — a clone of
  /// one another, rather than two copies that happen to be equal.
  ///
  /// The property the amputation contract is really about: `Clone` on
  /// a message is a refcount bump. `PartialEq` answers a different
  /// question (do these hold the same bytes), and a test that wants to
  /// prove the clone did not copy has to ask this one.
  #[inline]
  pub fn ptr_eq(&self, other: &Self) -> bool {
    match (&self.0, &other.0) {
      (Inner::Shared(a), Inner::Shared(b)) => Arc::ptr_eq(a, b),
    }
  }
}

impl AsRef<[u8]> for FfmpegBytes {
  #[inline]
  fn as_ref(&self) -> &[u8] {
    self.as_slice()
  }
}

impl fmt::Debug for FfmpegBytes {
  /// Length only, never the bytes.
  ///
  /// A derived `Debug` would print a decoded 4K plane one integer at a
  /// time; this type is reached from the derived `Debug` of every
  /// packet, frame and side-data entry in the crate, so the terse form
  /// is the one that keeps those useful. Mirrors what `FfmpegBuffer`'s
  /// own hand-written `Debug` did through 0.8.
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("FfmpegBytes")
      .field("len", &self.len())
      .finish()
  }
}

/// The process-wide empty `Arc`, so a zero-length carrier costs a
/// refcount bump rather than an allocation.
fn shared_empty() -> Arc<[u8]> {
  static EMPTY: OnceLock<Arc<[u8]>> = OnceLock::new();
  EMPTY.get_or_init(|| Arc::from(&[][..])).clone()
}

/// Payload for [`PacketBufferError::PacketTooLarge`].
///
/// A packet's payload is larger than the budget in force.
///
/// Refused **before** the copy: 0.8 answered a claimed payload with a
/// refcount, so an absurd `size` cost nothing; 0.9 answers it with an
/// allocation, so the claim has to be judged first.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a {bytes}-byte packet payload exceeds the {limit}-byte budget")]
pub struct PacketTooLarge {
  bytes: usize,
  limit: usize,
}

impl PacketTooLarge {
  /// Constructs a `PacketTooLarge` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(bytes: usize, limit: usize) -> Self {
    Self { bytes, limit }
  }
  /// The payload length the packet declared.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`PacketBufferError::Bounds`].
///
/// The payload does not lie inside the packet's own buffer.
/// `AVPacket` guarantees it does; a packet that says otherwise is
/// malformed, and wrapping it would hand out a view over memory the
/// buffer does not own.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a {len}-byte payload at offset {offset} does not lie inside a {size}-byte buffer")]
pub struct Bounds {
  offset: usize,
  len: usize,
  size: usize,
}

impl Bounds {
  /// Constructs a `Bounds` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(offset: usize, len: usize, size: usize) -> Self {
    Self { offset, len, size }
  }
  /// Where the payload starts inside the buffer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn offset(&self) -> usize {
    self.offset
  }
  /// The payload's length in bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn len(&self) -> usize {
    self.len
  }
  /// `true` when the payload is zero bytes long.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn is_empty(&self) -> bool {
    self.len == 0
  }
  /// The buffer's own length in bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn size(&self) -> usize {
    self.size
  }
}

/// Payload for [`PacketBufferError::SideDataEntries`].
///
/// A packet declares more side-data entries than this crate will
/// walk, or a negative count.
///
/// The cap bounds the work a crafted packet can demand *before* it is
/// refused. It cannot trip on anything FFmpeg's own packet API
/// produces: both `av_packet_new_side_data` and
/// `av_packet_add_side_data` replace an entry of the same type, so a
/// packet carries at most one entry per named type — forty-three in
/// this build, and the cap tracks that number if it ever grows past
/// the floor.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a packet declaring {count} side-data entries cannot be carried (limit {cap})")]
pub struct SideDataEntries {
  count: i32,
  cap: usize,
}

impl SideDataEntries {
  /// Constructs a `SideDataEntries` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(count: i32, cap: usize) -> Self {
    Self { count, cap }
  }
  /// The count the packet declared.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn count(&self) -> i32 {
    self.count
  }
  /// The most entries this crate will walk.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn cap(&self) -> usize {
    self.cap
  }
}

/// Payload for [`PacketBufferError::SideDataArray`].
///
/// A packet declares side-data entries and carries no array to read
/// them from.
///
/// Malformed, and named rather than read as "no side data": a null
/// array with a positive count is the same silent loss as a truncated
/// copy, reached through the pointer instead of the cap.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a packet declaring {count} side-data entries carries no array")]
pub struct SideDataArray {
  count: i32,
}

impl SideDataArray {
  /// Constructs a `SideDataArray` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(count: i32) -> Self {
    Self { count }
  }
  /// The count the packet declared.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn count(&self) -> i32 {
    self.count
  }
}

/// Payload for [`PacketBufferError::SideDataPayload`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("side-data entry {index} declares {size} bytes and carries no data")]
pub struct SideDataPayload {
  index: usize,
  size: usize,
}

impl SideDataPayload {
  /// Constructs a `SideDataPayload` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(index: usize, size: usize) -> Self {
    Self { index, size }
  }
  /// The entry's position in the packet's array.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn index(&self) -> usize {
    self.index
  }
  /// The length the entry declared.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn size(&self) -> usize {
    self.size
  }
}

/// Payload for [`PacketBufferError::SideDataBytes`].
///
/// A packet's side data is larger than this crate will copy.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{bytes} bytes of side data cannot be carried (limit {cap})")]
pub struct SideDataBytes {
  bytes: usize,
  cap: usize,
}

impl SideDataBytes {
  /// Constructs a `SideDataBytes` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(bytes: usize, cap: usize) -> Self {
    Self { bytes, cap }
  }
  /// The total the packet's entries reached.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The most bytes this crate will copy.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn cap(&self) -> usize {
    self.cap
  }
}

/// Payload for [`PacketBufferError::UnrepresentableFlags`].
///
/// A packet carries flag bits the portable vocabulary cannot hold.
///
/// `mediadecode`'s `PacketFlags` is a `u8` bit set, and every packet
/// flag FFmpeg names today lives in that byte — so this cannot fire
/// against this build. It exists so that the day one does not, the
/// packet is refused by name instead of arriving with a bit quietly
/// missing: the same rule the rest of this boundary keeps.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("packet flags {raw:#x} do not fit the portable flag set")]
pub struct UnrepresentableFlags {
  raw: i32,
}

impl UnrepresentableFlags {
  /// Constructs an `UnrepresentableFlags` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(raw: i32) -> Self {
    Self { raw }
  }
  /// `AVPacket.flags` as FFmpeg wrote it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn raw(&self) -> i32 {
    self.raw
  }
}

/// Payload for [`PacketBufferError::SideDataAlloc`].
///
/// Out of memory copying a side-data entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("out of memory copying {size} bytes of side data")]
pub struct SideDataAlloc {
  size: usize,
}

impl SideDataAlloc {
  /// Constructs a `SideDataAlloc` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(size: usize) -> Self {
    Self { size }
  }
  /// The entry's length in bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn size(&self) -> usize {
    self.size
  }
}
/// Why a packet could not be carried across the boundary — its payload,
/// or the side data that comes with it.
///
/// Every arm means the bytes are real and this crate could not carry
/// them — never that there were none. "No payload" is `Ok(None)` from
/// [`payload_of`], and keeping the two apart is the whole point of the
/// type: a demuxer that reads a malformed packet as an empty marker
/// drops a video packet and carries on as though the file said so. The
/// side-data arms exist for the same reason one tier along — a packet
/// whose side data cannot be carried whole is refused, never delivered
/// with some of it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
pub enum PacketBufferError {
  /// The payload is larger than the budget in force. Refused before
  /// the copy.
  #[error(transparent)]
  PacketTooLarge(#[from] PacketTooLarge),

  /// The payload does not lie inside the packet's own buffer.
  #[error(transparent)]
  Bounds(#[from] Bounds),

  /// A packet declares more side-data entries than this crate will
  /// walk, or a negative count.
  #[error(transparent)]
  SideDataEntries(#[from] SideDataEntries),

  /// A packet declares side-data entries and carries no array to read
  /// them from.
  #[error(transparent)]
  SideDataArray(#[from] SideDataArray),

  /// A side-data entry declares bytes it does not carry.
  #[error(transparent)]
  SideDataPayload(#[from] SideDataPayload),

  /// A packet's side data is larger than this crate will copy.
  #[error(transparent)]
  SideDataBytes(#[from] SideDataBytes),

  /// A packet carries flag bits the portable vocabulary cannot hold.
  #[error(transparent)]
  UnrepresentableFlags(#[from] UnrepresentableFlags),

  /// A packet is marked `AV_PKT_FLAG_TRUSTED`, so its payload may hold
  /// pointers rather than bytes. See [`TrustedPayload`].
  #[error(transparent)]
  TrustedPayload(#[from] TrustedPayload),

  /// The capture itself failed — an allocation on the owned lane, a
  /// refcount on the view lane. See [`CaptureFailed`].
  #[error(transparent)]
  CaptureFailed(#[from] CaptureFailed),

  /// The payload's buffer is referenced by something other than the
  /// packet it came from. See [`SharedPayload`].
  #[error(transparent)]
  SharedPayload(#[from] SharedPayload),

  /// Out of memory copying a side-data entry.
  #[error(transparent)]
  SideDataAlloc(#[from] SideDataAlloc),
}

impl PacketBufferError {
  /// Whether the demux session should **park** the packet this refusal
  /// came from and re-attempt it on the next pull.
  ///
  /// Deliberately not public, and deliberately named for the decision
  /// rather than for a property of the error. It was briefly public as
  /// `is_transient`, which promised more than an error enum can know:
  /// whether retrying helps depends on *what was retried*.
  /// `SharedPayload` is permanent for a caller who keeps their other
  /// reference and retryable the moment they drop it;
  /// `CaptureFailed` is worth another attempt only if the packet still
  /// exists to attempt, which on a **consuming** conversion it does
  /// not. Only the demux loop knows both halves — it still holds the
  /// packet, and it knows nobody else does.
  ///
  /// So this answers one question for one caller. An allocation that
  /// failed says nothing about the packet, and the demux loop is
  /// holding the bytes; everything else is a fact about the packet
  /// itself, and parking it would answer every later pull with the same
  /// error instead of letting the session make progress.
  ///
  /// **The door left open:** a public retry signal would have to know
  /// which operation produced the error and what the caller still
  /// holds — an operation-aware answer, not a property of this enum.
  /// If one is ever wanted it is designed then, not approximated now.
  #[inline]
  pub(crate) const fn parks_in_demux(&self) -> bool {
    matches!(self, Self::CaptureFailed(_) | Self::SideDataAlloc(_))
  }
}

/// `AV_PKT_FLAG_TRUSTED` as the bit the portable `PacketFlags` byte
/// carries it in.
///
/// The core vocabulary deliberately does not *name* this flag — it is
/// FFmpeg's, not a portable fact about packets — but `from_bits_retain`
/// keeps the bit, so this crate can recognise its own flag coming back
/// without the core growing a constant for it.
pub(crate) const TRUSTED_BIT: u8 = ffmpeg_next::ffi::AV_PKT_FLAG_TRUSTED as u8;

/// Compile-time proof that the flag really does fit the byte, so the
/// cast above cannot silently become a different bit.
const _: () = {
  assert!(
    ffmpeg_next::ffi::AV_PKT_FLAG_TRUSTED > 0
      && ffmpeg_next::ffi::AV_PKT_FLAG_TRUSTED <= u8::MAX as std::ffi::c_int,
    "AV_PKT_FLAG_TRUSTED no longer fits the portable flag byte",
  );
};

/// Payload for [`PacketBufferError::TrustedPayload`] and
/// [`crate::boundary::PacketBuildError::TrustedPayload`].
///
/// A packet carrying `AV_PKT_FLAG_TRUSTED`, refused on both legs.
///
/// # Why a flag makes a payload uncarriable
///
/// `AV_PKT_FLAG_TRUSTED` is FFmpeg's marker for a packet whose bytes
/// came from a source the *decoder* may treat as its own — and the
/// wrapped-AVFrame producers use it for exactly that: the payload is
/// not media, it is a **structure containing pointers to other live
/// objects** (an `AVFrame` and its buffers), passed by address between
/// components inside one FFmpeg pipeline.
///
/// This crate copies bytes. A pointer copied by value is not owned by
/// the copy — and that is not a gap this crate can close, because there
/// is no bound on what a payload's pointers might reach. So the
/// amputation has a corollary:
///
/// > **A payload that carries addresses instead of bytes cannot be
/// > carried.** Copying it produces a message that looks owned, is
/// > `Send + Sync + 'static` by every type-level test, and dangles the
/// > moment its source is dropped — a use-after-free reachable through
/// > entirely safe API.
///
/// Refusing is not conservatism, it is the only correct answer: the
/// contract this crate exists to keep says every byte leaving FFmpeg is
/// copied once into memory Rust owns, and a pointer cannot be.
///
/// Refused at **both** legs, because either one alone leaves the loop
/// open: copy-out ([`payload_of`]) is where such a packet would enter
/// the graph, and the reverse builders are where a flag that survived
/// some other route would be handed back to a decoder that trusts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedPayload {
  len: usize,
}

impl TrustedPayload {
  /// Constructs a `TrustedPayload` payload.
  #[inline]
  pub const fn new(len: usize) -> Self {
    Self { len }
  }
  /// How many bytes the packet declared.
  #[inline]
  pub const fn len(&self) -> usize {
    self.len
  }
  /// Whether the refused packet declared no bytes.
  #[inline]
  pub const fn is_empty(&self) -> bool {
    self.len == 0
  }
}

impl core::fmt::Display for TrustedPayload {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "packet of {} bytes carries AV_PKT_FLAG_TRUSTED; a payload that may hold \
       pointers to other objects cannot be copied into an owned carrier",
      self.len,
    )
  }
}

impl std::error::Error for TrustedPayload {}

/// The payload of a raw `AVPacket`, copied out.
///
/// Shared by the four timed boundary conversions, the attachment
/// conversion, and the demuxer's capture of `AVStream.attached_pic` —
/// an `AVPacket` embedded in the stream by value, which no safe
/// wrapper reaches. One implementation, so the empty-versus-malformed
/// distinction cannot drift between them.
///
/// `Ok(None)` means the packet carries no payload at all: an empty
/// marker, which some demuxers emit. That is a fact about the packet
/// and is kept apart from [`PacketBufferError`], which is a failure to
/// take a payload that *is* there.
///
/// A packet whose `buf` is null — a stack- or arena-allocated
/// `AVPacket` — still reads as "no payload", exactly as it did before
/// the amputation. It is tempting now that the bytes are copied to
/// serve those from `data` / `size` directly, and that is precisely the
/// case with no owning buffer to bound the read against: the claim
/// would have to be taken on faith.
///
/// # Safety
///
/// `pkt` must be a live `*const AVPacket` for the duration of this
/// call.
pub(crate) unsafe fn payload_of<C: crate::FfmpegCarrier + crate::CarrierOps>(
  pkt: *const ffmpeg_next::ffi::AVPacket,
  budget: usize,
  provenance: PayloadProvenance,
) -> Result<Option<C::Buffer>, PacketBufferError> {
  // SAFETY: `pkt` is live per the contract above; `.buf`, `.data` and
  // `.size` are public fields on `AVPacket`, and `buf` may be null
  // (stack-allocated packets).
  let buf_ptr = unsafe { (*pkt).buf };
  let data_ptr = unsafe { (*pkt).data };
  let size_raw = unsafe { (*pkt).size };
  // **The uncarriable-payload refusal, ahead of everything.** See
  // [`TrustedPayload`]: this flag marks a payload that may be a
  // structure of pointers into other live objects rather than media
  // bytes, and copying those bytes would mint an owned-looking carrier
  // full of addresses that dangle as soon as the source is dropped.
  //
  // Judged before the empty-payload answer as well as before the copy:
  // "there is nothing to take here" is the wrong reply to a packet this
  // crate must not take *anything* from.
  //
  // SAFETY: `pkt` is live per the contract; `flags` is a public `c_int`
  // field, read as the integer it is.
  let flags_raw = unsafe { (*pkt).flags };
  if flags_raw & ffmpeg_next::ffi::AV_PKT_FLAG_TRUSTED != 0 {
    return Err(PacketBufferError::TrustedPayload(TrustedPayload::new(
      size_raw.max(0) as usize,
    )));
  }
  if buf_ptr.is_null() || data_ptr.is_null() || size_raw <= 0 {
    return Ok(None);
  }
  let len = size_raw as usize;
  // **The budget, before anything is read or allocated.** Judged on
  // the declared length rather than on what the copy turns out to
  // cost, because the point is to refuse without paying. Ahead of the
  // bounds check too: a forged `size` is exactly what both exist for,
  // and the cheaper judgement goes first.
  if len > budget {
    return Err(PacketBufferError::PacketTooLarge(PacketTooLarge::new(
      len, budget,
    )));
  }
  // SAFETY: `buf_ptr` is a live `AVBufferRef` owned by the packet.
  let buf_data = unsafe { (*buf_ptr).data };
  let size = unsafe { (*buf_ptr).size };
  if buf_data.is_null() {
    return Err(PacketBufferError::Bounds(Bounds::new(0, len, size)));
  }
  // `AVPacket` guarantees `data` lies within
  // `buf->data .. buf->data + buf->size`. Checked before the copy, not
  // instead of it: 0.8 formed a view over the claimed range and a
  // malformed `size` handed out a slice nobody read; 0.9 reads every
  // byte of it, so an unchecked claim is an out-of-bounds read rather
  // than a latent one.
  let offset = (data_ptr as usize).wrapping_sub(buf_data as usize);
  match offset.checked_add(len) {
    Some(end) if end <= size => {}
    _ => {
      return Err(PacketBufferError::Bounds(Bounds::new(offset, len, size)));
    }
  }
  // **The sharing question, asked before any byte is read.**
  //
  // Everything below reads the payload — the view lane by handing out a
  // span over it, the owned lane by copying it — so a buffer somebody
  // else references has to be classified before it is touched. What
  // matters is not the count but **who** the other holder is, and only
  // the caller of this function knows that: see [`PayloadProvenance`]
  // for the dichotomy and [`PayloadProvenance::route`] for the table.
  //
  // Placed here rather than inside the capture so the ordering is a
  // property of this function rather than of two lane impls: nothing
  // between the bounds proof and this decision touches the payload.
  //
  // SAFETY: `buf_ptr` is a live `AVBufferRef` owned by the packet;
  // `av_buffer_get_ref_count` only reads its atomic.
  let references = unsafe { ffmpeg_next::ffi::av_buffer_get_ref_count(buf_ptr.cast_const()) };
  let route = provenance.route(references != 1);
  if route == CaptureRoute::Refuse {
    return Err(PacketBufferError::SharedPayload(SharedPayload::new(
      references,
    )));
  }

  // **The capture, and the only step the two lanes spell differently.**
  // Everything above — the `TRUSTED` refusal, the empty answer, the
  // budget, the extent proof — is shared, which is what keeps the view
  // lane from having to re-earn a single one of them.
  //
  // The **packet-payload** capture, not the general one: this range is
  // an `AVPacket`'s payload inside that packet's own buffer, which is
  // the one place libavformat's trailing-padding contract applies. The
  // view lane records that, and the send leg is the only thing that
  // reads it back — see `boundary::share_or_copy`.
  //
  // SAFETY: `offset + len` was just proved to lie inside `buf_ptr`'s
  // own `size`, and `buf_ptr` is a live `AVBufferRef` the packet owns.
  let carried = match route {
    CaptureRoute::Capture => unsafe { C::capture_packet_payload(buf_ptr, offset, len) },
    CaptureRoute::Copy => {
      // A demux-delivered packet whose buffer libavformat also holds.
      // Reading it here is race-free — every other reference is
      // C-owned, no `ffmpeg_next::Packet` wraps one, and this crate
      // holds the `AVFormatContext` exclusively for the duration of
      // the call — but the copy is what keeps that argument confined
      // to *this* call instead of to the carrier's whole life.
      //
      // SAFETY: the extent was proved above and `buf_data` is
      // non-null.
      let bytes = unsafe { core::slice::from_raw_parts(buf_data.add(offset).cast_const(), len) };
      C::from_bytes(bytes)
    }
    // Answered before the payload was touched.
    CaptureRoute::Refuse => unreachable!("a refusal returns above"),
  };
  carried
    .map(Some)
    .ok_or(PacketBufferError::CaptureFailed(CaptureFailed::new(len)))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::limits::DEFAULT_MAX_PACKET_BYTES;
  use ffmpeg_next::{Packet, packet::Ref};

  #[test]
  fn a_real_payload_is_copied_out_whole() {
    let packet = Packet::copy(&[1u8, 2, 3, 4]);
    // SAFETY: `packet` owns a live `AVPacket` for the call.
    let payload = unsafe {
      payload_of::<crate::Owned>(
        packet.as_ptr(),
        DEFAULT_MAX_PACKET_BYTES,
        PayloadProvenance::CallerSupplied,
      )
    }
    .expect("a well-formed packet is carriable")
    .expect("present");
    assert_eq!(payload.as_ref(), &[1, 2, 3, 4]);
  }

  #[test]
  fn the_copy_outlives_the_packet_it_came_from() {
    // The whole point of the amputation: FFmpeg's allocation is gone
    // and the bytes are still here.
    let packet = Packet::copy(&[9u8, 8, 7]);
    // SAFETY: `packet` owns a live `AVPacket` for the call.
    let payload = unsafe {
      payload_of::<crate::Owned>(
        packet.as_ptr(),
        DEFAULT_MAX_PACKET_BYTES,
        PayloadProvenance::CallerSupplied,
      )
    }
    .expect("carriable")
    .expect("present");
    let shared = payload.clone();
    assert!(shared.ptr_eq(&payload), "the clone copied the bytes");
    drop(packet);
    drop(payload);
    assert_eq!(shared.as_ref(), &[9, 8, 7]);
  }

  #[test]
  fn an_empty_packet_has_no_payload_rather_than_a_failure() {
    let packet = Packet::empty();
    // SAFETY: `packet` owns a live `AVPacket` for the call.
    assert!(
      unsafe {
        payload_of::<crate::Owned>(
          packet.as_ptr(),
          DEFAULT_MAX_PACKET_BYTES,
          PayloadProvenance::CallerSupplied,
        )
      }
      .expect("not a failure")
      .is_none()
    );
  }

  #[test]
  fn a_payload_outside_its_own_buffer_is_refused_before_a_byte_is_read() {
    use ffmpeg_next::packet::Mut;
    let mut packet = Packet::copy(&[1u8, 2, 3, 4]);
    // SAFETY: `packet` owns a live `AVPacket`; `size` is a public
    // field. The forged claim is the read this check exists to stop.
    unsafe {
      (*packet.as_mut_ptr()).size = 1 << 20;
    }
    // SAFETY: `packet` owns a live `AVPacket` for the call.
    assert!(matches!(
      unsafe {
        payload_of::<crate::Owned>(
          packet.as_ptr(),
          DEFAULT_MAX_PACKET_BYTES,
          PayloadProvenance::CallerSupplied,
        )
      },
      Err(PacketBufferError::Bounds(_)),
    ));
  }

  #[test]
  fn the_shared_empty_carrier_is_one_allocation() {
    let a = FfmpegBytes::empty();
    let b = FfmpegBytes::empty();
    assert!(a.is_empty());
    assert_eq!(a.len(), 0);
    assert!(a.ptr_eq(&b), "the empty carrier is shared, not remade");
    // And a zero-length copy lands on that same allocation rather than
    // minting its own.
    assert!(FfmpegBytes::copy_from_slice(&[]).ptr_eq(&a));
  }

  #[test]
  fn copy_out_is_owned_and_shareable() {
    fn owned_and_shareable<T: Send + Sync + Clone + 'static>(_: &T) {}
    let carrier = FfmpegBytes::copy_from_slice(&[4u8, 5, 6]);
    owned_and_shareable(&carrier);
    assert_eq!(carrier.as_ref(), &[4, 5, 6]);
    // Terse `Debug` — the bytes never reach a log line through it.
    let rendered = format!("{carrier:?}");
    assert!(rendered.contains("len: 3"), "got {rendered}");
    assert!(!rendered.contains('4'), "got {rendered}");
  }
}

/// Where the packet a payload is taken from came from — and therefore
/// what a second reference to its buffer can do.
///
/// The dichotomy is **delivered by libavformat** versus **handed over
/// by a caller**, and it is about who can *write*, not how many
/// references there are.
///
/// * A packet this crate's own read loop just took from
///   `av_read_frame` — or hoisted out of `AVStream.attached_pic` — may
///   well share its buffer, and every other reference to it is
///   libavformat's. FFmpeg writes through a buffer only after
///   `av_buffer_make_writable`, which *copies* when the buffer is
///   shared; and while this crate is reading, it holds the
///   `AVFormatContext` exclusively, so no libavformat code is running
///   at all. There is no safe-Rust `data_mut` on any of those
///   references, because no `ffmpeg_next::Packet` wraps them.
/// * A packet a **caller** hands over may share its buffer with
///   another `ffmpeg_next::Packet` — and that type's `data_mut` writes
///   in place from safe code, without consulting writability. That is
///   the writer the uniqueness rule exists for, and it may be on
///   another thread.
///
/// Spelled out as a parameter rather than assumed at the call sites,
/// because a blanket "refcount must be one" looked right and was twice
/// wrong: it refused every embedded cover picture, and then every
/// packet from a queue-backed subtitle demuxer, which delivers
/// `av_packet_ref`s of originals it keeps in its own queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PayloadProvenance {
  /// A packet a caller handed to a public conversion. A second
  /// reference may be a `Packet` with a safe `data_mut`; shared is
  /// refused by name.
  CallerSupplied,
  /// A packet this crate's demux loop just received from
  /// `av_read_frame`. Secondary references are libavformat's own.
  DemuxDelivered,
  /// The container's parked picture, whether hoisted at open or queued
  /// as a stream's first packet. Written once while the container was
  /// opened and never again.
  AttachedPicture,
}

/// How a payload of a given provenance may be captured once its extent
/// is proved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureRoute {
  /// The ordinary road: the view lane takes a window, the owned lane
  /// copies.
  Capture,
  /// Both lanes copy. The bytes are safe to *read* — no safe-Rust
  /// writer exists — but no long-lived window may be opened onto a
  /// buffer somebody else also holds unless something stronger than
  /// "nobody is writing right now" is true of it.
  Copy,
  /// Refuse without reading a byte.
  Refuse,
}

impl PayloadProvenance {
  /// What to do with a payload whose buffer has `shared` other
  /// references.
  ///
  /// | provenance | unique | shared | the argument |
  /// |---|---|---|---|
  /// | [`Self::CallerSupplied`] | capture | **refuse** | a second `Packet`'s `data_mut` writes in place from safe code, possibly on another thread |
  /// | [`Self::DemuxDelivered`] | capture | **copy** | the read is race-free — every other reference is C-owned and the context is held exclusively — but a *window* would outlive that exclusivity, and FFmpeg's copy-on-write discipline is a weaker guarantee than this crate wants under a long-lived span |
  /// | [`Self::AttachedPicture`] | capture | **capture** | `AVStream.attached_pic` is written once while the container opens and never again, so a window onto it is as stable as one onto a private buffer |
  ///
  /// The middle row is the deliberate one. Sharing there would have
  /// rested on "libavformat honours its own writability rules
  /// forever"; copying rests on "nothing can be writing while we hold
  /// the context", which is a fact about *this* call and needs no
  /// promise about anyone's future behaviour. Subtitle queues — the
  /// shape that produced this row — carry payloads measured in bytes,
  /// so the copy is not a cost worth an argument.
  #[inline]
  pub(crate) const fn route(self, shared: bool) -> CaptureRoute {
    match (self, shared) {
      (_, false) | (Self::AttachedPicture, true) => CaptureRoute::Capture,
      (Self::DemuxDelivered, true) => CaptureRoute::Copy,
      (Self::CallerSupplied, true) => CaptureRoute::Refuse,
    }
  }
}

/// A packet whose payload buffer somebody else still references.
///
/// **Refused without reading a byte of it, and that is the whole
/// point.** A refcount above one is exactly the state in which another
/// handle to the same allocation may exist — `ffmpeg_next::Packet`
/// hands out `&mut [u8]` through `data_mut` from entirely safe code,
/// and a `Packet` is `Send`, so that handle may be on another thread
/// writing right now. Forming a `&[u8]` over those bytes is a data race
/// whether the bytes are then viewed *or copied*: the copy needs the
/// read, and the read is the race.
///
/// An earlier round answered this shape with a silent copy, reasoning
/// that a copy is always sound and keeps the API total. That was wrong
/// in the direction that matters — it traded soundness for totality.
/// The refcount protects the allocation's *lifetime*; it says nothing
/// about who may be writing into it.
///
/// The ordinary roads never see this: a packet from `av_read_frame` is
/// uniquely referenced, and so is one the caller cloned successfully.
/// What produces it is a second reference the caller may not know they
/// have — see `ffmpeg_next::Packet::clone`, which ignores
/// `av_packet_make_writable`'s return code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
  "packet payload buffer is shared ({references} references): its bytes cannot be read \
   without racing whoever else holds it"
)]
pub struct SharedPayload {
  references: i32,
}

impl SharedPayload {
  /// Constructs a `SharedPayload` payload.
  #[inline]
  #[must_use]
  pub const fn new(references: i32) -> Self {
    Self { references }
  }

  /// References the payload's buffer had when it was refused.
  #[inline]
  #[must_use]
  pub const fn references(&self) -> i32 {
    self.references
  }
}

/// Payload for [`PacketBufferError::CaptureFailed`].
///
/// The proofs all passed and the carrier still could not be formed:
/// `av_buffer_alloc` returned null on the owned lane, or
/// `av_buffer_ref` did on the view lane. Distinct from
/// [`Bounds`] on purpose — a malformed packet and an exhausted
/// allocator are different facts, and 0.8 reported both as an absent
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("could not capture a {len}-byte payload: the allocator or the refcount refused")]
pub struct CaptureFailed {
  len: usize,
}

impl CaptureFailed {
  /// Constructs a `CaptureFailed` payload.
  #[inline]
  pub const fn new(len: usize) -> Self {
    Self { len }
  }
  /// Bytes the capture was for.
  #[inline]
  pub const fn len(&self) -> usize {
    self.len
  }
  /// Whether the refused capture was of no bytes.
  #[inline]
  pub const fn is_empty(&self) -> bool {
    self.len == 0
  }
}

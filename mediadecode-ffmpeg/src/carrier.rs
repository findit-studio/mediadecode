//! The carrier-strategy seam: which lane a demuxer or decoder captures
//! into.
//!
//! This crate ships **two first-class carriers**, and neither is a
//! feature flag on the other:
//!
//! * [`Owned`] captures into [`FfmpegBytes`](crate::FfmpegBytes) — every
//!   byte copied once at the boundary into memory Rust owns. `Send +
//!   Sync`, lifetime answerable to nobody, safe to fan out across a
//!   graph. This is the default, and the lane the
//!   [amputation contract][law] governs.
//! * [`View`] captures into [`FfmpegBuffer`](crate::FfmpegBuffer) — a
//!   refcounted view onto the allocation FFmpeg already made. `Send`
//!   and **not** `Sync`, lifetime pinned to the backend's pools,
//!   zero-copy.
//!
//! # Why a sealed seam and not an open trait
//!
//! A carrier strategy is not an extension point. Implementing one
//! correctly means knowing what an `AVBufferRef` guarantees, which
//! plane extents are initialised, and which of this crate's proofs run
//! before a capture is allowed to happen — knowledge that lives in this
//! crate and cannot be documented into a third party. A third strategy
//! would also have to answer the questions this seam does not ask,
//! because only two lanes exist to ask them of.
//!
//! So the trait is **sealed**: public to name and to bound on, closed
//! to implement. If a third lane is ever wanted, it is added here,
//! where it can be held to the same proofs — the seam being closed is
//! what makes that a change to one file rather than a change to a
//! contract.
//!
//! # Why the operations are not public either
//!
//! Sealing stopped outsiders *implementing* the seam. It did not stop
//! them *calling* it, and for a while that was a hole with teeth: the
//! operations take offsets, lengths and row geometry which the unsafe
//! layer beneath them acts on, so their invariants live in the caller.
//! `View::from_rows(1, 64, |_| &[0])` was safe code, compiled, and
//! copied sixty-four bytes out of a one-byte slice.
//!
//! Two answers, applied together:
//!
//! * every operation moved onto the private supertrait in
//!   [`sealed`], which no path outside this crate can name and
//!   therefore no call outside this crate can reach. What stays public
//!   is the *name* of a lane and the ability to bound on it — which is
//!   all a consumer ever needed;
//! * and where a length still arrives from a caller, it is **checked**
//!   rather than asserted. A `debug_assert` is a note to the author; it
//!   is not a bounds check, and it is not there at all in the profile
//!   that matters.
//!
//! The rule the two share: an invariant that lives in the caller is an
//! invariant the caller must be inside this crate to be trusted with.
//!
//! # Where the bound sits
//!
//! At the narrowest site each one can be written at, and no wider:
//!
//! * **Structural** on the types whose *fields* name `C::Buffer` —
//!   [`CarrierDemuxer`](crate::demuxer::CarrierDemuxer) holds built
//!   tracks, [`CarrierResampler`](crate::resampler::CarrierResampler)
//!   holds a queue of frames. A struct whose field type is a projection
//!   cannot be well-formed without the bound that gives the projection
//!   meaning, so writing it there is not a choice.
//! * **Behavioural** on the decoders, which carry `C` only as a
//!   `PhantomData` marker: their struct declarations take a bare `C`
//!   and the bound sits on the impls whose methods capture.
//! * **Absent** from `*Extra`, `SideDataEntry` and every side-data
//!   collector. Side data has no `AVBufferRef` to share, so both lanes
//!   copy it and the parameter never reaches those types at all.
//!
//! The lanes are then named by alias rather than by a defaulted type
//! parameter, because a default is used when a type is *written* and
//! never inferred from a call — `Demuxer::open(path)` would have had no
//! default to fall back on.
//!
//! [law]: mediadecode::adapter#the-d-seat-amputation-contract

use ffmpeg_next::ffi::AVBufferRef;

use crate::{FfmpegBytes, view::FfmpegBuffer};

pub(crate) use ops::{BodyRoute, CarrierOps};

pub(crate) mod sealed {
  /// Closes [`super::FfmpegCarrier`] to outside implementations, and
  /// **carries nothing**.
  ///
  /// An earlier round put the operations here, reasoning that a trait
  /// no outside path can name is a trait no outside call can reach.
  /// That was wrong in a way worth recording: associated items resolve
  /// through a *bound*, not through a path, so downstream code written
  /// as `fn f<C: FfmpegCarrier>()` type-checked `C::capture(..)` and
  /// `C::from_rows(..)` perfectly well — the seal stopped
  /// implementations and nothing else. The operations now live on
  /// [`CarrierOps`](super::CarrierOps), which is **not** a supertrait
  /// of anything public, so no bound a downstream crate can write
  /// reaches them.
  pub trait Sealed {}
}

pub(crate) mod ops {
  use ffmpeg_next::ffi::AVBufferRef;

  /// Which body a rebuilt `AVPacket` gets.
  ///
  /// The distinction exists because the two roads have different
  /// exposure. A packet a caller is *handed* is a public
  /// `ffmpeg_next::Packet`, and that type can lend `&mut [u8]` — so its
  /// body must be storage nobody else can read, which means a copy on
  /// either lane. A packet built to be **submitted and dropped** inside
  /// one crate-private call never escapes to anywhere a `&mut` can be
  /// taken from it, so the view lane may share there.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum BodyRoute {
    /// The body is copied into storage the packet alone owns.
    Copy,
    /// The body may share the carrier's buffer, if this lane can prove
    /// that is safe for a decoder to read. Only ever asked for from a
    /// scoped submission.
    Submission,
  }

  /// Every operation a lane performs — and the reason none of them is
  /// reachable from outside this crate.
  ///
  /// This trait is deliberately **not** a supertrait of
  /// [`FfmpegCarrier`](super::FfmpegCarrier). A supertrait would put
  /// its items in scope for any bound that names the subtrait, and
  /// `fn f<C: FfmpegCarrier>()` written in a downstream crate would
  /// then type-check `C::from_rows(1, 64, |_| &[0])` — safe code, an
  /// out-of-bounds read, in somebody else's crate. Sealing prevents
  /// implementing; only an unreachable *bound* prevents calling.
  ///
  /// So the two are joined nowhere: `FfmpegCarrier` names a lane and
  /// says what it carries, this says what it can do, and the functions
  /// in this crate that need the second ask for it by a name no
  /// downstream crate can write.
  pub trait CarrierOps: super::FfmpegCarrier {
    /// A carrier over no bytes.
    ///
    /// Allocation-free on both lanes; it is what an unpopulated plane
    /// slot holds, and eight per frame is not a place to put a
    /// failure mode.
    fn empty() -> Self::Buffer;

    /// A carrier over bytes that live outside any `AVBufferRef`.
    ///
    /// Subtitle rect text and a rect's own palette are plain
    /// allocations with no refcount to share, so **both** lanes copy
    /// here. The view lane says so rather than implying its whole road
    /// is zero-copy.
    ///
    /// `None` when the copy cannot be allocated. Fallible on both lanes
    /// so that neither can answer an allocation failure with an empty
    /// carrier — a plane that silently became zero bytes is a frame
    /// whose header is a lie.
    fn from_bytes(bytes: &[u8]) -> Option<Self::Buffer>;

    /// A carrier over `len` bytes at `offset` inside the live
    /// `AVBufferRef` `buf`.
    ///
    /// The owned lane copies them out; the view lane takes a reference.
    /// Either way the caller has already proved the extent lies inside
    /// the buffer — this is the capture, not the judgement.
    ///
    /// Nothing may be assumed about the bytes **after** the captured
    /// range: this is the road frame planes and palettes take. See
    /// [`Self::capture_packet_payload`] for the one that may.
    ///
    /// `None` when the capture itself fails: an allocation on the owned
    /// lane, a refcount on the view lane.
    ///
    /// # Safety
    ///
    /// `buf` must be a live `AVBufferRef`, and `offset + len` must be
    /// within its `size`.
    unsafe fn capture(buf: *mut AVBufferRef, offset: usize, len: usize) -> Option<Self::Buffer>;

    /// [`Self::capture`] for a payload taken out of an `AVPacket`'s own
    /// buffer.
    ///
    /// The only difference is what the carrier records about itself:
    /// libavformat allocates `AV_INPUT_BUFFER_PADDING_SIZE` zeroed
    /// bytes behind every packet it produces, and this is the one
    /// capture that may claim them. That claim is what the send leg
    /// checks before it shares anything with a decoder.
    ///
    /// # Safety
    ///
    /// As [`Self::capture`], and `buf` must be the buffer of an
    /// `AVPacket` whose payload this range is.
    unsafe fn capture_packet_payload(
      buf: *mut AVBufferRef,
      offset: usize,
      len: usize,
    ) -> Option<Self::Buffer>;

    /// A carrier over `rows` runs of `row_bytes`, gathered from a
    /// **padded** plane.
    ///
    /// Copied on **both** lanes, and this is the one place the view
    /// lane cannot share on principle rather than on plumbing. A padded
    /// plane is `linesize` wide and only its first `row_bytes` per row
    /// are the decoder's output; the rest is allocator scratch that
    /// nothing wrote. A carrier is an `AsRef<[u8]>`, so sharing the
    /// padded span would form a slice over uninitialised memory —
    /// undefined before a consumer reads a byte of it, and the same
    /// information leak the owned lane refused when it stopped
    /// exporting `linesize`.
    ///
    /// So a padded plane is compacted, on either lane, and arrives with
    /// `row_bytes` as its stride. `None` when the gather cannot be
    /// allocated, or when a row is not exactly `row_bytes` wide.
    fn from_rows<'a>(
      rows: usize,
      row_bytes: usize,
      row: impl FnMut(usize) -> &'a [u8],
    ) -> Option<Self::Buffer>;

    /// A capture claimed **before** the bytes exist, whose length is
    /// settled after they do. See [`Self::reserve`].
    type Reserved;

    /// Claims `cap` bytes at `offset` inside `buf`, without reading
    /// them.
    ///
    /// For a producer that allocates its own output frame and then
    /// hands it to FFmpeg to fill — the resampler — this is what keeps
    /// every fallible step on the *near* side of the conversion. `swr`
    /// consumes its input as it runs, so a failure after it has run
    /// leaves a session no caller can retry; reserving first and
    /// committing after means the only thing left to do once the bytes
    /// are there is name how many of them are real.
    ///
    /// The view lane takes its reference here — the refcount is what
    /// can fail, and it fails before anything is consumed. The owned
    /// lane reserves nothing and copies at [`Self::commit`], because
    /// copying uninitialised capacity is exactly the read this crate
    /// refuses everywhere else.
    ///
    /// # Safety
    ///
    /// `buf` must be a live `AVBufferRef`, `offset + cap` must be
    /// within its `size`, and the buffer must stay alive until the
    /// matching [`Self::commit`].
    unsafe fn reserve(buf: *mut AVBufferRef, offset: usize, cap: usize) -> Option<Self::Reserved>;

    /// Settles a [`Self::reserve`] at its true length. Infallible.
    ///
    /// # Safety
    ///
    /// `len` must be at most the `cap` the reservation was taken with,
    /// those `len` bytes must now be initialised, and the buffer must
    /// still be alive.
    unsafe fn commit(reserved: Self::Reserved, len: usize) -> Self::Buffer;

    /// Builds the body of an `AVPacket` on the way **into** a decoder.
    ///
    /// [`BodyRoute::Copy`] always copies, on either lane. Only
    /// [`BodyRoute::Submission`] lets the view lane share, and only
    /// where it can prove a decoder may read past the payload — see
    /// `boundary::share_or_copy`.
    ///
    /// This is why the reverse builders are one family rather than
    /// two: every judgement they make — the `TRUSTED` refusal, the
    /// side-data caps, the send budget — is about sizes and shapes,
    /// which are the same on both lanes. Only the body differs, and
    /// only here.
    fn packet_body(
      body: &Self::Buffer,
      route: BodyRoute,
    ) -> std::result::Result<ffmpeg_next::Packet, ffmpeg_next::Error>;
  }
}

/// How a lane turns FFmpeg's bytes into a carrier.
///
/// **Sealed, and almost empty by design.** Naming a lane, bounding on
/// one, and asking what it carries are public; performing a capture is
/// not. Every operation lives on a private trait that is *not* a
/// supertrait of this one — see the [module docs](self) for why that
/// distinction is the whole of the wall.
///
/// What a downstream crate can do — name a lane, hold the types
/// parameterized by one, be generic over it, and drive either:
///
/// ```
/// use mediadecode_ffmpeg::{FfmpegBuffer, FfmpegBytes, FfmpegCarrier, Owned, View};
///
/// // Name what a lane carries, and be generic over the lane.
/// fn carried<C: FfmpegCarrier>(buffer: &C::Buffer) -> usize {
///   buffer.as_ref().len()
/// }
/// let _: fn(&FfmpegBytes) -> usize = carried::<Owned>;
/// let _: fn(&FfmpegBuffer) -> usize = carried::<View>;
/// ```
///
/// Holding a lane-parameterized type generically — the public structs
/// carry **only** this bound, so a consumer's own generic code can pass
/// them around:
///
/// ```
/// use mediadecode::demuxer::TrackInfo;
/// use mediadecode_ffmpeg::{
///   CarrierAudioStreamDecoder, CarrierDemuxer, CarrierVideoStreamDecoder, Ffmpeg,
///   FfmpegCarrier, Owned, View,
/// };
///
/// fn tracks_of<C: FfmpegCarrier>(demuxer: &CarrierDemuxer<C>) -> usize {
///   // A field read is not an operation on the lane, so this is
///   // exactly as generic as it looks.
///   core::mem::size_of_val(demuxer)
/// }
///
/// fn hold<C: FfmpegCarrier>(
///   _demuxer: &CarrierDemuxer<C>,
///   _audio: &CarrierAudioStreamDecoder<C>,
///   _video: &CarrierVideoStreamDecoder<C>,
/// ) {
/// }
///
/// let _: fn(&CarrierDemuxer<View>) -> usize = tracks_of::<View>;
/// let _: fn(&CarrierDemuxer<Owned>) -> usize = tracks_of::<Owned>;
/// let _ = hold::<View>;
/// let _ = hold::<Owned>;
/// let _: fn() -> Vec<TrackInfo<Ffmpeg>> = || Vec::new();
/// ```
///
/// And calling at either concrete lane, through the aliases or through
/// the `Carrier*` names directly:
///
/// ```no_run
/// use mediadecode::demuxer::Demuxer;
/// use mediadecode_ffmpeg::{
///   CarrierDemuxer, FfmpegDemuxer, FfmpegOwnedDemuxer, Owned, View,
/// };
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut viewed = FfmpegDemuxer::open("clip.mkv")?;
/// let mut owned = CarrierDemuxer::<Owned>::open("clip.mkv")?;
/// let _ = viewed.tracks().len();
/// let _ = owned.next_packet()?;
/// let _: fn(&std::path::Path) -> _ = CarrierDemuxer::<View>::open::<std::path::Path>;
/// let _: fn(&std::path::Path) -> _ = FfmpegOwnedDemuxer::open::<std::path::Path>;
/// # Ok(())
/// # }
/// ```
///
/// What no bound reaches is the **operations** — see below. Being
/// generic over the lane and *driving* it are different asks: the
/// second needs the operations, so a consumer writes lane-generic
/// helpers over `C::Buffer` and instantiates the doors at the two
/// concrete lanes. That is the trade the wall costs, and it is
/// deliberate.
///
/// What it cannot, and must not be able to: every one of these takes an
/// extent, a geometry or a provenance claim that only this crate is in
/// a position to establish. `from_rows` is the sharpest — it is
/// **safe**, and a caller who could reach it could ask for sixty-four
/// bytes out of a one-byte row.
///
/// ```compile_fail,E0599
/// use mediadecode_ffmpeg::FfmpegCarrier;
/// fn downstream<C: FfmpegCarrier>() -> C::Buffer {
///   C::empty()
/// }
/// ```
///
/// ```compile_fail,E0599
/// use mediadecode_ffmpeg::FfmpegCarrier;
/// fn downstream<C: FfmpegCarrier>(row: &[u8]) -> Option<C::Buffer> {
///   C::from_rows(1, 64, |_| row)
/// }
/// ```
///
/// ```compile_fail,E0599
/// use mediadecode_ffmpeg::FfmpegCarrier;
/// unsafe fn downstream<C: FfmpegCarrier>(
///   buf: *mut ffmpeg_next::ffi::AVBufferRef,
/// ) -> Option<C::Buffer> {
///   unsafe { C::capture(buf, 0, 64) }
/// }
/// ```
///
/// ```compile_fail,E0599
/// use mediadecode_ffmpeg::FfmpegCarrier;
/// unsafe fn downstream<C: FfmpegCarrier>(
///   buf: *mut ffmpeg_next::ffi::AVBufferRef,
/// ) -> Option<C::Buffer> {
///   // The provenance claim: minting this downstream would let a frame
///   // plane pass itself off as a padded packet payload.
///   unsafe { C::capture_packet_payload(buf, 0, 64) }
/// }
/// ```
///
/// ```compile_fail,E0599
/// use mediadecode_ffmpeg::FfmpegCarrier;
/// unsafe fn downstream<C: FfmpegCarrier>(buf: *mut ffmpeg_next::ffi::AVBufferRef) {
///   let reserved = unsafe { C::reserve(buf, 0, 64) };
/// }
/// ```
///
/// ```compile_fail,E0599
/// use mediadecode_ffmpeg::FfmpegCarrier;
/// unsafe fn downstream<C: FfmpegCarrier>(reserved: ()) -> C::Buffer {
///   unsafe { C::commit(reserved, 64) }
/// }
/// ```
///
/// The generic bodies behind the per-lane faces are equally out of
/// reach. They carry the operations' bound, so reaching one at a
/// concrete lane would be a way around the wall that never names it:
///
/// ```compile_fail,E0624
/// use mediadecode_ffmpeg::{CarrierAudioStreamDecoder, DecoderLimits, Owned};
/// fn downstream(parameters: ffmpeg_next::codec::Parameters) {
///   let _ = CarrierAudioStreamDecoder::<Owned>::open_impl(
///     parameters,
///     mediadecode::Timebase::default(),
///     DecoderLimits::default(),
///   );
/// }
/// ```
///
/// ```compile_fail,E0624
/// use mediadecode_ffmpeg::{CarrierDemuxer, View};
/// fn downstream() {
///   let _ = CarrierDemuxer::<View>::open_impl("clip.mkv");
/// }
/// ```
pub trait FfmpegCarrier: sealed::Sealed + Copy + Clone + core::fmt::Debug + 'static {
  /// The carrier this lane produces.
  ///
  /// `Send` but not necessarily `Sync`: the view lane is `Send`-only,
  /// and requiring `Sync` here would have closed the seam to it.
  ///
  /// A **type**, and the only thing on this trait — naming
  /// `<View as FfmpegCarrier>::Buffer` tells a consumer what a lane
  /// hands them, and tells them nothing they could misuse. Everything
  /// that *acts* is on the private ops trait.
  type Buffer: AsRef<[u8]> + Clone + Send + 'static;
}

/// The **owned** lane: every byte copied once at the boundary.
///
/// The default carrier, and the one the [amputation contract][law]
/// governs. Frames and packets on this lane are `Send + Sync +
/// 'static`, clone by refcount, and owe nothing to the decoder that
/// produced them.
///
/// [law]: mediadecode::adapter#the-d-seat-amputation-contract
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Owned;

/// The **view** lane: refcounted zero-copy handles onto FFmpeg's own
/// allocations.
///
/// `Send` and not `Sync`, with a lifetime pinned to the backend's
/// buffer pools — a frame held is a pool slot held. See
/// [the carrier lanes][lanes] for the tradeoff table and for why graph
/// traffic belongs on [`Owned`].
///
/// [lanes]: mediadecode::adapter#the-two-carrier-lanes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct View;

impl sealed::Sealed for Owned {}
impl sealed::Sealed for View {}

impl FfmpegCarrier for Owned {
  type Buffer = FfmpegBytes;
}

impl FfmpegCarrier for View {
  type Buffer = FfmpegBuffer;
}

impl ops::CarrierOps for Owned {
  fn empty() -> Self::Buffer {
    FfmpegBytes::empty()
  }

  fn from_bytes(bytes: &[u8]) -> Option<Self::Buffer> {
    Some(FfmpegBytes::copy_from_slice(bytes))
  }

  fn from_rows<'a>(
    rows: usize,
    row_bytes: usize,
    row: impl FnMut(usize) -> &'a [u8],
  ) -> Option<Self::Buffer> {
    FfmpegBytes::from_rows(rows, row_bytes, row)
  }

  /// The plane's start. Nothing is claimed and nothing can fail:
  /// this lane's cost is a copy, and the copy happens at `commit` once
  /// the bytes are real.
  type Reserved = *const u8;

  unsafe fn reserve(buf: *mut AVBufferRef, offset: usize, _cap: usize) -> Option<Self::Reserved> {
    // SAFETY: `buf` is live per the contract and `offset` is inside it.
    let data = unsafe { (*buf).data };
    if data.is_null() {
      return None;
    }
    // SAFETY: `offset` is within the buffer per the contract.
    Some(unsafe { data.add(offset).cast_const() })
  }

  unsafe fn commit(reserved: Self::Reserved, len: usize) -> Self::Buffer {
    if len == 0 {
      return FfmpegBytes::empty();
    }
    // SAFETY: the caller promises `len` initialised bytes from the
    // reserved pointer, inside a buffer still alive.
    FfmpegBytes::copy_from_slice(unsafe { core::slice::from_raw_parts(reserved, len) })
  }

  /// Both routes copy. This lane's carrier is Rust-owned memory with no
  /// `AVBufferRef` behind it to hand back, so there is nothing to share
  /// on either road and no distinction to draw.
  fn packet_body(
    body: &Self::Buffer,
    _route: BodyRoute,
  ) -> std::result::Result<ffmpeg_next::Packet, ffmpeg_next::Error> {
    crate::boundary::try_packet_copy(body.as_ref())
  }

  unsafe fn capture(buf: *mut AVBufferRef, offset: usize, len: usize) -> Option<Self::Buffer> {
    if len == 0 {
      return Some(FfmpegBytes::empty());
    }
    // SAFETY: the caller proved `offset + len <= (*buf).size` against a
    // live buffer, so the range is inside an allocation FFmpeg holds.
    let bytes = unsafe {
      let data = (*buf).data;
      if data.is_null() {
        return None;
      }
      core::slice::from_raw_parts(data.add(offset).cast_const(), len)
    };
    Some(FfmpegBytes::copy_from_slice(bytes))
  }

  /// Indistinguishable from [`capture`](Self::capture) here: this lane
  /// copies the payload out, so what follows it in FFmpeg's allocation
  /// is not a fact about the carrier.
  unsafe fn capture_packet_payload(
    buf: *mut AVBufferRef,
    offset: usize,
    len: usize,
  ) -> Option<Self::Buffer> {
    // SAFETY: the caller's contract is the one `capture` states.
    unsafe { <Self as ops::CarrierOps>::capture(buf, offset, len) }
  }
}

impl ops::CarrierOps for View {
  fn empty() -> Self::Buffer {
    FfmpegBuffer::empty()
  }

  fn from_bytes(bytes: &[u8]) -> Option<Self::Buffer> {
    // No `AVBufferRef` to share, so this lane copies too — into one of
    // its own, so the carrier type stays uniform.
    FfmpegBuffer::copy_from_slice(bytes)
  }

  fn from_rows<'a>(
    rows: usize,
    row_bytes: usize,
    row: impl FnMut(usize) -> &'a [u8],
  ) -> Option<Self::Buffer> {
    FfmpegBuffer::from_rows(rows, row_bytes, row)
  }

  /// The view itself, taken at full capacity. The refcount — the only
  /// step that can fail — is therefore already paid when the bytes
  /// arrive.
  type Reserved = FfmpegBuffer;

  unsafe fn reserve(buf: *mut AVBufferRef, offset: usize, cap: usize) -> Option<Self::Reserved> {
    // SAFETY: the caller proved the extent; `view_of` proves it again.
    // A reservation is over an output frame this crate allocated, not
    // over a packet, so it carries no padding claim.
    unsafe { FfmpegBuffer::view_of(buf, offset, cap, crate::view::Origin::Foreign) }
  }

  unsafe fn commit(mut reserved: Self::Reserved, len: usize) -> Self::Buffer {
    // No bytes move: the reference is held already and this only names
    // how much of it is real output. The capacity past `len` is
    // untouched allocator memory, and narrowing here is what keeps it
    // out of every span this carrier hands out.
    reserved.shrink_to(len);
    reserved
  }

  fn packet_body(
    body: &Self::Buffer,
    route: BodyRoute,
  ) -> std::result::Result<ffmpeg_next::Packet, ffmpeg_next::Error> {
    match route {
      // **A packet handed to a caller never shares.** `ffmpeg_next::
      // Packet` lends `&mut [u8]` through `data_mut`, and the carrier
      // it was built from still lends `&[u8]` — two live references to
      // one allocation, one of them mutable, from entirely safe code.
      // Copying here is what makes that unconstructible.
      BodyRoute::Copy => crate::boundary::try_packet_copy(body.as_ref()),
      BodyRoute::Submission => crate::boundary::share_or_copy(body),
    }
  }

  unsafe fn capture(buf: *mut AVBufferRef, offset: usize, len: usize) -> Option<Self::Buffer> {
    // SAFETY: the caller proved the extent; `view_of` proves it again
    // against the buffer's own `size`, because a constructor that
    // trusts its arguments is one bad caller away from a view over
    // somebody else's memory.
    unsafe { FfmpegBuffer::view_of(buf, offset, len, crate::view::Origin::Foreign) }
  }

  unsafe fn capture_packet_payload(
    buf: *mut AVBufferRef,
    offset: usize,
    len: usize,
  ) -> Option<Self::Buffer> {
    // SAFETY: as `capture`, with the caller additionally promising this
    // range is an `AVPacket`'s payload — which is what entitles the
    // carrier to claim the padding behind it. The caller has also
    // already refused any buffer with more than one reference
    // (`buffer::payload_of`), so nothing else can be writing here.
    unsafe { FfmpegBuffer::view_of(buf, offset, len, crate::view::Origin::PacketPayload) }
  }
}

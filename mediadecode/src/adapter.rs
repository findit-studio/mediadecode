//! Adapter traits — the per-kind backend "vocabulary."
//!
//! A backend implements only the kinds it handles. R3D / BRAW /
//! ARRIRAW / X-OCN / Canon RAW Light implement only [`VideoAdapter`].
//! FFmpeg implements all four. The buffer type is **not** part of
//! these traits — it's a struct generic on `Packet` / `Frame` so the
//! same adapter can be used with different buffer types at different
//! call sites.
//!
//! # The D-seat amputation contract
//!
//! Every packet and frame in this crate carries a `D` — the buffer
//! type its bytes live in — and every backend chooses what to put
//! there. This is the one law that choice has to obey:
//!
//! > **A backend's `D`-seat carrier must be owned, `Send + Sync`, and
//! > cheap to clone (a refcount bump), with its lifetime fully
//! > decoupled from the backend's internal buffers at the exit. No
//! > backend-internal lifetime — an FFI pointer, a pooled buffer, a
//! > JavaScript handle — crosses the seam.**
//!
//! The bytes are copied **once**, at the boundary, and what leaves is
//! Rust-owned memory. A backend that hands out a view into its own
//! allocation instead has not exported a frame; it has exported a
//! borrow with the lifetime erased, and every consumer downstream
//! inherits a rule it cannot see: hold this only as long as the
//! decoder lives, do not send it to another thread, do not put it in
//! a cache. A graph made of such frames cannot fan a message out to
//! two consumers without the backend's permission.
//!
//! What the contract buys, in the order it matters:
//!
//! - **`Send + Sync`.** A frame can cross a channel and be *read* from
//!   several threads at once. Refcounted-view carriers are routinely
//!   `Send` and not `Sync`, which is exactly the shape that makes
//!   fan-out impossible.
//! - **A lifetime that is nobody's business.** The decoder can be
//!   dropped, flushed, seeked, or reopened while a frame it produced is
//!   still in flight.
//! - **Clone is a refcount bump.** The message-carrier law (see
//!   [`TrackInfo`](crate::demuxer::TrackInfo)) says `Clone` on a message
//!   is never a deep copy; a carrier that is already owned and
//!   refcounted keeps that true for free.
//!
//! The cost is one copy per exit, and it is the honest one: the
//! alternative was not zero copies, it was a copy the consumer had to
//! make itself, later, without knowing why.
//!
//! ## The corollary: some payloads cannot be carried at all
//!
//! Copying bytes only produces ownership when the bytes *are* the
//! payload. A backend may be handed something whose bytes are
//! **addresses of other live objects** — and copying those yields a
//! carrier that is owned by every type-level test the contract states,
//! `Send + Sync + 'static`, clone-is-a-refcount-bump, and that dangles
//! the instant its source is dropped. The copy moved the pointer; it
//! could not move what the pointer names.
//!
//! > **A payload that carries addresses instead of bytes is
//! > uncarriable.** A backend that meets one must refuse it by name, on
//! > *both* legs — the one that takes payloads out of the backend and
//! > the one that hands them back in — and never mint a carrier for it.
//!
//! There is no bound on what such a payload's pointers might reach, so
//! there is no depth of copying that would make it safe; "deeply own
//! the referenced objects" is not a smaller version of this problem,
//! it is a different backend. Refusal is the correct answer, not a
//! conservative one.
//!
//! FFmpeg's `AV_PKT_FLAG_TRUSTED` is the concrete instance —
//! wrapped-`AVFrame` producers use it for packets whose body is an
//! `AVFrame` pointer structure passed between components inside one
//! pipeline — and `mediadecode-ffmpeg` refuses it at both legs. A
//! backend with an equivalent (a handle table, a shared-memory cookie,
//! an object id) has the same duty.
//!
//! **This crate does not name the carrier.** No signature here or in
//! [`crate::decoder`] mentions a concrete buffer type; `D` and
//! `Buffer` stay generic and their bounds stay minimal
//! (`AsRef<[u8]>`), so a consumer states what it spends. What
//! satisfies the contract is the backend's use-site choice —
//! `mediadecode-ffmpeg` binds an opaque `FfmpegBytes` over an
//! `alloc::sync::Arc<[u8]>`, `mediadecode-webcodecs` binds its own
//! `Arc`-backed view — and a `no_std` backend with a static arena can
//! satisfy it too. The core neither knows nor cares which, which is
//! also what lets a backend change its storage without changing its
//! frames.
//!
//! # The resource governance contract
//!
//! *User-ruled 2026-08-25.*
//!
//! A decoding backend stands between a caller and a substrate — a C
//! library, a browser API, a driver — and every one of those substrates
//! allocates memory in response to bytes an attacker chose. A backend
//! therefore owes the caller an answer to "how much can this cost?",
//! and the honest answer has three tiers, not one. Stating where they
//! end is part of the contract: a boundary that is never written down
//! gets rediscovered, one review round at a time, as though it were a
//! defect.
//!
//! ## Tier one — what the backend allocates itself
//!
//! **Every byte a backend copies or allocates is bounded, by a named
//! seat or by a format.** No exceptions and no third kind: a buffer
//! whose size comes from a file answers to a configured ceiling, and a
//! buffer whose size is a property of a format answers to that format.
//! A site that appears to be neither has not been thought about yet.
//!
//! This tier is provable, and it is proved by enumeration rather than
//! by assertion: a backend is expected to keep an accounting of its own
//! allocation sites and what bounds each one.
//! `mediadecode-ffmpeg` keeps that table in its `buffer` module.
//!
//! Two rules this tier has cost real defects to learn:
//!
//! - **A judge must dominate the allocator's arithmetic, not the
//!   payload's.** A budget compared against what the bytes nominally
//!   weigh is not a budget on what will be spent. Allocators align,
//!   pad, and round; on ordinary inputs the difference is under one
//!   percent, which is exactly why under-pricing hides.
//! - **Everything a conversion can refuse is refused before anything it
//!   can allocate is allocated.** A correct refusal that arrives after
//!   the expensive half of the work is a correct refusal that did not
//!   help.
//!
//! ## Tier two — the substrate's own knobs
//!
//! **A backend sets every resource knob its substrate offers, at every
//! interposition point the substrate exposes.** These are not the
//! backend's allocations; they are the substrate's, intercepted where
//! the substrate agreed to be interrupted — a ceiling field, an
//! allocation callback, a negotiation hook, a custom I/O layer.
//!
//! This tier is **defense in depth, and it is not a proof.** Each knob
//! bounds what that knob was built to bound, and the union of them is
//! whatever the substrate's authors chose to make interruptible. A
//! backend is obliged to use all of them and obliged not to claim that
//! using them is the same as bounding the substrate.
//!
//! `mediadecode-ffmpeg` enumerates its knobs — what each one is, and
//! what each one bounds — in its `buffer` module, beside the tier-one
//! table.
//!
//! ## Tier three — the boundary
//!
//! **Allocations internal to the substrate, past its knob surface, are
//! the substrate's territory.** A parser can describe, in a handful of
//! bytes, a structure whose in-memory form is far larger, and nothing
//! outside that parser can observe it happen. A driver can size a pool
//! however it likes behind a declared extent. Where a substrate offers
//! no interposition point, a backend outside it has none either.
//!
//! **This crate does not promise to be a hypervisor for its
//! substrates.** It promises tier one, it performs tier two, and it
//! names tier three rather than papering over it. Bounding the input to
//! an amplification is achievable and is done — a parser cannot
//! allocate from bytes it was never handed — but bounding the output is
//! the substrate's own hardening work.
//!
//! **A deployment that needs a hard memory bound places the decode
//! behind an OS-level instrument**: an address-space or memory rlimit,
//! a cgroup, or a memory-limited worker process it can restart. That is
//! the industry answer for `libav*` and for every comparable substrate,
//! and it is the only instrument that actually bounds a foreign
//! allocator. The seats in tiers one and two **compose with** it — they
//! turn most hostile inputs into a named error instead of a killed
//! worker, and they make the worker's limit a backstop rather than a
//! first line — but they do not replace it, and a caller who treats
//! them as a replacement has been told otherwise here.

use core::fmt::Debug;

/// Backend vocabulary for compressed/decoded **video**.
pub trait VideoAdapter {
  /// Codec identifier (e.g. backend-specific newtype around
  /// FFmpeg `AVCodecID`, WebCodecs codec string, etc.).
  type CodecId: Copy + Eq + Debug;
  /// Pixel format identifier (e.g. backend-specific newtype around
  /// FFmpeg `AVPixelFormat`, WebCodecs `VideoPixelFormat`, RAW
  /// `VideoPixelType`, BRAW `BlackmagicRawResourceFormat`).
  ///
  /// `Clone`, not `Copy`, for the same reason
  /// [`AudioAdapter::ChannelLayout`] is: mediaframe 0.3's
  /// `PixelFormat` carries an owned `Other(SmolStr)` arm at the
  /// `alloc` tier, and that is the identifier the FFmpeg and
  /// WebCodecs adapters bind here.
  type PixelFormat: Clone + Eq + Debug;
  /// Backend-specific extras carried on every `VideoPacket` (e.g.
  /// FFmpeg side-data, WebCodecs metadata).
  type PacketExtra;
  /// Backend-specific extras carried on every `VideoFrame` (e.g.
  /// HDR mastering display, RAW sensor metadata, picture type).
  type FrameExtra;
}

/// Backend vocabulary for compressed/decoded **audio**.
pub trait AudioAdapter {
  /// Codec identifier.
  type CodecId: Copy + Eq + Debug;
  /// Sample format identifier (e.g. FFmpeg `AVSampleFormat`,
  /// WebCodecs `AudioSampleFormat`).
  type SampleFormat: Copy + Eq + Debug;
  /// Channel layout identifier (FFmpeg `AVChannelLayout`,
  /// WebCodecs raw count, RAW SDK fixed layouts).
  type ChannelLayout: Clone + Eq + Debug;
  /// Backend-specific extras carried on every `AudioPacket`.
  type PacketExtra;
  /// Backend-specific extras carried on every `AudioFrame`.
  type FrameExtra;
}

/// Backend vocabulary for compressed/decoded **subtitles**.
pub trait SubtitleAdapter {
  /// Codec identifier.
  type CodecId: Copy + Eq + Debug;
  /// Backend-specific extras carried on every `SubtitlePacket`.
  type PacketExtra;
  /// Backend-specific extras carried on every `SubtitleFrame`.
  type FrameExtra;
}

/// Backend vocabulary for a compressed/decoded **still image** — cover
/// art, an embedded thumbnail, a poster frame.
///
/// A separate vocabulary from [`VideoAdapter`] rather than a reuse of
/// it, because the two disagree about the one thing an adapter exists
/// to name: what rides the frame. A still's extras are EXIF, an ICC
/// profile, an orientation — not a picture type, not a field order,
/// not a best-effort timestamp. Bending [`VideoAdapter::FrameExtra`]
/// over both would give every motion frame seats that only a still
/// fills and every still seats that only motion fills.
///
/// The compressed side is an
/// [`AttachmentPacket`](crate::demuxer::AttachmentPacket): a still
/// image inside a container is an attachment, one whole file, no
/// timeline. [`PacketExtra`](Self::PacketExtra) is therefore the
/// attachment's extras, and a backend that also implements
/// [`DemuxAdapter`](crate::demuxer::DemuxAdapter) will normally bind
/// the same type in both seats — the packet a demuxer hands out is the
/// packet an image decoder is fed.
pub trait ImageAdapter {
  /// Codec identifier — the still's own codec (MJPEG, PNG, BMP, …),
  /// in the same namespace the rest of the backend uses.
  type CodecId: Copy + Eq + Debug;
  /// Pixel format identifier.
  ///
  /// `Clone`, not `Copy`, for the same reason
  /// [`VideoAdapter::PixelFormat`] is.
  type PixelFormat: Clone + Eq + Debug;
  /// Backend-specific extras carried on the
  /// [`AttachmentPacket`](crate::demuxer::AttachmentPacket) an image
  /// decoder is fed.
  type PacketExtra;
  /// Backend-specific extras carried on every
  /// [`ImageFrame`](crate::frame::ImageFrame) — EXIF, ICC profile,
  /// side data.
  type FrameExtra;
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Zero-sized "loopback" adapter that implements all three traits
  /// with `()` extras. Proves the traits are object-safe-ish in the
  /// associated-type sense (i.e. they can be implemented).
  pub struct Loopback;

  impl VideoAdapter for Loopback {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  impl AudioAdapter for Loopback {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  impl SubtitleAdapter for Loopback {
    type CodecId = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  impl ImageAdapter for Loopback {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  #[test]
  fn loopback_compiles() {
    // The fact that this test compiles means the four traits
    // are implementable. No runtime assertions necessary.
    fn _video<A: VideoAdapter>() {}
    fn _audio<A: AudioAdapter>() {}
    fn _subtitle<A: SubtitleAdapter>() {}
    fn _image<A: ImageAdapter>() {}
    _video::<Loopback>();
    _audio::<Loopback>();
    _subtitle::<Loopback>();
    _image::<Loopback>();
  }
}

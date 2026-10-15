//! Pixel format identifier: re-exported from
//! [`mediaframe::pixel_format`].
//!
//! mediadecode used to define this enum locally; the canonical
//! definition now lives in the lowest-layer `mediaframe` crate so
//! colconv, mediadecode, and scenesdetect share a single
//! identifier. Backends consume the re-export via
//! `mediadecode::PixelFormat` or `mediadecode::pixel_format::PixelFormat`
//! exactly as before.
//!
//! Note: mediaframe 0.3 struck the numeric escape `Unknown(u32)`.
//! `PixelFormat::None` is a **named** member — FFmpeg's own
//! `AV_PIX_FMT_NONE`, and the [`Default`] — and it is what mediadecode
//! backends produce when an FFmpeg / WebCodecs identifier doesn't map
//! to a known format. The open extension arm mediaframe offers instead
//! is `Other(SmolStr)`, which lives behind mediaframe's `alloc` feature;
//! mediadecode pins mediaframe at the no-alloc tier, so this re-export
//! is a closed vocabulary here.

pub use mediaframe::pixel_format::PixelFormat;

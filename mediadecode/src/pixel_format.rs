//! Pixel format identifier: re-exported from
//! [`videoframe::pixel_format`].
//!
//! mediadecode used to define this enum locally; the canonical
//! definition now lives in the lowest-layer `videoframe` crate so
//! colconv, mediadecode, and scenesdetect share a single
//! identifier. Backends consume the re-export via
//! `mediadecode::PixelFormat` or `mediadecode::pixel_format::PixelFormat`
//! exactly as before.
//!
//! Note: the videoframe variant for unrecognized wire values is
//! `Unknown(u32)` (preserves the raw integer for lossless round-trip),
//! not the prior unit-variant `Unknown`. Mediadecode backends fall
//! through to `PixelFormat::Unknown(raw as u32)` when an FFmpeg /
//! WebCodecs identifier doesn't map to a known format.

pub use videoframe::pixel_format::PixelFormat;

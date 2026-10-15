#![doc = include_str!("../README.md")]
#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

/// The shared vocabulary — the `mediaspine` crate. Feature `core`.
#[cfg(feature = "core")]
#[cfg_attr(docsrs, doc(cfg(feature = "core")))]
pub use mediaspine as spine;

/// The read faces — the `mediadecode` crate. Feature `decode`.
#[cfg(feature = "decode")]
#[cfg_attr(docsrs, doc(cfg(feature = "decode")))]
pub use mediadecode as decode;

/// The write faces — the `mediaencode` crate. Feature `encode`.
#[cfg(feature = "encode")]
#[cfg_attr(docsrs, doc(cfg(feature = "encode")))]
pub use mediaencode as encode;

/// The editing timeline — the `mediaio-timeline` crate. Feature
/// `timeline`.
#[cfg(feature = "timeline")]
#[cfg_attr(docsrs, doc(cfg(feature = "timeline")))]
pub use mediaio_timeline as timeline;

/// The FFmpeg adapters. Feature `ffmpeg`, which links libavcodec and
/// libavformat.
#[cfg(feature = "ffmpeg")]
#[cfg_attr(docsrs, doc(cfg(feature = "ffmpeg")))]
pub mod ffmpeg {
  /// The base: the crossings between FFmpeg's memory and the vocabulary
  /// — the `mediaio-ffmpeg` crate.
  pub use mediaio_ffmpeg as base;

  /// The read adapter — the `mediadecode-ffmpeg` crate.
  pub use mediadecode_ffmpeg as decode;

  /// The write adapter — the `mediaencode-ffmpeg` crate.
  pub use mediaencode_ffmpeg as encode;
}

/// The WebCodecs read adapter (`wasm32` only) — the
/// `mediadecode-webcodecs` crate. Feature `webcodecs`.
#[cfg(feature = "webcodecs")]
#[cfg_attr(docsrs, doc(cfg(feature = "webcodecs")))]
pub use mediadecode_webcodecs as webcodecs;

/// The time primitives — the `mediatime` crate. Feature `time`.
#[cfg(feature = "time")]
#[cfg_attr(docsrs, doc(cfg(feature = "time")))]
pub use mediatime as time;

/// The frame and pixel vocabulary — the `mediaframe` crate. Feature
/// `frame`.
#[cfg(feature = "frame")]
#[cfg_attr(docsrs, doc(cfg(feature = "frame")))]
pub use mediaframe as frame;

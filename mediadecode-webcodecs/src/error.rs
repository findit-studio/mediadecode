//! Error types for the WebCodecs adapter.

use std::borrow::Cow;

use thiserror::Error;
use wasm_bindgen::{JsCast, JsValue};

/// A WebCodecs / DOM error captured from a `JsValue`. JS errors
/// don't carry a stable Rust type, so we stringify at the boundary
/// and keep the message; consumers needing the original
/// [`web_sys::DomException`] can downcast `JsValue` themselves.
///
/// The message is a `Cow<'static, str>` rather than a `String` so
/// allocation-failure error paths can construct an `Error` without
/// touching the allocator. Codex round 22 flagged that the OOM
/// handlers in `copy_video_frame` and `copy_audio_data` were
/// allocating a formatted `String` (and then a JS string and another
/// `Error`-internal `String`) at the exact moment the global
/// allocator had just refused a request, which can panic and abort
/// the wasm tab. With this `Cow`, [`Error::from_static`] takes a
/// `&'static str` and clones it as `Cow::Borrowed`, no allocation.
#[derive(Debug, Clone, Error)]
#[error("WebCodecs error: {message}")]
pub struct Error {
  message: Cow<'static, str>,
}

impl Error {
  /// Builds an `Error` from a `JsValue` returned by a fallible
  /// `web-sys` call or thrown into the `error` callback.
  pub fn from_js(value: JsValue) -> Self {
    let message: String = if let Some(exc) = value.dyn_ref::<web_sys::DomException>() {
      // `name + ": " + message` matches the browser's own toString.
      let mut s = exc.name();
      let m = exc.message();
      if !m.is_empty() {
        s.push_str(": ");
        s.push_str(&m);
      }
      s
    } else if let Some(s) = value.as_string() {
      s
    } else {
      format!("{value:?}")
    };
    Self {
      message: Cow::Owned(message),
    }
  }

  /// Build an `Error` from a static string slice without
  /// touching the allocator. Use this in OOM-failure paths
  /// where the global allocator may have just refused a
  /// request and a fresh `String` allocation could itself
  /// panic. Cloning the resulting `Error` is also alloc-free
  /// (`Cow::Borrowed` → `Cow::Borrowed`).
  pub const fn from_static(msg: &'static str) -> Self {
    Self {
      message: Cow::Borrowed(msg),
    }
  }

  /// The captured message.
  pub fn message(&self) -> &str {
    &self.message
  }
}

/// Errors from [`crate::WebCodecsVideoStreamDecoder`] — **faults
/// only**.
///
/// Three arms used to live here that were not faults at all.
/// `NoFrameReady` and `Eof` are
/// [`Received::NeedsInput`](mediadecode::Received) and
/// [`Received::Ended`](mediadecode::Received) now, and `OutputFull` is
/// [`Sent::MustDrain`](mediadecode::Sent) — so a consumer generic over
/// the traits reads the same protocol from this backend as from any
/// other. This adapter had the family's only complete four-arm
/// vocabulary, and that was the problem rather than the fix: the
/// vocabulary belonged one tier up, where every backend answers it.///
/// **Open fault taxonomy, so it is `#[non_exhaustive]`.** New ways to
/// fail are discovered — a browser version, a codec, a DOM exception
/// nobody has met — and a consumer that meets one it has never heard of
/// should take its generic-fault path, which is exactly what the
/// wildcard arm this attribute forces is for. The status vocabularies
/// opposite it, [`Sent`](mediadecode::Sent) and
/// [`Received`](mediadecode::Received), are exhaustive for the
/// mirror-image reason: their arms are the substrate's fixed state set,
/// and there the wildcard would be dead weight hiding a state a
/// consumer forgot.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum VideoDecodeError {
  /// `send_packet` was called after `send_eof` resolved. The
  /// stream is over; call `flush()` first to reset for reuse.
  ///
  /// **A caller usage fault, not back pressure**, which is the line
  /// that keeps it here while `OutputFull` left. Draining changes
  /// nothing about it: this decoder will refuse every packet until
  /// `flush()`, so answering [`Sent::MustDrain`](mediadecode::Sent)
  /// would send the caller into a loop with no exit.
  ///
  /// Spelled `AfterEof` to match the rest of the family — the FFmpeg
  /// adapter's resampler and subtitle seams name the same condition the
  /// same way. It was `AtEof`, and one condition wearing two words
  /// across two backends of one trait is the disease this release is
  /// curing.
  #[error("decoder is at EOF; flush() before sending new packets")]
  AfterEof,

  /// The codec was not supported by the host browser, or
  /// `VideoDecoder.isConfigSupported(...)` returned false.
  #[error("unsupported codec: {0}")]
  UnsupportedCodec(String),

  /// The host browser does not expose `VideoDecoder` (i.e. the
  /// WebCodecs API is missing).
  #[error("WebCodecs VideoDecoder is not available in this browser")]
  Unavailable,

  /// The decoder is dead — its `error` callback fired or it was
  /// closed. The contained error is the last fatal cause.
  #[error("decoder is closed: {0}")]
  Closed(Error),

  /// The pixel format reported by `VideoFrame.format` is unknown
  /// or unsupported by this adapter.
  #[error("unsupported pixel format: {0}")]
  UnsupportedPixelFormat(String),

  /// A `web-sys` call returned a JS error.
  #[error(transparent)]
  Js(#[from] Error),
}

/// Errors from [`crate::WebCodecsAudioStreamDecoder`] — **faults
/// only**. See [`VideoDecodeError`] for what left and why.///
/// **Open fault taxonomy, so it is `#[non_exhaustive]`.** New ways to
/// fail are discovered — a browser version, a codec, a DOM exception
/// nobody has met — and a consumer that meets one it has never heard of
/// should take its generic-fault path, which is exactly what the
/// wildcard arm this attribute forces is for. The status vocabularies
/// opposite it, [`Sent`](mediadecode::Sent) and
/// [`Received`](mediadecode::Received), are exhaustive for the
/// mirror-image reason: their arms are the substrate's fixed state set,
/// and there the wildcard would be dead weight hiding a state a
/// consumer forgot.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum AudioDecodeError {
  /// `send_packet` was called after `send_eof` resolved. See the
  /// matching variant on [`VideoDecodeError`].
  #[error("decoder is at EOF; flush() before sending new packets")]
  AfterEof,

  /// Codec is not supported by the host browser.
  #[error("unsupported codec: {0}")]
  UnsupportedCodec(String),

  /// The host browser does not expose `AudioDecoder`.
  #[error("WebCodecs AudioDecoder is not available in this browser")]
  Unavailable,

  /// The decoder is dead.
  #[error("decoder is closed: {0}")]
  Closed(Error),

  /// `AudioData.format` was unknown or unsupported.
  #[error("unsupported sample format: {0}")]
  UnsupportedSampleFormat(String),

  /// A `web-sys` call returned a JS error.
  #[error(transparent)]
  Js(#[from] Error),
}

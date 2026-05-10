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

/// Errors from [`crate::WebCodecsVideoStreamDecoder`].
#[derive(Debug, Clone, Error)]
pub enum VideoDecodeError {
  /// `send_packet` cannot admit another chunk because the
  /// adapter's output queue is full — the consumer must call
  /// `receive_frame` to drain at least one frame before
  /// retrying. Returned **without** awaiting so the caller's
  /// `&mut self` is released; awaiting under the borrow would
  /// deadlock since only `receive_frame` (which also takes
  /// `&mut self`) can shrink the output queue.
  #[error("output queue full; drain via receive_frame and retry")]
  OutputFull,

  /// `receive_frame` was called with nothing in flight: empty
  /// queue, no chunks submitted to the decoder, no copy tasks
  /// pending, and `send_eof` has not been called. Returned
  /// **without** awaiting because there is no source of new
  /// frames — only `send_packet` (also `&mut self`) can supply
  /// one, and awaiting here would deadlock the caller.
  #[error("no video frame available; submit packets via send_packet")]
  NoFrameReady,

  /// `send_packet` was called after `send_eof` resolved. The
  /// stream is over; call `flush()` first to reset for reuse.
  #[error("decoder is at EOF; flush() before sending new packets")]
  AtEof,

  /// Decoder reached end of stream — `receive_frame` will not
  /// produce any more frames until `flush` resets it.
  #[error("decoder exhausted; call flush to reuse")]
  Eof,

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

/// Errors from [`crate::WebCodecsAudioStreamDecoder`].
#[derive(Debug, Clone, Error)]
pub enum AudioDecodeError {
  /// `send_packet` cannot admit another chunk because the
  /// adapter's output queue is full. See the matching variant
  /// on [`VideoDecodeError`].
  #[error("output queue full; drain via receive_frame and retry")]
  OutputFull,

  /// `receive_frame` was called with nothing in flight. See
  /// the matching variant on [`VideoDecodeError`].
  #[error("no audio frame available; submit packets via send_packet")]
  NoFrameReady,

  /// `send_packet` was called after `send_eof` resolved.
  #[error("decoder is at EOF; flush() before sending new packets")]
  AtEof,

  /// Decoder reached end of stream.
  #[error("decoder exhausted; call flush to reuse")]
  Eof,

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

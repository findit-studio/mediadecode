use ffmpeg_next::Packet;

use crate::backend::Backend;

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned from [`crate::VideoDecoder`].
///
/// `Debug` is implemented manually because [`ffmpeg_next::Packet`]
/// (carried by `AllBackendsFailed::unconsumed_packets`) does not
/// derive `Debug`. The hand-written impl summarizes the packet count
/// rather than dumping each packet's fields, which would be both
/// noisy and useless for triage.
#[derive(thiserror::Error)]
pub enum Error {
  /// An underlying FFmpeg error.
  #[error("ffmpeg error: {0}")]
  Ffmpeg(#[from] ffmpeg_next::Error),

  /// `avcodec_find_decoder` returned null for the input codec id. The id
  /// is reported as the raw integer (`AVCodecID` discriminant) — we do not
  /// construct the bindgen `AVCodecID` enum from a runtime value, since
  /// values outside our build's discriminant set would invoke UB.
  #[error("no decoder for codec id {0}")]
  NoCodec(u32),

  /// The codec does not advertise a hardware configuration matching the
  /// requested backend (via `avcodec_get_hw_config`).
  #[error("codec does not support backend {0:?}")]
  BackendUnsupportedByCodec(Backend),

  /// `av_hwdevice_ctx_create` failed for the requested backend.
  #[error("hardware device init failed for {backend:?}: {source}")]
  HwDeviceInitFailed {
    /// Backend that failed to initialise.
    backend: Backend,
    /// Underlying FFmpeg error.
    source: ffmpeg_next::Error,
  },

  /// Auto-probe exhausted every backend in the platform's order. Empty
  /// `attempts` means the platform has no hardware backends listed in
  /// [`crate::Backend`] for the current `target_os` — callers must
  /// fall back to a software decoder of their choice.
  ///
  /// `unconsumed_packets` holds the packets the decoder accepted from
  /// the caller before the probe exhausted (refcounted shallow clones
  /// of the packets fed via `send_packet`). For non-seekable inputs
  /// (live streams, pipes, network sources) the caller cannot
  /// re-demux from start, so this crate surfaces the buffered history
  /// here so the caller can feed those packets directly into a
  /// software decoder of their choice. When `AllBackendsFailed` comes
  /// from [`crate::VideoDecoder::open`] (no packets were ever sent),
  /// this vec is empty.
  #[error("all hardware backends failed; attempts: {attempts:?}")]
  AllBackendsFailed {
    /// Per-backend errors collected during probing, in the order tried.
    attempts: Vec<(Backend, Box<Error>)>,
    /// Packets the decoder consumed from the caller before exhaustion.
    /// Replay them through a software decoder for non-seekable inputs.
    unconsumed_packets: Vec<Packet>,
  },

  /// Surfaced by [`crate::FfmpegVideoStreamDecoder`] when a HW->SW
  /// fallback attempt itself fails — e.g. the SW decoder failed to
  /// open, EOF replay returned EAGAIN past the bounded retry, or the
  /// per-frame replay queue exceeded its cap. The HW decoder has
  /// already consumed `unconsumed_packets` from the caller; we
  /// surface them here so non-seekable inputs (pipes, live streams)
  /// can drive their own decoder of last resort.
  #[error("HW->SW fallback failed: {source}")]
  FallbackFailed {
    /// Underlying error that aborted the fallback transition.
    source: Box<Error>,
    /// Packets that the HW path had consumed but had not yet decoded
    /// at fallback time. The caller can replay them through a
    /// software decoder of their choice.
    unconsumed_packets: Vec<Packet>,
  },
}

impl std::fmt::Debug for Error {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Error::Ffmpeg(e) => f.debug_tuple("Ffmpeg").field(e).finish(),
      Error::NoCodec(id) => f.debug_tuple("NoCodec").field(id).finish(),
      Error::BackendUnsupportedByCodec(b) => {
        f.debug_tuple("BackendUnsupportedByCodec").field(b).finish()
      }
      Error::HwDeviceInitFailed { backend, source } => f
        .debug_struct("HwDeviceInitFailed")
        .field("backend", backend)
        .field("source", source)
        .finish(),
      Error::AllBackendsFailed {
        attempts,
        unconsumed_packets,
      } => f
        .debug_struct("AllBackendsFailed")
        .field("attempts", attempts)
        // `Packet` is not `Debug`; print just the count so the error is
        // still useful for triage without dumping per-packet bytes.
        .field(
          "unconsumed_packets",
          &format_args!("[{} packets]", unconsumed_packets.len()),
        )
        .finish(),
      Error::FallbackFailed {
        source,
        unconsumed_packets,
      } => f
        .debug_struct("FallbackFailed")
        .field("source", source)
        .field(
          "unconsumed_packets",
          &format_args!("[{} packets]", unconsumed_packets.len()),
        )
        .finish(),
    }
  }
}

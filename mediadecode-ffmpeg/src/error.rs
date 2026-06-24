use ffmpeg_next::Packet;

use crate::backend::Backend;

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned from [`crate::VideoDecoder`].
///
/// `Debug` is derived; the variants that wrap a payload struct
/// (`HwDeviceInitFailed`, `AllBackendsFailed`, `FallbackFailed`)
/// delegate their `Debug` to the payload, which is hand-written
/// where needed because [`ffmpeg_next::Packet`] (carried by
/// `AllBackendsFailed::unconsumed_packets` /
/// `FallbackFailed::unconsumed_packets`) does not derive
/// `Debug`. Those payloads summarize the packet count rather
/// than dumping each packet's fields, which would be both noisy
/// and useless for triage.
#[derive(Debug, Clone, thiserror::Error)]
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

  /// `av_hwdevice_ctx_create` failed for the requested backend. See
  /// [`HwDeviceInitFailed`] for the payload details. `#[from]` gives
  /// a free `impl From<HwDeviceInitFailed> for Error`, so inner
  /// helpers that return `Result<_, HwDeviceInitFailed>` can be
  /// `?`-propagated into `Error` directly.
  #[error(transparent)]
  HwDeviceInitFailed(#[from] HwDeviceInitFailed),

  /// Auto-probe exhausted every backend in the platform's order. See
  /// [`AllBackendsFailed`] for the payload details (in particular the
  /// `unconsumed_packets` history that callers should replay through
  /// their own software decoder for non-seekable inputs). `#[from]`
  /// gives a free `impl From<AllBackendsFailed> for Error`.
  #[error(transparent)]
  AllBackendsFailed(#[from] AllBackendsFailed),

  /// Surfaced by [`crate::FfmpegVideoStreamDecoder`] when a HW->SW
  /// fallback attempt itself fails. See [`FallbackFailed`] for the
  /// payload details (in particular the rescued `unconsumed_packets`
  /// the HW path had already consumed from the caller). `#[from]`
  /// gives a free `impl From<FallbackFailed> for Error`.
  #[error(transparent)]
  FallbackFailed(#[from] FallbackFailed),
}

/// Payload for [`Error::HwDeviceInitFailed`].
///
/// `av_hwdevice_ctx_create` failed for the requested backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("hardware device init failed for {backend:?}: {source}")]
pub struct HwDeviceInitFailed {
  /// Backend that failed to initialise.
  backend: Backend,
  /// Underlying FFmpeg error.
  source: ffmpeg_next::Error,
}

impl HwDeviceInitFailed {
  /// Constructs a new [`HwDeviceInitFailed`] payload.
  #[inline]
  pub const fn new(backend: Backend, source: ffmpeg_next::Error) -> Self {
    Self { backend, source }
  }
  /// Backend that failed to initialise.
  #[inline]
  pub const fn backend(&self) -> Backend {
    self.backend
  }
  /// Underlying FFmpeg error.
  #[inline]
  pub const fn source(&self) -> &ffmpeg_next::Error {
    &self.source
  }
  /// Consume the payload, returning the backend identifier and the
  /// moved FFmpeg error so callers can take ownership without
  /// cloning.
  #[inline]
  pub fn into_parts(self) -> (Backend, ffmpeg_next::Error) {
    (self.backend, self.source)
  }
}

/// Where in the decoder's life a [`AllBackendsFailed`] was raised.
///
/// The [`crate::FfmpegVideoStreamDecoder`] wrapper routes its software-fallback
/// replay on **this explicit signal** rather than inferring origin from whether
/// `unconsumed_packets` is empty. Both origins can carry an empty
/// `unconsumed_packets` — a probe-era failure on the *first* packet (a
/// side-data / byte / packet cap trip, or an `av_packet_ref` ENOMEM) has no
/// prior history to surface, exactly like every post-commit failure — so
/// emptiness cannot disambiguate them. Conflating the two made the wrapper
/// treat a probe-era first-packet cap trip as post-commit: it would append a
/// clone of the borrowed current packet to an empty replay set and skip the
/// post-fallback `send_packet`, silently dropping that packet if the clone
/// failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackOrigin {
  /// Raised while the inner decoder's probe was still active (before the first
  /// frame). `unconsumed_packets` is the probe's buffered history (possibly
  /// empty when the failure landed on the very first packet). The wrapper
  /// replays that history and then routes the still-unconsumed current packet
  /// to the new software decoder itself.
  Probe,
  /// Raised after the probe collapsed (the committed backend failed at
  /// runtime). `unconsumed_packets` is always empty — the probe buffer is gone
  /// — so the wrapper does not replay: it opens a software decoder cold,
  /// forwards only the failing call's current packet (or EOF), and resyncs at
  /// the next keyframe, accepting a bounded, logged gap (degrade-and-continue).
  PostCommit,
}

impl FallbackOrigin {
  /// `true` for [`FallbackOrigin::PostCommit`].
  #[inline]
  pub const fn is_post_commit(self) -> bool {
    matches!(self, FallbackOrigin::PostCommit)
  }
}

/// Payload for [`Error::AllBackendsFailed`].
///
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
///
/// `origin` records whether the failure happened during the probe or after the
/// committed backend collapsed at runtime — the explicit signal the wrapper
/// routes on (see [`FallbackOrigin`]). It is never inferred from
/// `unconsumed_packets.is_empty()`, which both origins can satisfy.
///
/// `Debug` is hand-written: [`ffmpeg_next::Packet`] does not derive
/// `Debug`, so we print `[N packets]` instead of dumping per-packet
/// bytes, which would be both noisy and useless for triage.
#[derive(Clone, thiserror::Error)]
#[error("all hardware backends failed; attempts: {attempts:?}")]
pub struct AllBackendsFailed {
  /// Per-backend errors collected during probing, in the order tried.
  attempts: Vec<(Backend, Box<Error>)>,
  /// Packets the decoder consumed from the caller before exhaustion.
  /// Replay them through a software decoder for non-seekable inputs.
  unconsumed_packets: Vec<Packet>,
  /// Whether this was raised during the probe or post-commit. The wrapper's
  /// fallback replay routes on this, never on `unconsumed_packets` emptiness.
  origin: FallbackOrigin,
}

impl AllBackendsFailed {
  /// Constructs a probe-era [`AllBackendsFailed`] payload — raised while the
  /// inner decoder's probe is still active. `unconsumed_packets` is the probe's
  /// buffered history (possibly empty if the failure landed on the first
  /// packet). See [`FallbackOrigin::Probe`].
  ///
  /// Not `const fn`: the `Vec` arguments may carry destructors and
  /// the const evaluator can't prove their drop safe for arbitrary
  /// allocator state.
  #[inline]
  pub fn new(attempts: Vec<(Backend, Box<Error>)>, unconsumed_packets: Vec<Packet>) -> Self {
    Self {
      attempts,
      unconsumed_packets,
      origin: FallbackOrigin::Probe,
    }
  }
  /// Constructs a post-commit [`AllBackendsFailed`] payload — raised after the
  /// probe collapsed, when the committed backend failed at runtime.
  /// `unconsumed_packets` is always empty (the probe buffer is gone); the
  /// wrapper's retained GOP window supplies the replay set. See
  /// [`FallbackOrigin::PostCommit`].
  #[inline]
  pub fn new_post_commit(attempts: Vec<(Backend, Box<Error>)>) -> Self {
    Self {
      attempts,
      unconsumed_packets: Vec::new(),
      origin: FallbackOrigin::PostCommit,
    }
  }
  /// Per-backend errors collected during probing, in the order tried.
  #[inline]
  pub fn attempts(&self) -> &[(Backend, Box<Error>)] {
    &self.attempts
  }
  /// Where this failure was raised — the explicit probe-vs-post-commit signal
  /// the wrapper routes its fallback replay on.
  #[inline]
  pub const fn origin(&self) -> FallbackOrigin {
    self.origin
  }
  /// Packets the decoder consumed from the caller before exhaustion.
  /// Replay them through a software decoder for non-seekable inputs.
  #[inline]
  pub fn unconsumed_packets(&self) -> &[Packet] {
    &self.unconsumed_packets
  }
  /// Consume the payload, returning the moved unconsumed packets so
  /// non-seekable callers can replay them through a software decoder
  /// without cloning.
  #[inline]
  pub fn into_unconsumed_packets(self) -> Vec<Packet> {
    self.unconsumed_packets
  }
  /// Consume the payload, returning the moved attempts log and
  /// unconsumed packets.
  #[inline]
  pub fn into_parts(self) -> (Vec<(Backend, Box<Error>)>, Vec<Packet>) {
    (self.attempts, self.unconsumed_packets)
  }
}

impl std::fmt::Debug for AllBackendsFailed {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AllBackendsFailed")
      .field("attempts", &self.attempts)
      // `Packet` is not `Debug`; print just the count so the error is
      // still useful for triage without dumping per-packet bytes.
      .field(
        "unconsumed_packets",
        &format_args!("[{} packets]", self.unconsumed_packets.len()),
      )
      .field("origin", &self.origin)
      .finish()
  }
}

/// Payload for [`Error::FallbackFailed`].
///
/// Surfaced by [`crate::FfmpegVideoStreamDecoder`] when a HW->SW
/// fallback attempt itself fails — e.g. the SW decoder failed to
/// open, EOF replay returned EAGAIN past the bounded retry, or the
/// per-frame replay queue exceeded its cap. The HW decoder has
/// already consumed `unconsumed_packets` from the caller; we
/// surface them here so non-seekable inputs (pipes, live streams)
/// can drive their own decoder of last resort.
///
/// `Debug` is hand-written for the same reason as
/// [`AllBackendsFailed`]: [`ffmpeg_next::Packet`] does not derive
/// `Debug`.
#[derive(Clone, thiserror::Error)]
#[error("HW->SW fallback failed: {source}")]
pub struct FallbackFailed {
  /// Underlying error that aborted the fallback transition.
  source: Box<Error>,
  /// Packets that the HW path had consumed but had not yet decoded
  /// at fallback time. The caller can replay them through a
  /// software decoder of their choice.
  unconsumed_packets: Vec<Packet>,
}

impl FallbackFailed {
  /// Constructs a new [`FallbackFailed`] payload.
  ///
  /// Not `const fn`: the `Vec` argument may carry destructors.
  #[inline]
  pub fn new(source: Box<Error>, unconsumed_packets: Vec<Packet>) -> Self {
    Self {
      source,
      unconsumed_packets,
    }
  }
  /// Underlying error that aborted the fallback transition.
  #[inline]
  pub fn source(&self) -> &Error {
    &self.source
  }
  /// Packets that the HW path had consumed but had not yet decoded
  /// at fallback time.
  #[inline]
  pub fn unconsumed_packets(&self) -> &[Packet] {
    &self.unconsumed_packets
  }
  /// Consume the payload, returning the moved unconsumed packets so
  /// non-seekable callers can replay them through a software decoder
  /// without cloning.
  #[inline]
  pub fn into_unconsumed_packets(self) -> Vec<Packet> {
    self.unconsumed_packets
  }
  /// Consume the payload, returning the moved source error and
  /// unconsumed packets.
  #[inline]
  pub fn into_parts(self) -> (Box<Error>, Vec<Packet>) {
    (self.source, self.unconsumed_packets)
  }
}

impl std::fmt::Debug for FallbackFailed {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FallbackFailed")
      .field("source", &self.source)
      .field(
        "unconsumed_packets",
        &format_args!("[{} packets]", self.unconsumed_packets.len()),
      )
      .finish()
  }
}

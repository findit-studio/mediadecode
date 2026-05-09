use std::{collections::VecDeque, mem::ManuallyDrop, ptr};

use ffmpeg_next::{
  Codec, Packet, Rational,
  codec::{
    self,
    Context,
    // Bring the `Mut` / `Ref` traits into scope so `Packet::as_ptr` /
    // `Packet::as_mut_ptr` resolve. They are aliased to avoid shadowing
    // any future `Mut`/`Ref` types we might add — `cargo clippy` would
    // otherwise flag them as "unused" without the alias and the import
    // can mistakenly look unused. Confirmed in use by all `packet.as_ptr()`
    // / `packet.as_mut_ptr()` call sites in this module.
    packet::{Mut as PacketMut, Ref as PacketRef},
  },
  ffi::{
    AVBufferRef, AVCodec, AVFrame, AVHWFramesContext, AVMediaType, av_buffer_ref, av_buffer_unref,
    av_frame_move_ref, av_frame_unref, av_hwdevice_ctx_create, av_hwframe_transfer_data,
    av_packet_ref, avcodec_alloc_context3, avcodec_free_context, avcodec_parameters_alloc,
    avcodec_parameters_copy, avcodec_parameters_free, avcodec_parameters_to_context,
  },
  frame,
};

/// Local FFI shim: `avcodec_find_decoder` declared with `c_int` instead of
/// the bindgen `AVCodecID` enum. Constructing `AVCodecID` from a runtime
/// integer that isn't in our build's discriminant set is UB; calling the
/// C function with a raw int avoids that boundary entirely. Both Rust
/// declarations resolve to the same C symbol at link time.
mod c_shims {
  use super::AVCodec;
  use libc::c_int;
  unsafe extern "C" {
    pub fn avcodec_find_decoder(id: c_int) -> *const AVCodec;
  }
}

use crate::{
  backend::{self, Backend},
  error::{Error, Result},
  ffi::{CallbackState, codec_supports_hwaccel, get_hw_format},
  frame::Frame,
};

/// Hardware-accelerated video decoder.
///
/// Hardware-only — there is no software fallback inside this crate. If
/// every hardware backend in the platform's probe order fails to open,
/// `open` returns [`Error::AllBackendsFailed`] and the caller is
/// responsible for falling back to a software decoder of their choice
/// (e.g. `ffmpeg::decoder::Video`).
///
/// Mirrors `ffmpeg::decoder::Video`'s `send_packet`/`receive_frame` interface.
/// Decoded frames are returned through [`crate::Frame`], a CPU-side wrapper
/// whose accessors avoid the `AVPixelFormat`-enum UB that an unvalidated read
/// of FFmpeg's raw integer pixel formats can trigger.
///
/// `open` does a true probe: each backend opens with a strict `get_format`
/// callback. On the first non-transient error from a backend the decoder is
/// torn down and the next backend in probe order is tried, with all packets
/// seen so far replayed through it. The advance is *transactional* — the
/// candidate backend must successfully build and accept the replayed packets
/// before any probe state is consumed, so a failing backend in the middle of
/// the order does not strand the caller without history. Once the first frame
/// is delivered the probe collapses and subsequent calls go straight to the
/// active backend.
pub struct VideoDecoder {
  /// Live FFmpeg state for the currently active backend.
  state: DecoderState,
  /// Reusable frame buffer used for hw-side decoding before transfer / move.
  /// Internal use only — never handed to callers.
  hw_frame: frame::Video,
  /// Probe state: present until the first frame is received from the active
  /// backend, then `None`. While `Some`, packets are buffered for replay and
  /// non-transient errors / decoder failures advance to the next backend.
  probe: Option<ProbeState>,
  /// CPU-side frames produced by a candidate decoder during probe replay
  /// (when its internal queue filled and we had to drain output before the
  /// next `send_packet`). Already transferred from the candidate's
  /// `AVHWFramesContext` to a CPU frame, so they remain valid after the
  /// candidate state is committed. [`Self::receive_frame`] dequeues these
  /// FIFO before reading from `state.inner`.
  pending_frames: VecDeque<frame::Video>,
  /// Per-decoder byte budget for [`Self::pending_frames`] during probe
  /// replay. Defaults to [`DEFAULT_MAX_PROBE_PENDING_BYTES`]; override via
  /// [`Self::with_max_probe_pending_bytes`].
  max_probe_pending_bytes: usize,
}

/// Owned FFmpeg state for one open codec context. Has its own `Drop` so we
/// can swap it out cleanly during a probe advance via `mem::replace`.
struct DecoderState {
  /// Wrapped FFmpeg decoder. `ManuallyDrop` so we can sequence its drop
  /// before freeing the callback state.
  inner: ManuallyDrop<ffmpeg_next::decoder::Video>,
  /// Backend driving this state.
  backend: Backend,
  /// Owned reference produced by `av_hwdevice_ctx_create`.
  hw_device_ref: *mut AVBufferRef,
  /// Owned `Box<CallbackState>` raw pointer; `AVCodecContext::opaque`
  /// aliases it.
  callback_state: *mut CallbackState,
}

/// Maximum number of packets we are willing to buffer for probe replay
/// before abandoning the fallback safety net. Set high enough to absorb
/// long B-frame GOPs and codec setup latency, low enough to bound memory
/// against malicious / pathological streams that never produce a first
/// frame.
const MAX_PROBE_PACKETS: usize = 256;

/// Maximum total compressed-byte size of buffered probe packets. Each
/// `Packet` clone holds a refcounted reference to the demuxer's bitstream
/// data — even though the clone itself is shallow, the underlying buffers
/// stay alive until we drop them. 64 MiB is generous for normal video and
/// gives untrusted media a hard ceiling.
const MAX_PROBE_PACKET_BYTES: usize = 64 * 1024 * 1024;

/// Hard cap on the number of side-data entries we tolerate per buffered
/// packet. `av_packet_ref` allocates an `AVPacketSideData` descriptor and
/// an `AVBufferRef` per entry, so a packet stuffed with many tiny or
/// zero-sized entries can consume significant memory in descriptor /
/// allocator overhead even after [`packet_side_data_bytes`] charges
/// [`SIDE_DATA_ENTRY_OVERHEAD`] bytes per entry. Refusing to clone such
/// packets short-circuits the descriptor explosion path.
///
/// Sized for legitimate streams (typical video packets carry 0-5 side-
/// data entries; SEI-heavy HEVC/AV1 maybe a dozen) while comfortably
/// rejecting weaponised input.
const MAX_PROBE_PACKET_SIDE_DATA_ENTRIES: usize = 64;

/// Conservative per-side-data-entry overhead estimate used by both
/// [`packet_side_data_bytes`] and the budget accounting in
/// [`VideoDecoder::send_packet`]. Counts the `AVPacketSideData`
/// descriptor (24 bytes per the FFmpeg 8.x bindings), the `AVBufferRef`
/// FFmpeg allocates per entry, and a margin for malloc bookkeeping
/// (header bytes, alignment slack). Setting it on the high side keeps
/// the byte cap a true upper bound on retained memory; under-charging
/// would let many tiny entries slip past the cap.
const SIDE_DATA_ENTRY_OVERHEAD: usize = 80;

/// Conservative upper-bound bytes-per-pixel multiplier used to estimate
/// the size of a CPU frame **before** `av_hwframe_transfer_data`
/// allocates its pixel buffers. Covers every HW download format this
/// crate produces (worst case is `P416LE` / `P412LE` at 6 bytes/pixel
/// for 16-bit 4:4:4 semi-planar) plus a margin for FFmpeg's per-row
/// stride alignment (typically 32-byte aligned, ~5% extra at HD widths
/// and below).
///
/// Used by [`drain_into_pending`] as a pre-transfer guard: if the
/// product `width * height * WORST_CASE_BYTES_PER_PIXEL` would already
/// push `pending_bytes` past `max_probe_pending_bytes`, the candidate
/// replay refuses the frame *before* allocating. Without this, FFmpeg
/// would perform the full HW→CPU download (potentially ~100 MiB for
/// 8K HDR) and we would only reject the frame after RSS had already
/// spiked. The post-transfer accounting via [`cpu_frame_bytes`] stays in
/// place as a backstop using the frame's actual stride/format.
///
/// Slightly over-charges true 4:2:0 NV12 / P010 frames (which dominate
/// real workloads) — that's the right side to err on. Callers feeding
/// 8K+ workloads through the probe path can tune
/// [`VideoDecoder::with_max_probe_pending_bytes`] upward to compensate.
const WORST_CASE_BYTES_PER_PIXEL: usize = 8;

/// Maximum number of CPU frames we are willing to queue from a candidate
/// during probe replay. Each frame is a fully-allocated CPU buffer
/// (~3 MiB for 1080p NV12, ~24 MiB for 4K P010, ~96 MiB for 8K P010), so
/// an unbounded queue would OOM on a candidate with a shallow internal
/// queue against a deep replay history. This cap, together with
/// [`DEFAULT_MAX_PROBE_PENDING_BYTES`], is enforced as a hard limit during
/// replay: once either limit is reached, probe buffering fails for the
/// candidate (returns `ENOMEM` from `drain_into_pending`) instead of
/// queueing additional drained frames. The probe loop then advances to
/// the next backend or returns `Error::AllBackendsFailed` if exhausted.
const MAX_PROBE_PENDING_FRAMES: usize = 16;

/// Default byte budget for probe-replay drained frames. 256 MiB is enough
/// for 16 frames at 4K P010 (~24 MiB each = 384 MiB worst case under the
/// count cap), and is the cap that fires first for very high-resolution
/// content (8K P010: ~96 MiB per frame → only ~2 frames fit).
///
/// Override per-decoder with [`VideoDecoder::with_max_probe_pending_bytes`]
/// when targeting 8K+ workloads or memory-constrained environments.
///
/// TODO: when frames significantly exceed typical sizes, consider
/// memmap-backed pending buffers (write transferred frames to a temp file
/// or shared-memory segment) so the resident set stays bounded even when
/// the byte cap is raised. Out of scope for now.
pub const DEFAULT_MAX_PROBE_PENDING_BYTES: usize = 256 * 1024 * 1024;

/// State carried only during the probe window (before the first successful
/// frame). Holds enough information to tear down the current decoder and
/// retry with the next backend.
struct ProbeState {
  parameters: codec::Parameters,
  codec: Codec,
  /// Backends still to try, in order. Empty means "no more options after
  /// the active one fails" — `advance_probe` then surfaces
  /// [`Error::AllBackendsFailed`] so the contract is the same on
  /// single-backend platforms (e.g. macOS) as on multi-backend ones.
  remaining_backends: Vec<Backend>,
  /// Packets sent so far, kept for replay through any candidate backend.
  /// Preserved across failed candidates — only cleared when the probe
  /// collapses on a successful first frame, or when the probe is
  /// abandoned due to the size caps.
  buffered_packets: Vec<Packet>,
  /// Cumulative size (in compressed bytes) of `buffered_packets`. Tracked
  /// incrementally so we don't have to re-sum on every send.
  buffered_bytes: usize,
  /// Whether `send_eof` has been called; replayed alongside packets.
  eof_sent: bool,
  /// Per-backend errors captured since the probe window opened. Pushed
  /// whenever a backend's failure triggers `advance_probe` (the active
  /// backend that just failed) or a candidate's build / replay rejects
  /// it. Drained into [`Error::AllBackendsFailed`] when the probe
  /// exhausts every option.
  attempts: Vec<(Backend, Box<Error>)>,
}

// SAFETY: All raw pointers are exclusively owned by `DecoderState` and never
// shared. `ffmpeg::decoder::Video` is itself `Send` (its `Context` carries an
// `unsafe impl Send`). The decoder is not safe for concurrent use, hence not
// `Sync`.
unsafe impl Send for DecoderState {}
unsafe impl Send for VideoDecoder {}

impl Drop for DecoderState {
  fn drop(&mut self) {
    // Order matters:
    //  1. Drop the codec context first. While it lives, FFmpeg may invoke
    //     `get_format`, which dereferences `callback_state` via `opaque`.
    //  2. Free the callback state heap allocation.
    //  3. Release our hw device reference (FFmpeg released its own when
    //     the codec context was freed in step 1).
    unsafe {
      ManuallyDrop::drop(&mut self.inner);
      if !self.callback_state.is_null() {
        drop(Box::from_raw(self.callback_state));
        self.callback_state = ptr::null_mut();
      }
      if !self.hw_device_ref.is_null() {
        av_buffer_unref(&mut self.hw_device_ref);
      }
    }
  }
}

impl VideoDecoder {
  /// Auto-probe hardware backends in the platform's default order.
  ///
  /// Each backend opens with a strict `get_format` callback. The first
  /// backend whose `avcodec_open2` succeeds becomes active; if its first
  /// frame is unusable (decode error, transfer failure, or a CPU-format
  /// frame from a HW context) the decoder is torn down and the next backend
  /// is tried — packets sent so far are replayed through the new decoder
  /// transparently. The probe advance is transactional: the next backend
  /// must build *and* accept the replayed history before any probe state is
  /// consumed, so a misbehaving middle backend cannot strand the caller.
  ///
  /// [`Self::backend`] reflects whichever backend ultimately produced the
  /// first frame.
  ///
  /// [`Error::AllBackendsFailed`] surfaces in two places, with the same
  /// meaning ("no hardware backend can decode this stream — fall back to
  /// software yourself"):
  /// - From `open` itself, when no backend even opens.
  /// - From [`Self::send_packet`] / [`Self::send_eof`] /
  ///   [`Self::receive_frame`], when the initially-opened backend fails
  ///   at decode time and every remaining backend in the probe order
  ///   either also fails or doesn't exist. On single-backend platforms
  ///   (e.g. macOS, where the order is `[VideoToolbox]`), this is the
  ///   only place a HW-only failure surfaces.
  ///
  /// In both cases, `attempts` carries the per-backend error log. When
  /// the runtime path fires, `unconsumed_packets` also contains the
  /// packets the decoder consumed from the caller before the probe
  /// exhausted (refcounted shallow clones); for non-seekable inputs
  /// (live streams, pipes) the caller can replay these directly into
  /// a software decoder of their choice without re-demuxing. From the
  /// open-time path the vec is empty since no packets have been sent.
  ///
  /// On `Ok`, the returned decoder **always** has an active probe
  /// rescue safety net. If a parameters clone fails under memory
  /// pressure before the probe state can be set up, `open` returns
  /// `Err(Error::Ffmpeg(Other { errno: ENOMEM }))` rather than handing
  /// back a live decoder with no fallback contract. No packets have
  /// been sent yet, so the caller can retry or fall back to software
  /// with the original `parameters` directly.
  pub fn open(parameters: codec::Parameters) -> Result<Self> {
    let codec = find_decoder(&parameters)?;
    let order = backend::probe_order();

    let mut attempts: Vec<(Backend, Box<Error>)> = Vec::new();
    for (i, &backend) in order.iter().enumerate() {
      // Use the checked clone — ffmpeg-next's `Parameters::clone` does
      // `avcodec_parameters_alloc` without a null check and ignores the
      // return of `avcodec_parameters_copy`. Under OOM that path silently
      // produces a Parameters with a null inner pointer.
      let cloned_for_build = match try_clone_parameters(&parameters) {
        Ok(p) => p,
        Err(e) => {
          tracing::warn!(?backend, error = %e, "hwdecode: parameters clone failed");
          attempts.push((backend, Box::new(Error::Ffmpeg(e))));
          continue;
        }
      };
      match Self::build_state(cloned_for_build, codec, backend) {
        Ok(state) => {
          tracing::info!(?backend, "hwdecode: opened video decoder (probing)");
          let remaining = order[(i + 1)..].to_vec();
          // Deep-copy the caller's `parameters` before storing in ProbeState.
          // `codec::Parameters` from `stream.parameters()` carries an Rc
          // owner pointing at the demuxer; moving that Rc to a worker
          // thread (when VideoDecoder is sent) would race with the demuxer's
          // Rc on the original thread. The checked clone copies the bytes
          // into a fresh allocation with `owner: None`, severing the link.
          //
          // We always create ProbeState — even when `remaining` is empty
          // (single-backend platforms like macOS) — so that a first-frame
          // failure on the only backend surfaces as
          // `Error::AllBackendsFailed` from `receive_frame` /
          // `send_packet` rather than as a raw FFmpeg error. That keeps
          // the API contract the same regardless of how many HW backends
          // the platform exposes.
          //
          // If the clone fails (ENOMEM), fail the **whole open call**
          // rather than returning a live decoder with `probe: None`.
          // Returning Ok here would let the caller send packets that the
          // active backend consumes, and a subsequent backend failure
          // would then surface as a raw FFmpeg error with no
          // `unconsumed_packets` — silently breaking the rescue contract
          // for non-seekable inputs (live streams, pipes). Dropping the
          // already-built `state` here runs its FFmpeg cleanup, and the
          // caller can retry / fall back to software with the original
          // parameters in their hand (no packets were consumed yet).
          // Seed the probe's attempt log with any backends that failed
          // to open earlier in this loop (including
          // `BackendUnsupportedByCodec` and parameters-clone errors).
          // Without this, a runtime exhaustion on the active backend
          // would surface an `AllBackendsFailed` containing only the
          // active backend's runtime failure — losing the original
          // open-time causes that, on multi-backend platforms (Linux,
          // Windows), are usually the more diagnostic signal. E.g. a
          // VAAPI-then-CUDA host where VAAPI fails to open and CUDA
          // later fails at first-frame must report both failures in
          // probe order, not just CUDA.
          let probe = match try_clone_parameters(&parameters) {
            Ok(probe_params) => ProbeState {
              parameters: probe_params,
              codec,
              remaining_backends: remaining,
              buffered_packets: Vec::new(),
              buffered_bytes: 0,
              eof_sent: false,
              attempts: std::mem::take(&mut attempts),
            },
            Err(e) => {
              tracing::warn!(
                error = %e,
                "hwdecode: parameters clone failed for probe state at open; \
                 failing closed instead of returning a decoder without rescue"
              );
              return Err(Error::Ffmpeg(e));
            }
          };
          return Ok(Self {
            state,
            hw_frame: alloc_av_frame().map_err(Error::Ffmpeg)?,
            probe: Some(probe),
            pending_frames: VecDeque::new(),
            max_probe_pending_bytes: DEFAULT_MAX_PROBE_PENDING_BYTES,
          });
        }
        Err(e) => {
          tracing::warn!(?backend, error = %e, "hwdecode: backend open failed");
          attempts.push((backend, Box::new(e)));
        }
      }
    }
    Err(Error::AllBackendsFailed {
      attempts,
      // No packets have been consumed at open time.
      unconsumed_packets: Vec::new(),
    })
  }

  /// Open the decoder with a specific backend. No probe, no fallback.
  ///
  /// If `backend` cannot actually decode this stream, the failure surfaces
  /// from [`Self::receive_frame`] (the strict `get_format` callback returns
  /// `AV_PIX_FMT_NONE`, the decoder errors out). The caller is responsible
  /// for retrying with another hardware backend or falling back to a
  /// software decoder of their choice (e.g. `ffmpeg::decoder::Video`).
  pub fn open_with(parameters: codec::Parameters, backend: Backend) -> Result<Self> {
    let codec = find_decoder(&parameters)?;
    let state = Self::build_state(parameters, codec, backend)?;
    Ok(Self {
      state,
      hw_frame: alloc_av_frame().map_err(Error::Ffmpeg)?,
      probe: None,
      pending_frames: VecDeque::new(),
      max_probe_pending_bytes: DEFAULT_MAX_PROBE_PENDING_BYTES,
    })
  }

  /// Override the byte budget for probe-replay queued frames. Defaults to
  /// [`DEFAULT_MAX_PROBE_PENDING_BYTES`]. Use a higher value when targeting
  /// 8K+ workloads where 16 frames at full size could exceed the default;
  /// use a lower value in memory-constrained services to bound peak
  /// allocation more tightly.
  ///
  /// Setting after the first frame has been delivered is harmless but has
  /// no observable effect — the probe has already collapsed and the cap
  /// only applies during replay drain.
  ///
  /// Returns `self` for builder-style chaining:
  /// ```ignore
  /// let decoder = VideoDecoder::open(params)?
  ///     .with_max_probe_pending_bytes(1024 * 1024 * 1024); // 1 GiB
  /// ```
  pub fn with_max_probe_pending_bytes(mut self, bytes: usize) -> Self {
    self.max_probe_pending_bytes = bytes;
    self
  }

  /// The backend currently producing frames. While the probe is still in
  /// progress (no frame received yet) this returns the optimistically
  /// selected backend; after the first frame, it is the backend that
  /// actually produced it. Once stable, never changes again.
  pub fn backend(&self) -> Backend {
    self.state.backend
  }

  /// Decoder width in pixels.
  pub fn width(&self) -> u32 {
    self.state.inner.width()
  }

  /// Decoder height in pixels.
  pub fn height(&self) -> u32 {
    self.state.inner.height()
  }

  /// Codec context time base.
  pub fn time_base(&self) -> Rational {
    self.state.inner.time_base()
  }

  /// Frame rate from the codec context, if known.
  pub fn frame_rate(&self) -> Option<Rational> {
    self.state.inner.frame_rate()
  }

  /// Submit a packet to the decoder.
  ///
  /// On success — and only on success — the packet is buffered for potential
  /// replay through a fallback backend while the probe is active. EAGAIN
  /// (decoder needs `receive_frame` to drain output first) propagates as
  /// normal backpressure; the caller drains then retries.
  ///
  /// While the probe is active, a non-transient error (e.g. the active HW
  /// backend rejecting this stream's geometry on first packet) advances the
  /// probe to the next candidate and retries the packet there. The caller
  /// observes only the eventual success or, if the probe is exhausted, the
  /// final error.
  ///
  /// **Atomic probe rescue.** While the probe is active, the rescue
  /// invariant is that everything FFmpeg has consumed since open is
  /// reflected in `buffered_packets` (so a future
  /// [`Error::AllBackendsFailed`] can hand a complete replay history
  /// back to the caller for software fallback on a non-seekable input).
  /// If we cannot prove this packet is buffer-able — its side-data
  /// entry count exceeds [`MAX_PROBE_PACKET_SIDE_DATA_ENTRIES`], its
  /// bytes would push the probe past [`MAX_PROBE_PACKETS`] or
  /// [`MAX_PROBE_PACKET_BYTES`], or [`av_packet_ref`] fails ENOMEM —
  /// `send_packet` returns [`Error::AllBackendsFailed`] **without
  /// invoking** `state.inner.send_packet` on this packet. The caller's
  /// packet stays in their hand and `unconsumed_packets` carries the
  /// pre-existing buffered history, so they can replay
  /// `unconsumed_packets` plus the current packet through their
  /// software decoder of choice. The post-probe path (after the first
  /// frame, when `self.probe` is `None`) skips this pre-flight
  /// entirely.
  pub fn send_packet(&mut self, packet: &Packet) -> Result<()> {
    loop {
      // Pre-flight while probe is active: prove we can record this
      // packet for replay BEFORE the active decoder consumes it.
      // `staged_clone` carries the refcounted clone and the new
      // `buffered_bytes` value through the send below; we only commit
      // them to the probe state if FFmpeg accepts the packet.
      let staged_clone: Option<(Packet, usize)> = if let Some(probe) = self.probe.as_ref() {
        // Step 1: side-data entry count cap. Read just `side_data_elems`
        // (no array walk yet) so a corrupt or weaponised value cannot
        // drive an unbounded loop from the safe entry point.
        let side_count = packet_side_data_count(packet);
        if side_count > MAX_PROBE_PACKET_SIDE_DATA_ENTRIES {
          let probe = self.probe.take().expect("probe present");
          tracing::warn!(
            side_data_entries = side_count,
            max_side_data_entries = MAX_PROBE_PACKET_SIDE_DATA_ENTRIES,
            trigger = "side_data_entry_cap",
            "hwdecode: probe rescue exhausted before consuming packet; \
             returning AllBackendsFailed without invoking decoder"
          );
          return Err(Error::AllBackendsFailed {
            attempts: probe.attempts,
            unconsumed_packets: probe.buffered_packets,
          });
        }
        // Step 2: byte / packet count cap. `packet_side_data_bytes`
        // clamps its walk to MAX_PROBE_PACKET_SIDE_DATA_ENTRIES as
        // defense-in-depth even though the count check above already
        // bounded the array length.
        let pkt_size = packet.size().saturating_add(packet_side_data_bytes(
          packet,
          MAX_PROBE_PACKET_SIDE_DATA_ENTRIES,
        ));
        let new_count = probe.buffered_packets.len() + 1;
        let new_bytes = probe.buffered_bytes.saturating_add(pkt_size);
        if new_count > MAX_PROBE_PACKETS || new_bytes > MAX_PROBE_PACKET_BYTES {
          let probe = self.probe.take().expect("probe present");
          tracing::warn!(
            packets = new_count,
            bytes = new_bytes,
            side_data_entries = side_count,
            max_packets = MAX_PROBE_PACKETS,
            max_bytes = MAX_PROBE_PACKET_BYTES,
            trigger = "byte_or_packet_cap",
            "hwdecode: probe rescue exhausted before consuming packet; \
             returning AllBackendsFailed without invoking decoder"
          );
          return Err(Error::AllBackendsFailed {
            attempts: probe.attempts,
            unconsumed_packets: probe.buffered_packets,
          });
        }
        // Step 3: pre-clone before consuming. `av_packet_ref` is a
        // refcounted shallow clone (no payload deep-copy) but can still
        // ENOMEM on heavy side-data; if it does we bail rather than
        // consuming a packet we can't track.
        match try_clone_packet(packet) {
          Ok(c) => Some((c, new_bytes)),
          Err(e) => {
            let probe = self.probe.take().expect("probe present");
            tracing::warn!(
              error = %e,
              "hwdecode: packet clone failed before consuming; \
               returning AllBackendsFailed without invoking decoder"
            );
            return Err(Error::AllBackendsFailed {
              attempts: probe.attempts,
              unconsumed_packets: probe.buffered_packets,
            });
          }
        }
      } else {
        None
      };

      match self.state.inner.send_packet(packet) {
        Ok(()) => {
          if let Some((cloned, new_bytes)) = staged_clone {
            // Probe is still Some here: the only paths that take it are
            // the bailouts above (which return) and `advance_probe`'s
            // exhaustion (which would have propagated via `?`). Commit
            // the clone now that FFmpeg has accepted the packet.
            if let Some(probe) = self.probe.as_mut() {
              probe.buffered_packets.push(cloned);
              probe.buffered_bytes = new_bytes;
            }
          }
          return Ok(());
        }
        Err(e) if is_transient(&e) => {
          // EAGAIN / EOF backpressure — pass through unchanged. The
          // staged clone drops; the caller will retry after draining
          // and we'll re-clone at the top of the loop.
          return Err(Error::Ffmpeg(e));
        }
        Err(e) => {
          if self.probe.is_some() {
            // advance_probe consumes the error into `attempts` and
            // either installs a candidate (Ok — loop top re-clones for
            // the new candidate) or surfaces AllBackendsFailed (Err —
            // `?` propagates). Either way the staged clone we just
            // built drops without entering history; the next iteration
            // clones afresh against the new active state.
            self.advance_probe(Error::Ffmpeg(e))?;
            continue;
          }
          return Err(Error::Ffmpeg(e));
        }
      }
    }
  }

  /// Signal end-of-stream to the decoder.
  ///
  /// Recorded for replay only if the underlying `send_eof` succeeds. While
  /// the probe is active, non-transient errors trigger probe advance and
  /// retry, matching `send_packet`'s behaviour.
  pub fn send_eof(&mut self) -> Result<()> {
    loop {
      match self.state.inner.send_eof() {
        Ok(()) => {
          if let Some(probe) = self.probe.as_mut() {
            probe.eof_sent = true;
          }
          return Ok(());
        }
        Err(e) if is_transient(&e) => return Err(Error::Ffmpeg(e)),
        Err(e) => {
          if self.probe.is_some() {
            self.advance_probe(Error::Ffmpeg(e))?;
            continue;
          }
          return Err(Error::Ffmpeg(e));
        }
      }
    }
  }

  /// Receive a CPU-side decoded frame.
  ///
  /// The frame is downloaded with `av_hwframe_transfer_data` and metadata
  /// is copied via `av_frame_copy_props`. The caller's frame is always
  /// unref'd first, so reuse across resolution changes or different
  /// decoders is safe.
  ///
  /// While the probe window is open, *any* non-transient failure (decode
  /// error, transfer error, copy_props error, or a CPU-format frame from a
  /// HW-opened context) tears down the current decoder and advances to the
  /// next hardware backend in probe order, replaying buffered packets
  /// through it. Frames the candidate produced during replay (drained when
  /// `send_packet` returned EAGAIN) are queued and delivered FIFO via this
  /// method, so the caller never loses initial frames after a fallback.
  ///
  /// This crate is hardware-only: there is no software fallback inside the
  /// decoder. When every backend in the probe order has been exhausted —
  /// including the case of a single-backend platform whose only backend
  /// failed — this returns [`Error::AllBackendsFailed`] with the per-
  /// backend attempt log so the caller can branch into a software
  /// decoder of their choice.
  ///
  /// Returns the same transient signals as `ffmpeg::decoder::Video`:
  /// `Error::Ffmpeg(Other { errno: EAGAIN })` when no frame is ready and
  /// more packets must be sent, and `Error::Ffmpeg(Eof)` once fully drained.
  pub fn receive_frame(&mut self, frame: &mut Frame) -> Result<()> {
    // Pre-drain frames queued during probe replay. They are already CPU-side
    // (transferred at drain time, when the candidate's HW context was alive)
    // so we just move them into the caller's slot.
    if self.try_pop_pending(frame) {
      return Ok(());
    }

    loop {
      let res = self.state.inner.receive_frame(&mut self.hw_frame);
      match res {
        Err(e) => {
          // EAGAIN is normal backpressure — pass through unconditionally.
          if is_eagain(&e) {
            return Err(Error::Ffmpeg(e));
          }
          // EOF (and every other non-transient error): if we are still
          // probing, treat it as candidate failure — a backend that drains
          // to EOF without ever producing a frame should not silently
          // present as "stream over" to the caller. Advance and retry; if
          // every backend has been exhausted, advance_probe surfaces
          // AllBackendsFailed and `?` propagates it.
          if self.probe.is_some() {
            self.advance_probe(Error::Ffmpeg(e))?;
            // Probe advance may have populated `pending_frames`; deliver
            // one of those before reading more from the new candidate.
            if self.try_pop_pending(frame) {
              return Ok(());
            }
            continue;
          }
          // Probe collapsed already — surface the error (including EOF
          // for a genuinely empty stream).
          return Err(Error::Ffmpeg(e));
        }
        Ok(()) => {
          // Always attempt the HW→CPU transfer. With strict `get_format`,
          // libavcodec can only deliver frames in the wired-up HW format
          // (or fail). If a misbehaving codec ever hands us a CPU-side
          // frame anyway, `av_hwframe_transfer_data` returns AVERROR(EINVAL)
          // (neither src nor dst has an AVHWFramesContext attached) and we
          // route through the same error path below.
          match unsafe { transfer_hw_frame(frame, &mut self.hw_frame) } {
            Ok(()) => {
              self.probe = None;
              return Ok(());
            }
            Err(e) => {
              if self.probe.is_some() {
                self.advance_probe(Error::Ffmpeg(e))?;
                unsafe { av_frame_unref(frame.as_inner_mut().as_mut_ptr()) };
                if self.try_pop_pending(frame) {
                  return Ok(());
                }
                continue;
              }
              return Err(Error::Ffmpeg(e));
            }
          }
        }
      }
    }
  }

  /// Pop one queued frame (produced by a candidate decoder during probe
  /// replay) into the caller's slot. Returns `true` when a frame was
  /// delivered, `false` when the queue was empty.
  fn try_pop_pending(&mut self, frame: &mut Frame) -> bool {
    let Some(mut buffered) = self.pending_frames.pop_front() else {
      return false;
    };
    // SAFETY: `buffered` is a CPU-side AVFrame we previously transferred
    // and pushed into the queue; both pointers are valid.
    unsafe {
      av_frame_unref(frame.as_inner_mut().as_mut_ptr());
      av_frame_move_ref(frame.as_inner_mut().as_mut_ptr(), buffered.as_mut_ptr());
    }
    // Probe semantics: delivering a frame collapses the probe.
    self.probe = None;
    true
  }

  /// Flush internal buffers (e.g. after a seek).
  ///
  /// Discards every frame buffered by the decoder, every frame queued during
  /// probe replay (`pending_frames`), and the residual `hw_frame` scratch
  /// buffer. Probe-time replay state (buffered packets, EOF marker) is also
  /// cleared since post-seek packets do not align with the previously
  /// captured history. After a flush, the next `receive_frame` waits for new
  /// post-seek input.
  pub fn flush(&mut self) {
    self.state.inner.flush();
    // SAFETY: hw_frame is a valid AVFrame we own; av_frame_unref is a no-op
    // for an already-empty frame.
    unsafe { av_frame_unref(self.hw_frame.as_mut_ptr()) };
    self.pending_frames.clear();
    if let Some(probe) = self.probe.as_mut() {
      probe.buffered_packets.clear();
      probe.buffered_bytes = 0;
      probe.eof_sent = false;
    }
  }

  /// Try the next backend in `remaining_backends`. Transactional: a
  /// candidate must successfully build and accept the replayed history
  /// before any probe state is consumed. Backends that fail to build or
  /// reject the replay are recorded into `probe.attempts` and the loop
  /// continues to the next one.
  ///
  /// `last_error` is the error that triggered this advance — i.e. the
  /// failure of the currently active backend on `send_packet` /
  /// `send_eof` / `receive_frame`. It is recorded against the active
  /// backend before any candidate is tried so that a final
  /// `AllBackendsFailed` carries the full attempt log including the
  /// initially-opened backend's runtime failure.
  ///
  /// Returns:
  /// - `Ok(())` when a candidate is installed and replay completed —
  ///   caller should retry the operation.
  /// - `Err(Error::AllBackendsFailed { attempts })` when every remaining
  ///   backend has been exhausted (including the just-failed active one).
  ///   This is what the documented `open` contract promises, surfaced at
  ///   runtime so the caller can branch into a software fallback. On a
  ///   single-backend platform (e.g. macOS), this fires after the only
  ///   backend's first-frame failure; on multi-backend platforms it
  ///   fires after the last candidate's failure.
  /// - `Err(_)` for other fatal conditions surfaced by probe machinery
  ///   itself (e.g. `alloc_av_frame` ENOMEM during replay drain).
  fn advance_probe(&mut self, last_error: Error) -> Result<()> {
    // Record the failure that triggered this advance against the active
    // backend. If the probe was somehow already gone (shouldn't happen —
    // call sites guard with `self.probe.is_some()`), just propagate the
    // error so behaviour matches the pre-fix code path.
    let active_backend = self.state.backend;
    match self.probe.as_mut() {
      Some(probe) => probe.attempts.push((active_backend, Box::new(last_error))),
      None => return Err(last_error),
    }

    // Drop frames previously queued from the backend we're now abandoning.
    // They came from a candidate that just failed for cause and cannot be
    // trusted alongside frames we may queue from the next candidate. (If
    // this method is called repeatedly via chained probe advances, this
    // also keeps `pending_frames` from accumulating frames from multiple
    // rejected backends.)
    self.pending_frames.clear();

    loop {
      // Snapshot inputs without mutating probe state. Use the checked
      // clone helper rather than `Parameters::clone` (which masks ENOMEM).
      let (next_backend, parameters, codec) = match self.probe.as_ref() {
        Some(probe) if !probe.remaining_backends.is_empty() => {
          let parameters = match try_clone_parameters(&probe.parameters) {
            Ok(p) => p,
            Err(e) => {
              tracing::warn!(
                error = %e,
                "hwdecode: parameters clone failed during probe advance; popping backend and trying next"
              );
              let popped = self
                .probe
                .as_mut()
                .expect("probe state present")
                .remaining_backends
                .remove(0);
              self
                .probe
                .as_mut()
                .expect("probe state present")
                .attempts
                .push((popped, Box::new(Error::Ffmpeg(e))));
              continue;
            }
          };
          (probe.remaining_backends[0], parameters, probe.codec)
        }
        // No more candidates — surface the accumulated attempt log as
        // AllBackendsFailed so single- and multi-backend platforms have
        // the same contract for "every HW backend failed."
        //
        // Hand the buffered packet history back to the caller along
        // with the attempt log: those packets were consumed from the
        // caller's demuxer (and refcounted-cloned into `buffered_packets`)
        // before the probe exhausted, and for non-seekable inputs the
        // caller cannot re-demux them. Returning them here lets a
        // caller-side software fallback replay the same byte history
        // through `ffmpeg::decoder::Video` without losing initial frames.
        // Dropping `ProbeState` after the take frees the codec/params
        // refs we no longer need; only `attempts` and `buffered_packets`
        // are retained.
        _ => {
          let (attempts, unconsumed_packets) = self
            .probe
            .take()
            .map(|p| (p.attempts, p.buffered_packets))
            .unwrap_or_default();
          return Err(Error::AllBackendsFailed {
            attempts,
            unconsumed_packets,
          });
        }
      };

      let prev_backend = self.state.backend;
      tracing::warn!(from = ?prev_backend, to = ?next_backend, "hwdecode: advancing probe");

      // Build candidate. On failure, record into attempts and continue
      // without touching the packet buffer.
      let mut candidate_state = match Self::build_state(parameters, codec, next_backend) {
        Ok(s) => s,
        Err(e) => {
          tracing::warn!(?next_backend, error = %e, "hwdecode: candidate build failed");
          self
            .probe
            .as_mut()
            .expect("probe state present")
            .remaining_backends
            .remove(0);
          self
            .probe
            .as_mut()
            .expect("probe state present")
            .attempts
            .push((next_backend, Box::new(e)));
          continue;
        }
      };

      // Replay buffered history through the candidate WITHOUT installing it.
      // We borrow the buffer immutably; if replay fails the candidate's Drop
      // releases the FFmpeg state and the buffer is preserved for the next
      // attempt.
      //
      // EAGAIN handling: `avcodec_send_packet` may return EAGAIN when its
      // internal queue is full and the user is expected to drain output
      // first (B-frame buffering, candidate-specific queue depth, etc.).
      // This is normal flow — we drain frames out of the candidate, transfer
      // each one to a CPU frame, and stash them in `local_pending`. After
      // commit they move to `self.pending_frames` and are delivered FIFO
      // by `receive_frame`, so the caller never loses initial frames.
      let mut local_pending: VecDeque<frame::Video> = VecDeque::new();
      let mut local_pending_bytes: usize = 0;
      let max_pending_bytes = self.max_probe_pending_bytes;
      let replay_result: std::result::Result<(), ffmpeg_next::Error> = {
        let probe = self.probe.as_ref().expect("probe state present");
        let mut hw_buf = match alloc_av_frame() {
          Ok(f) => f,
          Err(e) => return Err(Error::Ffmpeg(e)),
        };
        let mut r: std::result::Result<(), ffmpeg_next::Error> = Ok(());

        'replay: for pkt in &probe.buffered_packets {
          loop {
            match candidate_state.inner.send_packet(pkt) {
              Ok(()) => break,
              Err(e) if is_eagain(&e) => {
                // Drain candidate output (transferring + queueing each frame)
                // and retry the same packet.
                if let Err(de) = drain_into_pending(
                  &mut candidate_state.inner,
                  &mut hw_buf,
                  &mut local_pending,
                  &mut local_pending_bytes,
                  max_pending_bytes,
                ) {
                  r = Err(de);
                  break 'replay;
                }
              }
              Err(e) => {
                r = Err(e);
                break 'replay;
              }
            }
          }
        }
        if r.is_ok() && probe.eof_sent {
          // `avcodec_send_packet(NULL)` (which `send_eof` becomes) can
          // return EAGAIN with the same drain-output-first semantics as
          // a regular send_packet. Loop drain+retry instead of failing
          // the candidate on backpressure.
          loop {
            match candidate_state.inner.send_eof() {
              Ok(()) => break,
              Err(e) if is_eagain(&e) => {
                if let Err(de) = drain_into_pending(
                  &mut candidate_state.inner,
                  &mut hw_buf,
                  &mut local_pending,
                  &mut local_pending_bytes,
                  max_pending_bytes,
                ) {
                  r = Err(de);
                  break;
                }
              }
              Err(e) => {
                r = Err(e);
                break;
              }
            }
          }
        }
        r
      };

      if let Err(e) = replay_result {
        tracing::warn!(?next_backend, error = %e, "hwdecode: candidate replay failed");
        // Drop candidate explicitly so its FFI cleanup runs now. Discard any
        // frames we drained from this candidate — they're tied to a decoder
        // we're throwing away.
        drop(candidate_state);
        drop(local_pending);
        self
          .probe
          .as_mut()
          .expect("probe state present")
          .remaining_backends
          .remove(0);
        self
          .probe
          .as_mut()
          .expect("probe state present")
          .attempts
          .push((next_backend, Box::new(Error::Ffmpeg(e))));
        continue;
      }

      // Commit: install the candidate, clear residual hw_frame, queue the
      // drained frames for the caller, and pop the now-active backend.
      self.state = candidate_state;
      unsafe { av_frame_unref(self.hw_frame.as_mut_ptr()) };
      self.pending_frames.append(&mut local_pending);
      self
        .probe
        .as_mut()
        .expect("probe state present")
        .remaining_backends
        .remove(0);
      return Ok(());
    }
  }

  /// Build raw FFmpeg state for one hardware backend. Strict `get_format`
  /// (NONE on missing HW format); cross-backend fallback is the caller's job.
  fn build_state(
    parameters: codec::Parameters,
    codec: Codec,
    backend: Backend,
  ) -> Result<DecoderState> {
    // Use our checked allocator instead of Context::from_parameters, which
    // does not null-check avcodec_alloc_context3 and would feed a null
    // AVCodecContext into FFmpeg under OOM.
    let mut ctx = build_codec_context(&parameters)?;
    let av_type = backend.av_hwdevice_type();

    // Verify the codec advertises this hwaccel **with the exact HW pix_fmt
    // we're about to wire up in `get_format`**. FFmpeg's HW config table
    // is keyed per (device_type, pix_fmt); a codec can advertise the same
    // device with several HW pix_fmts, so matching only on device_type
    // would let probing succeed for a backend whose pix_fmt the codec
    // never offers — the failure would then surface deep inside the
    // probe/decode loop. Matching the exact pix_fmt keeps the strict
    // `get_format` honest and gives `open_with` a clean rejection.
    let hw_pix_fmt = backend.hw_pixel_format();
    if !codec_supports_hwaccel(unsafe { codec.as_ptr() }, av_type, hw_pix_fmt as i32) {
      return Err(Error::BackendUnsupportedByCodec(backend));
    }

    // Create the device context.
    let mut hw_device_ref: *mut AVBufferRef = ptr::null_mut();
    // SAFETY: `hw_device_ref` is a stack ptr we hand FFmpeg to fill.
    let ret = unsafe {
      av_hwdevice_ctx_create(&mut hw_device_ref, av_type, ptr::null(), ptr::null_mut(), 0)
    };
    if ret < 0 {
      return Err(Error::HwDeviceInitFailed {
        backend,
        source: ffmpeg_next::Error::from(ret),
      });
    }

    let callback_state = Box::into_raw(Box::new(CallbackState {
      wanted: hw_pix_fmt,
      wanted_int: hw_pix_fmt as i32,
    }));
    // RAII guard: from now until the end-of-function `into_owned()`, every
    // early return — `av_buffer_ref` failure, `open_as` failure, codec_type
    // mismatch, or any future error path added between here and the
    // `DecoderState` construction — frees `hw_device_ref` and
    // `callback_state` via the guard's Drop. Without it, each error site
    // had to remember to clean up these two FFI-owned resources by hand;
    // the codec_type-mismatch branch was missed and silently leaked one
    // device ref + one heap allocation per bad input.
    let guard = PartialBuildState {
      hw_device_ref,
      callback_state,
    };

    // SAFETY: ctx is a freshly-constructed AVCodecContext we own;
    // av_buffer_ref bumps the refcount of the device buffer for FFmpeg's
    // use (we keep our own ref in `hw_device_ref` for cleanup).
    // av_buffer_ref returns NULL on allocation failure; we must check it
    // before assigning, otherwise the codec context would be opened with a
    // HW-flagged setup but no actual device reference.
    let device_ref_for_ctx = unsafe { av_buffer_ref(hw_device_ref) };
    if device_ref_for_ctx.is_null() {
      // guard's Drop frees hw_device_ref (the first ref) and callback_state.
      return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
        errno: libc::ENOMEM,
      }));
    }
    // SAFETY: device_ref_for_ctx is a valid AVBufferRef* from av_buffer_ref;
    // ctx is freshly built and owned by us. After this point ctx aliases
    // `callback_state` via `opaque` (FFmpeg never frees opaque, so
    // `callback_state` ownership stays with us / the guard) and aliases
    // `device_ref_for_ctx` (the second ref) via `hw_device_ctx` (FFmpeg
    // unrefs that on codec context drop, independent of the guard's first
    // ref).
    unsafe {
      let raw = ctx.as_mut_ptr();
      (*raw).hw_device_ctx = device_ref_for_ctx;
      (*raw).opaque = callback_state.cast();
      (*raw).get_format = Some(get_hw_format);
    }

    // Open the decoder. On failure `ctx`/`opened` Drop releases the codec
    // context (and via that the second device ref); the guard releases the
    // first device ref and the callback state.
    //
    // We deliberately bypass `Opened::video()` because it calls
    // `Context::medium()`, which reads `AVCodecContext.codec_type` as the
    // bindgen `AVMediaType` enum — the same UB hazard we've been
    // systematically removing. Instead: validate `codec_type` as a raw
    // `c_int` ourselves, then construct the `decoder::Video` wrapper
    // directly via its public tuple field.
    let opened = ctx.decoder().open_as(codec).map_err(Error::Ffmpeg)?;

    // Validate codec_type as a raw integer — never construct AVMediaType
    // from an unvalidated runtime value.
    // SAFETY: codec_type is bound as AVMediaType (`#[repr(i32)]`), same
    // size and alignment as i32; reading the bytes as i32 cannot be UB.
    let codec_type_int: i32 =
      unsafe { ptr::read(ptr::addr_of!((*opened.as_ptr()).codec_type) as *const i32) };
    let video_type_int: i32 = AVMediaType::AVMEDIA_TYPE_VIDEO as i32;
    if codec_type_int != video_type_int {
      // Not a video codec context — surface the same error
      // `Opened::video()` would have, without going through enum
      // construction. `opened`'s Drop releases the codec context; the
      // guard releases the first hw_device_ref and the callback state.
      return Err(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
    }
    // SAFETY of construction: `decoder::Video` is `pub struct Video(pub Opened)`.
    // We construct via the public field; this is the same wrapping
    // `Opened::video()` does on success, just without the enum read.
    let opened = ffmpeg_next::decoder::Video(opened);

    // Disarm the guard and transfer ownership of both resources into the
    // returned DecoderState (whose own Drop handles their lifetime).
    let (hw_device_ref, callback_state) = guard.into_owned();
    Ok(DecoderState {
      inner: ManuallyDrop::new(opened),
      backend,
      hw_device_ref,
      callback_state,
    })
  }
}

/// RAII guard for the partially-owned FFmpeg state that
/// [`VideoDecoder::build_state`] holds between the
/// `av_hwdevice_ctx_create` and `Box::into_raw(CallbackState)`
/// allocations and the final `DecoderState` construction.
///
/// If `build_state` returns `Err` for any reason in that window
/// (`av_buffer_ref` ENOMEM, `open_as` failure, codec_type mismatch, or
/// any future error path), this guard's `Drop` releases
/// `hw_device_ref` — the first ref returned by `av_hwdevice_ctx_create`,
/// distinct from the second ref FFmpeg unrefs when the codec context
/// drops — and the boxed `CallbackState`, which FFmpeg never touches
/// because `AVCodecContext::opaque` is purely user-owned.
///
/// Successful construction calls [`Self::into_owned`] to disarm the
/// guard and hand both pointers to the new `DecoderState`.
struct PartialBuildState {
  hw_device_ref: *mut AVBufferRef,
  callback_state: *mut CallbackState,
}

impl PartialBuildState {
  /// Disarm the guard: return the owned pointers and replace the guard's
  /// fields with null so its Drop is a no-op.
  fn into_owned(mut self) -> (*mut AVBufferRef, *mut CallbackState) {
    let hw = std::mem::replace(&mut self.hw_device_ref, ptr::null_mut());
    let cb = std::mem::replace(&mut self.callback_state, ptr::null_mut());
    (hw, cb)
  }
}

impl Drop for PartialBuildState {
  fn drop(&mut self) {
    // SAFETY: pointers are either freshly allocated by `build_state` (via
    // `av_hwdevice_ctx_create` and `Box::into_raw`) or null after
    // `into_owned`. Both `av_buffer_unref` and `Box::from_raw` need the
    // null check we apply here; both are otherwise sound on resources we
    // own.
    unsafe {
      if !self.hw_device_ref.is_null() {
        let mut hw = self.hw_device_ref;
        av_buffer_unref(&mut hw);
      }
      if !self.callback_state.is_null() {
        drop(Box::from_raw(self.callback_state));
      }
    }
  }
}

/// Download a HW frame into a CPU [`Frame`]. Always unrefs the destination
/// first so reuse across resolution changes is safe.
///
/// Deliberately does **not** call `av_frame_copy_props`. That FFmpeg
/// helper deep-copies AVFrame side data (SEI, mastering display, ICC
/// profiles, dynamic HDR, etc.), the metadata dict, and bumps both
/// `opaque_ref` and `private_ref` on every receive — none of which
/// `Frame` exposes via its public accessors. On a crafted stream with
/// megabytes of per-frame metadata that would mean an unbounded
/// allocation per receive, with no caller-visible benefit. We instead
/// copy only the scalar fields the public API can read (today: `pts`);
/// pixel layout (`width`, `height`, `format`, `linesize`, `data`) is
/// already set by `av_hwframe_transfer_data`. If `Frame` ever grows
/// accessors for timing extras (`duration`, `time_base`, `pkt_dts`) or
/// color metadata, add those to `copy_frame_props_minimal` at the same
/// time.
unsafe fn transfer_hw_frame(
  dst: &mut Frame,
  src: &mut frame::Video,
) -> std::result::Result<(), ffmpeg_next::Error> {
  unsafe {
    av_frame_unref(dst.as_inner_mut().as_mut_ptr());
    let ret = av_hwframe_transfer_data(dst.as_inner_mut().as_mut_ptr(), src.as_ptr(), 0);
    if ret < 0 {
      return Err(ffmpeg_next::Error::from(ret));
    }
    // Validate the post-transfer CPU pix_fmt against the safe `Frame`
    // accessor's supported set. FFmpeg picks the destination format
    // when `dst.format == AV_PIX_FMT_NONE` on entry (which it always is
    // here — `av_frame_unref` clears it) by walking the result of
    // `av_hwframe_transfer_get_formats`. Driver/version ordering can
    // pick a layout outside our NV*/P0xx/P2xx/P4xx set; the call would
    // return success while the resulting frame is unreadable through
    // `Frame::row` / `Frame::as_ptr` (those return `None` for
    // unsupported formats). Surface the unsupported result as a
    // transfer failure so `receive_frame`'s probe-active path advances
    // to the next backend rather than collapsing on an unusable frame;
    // post-probe, the caller gets an `Err` they can branch into a
    // software fallback.
    let dst_raw_fmt: i32 = (*dst.as_inner_mut().as_ptr()).format;
    let dst_pix_fmt = crate::boundary::from_av_pixel_format(dst_raw_fmt);
    if !crate::frame::is_supported_cpu_pix_fmt(dst_pix_fmt) {
      tracing::warn!(
        pix_fmt = dst_raw_fmt,
        "hwdecode: hw->cpu transfer produced unsupported pix_fmt; \
         treating as backend failure"
      );
      av_frame_unref(dst.as_inner_mut().as_mut_ptr());
      return Err(ffmpeg_next::Error::Other {
        errno: libc::EINVAL,
      });
    }
    if let Err(e) = copy_frame_props_minimal(dst.as_inner_mut().as_mut_ptr(), src.as_ptr()) {
      // Failed to propagate metadata. Reset the destination so the
      // partial frame doesn't leak (its pixel buffers were attached
      // by `av_hwframe_transfer_data` above) and surface as a
      // backend failure — the probe path will advance to the next
      // candidate; post-probe, the caller branches into SW fallback.
      av_frame_unref(dst.as_inner_mut().as_mut_ptr());
      return Err(e);
    }
  }
  Ok(())
}

/// Copies AVFrame metadata (timestamps, color metadata, crop rect,
/// flags, side data, etc.) from the source HW frame to the destination
/// CPU frame so the post-transfer frame surfaces the same metadata a
/// SW-decoded frame would.
///
/// Defers to FFmpeg's `av_frame_copy_props`, which handles the per-
/// `side_data[i]` allocation, dict copy, and refcounted buffer
/// replacements internally. The cost is bounded by what the source
/// frame attaches — typical HDR streams carry 1–3 side-data entries
/// (mastering display, content light level, dolby/HDR10+ dynamic
/// metadata) totalling a few hundred bytes, so per-frame allocation
/// overhead stays negligible relative to the pixel data already
/// transferred via `av_hwframe_transfer_data`.
///
/// # Safety
/// Both pointers must be valid `AVFrame` pointers we own. We do not
/// form `&AVFrame` — `av_frame_copy_props` operates on raw pointers
/// directly.
/// Sum the byte sizes of every entry in `(*frame).side_data[]`.
/// Used by the probe replay queue's byte-cap accounting so a
/// frame's deep-copied side-data is charged against
/// `max_probe_pending_bytes` along with its pixel buffers.
///
/// # Safety
/// `frame` must be a live `*const AVFrame`. Reads only `nb_side_data`,
/// the `side_data` pointer array, and each `AVFrameSideData.size` —
/// no `&AVFrame` reference is formed.
unsafe fn sum_side_data_bytes(frame: *const AVFrame) -> usize {
  // Clamp `nb_side_data` to the same entry cap the copy path
  // enforces. Without the clamp, a decoder-controlled or
  // version-skew `nb_side_data` value (the bindgen field is
  // `c_int`, signed) could drive this walk arbitrarily long
  // before the cap downstream kicks in. Negative values are
  // pinned to zero before casting.
  let raw = unsafe { (*frame).nb_side_data };
  let arr = unsafe { (*frame).side_data };
  if raw <= 0 || arr.is_null() {
    return 0;
  }
  let count = (raw as usize).min(HW_COPY_SIDE_DATA_MAX_ENTRIES);
  let mut total: usize = 0;
  for i in 0..count {
    // SAFETY: `arr` points to `nb_side_data` valid `*mut AVFrameSideData`
    // entries per FFmpeg's contract; `i < count` is in-bounds.
    let entry = unsafe { *arr.add(i) };
    if entry.is_null() {
      continue;
    }
    let sz = unsafe { (*entry).size };
    total = total.saturating_add(sz);
    if total >= HW_COPY_SIDE_DATA_MAX_TOTAL_BYTES {
      // Already at or above the byte cap — further entries can't
      // change the projected-vs-cap decision the caller makes.
      total = HW_COPY_SIDE_DATA_MAX_TOTAL_BYTES;
      break;
    }
  }
  total
}

/// Hard cap on the number of `AVFrameSideData` entries we copy from
/// HW source frame to CPU destination frame on the HW transfer
/// path. Mirrors `convert::SIDE_DATA_MAX_ENTRIES`; the public
/// converter re-enforces the same cap so this is defense in depth.
const HW_COPY_SIDE_DATA_MAX_ENTRIES: usize = 64;
/// Hard cap on the total side-data byte budget per HW transfer.
/// Mirrors `convert::SIDE_DATA_MAX_TOTAL_BYTES`.
const HW_COPY_SIDE_DATA_MAX_TOTAL_BYTES: usize = 256 * 1024;

/// Maps a raw `AV_FRAME_DATA_*` integer to the matching bindgen
/// `AVFrameSideDataType` enum value when (and only when) the integer
/// is a known discriminant in the linked FFmpeg's bindgen output.
/// Returns `None` for unknown / version-skew / corrupt values —
/// the caller drops those entries instead of `transmute`-ing an
/// arbitrary integer back into the enum (which would be immediate
/// UB if the discriminant isn't in the enum's set).
///
/// The whitelist covers the entries safe to preserve across HW
/// transfer:
/// - HDR10 / HDR10+ / Dolby Vision / Vivid / ambient HDR metadata
/// - SMPTE / GOP timecodes
/// - ICC color profile
/// - A53 closed captions
/// - Spherical / display matrix orientation
/// - Stereo3D layout
///
/// Other AV_FRAME_DATA_* constants exist (motion vectors, encoder
/// params, RPU buffers, …) but are either decoder-internal or
/// rarely useful through the public mediadecode API; dropping them
/// is the safe default.
fn whitelisted_side_data_kind(kind_raw: i32) -> Option<ffmpeg_next::ffi::AVFrameSideDataType> {
  use ffmpeg_next::ffi::AVFrameSideDataType;
  // Each match arm compares `kind_raw` against the i32 cast of a
  // known constant, then returns the constant itself — we never
  // construct the enum from arbitrary integer bytes.
  let kind = match kind_raw {
    x if x == AVFrameSideDataType::AV_FRAME_DATA_PANSCAN as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_PANSCAN
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_A53_CC as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_A53_CC
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_STEREO3D as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_STEREO3D
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_DISPLAYMATRIX as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_DISPLAYMATRIX
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_AFD as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_AFD
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_GOP_TIMECODE as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_GOP_TIMECODE
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_SPHERICAL as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_SPHERICAL
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_ICC_PROFILE as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_ICC_PROFILE
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_S12M_TIMECODE as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_S12M_TIMECODE
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_DYNAMIC_HDR_PLUS as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_DYNAMIC_HDR_PLUS
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_REGIONS_OF_INTEREST as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_REGIONS_OF_INTEREST
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_SEI_UNREGISTERED as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_SEI_UNREGISTERED
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_FILM_GRAIN_PARAMS as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_FILM_GRAIN_PARAMS
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_DOVI_RPU_BUFFER as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_DOVI_RPU_BUFFER
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_DOVI_METADATA as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_DOVI_METADATA
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_DYNAMIC_HDR_VIVID as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_DYNAMIC_HDR_VIVID
    }
    x if x == AVFrameSideDataType::AV_FRAME_DATA_AMBIENT_VIEWING_ENVIRONMENT as i32 => {
      AVFrameSideDataType::AV_FRAME_DATA_AMBIENT_VIEWING_ENVIRONMENT
    }
    _ => return None,
  };
  Some(kind)
}

unsafe fn copy_frame_props_minimal(
  dst: *mut AVFrame,
  src: *const AVFrame,
) -> std::result::Result<(), ffmpeg_next::Error> {
  // We deliberately do NOT use `av_frame_copy_props` here, despite
  // its convenience. Upstream `av_frame_copy_props` deep-copies
  // *every* `AVFrameSideData` entry, the metadata `AVDictionary`,
  // and refcounted `opaque_ref` / `private_ref` buffers — all from
  // attacker-controlled decoder output. A crafted stream with many
  // multi-MiB side-data entries could drive the per-frame
  // allocation cost arbitrarily high (one alloc per entry, with the
  // entry's bytes copied via `memcpy`). The downstream
  // `convert::collect_side_data` cap helps the *Rust* side but the
  // FFmpeg-side allocations have already happened.
  //
  // Instead we copy scalar fields manually (timestamps, color
  // metadata, picture type, flags) and copy side-data with a hard
  // cap matching the converter's. Metadata dict and opaque_ref /
  // private_ref are intentionally NOT copied — they're rarely
  // populated on decoded frames and represent unbounded surfaces.
  use core::ptr::{addr_of, addr_of_mut, read_unaligned, write_unaligned};
  use ffmpeg_next::ffi::av_frame_new_side_data;
  unsafe {
    // Scalar timestamps / flags / color / SAR / crop. None of
    // these allocate.
    (*dst).pts = (*src).pts;
    (*dst).pkt_dts = (*src).pkt_dts;
    (*dst).duration = (*src).duration;
    (*dst).best_effort_timestamp = (*src).best_effort_timestamp;
    (*dst).quality = (*src).quality;
    (*dst).repeat_pict = (*src).repeat_pict;
    (*dst).flags = (*src).flags;
    (*dst).sample_aspect_ratio = (*src).sample_aspect_ratio;
    (*dst).crop_left = (*src).crop_left;
    (*dst).crop_top = (*src).crop_top;
    (*dst).crop_right = (*src).crop_right;
    (*dst).crop_bottom = (*src).crop_bottom;
    (*dst).time_base = (*src).time_base;

    // Enum-typed fields: bit-copy raw to avoid materializing an
    // invalid `AVColorPrimaries` etc. on either side. `read_unaligned`
    // / `write_unaligned` on `i32` projections sidestep the bindgen
    // enum's discriminant-validity invariant.
    let pict_type_raw = read_unaligned(addr_of!((*src).pict_type) as *const i32);
    write_unaligned(addr_of_mut!((*dst).pict_type) as *mut i32, pict_type_raw);
    let cp_raw = read_unaligned(addr_of!((*src).color_primaries) as *const i32);
    write_unaligned(addr_of_mut!((*dst).color_primaries) as *mut i32, cp_raw);
    let trc_raw = read_unaligned(addr_of!((*src).color_trc) as *const i32);
    write_unaligned(addr_of_mut!((*dst).color_trc) as *mut i32, trc_raw);
    let cs_raw = read_unaligned(addr_of!((*src).colorspace) as *const i32);
    write_unaligned(addr_of_mut!((*dst).colorspace) as *mut i32, cs_raw);
    let cr_raw = read_unaligned(addr_of!((*src).color_range) as *const i32);
    write_unaligned(addr_of_mut!((*dst).color_range) as *mut i32, cr_raw);
    let cl_raw = read_unaligned(addr_of!((*src).chroma_location) as *const i32);
    write_unaligned(addr_of_mut!((*dst).chroma_location) as *mut i32, cl_raw);

    // Side-data: bounded copy. `av_frame_new_side_data(dst, type,
    // size)` allocates the entry and returns a pointer to write
    // the payload bytes into; a null return is the OOM signal.
    // Callers (`transfer_hw_frame`, `drain_into_pending`) hand us
    // freshly-unref'd `dst` frames, so any prior side-data has
    // already been freed by `av_frame_unref` — we don't need to
    // strip dst's existing side-data here.
    // Read `nb_side_data` as the bindgen `c_int` and clamp non-
    // positive values BEFORE casting to `usize`. A negative value
    // (corrupt / version-skew decoder output) cast directly to
    // `usize` becomes a huge positive count and would walk OOB
    // memory below; pinning to zero up front collapses that to a
    // no-op. Same signed-count guard `sum_side_data_bytes` applies.
    let nb_side_data_raw = (*src).nb_side_data;
    let src_arr = (*src).side_data;
    if nb_side_data_raw > 0 && !src_arr.is_null() {
      let count_raw = nb_side_data_raw as usize;
      let count = count_raw.min(HW_COPY_SIDE_DATA_MAX_ENTRIES);
      if count_raw > HW_COPY_SIDE_DATA_MAX_ENTRIES {
        tracing::warn!(
          cap = HW_COPY_SIDE_DATA_MAX_ENTRIES,
          requested = count_raw,
          "mediadecode-ffmpeg: HW->CPU transfer side-data entry cap reached; truncating",
        );
      }
      let mut total_bytes: usize = 0;
      for i in 0..count {
        let entry = *src_arr.add(i);
        if entry.is_null() {
          continue;
        }
        let kind_raw = read_unaligned(addr_of!((*entry).type_) as *const i32);
        let size = (*entry).size;
        let data_ptr = (*entry).data;
        if size == 0 || data_ptr.is_null() {
          continue;
        }
        // Whitelist gate: only proceed when `kind_raw` matches a
        // known `AV_FRAME_DATA_*` constant the linked FFmpeg's
        // bindgen output knows about. Without this gate, a
        // version-skew or hostile decoder could write a side-data
        // type integer outside our bindgen's discriminant set, and
        // constructing the `AVFrameSideDataType` enum value (so
        // we could pass it to `av_frame_new_side_data`) would be
        // immediate UB before the call. Unknown types are dropped
        // with a debug-level log — the public converter's
        // `collect_side_data` walks the destination raw and would
        // also surface them as bare integers in `SideDataEntry.kind`.
        let Some(kind_enum) = whitelisted_side_data_kind(kind_raw) else {
          tracing::debug!(
            kind_raw,
            "mediadecode-ffmpeg: unknown AV_FRAME_DATA type during HW->CPU transfer; dropping",
          );
          continue;
        };
        let projected = total_bytes.saturating_add(size);
        if projected > HW_COPY_SIDE_DATA_MAX_TOTAL_BYTES {
          tracing::warn!(
            cap = HW_COPY_SIDE_DATA_MAX_TOTAL_BYTES,
            projected,
            "mediadecode-ffmpeg: HW->CPU transfer side-data byte cap reached; dropping rest",
          );
          break;
        }
        let new_entry = av_frame_new_side_data(dst, kind_enum, size);
        if new_entry.is_null() {
          // OOM mid-loop: stop copying further entries but don't
          // fail the whole transfer — the frames we did copy stay
          // attached. The convert path's cap is the final guard.
          tracing::warn!("mediadecode-ffmpeg: av_frame_new_side_data OOM during HW->CPU transfer",);
          break;
        }
        // SAFETY: `(*new_entry).data` is allocated for `size` bytes
        // per av_frame_new_side_data's contract; `data_ptr` is
        // valid for `size` reads per AVFrameSideData's contract.
        core::ptr::copy_nonoverlapping(data_ptr, (*new_entry).data, size);
        total_bytes = projected;
      }
    }
  }
  Ok(())
}

/// `EAGAIN` and `EOF` are normal flow signals from `avcodec_receive_frame`
/// and must not be treated as backend failures.
fn is_transient(e: &ffmpeg_next::Error) -> bool {
  is_eagain(e) || matches!(e, ffmpeg_next::Error::Eof)
}

/// Reject a `codec::Parameters` whose inner `*mut AVCodecParameters` is
/// null. This guards the public trust boundary: ffmpeg-next can produce
/// such a `Parameters` under OOM (`Parameters::new()` does not check
/// `avcodec_parameters_alloc`), and a safe caller can legally hand one
/// in. Without this check, the very next `(*p.as_ptr()).field` read
/// would be a null deref.
fn ensure_parameters_non_null(parameters: &codec::Parameters) -> Result<()> {
  // SAFETY: as_ptr() returns the inner *const AVCodecParameters; we just
  // inspect the pointer value (no deref).
  if unsafe { parameters.as_ptr() }.is_null() {
    return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    }));
  }
  Ok(())
}

/// Allocate a fresh `frame::Video`, checking that `av_frame_alloc` did not
/// return NULL. ffmpeg-next's `frame::Video::empty()` does not surface that
/// failure and the resulting null pointer would be UB on the next field
/// access; this wrapper catches it and surfaces it as `ENOMEM`.
fn alloc_av_frame() -> std::result::Result<frame::Video, ffmpeg_next::Error> {
  let inner = frame::Video::empty();
  // SAFETY: as_ptr() just exposes the inner pointer for inspection.
  if unsafe { inner.as_ptr() }.is_null() {
    return Err(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    });
  }
  Ok(inner)
}

/// Build a fresh `Context` from `parameters`, checking the underlying
/// `avcodec_alloc_context3` for NULL before passing it to
/// `avcodec_parameters_to_context`. ffmpeg-next's `Context::from_parameters`
/// skips that check and would feed a null pointer into FFmpeg under OOM —
/// undefined behavior. This helper surfaces the failure as `ENOMEM` and
/// frees the context if `parameters_to_context` itself errors.
pub(crate) fn build_codec_context(parameters: &codec::Parameters) -> Result<Context> {
  ensure_parameters_non_null(parameters)?;
  // SAFETY: avcodec_alloc_context3(NULL) returns a fresh AVCodecContext
  // or NULL on allocation failure.
  let ctx_ptr = unsafe { avcodec_alloc_context3(ptr::null()) };
  if ctx_ptr.is_null() {
    return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    }));
  }
  // SAFETY: ctx_ptr is non-null and freshly allocated; parameters.as_ptr()
  // returns a valid AVCodecParameters pointer; the function copies bytes
  // out of parameters into the context.
  let ret = unsafe { avcodec_parameters_to_context(ctx_ptr, parameters.as_ptr()) };
  if ret < 0 {
    // SAFETY: ctx_ptr was allocated by us and never handed to anyone else.
    let mut p = ctx_ptr;
    unsafe { avcodec_free_context(&mut p) };
    return Err(Error::Ffmpeg(ffmpeg_next::Error::from(ret)));
  }
  // SAFETY: ctx_ptr is valid; passing `owner: None` means our wrapper owns
  // the allocation and `Context::drop` will run `avcodec_free_context`.
  Ok(unsafe { Context::wrap(ctx_ptr, None) })
}

/// Checked deep-clone of `codec::Parameters`. ffmpeg-next's
/// `Parameters::clone` allocates via `avcodec_parameters_alloc` without
/// checking for NULL and runs `avcodec_parameters_copy` without checking
/// the return code. On `ENOMEM` the result is a `Parameters` with a null
/// inner pointer, which becomes UB when later passed to FFmpeg.
///
/// This helper performs both calls explicitly, frees a partial allocation
/// on failure, and surfaces the AVERROR. The returned `Parameters` has
/// `owner: None`, severing any Rc link to the caller's demuxer (the
/// reason we deep-clone in the first place — see Send safety in
/// `VideoDecoder::open`).
pub(crate) fn try_clone_parameters(
  src: &codec::Parameters,
) -> std::result::Result<codec::Parameters, ffmpeg_next::Error> {
  // Reject a null inner pointer at the boundary; a deref inside
  // avcodec_parameters_copy below would otherwise be UB.
  if unsafe { src.as_ptr() }.is_null() {
    return Err(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    });
  }
  // SAFETY: avcodec_parameters_alloc returns a fresh AVCodecParameters
  // pointer or NULL on allocation failure.
  let dst_ptr = unsafe { avcodec_parameters_alloc() };
  if dst_ptr.is_null() {
    return Err(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    });
  }
  // SAFETY: dst_ptr is non-null and freshly allocated; src.as_ptr() is
  // a valid AVCodecParameters pointer; the function copies bytes from
  // src into dst.
  let ret = unsafe { avcodec_parameters_copy(dst_ptr, src.as_ptr()) };
  if ret < 0 {
    // SAFETY: dst_ptr was allocated by us and never handed out.
    let mut p = dst_ptr;
    unsafe { avcodec_parameters_free(&mut p) };
    return Err(ffmpeg_next::Error::from(ret));
  }
  // SAFETY: dst_ptr is a valid AVCodecParameters; passing `owner: None`
  // means our wrapper owns the allocation and `Parameters::drop` will
  // call `avcodec_parameters_free`.
  Ok(unsafe { codec::Parameters::wrap(dst_ptr, None) })
}

/// Checked counterpart to `Packet::clone()`. ffmpeg-next's `clone_from`
/// calls `av_packet_ref` and ignores the int return value; on `ENOMEM`
/// the destination is left empty while the caller assumes the clone
/// succeeded — corrupting any later replay history. This helper surfaces
/// the AVERROR. The result is a refcounted shallow clone — the payload
/// buffer is shared with `src` rather than deep-copied; the probe replay
/// only sends packets through `avcodec_send_packet`, which does not
/// require a writable buffer.
fn try_clone_packet(src: &Packet) -> std::result::Result<Packet, ffmpeg_next::Error> {
  let mut dst = Packet::empty();
  // SAFETY: dst is a freshly zero-initialized Packet (av_init_packet inside
  // Packet::empty); av_packet_ref initializes its data fields from src's
  // refcounted buffer or returns AVERROR(ENOMEM) on failure.
  let ret = unsafe { av_packet_ref(dst.as_mut_ptr(), src.as_ptr()) };
  if ret < 0 {
    return Err(ffmpeg_next::Error::from(ret));
  }
  Ok(dst)
}

/// Sum of `AVPacket.side_data[i].size` across every entry, plus
/// `nb_entries * SIDE_DATA_ENTRY_OVERHEAD` (descriptor + AVBufferRef +
/// allocator bookkeeping per entry). `av_packet_ref` performs a deep
/// copy of side data via `av_packet_copy_props`, so each probe-buffered
/// clone retains every one of these bytes. Charging both keeps
/// `MAX_PROBE_PACKET_BYTES` a true upper bound — without the overhead,
/// many zero-size entries slip past the cap on pure descriptor cost.
///
/// Walks at most `max_entries` entries even when `side_data_elems`
/// reports a larger count. Defense-in-depth against a corrupt or hostile
/// packet whose `side_data_elems` lies about the actual array length:
/// the caller is expected to also reject any packet whose count exceeds
/// the cap (so the inflated clone is never created), but bounding the
/// walk here means a stale or weaponised value can never trigger an
/// unbounded raw-pointer scan from the safe API.
///
/// Reads only the `size` field of each `AVPacketSideData` entry — never
/// touches the bindgen `AVPacketSideDataType` enum, so no UB even if a
/// future FFmpeg adds a side-data type discriminant our build doesn't
/// know.
fn packet_side_data_bytes(packet: &Packet, max_entries: usize) -> usize {
  // SAFETY: AVPacket.side_data is `*mut AVPacketSideData` and
  // side_data_elems is `c_int`; both are raw struct fields safe to read.
  // Field projection (`.size`) does not reconstruct the enum-typed `type_`
  // field, so the bindgen-enum UB hazard does not apply here.
  unsafe {
    let raw = packet.as_ptr();
    let nel = (*raw).side_data_elems;
    let arr = (*raw).side_data;
    if arr.is_null() || nel <= 0 || max_entries == 0 {
      return 0;
    }
    let count = (nel as usize).min(max_entries);
    let mut total = count.saturating_mul(SIDE_DATA_ENTRY_OVERHEAD);
    for i in 0..count {
      let entry = arr.add(i);
      total = total.saturating_add((*entry).size);
    }
    total
  }
}

/// Number of `AVPacketSideData` entries on `packet`. The probe buffer
/// uses this to enforce [`MAX_PROBE_PACKET_SIDE_DATA_ENTRIES`] before
/// cloning, so a packet whose entry count alone would dominate retained
/// memory is rejected up front.
fn packet_side_data_count(packet: &Packet) -> usize {
  // SAFETY: side_data_elems is `c_int`, safe to read; clamp negatives to 0.
  let nel = unsafe { (*packet.as_ptr()).side_data_elems };
  if nel <= 0 { 0 } else { nel as usize }
}

/// Just `EAGAIN` (separate from EOF — the FFmpeg send/receive state machine
/// distinguishes "drain output and retry" from "stream over").
fn is_eagain(e: &ffmpeg_next::Error) -> bool {
  matches!(e, ffmpeg_next::Error::Other { errno } if *errno == ffmpeg_next::error::EAGAIN)
}

/// Look up the decoder for `parameters` without going through the bindgen
/// `AVCodecID` Rust enum. Reads the codec_id field as raw `u32` via
/// `addr_of!` + `ptr::read` so a value not in our build's discriminant
/// set never invokes UB.
fn find_decoder(parameters: &codec::Parameters) -> Result<Codec> {
  ensure_parameters_non_null(parameters)?;
  // SAFETY: parameters' inner pointer is non-null (checked above);
  // addr_of! projects to the codec_id field; the *const u32 cast is sound
  // because AVCodecID is `#[repr(u32)]` (same size and alignment as u32).
  // Reading as u32 cannot be UB regardless of the value FFmpeg wrote.
  let raw_id: u32 =
    unsafe { ptr::read(ptr::addr_of!((*parameters.as_ptr()).codec_id) as *const u32) };

  // Call C `avcodec_find_decoder` via our local `c_int`-typed shim — we
  // never construct an `AVCodecID` enum from `raw_id`. The C function
  // returns NULL for unknown ids, which we surface as `Error::NoCodec`.
  // SAFETY: avcodec_find_decoder is a pure FFmpeg lookup; passing any
  // c_int is sound (returns NULL for unknown).
  let codec_ptr = unsafe { c_shims::avcodec_find_decoder(raw_id as libc::c_int) };
  if codec_ptr.is_null() {
    return Err(Error::NoCodec(raw_id));
  }
  // SAFETY: codec_ptr is a non-null *const AVCodec into FFmpeg's static
  // codec table; it lives for the duration of the program.
  Ok(unsafe { Codec::wrap(codec_ptr) })
}

/// Drain output frames from a candidate decoder during probe replay,
/// transferring each one from the candidate's HW context to a fresh CPU
/// frame and queueing it. Returns `Ok(())` once the candidate signals
/// EAGAIN/EOF. The transfer happens while the candidate is still alive
/// (its `AVHWFramesContext` is reachable); the resulting CPU frames remain
/// valid after the candidate is committed because they hold their own
/// buffer references with no dependency on the original device context.
fn drain_into_pending(
  decoder: &mut ffmpeg_next::decoder::Video,
  hw_buf: &mut frame::Video,
  pending: &mut VecDeque<frame::Video>,
  pending_bytes: &mut usize,
  max_bytes: usize,
) -> std::result::Result<(), ffmpeg_next::Error> {
  loop {
    match decoder.receive_frame(hw_buf) {
      Ok(()) => {
        // Pre-transfer cap check: if we are already at or over either cap,
        // the candidate is producing more than we can hold. Treat as an
        // explicit candidate failure so `advance_probe` can try the next
        // backend instead of committing a stream with silently-dropped
        // frames in the middle.
        //
        // TODO: at very large frame sizes (8K HDR P010, > ~96 MiB each)
        // even a single retained frame is significant. Future direction:
        // memmap-backed pending frames (write to a temp file or shared
        // memory segment) so the resident set stays bounded even when the
        // byte cap is raised. Out of scope for now.
        if pending.len() >= MAX_PROBE_PENDING_FRAMES || *pending_bytes >= max_bytes {
          tracing::warn!(
            frames = pending.len(),
            bytes = *pending_bytes,
            max_frames = MAX_PROBE_PENDING_FRAMES,
            max_bytes = max_bytes,
            "hwdecode: probe pending cap reached; failing candidate replay"
          );
          // SAFETY: hw_buf is owned and valid; unref of an empty frame is a no-op.
          unsafe { av_frame_unref(hw_buf.as_mut_ptr()) };
          return Err(ffmpeg_next::Error::Other {
            errno: libc::ENOMEM,
          });
        }
        // Pre-transfer size guard: `av_hwframe_transfer_data` will
        // allocate the CPU buffer based on `hw_buf`'s dimensions. If a
        // single frame's worst-case footprint already pushes past the
        // cap, refuse the candidate **before** allocating so RSS does
        // not spike on a frame we'd immediately drop. Uses a width *
        // height * `WORST_CASE_BYTES_PER_PIXEL` upper bound; the
        // post-transfer accounting via `cpu_frame_bytes` below stays in
        // place as a backstop using the actual stride/format.
        let estimated_bytes = match estimate_transfer_bytes(hw_buf) {
          Some(b) => b,
          None => {
            // SAFETY: AVFrame.width/height are c_int reads.
            let (w, h) = unsafe {
              let raw = hw_buf.as_ptr();
              ((*raw).width, (*raw).height)
            };
            tracing::warn!(
              width = w,
              height = h,
              "hwdecode: HW frame dimensions invalid for sizing; failing candidate replay"
            );
            unsafe { av_frame_unref(hw_buf.as_mut_ptr()) };
            return Err(ffmpeg_next::Error::Other {
              errno: libc::ENOMEM,
            });
          }
        };
        let estimated_total = pending_bytes.saturating_add(estimated_bytes);
        if estimated_total > max_bytes {
          // SAFETY: AVFrame.width/height are c_int reads.
          let (w, h) = unsafe {
            let raw = hw_buf.as_ptr();
            ((*raw).width, (*raw).height)
          };
          tracing::warn!(
            pending_bytes = *pending_bytes,
            estimated_bytes,
            width = w,
            height = h,
            max_bytes = max_bytes,
            "hwdecode: pre-transfer size estimate exceeds cap; \
             refusing candidate replay before allocating CPU frame"
          );
          unsafe { av_frame_unref(hw_buf.as_mut_ptr()) };
          return Err(ffmpeg_next::Error::Other {
            errno: libc::ENOMEM,
          });
        }
        let mut cpu = alloc_av_frame()?;
        // SAFETY: hw_buf is a freshly-decoded HW frame;
        // `av_hwframe_transfer_data` allocates pixel buffers on `cpu`.
        // We use `copy_frame_props_minimal` (only `pts`) instead of
        // `av_frame_copy_props` for the same reason as
        // `transfer_hw_frame`: the public `Frame` API does not expose
        // side data / metadata / opaque refs, so deep-copying them per
        // frame is pure cost and an unbounded allocation source on
        // attacker-controlled streams.
        unsafe {
          let r1 = av_hwframe_transfer_data(cpu.as_mut_ptr(), hw_buf.as_ptr(), 0);
          if r1 < 0 {
            return Err(ffmpeg_next::Error::from(r1));
          }
        }
        // Same post-transfer pix_fmt validation as `transfer_hw_frame`.
        // A driver that picks a CPU format outside our supported set
        // would queue an unusable frame here; later, when
        // `try_pop_pending` hands it to the caller, `Frame::row` /
        // `Frame::as_ptr` would return `None`. Refuse the candidate
        // before the queue grows so probing advances to the next
        // backend instead.
        let cpu_raw_fmt: i32 = unsafe { (*cpu.as_ptr()).format };
        let cpu_pix_fmt = crate::boundary::from_av_pixel_format(cpu_raw_fmt);
        if !crate::frame::is_supported_cpu_pix_fmt(cpu_pix_fmt) {
          tracing::warn!(
            pix_fmt = cpu_raw_fmt,
            "hwdecode: candidate produced unsupported CPU pix_fmt during \
             probe replay; failing candidate"
          );
          return Err(ffmpeg_next::Error::Other {
            errno: libc::EINVAL,
          });
        }
        let pixel_bytes = match cpu_frame_bytes(&cpu) {
          Some(b) => b,
          None => {
            // Unknown pix_fmt or vertically-flipped layout — we cannot
            // bound this frame's contribution against the byte cap, so up
            // to MAX_PROBE_PENDING_FRAMES of them could exhaust memory.
            // Fail the candidate so probing tries the next backend
            // rather than queueing untracked allocations.
            // SAFETY: AVFrame.format is c_int, safe to read.
            let pix_fmt: i32 = unsafe { (*cpu.as_ptr()).format };
            tracing::warn!(
              pix_fmt,
              "hwdecode: cannot size unknown CPU pix_fmt during replay; failing candidate"
            );
            // cpu drops here.
            return Err(ffmpeg_next::Error::Other {
              errno: libc::ENOMEM,
            });
          }
        };
        // Account for side-data bytes that `av_frame_copy_props`
        // will deep-copy from the source HW frame. HDR streams
        // typically carry mastering display + content light level
        // (~50 bytes) and dynamic HDR metadata (~few hundred bytes);
        // pathological side-data could otherwise quietly bypass the
        // pixel-data byte cap.
        // SAFETY: hw_buf is a valid AVFrame; we read scalar fields
        // and pointer arrays without forming a `&AVFrame`.
        let side_data_bytes = unsafe { sum_side_data_bytes(hw_buf.as_ptr()) };
        let new_total = pending_bytes
          .saturating_add(pixel_bytes)
          .saturating_add(side_data_bytes);
        if new_total > max_bytes {
          tracing::warn!(
            pending_bytes = *pending_bytes,
            pixel_bytes,
            side_data_bytes,
            max_bytes,
            "hwdecode: queueing this frame would exceed byte cap; \
             failing candidate replay"
          );
          // cpu drops here without ever paying a metadata deep copy.
          return Err(ffmpeg_next::Error::Other {
            errno: libc::ENOMEM,
          });
        }
        // Cap check passed — copy AVFrame metadata. SAFETY: cpu and
        // hw_buf are both valid AVFrames we own. On failure (OOM
        // during side-data alloc) we propagate so the probe candidate
        // is treated as failed rather than queueing a frame whose
        // metadata silently disappeared.
        unsafe { copy_frame_props_minimal(cpu.as_mut_ptr(), hw_buf.as_ptr()) }?;
        *pending_bytes = new_total;
        pending.push_back(cpu);
      }
      Err(e) if is_transient(&e) => return Ok(()),
      Err(e) => return Err(e),
    }
  }
}

/// Allocated frame dimensions according to `hw_buf.hw_frames_ctx`.
///
/// Per FFmpeg's `libavutil/hwcontext.c::transfer_data_alloc`, the CPU
/// destination of `av_hwframe_transfer_data` is allocated using
/// `AVHWFramesContext.width / .height` (the *allocated* surface size of
/// the HW pool); only afterwards is `dst->width / dst->height` reset to
/// `src->width / src->height` (the *display* size). For cropped or
/// heavily aligned streams the allocated dims can be much larger than
/// the display dims (e.g. coded 8192×8192 surface with a 100×100
/// display crop), so any byte-cap accounting that uses display dims
/// undercounts by `allocated_height / display_height` and lets the
/// real allocation slip past the cap.
///
/// Returns `None` when no `hw_frames_ctx` is attached or the dimensions
/// are non-positive — the caller treats `None` as "cannot prove
/// allocation extent, fail the candidate."
fn hw_frames_ctx_dimensions(frame: &frame::Video) -> Option<(i32, i32)> {
  // SAFETY: AVFrame.hw_frames_ctx is `*mut AVBufferRef`. When non-null,
  // its `data` field points to an `AVHWFramesContext`. We read `.width`
  // and `.height` (both `c_int`) via field projection — neither field is
  // enum-typed, so no bindgen-enum UB hazard.
  unsafe {
    let raw = frame.as_ptr();
    let hw_ctx_ref = (*raw).hw_frames_ctx;
    if hw_ctx_ref.is_null() {
      return None;
    }
    let data = (*hw_ctx_ref).data;
    if data.is_null() {
      return None;
    }
    let frames_ctx = data as *const AVHWFramesContext;
    let w: i32 = ptr::read(ptr::addr_of!((*frames_ctx).width));
    let h: i32 = ptr::read(ptr::addr_of!((*frames_ctx).height));
    if w <= 0 || h <= 0 {
      return None;
    }
    Some((w, h))
  }
}

/// Conservative upper-bound estimate of the bytes
/// `av_hwframe_transfer_data` will allocate when downloading `hw_buf` to
/// a CPU frame. Used by [`drain_into_pending`] as a pre-transfer guard
/// so a candidate replay can refuse a frame whose footprint would
/// exceed the byte budget *without* first paying the allocation.
///
/// Sizes from `hw_buf.hw_frames_ctx` (the allocated dims used by the
/// FFmpeg transfer path) rather than `AVFrame.width / .height` (display
/// dims). On a cropped stream the two can differ by orders of magnitude
/// and using display dims would let the real allocation slip past the
/// cap.
///
/// Returns `None` when `hw_frames_ctx` is missing or its width/height
/// are non-positive — caller treats as candidate failure since we
/// cannot prove the allocation extent. (A SW source frame on the probe
/// replay path is not expected; we don't fall back to display dims
/// because that's the exact attack the cap is meant to prevent.)
fn estimate_transfer_bytes(hw_buf: &frame::Video) -> Option<usize> {
  let (w, h) = hw_frames_ctx_dimensions(hw_buf)?;
  Some(
    (w as usize)
      .saturating_mul(h as usize)
      .saturating_mul(WORST_CASE_BYTES_PER_PIXEL),
  )
}

/// Exact resident size of a CPU frame: sum of `AVFrame.buf[i].size`
/// across every populated buffer.
///
/// `AVBufferRef.size` is documented as "Size of data in bytes" — the
/// real allocated extent FFmpeg used. Reading it directly handles the
/// cropped/aligned case where `AVFrame.height` (display) is smaller
/// than the underlying allocation height (the `AVHWFramesContext`
/// surface size FFmpeg sized the buffer for); a `linesize *
/// plane_height_for(display_height)` formula would undercount in that
/// case.
///
/// Returns `None` only when `linesize[0]` is negative — FFmpeg's
/// vertically-flipped layout. The crate's safe row accessors
/// ([`crate::Frame::row`] / [`crate::Frame::rows`]) already reject
/// negative-stride frames, so queueing one during probe replay would
/// just delay the failure to the consumer; refusing here lets the
/// probe loop advance to the next backend instead.
fn cpu_frame_bytes(frame: &frame::Video) -> Option<usize> {
  // SAFETY: AVFrame.linesize is `[c_int; 8]`; AVFrame.buf is
  // `[*mut AVBufferRef; 8]`; AVBufferRef.size is `usize`. All are
  // primitive reads / pointer dereferences with no enum interpretation.
  unsafe {
    let raw = frame.as_ptr();
    let first_linesize = (*raw).linesize[0];
    // Vertically-flipped (negative linesize) is the only "unsizeable"
    // case we still surface as `None`; everything else can be exactly
    // measured from buf[i].size.
    if first_linesize < 0 {
      return None;
    }
    let mut total: usize = 0;
    for i in 0..(*raw).buf.len() {
      let buf = (*raw).buf[i];
      if buf.is_null() {
        continue;
      }
      total = total.saturating_add((*buf).size);
    }
    Some(total)
  }
}

#[allow(dead_code)]
fn _assert_send() {
  fn check<T: Send>() {}
  check::<VideoDecoder>();
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn no_codec_for_unknown_id() {
    let err = Error::NoCodec(0);
    assert!(format!("{err}").contains("no decoder"));
  }

  #[test]
  fn videodecoder_is_send() {
    _assert_send();
  }

  #[test]
  fn is_transient_recognises_eagain_and_eof() {
    let eagain = ffmpeg_next::Error::Other {
      errno: ffmpeg_next::error::EAGAIN,
    };
    assert!(is_transient(&eagain));
    assert!(is_transient(&ffmpeg_next::Error::Eof));
    let other = ffmpeg_next::Error::InvalidData;
    assert!(!is_transient(&other));
  }

  /// Regression: a `codec::Parameters` with a null inner pointer must be
  /// rejected at the entrypoint, not deref'd. ffmpeg-next's
  /// `Parameters::new()` does not check `avcodec_parameters_alloc()`, so a
  /// safe caller can hand us such a value under OOM.
  #[test]
  fn open_rejects_null_parameters() {
    // SAFETY: Parameters::wrap accepts any pointer; we explicitly construct
    // one with null inner. avcodec_parameters_free is null-safe on Drop.
    let null_params = unsafe { codec::Parameters::wrap(std::ptr::null_mut(), None) };
    match VideoDecoder::open(null_params) {
      Ok(_) => panic!("open should fail on null parameters"),
      Err(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })) => {
        assert_eq!(errno, libc::ENOMEM, "expected ENOMEM, got {errno}");
      }
      Err(other) => panic!("expected Ffmpeg(Other {{ ENOMEM }}), got {other:?}"),
    }
  }

  #[test]
  fn open_with_rejects_null_parameters() {
    // SAFETY: see open_rejects_null_parameters.
    let null_params = unsafe { codec::Parameters::wrap(std::ptr::null_mut(), None) };
    match VideoDecoder::open_with(null_params, Backend::VideoToolbox) {
      Ok(_) => panic!("open_with should fail on null parameters"),
      Err(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })) => {
        assert_eq!(errno, libc::ENOMEM, "expected ENOMEM, got {errno}");
      }
      Err(other) => panic!("expected Ffmpeg(Other {{ ENOMEM }}), got {other:?}"),
    }
  }

  /// `try_clone_packet` calls `av_packet_ref`, which deep-copies side
  /// data via `av_packet_copy_props`. The probe budget therefore has to
  /// include side-data bytes — otherwise a stream with a 16-byte payload
  /// and a 1 MiB side-data attachment would only consume 16 bytes of the
  /// 64 MiB budget per packet, and 256 buffered clones would retain
  /// ~256 MiB of side data while logs claim a few KiB.
  #[test]
  fn packet_side_data_counts_against_probe_budget() {
    use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

    const PAYLOAD_SIZE: usize = 16;
    const SIDE_DATA_SIZE: usize = 1024 * 1024; // 1 MiB

    let mut packet = Packet::new(PAYLOAD_SIZE);
    // SAFETY: packet is a freshly allocated AVPacket; av_packet_new_side_data
    // attaches a fresh `SIDE_DATA_SIZE`-byte buffer of the requested type
    // to it and returns a writable pointer (or NULL on OOM).
    let p = unsafe {
      av_packet_new_side_data(
        packet.as_mut_ptr(),
        AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
        SIDE_DATA_SIZE,
      )
    };
    assert!(!p.is_null(), "av_packet_new_side_data returned NULL");

    assert_eq!(packet.size(), PAYLOAD_SIZE);
    let side = packet_side_data_bytes(&packet, MAX_PROBE_PACKET_SIDE_DATA_ENTRIES);
    assert!(
      side >= SIDE_DATA_SIZE,
      "side-data accounting must include the attached buffer; got {side}"
    );
    let total = packet.size().saturating_add(side);
    assert!(
      total >= PAYLOAD_SIZE + SIDE_DATA_SIZE,
      "probe budget must charge payload + side data; got {total}"
    );
  }

  #[test]
  fn packet_side_data_is_zero_when_no_side_data() {
    let packet = Packet::new(64);
    assert_eq!(
      packet_side_data_bytes(&packet, MAX_PROBE_PACKET_SIDE_DATA_ENTRIES),
      0
    );
    assert_eq!(packet_side_data_count(&packet), 0);
  }

  /// Packets with many tiny side-data entries must be charged the
  /// per-entry descriptor + ref overhead, even when each entry's payload
  /// `size` is zero. Without `SIDE_DATA_ENTRY_OVERHEAD`, a packet stuffed
  /// with N zero-byte entries would charge 0 bytes against the budget
  /// while `av_packet_ref` still allocates ~`N * 80` bytes of descriptor
  /// + AVBufferRef + allocator overhead per cloned copy.
  #[test]
  fn packet_side_data_bytes_charges_descriptor_overhead_for_zero_size_entries() {
    use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

    let mut packet = Packet::new(0);
    // Attach two zero-byte entries of distinct types so neither call
    // replaces the other.
    let p1 = unsafe {
      av_packet_new_side_data(
        packet.as_mut_ptr(),
        AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
        0,
      )
    };
    let p2 = unsafe {
      av_packet_new_side_data(
        packet.as_mut_ptr(),
        AVPacketSideDataType::AV_PKT_DATA_PALETTE,
        0,
      )
    };
    assert!(
      !p1.is_null() && !p2.is_null(),
      "av_packet_new_side_data NULL"
    );

    assert_eq!(packet_side_data_count(&packet), 2);
    let bytes = packet_side_data_bytes(&packet, MAX_PROBE_PACKET_SIDE_DATA_ENTRIES);
    assert!(
      bytes >= 2 * SIDE_DATA_ENTRY_OVERHEAD,
      "must charge descriptor overhead per entry even at zero payload; got {bytes}"
    );
  }

  /// `packet_side_data_bytes` must clamp its walk to `max_entries`
  /// regardless of `side_data_elems`. Defense-in-depth: the caller is
  /// expected to short-circuit packets whose count exceeds the cap, but
  /// if a corrupt or weaponised packet ever does reach the helper, the
  /// internal cap prevents an unbounded raw-pointer walk.
  ///
  /// This test attaches 5 entries of distinct types and asks the helper
  /// to walk only the first 2. Result must equal exactly `2 * overhead +
  /// (size_a + size_b)`, confirming entries 3-5 were not even read.
  #[test]
  fn packet_side_data_bytes_respects_max_entries_cap() {
    use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

    let mut packet = Packet::new(0);
    // Five distinct side-data types so each `av_packet_new_side_data`
    // call appends rather than replaces.
    let types_and_sizes: [(AVPacketSideDataType, usize); 5] = [
      (AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA, 100),
      (AVPacketSideDataType::AV_PKT_DATA_PALETTE, 200),
      (AVPacketSideDataType::AV_PKT_DATA_REPLAYGAIN, 300),
      (AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX, 400),
      (AVPacketSideDataType::AV_PKT_DATA_STEREO3D, 500),
    ];
    for (ty, size) in types_and_sizes {
      let p = unsafe { av_packet_new_side_data(packet.as_mut_ptr(), ty, size) };
      assert!(!p.is_null(), "av_packet_new_side_data returned NULL");
    }
    assert_eq!(packet_side_data_count(&packet), 5);

    let walked_2 = packet_side_data_bytes(&packet, 2);
    let walked_5 = packet_side_data_bytes(&packet, 5);

    assert_eq!(
      walked_2,
      2 * SIDE_DATA_ENTRY_OVERHEAD + 100 + 200,
      "max_entries=2 must walk exactly the first two entries"
    );
    assert_eq!(
      walked_5,
      5 * SIDE_DATA_ENTRY_OVERHEAD + 100 + 200 + 300 + 400 + 500,
      "max_entries=5 must walk all five entries"
    );
    // max_entries=0 short-circuits to 0.
    assert_eq!(packet_side_data_bytes(&packet, 0), 0);
    // max_entries larger than the actual count clamps to the actual count
    // (no out-of-bounds walk past `side_data_elems`).
    let walked_huge = packet_side_data_bytes(&packet, 1_000_000);
    assert_eq!(walked_huge, walked_5);
  }

  /// `MAX_PROBE_PACKET_SIDE_DATA_ENTRIES` is the cliff above which a
  /// packet is rejected from the probe buffer regardless of byte total —
  /// pure descriptor inflation is its own attack vector. Sanity-check
  /// that `packet_side_data_count` reports the value the cap is checked
  /// against.
  #[test]
  fn packet_side_data_count_reports_attached_entries() {
    use ffmpeg_next::ffi::{AVPacketSideDataType, av_packet_new_side_data};

    let mut packet = Packet::new(0);
    let _p1 = unsafe {
      av_packet_new_side_data(
        packet.as_mut_ptr(),
        AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
        4,
      )
    };
    let _p2 = unsafe {
      av_packet_new_side_data(
        packet.as_mut_ptr(),
        AVPacketSideDataType::AV_PKT_DATA_PALETTE,
        4,
      )
    };
    assert_eq!(packet_side_data_count(&packet), 2);
  }

  /// `cpu_frame_bytes` must refuse to size a frame whose first plane has
  /// a negative `linesize`. Pre-fix, the loop break treated negative the
  /// same as zero (FFmpeg's "no more populated planes" sentinel), so a
  /// vertically-flipped frame returned `Some(0)` and `drain_into_pending`
  /// would queue it as a 0-byte allocation — letting up to
  /// `MAX_PROBE_PENDING_FRAMES` such frames bypass the configured byte
  /// budget entirely.
  #[test]
  fn cpu_frame_bytes_rejects_negative_first_plane_linesize() {
    let mut f = frame::Video::empty();
    // SAFETY: f is freshly allocated; we set `format` to NV12 and the
    // first plane's linesize negative (FFmpeg's vertical-flip convention).
    // No backing data buffer is allocated — cpu_frame_bytes must reject
    // before any pointer dereference.
    unsafe {
      let raw = f.as_mut_ptr();
      (*raw).format = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
      (*raw).width = 1920;
      (*raw).height = 1080;
      (*raw).linesize[0] = -1920;
      (*raw).linesize[1] = -1920;
    }
    assert!(
      cpu_frame_bytes(&f).is_none(),
      "negative linesize must be unsizeable, not Some(0)"
    );
  }

  /// Build a synthetic `AVHWFramesContext`-backed `AVBufferRef` for
  /// tests. The buffer's data is a zeroed `AVHWFramesContext` with only
  /// `width` and `height` populated — enough for [`hw_frames_ctx_dimensions`]
  /// / [`estimate_transfer_bytes`] to read the allocated dims.
  ///
  /// Returned ref has refcount 1; transfer ownership into
  /// `AVFrame.hw_frames_ctx` and let `av_frame_unref` (called by
  /// `frame::Video::Drop`) free it via `av_buffer_default_free`.
  fn make_hw_frames_ctx_ref(w: i32, h: i32) -> *mut ffmpeg_next::ffi::AVBufferRef {
    use ffmpeg_next::ffi::av_buffer_alloc;
    use std::mem::size_of;

    // SAFETY: `av_buffer_alloc(n)` returns a fresh `AVBufferRef` whose
    // `.data` points to `n` bytes of allocator-supplied storage. We
    // zero the AVHWFramesContext and write only `width` / `height`,
    // which is all the helpers we test read.
    unsafe {
      let buf = av_buffer_alloc(size_of::<AVHWFramesContext>());
      assert!(!buf.is_null(), "av_buffer_alloc returned NULL");
      let data = (*buf).data as *mut AVHWFramesContext;
      std::ptr::write_bytes(data, 0, 1);
      (*data).width = w;
      (*data).height = h;
      buf
    }
  }

  /// Sanity-check the positive path with a real allocation: an
  /// `av_buffer_alloc`'d 4096-byte plane attached as `buf[0]` must
  /// surface as `Some(4096)`.
  #[test]
  fn cpu_frame_bytes_sums_buf_sizes() {
    use ffmpeg_next::ffi::av_buffer_alloc;

    let mut f = frame::Video::empty();
    // SAFETY: av_buffer_alloc returns a fresh AVBufferRef. Attaching it
    // to AVFrame.buf[0] transfers ownership to the frame; av_frame_unref
    // on Drop releases it.
    let buf0 = unsafe { av_buffer_alloc(4096) };
    let buf1 = unsafe { av_buffer_alloc(2048) };
    assert!(!buf0.is_null() && !buf1.is_null());
    unsafe {
      let raw = f.as_mut_ptr();
      (*raw).buf[0] = buf0;
      (*raw).buf[1] = buf1;
      // Positive linesize so the negative-stride rejection doesn't fire.
      (*raw).linesize[0] = 256;
    }
    assert_eq!(cpu_frame_bytes(&f), Some(4096 + 2048));
  }

  /// A frame with no populated `buf` entries — the empty-frame state
  /// `Frame::empty()` produces — must return `Some(0)`. (Pre-fix this
  /// case was sized via the linesize×plane_height table; the new
  /// `buf[i].size` accounting handles it without a special branch.)
  #[test]
  fn cpu_frame_bytes_zero_for_empty_frame() {
    let f = frame::Video::empty();
    assert_eq!(cpu_frame_bytes(&f), Some(0));
  }

  /// `cpu_frame_bytes` must size against the underlying
  /// `AVBufferRef.size`, not `linesize × plane_height_for(AVFrame.height)`.
  /// On a cropped or heavily aligned stream the underlying buffer can
  /// be far larger than `AVFrame.height` (display) suggests — a
  /// height-based formula under-counts the allocation by
  /// `allocated_height / display_height` and lets the real
  /// allocation slip past `max_probe_pending_bytes`.
  ///
  /// Build a 256-byte buffer, attach it as `buf[0]`, but set
  /// `AVFrame.height` to 1 to simulate a cropped display. The
  /// `buf[i].size` accounting must report 256, not `linesize * 1`.
  #[test]
  fn cpu_frame_bytes_uses_buf_size_independent_of_display_height() {
    use ffmpeg_next::ffi::av_buffer_alloc;

    let buf0 = unsafe { av_buffer_alloc(256) };
    assert!(!buf0.is_null());

    let mut f = frame::Video::empty();
    unsafe {
      let raw = f.as_mut_ptr();
      (*raw).format = ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32;
      // Display dims tiny — pre-fix would have used `height = 1` to
      // size the plane and reported `linesize * 1` ≪ 256.
      (*raw).width = 1;
      (*raw).height = 1;
      (*raw).linesize[0] = 32;
      (*raw).buf[0] = buf0;
    }
    assert_eq!(
      cpu_frame_bytes(&f),
      Some(256),
      "cropped/aligned frames must be sized by buf[i].size, not display dims"
    );
  }

  /// `estimate_transfer_bytes` must read `hw_frames_ctx.width / .height`
  /// (allocated dims) — not `AVFrame.width / .height` (display dims).
  /// Verify with a synthetic frames context that disagrees with the
  /// frame's display dims by 80×.
  #[test]
  fn estimate_transfer_bytes_reads_alloc_dims_from_hw_frames_ctx() {
    let buf = make_hw_frames_ctx_ref(8192, 8192);
    let mut f = frame::Video::empty();
    unsafe {
      let raw = f.as_mut_ptr();
      // Display dims: 100×100 — pre-fix the estimate was 80 KB. After
      // the fix it must be 8192×8192×8 = 512 MiB.
      (*raw).width = 100;
      (*raw).height = 100;
      (*raw).hw_frames_ctx = buf;
    }
    assert_eq!(
      estimate_transfer_bytes(&f),
      Some(8192usize * 8192 * WORST_CASE_BYTES_PER_PIXEL),
    );
  }

  /// A frame with no `hw_frames_ctx` cannot have its allocation extent
  /// proved — the helper returns `None` so the probe-replay caller
  /// fails the candidate rather than under-counting from display dims.
  /// (This is the exact attack the cap is meant to prevent.)
  #[test]
  fn estimate_transfer_bytes_returns_none_without_hw_frames_ctx() {
    let mut f = frame::Video::empty();
    unsafe {
      let raw = f.as_mut_ptr();
      (*raw).width = 1920;
      (*raw).height = 1080;
      // hw_frames_ctx stays null.
    }
    assert!(estimate_transfer_bytes(&f).is_none());
  }

  /// Non-positive `hw_frames_ctx` dimensions also surface as `None` —
  /// a corrupt or malformed HW pool descriptor must not get a free
  /// pass.
  #[test]
  fn estimate_transfer_bytes_rejects_non_positive_alloc_dimensions() {
    let mut f = frame::Video::empty();
    let buf = make_hw_frames_ctx_ref(0, 1080);
    unsafe {
      (*f.as_mut_ptr()).hw_frames_ctx = buf;
    }
    assert!(estimate_transfer_bytes(&f).is_none());
  }

  /// 8K HDR P010 has actual ~96 MiB resident size; the estimate should
  /// over-charge it (the right side to err on for a memory cap) while
  /// still fitting within the configurable
  /// [`DEFAULT_MAX_PROBE_PENDING_BYTES`] cap (256 MiB) for a single
  /// frame so a default-configured decoder is not forced to reject 8K
  /// streams outright.
  #[test]
  fn estimate_transfer_bytes_8k_fits_default_cap() {
    let buf = make_hw_frames_ctx_ref(7680, 4320);
    let mut f = frame::Video::empty();
    unsafe {
      (*f.as_mut_ptr()).hw_frames_ctx = buf;
    }
    let estimate = estimate_transfer_bytes(&f).expect("8K is sizable");
    assert!(
      estimate <= DEFAULT_MAX_PROBE_PENDING_BYTES,
      "8K estimate {estimate} must fit DEFAULT_MAX_PROBE_PENDING_BYTES \
       {DEFAULT_MAX_PROBE_PENDING_BYTES}; otherwise the default cap rejects \
       even a single 8K frame at probe time"
    );
    assert!(
      estimate > 96 * 1024 * 1024,
      "estimate must over-charge real 8K P010 to bound the worst case; got {estimate}"
    );
  }

  /// `PartialBuildState`'s `Drop` must be a no-op when both pointers are
  /// null — the disarmed-by-`into_owned` post-state. A panic / double-free
  /// here would break the success path of every `build_state` call.
  #[test]
  fn partial_build_state_drop_is_no_op_on_null_pointers() {
    let _g = PartialBuildState {
      hw_device_ref: ptr::null_mut(),
      callback_state: ptr::null_mut(),
    };
    // Drops at end of scope. Test passes if it doesn't panic / crash.
  }

  /// `into_owned` must return the original pointers and disarm the guard
  /// (so the guard's Drop becomes a no-op and the caller can safely
  /// transfer ownership to `DecoderState` without double-freeing).
  #[test]
  fn partial_build_state_into_owned_disarms_and_returns_originals() {
    use ffmpeg_next::ffi::{AVPixelFormat, av_buffer_alloc, av_buffer_unref};

    // SAFETY: av_buffer_alloc returns a fresh AVBufferRef* with refcount
    // 1, or NULL on OOM. We free it ourselves below (after into_owned
    // disarms the guard).
    let hw_ptr = unsafe { av_buffer_alloc(64) };
    assert!(!hw_ptr.is_null(), "av_buffer_alloc(64) returned NULL");
    let cb_ptr = Box::into_raw(Box::new(CallbackState {
      wanted: AVPixelFormat::AV_PIX_FMT_NONE,
      wanted_int: AVPixelFormat::AV_PIX_FMT_NONE as i32,
    }));

    let g = PartialBuildState {
      hw_device_ref: hw_ptr,
      callback_state: cb_ptr,
    };
    let (hw_back, cb_back) = g.into_owned();
    assert_eq!(
      hw_back, hw_ptr,
      "into_owned must return the original device ref"
    );
    assert_eq!(
      cb_back, cb_ptr,
      "into_owned must return the original callback box"
    );

    // Guard is now disarmed (its Drop ran with null pointers as soon as
    // into_owned consumed it). We own the pointers and must free them.
    // SAFETY: hw_ptr and cb_ptr are still the freshly-allocated values.
    unsafe {
      let mut hw = hw_back;
      av_buffer_unref(&mut hw);
      drop(Box::from_raw(cb_back));
    }
  }

  /// `send_packet` must NOT consume the packet through the active
  /// decoder if the probe rescue cannot record it. The wrong order is
  /// `state.inner.send_packet → cap check → abandon probe → return
  /// Ok` — by the time the probe is abandoned the packet is already
  /// in FFmpeg's state but missing from `buffered_packets`, so a
  /// later runtime exhaustion would surface `unconsumed_packets`
  /// without that packet and a non-seekable caller could not rebuild
  /// the input stream.
  ///
  /// Post-fix the pre-flight runs first: cap overflow returns
  /// `Err(AllBackendsFailed)` *before* `state.inner.send_packet` is
  /// called, the packet stays in the caller's hand, and the rescue
  /// history is the consistent record up to (but not including) it.
  ///
  /// `pending_frames` are still preserved across the bailout — they
  /// belong to the active backend (possibly a candidate `advance_probe`
  /// just committed) and the caller can drain them via `receive_frame`
  /// before switching to software.
  ///
  /// Live HW required: a real `VideoDecoder` is the only way to
  /// construct a valid `DecoderState` (its `Drop` invokes FFmpeg
  /// cleanup).
  #[test]
  #[ignore = "requires HWDECODE_SAMPLE_VIDEO and a working hardware backend"]
  fn cap_overflow_does_not_consume_packet_and_preserves_pending() {
    use ffmpeg_next::{format, media};

    let path = std::env::var_os("HWDECODE_SAMPLE_VIDEO")
      .expect("HWDECODE_SAMPLE_VIDEO must be set for this test");

    ffmpeg_next::init().expect("ffmpeg init");
    let mut input = format::input(&path).expect("open input");
    let stream_index = input
      .streams()
      .best(media::Type::Video)
      .expect("video stream")
      .index();
    let stream_params = input
      .streams()
      .best(media::Type::Video)
      .expect("video stream")
      .parameters();

    let mut decoder = VideoDecoder::open(stream_params).expect("open decoder");
    assert!(
      decoder.probe.is_some(),
      "probe must be active immediately after open"
    );

    // Inject sentinel frames as if `advance_probe` had drained them from
    // a freshly-committed candidate during this same send_packet call.
    decoder.pending_frames.push_back(frame::Video::empty());
    decoder.pending_frames.push_back(frame::Video::empty());
    let pending_before = decoder.pending_frames.len();

    // Pre-stage one buffered packet so we can verify the rescue history
    // is returned unchanged (not silently extended with the triggering
    // packet, and not dropped). Sized to push the byte counter to its
    // ceiling so the very next send_packet trips the byte/packet cap.
    let pre_existing = Packet::new(8);
    decoder
      .probe
      .as_mut()
      .expect("probe present")
      .buffered_packets
      .push(pre_existing);
    decoder
      .probe
      .as_mut()
      .expect("probe present")
      .buffered_bytes = MAX_PROBE_PACKET_BYTES;

    // Find the first video packet and feed it. The pre-flight must
    // surface AllBackendsFailed; `state.inner.send_packet` must NOT be
    // called on this packet.
    let mut hit_bailout = false;
    for (s, packet) in input.packets() {
      if s.index() != stream_index {
        continue;
      }
      match decoder.send_packet(&packet) {
        Err(Error::AllBackendsFailed {
          attempts,
          unconsumed_packets,
        }) => {
          assert_eq!(
            unconsumed_packets.len(),
            1,
            "rescue history must contain the pre-existing packet only — \
             the triggering packet must NOT have been consumed"
          );
          assert_eq!(
            unconsumed_packets[0].size(),
            8,
            "the pre-existing packet must come back unmodified"
          );
          assert!(
            attempts.is_empty(),
            "no backend failure occurred; attempts must be empty when \
             bailout fires from cap overflow alone"
          );
          hit_bailout = true;
          break;
        }
        Ok(()) => panic!("send_packet must bail out when probe is at the byte cap"),
        Err(other) => panic!("expected AllBackendsFailed bailout, got {other:?}"),
      }
    }
    assert!(
      hit_bailout,
      "expected at least one send_packet to trip the cap-overflow bailout"
    );

    assert!(
      decoder.probe.is_none(),
      "probe must be abandoned after cap overflow"
    );
    assert_eq!(
      decoder.pending_frames.len(),
      pending_before,
      "pending_frames belong to the active backend; abandon must not drop them"
    );
  }

  /// When `advance_probe` exhausts the probe (no more candidates and
  /// the active backend just failed), the `Err(AllBackendsFailed
  /// { unconsumed_packets, .. })` it returns must include the
  /// packets the decoder has already consumed from the caller's
  /// demuxer. For non-seekable inputs (live streams, pipes, network
  /// sources), losing those packets means the caller's software
  /// fallback cannot replay the initial bytes and silently drops
  /// the leading frames.
  ///
  /// Live HW required: we need a real `VideoDecoder` (its `Drop` runs
  /// FFmpeg cleanup) and `advance_probe` is private — only callable
  /// from the same module.
  #[test]
  #[ignore = "requires HWDECODE_SAMPLE_VIDEO and a working hardware backend"]
  fn all_backends_failed_returns_buffered_packets_to_caller() {
    use ffmpeg_next::{format, media};

    let path = std::env::var_os("HWDECODE_SAMPLE_VIDEO")
      .expect("HWDECODE_SAMPLE_VIDEO must be set for this test");

    ffmpeg_next::init().expect("ffmpeg init");
    let input = format::input(&path).expect("open input");
    let stream_params = input
      .streams()
      .best(media::Type::Video)
      .expect("video stream")
      .parameters();

    let mut decoder = VideoDecoder::open(stream_params).expect("open decoder");
    assert!(
      decoder.probe.is_some(),
      "probe must be active immediately after open"
    );

    // Stuff the probe history with two distinct packets and clear the
    // remaining_backends list so the next advance_probe call is forced
    // into the exhaustion branch.
    let p1 = Packet::new(16);
    let p2 = Packet::new(32);
    {
      let probe = decoder.probe.as_mut().expect("probe");
      probe.buffered_packets.push(p1);
      probe.buffered_packets.push(p2);
      probe.remaining_backends.clear();
    }

    // Trigger advance_probe directly with a synthetic non-transient
    // error. The exhaustion branch must take ownership of the
    // buffered packets and surface them via `unconsumed_packets`.
    let result = decoder.advance_probe(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
    match result {
      Err(Error::AllBackendsFailed {
        attempts,
        unconsumed_packets,
      }) => {
        assert_eq!(
          unconsumed_packets.len(),
          2,
          "buffered probe packets must be returned to the caller for SW fallback"
        );
        assert_eq!(unconsumed_packets[0].size(), 16);
        assert_eq!(unconsumed_packets[1].size(), 32);
        // The synthetic InvalidData was recorded against the active
        // backend before the exhaustion check, so attempts is non-empty.
        assert!(
          !attempts.is_empty(),
          "the active backend's failure should be in attempts"
        );
      }
      other => panic!("expected AllBackendsFailed, got {other:?}"),
    }
  }

  /// `ProbeState.attempts` must carry forward `open`'s accumulated
  /// failures from earlier backends in probe order. The wrong
  /// shape — initialising `ProbeState.attempts` to `Vec::new()` at
  /// the start of `open`'s "promote to runtime" step — drops
  /// earlier failures so a runtime exhaustion surfaces an
  /// `AllBackendsFailed` whose `attempts` log only mentions the
  /// active backend's failure (e.g. VAAPI's earlier open failure
  /// goes missing).
  ///
  /// `open` seeds `ProbeState.attempts` with the local `attempts`
  /// vec via `mem::take`, so a runtime exhaustion surfaces the
  /// full failure chain in probe order.
  ///
  /// Live HW required: opens a real decoder, manually injects a
  /// synthetic earlier-backend failure into `probe.attempts` (as if
  /// `open` had recorded one), then triggers exhaustion via
  /// `advance_probe`. The synthetic earlier failure must appear
  /// before the active backend's failure in the returned `attempts`.
  #[test]
  #[ignore = "requires HWDECODE_SAMPLE_VIDEO and a working hardware backend"]
  fn all_backends_failed_preserves_earlier_open_failures() {
    use ffmpeg_next::{format, media};

    let path = std::env::var_os("HWDECODE_SAMPLE_VIDEO")
      .expect("HWDECODE_SAMPLE_VIDEO must be set for this test");

    ffmpeg_next::init().expect("ffmpeg init");
    let input = format::input(&path).expect("open input");
    let stream_params = input
      .streams()
      .best(media::Type::Video)
      .expect("video stream")
      .parameters();

    let mut decoder = VideoDecoder::open(stream_params).expect("open decoder");
    let active_backend = decoder.backend();

    // Pick a Backend distinct from the active one to simulate a prior
    // open failure that `open`'s seeding would have captured. We use
    // `BackendUnsupportedByCodec` as the synthetic earlier error since
    // it doesn't depend on FFmpeg state.
    //
    // Choose any Backend that isn't the active one. On macOS the only
    // backend is VideoToolbox, so we use a non-Apple backend
    // (Vaapi/Cuda/D3d11va) — its "supported by codec" status is
    // irrelevant; we're injecting the synthetic failure directly.
    let earlier_backend = match active_backend {
      Backend::VideoToolbox => Backend::Vaapi,
      Backend::Vaapi => Backend::Cuda,
      Backend::Cuda => Backend::Vaapi,
      Backend::D3d11va => Backend::Cuda,
    };
    let synthetic_earlier = Error::BackendUnsupportedByCodec(earlier_backend);

    // Seed attempts as `open` would have if backend 0 failed before
    // the active backend opened.
    {
      let probe = decoder.probe.as_mut().expect("probe present");
      probe
        .attempts
        .push((earlier_backend, Box::new(synthetic_earlier)));
      probe.remaining_backends.clear(); // force exhaustion on next advance.
    }

    let result = decoder.advance_probe(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
    match result {
      Err(Error::AllBackendsFailed { attempts, .. }) => {
        assert_eq!(
          attempts.len(),
          2,
          "AllBackendsFailed must surface BOTH the seeded earlier failure \
           and the active backend's runtime failure"
        );
        assert_eq!(
          attempts[0].0, earlier_backend,
          "earlier open failure must come first in probe order"
        );
        assert!(
          matches!(*attempts[0].1, Error::BackendUnsupportedByCodec(_)),
          "earlier failure must preserve its original error variant"
        );
        assert_eq!(
          attempts[1].0, active_backend,
          "active backend's runtime failure must come second"
        );
        assert!(
          matches!(
            *attempts[1].1,
            Error::Ffmpeg(ffmpeg_next::Error::InvalidData)
          ),
          "active backend's failure must preserve the synthetic InvalidData"
        );
      }
      other => panic!("expected AllBackendsFailed, got {other:?}"),
    }
  }
}

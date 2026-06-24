//! `mediadecode::VideoStreamDecoder` impl with HW + SW fallback.
//!
//! [`FfmpegVideoStreamDecoder`] starts on the hardware path: an inner
//! [`crate::VideoDecoder`] that auto-probes VideoToolbox / VAAPI /
//! NVDEC / D3D11VA. When every HW backend fails — at `open` time
//! (no backend opens) or mid-stream ([`crate::Error::AllBackendsFailed`]
//! from `send_packet` / `receive_frame` / `send_eof`) — we transparently
//! fall back to a **software** `ffmpeg::decoder::Video` opened from the
//! same `Parameters`.
//!
//! Two HW-exhaustion shapes feed the same fallback, distinguished by an
//! **explicit origin** the `AllBackendsFailed` carries
//! ([`crate::error::FallbackOrigin`]) — *not* by whether its rescued
//! `unconsumed_packets` is empty (both shapes can be empty: a probe-era
//! failure on the first packet has no prior history, exactly like every
//! post-commit failure):
//!
//! * **Probe-era** (pre-first-frame, [`crate::error::FallbackOrigin::Probe`]):
//!   the inner decoder buffered every packet it consumed and surfaces them in
//!   `unconsumed_packets`. We **replay exactly those** through the SW decoder
//!   (lossless — no frame was delivered yet), then route the still-unconsumed
//!   current packet (the one the inner decoder failed on / refused) to SW
//!   ourselves. This is the original pre-runtime-fallback behaviour and is
//!   unchanged.
//! * **Post-commit** (after the first frame, the inner probe is gone,
//!   [`crate::error::FallbackOrigin::PostCommit`]): a runtime HW-decode failure
//!   — e.g. VideoToolbox choking on H.264 High 4:2:2 10-bit — is reclassified
//!   to `AllBackendsFailed` by the inner decoder with an **empty**
//!   `unconsumed_packets` (the probe buffer no longer exists). Here we
//!   **degrade and continue** rather than reconstruct: open the SW decoder with
//!   an empty replay set and let it **resync at the next keyframe**. Fed forward
//!   packets from the failure point, the SW decoder naturally produces nothing
//!   until that keyframe, then decodes normally from there. The bounded span
//!   from the failure point to the next keyframe is dropped — an accepted,
//!   **loudly logged** gap (a single `tracing::warn!`), not a silent one. The
//!   indexing pipeline this serves prefers a small logged gap over the
//!   error-prone mid-stream-reconstruction state machine a lossless replay
//!   would require (see findit-studio/mediadecode#12). The *bounded*-ness is
//!   **enforced, not assumed**: a post-commit fallback enters a degraded-resync
//!   mode that holds until a **keyframe-anchored** resync — the SW decoder
//!   delivering a frame *after* a keyframe was fed to it across the gap. (Gating
//!   on a keyframe, not on *any* frame, matters because a lenient codec will
//!   decode a lone P-frame from the dropped span into a concealed frame; that
//!   must not count as a resync, or the one-GOP bound isn't truly enforced.) If
//!   EOF is reached while the mode is still pending — no keyframe ever arrived
//!   across the gap and the whole tail was lost — `receive_frame` escalates with
//!   a distinct [`VideoDecodeError::PostCommitNeverResynced`] (and a
//!   `tracing::error!`) rather than surfacing a clean end-of-stream that would
//!   swallow the tail silently. So the gap is either bounded-and-logged (a real
//!   keyframe resync happened) or reported-at-EOF (it never did) — never
//!   silent-and-unbounded.
//!
//!   The post-commit path retains and reconstructs **zero** frames: it opens SW
//!   cold, forwards only the failure arm's current packet (or EOF), and lets SW
//!   resync naturally. It never populates the replay-frame queue, so the
//!   replay/conversion machinery the probe-era path uses cannot touch it.
//!
//! The probe-era replay happens before the new packet (or the next
//! `receive_frame` poll) is processed, so a probe-era HW exhaustion on a
//! non-seekable input loses no compressed data. The post-commit path
//! intentionally accepts the next-keyframe gap.
//!
//! After the transition the decoder stays on SW for the rest of its
//! life — there's no probe-back-to-HW logic; once we've decided the
//! stream isn't HW-decodable, that decision is sticky.
//!
//! Frames produced by either path are converted via
//! [`crate::convert::av_frame_to_video_frame`] so the consumer sees
//! the same `mediadecode::VideoFrame<PixelFormat, VideoFrameExtra,
//! FfmpegBuffer>` shape regardless of which backend produced it.

use std::collections::VecDeque;

/// Maximum number of frames the SW fallback replay path will buffer
/// while draining the new SW decoder during packet/EOF replay.
/// Replaying many compressed packets through SW can produce hundreds
/// of decoded frames before the fallback commits; with no cap the
/// resident memory grows unbounded (e.g. 4K frames at ~12 MB each ×
/// 100s of frames). 64 frames is enough room to absorb every
/// realistic codec's reorder/lookahead window without becoming a
/// resource sink.
const SW_REPLAY_FRAME_CAP: usize = 64;

use ffmpeg_next::{Packet, codec::Parameters, frame};
use mediadecode::{Timebase, decoder::VideoStreamDecoder, frame::VideoFrame, packet::VideoPacket};

use crate::{
  Error, Ffmpeg, FfmpegBuffer, Frame, VideoDecoder, boundary,
  convert::{self, ConvertError},
  decoder::{build_codec_context, try_clone_parameters},
  error::FallbackFailed,
  extras::{VideoFrameExtra, VideoPacketExtra},
  frame::alloc_av_video_frame,
};

/// `mediadecode::VideoStreamDecoder` impl with transparent HW → SW
/// fallback.
pub struct FfmpegVideoStreamDecoder {
  state: DecodeState,
  /// Codec parameters retained so we can open a software
  /// `ffmpeg::decoder::Video` if the HW probe exhausts.
  parameters: Parameters,
  /// HW-side scratch frame (filled by [`VideoDecoder::receive_frame`]).
  hw_scratch: Frame,
  /// SW-side scratch frame (filled by `ffmpeg::decoder::Video::receive_frame`).
  sw_scratch: frame::Video,
  /// Frames produced while draining the SW decoder during fallback
  /// replay (see [`Self::fall_back_to_sw`]). The trait's
  /// `receive_frame` delivers from this queue before pulling new
  /// frames from the SW decoder. Empty in steady-state operation.
  sw_replay_frames: VecDeque<frame::Video>,
  /// `true` once `send_eof` has been called on the active decoder.
  /// Used to propagate EOF to the SW decoder when fallback fires
  /// during the drain phase — without this, codecs that hold tail
  /// frames at EOF would hang waiting for an EOF they already saw on
  /// the HW path.
  eof_sent: bool,
  /// `true` between a **post-commit** fallback firing and a *keyframe-anchored*
  /// resync (the SW decoder delivering a frame **after** a keyframe was fed to
  /// it across the gap). A post-commit fallback opens SW cold and drops the
  /// bounded span up to the next keyframe; the promise is that the span is
  /// *bounded* — SW resyncs at that keyframe. This flag makes the promise
  /// enforced rather than assumed: while it is set we have no proof SW ever
  /// recovered from a real keyframe. It is cleared only when SW delivers a frame
  /// *and* [`Self::degraded_keyframe_seen`] is set (a lone concealed P-frame a
  /// lenient codec emits from the gap does **not** clear it); if EOF is reached
  /// while it is still set the loss is escalated (a distinct loud error) rather
  /// than silently swallowing the whole tail. Probe-era fallbacks never set it —
  /// they replay losslessly and produce frames immediately.
  degraded_resync_pending: bool,
  /// `true` once a **keyframe** packet has been successfully fed to the SW
  /// decoder while [`Self::degraded_resync_pending`] is set — i.e. a real resync
  /// anchor crossed the gap. The pending flag clears only on a delivered SW
  /// frame *after* this is set, so a concealed non-keyframe frame (a lenient
  /// codec decoding a lone P-frame from the dropped span) cannot masquerade as a
  /// resync and prematurely clear the guard. Set alongside `enter`/cleared with
  /// the pending flag.
  degraded_keyframe_seen: bool,
  /// Packets fed to the SW decoder since the post-commit fallback fired while
  /// [`Self::degraded_resync_pending`] is set — i.e. across the unresolved
  /// resync gap. Reported in the escalation message so the lost span is
  /// quantified ("N packets, no keyframe found"). Reset whenever the flag
  /// clears or on `flush`.
  degraded_packets_since_fallback: u64,
  /// Source-stream time base, used to label produced frames.
  time_base: Timebase,
}

/// Hardware-decode seam behind [`DecodeState::Hw`]. In production this is
/// the real [`VideoDecoder`]; tests substitute a fake to drive the
/// post-commit fallback path without a live GPU. Mirrors the subset of
/// `VideoDecoder`'s surface the wrapper drives on the HW path.
pub(crate) trait HwInner: Send {
  /// See [`VideoDecoder::send_packet`].
  fn send_packet(&mut self, packet: &Packet) -> Result<(), Error>;
  /// See [`VideoDecoder::receive_frame`].
  fn receive_frame(&mut self, frame: &mut Frame) -> Result<(), Error>;
  /// See [`VideoDecoder::send_eof`].
  fn send_eof(&mut self) -> Result<(), Error>;
  /// See [`VideoDecoder::flush`]. Returns `Result` for a uniform seam even
  /// though the inherent method is infallible.
  fn flush(&mut self) -> Result<(), Error>;
  /// Downcast to the concrete [`VideoDecoder`] when this seam is the real
  /// HW decoder, so [`FfmpegVideoStreamDecoder::hardware_inner`] can keep
  /// exposing it. Returns `None` for a test fake.
  fn as_video_decoder(&self) -> Option<&VideoDecoder>;
}

impl HwInner for VideoDecoder {
  #[inline]
  fn send_packet(&mut self, packet: &Packet) -> Result<(), Error> {
    VideoDecoder::send_packet(self, packet)
  }
  #[inline]
  fn receive_frame(&mut self, frame: &mut Frame) -> Result<(), Error> {
    VideoDecoder::receive_frame(self, frame)
  }
  #[inline]
  fn send_eof(&mut self) -> Result<(), Error> {
    VideoDecoder::send_eof(self)
  }
  #[inline]
  fn flush(&mut self) -> Result<(), Error> {
    VideoDecoder::flush(self);
    Ok(())
  }
  #[inline]
  fn as_video_decoder(&self) -> Option<&VideoDecoder> {
    Some(self)
  }
}

/// Internal: which backend is currently driving the decode.
enum DecodeState {
  /// Hardware-backed decoder (auto-probe). May transition to `Sw` on
  /// `AllBackendsFailed`. Boxed behind [`HwInner`] so tests can inject a
  /// fake HW decoder.
  Hw(Box<dyn HwInner>),
  /// Software decoder. Terminal state.
  Sw(ffmpeg_next::decoder::Video),
}

/// What the cold SW decoder is fed on a **post-commit** degrade transition,
/// named by the failure arm so the three shapes stay mutually exclusive (a
/// current packet and EOF are never forwarded together). The post-commit path
/// retains no replay frames, so this is the *only* thing handed to the new SW
/// decoder at fallback time. See [`FfmpegVideoStreamDecoder::degrade_to_sw`].
enum PostCommitInput<'a> {
  /// `send_packet` arm: forward this current packet — the one the HW decoder
  /// refused (so it was never in any replay set). If it is a keyframe it is the
  /// resync anchor.
  Packet(&'a Packet),
  /// `receive_frame` arm: a frame-time failure has no current packet to forward.
  FrameTime,
  /// `send_eof` arm: EOF was pending on the HW path; re-forward it to the cold
  /// SW so tail-delaying codecs don't hang.
  Eof,
}

impl FfmpegVideoStreamDecoder {
  /// Opens a decoder for the given codec parameters with the default
  /// HW backend probe order. If the HW probe can't open any backend,
  /// falls back to a software `ffmpeg::decoder::Video` immediately —
  /// `open` only returns `Err` when both paths fail.
  ///
  /// Subsequent mid-stream `AllBackendsFailed` from the HW path
  /// triggers the same SW fallback (with rescued packets replayed).
  pub fn open(parameters: Parameters, time_base: Timebase) -> Result<Self, Error> {
    // ffmpeg-next's `Parameters` carries an optional `owner: Rc<dyn Any>`
    // (when constructed from `stream.parameters()` it points back at
    // the demuxer's `AVStream`). Upstream marks the type `Send`
    // anyway, which is unsound the moment a non-`None` owner is in
    // play — moving such a value across threads moves the `Rc`. We
    // sidestep this by always storing a deep-cloned `Parameters`
    // (`avcodec_parameters_copy` produces an owner-free copy), so
    // the `FfmpegVideoStreamDecoder`'s `Send` reachability never
    // depends on the caller's owner discipline.
    //
    // Use `try_clone_parameters` instead of `Parameters::clone` —
    // ffmpeg-next's `clone` calls `Parameters::new()` which can
    // return a `Parameters` whose inner pointer is null on OOM
    // (`avcodec_parameters_alloc` returns null without indication);
    // the subsequent `avcodec_parameters_copy` against that null
    // destination is C UB. Our checked helper surfaces the OOM as
    // an error instead.
    let owned_parameters = try_clone_parameters(&parameters).map_err(Error::Ffmpeg)?;
    let hw_scratch = Frame::empty()?;
    let sw_scratch = alloc_av_video_frame()?;
    let state =
      match VideoDecoder::open(try_clone_parameters(&owned_parameters).map_err(Error::Ffmpeg)?) {
        Ok(hw) => DecodeState::Hw(Box::new(hw)),
        Err(Error::AllBackendsFailed(_)) => {
          // Open-time HW exhaustion: no rescued packets (open didn't
          // see any). Just open SW directly from our owned copy.
          let sw = open_sw_decoder(&owned_parameters)?;
          DecodeState::Sw(sw)
        }
        Err(other) => return Err(other),
      };
    Ok(Self {
      state,
      parameters: owned_parameters,
      hw_scratch,
      sw_scratch,
      sw_replay_frames: VecDeque::new(),
      eof_sent: false,
      degraded_resync_pending: false,
      degraded_keyframe_seen: false,
      degraded_packets_since_fallback: 0,
      time_base,
    })
  }

  /// Returns `true` when this decoder has fallen back to the software
  /// path. `false` while still on the HW probe (the initial state).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn is_software(&self) -> bool {
    matches!(self.state, DecodeState::Sw(_))
  }

  /// Returns `true` while the HW probe is still active.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn is_hardware(&self) -> bool {
    matches!(self.state, DecodeState::Hw(_))
  }

  /// Borrow the inner [`VideoDecoder`] when this decoder is still on the
  /// real HW path. Returns `None` after the SW fallback has fired (or, in
  /// tests, when the HW seam is a fake rather than a real decoder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn hardware_inner(&self) -> Option<&VideoDecoder> {
    match &self.state {
      DecodeState::Hw(hw) => hw.as_video_decoder(),
      DecodeState::Sw(_) => None,
    }
  }

  /// Returns the time base associated with the source stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn time_base(&self) -> Timebase {
    self.time_base
  }

  /// Internal: **probe-era** transition from HW to SW. Replays the rescued
  /// packets (the inner decoder's buffered history, already accepted by the HW
  /// probe but not yet decoded) through the new SW decoder so the stream resumes
  /// seamlessly. No frame was delivered on the HW path yet, so replaying the
  /// history is lossless.
  ///
  /// Only the probe-era branches drive this. The **post-commit** path does
  /// *not* — it retains and reconstructs zero frames, opening SW cold via
  /// [`Self::degrade_to_sw`] and resyncing at the next keyframe instead of
  /// replaying. (That is why this method's replay/drain machinery — and the
  /// finding that the in-transaction drain doesn't cover later frame
  /// *conversion* — cannot affect the post-commit path: it never produces a
  /// post-commit replay frame to convert.)
  ///
  /// **Transactional**: drained replay frames accumulate in a local
  /// queue; we only commit them to `self.sw_replay_frames` and switch
  /// `self.state` to `Sw` after the replay (and EOF re-forwarding, if
  /// needed) succeed. On failure, the SW decoder, the local frame
  /// queue, and (where reachable) any consumed packets are dropped —
  /// `self` is left in its prior state.
  ///
  /// **EOF-aware**: when EOF was already accepted on the HW path
  /// (`self.eof_sent`), the new SW decoder also receives `send_eof()`
  /// after replay. Without this, codecs that delay tail frames hang
  /// forever in the drain phase.
  ///
  /// **EAGAIN-aware**: if SW's `send_packet` returns EAGAIN during
  /// replay, drain produced frames into the local queue and retry.
  ///
  /// `eof_pending` is passed as a **local** argument rather than read from
  /// `self.eof_sent`: callers must not mutate `self.eof_sent` before this
  /// transaction commits (see [`VideoStreamDecoder::send_eof`]), so the
  /// in-transaction SW EOF re-forward keys off the local flag and `self`'s
  /// EOF state is updated only after a clean commit.
  fn fall_back_to_sw(
    &mut self,
    unconsumed_packets: std::vec::Vec<ffmpeg_next::Packet>,
    eof_pending: bool,
  ) -> Result<(), Error> {
    tracing::info!(
      packets_replayed = unconsumed_packets.len(),
      eof_pending,
      "mediadecode-ffmpeg: HW probe exhausted, falling back to software decode",
    );
    // Wrap the internal worker so any failure path returns the
    // rescued packets to the caller via `Error::FallbackFailed`.
    // Without this, non-seekable streams (live feeds, pipes) would
    // lose every compressed byte the HW path had consumed when a
    // fallback transition fails partway.
    match self.fall_back_to_sw_inner(&unconsumed_packets, eof_pending) {
      Ok(()) => Ok(()),
      Err(source) => Err(Error::FallbackFailed(FallbackFailed::new(
        Box::new(source),
        unconsumed_packets,
      ))),
    }
  }

  /// Worker for [`Self::fall_back_to_sw`]. Returns the rescued packets
  /// untouched on the borrowed slice; the wrapper takes ownership of
  /// them and surfaces them in `FallbackFailed` if this returns Err.
  fn fall_back_to_sw_inner(
    &mut self,
    unconsumed_packets: &[ffmpeg_next::Packet],
    eof_pending: bool,
  ) -> Result<(), Error> {
    let mut sw = open_sw_decoder(&self.parameters)?;
    let mut local_replay: VecDeque<frame::Video> = VecDeque::new();
    // Helper: drain SW into the local replay queue, capped at
    // `SW_REPLAY_FRAME_CAP`.
    //
    // Error discipline: stop the drain **only** on the transient
    // backpressure signals EAGAIN / EOF (the decoder has no more output for
    // now). Every other `ffmpeg_next::Error` — e.g. `InvalidData` from a
    // corrupt replayed packet — is a real decode failure and is propagated,
    // so a non-recoverable error surfaces as `FallbackFailed` (carrying the
    // replay packets) instead of being silently swallowed and the fallback
    // committed over corruption.
    fn drain_into(
      sw: &mut ffmpeg_next::decoder::Video,
      local_replay: &mut VecDeque<frame::Video>,
    ) -> std::result::Result<(), Error> {
      loop {
        let mut tmp = alloc_av_video_frame()?;
        match sw.receive_frame(&mut tmp) {
          Ok(()) => {
            if local_replay.len() >= SW_REPLAY_FRAME_CAP {
              tracing::error!(
                cap = SW_REPLAY_FRAME_CAP,
                "mediadecode-ffmpeg: SW fallback replay produced more frames than the \
                 replay cap allows; aborting fallback (no frames dropped — they're \
                 still in the SW decoder's internal queue and will be released when \
                 it drops)",
              );
              return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
                errno: libc::ENOMEM,
              }));
            }
            local_replay.push_back(tmp);
          }
          // EAGAIN / EOF: no more output for now — stop draining, success.
          Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
            break;
          }
          Err(ffmpeg_next::Error::Eof) => break,
          // Any other error is a genuine decode failure on a replayed
          // packet — surface it so it is not masked as a clean fallback.
          Err(other) => return Err(Error::Ffmpeg(other)),
        }
      }
      Ok(())
    }

    for pkt in unconsumed_packets {
      let mut attempts: u32 = 0;
      loop {
        match sw.send_packet(pkt) {
          Ok(()) => break,
          Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
            drain_into(&mut sw, &mut local_replay)?;
            attempts += 1;
            if attempts > 16 {
              return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
                errno: ffmpeg_next::error::EAGAIN,
              }));
            }
          }
          Err(other) => return Err(Error::Ffmpeg(other)),
        }
      }
    }
    // Re-forward EOF if the HW path already saw it. SW EOF can also
    // return EAGAIN until prior output is drained — mirror the
    // packet-replay loop.
    if eof_pending {
      let mut attempts: u32 = 0;
      loop {
        match sw.send_eof() {
          Ok(()) => break,
          Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
            drain_into(&mut sw, &mut local_replay)?;
            attempts += 1;
            if attempts > 16 {
              return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
                errno: ffmpeg_next::error::EAGAIN,
              }));
            }
          }
          Err(other) => return Err(Error::Ffmpeg(other)),
        }
      }
    }
    // Final drain BEFORE commit — the transactional commit boundary. The
    // EAGAIN-triggered drains above only fire when SW exerts backpressure mid
    // replay; a SW decoder that ACCEPTS every replayed packet (and the EOF)
    // without one then surfaces a non-transient error — `InvalidData` from a
    // corrupt replayed packet, or any other decode failure — only on the *next*
    // `receive_frame`. Without this drain that error would land after the
    // commit (frames appended, `state` flipped to `Sw`, rescued packets
    // dropped) and reach the caller as a plain decode failure, not
    // `FallbackFailed` — breaking probe-era recovery on non-seekable input.
    // Draining to EAGAIN/EOF here forces any such error to surface now, so it is
    // wrapped as `FallbackFailed` (retaining the rescued packets) and the
    // decoder stays on HW — nothing is committed. (Only the probe-era path
    // reaches this; the post-commit path degrades via `degrade_to_sw` and never
    // replays, so it has no drained frames to commit or convert.)
    drain_into(&mut sw, &mut local_replay)?;
    // Commit: only after replay, any EOF forwarding, AND the final drain
    // succeeded do we move the new SW decoder and queue into `self`.
    self.sw_replay_frames.append(&mut local_replay);
    self.state = DecodeState::Sw(sw);
    Ok(())
  }

  /// **Post-commit** degrade-and-continue transition: open the SW decoder
  /// **cold** and forward only the failure-arm's input, retaining and
  /// reconstructing **zero** frames. This is the whole post-commit path: open
  /// SW, forward the current packet (or EOF), degrade-track — nothing is drained
  /// into `sw_replay_frames`, so there is no replayed frame to convert later and
  /// no terminal-drain transaction to reason about. SW naturally produces no
  /// frame until the next keyframe arrives across the gap, then decodes normally;
  /// the failure-point→next-keyframe span is the accepted, logged drop.
  ///
  /// **Transactional (SW-open only)**: `self.state` flips to `Sw` *only after*
  /// `open_sw_decoder` and the input forward succeed. On any failure the new SW
  /// decoder is dropped and the decoder is left on its prior HW state, the error
  /// surfaced as [`Error::FallbackFailed`] (with an empty rescue set — a
  /// post-commit failure never carries unconsumed packets). With no replay-frame
  /// retention there is nothing else to roll back.
  ///
  /// On a clean commit it enters degraded-resync mode (see
  /// [`Self::enter_degraded_resync`]); if the forwarded current packet is itself
  /// a keyframe, the resync anchor is recorded immediately
  /// ([`Self::note_degraded_keyframe`]).
  fn degrade_to_sw(&mut self, input: PostCommitInput<'_>) -> Result<(), Error> {
    match self.degrade_to_sw_inner(input) {
      Ok(()) => Ok(()),
      // Post-commit rescue is always empty: the probe buffer is gone, and we
      // retain no replay frames, so there are no packets to hand back.
      Err(source) => Err(Error::FallbackFailed(FallbackFailed::new(
        Box::new(source),
        std::vec::Vec::new(),
      ))),
    }
  }

  /// Worker for [`Self::degrade_to_sw`]. Opens SW cold, forwards the arm's input,
  /// and on success commits + enters degraded-resync mode. Returns `Err` (and
  /// commits nothing) if SW cannot open or the forward fails.
  fn degrade_to_sw_inner(&mut self, input: PostCommitInput<'_>) -> Result<(), Error> {
    let mut sw = open_sw_decoder(&self.parameters)?;
    let mut forwarded_keyframe = false;
    let mut forwarded_packet = false;
    match input {
      PostCommitInput::Packet(pkt) => {
        // The HW decoder REFUSED this packet, so it was never decoded; forward
        // it to the cold SW. A failure here surfaces (it is not silently
        // dropped) and rolls back to HW.
        sw.send_packet(pkt).map_err(Error::Ffmpeg)?;
        forwarded_keyframe = pkt.is_key();
        forwarded_packet = true;
      }
      // Frame-time failure: there is no current packet to forward.
      PostCommitInput::FrameTime => {}
      PostCommitInput::Eof => {
        // EOF was pending on the HW path; the cold SW must also see it so codecs
        // that delay tail frames don't hang. A cold decoder (no packets sent)
        // has no buffered output, so this cannot return EAGAIN.
        sw.send_eof().map_err(Error::Ffmpeg)?;
      }
    }
    // Commit: only after a clean open + forward.
    self.state = DecodeState::Sw(sw);
    self.enter_degraded_resync();
    if forwarded_keyframe {
      // The refused current packet was itself the resync anchor.
      self.note_degraded_keyframe(true);
    }
    if forwarded_packet {
      self.count_degraded_packet();
    }
    Ok(())
  }

  /// Enter post-commit degraded mode after a post-commit fallback commits: the
  /// SW decoder opened cold and the span up to the next keyframe is being
  /// dropped. We hold this mode until SW proves a *keyframe-anchored* resync
  /// (a delivered frame after a keyframe was fed — see
  /// [`Self::note_degraded_keyframe`] / [`Self::resync_on_frame`]) and the EOF
  /// escalation in [`VideoStreamDecoder::receive_frame`]. Called only on the
  /// post-commit path, only after a clean commit. Resets the keyframe-seen anchor
  /// and the gap counter.
  #[inline]
  fn enter_degraded_resync(&mut self) {
    self.degraded_resync_pending = true;
    self.degraded_keyframe_seen = false;
    self.degraded_packets_since_fallback = 0;
  }

  /// Record that a packet fed to the SW decoder across an unresolved post-commit
  /// gap was a **keyframe** — the resync anchor. Only a frame delivered *after*
  /// this clears the pending flag, so a lenient codec's concealed P-frame can't
  /// masquerade as a resync. A no-op outside degraded mode, or for a
  /// non-keyframe.
  #[inline]
  fn note_degraded_keyframe(&mut self, is_key: bool) {
    if self.degraded_resync_pending && is_key {
      self.degraded_keyframe_seen = true;
    }
  }

  /// Count one packet fed to the SW decoder while a post-commit resync is still
  /// unproven, so the EOF escalation can quantify the lost tail. A no-op once
  /// SW has resynced (the flag is clear).
  #[inline]
  fn count_degraded_packet(&mut self) {
    if self.degraded_resync_pending {
      self.degraded_packets_since_fallback = self.degraded_packets_since_fallback.saturating_add(1);
    }
  }

  /// A SW frame was delivered. Clear post-commit degraded mode **only if** a
  /// keyframe was fed across the gap ([`Self::degraded_keyframe_seen`]) — that is
  /// a real keyframe-anchored resync, so the dropped span is now the promised
  /// *bounded* gap. A frame delivered with no keyframe yet (a concealed P-frame
  /// from the dropped span) leaves the guard set, so the one-GOP bound stays
  /// enforced and the EOF escalation still fires if no keyframe ever arrives.
  /// Idempotent; a no-op outside degraded mode (steady state, probe-era replay).
  #[inline]
  fn resync_on_frame(&mut self) {
    if self.degraded_resync_pending && self.degraded_keyframe_seen {
      self.clear_degraded_resync();
    }
  }

  /// Unconditionally reset post-commit degraded-mode state. Used where the gap
  /// is moot regardless of resync proof: a `flush` (seek/reset re-anchors the
  /// stream) and the cleanup after an EOF escalation has already fired (so a
  /// follow-up poll sees plain EOF, not a repeated escalation). The
  /// frame-delivery path uses the keyframe-gated [`Self::resync_on_frame`]
  /// instead.
  #[inline]
  fn clear_degraded_resync(&mut self) {
    self.degraded_resync_pending = false;
    self.degraded_keyframe_seen = false;
    self.degraded_packets_since_fallback = 0;
  }

  /// Internal: convert the active scratch frame into a
  /// `mediadecode::VideoFrame` and write into `dst`.
  fn deliver_frame(
    &mut self,
    dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, FfmpegBuffer>,
  ) -> Result<(), VideoDecodeError> {
    let av_frame = match &mut self.state {
      DecodeState::Hw(_) => unsafe { self.hw_scratch.as_inner_mut().as_ptr() },
      DecodeState::Sw(_) => unsafe { self.sw_scratch.as_ptr() },
    };
    // SAFETY: the scratch frame is live (just filled by the inner
    // decoder's `receive_frame`); convert bumps refcounts on each
    // plane buffer it pulls into the produced VideoFrame so the
    // scratch can be reused on the next call.
    let new_frame = unsafe { convert::av_frame_to_video_frame(av_frame, self.time_base) }
      .map_err(VideoDecodeError::Convert)?;
    *dst = new_frame;
    Ok(())
  }
}

#[cfg(test)]
impl FfmpegVideoStreamDecoder {
  /// Build a decoder around an injected HW seam, bypassing the real probe.
  /// Lets tests drive the post-commit fallback path with a [`HwInner`] fake
  /// instead of a live GPU. The SW fallback still opens the **real**
  /// `ffmpeg::decoder::Video` from `parameters`, so a fallback in these tests
  /// genuinely decodes.
  pub(crate) fn from_hw_inner_for_test(
    hw: Box<dyn HwInner>,
    parameters: Parameters,
    time_base: Timebase,
  ) -> Result<Self, Error> {
    let owned_parameters = try_clone_parameters(&parameters).map_err(Error::Ffmpeg)?;
    Ok(Self {
      state: DecodeState::Hw(hw),
      parameters: owned_parameters,
      hw_scratch: Frame::empty()?,
      sw_scratch: alloc_av_video_frame()?,
      sw_replay_frames: VecDeque::new(),
      eof_sent: false,
      degraded_resync_pending: false,
      degraded_keyframe_seen: false,
      degraded_packets_since_fallback: 0,
      time_base,
    })
  }

  /// Whether `send_eof` has been committed on the active decoder. Lets the
  /// rollback tests assert that a failed EOF fallback restores (never
  /// half-mutates) `eof_sent`.
  pub(crate) const fn eof_sent_for_test(&self) -> bool {
    self.eof_sent
  }

  /// Whether a post-commit fallback is awaiting a keyframe-anchored resync.
  /// Lets the escalation tests observe the degraded-resync state machine.
  pub(crate) const fn degraded_resync_pending_for_test(&self) -> bool {
    self.degraded_resync_pending
  }

  /// Whether a keyframe has been fed to the SW decoder across the unresolved
  /// post-commit gap (the resync anchor). Lets the keyframe-gating test confirm
  /// a concealed P-frame does not set it (so the resync clear stays blocked).
  pub(crate) const fn degraded_keyframe_seen_for_test(&self) -> bool {
    self.degraded_keyframe_seen
  }

  /// Whether the post-commit path retained any replay frames — must always be
  /// empty for a post-commit fallback (it retains zero). Lets the finding-1
  /// dissolution test assert no replay frame was ever queued.
  pub(crate) fn sw_replay_frames_is_empty_for_test(&self) -> bool {
    self.sw_replay_frames.is_empty()
  }

  /// Packets fed to SW across an unresolved post-commit resync gap. Lets the
  /// counter test confirm packets crossing the gap from the `send_packet` arm
  /// are tallied (and cleared on resync).
  pub(crate) const fn degraded_packets_since_fallback_for_test(&self) -> u64 {
    self.degraded_packets_since_fallback
  }
}

impl VideoStreamDecoder for FfmpegVideoStreamDecoder {
  type Adapter = Ffmpeg;
  type Buffer = FfmpegBuffer;
  type Error = VideoDecodeError;

  fn send_packet(
    &mut self,
    packet: &VideoPacket<VideoPacketExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    let av_pkt = boundary::ffmpeg_packet_from_video_packet(packet)
      .map_err(|e| VideoDecodeError::Decode(Error::Ffmpeg(e)))?;
    match &mut self.state {
      DecodeState::Hw(hw) => match hw.send_packet(&av_pkt) {
        Ok(()) => Ok(()),
        Err(Error::AllBackendsFailed(p)) => {
          // Route on the EXPLICIT origin, never on whether `rescued` is empty (a
          // probe-era first-packet cap trip is *also* empty).
          if p.origin().is_post_commit() {
            // Post-commit: DEGRADE AND CONTINUE. No lossless mid-stream
            // reconstruction — the SW decoder opens cold, retains zero replay
            // frames, and resyncs at the next keyframe. The current packet (the
            // one HW REFUSED) is forwarded to that cold SW: if it is the resync
            // keyframe SW decodes from it, otherwise SW drops it until a keyframe
            // arrives. The bounded span from here to that keyframe is dropped — a
            // loudly logged gap (see the `warn!`), not a silent one.
            tracing::warn!(
              backend = ?p.attempts().last().map(|(b, _)| *b),
              pts = ?av_pkt.pts(),
              "mediadecode-ffmpeg: HW decode failed post-commit; falling back to \
               software, resyncing at next keyframe — a bounded span of frames \
               may be dropped at this boundary",
            );
            // Transactional SW-open + current-packet forward; degrade-tracking
            // (incl. keyframe-anchor recording) happens inside on a clean commit.
            // A failure surfaces `FallbackFailed` and stays on HW.
            return self
              .degrade_to_sw(PostCommitInput::Packet(&av_pkt))
              .map_err(VideoDecodeError::Decode);
          }
          // Probe-era: replay the inner decoder's buffered history (lossless —
          // no frame was delivered yet), then forward the still-unconsumed
          // current packet to SW.
          let rescued = p.into_unconsumed_packets();
          // `eof_pending` is the committed EOF state — never pre-mutated here.
          let eof_pending = self.eof_sent;
          self
            .fall_back_to_sw(rescued, eof_pending)
            .map_err(VideoDecodeError::Decode)?;
          // Forward the new (still-unconsumed) current packet to the
          // freshly-opened SW decoder — the HW decoder REFUSED it, so it was not
          // in the replay set. A failure here surfaces (it is not silently
          // dropped).
          if let DecodeState::Sw(sw) = &mut self.state {
            sw.send_packet(&av_pkt)
              .map_err(|e| VideoDecodeError::Decode(Error::Ffmpeg(e)))?;
          }
          Ok(())
        }
        Err(other) => Err(VideoDecodeError::Decode(other)),
      },
      DecodeState::Sw(sw) => {
        sw.send_packet(&av_pkt)
          .map_err(|e| VideoDecodeError::Decode(Error::Ffmpeg(e)))?;
        // A keyframe fed across an unresolved post-commit gap is the resync
        // anchor; record it so the next delivered frame can clear the guard.
        self.note_degraded_keyframe(av_pkt.is_key());
        // Count packets crossing an unresolved post-commit resync gap so the
        // escalation at EOF can report how much tail was lost.
        self.count_degraded_packet();
        Ok(())
      }
    }
  }

  fn receive_frame(
    &mut self,
    dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    // Deliver any frames produced during SW fallback replay before
    // pulling new ones from the SW decoder. This is the queue
    // populated by `fall_back_to_sw` when SW returned EAGAIN during
    // packet replay — a **probe-era** path only (the post-commit path retains
    // no replay frames), so `resync_on_frame` here is a no-op (probe-era never
    // enters degraded mode).
    if let Some(replayed) = self.sw_replay_frames.pop_front() {
      // SAFETY: `replayed` is a live AVFrame owned by us; convert
      // bumps refcounts on each plane buffer.
      let new_frame =
        unsafe { convert::av_frame_to_video_frame(replayed.as_ptr(), self.time_base) }
          .map_err(VideoDecodeError::Convert)?;
      self.resync_on_frame();
      *dst = new_frame;
      return Ok(());
    }
    loop {
      match &mut self.state {
        DecodeState::Hw(hw) => match hw.receive_frame(&mut self.hw_scratch) {
          Ok(()) => return self.deliver_frame(dst),
          Err(Error::AllBackendsFailed(p)) => {
            // HW exhausted at frame-time. There is no current packet here.
            // Route on the explicit origin.
            if p.origin().is_post_commit() {
              // Post-commit: DEGRADE AND CONTINUE — open SW cold (no current
              // packet to forward, no replay frames retained) and resync at the
              // next keyframe, dropping the bounded span up to it. Loud single
              // `warn!` marks that accepted gap. A clean commit enters degraded
              // mode; a SW-open failure surfaces `FallbackFailed` and stays HW.
              tracing::warn!(
                backend = ?p.attempts().last().map(|(b, _)| *b),
                "mediadecode-ffmpeg: HW decode failed post-commit at frame-time; \
                 falling back to software, resyncing at next keyframe — a bounded \
                 span of frames may be dropped at this boundary",
              );
              self
                .degrade_to_sw(PostCommitInput::FrameTime)
                .map_err(VideoDecodeError::Decode)?;
              // Nothing to deliver yet — fall through to the loop; the next
              // iteration takes the Sw arm and pulls from the cold SW decoder.
              continue;
            }
            // Probe-era: replay the buffered history (lossless).
            let rescued = p.into_unconsumed_packets();
            // `eof_pending` is the committed EOF state — never pre-mutated here.
            let eof_pending = self.eof_sent;
            self
              .fall_back_to_sw(rescued, eof_pending)
              .map_err(VideoDecodeError::Decode)?;
            // If the replay produced any drained frames, return one
            // immediately — preserves stream order vs. whatever the
            // SW decoder will produce next.
            if let Some(replayed) = self.sw_replay_frames.pop_front() {
              // SAFETY: `replayed` is a live AVFrame owned by us; convert bumps
              // refcounts on each plane buffer.
              let new_frame =
                unsafe { convert::av_frame_to_video_frame(replayed.as_ptr(), self.time_base) }
                  .map_err(VideoDecodeError::Convert)?;
              self.resync_on_frame();
              *dst = new_frame;
              return Ok(());
            }
            // Fall through to the loop; next iteration takes the Sw arm.
          }
          Err(other) => return Err(VideoDecodeError::Decode(other)),
        },
        DecodeState::Sw(sw) => {
          // Convert inline (rather than via `deliver_frame`, which borrows all
          // of `self`) so only the disjoint fields `sw_scratch` / `time_base`
          // are touched alongside the `self.state` borrow `sw` holds.
          match sw.receive_frame(&mut self.sw_scratch) {
            Ok(()) => {
              // SAFETY: the scratch frame is live (just filled by
              // `receive_frame`); convert bumps plane refcounts so the
              // scratch can be reused on the next call.
              let new_frame = unsafe {
                convert::av_frame_to_video_frame(self.sw_scratch.as_ptr(), self.time_base)
              }
              .map_err(VideoDecodeError::Convert)?;
              // SW produced a frame. Clear degraded mode only if a keyframe was
              // fed across the gap — a real keyframe-anchored resync, so the
              // dropped span is the promised bounded gap. A concealed P-frame
              // (no keyframe yet) does not clear it (see `resync_on_frame`).
              self.resync_on_frame();
              *dst = new_frame;
              return Ok(());
            }
            // EOF while a post-commit resync is still unproven: SW never emitted
            // a frame between the fallback and end-of-stream, so no keyframe
            // arrived across the gap and the ENTIRE tail was lost — not the
            // bounded span the degrade-and-continue path promises. Escalate
            // loudly with a distinct error instead of surfacing a clean `Eof`
            // that would silently swallow the tail. (Resync clears the flag, so
            // a normal degraded-then-recovered stream reaches EOF with the flag
            // already clear and takes the plain `Eof` path below.)
            Err(ffmpeg_next::Error::Eof) if self.degraded_resync_pending => {
              let packets_lost = self.degraded_packets_since_fallback;
              tracing::error!(
                packets_lost,
                "mediadecode-ffmpeg: post-commit HW->SW fallback never resynced before EOF — \
                 {packets_lost} packets fed to the software decoder produced no frame (no \
                 keyframe found across the gap); the stream tail from the fallback point was \
                 lost",
              );
              // Clear so a subsequent `receive_frame` poll (callers often drain
              // to EOF) sees plain EOF, not a repeated escalation.
              self.clear_degraded_resync();
              return Err(VideoDecodeError::PostCommitNeverResynced { packets_lost });
            }
            Err(e) => return Err(VideoDecodeError::Decode(Error::Ffmpeg(e))),
          }
        }
      }
    }
  }

  fn send_eof(&mut self) -> Result<(), Self::Error> {
    let outcome = match &mut self.state {
      DecodeState::Hw(hw) => match hw.send_eof() {
        Ok(()) => Ok(()),
        Err(Error::AllBackendsFailed(p)) => {
          // EOF is pending for this transaction, so the SW decoder must also
          // receive `send_eof` (codecs that delay tail frames hang otherwise).
          // We pass that intent locally rather than pre-setting `self.eof_sent`:
          // a fallback that fails returns `FallbackFailed` and stays on HW, and a
          // half-mutated `self.eof_sent = true` would then make a *later*
          // fallback inject an EOF into SW even though this `send_eof` errored.
          // `self.eof_sent` is committed only after the whole operation succeeds
          // (the `outcome` check below), keeping the fallback all-or-nothing.
          if p.origin().is_post_commit() {
            // Post-commit: DEGRADE AND CONTINUE — open SW cold, re-forward EOF
            // (no current packet, no replay frames). The cold SW produces no
            // frame from EOF alone, so the drain-to-EOF in `receive_frame`
            // escalates (`PostCommitNeverResynced`) unless a later keyframe-fed
            // poll resyncs first. A clean commit enters degraded mode; a SW-open
            // failure surfaces `FallbackFailed` and stays HW.
            tracing::warn!(
              backend = ?p.attempts().last().map(|(b, _)| *b),
              "mediadecode-ffmpeg: HW decode failed post-commit at EOF; falling \
               back to software — a bounded span of tail frames may be dropped",
            );
            self
              .degrade_to_sw(PostCommitInput::Eof)
              .map_err(VideoDecodeError::Decode)
          } else {
            // Probe-era: replay the buffered history (lossless), re-forwarding
            // EOF inside the transaction.
            let rescued = p.into_unconsumed_packets();
            self
              .fall_back_to_sw(rescued, true)
              .map_err(VideoDecodeError::Decode)
          }
        }
        Err(other) => Err(VideoDecodeError::Decode(other)),
      },
      DecodeState::Sw(sw) => sw
        .send_eof()
        .map_err(|e| VideoDecodeError::Decode(Error::Ffmpeg(e))),
    };
    // Commit EOF state only on success — a failed fallback left `self.eof_sent`
    // untouched (restored-by-construction: we never mutated it), so HW stays
    // EOF-not-yet-sent and a retry behaves correctly.
    if outcome.is_ok() {
      self.eof_sent = true;
    }
    outcome
  }

  fn flush(&mut self) -> Result<(), Self::Error> {
    // Drop any frames buffered during SW fallback replay before
    // flushing the inner decoder — otherwise a seek/reset would
    // surface stale pre-flush frames on the next `receive_frame`.
    self.sw_replay_frames.clear();
    // Flush ends the drain phase; the decoder accepts new packets
    // after this, so reset EOF tracking.
    self.eof_sent = false;
    // A flush (seek/reset) re-anchors the stream — any in-flight post-commit
    // resync tracking from before the flush is moot. Clear it so the next EOF
    // doesn't escalate over a now-irrelevant pre-flush gap.
    self.clear_degraded_resync();
    match &mut self.state {
      // The HW seam's `flush` returns `Result` for a uniform trait; the
      // real `VideoDecoder::flush` is infallible (always `Ok`).
      DecodeState::Hw(hw) => hw.flush().map_err(VideoDecodeError::Decode)?,
      DecodeState::Sw(sw) => sw.flush(),
    }
    Ok(())
  }
}

fn open_sw_decoder(parameters: &Parameters) -> Result<ffmpeg_next::decoder::Video, Error> {
  // Use the checked codec-context builder — ffmpeg-next's
  // `Context::from_parameters` calls `Context::new()` which doesn't
  // null-check `avcodec_alloc_context3`'s return value before
  // running `avcodec_parameters_to_context` against it. Under
  // memory pressure that's C-level UB; `build_codec_context`
  // surfaces the OOM as an error instead.
  let ctx = build_codec_context(parameters)?;
  ctx.decoder().video().map_err(Error::Ffmpeg)
}

/// Error type for [`FfmpegVideoStreamDecoder`].
#[derive(thiserror::Error, Debug)]
pub enum VideoDecodeError {
  /// The wrapped decoder (HW or SW) reported an error.
  #[error(transparent)]
  Decode(#[from] Error),
  /// Frame conversion from FFmpeg's native types to mediadecode's
  /// types failed.
  #[error(transparent)]
  Convert(#[from] ConvertError),
  /// A **post-commit** HW->SW fallback degraded the stream (dropping the
  /// bounded span up to the next keyframe) but the software decoder reached
  /// EOF without ever producing a frame — it never resynced, so the entire
  /// tail from the failure point was lost. The "bounded, logged gap" the
  /// post-commit path promises did not materialise (no keyframe arrived before
  /// EOF), so the loss is surfaced loudly here instead of being silently
  /// swallowed as a clean end-of-stream. `packets_lost` is the number of
  /// packets fed to SW across the unresolved gap.
  #[error(
    "post-commit HW->SW fallback never resynced before EOF: {packets_lost} packets fed to the \
     software decoder produced no frame (no keyframe found across the gap) — the stream tail \
     from the fallback point was lost"
  )]
  PostCommitNeverResynced {
    /// Packets fed to the software decoder across the unresolved resync gap.
    packets_lost: u64,
  },
}

#[cfg(test)]
mod tests;

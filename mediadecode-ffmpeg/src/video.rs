//! `mediadecode::VideoStreamDecoder` impl with HW + SW fallback.
//!
//! [`FfmpegVideoStreamDecoder`] starts on the hardware path: an inner
//! [`crate::VideoDecoder`] that auto-probes VideoToolbox / VAAPI /
//! NVDEC / D3D11VA. When every HW backend fails — at `open` time
//! (no backend opens) or mid-stream ([`crate::Error::AllBackendsFailed`]
//! from `send_packet` / `receive_frame` / `send_eof`) — we transparently
//! fall back to a **software** `ffmpeg::decoder::Video` opened from the
//! same `Parameters`. The rescued `unconsumed_packets` from the HW
//! probe are replayed through the SW decoder before the new packet (or
//! the next `receive_frame` poll) is processed, so non-seekable inputs
//! survive a mid-stream HW exhaustion without losing any compressed
//! data.
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

use ffmpeg_next::{codec::Parameters, frame};
use mediadecode::{Timebase, decoder::VideoStreamDecoder, frame::VideoFrame, packet::VideoPacket};

use crate::{
  Error, Ffmpeg, FfmpegBuffer, Frame, VideoDecoder, boundary,
  convert::{self, ConvertError},
  decoder::{build_codec_context, try_clone_parameters},
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
  /// Source-stream time base, used to label produced frames.
  time_base: Timebase,
}

/// Internal: which backend is currently driving the decode.
enum DecodeState {
  /// Hardware-backed decoder (auto-probe). May transition to `Sw` on
  /// `AllBackendsFailed`.
  Hw(VideoDecoder),
  /// Software decoder. Terminal state.
  Sw(ffmpeg_next::decoder::Video),
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
        Ok(hw) => DecodeState::Hw(hw),
        Err(Error::AllBackendsFailed { .. }) => {
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

  /// Borrow the inner [`VideoDecoder`] when this decoder is still on
  /// the HW path. Returns `None` after the SW fallback has fired.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn hardware_inner(&self) -> Option<&VideoDecoder> {
    match &self.state {
      DecodeState::Hw(hw) => Some(hw),
      DecodeState::Sw(_) => None,
    }
  }

  /// Returns the time base associated with the source stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn time_base(&self) -> Timebase {
    self.time_base
  }

  /// Internal: transition from HW to SW. Replays the rescued packets
  /// (already accepted by the HW probe but not yet decoded) through
  /// the new SW decoder so the stream resumes seamlessly.
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
  fn fall_back_to_sw(
    &mut self,
    unconsumed_packets: std::vec::Vec<ffmpeg_next::Packet>,
  ) -> Result<(), Error> {
    tracing::info!(
      packets_replayed = unconsumed_packets.len(),
      eof_pending = self.eof_sent,
      "mediadecode-ffmpeg: HW probe exhausted, falling back to software decode",
    );
    // Wrap the internal worker so any failure path returns the
    // rescued packets to the caller via `Error::FallbackFailed`.
    // Without this, non-seekable streams (live feeds, pipes) would
    // lose every compressed byte the HW path had consumed when a
    // fallback transition fails partway.
    match self.fall_back_to_sw_inner(&unconsumed_packets) {
      Ok(()) => Ok(()),
      Err(source) => Err(Error::FallbackFailed {
        source: Box::new(source),
        unconsumed_packets,
      }),
    }
  }

  /// Worker for [`Self::fall_back_to_sw`]. Returns the rescued packets
  /// untouched on the borrowed slice; the wrapper takes ownership of
  /// them and surfaces them in `FallbackFailed` if this returns Err.
  fn fall_back_to_sw_inner(
    &mut self,
    unconsumed_packets: &[ffmpeg_next::Packet],
  ) -> Result<(), Error> {
    let mut sw = open_sw_decoder(&self.parameters)?;
    let mut local_replay: VecDeque<frame::Video> = VecDeque::new();
    // Helper: drain SW into the local replay queue, capped at
    // `SW_REPLAY_FRAME_CAP`. Returns an error when the cap is
    // exceeded — the fallback caller treats this as a non-recoverable
    // failure rather than silently dropping decoded frames.
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
          Err(_) => break,
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
    if self.eof_sent {
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
    // Commit: only after replay (and any EOF forwarding) succeeded
    // do we move the new SW decoder and queue into `self`.
    self.sw_replay_frames.append(&mut local_replay);
    self.state = DecodeState::Sw(sw);
    Ok(())
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
        Err(Error::AllBackendsFailed {
          unconsumed_packets, ..
        }) => {
          self
            .fall_back_to_sw(unconsumed_packets)
            .map_err(VideoDecodeError::Decode)?;
          // Now route the *new* packet to the freshly-opened SW
          // decoder — the rescued packets were already replayed.
          if let DecodeState::Sw(sw) = &mut self.state {
            sw.send_packet(&av_pkt)
              .map_err(|e| VideoDecodeError::Decode(Error::Ffmpeg(e)))?;
          }
          Ok(())
        }
        Err(other) => Err(VideoDecodeError::Decode(other)),
      },
      DecodeState::Sw(sw) => sw
        .send_packet(&av_pkt)
        .map_err(|e| VideoDecodeError::Decode(Error::Ffmpeg(e))),
    }
  }

  fn receive_frame(
    &mut self,
    dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    // Deliver any frames produced during SW fallback replay before
    // pulling new ones from the SW decoder. This is the queue
    // populated by `fall_back_to_sw` when SW returned EAGAIN during
    // packet replay.
    if let Some(replayed) = self.sw_replay_frames.pop_front() {
      // SAFETY: `replayed` is a live AVFrame owned by us; convert
      // bumps refcounts on each plane buffer.
      let new_frame =
        unsafe { convert::av_frame_to_video_frame(replayed.as_ptr(), self.time_base) }
          .map_err(VideoDecodeError::Convert)?;
      *dst = new_frame;
      return Ok(());
    }
    loop {
      match &mut self.state {
        DecodeState::Hw(hw) => match hw.receive_frame(&mut self.hw_scratch) {
          Ok(()) => return self.deliver_frame(dst),
          Err(Error::AllBackendsFailed {
            unconsumed_packets, ..
          }) => {
            // Probe exhausted at frame-time. Open SW, replay packets,
            // loop back so the SW path tries to receive_frame.
            self
              .fall_back_to_sw(unconsumed_packets)
              .map_err(VideoDecodeError::Decode)?;
            // If the replay produced any drained frames, return one
            // immediately — preserves stream order vs. whatever the
            // SW decoder will produce next.
            if let Some(replayed) = self.sw_replay_frames.pop_front() {
              // SAFETY: see above.
              let new_frame =
                unsafe { convert::av_frame_to_video_frame(replayed.as_ptr(), self.time_base) }
                  .map_err(VideoDecodeError::Convert)?;
              *dst = new_frame;
              return Ok(());
            }
            // Fall through to the loop; next iteration takes the Sw arm.
          }
          Err(other) => return Err(VideoDecodeError::Decode(other)),
        },
        DecodeState::Sw(sw) => {
          return match sw.receive_frame(&mut self.sw_scratch) {
            Ok(()) => self.deliver_frame(dst),
            Err(e) => Err(VideoDecodeError::Decode(Error::Ffmpeg(e))),
          };
        }
      }
    }
  }

  fn send_eof(&mut self) -> Result<(), Self::Error> {
    let outcome = match &mut self.state {
      DecodeState::Hw(hw) => match hw.send_eof() {
        Ok(()) => Ok(()),
        Err(Error::AllBackendsFailed {
          unconsumed_packets, ..
        }) => {
          // Mark EOF as already accepted *before* fallback so that
          // `fall_back_to_sw` forwards it to the new SW decoder
          // transactionally — packet replay, EOF replay, and the
          // bounded EAGAIN-drain retry all happen inside the
          // FallbackFailed-wrapping helper. Without this flag set
          // first, the SW EOF would happen outside the transaction
          // and a failure there would lose the rescued packets.
          self.eof_sent = true;
          self
            .fall_back_to_sw(unconsumed_packets)
            .map_err(VideoDecodeError::Decode)?;
          Ok(())
        }
        Err(other) => Err(VideoDecodeError::Decode(other)),
      },
      DecodeState::Sw(sw) => sw
        .send_eof()
        .map_err(|e| VideoDecodeError::Decode(Error::Ffmpeg(e))),
    };
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
    match &mut self.state {
      DecodeState::Hw(hw) => hw.flush(),
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
}

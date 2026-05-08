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

use ffmpeg_next::codec::Parameters;
use ffmpeg_next::frame;
use mediadecode::{
  Timebase, decoder::VideoStreamDecoder, frame::VideoFrame, packet::VideoPacket,
};

use crate::{
  Error, Ffmpeg, FfmpegBuffer, Frame, VideoDecoder, boundary, convert,
  extras::{VideoFrameExtra, VideoPacketExtra},
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
    let hw_scratch = Frame::empty()?;
    let sw_scratch = frame::Video::empty();
    let state = match VideoDecoder::open(parameters.clone()) {
      Ok(hw) => DecodeState::Hw(hw),
      Err(Error::AllBackendsFailed { .. }) => {
        // Open-time HW exhaustion: no rescued packets (open didn't
        // see any). Just open SW directly.
        let sw = open_sw_decoder(&parameters)?;
        DecodeState::Sw(sw)
      }
      Err(other) => return Err(other),
    };
    Ok(Self {
      state,
      parameters,
      hw_scratch,
      sw_scratch,
      time_base,
    })
  }

  /// Returns `true` when this decoder has fallen back to the software
  /// path. `false` while still on the HW probe (the initial state).
  pub fn is_software(&self) -> bool {
    matches!(self.state, DecodeState::Sw(_))
  }

  /// Returns `true` while the HW probe is still active.
  pub fn is_hardware(&self) -> bool {
    matches!(self.state, DecodeState::Hw(_))
  }

  /// Borrow the inner [`VideoDecoder`] when this decoder is still on
  /// the HW path. Returns `None` after the SW fallback has fired.
  pub fn hardware_inner(&self) -> Option<&VideoDecoder> {
    match &self.state {
      DecodeState::Hw(hw) => Some(hw),
      DecodeState::Sw(_) => None,
    }
  }

  /// Returns the time base associated with the source stream.
  pub fn time_base(&self) -> Timebase {
    self.time_base
  }

  /// Internal: transition from HW to SW. Replays the rescued packets
  /// (already accepted by the HW probe but not yet decoded) through
  /// the new SW decoder so the stream resumes seamlessly.
  fn fall_back_to_sw(
    &mut self,
    unconsumed_packets: std::vec::Vec<ffmpeg_next::Packet>,
  ) -> Result<(), Error> {
    tracing::info!(
      packets_replayed = unconsumed_packets.len(),
      "mediadecode-ffmpeg: HW probe exhausted, falling back to software decode",
    );
    let mut sw = open_sw_decoder(&self.parameters)?;
    for pkt in &unconsumed_packets {
      // We forward each rescued packet to SW. Errors from individual
      // replays are surfaced — the caller can branch on them, but in
      // practice if the SW decoder rejects a packet the stream is
      // unusable anyway.
      sw.send_packet(pkt).map_err(Error::Ffmpeg)?;
    }
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
    let av_pkt = boundary::ffmpeg_packet_from_video_packet(packet);
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
    dst: &mut VideoFrame<
      mediadecode::PixelFormat,
      VideoFrameExtra,
      Self::Buffer,
    >,
  ) -> Result<(), Self::Error> {
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
    match &mut self.state {
      DecodeState::Hw(hw) => match hw.send_eof() {
        Ok(()) => Ok(()),
        Err(Error::AllBackendsFailed {
          unconsumed_packets, ..
        }) => {
          self
            .fall_back_to_sw(unconsumed_packets)
            .map_err(VideoDecodeError::Decode)?;
          if let DecodeState::Sw(sw) = &mut self.state {
            sw.send_eof()
              .map_err(|e| VideoDecodeError::Decode(Error::Ffmpeg(e)))?;
          }
          Ok(())
        }
        Err(other) => Err(VideoDecodeError::Decode(other)),
      },
      DecodeState::Sw(sw) => sw
        .send_eof()
        .map_err(|e| VideoDecodeError::Decode(Error::Ffmpeg(e))),
    }
  }

  fn flush(&mut self) -> Result<(), Self::Error> {
    match &mut self.state {
      DecodeState::Hw(hw) => {
        hw.flush();
        Ok(())
      }
      DecodeState::Sw(sw) => {
        sw.flush();
        Ok(())
      }
    }
  }
}

fn open_sw_decoder(parameters: &Parameters) -> Result<ffmpeg_next::decoder::Video, Error> {
  let ctx =
    ffmpeg_next::codec::Context::from_parameters(parameters.clone()).map_err(Error::Ffmpeg)?;
  ctx.decoder().video().map_err(Error::Ffmpeg)
}

/// Error type for [`FfmpegVideoStreamDecoder`].
#[derive(thiserror::Error, Debug)]
pub enum VideoDecodeError {
  /// The wrapped decoder (HW or SW) reported an error.
  #[error("{0}")]
  Decode(#[from] Error),
  /// Frame conversion from FFmpeg's native types to mediadecode's
  /// types failed.
  #[error("frame conversion failed: {0}")]
  Convert(crate::convert::ConvertError),
}

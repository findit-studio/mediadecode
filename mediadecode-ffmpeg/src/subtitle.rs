//! `mediadecode::SubtitleDecoder` impl backed by
//! `ffmpeg::decoder::Subtitle`.
//!
//! Subtitles use FFmpeg's legacy synchronous `decode()` API rather
//! than `send_packet`/`receive_frame`. We bridge the difference by
//! converting the produced `AVSubtitle` into a
//! [`mediadecode::SubtitleFrame`] inside [`SubtitleDecoder::send_packet`]
//! and stashing it in `pending` for the next [`SubtitleDecoder::receive_frame`]
//! call. This matches the trait's contract: `send_packet` enqueues
//! work, `receive_frame` drains one decoded frame at a time, and
//! `NoFrameReady` is signalled via [`SubtitleDecodeError::NoFrameReady`].

use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::{codec::Parameters, ffi::avsubtitle_free};
use mediadecode::{
  Timebase, decoder::SubtitleDecoder, frame::SubtitleFrame, packet::SubtitlePacket,
};

use crate::{
  DecoderLimits, Error, Ffmpeg, boundary,
  convert::{self, ConvertError},
  decoder::build_codec_context,
  extras::{SubtitleFrameExtra, SubtitlePacketExtra},
};

/// RAII wrapper that owns an `ffmpeg_next::Subtitle` scratch slot and
/// frees the FFmpeg-side rect allocations on drop / explicit `clear`.
///
/// `ffmpeg::Subtitle::new()` zero-initializes; `decoder.decode()` may
/// allocate per-rect storage (`AVSubtitleRect.text` / `.ass` /
/// `.data[0]` / `.data[1]`) which only `avsubtitle_free` releases.
/// Without this wrapper, every successful decode leaks until the
/// decoder drops.
struct ScratchSubtitle {
  inner: ffmpeg_next::Subtitle,
}

impl ScratchSubtitle {
  fn new() -> Self {
    Self {
      inner: ffmpeg_next::Subtitle::new(),
    }
  }

  fn clear(&mut self) {
    // SAFETY: `inner` holds a valid AVSubtitle (zero-initialized or
    // populated by `decode`). `avsubtitle_free` frees the rect array
    // and per-rect allocations, then leaves the struct in a state
    // suitable for reuse by the next decode call.
    unsafe { avsubtitle_free(self.inner.as_mut_ptr()) };
  }
}

impl Drop for ScratchSubtitle {
  fn drop(&mut self) {
    self.clear();
  }
}

/// `mediadecode::SubtitleDecoder` impl wrapping `ffmpeg::decoder::Subtitle`.
///
/// Subtitle decoders are stateless from FFmpeg's perspective — each
/// `decode()` call consumes one packet and produces zero-or-one
/// `AVSubtitle`. The pending-frame buffer here is a one-slot queue
/// so the trait's `send_packet` / `receive_frame` split works.
pub struct CarrierSubtitleStreamDecoder<C: crate::FfmpegCarrier> {
  decoder: ffmpeg_next::decoder::Subtitle,
  scratch: ScratchSubtitle,
  /// `true` when [`Self::scratch`] holds a decoded `AVSubtitle` that
  /// has not been converted and delivered.
  ///
  /// **The conversion happens on the receive side, and that is the
  /// point.** `avcodec_decode_subtitle2` consumes the packet: once it
  /// has answered, the cue exists only in this scratch, and nothing
  /// re-offers it. Converting inside `send_packet` meant an allocation
  /// that failed took the cue with it — the scratch was freed, the
  /// error returned, and the caller's next packet decoded the *next*
  /// cue. Deferring the conversion to `receive_frame` gives it a seat
  /// to fail into: the scratch is cleared when a carrier exists for it,
  /// not before.
  ///
  /// The same shape the audio and video decoders keep for a decoded
  /// `AVFrame`, and the same discipline `flush` clears.
  scratch_pending: bool,
  time_base: Timebase,
  /// Retained, not discarded at open: the send path judges
  /// [`DecoderLimits::max_packet_bytes`] against every packet it
  /// rebuilds into an `AVPacket`.
  limits: DecoderLimits,
  /// Keeps the [`CallbackState`](crate::ffi::CallbackState) alive for as
  /// long as the codec context that points at it.
  ///
  /// Declared **after** the decoder on purpose: struct fields drop in
  /// declaration order, so the `AVCodecContext` is freed first and the
  /// state it references outlives it.
  _callback_state: Box<crate::ffi::CallbackState>,
  /// The lane this decoder captures into. A marker: the carrier
  /// appears in the frames it produces, not in its own state.
  _carrier: core::marker::PhantomData<C>,
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierSubtitleStreamDecoder<C> {
  /// Opens a subtitle decoder for the given codec parameters.
  ///
  /// `limits` reaches the `AVCodecContext` this call opens. A subtitle
  /// decoder produces no pixels, so the pixel half never fires here —
  /// it is passed for one reason: every decoder this crate opens gets
  /// the same ceiling written into it, and a seam that skipped one
  /// would be a seam somebody has to remember.
  pub(crate) fn open_impl(
    parameters: Parameters,
    time_base: Timebase,
    limits: DecoderLimits,
  ) -> Result<Self, SubtitleDecodeError> {
    // Use the checked codec-context builder — `Context::from_parameters`
    // is OOM-UB-prone (see `crate::decoder::build_codec_context`).
    let (ctx, callback_state) =
      build_codec_context(&parameters, limits).map_err(SubtitleDecodeError::Decode)?;
    // Opened without forming a bindgen enum from FFmpeg memory: the codec
    // is resolved off a raw `codec_id`, and the medium is proved off a raw
    // `codec_type`. See `crate::decoder::ensure_codec_type`.
    let codec = crate::decoder::find_decoder(&parameters).map_err(SubtitleDecodeError::Decode)?;
    let opened = ctx
      .decoder()
      .open_as(codec)
      .map_err(|e| SubtitleDecodeError::Decode(Error::Ffmpeg(e)))?;
    crate::decoder::ensure_codec_type(
      &opened,
      ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE,
    )
    .map_err(SubtitleDecodeError::Decode)?;
    let decoder = ffmpeg_next::decoder::Subtitle(opened);
    Ok(Self {
      decoder,
      scratch: ScratchSubtitle::new(),
      scratch_pending: false,
      time_base,
      limits,
      _callback_state: callback_state,
      _carrier: core::marker::PhantomData,
    })
  }

  /// Returns the time base associated with the source stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn time_base_impl(&self) -> Timebase {
    self.time_base
  }

  /// The ceilings this decoder was opened with.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn limits_impl(&self) -> DecoderLimits {
    self.limits
  }

  /// Borrow the wrapped `ffmpeg::decoder::Subtitle`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn inner_impl(&self) -> &ffmpeg_next::decoder::Subtitle {
    &self.decoder
  }
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierSubtitleStreamDecoder<C> {
  pub(crate) fn send_packet_impl(
    &mut self,
    packet: &SubtitlePacket<SubtitlePacketExtra, C::Buffer>,
  ) -> Result<(), SubtitleDecodeError> {
    // Disallow sending while a previously-decoded frame hasn't been
    // drained yet. The legacy `decode()` API produces a frame inline,
    // so a second send would silently drop the first — surface that
    // as an error so callers notice the drain ordering.
    if self.scratch_pending {
      return Err(SubtitleDecodeError::FramePending);
    }
    // Free any allocations from a previous decode before reusing the
    // scratch — avoids leaking when the previous packet produced no
    // frame (got == false, which still mutates the struct).
    self.scratch.clear();
    // Scoped submission — see `boundary::with_ffmpeg_subtitle_packet`.
    let state: *const crate::ffi::CallbackState = &*self._callback_state;
    let decoder = &mut self.decoder;
    let scratch = &mut self.scratch.inner;
    let got = boundary::with_ffmpeg_subtitle_packet::<C, _>(
      packet,
      self.limits.packet_limits(),
      // Nothing on this road records what it is sent, so the packet
      // really does die inside the call and its body may be shared.
      crate::carrier::BodyRoute::Submission,
      |av_pkt| {
        decoder.decode(av_pkt, scratch).map_err(|e| {
          // SAFETY: the callback state outlives this decoder.
          SubtitleDecodeError::Decode(crate::decoder::software_exit(unsafe { &*state }, e))
        })
      },
    )
    .map_err(|e| SubtitleDecodeError::Decode(Error::PacketBuild(e)))??;
    // The cue stays in the scratch until a carrier exists for it — see
    // [`Self::scratch_pending`]. Nothing is converted here.
    self.scratch_pending = got;
    Ok(())
  }

  pub(crate) fn receive_frame_impl(
    &mut self,
    dst: &mut SubtitleFrame<SubtitleFrameExtra, C::Buffer>,
  ) -> Result<(), SubtitleDecodeError> {
    if !self.scratch_pending {
      return Err(SubtitleDecodeError::NoFrameReady);
    }
    // SAFETY: `scratch.inner` is a live `AVSubtitle` filled by the
    // decode this seat is holding. Conversion copies every rect it
    // takes — `AVSubtitleRect` has no refcounted buffer, so both lanes
    // copy — and the FFmpeg-side allocations are released below, once
    // there is something to release them in favour of.
    let converted = unsafe {
      convert::av_subtitle_to_subtitle_frame_as::<C>(self.scratch.inner.as_ptr(), self.time_base)
    };
    match converted {
      Ok(frame) => {
        self.scratch.clear();
        self.scratch_pending = false;
        *dst = frame;
        Ok(())
      }
      Err(e) if e.parks_in_decode() => {
        // Kept: another attempt could carry this cue, and there is no
        // other copy of it anywhere.
        Err(SubtitleDecodeError::Convert(e))
      }
      Err(e) => {
        // A cue nothing can carry is let go — freed immediately, so a
        // caller that ignores the error cannot leave the scratch
        // holding FFmpeg allocations, and re-offering it forever would
        // stall the session.
        self.scratch.clear();
        self.scratch_pending = false;
        Err(SubtitleDecodeError::Convert(e))
      }
    }
  }

  pub(crate) fn send_eof_impl(&mut self) -> Result<(), SubtitleDecodeError> {
    // Subtitle decoders have no draining — the legacy decode() API
    // produces a frame inline with each packet. EOF is a no-op.
    Ok(())
  }

  pub(crate) fn flush_impl(&mut self) -> Result<(), SubtitleDecodeError> {
    self.decoder.flush();
    // A held cue belongs to the position being abandoned.
    self.scratch_pending = false;
    self.scratch.clear();
    Ok(())
  }
}

macro_rules! subtitle_lane_face {
  ($($lane:ty),+ $(,)?) => { $(
    impl CarrierSubtitleStreamDecoder<$lane> {
      /// Opens a subtitle decoder for `parameters`.
      pub fn open(
        parameters: Parameters,
        time_base: Timebase,
        limits: DecoderLimits,
      ) -> Result<Self, SubtitleDecodeError> {
        Self::open_impl(parameters, time_base, limits)
      }

      /// The time base associated with the source stream.
      pub const fn time_base(&self) -> Timebase {
        self.time_base_impl()
      }

      /// The budgets this decoder was opened with.
      pub const fn limits(&self) -> DecoderLimits {
        self.limits_impl()
      }

      /// The wrapped decoder context.
      pub const fn inner(&self) -> &ffmpeg_next::decoder::Subtitle {
        self.inner_impl()
      }
    }

    impl SubtitleDecoder for CarrierSubtitleStreamDecoder<$lane> {
      type Adapter = Ffmpeg;
      type Buffer = <$lane as crate::FfmpegCarrier>::Buffer;
      type Error = SubtitleDecodeError;

      fn send_packet(
        &mut self,
        packet: &SubtitlePacket<SubtitlePacketExtra, Self::Buffer>,
      ) -> Result<(), Self::Error> {
        self.send_packet_impl(packet)
      }

      fn receive_frame(
        &mut self,
        dst: &mut SubtitleFrame<SubtitleFrameExtra, Self::Buffer>,
      ) -> Result<(), Self::Error> {
        self.receive_frame_impl(dst)
      }

      fn send_eof(&mut self) -> Result<(), Self::Error> {
        self.send_eof_impl()
      }

      fn flush(&mut self) -> Result<(), Self::Error> {
        self.flush_impl()
      }
    }
  )+ };
}

subtitle_lane_face!(crate::View, crate::Owned);

/// Errors from [`FfmpegSubtitleStreamDecoder`].
#[derive(thiserror::Error, Debug, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
pub enum SubtitleDecodeError {
  /// The wrapped `ffmpeg::decoder::Subtitle` reported an error.
  #[error(transparent)]
  Decode(#[from] Error),
  /// Conversion from FFmpeg's `AVSubtitle` to mediadecode's
  /// `SubtitleFrame` failed.
  #[error(transparent)]
  Convert(#[from] ConvertError),
  /// `receive_frame` was called with no buffered frame ready — caller
  /// should send another packet.
  #[error("no subtitle frame ready; send another packet first")]
  NoFrameReady,
  /// `send_packet` was called while a decoded frame from a previous
  /// packet hasn't been drained — the legacy `decode()` API can't
  /// queue, so the caller must drain via `receive_frame` first.
  #[error("subtitle frame already pending; drain via receive_frame first")]
  FramePending,
}

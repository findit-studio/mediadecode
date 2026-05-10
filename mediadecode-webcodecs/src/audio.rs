//! `mediadecode::future::local::AudioStreamDecoder` impl backed
//! by `web_sys::AudioDecoder`.
//!
//! Mirrors [`crate::video`] in shape — the WebCodecs adapter is
//! async-only (`!Send`), so this module implements only the
//! `mediadecode::future::local::AudioStreamDecoder` variant of
//! the trait. The internal frame queue + epoch + waker pattern is
//! identical to the video decoder; the differences are
//! AudioCodecs-specific (no async copy — `AudioData.copyTo` is
//! sync — and per-plane copies for planar formats).
//!
//! Backpressure works the same way as video: `send_packet` yields
//! while `AudioDecoder.decodeQueueSize >= [`MAX_DECODE_QUEUE`]
//! and resumes when the WebCodecs `dequeue` event fires.

use std::num::NonZeroU32;

use mediadecode::{
  Timebase, Timestamp,
  channel::AudioChannelLayout,
  frame::{AudioFrame, Plane},
  future::local::AudioStreamDecoder,
  packet::{AudioPacket, PacketFlags},
};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::{
  adapter::WebCodecs,
  buffer::WebCodecsBuffer,
  codec_id::AudioCodecId,
  codec_string,
  dispatch::{
    allocate_value_handler, allocate_void_handler, free_value_handler, free_void_handler,
    make_value_trampoline, make_void_trampoline,
  },
  error::{AudioDecodeError, Error},
  extras::{AudioFrameExtra, AudioPacketExtra},
  sample_format::SampleFormat,
  state::{DecodedFrame, PendingOutput, SharedState},
  video::{MAX_INPUT_INFLIGHT_BYTES, MAX_INPUT_PACKET_BYTES},
};

/// Decoder-side cap (`decode_queue_size + pending_copies`).
/// Sized to absorb a complete reorder-buffer release at EOF
/// — typical audio codecs don't reorder, but matching the
/// video shape keeps the cross-adapter behavior consistent.
pub const MAX_PENDING_DECODE: u32 = 64;

/// Hard cap on a single audio frame's per-plane
/// `allocation_size` (summed across planes). 1 MiB is far
/// above any realistic codec frame (typical AAC ≈ 32 KiB,
/// large Opus frame ≈ 120 KiB, ~5 seconds of 48 kHz stereo
/// f32 at the cap) — anything bigger is malformed input. The
/// output callback measures the actual `AudioData` sum and
/// fails closed before spawning a copy task when it exceeds
/// this cap.
pub const MAX_FRAME_ALLOCATION_BYTES: u32 = 1024 * 1024; // 1 MiB

/// Aggregate byte budget across `pending_copies + queue.len()`.
/// At the per-frame cap that's 64 frames in flight worth of
/// memory — generous for audio, still bounded.
pub const MAX_INFLIGHT_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

/// Output-queue cap. `send_packet` returns
/// [`AudioDecodeError::OutputFull`] (no await) when the queue
/// reaches this size — see the matching note on
/// `MAX_QUEUED_OUTPUT` in `video.rs`.
pub const MAX_QUEUED_OUTPUT: u32 = 64;

/// One CPU-side audio frame produced by the WebCodecs `output`
/// callback after `copyTo` returns. For interleaved sample
/// formats `plane_count == 1`; for planar formats it equals
/// `channel_count` (capped at 8) and each plane carries that
/// channel's samples.
pub(crate) struct DecodedAudioFrame {
  pts: Option<Timestamp>,
  duration: Option<Timestamp>,
  sample_rate: u32,
  nb_samples: u32,
  channel_count: u8,
  format: SampleFormat,
  planes: [(WebCodecsBuffer, u32); 8],
  plane_count: u8,
  key: bool,
  /// Sum of plane bytes pinned in this frame's `Arc<[u8]>`s.
  /// Lets queue admission charge the actual on-queue size
  /// rather than a per-frame constant, mirroring the video
  /// path. The output callback's *budget* reservation is
  /// `2 × byte_size` (Vec→Arc copy peak); after the copy
  /// settles only the Arcs remain, so on-queue accounting
  /// is `byte_size` exactly.
  byte_size: u32,
}

impl DecodedAudioFrame {
  /// Construct a CPU-side audio frame body.
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    pts: Option<Timestamp>,
    duration: Option<Timestamp>,
    sample_rate: u32,
    nb_samples: u32,
    channel_count: u8,
    format: SampleFormat,
    planes: [(WebCodecsBuffer, u32); 8],
    plane_count: u8,
    key: bool,
    byte_size: u32,
  ) -> Self {
    Self {
      pts,
      duration,
      sample_rate,
      nb_samples,
      channel_count,
      format,
      planes,
      plane_count,
      key,
      byte_size,
    }
  }
  /// Bytes pinned by this frame's plane `Arc<[u8]>`s. See
  /// the field doc for why we track it.
  pub const fn byte_size(&self) -> u32 {
    self.byte_size
  }

  /// User PTS (restored from the side-map record).
  pub const fn pts(&self) -> Option<Timestamp> {
    self.pts
  }
  /// Frame duration as reported by `AudioData.duration`.
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Sample rate (Hz).
  pub const fn sample_rate(&self) -> u32 {
    self.sample_rate
  }
  /// Sample count for this frame.
  pub const fn nb_samples(&self) -> u32 {
    self.nb_samples
  }
  /// Channel count.
  pub const fn channel_count(&self) -> u8 {
    self.channel_count
  }
  /// Sample format.
  pub const fn format(&self) -> SampleFormat {
    self.format
  }
  /// Consume `self` and return the planes by value.
  pub fn into_planes(self) -> [(WebCodecsBuffer, u32); 8] {
    self.planes
  }
  /// Number of populated planes.
  pub const fn plane_count(&self) -> u8 {
    self.plane_count
  }
  /// Whether the originating chunk was a key chunk.
  pub const fn key(&self) -> bool {
    self.key
  }
}

const MICROS: Timebase = match NonZeroU32::new(1_000_000) {
  Some(d) => Timebase::new(1, d),
  None => unreachable!(),
};

/// `mediadecode::future::local::AudioStreamDecoder` impl wrapping
/// `web_sys::AudioDecoder`.
pub struct WebCodecsAudioStreamDecoder {
  decoder: web_sys::AudioDecoder,
  /// Captured config — see the matching note in `video.rs`.
  /// `flush()` re-applies this after `reset()` so the decoder
  /// remains reusable across seeks.
  config: web_sys::AudioDecoderConfig,
  state: SharedState<DecodedAudioFrame>,
  time_base: Timebase,
  /// Captured at open; used to pick the output-matching
  /// strategy (FIFO for rebasing PCM-family codecs, exact-ID
  /// lookup for echoing codecs). Stored on the struct so
  /// `flush()`'s decoder rebuild can re-derive the same
  /// behaviour without re-parsing the codec string. See
  /// [`codec_rebases_timestamps`].
  rebases_timestamps: bool,
  /// Channel count from open. Used to compute the expected
  /// sample count of each PCM input packet (and verify
  /// `AudioData.number_of_frames` matches it on output —
  /// codex round 6 multi-output defense).
  channel_count: u8,
  /// Bytes-per-sample for PCM-family codecs (`Some(2)` for
  /// `pcm-s16`, `Some(4)` for `pcm-f32` / `pcm-s32`, …).
  /// `None` for codecs that don't have a fixed bytes-per-
  /// sample mapping (Opus / AAC / FLAC / Vorbis); the per-
  /// output sample-count check is skipped in that case
  /// (those use exact-ID matching instead, where the same
  /// concern is addressed by fail-closed-on-miss).
  pcm_bytes_per_sample: Option<u32>,
  /// Slot IDs for the JS-side trampolines registered with
  /// the `AudioDecoder`. The trampolines themselves are
  /// stable JS functions that never invalidate; the actual
  /// Rust handler bodies live in the [`crate::dispatch`]
  /// slot maps and are freed at `Drop` / replaced at
  /// `flush()`. Late callbacks against a freed slot find an
  /// empty entry and orphan-close their `AudioData` —
  /// that's the entire round-9-through-11 teardown class
  /// addressed structurally rather than by timing
  /// assumptions.
  output_slot_id: u64,
  error_slot_id: u64,
  dequeue_slot_id: u64,
  eof: bool,
}

impl WebCodecsAudioStreamDecoder {
  /// Open a decoder for a known [`AudioCodecId`].
  pub fn open(
    codec: AudioCodecId,
    audio_specific_config: Option<&[u8]>,
    sample_rate: u32,
    channel_count: u8,
    time_base: Timebase,
  ) -> Result<Self, AudioDecodeError> {
    let codec_string = codec_string::for_audio(codec, audio_specific_config)?;
    Self::open_with_codec_string(
      &codec_string,
      audio_specific_config,
      sample_rate,
      channel_count,
      time_base,
    )
  }

  /// Open a decoder with an explicit WebCodecs codec string
  /// (`"opus"`, `"mp4a.40.2"`, …) and an optional codec
  /// configuration blob.
  ///
  /// `description`, when `Some`, is forwarded to
  /// [`AudioDecoderConfig.description`](https://www.w3.org/TR/webcodecs/#dom-audiodecoderconfig-description).
  /// For raw MP4 AAC the description **must** be the
  /// `AudioSpecificConfig` (ASC) bytes; without it WebCodecs
  /// expects ADTS-framed input and raw AAC samples fail to
  /// decode. Opus / Vorbis / FLAC accept it as the codec
  /// private data when present.
  pub fn open_with_codec_string(
    codec_string: &str,
    description: Option<&[u8]>,
    sample_rate: u32,
    channel_count: u8,
    time_base: Timebase,
  ) -> Result<Self, AudioDecodeError> {
    // Preallocate the output queue to the worst-case
    // simultaneously-queued frame count: producer-side cap
    // (`MAX_QUEUED_OUTPUT`) plus one decoded-batch worth of
    // in-flight frames (`MAX_PENDING_DECODE`). Admission
    // gates the sum so `push_queue` never reallocates
    // (codex round 27).
    let state = SharedState::<DecodedAudioFrame>::try_new(
      MAX_QUEUED_OUTPUT as usize + MAX_PENDING_DECODE as usize,
    )
    .map_err(AudioDecodeError::Js)?;
    let config = web_sys::AudioDecoderConfig::new(codec_string, channel_count as u32, sample_rate);
    if let Some(bytes) = description {
      // Codex round 35 [accepted]: cap codec-description size
      // before any JS allocation. Same rationale as the video
      // path — `description` is demuxer-controlled and a
      // multi-MiB blob would otherwise bypass per-packet caps.
      if bytes.len() > crate::video::MAX_CODEC_DESCRIPTION_BYTES {
        return Err(AudioDecodeError::Js(Error::from_static(
          "codec description exceeds MAX_CODEC_DESCRIPTION_BYTES",
        )));
      }
      // JS-owned copy — see the matching note in `video.rs`.
      // Fallible construction (codex round 21) so a JS-heap
      // OOM at open returns Err rather than aborting.
      let arr = crate::video::try_new_uint8_array(bytes.len() as u32)
        .map_err(|e| AudioDecodeError::Js(Error::from_js(e)))?;
      arr.copy_from(bytes);
      config.set_description_u8_array(&arr);
    }
    let rebases_timestamps = codec_rebases_timestamps(codec_string);
    let pcm_bytes_per_sample = pcm_bytes_per_sample_for(codec_string);
    // Reject rebasing codecs (PCM family + ulaw/alaw) without a
    // known bytes-per-sample mapping: codex round 8 flagged
    // that the rebasing path must FIFO-pop pending records
    // *before* it can prove the AudioData belongs to that
    // input, so the only safe defense against spec-allowed
    // multi-output is the per-record sample-count check —
    // which requires `expected_samples` to be non-zero, which
    // requires `pcm_bytes_per_sample` to map. `pcm-s24` (and
    // any future variant) lands here; surface as an open-
    // time error rather than silently FIFO-popping under a
    // disabled check.
    if rebases_timestamps && pcm_bytes_per_sample.is_none() {
      return Err(AudioDecodeError::Js(Error::from_js(JsValue::from_str(
        &format!(
          "audio codec {codec_string:?} rebases timestamps but the adapter \
           has no bytes-per-sample mapping for it; cannot safely match \
           decoded outputs (rejected at open)"
        ),
      ))));
    }
    let installed = install_audio_decoder(&state, &config, rebases_timestamps)?;

    Ok(Self {
      decoder: installed.decoder,
      config,
      state,
      time_base,
      rebases_timestamps,
      channel_count,
      pcm_bytes_per_sample,
      output_slot_id: installed.output_slot_id,
      error_slot_id: installed.error_slot_id,
      dequeue_slot_id: installed.dequeue_slot_id,
      eof: false,
    })
  }

  /// The codec time base passed at construction.
  pub const fn time_base(&self) -> Timebase {
    self.time_base
  }

  fn check_closed(&self) -> Result<(), AudioDecodeError> {
    if let Some(err) = self.state.borrow().last_error_clone() {
      return Err(AudioDecodeError::Closed(err));
    }
    Ok(())
  }

  /// Admission control for `send_packet`. See the matching
  /// helper in `video.rs` — same shape, same rationale.
  async fn await_decode_room(&self) -> Result<(), AudioDecodeError> {
    loop {
      {
        let inner = self.state.borrow();
        if let Some(err) = inner.last_error_clone() {
          return Err(AudioDecodeError::Closed(err));
        }
        if inner.queue_len() >= MAX_QUEUED_OUTPUT as usize {
          return Err(AudioDecodeError::OutputFull);
        }
        // Aggregate byte budget — exact tracked bytes plus
        // `last_measured_frame_bytes` headroom. See matching
        // note in `video.rs::await_decode_room`. For audio
        // the per-frame budget is uniform
        // (`MAX_INFLIGHT_PER_FRAME_BYTES`) so the headroom
        // converges to that constant after the first output.
        let in_flight_bytes = inner
          .queue_bytes()
          .saturating_add(inner.pending_copy_bytes())
          .saturating_add(inner.last_measured_frame_bytes());
        // `>` rather than `>=` so the exact-cap boundary
        // matches the output-side enforcement (see the note
        // in `video.rs::await_decode_room`).
        if in_flight_bytes > MAX_INFLIGHT_BYTES {
          return Err(AudioDecodeError::OutputFull);
        }
        if pending_decode(&self.decoder, &inner) < MAX_PENDING_DECODE as usize {
          return Ok(());
        }
      }
      let _guard = self.state.dequeue_waker_guard();
      let state = self.state.clone();
      let decoder = self.decoder.clone();
      core::future::poll_fn(move |cx| {
        let (closed, busy, queue_full) = {
          let inner = state.borrow();
          (
            inner.is_closed(),
            pending_decode(&decoder, &inner) >= MAX_PENDING_DECODE as usize,
            inner.queue_len() >= MAX_QUEUED_OUTPUT as usize,
          )
        };
        if closed || !busy || queue_full {
          return core::task::Poll::Ready(());
        }
        state.borrow_mut().set_dequeue_waker(cx.waker().clone());
        let (closed_2, busy_2, queue_full_2) = {
          let inner = state.borrow();
          (
            inner.is_closed(),
            pending_decode(&decoder, &inner) >= MAX_PENDING_DECODE as usize,
            inner.queue_len() >= MAX_QUEUED_OUTPUT as usize,
          )
        };
        if closed_2 || !busy_2 || queue_full_2 {
          state.borrow_mut().clear_dequeue_waker();
          core::task::Poll::Ready(())
        } else {
          core::task::Poll::Pending
        }
      })
      .await;
    }
  }
}

/// Audio counterpart to `pending_decode` in `video.rs` —
/// `decode_queue_size` is the only safe input-side cap; see
/// `video.rs::await_decode_room` for the deadlock rationale.
fn pending_decode(
  decoder: &web_sys::AudioDecoder,
  inner: &crate::state::Inner<DecodedAudioFrame>,
) -> usize {
  (decoder.decode_queue_size() as usize).saturating_add(inner.pending_copies() as usize)
}

impl Drop for WebCodecsAudioStreamDecoder {
  fn drop(&mut self) {
    // Trampoline-based teardown is uncomplicated: free the
    // dispatcher slots and close the decoder. Late callbacks
    // for this decoder fire into the always-callable
    // dispatcher, which finds their slot empty and orphan-
    // closes the `AudioData`. No `Closure` to invalidate, no
    // `Rc<SharedState>` retained past this scope.
    self.state.bump_epoch();
    self.decoder.set_ondequeue(None);
    let _ = self.decoder.close();
    self.state.clear_close_hook();
    free_value_handler(self.output_slot_id);
    free_value_handler(self.error_slot_id);
    free_void_handler(self.dequeue_slot_id);
  }
}

impl AudioStreamDecoder for WebCodecsAudioStreamDecoder {
  type Adapter = WebCodecs;
  type Buffer = WebCodecsBuffer;
  type Error = AudioDecodeError;

  async fn send_packet(
    &mut self,
    packet: &AudioPacket<AudioPacketExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    self.check_closed()?;
    if self.eof {
      return Err(AudioDecodeError::AtEof);
    }
    self.await_decode_room().await?;

    // The user's `PacketFlags::KEY` is preserved for downstream
    // frame metadata (`AudioFrameExtra::key`), but the WebCodecs
    // chunk type is **always** `Key` for the audio codecs this
    // adapter accepts. Per the WebCodecs codec registrations,
    // every Opus / AAC / FLAC / Vorbis / MP3 / A-law / μ-law /
    // PCM packet is independently decodable, and Chromium / Safari
    // reject `Delta` chunks for these codecs with an
    // `EncodingError`. Sending the demuxer's flag through
    // unmodified lost packets that the demuxer happened to omit
    // the flag on (common for Opus where the flag is rarely set).
    let key = packet.flags().contains(PacketFlags::KEY);
    let chunk_type = web_sys::EncodedAudioChunkType::Key;

    let bytes = packet.data().as_ref();
    // Compressed-input byte caps. See the matching notes in
    // `video.rs::send_packet`. The audio caps are scaled to
    // typical packet sizes (Opus ≤ 1500 B, AAC ≤ ~8 KB).
    if bytes.len() > MAX_INPUT_PACKET_BYTES {
      let err = Error::from_js(JsValue::from_str(
        "encoded audio packet exceeds MAX_INPUT_PACKET_BYTES",
      ));
      self.state.borrow_mut().record_close(err.clone());
      self.state.invoke_close_hook();
      self.state.wake_all();
      return Err(AudioDecodeError::Closed(err));
    }
    {
      let inner = self.state.borrow();
      if inner
        .pending_input_bytes()
        .saturating_add(bytes.len() as u64)
        > MAX_INPUT_INFLIGHT_BYTES
      {
        drop(inner);
        let err = Error::from_js(JsValue::from_str(
          "encoded audio pending_input_bytes would exceed MAX_INPUT_INFLIGHT_BYTES; \
           flush() to recover",
        ));
        self.state.borrow_mut().record_close(err.clone());
        self.state.invoke_close_hook();
        self.state.wake_all();
        return Err(AudioDecodeError::Closed(err));
      }
    }
    // PCM trust-boundary check, ahead of any JS allocation.
    // Codex round 30 introduced the partial-sample reject
    // and round 31 [accepted] flagged that running it after
    // `try_new_uint8_array` + `EncodedAudioChunk::new` let
    // a caller force unbounded JS-heap allocation by feeding
    // malformed packets just below `MAX_INPUT_PACKET_BYTES`.
    // Reject before touching JS so a malformed-packet stream
    // observes ordinary `Err`s without browser memory churn.
    //
    // For fixed-width PCM the byte length must be an exact
    // multiple of `bps × channel_count`; integer-division
    // flooring of the partial-sample remainder would let
    // `AudioData.number_of_frames == expected` pass and
    // publish corrupt input as clean decoded audio. Non-fixed-
    // width PCM (`pcm-s24` etc.) and compressed codecs skip
    // the check (`expected_samples = 0` means "unknown",
    // matching the codex round 6 contract).
    let expected_samples = match self.pcm_bytes_per_sample {
      Some(bps) if self.channel_count > 0 => {
        let denom = bps as usize * self.channel_count as usize;
        if bytes.len() % denom != 0 {
          return Err(AudioDecodeError::Js(Error::from_static(
            "malformed PCM packet: byte length is not a multiple of \
             bytes_per_sample × channel_count",
          )));
        }
        u32::try_from(bytes.len() / denom).map_err(|_| {
          AudioDecodeError::Js(Error::from_static("PCM packet sample count exceeds u32"))
        })?
      }
      _ => 0,
    };

    // Fallible `Uint8Array(len)` construction (codex round
    // 21). The direct `new_with_length` constructor throws
    // on JS-heap OOM and aborts the wasm tab; this path
    // routes the throw through `record_close` instead.
    let buf = match crate::video::try_new_uint8_array(bytes.len() as u32) {
      Ok(b) => b,
      Err(err) => {
        let err = Error::from_js(err);
        self.state.borrow_mut().record_close(err.clone());
        self.state.invoke_close_hook();
        self.state.wake_all();
        return Err(AudioDecodeError::Closed(err));
      }
    };
    buf.copy_from(bytes);

    // Allocate the ID before chunk construction; defer the
    // side-map insertion until the chunk has been built so a
    // `EncodedAudioChunk::new` failure can't leak a phantom
    // record. See the matching note in `video.rs`.
    let submission_id = self.state.borrow_mut().next_submission_id();

    let init = web_sys::EncodedAudioChunkInit::new(&buf.into(), 0, chunk_type);
    init.set_timestamp_f64(submission_id as f64);
    if let Some(d) = packet.duration() {
      let duration_us = d.rescale_to(MICROS).pts();
      init.set_duration_f64(duration_us as f64);
    }

    let chunk = web_sys::EncodedAudioChunk::new(&init).map_err(Error::from_js)?;

    let insert_res = {
      let mut inner = self.state.borrow_mut();
      let epoch = inner.epoch();
      inner.insert_pending_output(
        submission_id,
        PendingOutput::new(
          epoch,
          packet.pts(),
          key,
          bytes.len() as u32,
          expected_samples,
        ),
      )
    };
    if let Err(err) = insert_res {
      // `insert_pending_output` ran `Inner::record_close` on
      // overflow — drain the JS-side decoder too. Idempotent
      // close per spec. See the matching note in `video.rs`.
      self.state.invoke_close_hook();
      self.state.wake_all();
      return Err(AudioDecodeError::Closed(err));
    }

    if let Err(e) = self.decoder.decode(&chunk) {
      self.state.borrow_mut().remove_pending_output(submission_id);
      return Err(AudioDecodeError::Js(Error::from_js(e)));
    }
    Ok(())
  }

  async fn receive_frame(
    &mut self,
    dst: &mut AudioFrame<SampleFormat, AudioChannelLayout, AudioFrameExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    let frame = wait_for_frame(&self.state, &self.decoder, self.eof).await?;

    let sample_rate = frame.sample_rate();
    let nb_samples = frame.nb_samples();
    let channel_count = frame.channel_count();
    let format = frame.format();
    let plane_count = frame.plane_count();
    let key = frame.key();
    let pts = frame.pts();
    let duration = frame.duration();
    let [p0, p1, p2, p3, p4, p5, p6, p7] = frame.into_planes();
    let planes: [Plane<WebCodecsBuffer>; 8] = [
      Plane::new(p0.0, p0.1),
      Plane::new(p1.0, p1.1),
      Plane::new(p2.0, p2.1),
      Plane::new(p3.0, p3.1),
      Plane::new(p4.0, p4.1),
      Plane::new(p5.0, p5.1),
      Plane::new(p6.0, p6.1),
      Plane::new(p7.0, p7.1),
    ];

    *dst = AudioFrame::new(
      sample_rate,
      nb_samples,
      channel_count,
      format,
      // WebCodecs gives us a channel count but not a layout
      // tag — `AudioChannelLayout::new(N)` produces a layout
      // with the right channel count and `Unspecified` order
      // (vs. `default()` which is 0-channel).
      AudioChannelLayout::new(channel_count as u32),
      planes,
      plane_count,
      AudioFrameExtra::new(key),
    )
    .with_pts(pts)
    .with_duration(duration);
    Ok(())
  }

  async fn send_eof(&mut self) -> Result<(), Self::Error> {
    self.check_closed()?;
    // Cancellation / failure poison — mirror of
    // `video.rs::send_eof`. The JS-side `flush()` is not
    // cancellable, so on cancel we bump the epoch (any late
    // outputs are now stale) and record a Closed error so the
    // user must call `flush()` to recover rather than retry on
    // a partially-drained decoder.
    struct EofCancelGuard<F> {
      state: SharedState<F>,
      completed: bool,
    }
    impl<F> Drop for EofCancelGuard<F> {
      fn drop(&mut self) {
        if !self.completed {
          self.state.bump_epoch();
          self
            .state
            .borrow_mut()
            .record_close(Error::from_js(wasm_bindgen::JsValue::from_str(
              "send_eof was cancelled — call flush() to recover",
            )));
          // Stop the still-running JS-side flush — see the
          // matching note in `video.rs::send_eof`.
          self.state.invoke_close_hook();
        }
        self.state.wake_all();
      }
    }
    let mut guard = EofCancelGuard {
      state: self.state.clone(),
      completed: false,
    };

    let promise = self.decoder.flush();
    JsFuture::from(promise).await.map_err(Error::from_js)?;

    {
      let mut inner = self.state.borrow_mut();
      inner.clear_pending_outputs();
    }
    self.eof = true;
    guard.completed = true;
    Ok(())
  }

  /// Rebuild the underlying `AudioDecoder` from scratch.
  /// Trampoline-based callback registration (see
  /// [`crate::dispatch`]) makes the rebuild structurally
  /// safe regardless of pending JS-side callback timing:
  /// the new decoder gets fresh slot IDs, the old slots are
  /// freed immediately, and any callback queued before the
  /// old `decoder.close()` that fires later finds an empty
  /// slot and orphan-closes its `AudioData` via the always-
  /// callable dispatcher. No `Closure` invalidation, no
  /// retained-callback bookkeeping, no event-loop yield.
  ///
  /// `FlushGuard` covers cancellation safety: if the future
  /// is dropped or `install_audio_decoder` returns Err after
  /// `decoder.close()`, the guard records a `Closed` so the
  /// caller observes the half-rebuilt state cleanly instead
  /// of a passing `check_closed()` over a dead decoder.
  async fn flush(&mut self) -> Result<(), Self::Error> {
    self.state.bump_epoch();
    self.eof = false;
    let _ = self.decoder.close();
    free_value_handler(self.output_slot_id);
    free_value_handler(self.error_slot_id);
    free_void_handler(self.dequeue_slot_id);
    struct FlushGuard<'a> {
      state: &'a SharedState<DecodedAudioFrame>,
      completed: bool,
    }
    impl Drop for FlushGuard<'_> {
      fn drop(&mut self) {
        if !self.completed {
          let just_closed =
            self
              .state
              .borrow_mut()
              .record_close(Error::from_js(JsValue::from_str(
                "audio flush did not complete (cancelled or rebuild failed); \
               decoder is closed",
              )));
          if just_closed {
            self.state.invoke_close_hook();
          }
          self.state.wake_all();
        }
      }
    }
    let mut guard = FlushGuard {
      state: &self.state,
      completed: false,
    };
    let installed = install_audio_decoder(&self.state, &self.config, self.rebases_timestamps)?;
    self.decoder = installed.decoder;
    self.output_slot_id = installed.output_slot_id;
    self.error_slot_id = installed.error_slot_id;
    self.dequeue_slot_id = installed.dequeue_slot_id;
    guard.completed = true;
    self.state.borrow_mut().clear_last_error();
    self.state.wake_dequeue();
    Ok(())
  }
}

/// Audio counterpart to `video.rs::wait_for_frame`.
async fn wait_for_frame(
  state: &SharedState<DecodedAudioFrame>,
  decoder: &web_sys::AudioDecoder,
  eof: bool,
) -> Result<DecodedAudioFrame, AudioDecodeError> {
  loop {
    let popped = {
      let mut inner = state.borrow_mut();
      if let Some(err) = inner.last_error_clone() {
        return Err(AudioDecodeError::Closed(err));
      }
      // Read the head's `byte_size` first, then pop with
      // that exact amount so `queue_bytes` decrements by
      // what `push_queue` added. Mirrors the video path.
      let head_bytes = inner
        .peek_queue_head()
        .map(|f| f.byte_size() as u64)
        .unwrap_or(0);
      let frame = inner.pop_queue(head_bytes);
      if frame.is_none() {
        // `pending_outputs` deliberately excluded — see the
        // matching note in `video.rs::wait_for_frame`.
        let active_decode_work = inner.pending_copies() > 0 || decoder.decode_queue_size() > 0;
        if !active_decode_work {
          if eof {
            return Err(AudioDecodeError::Eof);
          }
          return Err(AudioDecodeError::NoFrameReady);
        }
      }
      frame
    };
    if let Some(frame) = popped.map(DecodedFrame::into_frame) {
      // Popping reduces `queue.len()`, which may unblock a
      // backpressured `send_packet`. Wake outside the borrow.
      state.wake_dequeue();
      return Ok(frame);
    }
    let _guard = state.receiver_waker_guard();
    core::future::poll_fn(|cx| {
      let mut inner = state.borrow_mut();
      // `pending_outputs` deliberately excluded — parking on
      // a non-empty side map deadlocks reorder-buffered codecs
      // (decoder needs more input via `send_packet`, which is
      // blocked behind this `&mut self`). See the matching
      // note in `video.rs::wait_for_frame`.
      let active_decode_work = inner.pending_copies() > 0 || decoder.decode_queue_size() > 0;
      // Resolve Ready when the queue has a frame, the decoder
      // has been closed, OR when no further JS work is queued
      // (the outer loop translates the latter to `NoFrameReady`
      // / `Eof`). Park otherwise.
      if !inner.queue_is_empty() || inner.is_closed() || !active_decode_work {
        core::task::Poll::Ready(())
      } else {
        inner.set_receiver_waker(cx.waker().clone());
        core::task::Poll::Pending
      }
    })
    .await;
  }
}

/// RAII rollback for the post-admission window in the audio
/// output callback. See [`crate::video::VideoCopyAdmission`] for
/// the rationale (codex round 33). Audio's copy is sync rather
/// than async, but the spawn_local path is identical: pending_copies
/// is incremented before `spawn_local`, and an unwinding alloc
/// failure between admission and the future's first poll would
/// strand the counter and the JS `AudioData`.
struct AudioCopyAdmission {
  state: SharedState<DecodedAudioFrame>,
  data: Option<web_sys::AudioData>,
  byte_estimate: u64,
  armed: bool,
}

impl Drop for AudioCopyAdmission {
  fn drop(&mut self) {
    if !self.armed {
      return;
    }
    if let Some(d) = self.data.take() {
      d.close();
    }
    self.state.borrow_mut().sub_pending_copy(self.byte_estimate);
    self.state.wake_all();
  }
}

impl AudioCopyAdmission {
  fn disarm(&mut self) -> web_sys::AudioData {
    self.armed = false;
    self
      .data
      .take()
      .expect("AudioCopyAdmission::disarm called twice")
  }
}

// `byte_estimate` mirrors the video path: same value passed at
// admission gets subtracted on every completion outcome
// (Stale / Pushed / Errored).
async fn handle_audio_data(
  mut admission: AudioCopyAdmission,
  state: SharedState<DecodedAudioFrame>,
  epoch: u64,
  record: PendingOutput,
  byte_estimate: u64,
) {
  // First action: disarm the spawn-panic rollback. From here on
  // this task is responsible for closing the JS data and
  // decrementing pending_copies.
  let data = admission.disarm();
  drop(admission);
  // RAII guard mirrors the video path — see `video.rs` for the
  // GPU-throttling rationale (audio doesn't have GPU surfaces,
  // but the same close-on-every-path discipline keeps the
  // resource lifecycle uniform).
  let data_guard = JsAudioGuard(Some(data));

  // Early-bail: skip the copy if `flush()` already advanced
  // the epoch. Wake both — see the matching note in
  // `video.rs`.
  if state.epoch() != epoch {
    state.borrow_mut().sub_pending_copy(byte_estimate);
    state.wake_all();
    return;
  }

  let result = copy_audio_data(data_guard.data(), record.user_pts(), record.key()).await;
  drop(data_guard);

  enum Outcome {
    Pushed,
    /// `just_closed` fires the underlying-decoder close hook
    /// once. Mirror of the `video.rs` Errored variant.
    Errored {
      just_closed: bool,
    },
    Stale,
  }
  let outcome = {
    let mut inner = state.borrow_mut();
    // Always decrement — see `video.rs` and `state.rs` notes:
    // `bump_epoch` deliberately does not reset
    // `pending_copies`, so live-copy accounting stays accurate
    // across flush boundaries.
    inner.sub_pending_copy(byte_estimate);
    if inner.epoch() != epoch {
      Outcome::Stale
    } else if inner.is_closed() {
      // Decoder closed by another path while this copy was
      // in flight — discard rather than push onto a queue
      // the user can no longer drain. See the matching note
      // in `video.rs`.
      Outcome::Stale
    } else {
      match result {
        Ok(decoded) => {
          // Allow the queue to exceed `MAX_QUEUED_OUTPUT` —
          // see the matching note in `video.rs`. The byte
          // cap (`MAX_INFLIGHT_BYTES`) is the only hard bound
          // on memory; `queue_bytes` accounts for the actual
          // pinned Arc bytes of each frame (`byte_size`),
          // not a per-frame constant. The earlier revision
          // tracked a fixed `MAX_INFLIGHT_PER_FRAME_BYTES`
          // per slot, which under-counted high-bandwidth
          // streams that exceeded the constant per frame.
          let bytes = decoded.byte_size() as u64;
          let projected = inner.queue_bytes().saturating_add(bytes);
          if projected > MAX_INFLIGHT_BYTES {
            let just_closed = inner.record_close(Error::from_js(JsValue::from_str(
              "audio output queue exceeded MAX_INFLIGHT_BYTES at copy completion",
            )));
            Outcome::Errored { just_closed }
          } else if let Err(_returned) = inner.push_queue(DecodedFrame::new(decoded), bytes) {
            // Codex round 27 [accepted]: queue at preallocated
            // cap. Admission gating should make this unreachable;
            // if it happens, fail-closed rather than fall through
            // to a `VecDeque::push_back` OOM panic.
            let just_closed = inner.record_close(Error::from_static(
              "audio decoded-frame queue reached capacity; \
               admission gate failed to bound the output queue",
            ));
            Outcome::Errored { just_closed }
          } else {
            Outcome::Pushed
          }
        }
        Err(err) => {
          let just_closed = inner.record_close(err);
          Outcome::Errored { just_closed }
        }
      }
    }
  };
  match outcome {
    Outcome::Pushed => state.wake_all(),
    Outcome::Errored { just_closed } => {
      if just_closed {
        state.invoke_close_hook();
      }
      state.wake_all();
    }
    // Wake both on Stale too — see the matching note in
    // `video.rs`: a post-flush `receive_frame` parked on
    // `nothing_in_flight` is waiting for stale copies to drain.
    Outcome::Stale => state.wake_all(),
  }
}

/// RAII guard that calls `AudioData.close()` on drop, mirroring
/// `JsFrameGuard` in `video.rs`.
struct JsAudioGuard(Option<web_sys::AudioData>);
impl JsAudioGuard {
  fn data(&self) -> &web_sys::AudioData {
    self.0.as_ref().expect("audio data already taken")
  }
}
impl Drop for JsAudioGuard {
  fn drop(&mut self) {
    if let Some(data) = self.0.take() {
      data.close();
    }
  }
}

async fn copy_audio_data(
  data: &web_sys::AudioData,
  user_pts: Option<Timestamp>,
  key: bool,
) -> Result<DecodedAudioFrame, Error> {
  let format_str = data
    .format()
    .ok_or_else(|| Error::from_js(JsValue::from_str("AudioData.format is null")))?;
  let format = SampleFormat::from_spec_name(audio_sample_format_name(format_str))
    .ok_or_else(|| Error::from_js(JsValue::from_str("unknown AudioSampleFormat")))?;

  let sample_rate = data.sample_rate() as u32;
  let nb_samples = data.number_of_frames();

  let channel_count_u32 = data.number_of_channels();
  let channel_count: u8 = u8::try_from(channel_count_u32).map_err(|_| {
    Error::from_js(JsValue::from_str(&format!(
      "AudioData.numberOfChannels = {channel_count_u32} exceeds the 255 the AudioFrame channel_count field encodes"
    )))
  })?;
  if channel_count == 0 {
    return Err(Error::from_js(JsValue::from_str(
      "AudioData reports zero channels",
    )));
  }

  // `data.timestamp()` is the submission ID we stamped onto
  // the chunk, NOT a wallclock value. The user's real PTS came
  // through the `pending_outputs` side map.
  let pts = user_pts;
  let duration = Some(Timestamp::new(data.duration() as i64, MICROS));

  let plane_count = if format.is_planar() {
    if channel_count as usize > 8 {
      return Err(Error::from_js(JsValue::from_str(&format!(
        "{channel_count}-channel planar audio exceeds the 8-plane cap of mediadecode::AudioFrame"
      ))));
    }
    channel_count as usize
  } else {
    1
  };

  // Codex round 23: precompute the per-plane byte count
  // expected from `nb_samples × channels × bytes_per_sample`
  // (interleaved) or `nb_samples × bytes_per_sample`
  // (planar). Each `data.allocation_size(plane)` is then
  // checked against this exact value before the copy
  // proceeds — a browser/version-skew that returns a
  // shorter allocation than the metadata implies would
  // otherwise let `nb_samples` / `channel_count` /
  // `format` reach `DecodedAudioFrame::new` with bytes
  // shorter than they describe, and a downstream consumer
  // computing `samples × bps` to walk the plane could
  // panic on safe slice or read past the buffer in
  // unsafe/FFI code.
  let bytes_per_sample = audio_sample_format_bytes(format_str);
  if bytes_per_sample == 0 {
    return Err(Error::from_js(JsValue::from_str(
      "AudioData has unknown bytes-per-sample for plane size validation",
    )));
  }
  let per_plane_samples = nb_samples;
  let expected_plane_bytes: u32 = if format.is_planar() {
    per_plane_samples.saturating_mul(bytes_per_sample)
  } else {
    per_plane_samples
      .saturating_mul(bytes_per_sample)
      .saturating_mul(channel_count_u32)
  };

  let mut planes: [(WebCodecsBuffer, u32); 8] = [
    (WebCodecsBuffer::empty(), 0),
    (WebCodecsBuffer::empty(), 0),
    (WebCodecsBuffer::empty(), 0),
    (WebCodecsBuffer::empty(), 0),
    (WebCodecsBuffer::empty(), 0),
    (WebCodecsBuffer::empty(), 0),
    (WebCodecsBuffer::empty(), 0),
    (WebCodecsBuffer::empty(), 0),
  ];
  let mut total_bytes: u32 = 0;
  for (plane_index, slot) in planes.iter_mut().enumerate().take(plane_count) {
    let opts = web_sys::AudioDataCopyToOptions::new(plane_index as u32);
    let plane_size = data.allocation_size(&opts).map_err(Error::from_js)?;
    // Each plane's byte count must match the layout
    // implied by `nb_samples × bytes_per_sample` (or × channels
    // for interleaved). Mismatch indicates the AudioData's
    // metadata can't be honoured against its actual buffer
    // size; fail closed rather than publish a frame whose
    // metadata over-promises.
    if plane_size != expected_plane_bytes {
      return Err(Error::from_js(JsValue::from_str(&format!(
        "AudioData plane {plane_index} allocation_size = {plane_size} \
         does not match expected {expected_plane_bytes} bytes \
         (nb_samples = {nb_samples}, bps = {bytes_per_sample}, \
         channels = {channel_count_u32}, planar = {})",
        format.is_planar(),
      ))));
    }
    total_bytes = total_bytes.saturating_add(plane_size);
    if total_bytes > MAX_FRAME_ALLOCATION_BYTES {
      return Err(Error::from_js(JsValue::from_str(&format!(
        "AudioData total allocation_size > MAX_FRAME_ALLOCATION_BYTES = {MAX_FRAME_ALLOCATION_BYTES}"
      ))));
    }
    let size = plane_size as usize;
    // Fallible Rust allocation (codex round 21). Cap is
    // already enforced above; this guards against OS-level
    // allocator failure on systems under memory pressure
    // even when the requested size is within policy. Uses
    // `Error::from_static` so the OOM-failure path itself
    // doesn't allocate (codex round 22).
    let mut bytes: Vec<u8> = Vec::new();
    bytes
      .try_reserve_exact(size)
      .map_err(|_| Error::from_static("Rust allocation for AudioData plane copy failed"))?;
    bytes.resize(size, 0);
    data
      .copy_to_with_u8_slice(&mut bytes, &opts)
      .map_err(Error::from_js)?;
    let stride = size as u32;
    *slot = (WebCodecsBuffer::from_bytes(bytes), stride);
  }

  Ok(DecodedAudioFrame::new(
    pts,
    duration,
    sample_rate,
    nb_samples,
    channel_count,
    format,
    planes,
    plane_count as u8,
    key,
    total_bytes,
  ))
}

/// Whether a WebCodecs audio codec rebases the
/// `AudioData.timestamp` it emits, rather than echoing the
/// `EncodedAudioChunk.timestamp` we stamp on input.
///
/// PCM-family decoders (`pcm-s16`, `pcm-f32`, `pcm-u8`,
/// `pcm-s24`, …) and the log-PCM decoders `ulaw` and `alaw`
/// rebase: Chrome emits microsecond timestamps relative to
/// the *first* chunk's value, advancing by cumulative
/// decoded-sample duration. Every other codec we accept
/// (Opus, AAC, FLAC, Vorbis) faithfully echoes the chunk's
/// timestamp on its output. The matching strategy in
/// [`make_audio_output_cb`] branches on this flag — FIFO for
/// rebasing codecs, exact-ID lookup for echoing codecs.
fn codec_rebases_timestamps(codec_string: &str) -> bool {
  codec_string.starts_with("pcm-") || codec_string == "ulaw" || codec_string == "alaw"
}

/// Bytes per sample for PCM-family codec strings whose
/// encoded sample count is `body_len / (bps * channels)`.
/// Returns `None` for codec strings that have no fixed
/// mapping (every non-PCM codec, plus PCM variants with
/// non-integer-byte sample widths like `pcm-s24` packed in
/// arbitrary containers — we skip the per-output sample-
/// count check rather than risk a wrong number).
fn pcm_bytes_per_sample_for(codec_string: &str) -> Option<u32> {
  match codec_string {
    "pcm-u8" | "ulaw" | "alaw" => Some(1),
    "pcm-s16" => Some(2),
    "pcm-s32" | "pcm-f32" => Some(4),
    _ => None,
  }
}

/// Result of [`install_audio_decoder`]: the configured
/// `AudioDecoder` plus the slot IDs of the trampolines
/// registered with it. Slot IDs are owned by the caller and
/// must be `free_value_handler` / `free_void_handler`'d at
/// decoder Drop / replacement.
struct InstalledAudioDecoder {
  decoder: web_sys::AudioDecoder,
  output_slot_id: u64,
  error_slot_id: u64,
  dequeue_slot_id: u64,
}

/// Build a fresh `web_sys::AudioDecoder` and configure it.
///
/// The output / error / dequeue handlers are inserted into
/// the [`crate::dispatch`] slot maps and the JS-side
/// trampolines (which never invalidate) are registered with
/// the decoder. Slot IDs come back via
/// `InstalledAudioDecoder` for the caller to track and free
/// at the right time.
///
/// `configure()` runs before `set_close_hook` to keep state
/// consistent on configure-failure paths: codex round 7
/// flagged that an earlier order (close_hook then configure)
/// could leave `state.close_hook` referencing a partially-
/// built decoder while the caller still held the old one.
fn install_audio_decoder(
  state: &SharedState<DecodedAudioFrame>,
  config: &web_sys::AudioDecoderConfig,
  rebases_timestamps: bool,
) -> Result<InstalledAudioDecoder, AudioDecodeError> {
  let output_handler = make_audio_output_handler(state.clone(), rebases_timestamps);
  let error_handler = make_audio_error_handler(state.clone());
  let dequeue_handler = make_audio_dequeue_handler(state.clone());

  // Codex round 28 [accepted]: each allocation can fail with
  // OOM. Free earlier slots if a later one fails.
  let output_slot_id = allocate_value_handler(output_handler).map_err(AudioDecodeError::Js)?;
  let error_slot_id = match allocate_value_handler(error_handler) {
    Ok(id) => id,
    Err(err) => {
      free_value_handler(output_slot_id);
      return Err(AudioDecodeError::Js(err));
    }
  };
  let dequeue_slot_id = match allocate_void_handler(dequeue_handler) {
    Ok(id) => id,
    Err(err) => {
      free_value_handler(output_slot_id);
      free_value_handler(error_slot_id);
      return Err(AudioDecodeError::Js(err));
    }
  };

  let output_trampoline = make_value_trampoline(output_slot_id);
  let error_trampoline = make_value_trampoline(error_slot_id);
  let dequeue_trampoline = make_void_trampoline(dequeue_slot_id);

  let init = web_sys::AudioDecoderInit::new(&error_trampoline, &output_trampoline);
  let decoder = match web_sys::AudioDecoder::new(&init) {
    Ok(d) => d,
    Err(err) => {
      // Free the slots we just allocated; they own the
      // captured `SharedState` and would leak otherwise.
      free_value_handler(output_slot_id);
      free_value_handler(error_slot_id);
      free_void_handler(dequeue_slot_id);
      return Err(AudioDecodeError::Js(Error::from_js(err)));
    }
  };
  decoder.set_ondequeue(Some(&dequeue_trampoline));

  if let Err(err) = decoder.configure(config) {
    let _ = decoder.close();
    free_value_handler(output_slot_id);
    free_value_handler(error_slot_id);
    free_void_handler(dequeue_slot_id);
    return Err(AudioDecodeError::Js(Error::from_js(err)));
  }

  // Adapter-internal fatal closes drain the JS-side encoded
  // queue immediately. See the matching note in `video.rs`.
  // `set_close_hook` overwrites any prior hook, so a flush
  // rebuild rebinds it to the new decoder atomically with
  // the rest of the swap performed by the caller.
  state.set_close_hook_audio(decoder.clone());
  Ok(InstalledAudioDecoder {
    decoder,
    output_slot_id,
    error_slot_id,
    dequeue_slot_id,
  })
}

/// Build the audio output callback closure. The matching
/// strategy depends on `rebases_timestamps`:
///
/// - `false` (echoing codecs: Opus, AAC, FLAC, Vorbis):
///   exact-ID lookup against the side map. Fail-closed on a
///   current-generation miss. The WebCodecs spec allows a
///   decoder to emit multiple `AudioData` outputs per input
///   chunk; under exact-ID matching, the second output for a
///   given chunk would miss the (already-removed) record and
///   trip the fail-closed branch — surfacing the spec
///   violation rather than letting it cascade through FIFO
///   misalignment. Browsers we accept don't actually do this
///   in practice, but the defense matches codex round 5's
///   recommendation.
///
/// - `true` (rebasing codecs: PCM family, μ-law, A-law):
///   FIFO-pop the oldest pending record, ignoring
///   `data.timestamp()`. Chrome's PCM decoders rebase
///   `AudioData.timestamp` to (first_chunk_ts +
///   cumulative_us), so exact-ID lookup wouldn't work after
///   the first output anyway. PCM is non-reordering and
///   empirically 1-output-per-chunk under Chrome, so the
///   oldest pending record is always the right one. A
///   spec-allowed but unobserved multi-output split here
///   would surface as PTS/key drift but not as silent data
///   corruption — `bytes_size` accounting still applies to
///   the popped record's chunk.
///
/// Captures `state` and `rebases_timestamps`.
fn make_audio_output_handler(
  state: SharedState<DecodedAudioFrame>,
  rebases_timestamps: bool,
) -> Box<dyn FnMut(JsValue)> {
  Box::new(move |value: JsValue| {
    let Ok(data) = value.dyn_into::<web_sys::AudioData>() else {
      return;
    };
    let submission_id = data.timestamp() as i64;
    // Codex round 17: validate the side-map record FIRST,
    // *before* invoking `measure_audio_data_bytes`. The
    // measurement walks per-plane `allocation_size` queries
    // against the JS-supplied `numberOfChannels`, so an
    // unsolicited or stale `AudioData` advertising a huge
    // planar channel count could otherwise spin up a per-
    // plane loop before the fail-closed/orphan-close path
    // ran. (The cap inside `measure_audio_data_bytes`
    // bounds that loop independently, but the cleanest
    // protection is to skip measurement entirely for a
    // callback whose record we already know is stale.)
    //
    // `(Option<resolved>, just_closed)` — the bool fires the
    // underlying-decoder close hook once after the inner
    // borrow drops.
    let (resolved, just_closed) = 'budget: {
      let mut inner = state.borrow_mut();
      // Fail-closed on prior fatal error — see the matching
      // note in `video.rs`.
      if inner.is_closed() {
        break 'budget (None, false);
      }
      let current_epoch = inner.epoch();
      let floor = inner.epoch_id_floor();
      // Cross-flush race (codex round 4): a pre-flush
      // callback that survives `decoder.reset()` for any
      // reason would consume a post-flush record. The
      // primary defense lives in `install_audio_decoder` —
      // `flush()` rebuilds the entire decoder + closures, so
      // any pre-flush callback would invoke a dropped (and
      // therefore `wasm-bindgen`-invalidated) JS wrapper
      // before reaching this code. The epoch check below is
      // redundant defense that costs essentially nothing.
      //
      // Multi-output-per-chunk (codex round 5): the
      // WebCodecs spec allows the decoder to emit several
      // `AudioData` outputs per `decode()` call. Under FIFO
      // matching this would consume future packets'
      // records and decrement `pending_input_bytes` for
      // bytes still pinned in the JS decoder; under exact-
      // ID matching the first output retires the record and
      // subsequent outputs miss → fail-closed surfaces the
      // violation cleanly.
      let mut missing_close: bool = false;
      let record_opt = if rebases_timestamps {
        // PCM-family path: Chrome rebases `AudioData.timestamp`
        // to (first_chunk_ts + cumulative_us), so exact-ID
        // lookup misses every output past the first. PCM is
        // non-reordering and empirically 1-out-per-chunk in
        // Chrome, so the oldest pending record is always the
        // matching one. Stale-epoch entries drop silently.
        //
        // Multi-output defense (codex round 6): the
        // WebCodecs spec allows a decoder to emit several
        // `AudioData` outputs per `decode()`. Under naive
        // FIFO matching the second output for a chunk would
        // pop the *next* chunk's record and attach its
        // PTS/key to stale audio. PCM is uniquely amenable
        // to a sample-count check: each input packet's
        // expected sample count is `body_len / (bps *
        // channels)` for codecs where bytes-per-sample is
        // fixed (`expected_samples` field on the record).
        // If `data.number_of_frames` differs, fail closed.
        // Zero `expected_samples` means we don't have a
        // bytes/sample mapping for this codec (e.g. an
        // unrecognised `pcm-*` variant) and skip the check.
        let popped = inner.pop_oldest_pending_output();
        match popped {
          Some((_, record)) if record.epoch() == current_epoch => {
            let expected = record.expected_samples();
            // Rebasing codecs that survived `open_with_codec_string`
            // must have a known bytes-per-sample mapping (the
            // open-time gate enforces this), so `expected` is
            // strictly positive here. Defend against an
            // accidental zero in case a future code path
            // bypasses that gate: without the per-output
            // sample-count check, a multi-output split would
            // silently consume the next chunk's record. Fail
            // closed instead.
            if expected == 0 {
              let just_closed = inner.record_close(Error::from_js(JsValue::from_str(
                "WebCodecs audio output: rebasing-codec record has no \
                 expected_samples — cannot validate against multi-output \
                 split; refusing FIFO match",
              )));
              break 'budget (None, just_closed);
            }
            let actual = data.number_of_frames();
            if actual != expected {
              let just_closed = inner.record_close(Error::from_js(JsValue::from_str(&format!(
                "WebCodecs audio output: AudioData reported {actual} samples \
                   but the matching pending chunk encoded {expected} samples \
                   (suggests a multi-output codec split — refusing to attach \
                   stale audio to a wrong record)"
              ))));
              break 'budget (None, just_closed);
            }
            Some(record)
          }
          Some(_) => None,
          None => None,
        }
      } else {
        // Echoing-codec path: `data.timestamp()` is the
        // submission ID we stamped onto the input chunk.
        // Exact-ID lookup. Stale-epoch entries drop, IDs
        // below `floor` are pre-flush stragglers and drop,
        // current-generation misses fail closed (multi-
        // output edge case the spec allows but our accepted
        // codecs don't actually exhibit; surface it as an
        // error rather than scrambling subsequent records).
        match inner.remove_pending_output(submission_id) {
          Some(record) if record.epoch() == current_epoch => Some(record),
          Some(_) => None,
          None if submission_id < floor => None,
          None => {
            missing_close = inner.record_close(Error::from_js(JsValue::from_str(
              "WebCodecs audio output: current-generation submission_id \
               has no side-map entry (multi-output codec spec deviation)",
            )));
            None
          }
        }
      };
      let Some(record) = record_opt else {
        break 'budget (None, missing_close);
      };
      // Now that we have a current-generation record, it's
      // safe to measure. Walking per-plane `allocation_size`
      // for a stale callback above would have been wasted
      // work at best; with an unbounded planar channel
      // count from a malformed `AudioData`, codex round 17
      // pointed out it could also spin the JS thread
      // before the fail-closed path ran. The capped helper
      // below bounds the loop, and skipping it for stale
      // records eliminates the cost entirely.
      let measurement = measure_audio_data_bytes(&data);
      let total_bytes = match measurement {
        Some(t) if t <= MAX_FRAME_ALLOCATION_BYTES => t,
        Some(t) => {
          let just_closed = inner.record_close(Error::from_js(JsValue::from_str(&format!(
            "AudioData total allocation_size {t} > \
               MAX_FRAME_ALLOCATION_BYTES = {MAX_FRAME_ALLOCATION_BYTES}; \
               refusing admission"
          ))));
          break 'budget (None, just_closed);
        }
        None => {
          let just_closed = inner.record_close(Error::from_js(JsValue::from_str(
            "AudioData allocation_size measurement failed \
             (format absent, unknown, zero channels, or planar channel \
             count exceeds 8); refusing admission",
          )));
          break 'budget (None, just_closed);
        }
      };
      let new_frame_bytes: u64 = (total_bytes as u64).saturating_mul(2);
      let projected_bytes = inner
        .queue_bytes()
        .saturating_add(inner.pending_copy_bytes())
        .saturating_add(new_frame_bytes);
      if inner.pending_copies() >= MAX_PENDING_DECODE {
        let just_closed = inner.record_close(Error::from_js(JsValue::from_str(
          "WebCodecs output burst exceeded MAX_PENDING_DECODE; \
           audio frames would be lost",
        )));
        break 'budget (None, just_closed);
      }
      if projected_bytes > MAX_INFLIGHT_BYTES {
        let just_closed = inner.record_close(Error::from_js(JsValue::from_str(
          "WebCodecs output burst would exceed MAX_INFLIGHT_BYTES; \
           audio frames would be lost",
        )));
        break 'budget (None, just_closed);
      }
      inner.add_pending_copy(new_frame_bytes);
      (Some((record, current_epoch, new_frame_bytes)), false)
    };
    let Some((record, captured_epoch, new_frame_bytes)) = resolved else {
      data.close();
      if just_closed {
        state.invoke_close_hook();
      }
      state.wake_all();
      return;
    };
    // Build the admission guard *before* spawn_local to roll
    // back pending_copies + close the JS AudioData if the
    // task allocation panics under unwinding. Codex round 33.
    let admission = AudioCopyAdmission {
      state: state.clone(),
      data: Some(data),
      byte_estimate: new_frame_bytes,
      armed: true,
    };
    spawn_local(handle_audio_data(
      admission,
      state.clone(),
      captured_epoch,
      record,
      new_frame_bytes,
    ));
  })
}

/// Build the audio error handler.
fn make_audio_error_handler(state: SharedState<DecodedAudioFrame>) -> Box<dyn FnMut(JsValue)> {
  Box::new(move |value: JsValue| {
    // Drop the borrow before waking. Wake BOTH wakers so a
    // producer parked in `await_decode_room` unblocks
    // alongside the consumer — see the matching note in
    // `video.rs`.
    state.borrow_mut().record_close(Error::from_js(value));
    state.wake_all();
  })
}

/// Build the audio dequeue handler.
fn make_audio_dequeue_handler(state: SharedState<DecodedAudioFrame>) -> Box<dyn FnMut()> {
  Box::new(move || {
    // Wake both — see the matching note in `video.rs`.
    state.wake_all();
  })
}

/// Hard upper bound on the planar-format channel count
/// `measure_audio_data_bytes` is willing to iterate over.
/// Matches the 8-plane cap inside `copy_audio_data`'s
/// `mediadecode::AudioFrame` array. Codex round 17 flagged
/// that without an early cap, a stale or browser-version-
/// skewed `AudioData` advertising a huge `numberOfChannels`
/// could spin a per-plane `allocation_size` loop before any
/// caller-side fail-closed path ran.
const MAX_PLANAR_CHANNELS_FOR_MEASUREMENT: u32 = 8;

/// Sum the byte size of every plane `copy_audio_data` will
/// pull out of `data`. Returns `None` when the AudioData is
/// unusable — caller must fail closed on `None`. Specific
/// `None`-returning conditions:
///
/// - `format()` is absent or unrecognised.
/// - `numberOfChannels` is zero.
/// - `numberOfChannels` exceeds
///   `MAX_PLANAR_CHANNELS_FOR_MEASUREMENT` for a planar
///   format. Capping here (instead of letting the loop
///   run) keeps the measurement constant-time even for an
///   adversarial AudioData; the same 8-plane limit is
///   enforced again inside `copy_audio_data`, but only
///   after the JS heap has already been touched per plane.
/// - Any `allocation_size` query failed, or the running
///   sum overflowed `u32`.
fn measure_audio_data_bytes(data: &web_sys::AudioData) -> Option<u32> {
  let format_str = data.format()?;
  let format = SampleFormat::from_spec_name(audio_sample_format_name(format_str))?;
  let channels = data.number_of_channels();
  if channels == 0 {
    return None;
  }
  let plane_count: u32 = if format.is_planar() {
    if channels > MAX_PLANAR_CHANNELS_FOR_MEASUREMENT {
      return None;
    }
    channels
  } else {
    1
  };
  let mut total: u32 = 0;
  for plane in 0..plane_count {
    let opts = web_sys::AudioDataCopyToOptions::new(plane);
    let plane_size = data.allocation_size(&opts).ok()?;
    total = total.checked_add(plane_size)?;
  }
  Some(total)
}

/// Bytes per sample for a `web_sys::AudioSampleFormat`.
/// Returns `0` for any value the WebCodecs spec doesn't
/// define (caller fails closed). Used by `copy_audio_data`'s
/// per-plane size validation (codex round 23).
fn audio_sample_format_bytes(fmt: web_sys::AudioSampleFormat) -> u32 {
  use web_sys::AudioSampleFormat as W;
  match fmt {
    W::U8 | W::U8Planar => 1,
    W::S16 | W::S16Planar => 2,
    W::S32 | W::S32Planar | W::F32 | W::F32Planar => 4,
    _ => 0,
  }
}

/// `web_sys::AudioSampleFormat` → spec-string conversion.
fn audio_sample_format_name(fmt: web_sys::AudioSampleFormat) -> &'static str {
  use web_sys::AudioSampleFormat as W;
  match fmt {
    W::U8 => "u8",
    W::S16 => "s16",
    W::S32 => "s32",
    W::F32 => "f32",
    W::U8Planar => "u8-planar",
    W::S16Planar => "s16-planar",
    W::S32Planar => "s32-planar",
    W::F32Planar => "f32-planar",
    _ => "",
  }
}

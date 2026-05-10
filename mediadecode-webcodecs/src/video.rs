//! `mediadecode::future::local::VideoStreamDecoder` impl backed by
//! `web_sys::VideoDecoder`.
//!
//! WebCodecs is fundamentally async + `!Send`: the API is
//! callback-driven (`output` / `error`), every value held during a
//! decode is a `JsValue` (or a `Closure` / `Promise`), and the
//! browser's event loop is single-threaded anyway. This adapter
//! therefore implements only the async (`local`) variant of
//! `VideoStreamDecoder` — there is no sync impl.
//!
//! Internally the decoder still runs a frame queue: the `output`
//! callback `spawn_local`s a copy task that pulls bytes out of the
//! JS-side `VideoFrame` (only available via async `copyTo`),
//! pushes the result onto the queue, and wakes the receive future.
//! `receive_frame` is a `poll_fn` that drains the queue, returns
//! [`VideoDecodeError::Eof`] once `send_eof` has completed and the
//! drain is empty, or registers a waker and yields otherwise.
//!
//! # Backpressure
//!
//! `send_packet` awaits while `VideoDecoder.decodeQueueSize >=
//! `[`MAX_DECODE_QUEUE`]. Without this the caller can submit
//! packets faster than the browser can decode them and faster
//! than `receive_frame` can drain the resulting frames, which
//! grows both the WebCodecs internal queue and the Rust output
//! queue without bound. The `dequeue` event fires whenever a
//! chunk leaves the WebCodecs internal queue; we hook it to wake
//! the producer's waker when there's room again.

use std::{num::NonZeroU32, sync::Arc};

use mediadecode::{
  Timebase, Timestamp,
  color::{ColorInfo, ColorMatrix, ColorPrimaries, ColorRange, ColorTransfer},
  frame::{Dimensions, Plane, Rect, VideoFrame},
  future::local::VideoStreamDecoder,
  packet::{PacketFlags, VideoPacket},
  pixel_format::PixelFormat,
};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::{
  adapter::WebCodecs,
  buffer::WebCodecsBuffer,
  codec_id::VideoCodecId,
  codec_string,
  dispatch::{
    allocate_value_handler, allocate_void_handler, free_value_handler, free_void_handler,
    make_value_trampoline, make_void_trampoline,
  },
  error::{Error, VideoDecodeError},
  extras::{VideoFrameExtra, VideoPacketExtra},
  state::{DecodedFrame, PendingOutput, PendingPush, SharedState},
};

/// Soft cap on the **decoder-side** pipeline:
/// `decode_queue_size + pending_copies`. `send_packet` awaits
/// on this cap because the decoder side drains *on its own* —
/// output callbacks fire from the JS event loop without any
/// consumer action, so awaiting cannot deadlock.
///
/// Sized to absorb a complete reorder-buffer release at EOF:
/// h.264 / hevc max DPB depth is ~16, so 32 leaves headroom
/// for the in-flight admission count plus the codec's
/// internal buffer emptying simultaneously when
/// `decoder.flush()` resolves.
pub const MAX_PENDING_DECODE: u32 = 32;

/// Hard cap on a single video frame's `allocation_size`.
/// Catches malformed streams claiming an absurd allocation in
/// one allocation. Aggregate memory is bounded separately by
/// [`MAX_INFLIGHT_BYTES`].
pub const MAX_FRAME_ALLOCATION_BYTES: u32 = 256 * 1024 * 1024; // 256 MiB

/// Aggregate byte budget across `pending_copies + queue.len()`.
/// Browser tabs share a finite wasm linear memory (~2–4 GiB
/// max, often less in practice) and a JS heap that pays for
/// every `Uint8Array`-backed `copyTo` destination. Bounding
/// total in-flight pixel bytes keeps the adapter polite to
/// other JS state.
///
/// `send_packet` estimates per-frame bytes as
/// `coded_width × coded_height × 4` (worst-case 4 bytes per
/// pixel, conservative across NV12 / P010 / RGBA) and applies
/// `OutputFull` when the projected aggregate would exceed
/// this cap. The 4 bytes/pixel constant is intentionally
/// pessimistic — for NV12 (~1.5 B/px) the budget pipelines
/// fewer frames than strictly necessary, but the trade vs.
/// risk-of-OOM-in-a-shared-process leans defensive.
pub const MAX_INFLIGHT_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB

/// Hard cap on codec-private description bytes (avcC / hvcC /
/// similar) accepted at decoder open. The container is the
/// source of this blob and an adversarial demuxer could
/// otherwise force an arbitrary JS `Uint8Array` allocation at
/// open, bypassing the per-packet caps that only run in
/// `send_packet`. 64 KiB sits well above any realistic codec
/// config record. Codex round 35 [accepted].
pub const MAX_CODEC_DESCRIPTION_BYTES: usize = 64 * 1024;

/// Hard cap on a single encoded video packet's compressed
/// byte size. Catches malformed bitstreams claiming an absurd
/// chunk in one packet (e.g. fuzz-corrupted MP4 samples).
/// Sized to admit the largest realistic intra-only HEVC
/// keyframe at 8K10b plus headroom while still rejecting
/// pathological inputs before they reach the JS heap.
pub const MAX_INPUT_PACKET_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

/// Aggregate cap on compressed bytes pinned in the JS heap
/// (across `EncodedVideoChunk`s waiting in
/// `decoder.decodeQueueSize` plus the codec's reorder
/// buffer). Tracked via [`crate::state::Inner::pending_input_bytes`]
/// — incremented when `send_packet` admits a chunk,
/// decremented when the matching output callback (or
/// `bump_epoch` / `record_close`) drops the side-map entry.
/// Hitting the cap surfaces as a `Closed` error: a
/// reorder-stalled or zero-output stream can't recover by
/// further input pressure, so the user must `flush()` to
/// reset the JS decoder.
pub const MAX_INPUT_INFLIGHT_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

/// Soft cap on the **consumer-side** output queue. `send_packet`
/// **does not** await on this — the only path that drains the
/// queue is `receive_frame`, which also takes `&mut self` on
/// the decoder, so awaiting here would deadlock. Instead, when
/// the queue is at the cap, `send_packet` returns
/// [`VideoDecodeError::OutputFull`] and the caller drives the
/// drain themselves (typically by interleaving `send_packet`
/// and `receive_frame` calls).
///
/// Peak in-flight memory is bounded by
/// `(MAX_PENDING_DECODE + MAX_QUEUED_OUTPUT) ×
/// allocation_size_per_frame` — the decoder side can complete
/// up to `MAX_PENDING_DECODE` more frames into the queue while
/// `send_packet` is parked, so the actual queue length can
/// briefly exceed `MAX_QUEUED_OUTPUT` by that delta before the
/// next admission is blocked.
pub const MAX_QUEUED_OUTPUT: u32 = 16;

/// One CPU-side video frame produced by the WebCodecs `output`
/// callback after `copyTo` resolves. Planes share a single
/// `Arc<[u8]>` allocation via [`WebCodecsBuffer`]; unused plane
/// slots hold [`WebCodecsBuffer::empty`] with stride `0`.
pub(crate) struct DecodedVideoFrame {
  pts: Option<Timestamp>,
  duration: Option<Timestamp>,
  dimensions: Dimensions,
  visible_rect: Option<Rect>,
  format: PixelFormat,
  planes: [(WebCodecsBuffer, u32); 4],
  plane_count: u8,
  key: bool,
  byte_size: u32,
  color: ColorInfo,
}

impl DecodedVideoFrame {
  /// Construct a CPU-side frame body. Called from
  /// [`copy_video_frame`] after `copyTo` resolves; not
  /// intended for external use.
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    pts: Option<Timestamp>,
    duration: Option<Timestamp>,
    dimensions: Dimensions,
    visible_rect: Option<Rect>,
    format: PixelFormat,
    planes: [(WebCodecsBuffer, u32); 4],
    plane_count: u8,
    key: bool,
    byte_size: u32,
    color: ColorInfo,
  ) -> Self {
    Self {
      pts,
      duration,
      dimensions,
      visible_rect,
      format,
      planes,
      plane_count,
      key,
      byte_size,
      color,
    }
  }

  /// User's PTS (the JS-side timestamp is the submission ID,
  /// so the real PTS is restored from the side-map record).
  pub const fn pts(&self) -> Option<Timestamp> {
    self.pts
  }
  /// Frame duration as reported by `VideoFrame.duration`.
  pub const fn duration(&self) -> Option<Timestamp> {
    self.duration
  }
  /// Coded dimensions (matches the `copyTo` rect this body
  /// was built against).
  pub const fn dimensions(&self) -> Dimensions {
    self.dimensions
  }
  /// Visible rect read from `VideoFrame.visibleRect`, if
  /// reported.
  pub const fn visible_rect(&self) -> Option<Rect> {
    self.visible_rect
  }
  /// Pixel format mapped from `VideoFrame.format`.
  pub const fn format(&self) -> PixelFormat {
    self.format
  }
  /// Consume `self` and return the planes by value (so the
  /// caller can destructure them without cloning the
  /// `WebCodecsBuffer` refcounts more than necessary).
  pub fn into_planes(self) -> [(WebCodecsBuffer, u32); 4] {
    self.planes
  }
  /// Number of planes used (the array tail is empty placeholders).
  pub const fn plane_count(&self) -> u8 {
    self.plane_count
  }
  /// Whether the originating chunk was a key chunk.
  pub const fn key(&self) -> bool {
    self.key
  }
  /// Bytes of the underlying contiguous allocation. See the
  /// `byte_size` doc on the field for why we track this rather
  /// than projecting from `coded_width × coded_height × 4`.
  pub const fn byte_size(&self) -> u32 {
    self.byte_size
  }
  /// Color metadata mapped from `VideoFrame.colorSpace`.
  pub const fn color(&self) -> ColorInfo {
    self.color
  }
}

const MICROS: Timebase = match NonZeroU32::new(1_000_000) {
  Some(d) => Timebase::new(1, d),
  None => unreachable!(),
};

/// `mediadecode::future::local::VideoStreamDecoder` impl wrapping
/// `web_sys::VideoDecoder`.
pub struct WebCodecsVideoStreamDecoder {
  decoder: web_sys::VideoDecoder,
  /// Configuration captured at `open` time. WebCodecs `reset()`
  /// returns the decoder to the unconfigured state, so `flush()`
  /// re-applies this config to keep the decoder reusable across
  /// seeks / format-stable resets.
  config: web_sys::VideoDecoderConfig,
  state: SharedState<DecodedVideoFrame>,
  time_base: Timebase,
  // Closures kept alive for the lifetime of the decoder. JS holds
  // them via `init_dict.output(...)`, but Rust must own them to
  // keep the underlying function pointer valid.
  /// Trampoline slot IDs for the JS-side callbacks; see
  /// [`crate::dispatch`] for the design and the matching
  /// audio adapter for prior art.
  output_slot_id: u64,
  error_slot_id: u64,
  dequeue_slot_id: u64,
  /// `true` once the `send_eof` future has resolved. The async
  /// `receive_frame` returns `Eof` after this when the queue
  /// drains and no copy tasks remain.
  eof: bool,
}

impl WebCodecsVideoStreamDecoder {
  /// Open a decoder for a known [`VideoCodecId`].
  ///
  /// Returns [`VideoDecodeError::UnsupportedCodec`] for codecs that
  /// require extradata-derived codec strings (H.264, HEVC, VP9, AV1)
  /// — use [`Self::open_with_codec_string`] in that case.
  pub fn open(
    codec: VideoCodecId,
    extradata: Option<&[u8]>,
    coded_dimensions: Dimensions,
    time_base: Timebase,
  ) -> Result<Self, VideoDecodeError> {
    let codec_string = codec_string::for_video(codec, extradata)?;
    Self::open_with_codec_string(&codec_string, extradata, coded_dimensions, time_base)
  }

  /// Open a decoder with an explicit WebCodecs codec string
  /// (`"avc1.640028"`, `"vp09.00.10.08.420.0.1.1.1"`, …) and the
  /// raw codec configuration record (extradata). Use this when
  /// the caller has already parsed extradata (typically JS-side
  /// via `mp4box.js`).
  ///
  /// `description`, when `Some`, is forwarded to
  /// [`VideoDecoderConfig.description`](https://www.w3.org/TR/webcodecs/#dom-videodecoderconfig-description).
  /// For MP4-demuxed AVC / HEVC the description **must** be the
  /// codec configuration record (AVCDecoderConfigurationRecord
  /// / HEVCDecoderConfigurationRecord); without it WebCodecs
  /// falls back to Annex B byte-stream parsing and length-
  /// prefixed NALU samples fail to decode. VP8 / VP9 / AV1 do
  /// not require it but accept it as configOBUs / similar.
  pub fn open_with_codec_string(
    codec_string: &str,
    description: Option<&[u8]>,
    coded_dimensions: Dimensions,
    time_base: Timebase,
  ) -> Result<Self, VideoDecodeError> {
    // Preallocate the output queue to the worst-case
    // simultaneously-queued frame count: the producer-side
    // cap (`MAX_QUEUED_OUTPUT`) plus one decoded-batch worth
    // of in-flight frames that may still drain after the cap
    // is reached (`MAX_PENDING_DECODE`). Admission gates the
    // sum so `push_queue` never reallocates (codex round 27).
    let state = SharedState::<DecodedVideoFrame>::try_new(
      MAX_QUEUED_OUTPUT as usize + MAX_PENDING_DECODE as usize,
    )
    .map_err(VideoDecodeError::Js)?;

    // ---- output callback: wraps each VideoFrame into a
    // copy task. Falls back to a conservative bound derived
    // from each frame's reported coded dimensions (not the
    // open-time projection — see codex round 12 fix below)
    // when `allocation_size_with_options` throws.
    let output_handler: Box<dyn FnMut(JsValue)> = {
      let state = state.clone();
      Box::new(move |value: JsValue| {
        let Ok(frame) = value.dyn_into::<web_sys::VideoFrame>() else {
          return;
        };
        let submission_id = frame.timestamp() as i64;
        // `frame.allocation_size()` (no options) reports the
        // **visibleRect** size by default, but `copy_video_frame`
        // copies the full coded rectangle. The two can differ
        // (a 1920×1088 coded frame with a 1920×1080 visible
        // rect has ~0.7% smaller default-rect allocation), so
        // budgeting against the default would under-count
        // every frame. Build the same full-rect options we
        // pass to `copyTo` and query against those, then
        // double — `copy_video_frame` peaks at JS Uint8Array
        // + Rust Vec coexisting briefly during the `copyTo`
        // settle.
        // `allocation_size_with_options` is the only
        // measurement we trust for the budget. Codex round
        // 12 had us fall back to a `width × height × 4`
        // projection on measurement failure; codex round 13
        // pointed out that 12-bit RGBA is 6 bytes/pixel
        // (and similar for higher-depth planar formats), so
        // any fallback hard-coded at 4 bpp under-counts.
        // Rather than maintain a fragile worst-case-bytes-
        // per-format table that would need to track the
        // WebCodecs registry, fail closed when measurement
        // `(Option<resolved>, just_closed)` — the bool is set
        // when this callback transitioned `last_error` from
        // unset to set (a record_close), so we know to fire
        // the underlying-decoder close hook *once* after
        // dropping the inner borrow.
        let (resolved, just_closed) = 'budget: {
          let mut inner = state.borrow_mut();
          // Fail-closed: if any prior path has set
          // `last_error` (e.g. `pending_outputs` overflow,
          // queue overflow, copy concurrency cap, JS error
          // callback), refuse new copy work entirely.
          // Without this, late output callbacks keep
          // allocating after the adapter has reported
          // `Closed` to the user — a real memory leak in
          // browser/wasm contexts where the host page
          // typically holds the adapter for a long time.
          if inner.is_closed() {
            break 'budget (None, false);
          }
          // Generation validation FIRST — codex round 15:
          // resolving the side-map record before measuring
          // the frame ensures a stale (pre-flush) callback
          // delivered after `bump_epoch` drops cleanly even
          // if its `allocation_size_with_options` would
          // throw. The previous order let a measurement-
          // error stale callback `record_close` the freshly
          // flushed decoder, turning a recoverable seek into
          // a user-visible `Closed`.
          let current_epoch = inner.epoch();
          let floor = inner.epoch_id_floor();
          let lookup = inner.remove_pending_output(submission_id);
          let mut missing_close: bool = false;
          let record_opt = match lookup {
            Some(record) if record.epoch() == current_epoch => Some(record),
            Some(_) => None,
            None if submission_id < floor => None,
            None => {
              // Current-generation timestamp miss for video.
              // Chromium echoes the submission ID we stamped
              // onto every video chunk we admit, so this
              // branch only fires on a multi-output codec
              // split or a spec-violation. Fail-closed
              // surfaces the violation instead of FIFO-
              // popping (which would steal another packet's
              // PTS/key — h264/hevc B-frame reordering
              // means output order ≠ input order even with
              // echoed timestamps).
              missing_close = inner.record_close(Error::from_js(JsValue::from_str(
                "WebCodecs video output: current-generation submission_id \
                 has no side-map entry; refusing to fabricate a record \
                 (would corrupt PTS for reordered codecs)",
              )));
              None
            }
          };
          let Some(record) = record_opt else {
            break 'budget (None, missing_close);
          };
          // Now we have a current-generation record — only
          // now is it safe to measure the JS frame and fail
          // closed on a measurement error or burst-cap trip,
          // because we've already verified the callback
          // belongs to *this* decoder instance.
          //
          // Codex round 32 [accepted]: previously the `× 2`
          // Vec→Arc copy peak measurement and its
          // `DomRectInit` / `VideoFrameCopyToOptions` JS
          // allocations ran *before* this validation. A late
          // callback delivered after `reset()` or a fatal
          // close still constructed the option objects and
          // called into WebCodecs even though the frame was
          // about to be orphan-closed. Moving the
          // measurement here means stale frames consume zero
          // JS allocator pressure on the closed-decoder path.
          let new_frame_bytes: u64 = {
            let copy_rect = web_sys::DomRectInit::new();
            copy_rect.set_x(0.0);
            copy_rect.set_y(0.0);
            copy_rect.set_width(frame.coded_width() as f64);
            copy_rect.set_height(frame.coded_height() as f64);
            let copy_opts = web_sys::VideoFrameCopyToOptions::new();
            copy_opts.set_rect(&copy_rect);
            match frame.allocation_size_with_options(&copy_opts) {
              Ok(n) => u64::from(n).saturating_mul(2),
              Err(_) => {
                let just_closed = inner.record_close(Error::from_js(JsValue::from_str(
                  "WebCodecs VideoFrame.allocation_size_with_options failed; \
                   cannot enforce per-frame byte cap, refusing admission",
                )));
                // The pending record was already removed
                // from the side map by the lookup above; the
                // close path doesn't need to put it back
                // since the decoder is now closed.
                break 'budget (None, just_closed);
              }
            }
          };
          // Exact accounting: every term is a measured byte
          // total (queued frames carry `byte_size`, pending
          // copies are tracked in `pending_copy_bytes` from
          // this admission + every prior live admission).
          // This is the upper bound the adapter actually
          // pins in browser memory.
          // Aggregate accounting includes
          // `pending_push_bytes` so out-of-order completions
          // parked in the ordering buffer are visible to
          // `MAX_INFLIGHT_BYTES` (codex round 19). Without
          // this, a slow first sequence could let many
          // later copies pile up in `pending_pushes` past
          // the documented cap.
          let projected_bytes = inner
            .queue_bytes()
            .saturating_add(inner.pending_copy_bytes())
            .saturating_add(inner.pending_push_bytes())
            .saturating_add(new_frame_bytes);
          if inner.pending_copies() >= MAX_PENDING_DECODE {
            let just_closed = inner.record_close(Error::from_js(JsValue::from_str(
              "WebCodecs output burst exceeded MAX_PENDING_DECODE; \
               video frames would be lost",
            )));
            break 'budget (None, just_closed);
          }
          if new_frame_bytes > MAX_FRAME_ALLOCATION_BYTES as u64 {
            // Single-frame cap. The decoder's per-frame
            // copy site also enforces this, but catching it
            // here drops the JS frame immediately rather
            // than spawning a copy task that's guaranteed
            // to fail.
            let just_closed = inner.record_close(Error::from_js(JsValue::from_str(
              "WebCodecs decoded frame allocation_size exceeded \
               MAX_FRAME_ALLOCATION_BYTES",
            )));
            break 'budget (None, just_closed);
          }
          if projected_bytes > MAX_INFLIGHT_BYTES {
            // Output-side byte enforcement. The admission-
            // side check in `send_packet` is bypassed by
            // every output callback that follows admission,
            // so a flush / reorder burst could spawn many
            // copies each allocating up to
            // `MAX_FRAME_ALLOCATION_BYTES` before any later
            // cleanup runs.
            let just_closed = inner.record_close(Error::from_js(JsValue::from_str(
              "WebCodecs output burst would exceed MAX_INFLIGHT_BYTES; \
               video frames would be lost",
            )));
            break 'budget (None, just_closed);
          }
          inner.add_pending_copy(new_frame_bytes);
          (Some((record, current_epoch, new_frame_bytes)), false)
        };
        let Some((record, captured_epoch, new_frame_bytes)) = resolved else {
          // Stale, unknown, or over-cap — close the JS frame
          // to release the GPU surface. If this callback was
          // the one that flipped `last_error`, also close the
          // underlying WebCodecs decoder so its encoded-chunk
          // queue drains immediately instead of waiting for
          // the user to flush() / drop. Wake waiters in case
          // we just set `last_error`.
          frame.close();
          if just_closed {
            state.invoke_close_hook();
          }
          state.wake_all();
          return;
        };
        // Assign a sequence in output() order. The async
        // copy task delivers via `deliver_pending_push`,
        // which keeps the queue's push order matching the
        // sequence even when copy Promises resolve out of
        // order (codex round 18).
        let sequence = state.allocate_output_sequence();
        // Build the admission guard *before* spawn_local so an
        // unwinding allocation failure inside spawn_local rolls
        // back pending_copies and closes the JS frame instead of
        // stranding both. The guard is moved into the future and
        // disarmed as the future's first statement (see
        // `VideoCopyAdmission`). Codex round 33.
        let admission = VideoCopyAdmission {
          state: state.clone(),
          frame: Some(frame),
          byte_estimate: new_frame_bytes,
          sequence,
          armed: true,
        };
        spawn_local(handle_video_frame(
          admission,
          state.clone(),
          captured_epoch,
          record,
          new_frame_bytes,
          sequence,
        ));
      })
    };

    // ---- error callback: stash the failure, wake any waiter.
    let error_handler: Box<dyn FnMut(JsValue)> = {
      let state = state.clone();
      Box::new(move |value: JsValue| {
        // Hold the borrow only long enough to write the error,
        // then drop it before invoking the wakers. Wake BOTH —
        // a producer parked in `await_decode_room` on
        // `dequeue_waker` would otherwise sleep forever (no
        // future `dequeue` event will fire on a closed decoder),
        // stranding the future on a dead pipeline.
        //
        // The error came from the underlying decoder so it has
        // already entered its "closed" / "errored" state; we
        // skip `invoke_close_hook` here (close() on a
        // closed decoder is a no-op anyway, but avoiding the
        // call also keeps the JS-bridge round-trip count down).
        state.borrow_mut().record_close(Error::from_js(value));
        state.wake_all();
      })
    };

    // ---- dequeue callback: wakes any pending `send_packet`
    // backpressure waiter when WebCodecs decodes a chunk and
    // its internal queue drops. Also wakes the receiver: a
    // `receive_frame` parked on `decode_queue_size > 0`
    // needs to re-poll when that count changes.
    let dequeue_handler: Box<dyn FnMut()> = {
      let state = state.clone();
      Box::new(move || {
        state.wake_all();
      })
    };

    // Codex round 28 [accepted]: each allocation can fail
    // with OOM. Free earlier slots if a later one fails so
    // an installed-but-orphaned trampoline never accrues.
    let output_slot_id = allocate_value_handler(output_handler).map_err(VideoDecodeError::Js)?;
    let error_slot_id = match allocate_value_handler(error_handler) {
      Ok(id) => id,
      Err(err) => {
        free_value_handler(output_slot_id);
        return Err(VideoDecodeError::Js(err));
      }
    };
    let dequeue_slot_id = match allocate_void_handler(dequeue_handler) {
      Ok(id) => id,
      Err(err) => {
        free_value_handler(output_slot_id);
        free_value_handler(error_slot_id);
        return Err(VideoDecodeError::Js(err));
      }
    };
    let output_trampoline = make_value_trampoline(output_slot_id);
    let error_trampoline = make_value_trampoline(error_slot_id);
    let dequeue_trampoline = make_void_trampoline(dequeue_slot_id);

    let init = web_sys::VideoDecoderInit::new(&error_trampoline, &output_trampoline);
    let decoder = match web_sys::VideoDecoder::new(&init) {
      Ok(d) => d,
      Err(err) => {
        free_value_handler(output_slot_id);
        free_value_handler(error_slot_id);
        free_void_handler(dequeue_slot_id);
        return Err(VideoDecodeError::Js(Error::from_js(err)));
      }
    };
    decoder.set_ondequeue(Some(&dequeue_trampoline));

    // Build the config first, then configure FIRST, then
    // publish the close_hook. Codex round 13 flagged that
    // a configure failure here used to leak the slot map
    // entries (and the close-hook reference, on the prior
    // ordering). The atomic install pattern: any failure
    // before close_hook is published just frees its own
    // resources; a failure after close_hook publish would
    // need to clear the hook (no such failure path exists
    // today, since `set_close_hook` is the last fallible
    // step's logical successor).
    let config = web_sys::VideoDecoderConfig::new(codec_string);
    config.set_coded_width(coded_dimensions.width());
    config.set_coded_height(coded_dimensions.height());
    if let Some(bytes) = description {
      // Codex round 35 [accepted]: cap codec-description
      // size before any JS allocation. `description` is
      // demuxer-controlled codec-private data (avcC/hvcC
      // and similar); a malicious container could otherwise
      // present a multi-MiB blob and force a corresponding
      // `Uint8Array` allocation at open, bypassing the
      // packet-byte caps that only apply to `send_packet`.
      // 64 KiB is far above any realistic codec config record
      // (typical avcC ≈ 50–200 bytes; HEVC hvcC with
      // multiple SPS / PPS / VPS ≈ a few KiB max).
      if bytes.len() > MAX_CODEC_DESCRIPTION_BYTES {
        free_value_handler(output_slot_id);
        free_value_handler(error_slot_id);
        free_void_handler(dequeue_slot_id);
        return Err(VideoDecodeError::Js(Error::from_static(
          "codec description exceeds MAX_CODEC_DESCRIPTION_BYTES",
        )));
      }
      // Build a JS-owned copy of the codec configuration
      // record (no `Uint8Array::view` over wasm linear
      // memory — `configure()` is sync but the spec is
      // free to keep the buffer alive on the JS side, and
      // we don't want a `memory.grow` to invalidate the
      // view). Fallible construction (codex round 21).
      let arr = match try_new_uint8_array(bytes.len() as u32) {
        Ok(a) => a,
        Err(err) => {
          free_value_handler(output_slot_id);
          free_value_handler(error_slot_id);
          free_void_handler(dequeue_slot_id);
          return Err(VideoDecodeError::Js(Error::from_js(err)));
        }
      };
      arr.copy_from(bytes);
      config.set_description_u8_array(&arr);
    }
    if let Err(err) = decoder.configure(&config) {
      let _ = decoder.close();
      free_value_handler(output_slot_id);
      free_value_handler(error_slot_id);
      free_void_handler(dequeue_slot_id);
      return Err(VideoDecodeError::Js(Error::from_js(err)));
    }

    // Now publish the close hook. Adapter-internal fatal
    // closes call this hook to drop any JS-side encoded
    // queue immediately. Use `reset()` rather than `close()`:
    // `close()` puts the decoder in the terminal "closed"
    // state, after which `reset()` / `configure()` (the
    // calls `flush()` makes to recover) both throw
    // `InvalidStateError`. With `reset()` the decoder
    // transitions to "unconfigured" — encoded queue dropped,
    // pending decode work cancelled — and the user's
    // documented `flush()` recovery path still works.
    state.set_close_hook_video(decoder.clone());

    Ok(Self {
      decoder,
      config,
      state,
      time_base,
      output_slot_id,
      error_slot_id,
      dequeue_slot_id,
      eof: false,
    })
  }

  /// The codec time base passed at construction, used to translate
  /// `Timestamp` PTS / duration to / from WebCodecs microseconds.
  pub const fn time_base(&self) -> Timebase {
    self.time_base
  }

  fn check_closed(&self) -> Result<(), VideoDecodeError> {
    if let Some(err) = self.state.borrow().last_error_clone() {
      return Err(VideoDecodeError::Closed(err));
    }
    Ok(())
  }

  /// Admission control for `send_packet`. Returns:
  ///
  /// - `Err(Closed)` if the decoder has errored.
  /// - `Err(OutputFull)` *immediately* (no await) when the
  ///   output queue is at [`MAX_QUEUED_OUTPUT`]. Caller drains
  ///   via `receive_frame` and retries (cannot await here —
  ///   would deadlock on `&mut self`).
  /// - `Ok(())` once `decode_queue_size + pending_copies <
  ///   MAX_PENDING_DECODE`. Awaits while the cap is closed,
  ///   waking on the WebCodecs `dequeue` event.
  ///
  /// The decoder-side cap is gated on
  /// [`web_sys::VideoDecoder::decode_queue_size`] — the
  /// browser's count of chunks waiting in its input queue.
  /// We **cannot** gate on `pending_outputs.len()`: that map
  /// also holds chunks the codec has accepted but is buffering
  /// internally for B-frame reordering / DTS/PTS reorder, and
  /// those entries only drain when subsequent input arrives.
  /// Gating on them would deadlock streams whose first N
  /// chunks reorder before output (a producer parked at the
  /// cap can't supply the input the decoder is waiting on).
  /// Browser internal pipelines self-bound; we trust them.
  async fn await_decode_room(&self) -> Result<(), VideoDecodeError> {
    loop {
      {
        let inner = self.state.borrow();
        if let Some(err) = inner.last_error_clone() {
          return Err(VideoDecodeError::Closed(err));
        }
        if inner.queue_len() >= MAX_QUEUED_OUTPUT as usize {
          return Err(VideoDecodeError::OutputFull);
        }
        // Aggregate byte budget. Three terms:
        //
        //   `queue_bytes` — exact bytes pinned in `queue`.
        //   `pending_copy_bytes` — exact bytes pinned in
        //     spawned copy tasks (`× 2` factor at admission
        //     covers the JS Uint8Array + Rust Vec peak).
        //   `last_measured_frame_bytes` — measured headroom
        //     from the most recent output_cb. `0` until the
        //     first output arrives, so the first chunk
        //     admits unconditionally; thereafter the budget
        //     reserves the size of an actual decoded frame
        //     rather than an open-time worst-case projection
        //     (which would mis-reject 8K streams whose true
        //     output is well under the worst-case bound).
        //
        // Reorder backlog (entries already in `pending_outputs`
        // for which `output_cb` hasn't fired) is *not*
        // reserved here — round-14 review found that
        // deadlocks reorder-heavy codecs.
        let in_flight_bytes = inner
          .queue_bytes()
          .saturating_add(inner.pending_copy_bytes())
          .saturating_add(inner.pending_push_bytes())
          .saturating_add(inner.last_measured_frame_bytes());
        // `>` rather than `>=` so the exact-cap boundary is
        // admissible: the output-side enforcement also uses
        // `>` (a frame whose `allocation_size_with_options`
        // lands exactly at the cap pushes through), and a
        // stale `last_measured_frame_bytes` of cap-size
        // would otherwise wedge admission forever once the
        // pipeline drains.
        if in_flight_bytes > MAX_INFLIGHT_BYTES {
          return Err(VideoDecodeError::OutputFull);
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

/// Decoder-side budget = browser's `decodeQueueSize` (chunks
/// waiting in the input queue, drained by `dequeue` events) +
/// `pending_copies` (frames being copied off the GPU) +
/// `pending_pushes_len()` (frames whose copy completed but
/// are parked waiting for an earlier sequence to drain).
///
/// Codex round 25 [accepted] flagged that omitting
/// `pending_pushes_len` here let a stalled head copy plus
/// many tiny later completions retain unbounded entries in
/// the reorder buffer: each completion decremented
/// `pending_copies`, opening the admission gate, while the
/// completed frame sat in `pending_pushes` waiting for the
/// head — so `pending_pushes` could grow until
/// `MAX_INFLIGHT_BYTES` tripped (or, with tiny frames, until
/// node allocations themselves OOM'd in a JS callback
/// context). Folding it in here makes the *total* in-flight
/// count (`decode_queue_size + pending_copies +
/// pending_pushes_len`) bounded by `MAX_PENDING_DECODE`, so
/// `pending_pushes_len` alone is bounded by the same number
/// (matches `MAX_PENDING_PUSHES`).
///
/// Does NOT include `pending_outputs.len()` — see the
/// [`WebCodecsVideoStreamDecoder::await_decode_room`] doc for
/// why gating on the side map deadlocks reorder-buffered
/// streams.
fn pending_decode(
  decoder: &web_sys::VideoDecoder,
  inner: &crate::state::Inner<DecodedVideoFrame>,
) -> usize {
  (decoder.decode_queue_size() as usize)
    .saturating_add(inner.pending_copies() as usize)
    .saturating_add(inner.pending_pushes_len())
}

impl Drop for WebCodecsVideoStreamDecoder {
  fn drop(&mut self) {
    // Trampoline-based teardown is uncomplicated: free the
    // dispatcher slots and close the decoder. Late callbacks
    // for this decoder fire into the always-callable
    // dispatcher, which finds their slot empty and orphan-
    // closes the `VideoFrame`. No `Closure` to invalidate,
    // no `Rc<SharedState>` retained past this scope.
    self.state.bump_epoch();
    self.decoder.set_ondequeue(None);
    let _ = self.decoder.close();
    self.state.clear_close_hook();
    free_value_handler(self.output_slot_id);
    free_value_handler(self.error_slot_id);
    free_void_handler(self.dequeue_slot_id);
  }
}

impl VideoStreamDecoder for WebCodecsVideoStreamDecoder {
  type Adapter = WebCodecs;
  type Buffer = WebCodecsBuffer;
  type Error = VideoDecodeError;

  async fn send_packet(
    &mut self,
    packet: &VideoPacket<VideoPacketExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    self.check_closed()?;
    if self.eof {
      // `send_eof` already drained the decoder; admitting more
      // chunks now would either silently corrupt observation
      // (`receive_frame` would still report `Eof` with the new
      // work in flight) or violate the spec (depending on the
      // browser's tolerance). Caller must `flush()` to reset.
      return Err(VideoDecodeError::AtEof);
    }
    self.await_decode_room().await?;

    let key = packet.flags().contains(PacketFlags::KEY);
    let chunk_type = if key {
      web_sys::EncodedVideoChunkType::Key
    } else {
      web_sys::EncodedVideoChunkType::Delta
    };

    let bytes = packet.data().as_ref();
    // Compressed-input byte caps. Checked **before** the
    // `Uint8Array` allocation so a malformed stream can't pin
    // a 16+ MiB JS allocation just to be rejected. Both caps
    // surface as fatal `Closed` because reorder-stalled / huge
    // chunks can't be unstuck by output draining alone — the
    // user has to `flush()` the JS decoder to recover.
    if bytes.len() > MAX_INPUT_PACKET_BYTES {
      let err = Error::from_js(JsValue::from_str(
        "encoded video packet exceeds MAX_INPUT_PACKET_BYTES",
      ));
      self.state.borrow_mut().record_close(err.clone());
      self.state.invoke_close_hook();
      self.state.wake_all();
      return Err(VideoDecodeError::Closed(err));
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
          "encoded video pending_input_bytes would exceed MAX_INPUT_INFLIGHT_BYTES; \
           decoder is reorder-stalled — flush() to recover",
        ));
        self.state.borrow_mut().record_close(err.clone());
        self.state.invoke_close_hook();
        self.state.wake_all();
        return Err(VideoDecodeError::Closed(err));
      }
    }
    // Fallible `Uint8Array(len)` construction. Codex round
    // 21: the direct `new_with_length` constructor throws on
    // JS-heap OOM and aborts the wasm tab; this path lands
    // the throw as `Err(JsValue)` we can route through
    // `record_close` instead.
    let buf = match try_new_uint8_array(bytes.len() as u32) {
      Ok(b) => b,
      Err(err) => {
        let err = Error::from_js(err);
        self.state.borrow_mut().record_close(err.clone());
        self.state.invoke_close_hook();
        self.state.wake_all();
        return Err(VideoDecodeError::Closed(err));
      }
    };
    buf.copy_from(bytes);

    // Allocate the submission ID up-front so the JS-side
    // `EncodedVideoChunk.timestamp` can carry it. Insertion
    // into `pending_outputs` is deferred until AFTER the
    // chunk is constructed: a failure inside
    // `EncodedVideoChunk::new` would otherwise leave a
    // phantom record that no callback can ever clean up,
    // permanently consuming a slot in the
    // `MAX_PENDING_DECODE` budget.
    let submission_id = self.state.borrow_mut().next_submission_id();

    // `i32` constructor / `u32` setter for duration vs spec's
    // `long long` — use `_f64` setters to retain the full i64
    // range (submission IDs grow monotonically, eventually past
    // i32 in long-running sessions).
    let init = web_sys::EncodedVideoChunkInit::new(&buf.into(), 0, chunk_type);
    init.set_timestamp_f64(submission_id as f64);
    if let Some(d) = packet.duration() {
      let duration_us = d.rescale_to(MICROS).pts();
      init.set_duration_f64(duration_us as f64);
    }

    let chunk = web_sys::EncodedVideoChunk::new(&init).map_err(Error::from_js)?;

    // Chunk built successfully — now insert the side-map record
    // through the cap-enforcing helper. Overflow surfaces as a
    // `Closed` error so the caller knows the stream wasn't
    // decoded cleanly, instead of silently losing frames.
    let insert_res = {
      let mut inner = self.state.borrow_mut();
      let epoch = inner.epoch();
      inner.insert_pending_output(
        submission_id,
        // expected_samples is audio-only; pass 0 here so the
        // audio-side multi-output sample-count check stays
        // disabled for video records (video uses exact-ID
        // matching and doesn't need a sample-count gate).
        PendingOutput::new(epoch, packet.pts(), key, bytes.len() as u32, 0),
      )
    };
    if let Err(err) = insert_res {
      // `insert_pending_output` ran `Inner::record_close` on
      // overflow. Drain the JS-side decoder too so its
      // encoded queue doesn't sit until the user's recovery
      // path. `close()` is idempotent per the WebCodecs spec.
      self.state.invoke_close_hook();
      self.state.wake_all();
      return Err(VideoDecodeError::Closed(err));
    }

    if let Err(e) = self.decoder.decode(&chunk) {
      // Decoder rejected the chunk — remove the record so
      // backpressure accounting (count *and* bytes) stays
      // accurate.
      self.state.borrow_mut().remove_pending_output(submission_id);
      return Err(VideoDecodeError::Js(Error::from_js(e)));
    }
    Ok(())
  }

  async fn receive_frame(
    &mut self,
    dst: &mut VideoFrame<PixelFormat, VideoFrameExtra, Self::Buffer>,
  ) -> Result<(), Self::Error> {
    let frame = wait_for_frame(&self.state, &self.decoder, self.eof).await?;

    let dimensions = frame.dimensions();
    let format = frame.format();
    let plane_count = frame.plane_count();
    let pts = frame.pts();
    let duration = frame.duration();
    let visible_rect = frame.visible_rect();
    let color = frame.color();
    let key = frame.key();
    let [p0, p1, p2, p3] = frame.into_planes();
    let planes = [
      Plane::new(p0.0, p0.1),
      Plane::new(p1.0, p1.1),
      Plane::new(p2.0, p2.1),
      Plane::new(p3.0, p3.1),
    ];
    *dst = VideoFrame::new(
      dimensions,
      format,
      planes,
      plane_count,
      VideoFrameExtra::new(key),
    )
    .with_pts(pts)
    .with_duration(duration)
    .with_visible_rect(visible_rect)
    .with_color(color);
    Ok(())
  }

  async fn send_eof(&mut self) -> Result<(), Self::Error> {
    self.check_closed()?;

    // Cancellation / failure policy:
    //
    // - On successful await: clear `pending_outputs` (residual
    //   entries are zero-output chunks per spec) and set
    //   `self.eof = true`. Done in the post-await block.
    // - On cancellation OR Promise rejection: the Drop guard
    //   *poisons* the decoder. The JS-side `flush()` is not
    //   cancellable — the browser keeps draining its queue
    //   and firing output callbacks after we exit. Without
    //   poisoning, those late frames would be silently
    //   discarded (their `pending_outputs` records were
    //   cleared on cancel) and a retried `send_eof` would
    //   look clean while frames went missing. By bumping the
    //   epoch and recording a Closed error, the cancellation
    //   becomes observable: the user must call `flush()` to
    //   recover, which `reset()`s the JS decoder (purging the
    //   leftover output stream) and clears `last_error`.
    struct EofCancelGuard<F> {
      state: SharedState<F>,
      completed: bool,
    }
    impl<F> Drop for EofCancelGuard<F> {
      fn drop(&mut self) {
        if !self.completed {
          // Bump the epoch first so any late output callbacks
          // from the still-draining JS flush short-circuit on
          // epoch mismatch (no allocation, no spawn). This
          // also clears `pending_outputs` and `queue` /
          // `queue_bytes`. Then record a Closed error so the
          // user's next call sees the cancellation rather
          // than a deceptively clean state on retry.
          self.state.bump_epoch();
          self
            .state
            .borrow_mut()
            .record_close(Error::from_js(wasm_bindgen::JsValue::from_str(
              "send_eof was cancelled — call flush() to recover",
            )));
          // The JS-side `flush()` Promise is not cancellable;
          // without this hook the decoder keeps draining
          // chunks and firing output callbacks for frames
          // Rust has already abandoned. The hook calls
          // `decoder.reset()` (purges queued chunks +
          // pending decode work) so the cancellation actually
          // stops the JS work the user is no longer waiting
          // on. Invoked after the borrow drops.
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

    // Successful drain — clear residual zero-output chunks,
    // mark guard as completed (so Drop does NOT re-clear),
    // and finalize EOF.
    {
      let mut inner = self.state.borrow_mut();
      inner.clear_pending_outputs();
    }
    self.eof = true;
    guard.completed = true;
    Ok(())
  }

  async fn flush(&mut self) -> Result<(), Self::Error> {
    // Bump the epoch and clear the queue BEFORE the fallible
    // `reset()` / `configure()` calls. If `configure()` fails
    // and we returned early without bumping, pre-flush frames
    // would survive in `state.queue` and stale copy tasks
    // would still match the current epoch.
    //
    // `bump_epoch` deliberately does NOT clear `last_error`:
    // if the decoder was already closed by a fatal error,
    // `reset()` will throw, this method will return `Err`, and
    // the closed marker has to remain set so a subsequent
    // `receive_frame` returns `Closed(...)` instead of parking
    // forever on a permanently dead decoder.
    //
    // `FlushGuard` covers the failure path codex round 14
    // flagged: an early-return from `?` on `reset()` /
    // `configure()` used to leave the JS decoder unconfigured
    // while `check_closed()` still passed, so subsequent
    // `send_packet` / `receive_frame` would surface
    // `Js`/`NoFrameReady` instead of `Closed`. The guard
    // records `Closed` on every drop except the success path
    // that explicitly sets `completed = true`.
    self.state.bump_epoch();
    self.eof = false;
    struct FlushGuard<'a> {
      state: &'a SharedState<DecodedVideoFrame>,
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
                "video flush did not complete (reset/configure failed); \
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
    // `reset()` drops queued chunks and cancels in-flight output
    // synchronously. Per the WebCodecs spec it also returns the
    // decoder to the unconfigured state — re-apply the captured
    // config so the decoder stays reusable across seeks.
    self.decoder.reset().map_err(Error::from_js)?;
    self
      .decoder
      .configure(&self.config)
      .map_err(Error::from_js)?;
    // Both JS calls succeeded — the decoder is freshly usable.
    // Mark the guard complete BEFORE clearing last_error so any
    // panic between here and the clear leaves the decoder
    // properly poisoned.
    guard.completed = true;
    self.state.borrow_mut().clear_last_error();
    self.state.wake_dequeue();
    Ok(())
  }
}

/// Pop a frame from the queue, or yield until one becomes
/// available (or EOF is reached, or the decoder closes).
///
/// Awaitable conditions: `pending_copies > 0` (a copy task in
/// flight will push to queue) and `decode_queue_size > 0` (the
/// browser is processing chunks the user just submitted —
/// output callback will fire when ready). Both drain via JS
/// event-loop activity without consumer action.
///
/// We **never** await on `pending_outputs`: that map can hold
/// chunks the decoder is buffering for B-frame / DTS reorder,
/// and they only drain when *more input* arrives — which only
/// `send_packet` can supply. Waiting under `&mut self` would
/// deadlock since the caller can't feed that input. Returning
/// `NoFrameReady` instead lets the caller drive.
async fn wait_for_frame(
  state: &SharedState<DecodedVideoFrame>,
  decoder: &web_sys::VideoDecoder,
  eof: bool,
) -> Result<DecodedVideoFrame, VideoDecodeError> {
  loop {
    let popped = {
      let mut inner = state.borrow_mut();
      if let Some(err) = inner.last_error_clone() {
        return Err(VideoDecodeError::Closed(err));
      }
      // Read the head's `byte_size` first, then pop with the
      // same value so `Inner::queue_bytes` tracks reality.
      // Doing it in two steps keeps `pop_queue` body-agnostic.
      let head_bytes = inner
        .peek_queue_head()
        .map(|f| f.byte_size() as u64)
        .unwrap_or(0);
      let frame = inner.pop_queue(head_bytes);
      if frame.is_none() {
        // `pending_outputs.len()` deliberately does NOT
        // contribute here. A non-empty side map can mean
        // either "output callback is about to fire" or
        // "decoder is buffering for B-frame reorder until
        // more input arrives" — and the latter, when paired
        // with this method's `&mut self`, would deadlock the
        // caller (they cannot call `send_packet` to supply
        // the next input while parked here). The race window
        // between `decode_queue_size` decrementing and the
        // matching output callback firing is real but
        // microsecond-scale; the right resolution is to
        // return `NoFrameReady` and let the caller retry,
        // not to park on a state that may never advance.
        let active_decode_work = inner.pending_copies() > 0 || decoder.decode_queue_size() > 0;
        if !active_decode_work {
          if eof {
            return Err(VideoDecodeError::Eof);
          }
          return Err(VideoDecodeError::NoFrameReady);
        }
      }
      frame
    };
    if let Some(frame) = popped.map(DecodedFrame::into_frame) {
      // Popping decreases `queue.len()`, which may bring the
      // pipeline back below `MAX_QUEUED_OUTPUT`. A producer
      // that hit `OutputFull` and is now retrying will see the
      // freed slot on its next call. Also wake any decoder-side
      // backpressure waiter — they may have been parked
      // while the queue was full.
      state.wake_dequeue();
      return Ok(frame);
    }
    // Register the current task's waker and yield. The output
    // callback's spawned copy task wakes us when it pushes a
    // frame; the error callback wakes us when it sets
    // `last_error`. The guard clears `receiver_waker` if this
    // future is cancelled mid-await so a dropped task doesn't
    // leave a stale waker pinned in shared state.
    let _guard = state.receiver_waker_guard();
    core::future::poll_fn(|cx| {
      let mut inner = state.borrow_mut();
      // `pending_outputs` deliberately excluded — see the
      // matching note in the outer sync check. Parking on a
      // non-empty side map deadlocks B-frame reorder where
      // the user must call `send_packet` (blocked by
      // `&mut self`) to advance.
      let active_decode_work = inner.pending_copies() > 0 || decoder.decode_queue_size() > 0;
      // Resolve Ready when the queue has a frame, the decoder
      // has been closed, OR no further JS work can advance
      // without caller action (the outer loop translates the
      // last branch to `NoFrameReady` / `Eof`). Park otherwise.
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

/// RAII rollback for the post-admission window in the video
/// output callback: pending_copies has been incremented and the
/// JS `VideoFrame` is owned by us, but `spawn_local` (which
/// allocates an internal task `Rc`) hasn't returned yet. Codex
/// round 33 [accepted]: under panic=unwind, a `spawn_local`
/// allocation failure between admission and the future's first
/// poll would leave `pending_copies` stuck and the JS frame
/// unclosed. Constructing the admission guard *before*
/// `spawn_local` and disarming it as the future's first
/// statement closes that window: an unwinding spawn-panic drops
/// the guard, which closes the frame, decrements the counter,
/// and wakes parked waiters. wasm32-unknown-unknown defaults to
/// panic=abort (so the practical risk is "tab dies" — nothing
/// the guard can fix), but the guard documents the invariant
/// and protects the panic=unwind case if/when adopted.
struct VideoCopyAdmission {
  state: SharedState<DecodedVideoFrame>,
  frame: Option<web_sys::VideoFrame>,
  byte_estimate: u64,
  /// Reorder-buffer sequence assigned to this admission. The
  /// rollback path delivers `PendingPush::Skipped` for it so
  /// later completions can advance past the missing slot —
  /// otherwise the head cursor would wedge here permanently
  /// (codex round 34 [accepted]).
  sequence: u32,
  armed: bool,
}

impl Drop for VideoCopyAdmission {
  fn drop(&mut self) {
    if !self.armed {
      return;
    }
    if let Some(f) = self.frame.take() {
      f.close();
    }
    self.state.borrow_mut().sub_pending_copy(self.byte_estimate);
    // Deliver `Skipped` for the allocated sequence so the
    // reorder buffer's `next_push_sequence` cursor can drain
    // past it. Without this, every later completion would park
    // in `pending_pushes` behind a sequence that never arrives,
    // and `receive_frame` would observe a wedged decoder.
    // `deliver_pending_push` itself fires `wake_all` on the
    // close path it owns; we wake again afterwards as a belt-
    // and-suspenders for the parked-receiver case where the
    // delivery short-circuited via `is_closed()` (no work
    // done, no inner waker fired).
    self
      .state
      .deliver_pending_push(self.sequence, crate::state::PendingPush::Skipped);
    self.state.wake_all();
  }
}

impl VideoCopyAdmission {
  /// Suppress the rollback Drop and surrender ownership of the
  /// JS frame. Called as the first statement inside the spawned
  /// future; from that point the future is responsible for
  /// closing the frame, decrementing `pending_copies`, and
  /// delivering its own outcome (`Ready` or `Skipped`) for the
  /// reserved sequence.
  fn disarm(&mut self) -> web_sys::VideoFrame {
    self.armed = false;
    self
      .frame
      .take()
      .expect("VideoCopyAdmission::disarm called twice")
  }
}

/// Async helper: copy a JS-side `VideoFrame`'s pixels into an
/// owned byte buffer and enqueue the result for `receive_frame`.
///
/// `epoch` is captured by the output callback at the moment this
/// task is spawned. If `flush()` (or decoder `Drop`) advances the
/// epoch while we're awaiting the `copyTo` Promise, we discard
/// the result without touching `pending_copies` — `bump_epoch`
/// already cleared it for the new generation.
// `byte_estimate` is the `allocation_size_with_options` reported
// at admission, captured here so the spawned task subtracts the
// same number the output callback added — across every outcome
// (Stale, Pushed, Errored).
async fn handle_video_frame(
  mut admission: VideoCopyAdmission,
  state: SharedState<DecodedVideoFrame>,
  epoch: u64,
  record: PendingOutput,
  byte_estimate: u64,
  sequence: u32,
) {
  // First action: disarm the spawn-panic rollback — from here
  // on this task is responsible for the frame and the
  // pending_copies decrement.
  let frame = admission.disarm();
  drop(admission);
  // Always close the JS-side frame so the GPU surface is
  // released (otherwise WebCodecs throttles after a few hundred
  // outstanding frames).
  let frame_guard = JsFrameGuard(Some(frame));

  // Early-bail: if `flush()` advanced the epoch between the
  // output callback firing and this task starting, skip the
  // expensive `copyTo` entirely. Decrement `pending_copies`
  // immediately so a new-generation producer parked on the
  // output-queue cap unblocks; the `JsFrameGuard` closes the
  // JS frame on return. Wake BOTH wakers — a `receive_frame`
  // parked on `nothing_in_flight` (post-flush, waiting for the
  // last stale copy to drain) needs to be woken when this
  // decrement might satisfy the predicate.
  if state.epoch() != epoch {
    state.borrow_mut().sub_pending_copy(byte_estimate);
    // The sequence has been allocated against this generation
    // but `bump_epoch` has reset `next_push_sequence` to 0,
    // so this stale sequence will never be drained from
    // `pending_pushes` — don't insert. The receiver wakes
    // here regardless (a parked post-flush waiter watching
    // `pending_copies()` for drain progress).
    state.wake_all();
    return;
  }

  let result = copy_video_frame(frame_guard.frame(), record.user_pts(), record.key()).await;
  drop(frame_guard);

  // Mutate state under a tight borrow, then release it BEFORE
  // invoking any waker. `Waker::wake` may inline-poll the
  // awakened future, which would re-borrow `RefCell` and panic
  // if the original borrow were still alive.
  //
  // Phase 1: classify the outcome under a tight borrow,
  // build a `PendingPush` (or signal a stale-epoch skip),
  // and decrement live-copy state.
  enum Outcome<F> {
    /// Same-generation success — deliver the Ready push and
    /// wake the dequeue waker.
    Push(PendingPush<F>),
    /// Same-generation error — record_close + deliver a
    /// `Skipped` so the cursor advances past this sequence.
    Errored {
      just_closed: bool,
      push: PendingPush<F>,
    },
    /// Pre-flush stale: epoch bumped between admission and
    /// copy completion. **Do not deliver** — codex round 19:
    /// `bump_epoch` reset `next_push_sequence` to 0, so a
    /// `Skipped` at the old sequence would land in the new
    /// generation's `pending_pushes` and prematurely advance
    /// the cursor. Just decrement and wake.
    StaleEpoch,
    /// Same-generation but the decoder is closed. Same as
    /// Errored without the just_closed flag — `record_close`
    /// already cleared `pending_pushes`, so the
    /// `deliver_pending_push` is a no-op (its is_closed
    /// guard drops the push), but we still call it to keep
    /// the cursor moving for any straggler in this
    /// generation that might still arrive (it won't, since
    /// the closed path discards). Wake both so a parked
    /// receiver observes the close.
    AlreadyClosed,
  }
  let outcome: Outcome<DecodedVideoFrame> = {
    let mut inner = state.borrow_mut();
    // Always decrement — stale or not — so the live-copy
    // counter that drives `MAX_OUTPUT_QUEUE` backpressure
    // tracks reality. `bump_epoch` deliberately leaves this
    // counter alone (see `state.rs`).
    inner.sub_pending_copy(byte_estimate);
    if inner.epoch() != epoch {
      Outcome::StaleEpoch
    } else if inner.is_closed() {
      Outcome::AlreadyClosed
    } else {
      match result {
        Ok(decoded) => {
          // Defence-in-depth: fatal-close only if the actual
          // bytes (combined with already-pinned
          // queue_bytes + pending_push_bytes) blow the byte
          // cap. Pending_push_bytes is included because
          // out-of-order completions parked there are also
          // real memory.
          let bytes = decoded.byte_size() as u64;
          let projected = inner
            .queue_bytes()
            .saturating_add(inner.pending_push_bytes())
            .saturating_add(bytes);
          if projected > MAX_INFLIGHT_BYTES {
            let just_closed = inner.record_close(Error::from_js(JsValue::from_str(
              "video output queue exceeded MAX_INFLIGHT_BYTES at copy completion",
            )));
            Outcome::Errored {
              just_closed,
              push: PendingPush::Skipped,
            }
          } else {
            Outcome::Push(PendingPush::Ready(DecodedFrame::new(decoded), bytes))
          }
        }
        Err(err) => {
          let just_closed = inner.record_close(err);
          Outcome::Errored {
            just_closed,
            push: PendingPush::Skipped,
          }
        }
      }
    }
  };
  // Phase 2: deliver / wake outside the borrow.
  match outcome {
    Outcome::Push(push) => {
      state.deliver_pending_push(sequence, push);
      // `deliver_pending_push` wakes the receiver if
      // anything landed; also wake the dequeue waker so a
      // backpressured producer unblocks on the
      // pending_copies decrement.
      state.wake_dequeue();
    }
    Outcome::Errored { just_closed, push } => {
      state.deliver_pending_push(sequence, push);
      if just_closed {
        state.invoke_close_hook();
      }
      state.wake_all();
    }
    Outcome::StaleEpoch | Outcome::AlreadyClosed => {
      // Wake both — see the matching note for the legacy
      // Stale outcome: a parked `nothing_in_flight` waiter
      // observes the now-decremented `pending_copies`.
      state.wake_all();
    }
  }
}

/// RAII guard that calls `VideoFrame.close()` on drop.
/// `WebCodecs` throttles aggressively after a few hundred
/// open frames; an early-return path that forgets to close
/// would silently kill the decoder.
struct JsFrameGuard(Option<web_sys::VideoFrame>);
impl JsFrameGuard {
  fn frame(&self) -> &web_sys::VideoFrame {
    self.0.as_ref().expect("frame already taken")
  }
}
impl Drop for JsFrameGuard {
  fn drop(&mut self) {
    if let Some(frame) = self.0.take() {
      frame.close();
    }
  }
}

/// Construct `new Uint8Array(size)` through
/// `js_sys::Reflect::construct` so a JS-side allocation
/// failure (RangeError / out-of-memory) is captured as
/// `Result::Err(JsValue)` instead of unwinding through the
/// wasm boundary. Direct `Uint8Array::new_with_length` would
/// throw on OOM and abort the tab. Codex round 20.
///
/// `pub(crate)` so the audio adapter can use the same
/// fallible path for its `EncodedAudioChunk` and
/// codec-description copies (codex round 21).
pub(crate) fn try_new_uint8_array(size: u32) -> Result<js_sys::Uint8Array, JsValue> {
  let global = js_sys::global();
  let ctor_value = js_sys::Reflect::get(&global, &JsValue::from_str("Uint8Array"))?;
  let ctor: js_sys::Function = ctor_value
    .dyn_into()
    .map_err(|_| JsValue::from_str("globalThis.Uint8Array is not a function"))?;
  let args = js_sys::Array::new();
  args.push(&JsValue::from_f64(size as f64));
  let constructed = js_sys::Reflect::construct(&ctor, &args)?;
  constructed
    .dyn_into::<js_sys::Uint8Array>()
    .map_err(|_| JsValue::from_str("constructed value is not a Uint8Array"))
}

async fn copy_video_frame(
  frame: &web_sys::VideoFrame,
  user_pts: Option<Timestamp>,
  key: bool,
) -> Result<DecodedVideoFrame, Error> {
  let dimensions = Dimensions::new(frame.coded_width(), frame.coded_height());
  let width = dimensions.width();
  let height = dimensions.height();
  let format = match frame.format() {
    Some(f) => map_pixel_format(f)?,
    None => {
      return Err(Error::from_js(JsValue::from_str(
        "VideoFrame.format is null",
      )));
    }
  };
  // `frame.timestamp()` is the submission ID we stamped onto
  // the chunk, NOT a wallclock value. The user's real PTS came
  // through the `pending_outputs` side map.
  let pts = user_pts;
  let duration = frame.duration().map(|d| Timestamp::new(d as i64, MICROS));
  let visible_rect = read_visible_rect(frame, dimensions);
  let color = read_color_info(frame);

  // Force `copyTo` to use the FULL coded rectangle, not the
  // default `visibleRect`. We report `coded_width` /
  // `coded_height` as the frame's dimensions, so the copied
  // byte count must match those dimensions; otherwise a
  // consumer walking by `width × stride` reads past the actual
  // copied bytes. `visible_rect` is propagated separately so
  // consumers that want the visible region can crop.
  let copy_rect = web_sys::DomRectInit::new();
  copy_rect.set_x(0.0);
  copy_rect.set_y(0.0);
  copy_rect.set_width(width as f64);
  copy_rect.set_height(height as f64);
  let copy_opts = web_sys::VideoFrameCopyToOptions::new();
  copy_opts.set_rect(&copy_rect);

  let size_u32 = frame
    .allocation_size_with_options(&copy_opts)
    .map_err(Error::from_js)?;
  if size_u32 > MAX_FRAME_ALLOCATION_BYTES {
    return Err(Error::from_js(JsValue::from_str(&format!(
      "VideoFrame.allocationSize = {size_u32} exceeds MAX_FRAME_ALLOCATION_BYTES = {MAX_FRAME_ALLOCATION_BYTES}"
    ))));
  }
  let size = size_u32 as usize;
  // The destination MUST live outside Wasm linear memory across
  // the `copyTo` await — `wasm-bindgen`'s `&mut [u8]` overload
  // hands JS a typed-array view backed by Wasm memory, and any
  // allocation that triggers `memory.grow` while the Promise is
  // pending detaches that view, so the resolved bytes can be
  // zero or partial. Allocate a JS-side `Uint8Array` (lives in
  // the JS heap, immune to Wasm growth) and copy back into a
  // Rust `Vec<u8>` after the Promise settles.
  // Both allocations need to be fallible. Codex round 20
  // flagged that even a cap-compliant frame near
  // `MAX_FRAME_ALLOCATION_BYTES` can require hundreds of MiB
  // across the JS heap and wasm linear memory, and either
  // could fail under memory pressure. The previous direct
  // calls (`Uint8Array::new_with_length` + `vec![0u8; size]`)
  // would either trap (wasm-bindgen rethrows JS allocation
  // errors as panics through wasm) or panic-on-OOM (Rust
  // Vec). Both abort the wasm tab. Convert allocation
  // failures into a `VideoDecodeError::Js` so the caller can
  // fail-close the decoder instead.
  //
  // - JS side: build the Uint8Array via `Reflect::construct`
  //   so a JS-side throw lands as `Result::Err` here rather
  //   than unwinding into wasm.
  // - Rust side: `Vec::try_reserve_exact` returns `Err` on
  //   OOM; `vec![0u8; size]` would have panicked.
  // The first allocation (Uint8Array) can still allocate
  // for its error message via `Error::from_js` because it
  // succeeded before reaching the JS-throw path; the throw
  // means the JS heap rejected the request, but a small
  // Rust String is being constructed at that point on a
  // path where Rust-side allocation hasn't been attempted
  // yet. The Vec allocation is the riskier one — when
  // `try_reserve_exact` fails, the global allocator just
  // refused; allocating a fresh formatted error message
  // there could re-fail. Codex round 22: use a static
  // error via `Error::from_static` for the Rust-OOM path.
  let dst = try_new_uint8_array(size as u32).map_err(Error::from_js)?;
  let promise = frame.copy_to_with_u8_array_and_options(&dst, &copy_opts);
  let layouts_js = JsFuture::from(promise).await.map_err(Error::from_js)?;
  let mut bytes: Vec<u8> = Vec::new();
  bytes
    .try_reserve_exact(size)
    .map_err(|_| Error::from_static("Rust allocation for VideoFrame copy failed"))?;
  bytes.resize(size, 0);
  dst.copy_to(&mut bytes);
  // Release the JS-heap `Uint8Array` so JS-heap and Rust-
  // Vec memory don't both stay live longer than necessary.
  // The Vec→Arc step below is now `Arc::new(bytes)` (codex
  // round 21), which moves the Vec by value into a small
  // refcount-header allocation — no second size-byte
  // allocation, so peak per frame is 2× (JS Uint8Array +
  // Rust Vec) matching the aggregate-cap reservation.
  drop(dst);

  // The promise resolves to a `sequence<PlaneLayout>` — one entry
  // per plane in pixel-format order, each carrying { offset,
  // stride }. Walk it to size each plane's view.
  let layouts: js_sys::Array = layouts_js.dyn_into().map_err(|_| {
    Error::from_js(JsValue::from_str(
      "VideoFrame.copyTo did not resolve to PlaneLayout array",
    ))
  })?;

  let raw_layout_count = layouts.length() as usize;
  // Codex round 16 flagged that the previous `min(4)` truncation
  // accepted unexpected layout-array sizes silently. Compare
  // against the expected per-format plane count instead, and
  // reject mismatches before constructing planes.
  let expected = expected_plane_layout(format, width, height);
  if let Some(layout) = expected
    && raw_layout_count != layout.count
  {
    return Err(Error::from_js(JsValue::from_str(&format!(
      "VideoFrame.copyTo PlaneLayout count = {raw_layout_count} does not match the \
       expected {} planes for format {format:?}",
      layout.count,
    ))));
  }
  let plane_count = if let Some(layout) = expected {
    layout.count
  } else {
    // Format whose plane layout we don't model — fall back to
    // the raw array size capped at 4. The downstream slice/
    // stride validation still runs against `plane_len`, so we
    // never publish a buffer view that overruns the backing
    // allocation; we just don't enforce the per-format
    // (rows × stride) extent bound.
    raw_layout_count.min(4)
  };
  if plane_count == 0 {
    return Err(Error::from_js(JsValue::from_str(
      "VideoFrame.copyTo returned empty PlaneLayout array",
    )));
  }

  let mut layout_pairs: [(u32, u32); 4] = [(0, 0); 4];
  for (i, slot) in layout_pairs.iter_mut().enumerate().take(plane_count) {
    let layout = layouts
      .get(i as u32)
      .dyn_into::<web_sys::PlaneLayout>()
      .map_err(|_| {
        Error::from_js(JsValue::from_str(
          "VideoFrame.copyTo PlaneLayout entry has wrong type",
        ))
      })?;
    *slot = (layout.get_offset(), layout.get_stride());
  }

  // Validate offsets before constructing buffers. Without this, a
  // browser / API-version-skew that returns non-monotonic or
  // out-of-bounds offsets would either silently corrupt frames
  // (offset_next < offset_current → zero-length plane) or fire a
  // debug-assert / out-of-bounds slice in `WebCodecsBuffer`'s
  // `AsRef<[u8]>` later. Surface the violation as an explicit
  // decode error instead.
  let total_size = size;
  for i in 0..plane_count {
    let offset = layout_pairs[i].0 as usize;
    let next_offset = if i + 1 < plane_count {
      layout_pairs[i + 1].0 as usize
    } else {
      total_size
    };
    if offset > total_size {
      return Err(Error::from_js(JsValue::from_str(&format!(
        "PlaneLayout[{i}].offset = {offset} exceeds allocation_size = {total_size}"
      ))));
    }
    if next_offset > total_size {
      return Err(Error::from_js(JsValue::from_str(&format!(
        "PlaneLayout[{i}] runs past allocation_size = {total_size}"
      ))));
    }
    if next_offset < offset {
      return Err(Error::from_js(JsValue::from_str(&format!(
        "PlaneLayout offsets are non-monotonic at plane {i}: {offset} → {next_offset}"
      ))));
    }
    // Codex round 16: validate stride against expected
    // per-format `(rows, row_bytes)` so a bogus
    // browser-supplied stride can't make downstream
    // consumers slice past the buffer. The check fires
    // only when we know the expected layout for `format`
    // (see `expected_plane_layout`); unknown formats fall
    // through with offset/extent validation only.
    if let Some(layout) = expected {
      let PlaneDim { rows, row_bytes } = layout.planes[i];
      let stride = layout_pairs[i].1;
      let plane_len_u32 = u32::try_from(next_offset - offset).unwrap_or(u32::MAX);
      if stride < row_bytes {
        return Err(Error::from_js(JsValue::from_str(&format!(
          "PlaneLayout[{i}] stride = {stride} is below the expected row \
           bytes = {row_bytes} for format {format:?}"
        ))));
      }
      // Worst-case extent: row (rows-1) starts at offset
      // (rows-1) * stride and reads row_bytes bytes. Must fit
      // entirely within the plane slice.
      if let Some(rows_minus_one) = rows.checked_sub(1) {
        let last_row_offset = rows_minus_one
          .checked_mul(stride)
          .and_then(|n| n.checked_add(row_bytes));
        match last_row_offset {
          Some(end) if end <= plane_len_u32 => {}
          _ => {
            return Err(Error::from_js(JsValue::from_str(&format!(
              "PlaneLayout[{i}] stride = {stride} × rows = {rows} \
               + row_bytes = {row_bytes} overruns plane_len = {plane_len_u32} \
               for format {format:?}"
            ))));
          }
        }
      }
    }
  }

  // Move the Vec into an `Arc<Vec<u8>>` — see
  // `WebCodecsBuffer`'s type-doc for the reason. `Arc::new`
  // allocates only a small refcount header here; the data
  // bytes (already `try_reserve_exact`-allocated above) are
  // moved into the Arc by value, no memcpy.
  let arc: Arc<Vec<u8>> = Arc::new(bytes);

  let mut planes: [(WebCodecsBuffer, u32); 4] = [
    (WebCodecsBuffer::empty(), 0),
    (WebCodecsBuffer::empty(), 0),
    (WebCodecsBuffer::empty(), 0),
    (WebCodecsBuffer::empty(), 0),
  ];
  for i in 0..plane_count {
    let (offset, stride) = layout_pairs[i];
    let next_offset = if i + 1 < plane_count {
      layout_pairs[i + 1].0 as usize
    } else {
      total_size
    };
    let plane_len = next_offset - offset as usize;
    planes[i] = (
      WebCodecsBuffer::from_arc_range(Arc::clone(&arc), offset as usize, plane_len),
      stride,
    );
  }

  Ok(DecodedVideoFrame::new(
    pts,
    duration,
    dimensions,
    visible_rect,
    format,
    planes,
    plane_count as u8,
    key,
    total_size as u32,
    color,
  ))
}

/// Translate `VideoFrame.colorSpace` into the workspace's
/// [`ColorInfo`]. Each component of `VideoColorSpace` is
/// nullable per the WebCodecs spec; absent entries fall through
/// as the corresponding `Unspecified` variant so
/// downstream code can still distinguish "decoder reported X"
/// from "we have no idea".
fn read_color_info(frame: &web_sys::VideoFrame) -> ColorInfo {
  let cs = frame.color_space();
  let primaries = match cs.primaries() {
    Some(web_sys::VideoColorPrimaries::Bt709) => ColorPrimaries::Bt709,
    Some(web_sys::VideoColorPrimaries::Bt470bg) => ColorPrimaries::Bt470Bg,
    Some(web_sys::VideoColorPrimaries::Smpte170m) => ColorPrimaries::Smpte170M,
    Some(web_sys::VideoColorPrimaries::Bt2020) => ColorPrimaries::Bt2020,
    Some(web_sys::VideoColorPrimaries::Smpte432) => ColorPrimaries::SmpteEg432,
    _ => ColorPrimaries::Unspecified,
  };
  let transfer = match cs.transfer() {
    Some(web_sys::VideoTransferCharacteristics::Bt709) => ColorTransfer::Bt709,
    Some(web_sys::VideoTransferCharacteristics::Smpte170m) => ColorTransfer::Smpte170M,
    Some(web_sys::VideoTransferCharacteristics::Iec6196621) => ColorTransfer::Iec6196621,
    Some(web_sys::VideoTransferCharacteristics::Linear) => ColorTransfer::Linear,
    Some(web_sys::VideoTransferCharacteristics::Pq) => ColorTransfer::SmpteSt2084Pq,
    Some(web_sys::VideoTransferCharacteristics::Hlg) => ColorTransfer::AribStdB67Hlg,
    _ => ColorTransfer::Unspecified,
  };
  // WebCodecs has no "unspecified" matrix variant — when the
  // matrix is unknown the getter returns `None`, which we map
  // back to the workspace's BT.709 default (the type's
  // `#[default]`). `Rgb` is preserved as YCgCo's nearest
  // surface-level cousin only as a last resort; in practice
  // `Rgb` indicates raw RGB output where the matrix doesn't
  // apply.
  let matrix = match cs.matrix() {
    Some(web_sys::VideoMatrixCoefficients::Bt709) => ColorMatrix::Bt709,
    Some(web_sys::VideoMatrixCoefficients::Bt470bg) => ColorMatrix::Bt601,
    Some(web_sys::VideoMatrixCoefficients::Smpte170m) => ColorMatrix::Bt601,
    Some(web_sys::VideoMatrixCoefficients::Bt2020Ncl) => ColorMatrix::Bt2020Ncl,
    _ => ColorMatrix::default(),
  };
  let range = match cs.full_range() {
    Some(true) => ColorRange::Full,
    Some(false) => ColorRange::Limited,
    None => ColorRange::Unspecified,
  };
  ColorInfo::UNSPECIFIED
    .with_primaries(primaries)
    .with_transfer(transfer)
    .with_matrix(matrix)
    .with_range(range)
}

/// Read `VideoFrame.visibleRect` and convert it to a
/// `mediadecode::Rect`. Returns `None` if WebCodecs reports no
/// visible rect or the values are out of range / non-finite.
fn read_visible_rect(frame: &web_sys::VideoFrame, coded: Dimensions) -> Option<Rect> {
  let dom = frame.visible_rect()?;
  let x = dom.x();
  let y = dom.y();
  let w = dom.width();
  let h = dom.height();
  // WebCodecs guarantees integer pixel coordinates, but the JS
  // type is `f64`. Reject NaN / negative / overflow rather than
  // truncating into garbage.
  if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite()) {
    return None;
  }
  if x < 0.0 || y < 0.0 || w < 0.0 || h < 0.0 {
    return None;
  }
  if x > u32::MAX as f64 || y > u32::MAX as f64 || w > u32::MAX as f64 || h > u32::MAX as f64 {
    return None;
  }
  let x = x as u32;
  let y = y as u32;
  let w = w as u32;
  let h = h as u32;
  // Codex round 24 [accepted]: the rect must fit inside the
  // coded frame. The browser is the only producer today and
  // emits valid rects, but downstream crop/conversion code
  // trusts this metadata; an out-of-bounds visibleRect from
  // a malformed bitstream or a future browser bug could drive
  // out-of-bounds reads. Use checked addition to also reject
  // pathological `x + w > u32::MAX` cases.
  let x_end = x.checked_add(w)?;
  let y_end = y.checked_add(h)?;
  if x_end > coded.width() || y_end > coded.height() {
    return None;
  }
  Some(Rect::new(x, y, w, h))
}

/// Per-plane row dimensions returned by
/// [`expected_plane_layout`]. `rows × row_bytes` is the
/// expected payload size of the plane; the validator
/// rejects any layout whose stride extent
/// (`(rows - 1) × stride + row_bytes`) overruns the actual
/// `plane_len`.
#[derive(Clone, Copy, Default)]
struct PlaneDim {
  rows: u32,
  row_bytes: u32,
}

const fn dim(rows: u32, row_bytes: u32) -> PlaneDim {
  PlaneDim { rows, row_bytes }
}

/// Stack-allocated per-format plane layout. `planes[..count]`
/// are the active plane descriptors; trailing entries are
/// `PlaneDim::default()` and unused.
#[derive(Clone, Copy)]
struct PlaneLayout {
  planes: [PlaneDim; 4],
  count: usize,
}

/// Expected per-plane row dimensions for the given pixel
/// format and coded dimensions. Returns `None` for formats
/// `copy_video_frame` doesn't have a layout model for (the
/// per-plane stride validation is then skipped — offset/
/// extent validation against `plane_len` still runs, so the
/// buffer view never overruns the allocation; we just
/// don't catch a "stride says rows fit but they don't"
/// bogosity for that format).
///
/// Codex round 23 flagged that the prior `Option<Vec<(u32,
/// u32)>>` return shape did a heap allocation on the same
/// path that's expected to survive memory pressure (this
/// runs after the big frame-copy `Vec` allocation). The
/// `PlaneLayout` struct returned here is fully stack-
/// allocated. Codex follow-up: the previous
/// `([(u32, u32); 4], usize)` tuple shape was opaque at
/// call sites; named struct improves readability without
/// changing behaviour.
///
/// Width/height arithmetic uses `div_ceil(2)` for sub-
/// sampled chroma planes so odd dimensions are handled
/// (the browser pads to even bounds). `saturating_mul` on
/// the bytes-per-sample multiplier upper-bounds at
/// `u32::MAX` rather than overflowing — the validation
/// caller compares against `plane_len_u32` so a saturated
/// row_bytes naturally fails closed.
fn expected_plane_layout(format: PixelFormat, width: u32, height: u32) -> Option<PlaneLayout> {
  use PixelFormat as P;
  let halfw = width.div_ceil(2);
  let halfh = height.div_ceil(2);
  let w2 = width.saturating_mul(2);
  let halfw2 = halfw.saturating_mul(2);
  let w4 = width.saturating_mul(4);
  let none = PlaneDim::default();
  let (planes, count): ([PlaneDim; 4], usize) = match format {
    // 4:2:0 planar, 8-bit (Y, U, V)
    P::Yuv420p => (
      [
        dim(height, width),
        dim(halfh, halfw),
        dim(halfh, halfw),
        none,
      ],
      3,
    ),
    P::Yuv420p10Le | P::Yuv420p12Le => (
      [
        dim(height, w2),
        dim(halfh, halfw2),
        dim(halfh, halfw2),
        none,
      ],
      3,
    ),
    // 4:2:0 planar with alpha (Y, U, V, A)
    P::Yuva420p => (
      [
        dim(height, width),
        dim(halfh, halfw),
        dim(halfh, halfw),
        dim(height, width),
      ],
      4,
    ),
    P::Yuva420p10Le | P::Yuva420p12Le => (
      [
        dim(height, w2),
        dim(halfh, halfw2),
        dim(halfh, halfw2),
        dim(height, w2),
      ],
      4,
    ),
    // 4:2:2 planar (Y, U, V; UV = full height × half width)
    P::Yuv422p => (
      [
        dim(height, width),
        dim(height, halfw),
        dim(height, halfw),
        none,
      ],
      3,
    ),
    P::Yuv422p10Le | P::Yuv422p12Le => (
      [
        dim(height, w2),
        dim(height, halfw2),
        dim(height, halfw2),
        none,
      ],
      3,
    ),
    P::Yuva422p => (
      [
        dim(height, width),
        dim(height, halfw),
        dim(height, halfw),
        dim(height, width),
      ],
      4,
    ),
    P::Yuva422p10Le | P::Yuva422p12Le => (
      [
        dim(height, w2),
        dim(height, halfw2),
        dim(height, halfw2),
        dim(height, w2),
      ],
      4,
    ),
    // 4:4:4 planar (all planes full size)
    P::Yuv444p => ([dim(height, width); 4], 3),
    P::Yuv444p10Le | P::Yuv444p12Le => ([dim(height, w2); 4], 3),
    P::Yuva444p => ([dim(height, width); 4], 4),
    P::Yuva444p10Le | P::Yuva444p12Le => ([dim(height, w2); 4], 4),
    // NV12 (Y full + interleaved UV at half height × full width bytes)
    P::Nv12 => ([dim(height, width), dim(halfh, width), none, none], 2),
    // Packed 4-byte-per-pixel formats (RGBA / RGBX / BGRA / BGRX)
    P::Rgba | P::Rgbx | P::Bgra | P::Bgrx => ([dim(height, w4), none, none, none], 1),
    _ => return None,
  };
  Some(PlaneLayout { planes, count })
}

/// Map WebCodecs `VideoPixelFormat` → `mediadecode::PixelFormat`.
///
/// High-bit-depth (10 / 12 bpc) and alpha-bearing formats map
/// to the workspace's `*Le` variants — WebCodecs streams pixel
/// bytes in the platform's native order, and `wasm32` is
/// always little-endian. Variants WebCodecs doesn't yet
/// stabilise (e.g. the experimental P016 / RGBA16 / 9-bit
/// alpha forms) still surface as
/// [`Error::from_js`] so a future browser introducing them
/// fails loudly rather than silently misinterpreting bytes.
fn map_pixel_format(fmt: web_sys::VideoPixelFormat) -> Result<PixelFormat, Error> {
  use web_sys::VideoPixelFormat as W;
  Ok(match fmt {
    W::I420 => PixelFormat::Yuv420p,
    W::I420p10 => PixelFormat::Yuv420p10Le,
    W::I420p12 => PixelFormat::Yuv420p12Le,
    W::I420a => PixelFormat::Yuva420p,
    W::I420ap10 => PixelFormat::Yuva420p10Le,
    W::I420ap12 => PixelFormat::Yuva420p12Le,
    W::I422 => PixelFormat::Yuv422p,
    W::I422p10 => PixelFormat::Yuv422p10Le,
    W::I422p12 => PixelFormat::Yuv422p12Le,
    W::I422a => PixelFormat::Yuva422p,
    W::I422ap10 => PixelFormat::Yuva422p10Le,
    W::I422ap12 => PixelFormat::Yuva422p12Le,
    W::I444 => PixelFormat::Yuv444p,
    W::I444p10 => PixelFormat::Yuv444p10Le,
    W::I444p12 => PixelFormat::Yuv444p12Le,
    W::I444a => PixelFormat::Yuva444p,
    W::I444ap10 => PixelFormat::Yuva444p10Le,
    W::I444ap12 => PixelFormat::Yuva444p12Le,
    W::Nv12 => PixelFormat::Nv12,
    W::Rgba => PixelFormat::Rgba,
    W::Rgbx => PixelFormat::Rgbx,
    W::Bgra => PixelFormat::Bgra,
    W::Bgrx => PixelFormat::Bgrx,
    other => {
      return Err(Error::from_js(JsValue::from_str(&format!(
        "unsupported VideoPixelFormat: {other:?}"
      ))));
    }
  })
}

//! Shared decoder state between the synchronous Rust methods and
//! the JS callback closures.
//!
//! Both `WebCodecsVideoStreamDecoder` and `WebCodecsAudioStreamDecoder`
//! parameterize this module by the kind of frame they produce.
//! Wrapping it in `Rc<RefCell<…>>` is fine — WebCodecs callbacks
//! always run on the same thread (the renderer / worker that
//! created the decoder), so there's no cross-thread aliasing to
//! worry about.
//!
//! # Drain / reset state machine
//!
//! Now that the WebCodecs adapter implements only the async
//! [`mediadecode::future::local`] traits, `send_eof` itself
//! awaits the `decoder.flush()` Promise — there's no sync EOF
//! gate to race against, so the previous `flush_pending` flag is
//! gone. What remains:
//!
//! - `epoch` is a monotonically increasing counter incremented on
//!   every `flush()` (and at decoder drop). Each spawned copy task
//!   captures the epoch at start and discards its result if the
//!   epoch advanced while it was awaiting `copyTo`. That blocks
//!   pre-flush frames from leaking into the post-flush queue.
//! - `pending_copies` counts copy tasks that have started but not
//!   yet pushed their frame onto the queue. The async
//!   `receive_frame` resolves with `Eof` only when the queue is
//!   empty, `pending_copies == 0`, and `send_eof` has completed.
//!
//! Two wakers track the two distinct waiters:
//!
//! - `receiver_waker` is registered by `receive_frame` while it
//!   waits for a frame to arrive (or `last_error` to be set, or
//!   EOF to be reached). The output / error callbacks wake it.
//! - `dequeue_waker` is registered by `send_packet` while it
//!   awaits backpressure relief. The WebCodecs `dequeue` event
//!   wakes it when a chunk leaves the decoder's internal queue.
//!
//! # Wake ordering
//!
//! `Waker::wake()` is allowed to schedule the woken task to run
//! immediately (some executors poll inline), which can re-enter
//! this state via the awakened future. Calling `wake()` while a
//! `RefCell` borrow is still held would cause a re-borrow panic.
//! All call sites must therefore extract the waker, **drop the
//! borrow**, then invoke `wake()`. The [`SharedState::wake_all`]
//! and [`SharedState::wake_dequeue`] helpers encapsulate that
//! pattern; prefer them over poking the waker fields directly.

use std::{cell::RefCell, collections::VecDeque, rc::Rc, task::Waker};

use mediadecode::Timestamp;
use wasm_bindgen::JsValue;

use crate::error::Error;

/// Owned frame entry queued for `receive_frame`. Wraps a
/// backend-specific frame body (`DecodedVideoFrame` /
/// `DecodedAudioFrame`) so the [`Inner::queue`] type is generic
/// in the body. Field is private; access via [`Self::frame`] /
/// [`Self::into_frame`].
pub(crate) struct DecodedFrame<F> {
  frame: F,
}

impl<F> DecodedFrame<F> {
  /// Wrap an already-CPU-side frame body for queueing.
  pub const fn new(frame: F) -> Self {
    Self { frame }
  }

  /// Borrow the wrapped frame body.
  pub const fn frame(&self) -> &F {
    &self.frame
  }

  /// Consume the wrapper and return the wrapped body.
  pub fn into_frame(self) -> F {
    self.frame
  }
}

/// Per-submission record kept in the [`Inner::pending_outputs`]
/// map until the output callback for the chunk fires (or
/// `bump_epoch` evicts it).
#[derive(Clone, Copy)]
pub(crate) struct PendingOutput {
  epoch: u64,
  user_pts: Option<Timestamp>,
  key: bool,
  input_bytes: u32,
  expected_samples: u32,
}

impl PendingOutput {
  /// Build a side-map record. `epoch` is the decoder
  /// generation at the time `send_packet` admitted the chunk
  /// (closes the late-pre-flush-callback hole — see the
  /// round-12 history). `user_pts` is the user's original PTS
  /// (the JS-side timestamp is the submission ID, so the
  /// real PTS only survives via this field). `key` mirrors
  /// the originating packet's `PacketFlags::KEY` for
  /// downstream `*FrameExtra::key`. `input_bytes` is the
  /// compressed packet size captured for the aggregate
  /// pending-input byte counter (so it can be decremented
  /// by the exact amount on entry removal).
  /// `expected_samples` is non-zero only for PCM-family
  /// audio packets (where the sample count is derivable from
  /// the byte length and codec format) and lets the audio
  /// output callback fail-closed if Chrome emits more
  /// `AudioData` outputs than the chunk's encoded sample
  /// count would justify (codex round 6: WebCodecs spec
  /// allows multi-output-per-chunk, FIFO matching would
  /// otherwise consume future packets' records). Zero means
  /// "unknown / no per-output check".
  pub const fn new(
    epoch: u64,
    user_pts: Option<Timestamp>,
    key: bool,
    input_bytes: u32,
    expected_samples: u32,
  ) -> Self {
    Self {
      epoch,
      user_pts,
      key,
      input_bytes,
      expected_samples,
    }
  }

  /// Sample count we expect this chunk to decode into, when
  /// known. Zero if unknown — see field/`new` doc.
  pub const fn expected_samples(&self) -> u32 {
    self.expected_samples
  }

  /// Decoder generation at admission.
  pub const fn epoch(&self) -> u64 {
    self.epoch
  }

  /// User's PTS in `MICROS` scale, if known.
  pub const fn user_pts(&self) -> Option<Timestamp> {
    self.user_pts
  }

  /// Whether the originating packet was a key chunk.
  pub const fn key(&self) -> bool {
    self.key
  }
}

/// Shared mutable state accessed by both the JS callbacks and the
/// owning decoder. Constructed with [`SharedState::new`] and cloned
/// (`Rc::clone`) into each closure.
///
/// All fields are private; mutation goes through purpose-named
/// methods (`push_queue`, `pop_queue`, `add_pending_copy`, …) so
/// invariants like "`queue_bytes` tracks `queue` byte total" and
/// "`pending_copy_bytes` is incremented and decremented by the
/// same amount per copy task" can't drift through accidental
/// independent edits.
pub(crate) struct Inner<F> {
  queue: VecDeque<DecodedFrame<F>>,
  queue_bytes: u64,
  last_error: Option<Error>,
  pending_outputs: PendingOutputMap,
  next_submission_id: i64,
  /// `next_submission_id` snapshot taken at the most recent
  /// `bump_epoch` (or 0 at construction). IDs strictly below
  /// this floor were issued in some prior generation; an
  /// output callback firing for one of those IDs should be
  /// silently dropped (epoch advanced past it). IDs at or
  /// above the floor are current-generation: an output
  /// callback that fails to find them in `pending_outputs`
  /// indicates a missing record (already consumed by a peer
  /// duplicate callback, or never inserted at all) — fail
  /// closed rather than silently discard.
  epoch_id_floor: i64,
  pending_input_bytes: u64,
  pending_copies: u32,
  pending_copy_bytes: u64,
  /// Bytes the most recent output callback reserved for its
  /// copy task (`allocation_size_with_options × 2` for the
  /// JS+Rust peak). Updated each time the output callback
  /// admits a copy. Used by the producer-side admission
  /// budget as a measured headroom estimate — before any
  /// frame has been seen this is `0` (admission lets the
  /// first chunk through unconditionally), after which it
  /// reflects the actual decoded peak rather than an
  /// open-time guess.
  last_measured_frame_bytes: u64,
  epoch: u64,
  /// Monotonic sequence assigned by the output callback as
  /// frames are admitted, in `output()` order. `handle_*_frame`
  /// passes the assigned sequence through to its async copy
  /// task; the task's completion uses the sequence as the
  /// key in `pending_pushes` so the queue can drain in the
  /// original output order even when async `copyTo` Promises
  /// resolve out of order. Codex round 18: without this, a
  /// later frame whose copy completes first would push to
  /// the queue ahead of an earlier frame, surfacing PTS
  /// inversion to consumers.
  next_output_sequence: u32,
  /// Next sequence the queue is ready to accept. Pending
  /// pushes drain into the queue while their key matches.
  next_push_sequence: u32,
  /// Frames whose async copy has completed but whose output-
  /// order sequence is not yet at the head. Drained into
  /// `queue` in sequence order whenever a delivery lands the
  /// next-expected key.
  pending_pushes: PendingPushMap<F>,
  /// Bytes pinned across `pending_pushes`'s `Ready` entries.
  /// Tracked alongside `queue_bytes` and `pending_copy_bytes`
  /// in admission budget checks so an out-of-order copy
  /// completion can't bypass `MAX_INFLIGHT_BYTES` while
  /// waiting for an earlier sequence to drain. Codex round
  /// 19: the previous design moved bytes from
  /// `pending_copy_bytes` to "limbo" while parked here,
  /// invisible to caps until they finally landed in
  /// `queue_bytes`.
  pending_push_bytes: u64,
  receiver_waker: Option<Waker>,
  dequeue_waker: Option<Waker>,
}

/// Bounded sorted-`Vec` side map for [`PendingOutput`] records.
/// Preallocated to [`MAX_PENDING_OUTPUTS`] at construction so all
/// subsequent admission-path insert/remove operations are
/// allocation-free.
///
/// Codex round 24 [accepted]: the previous `BTreeMap<i64, _>`
/// allocated a fresh node on every `insert`, on a `send_packet`
/// path that had already copied the chunk's encoded bytes into
/// a JS `Uint8Array`. A node-allocation OOM there would have
/// panicked into wasm `unreachable!` rather than routing through
/// `record_close`, leaving the JS allocation alive on a tab
/// that's about to die. With a sorted-Vec backing whose capacity
/// is fixed at construction, `insert` is provably alloc-free as
/// long as the caller's pre-existing `len() >= MAX_PENDING_OUTPUTS`
/// fail-closed check has run first (it does — see
/// `Inner::insert_pending_output`). The one-time
/// `Vec::with_capacity` call happens at decoder construction
/// (cold path, before any JS state is created) and trades the
/// previous "per-`send_packet` BTreeMap node alloc" failure mode
/// for an "init-time failure" mode that surfaces as the user
/// never receiving a working decoder, the same outcome
/// `BTreeMap::new` already had.
///
/// `pop_first` uses `Vec::remove(0)` which is `O(len)`, but
/// `len ≤ 256` so the absolute cost is trivial vs. the upside
/// of a single contiguous allocation.
pub(crate) struct PendingOutputMap {
  entries: Vec<(i64, PendingOutput)>,
}

impl PendingOutputMap {
  /// Construct an empty map preallocated to `cap`, fallibly.
  /// Codex round 29 [accepted]: the init-time alloc must
  /// surface OOM as a `Result` rather than panic, so
  /// `WebCodecsVideoStreamDecoder::open` /
  /// `WebCodecsAudioStreamDecoder::open` can return
  /// `*DecodeError::Js` instead of aborting the wasm tab on
  /// open-time memory pressure.
  pub fn try_with_capacity(cap: usize) -> Result<Self, Error> {
    let mut entries = Vec::new();
    entries
      .try_reserve_exact(cap)
      .map_err(|_| Error::from_static("out of memory: pending_outputs"))?;
    Ok(Self { entries })
  }

  /// Number of records currently held.
  pub fn len(&self) -> usize {
    self.entries.len()
  }

  /// Insert (or overwrite) the record for `id`. Caller must
  /// ensure `self.len() < self.capacity()` before calling — the
  /// `Inner::insert_pending_output` cap check is the single
  /// gating site.
  pub fn insert(&mut self, id: i64, record: PendingOutput) {
    debug_assert!(
      self.entries.len() < self.entries.capacity()
        || self.entries.binary_search_by_key(&id, |(k, _)| *k).is_ok(),
      "PendingOutputMap::insert at capacity would reallocate"
    );
    match self.entries.binary_search_by_key(&id, |(k, _)| *k) {
      Ok(idx) => self.entries[idx].1 = record,
      Err(idx) => self.entries.insert(idx, (id, record)),
    }
  }

  /// Remove and return the record for `id`, or `None` if absent.
  pub fn remove(&mut self, id: i64) -> Option<PendingOutput> {
    let idx = self.entries.binary_search_by_key(&id, |(k, _)| *k).ok()?;
    Some(self.entries.remove(idx).1)
  }

  /// Remove and return the smallest-`id` record, or `None` if
  /// the map is empty. Mirrors `BTreeMap::pop_first`.
  pub fn pop_first(&mut self) -> Option<(i64, PendingOutput)> {
    if self.entries.is_empty() {
      None
    } else {
      Some(self.entries.remove(0))
    }
  }

  /// Drop every record. Capacity is preserved.
  pub fn clear(&mut self) {
    self.entries.clear();
  }
}

/// Bounded sorted-`Vec` reorder buffer for [`PendingPush`]
/// records keyed by output-order sequence.
///
/// Codex round 25 [accepted]: the previous `BTreeMap<u32, _>`
/// was byte-bounded only via `pending_push_bytes` — its count
/// could grow unboundedly when an early copy stalled while
/// many tiny later frames completed (each completion
/// decrements `pending_copies`, which is the *count* gate on
/// admission, but pending_push entries don't count there).
/// Worse, the BTreeMap insert in the copy-completion callback
/// was an infallible allocation: a node-alloc OOM would have
/// panicked in a JS-callback context, leaving the underlying
/// `web_sys::VideoDecoder` running while the adapter aborted.
///
/// Mirrors [`PendingOutputMap`]: capacity is fixed at
/// construction (`MAX_PENDING_PUSHES` slots, sized to match
/// `MAX_PENDING_DECODE`); the existing admission gate is
/// extended to include `pending_pushes.len()` so the *total*
/// in-flight count (`decode_queue_size + pending_copies +
/// pending_pushes.len()`) is bounded by `MAX_PENDING_DECODE`.
/// Insert is provably alloc-free as long as that gate holds;
/// `try_insert` returns `Err` if the cap is somehow exceeded
/// so the completion callback can fail-closed via
/// `record_close` instead of aborting the tab.
pub(crate) struct PendingPushMap<F> {
  entries: Vec<(u32, PendingPush<F>)>,
}

impl<F> PendingPushMap<F> {
  /// Construct an empty reorder buffer preallocated to `cap`,
  /// fallibly. Codex round 29 [accepted]: same OOM-as-Result
  /// contract as [`PendingOutputMap::try_with_capacity`].
  pub fn try_with_capacity(cap: usize) -> Result<Self, Error> {
    let mut entries = Vec::new();
    entries
      .try_reserve_exact(cap)
      .map_err(|_| Error::from_static("out of memory: pending_pushes"))?;
    Ok(Self { entries })
  }

  /// Number of records currently parked.
  pub fn len(&self) -> usize {
    self.entries.len()
  }

  /// Insert (or overwrite) the record at `sequence`. Returns
  /// `Err(push)` (handing the value back) if the buffer is at
  /// capacity and the key isn't already present — caller is
  /// expected to translate that into `record_close` rather
  /// than reattempting. Same-key overwrite is always allowed
  /// since it doesn't grow the Vec.
  pub fn try_insert(&mut self, sequence: u32, push: PendingPush<F>) -> Result<(), PendingPush<F>> {
    match self.entries.binary_search_by_key(&sequence, |(k, _)| *k) {
      Ok(idx) => {
        self.entries[idx].1 = push;
        Ok(())
      }
      Err(idx) => {
        if self.entries.len() >= self.entries.capacity() {
          return Err(push);
        }
        self.entries.insert(idx, (sequence, push));
        Ok(())
      }
    }
  }

  /// Remove and return the record at `sequence`, if present.
  pub fn remove(&mut self, sequence: u32) -> Option<PendingPush<F>> {
    let idx = self
      .entries
      .binary_search_by_key(&sequence, |(k, _)| *k)
      .ok()?;
    Some(self.entries.remove(idx).1)
  }

  /// Drop every parked record. Capacity is preserved.
  pub fn clear(&mut self) {
    self.entries.clear();
  }
}

/// Outcome of an output-callback copy task, awaiting its turn
/// in [`Inner::pending_pushes`].
pub(crate) enum PendingPush<F> {
  /// Copy succeeded; push the frame on its turn, charging
  /// `byte_size` against `queue_bytes`.
  Ready(DecodedFrame<F>, u64),
  /// Copy errored, was stale, or otherwise produced nothing
  /// to publish — the queue still needs to advance past this
  /// sequence so the next-expected pointer keeps moving.
  Skipped,
}

impl<F> Inner<F> {
  fn try_new(max_queue: usize) -> Result<Self, Error> {
    let mut queue = VecDeque::new();
    queue
      .try_reserve_exact(max_queue)
      .map_err(|_| Error::from_static("out of memory: decoded-frame queue"))?;
    Ok(Self {
      queue,
      queue_bytes: 0,
      last_error: None,
      pending_outputs: PendingOutputMap::try_with_capacity(MAX_PENDING_OUTPUTS)?,
      pending_input_bytes: 0,
      next_submission_id: BASE_SUBMISSION_ID,
      epoch_id_floor: BASE_SUBMISSION_ID,
      pending_copies: 0,
      pending_copy_bytes: 0,
      last_measured_frame_bytes: 0,
      epoch: 0,
      next_output_sequence: 0,
      next_push_sequence: 0,
      pending_pushes: PendingPushMap::try_with_capacity(MAX_PENDING_PUSHES)?,
      pending_push_bytes: 0,
      receiver_waker: None,
      dequeue_waker: None,
    })
  }

  // -------- queue ----------

  /// Number of decoded frames waiting for `receive_frame`.
  pub fn queue_len(&self) -> usize {
    self.queue.len()
  }

  /// `true` if the queue holds no decoded frames.
  pub fn queue_is_empty(&self) -> bool {
    self.queue.is_empty()
  }

  /// Push a decoded frame, increasing [`Self::queue_bytes`] by
  /// `byte_size`. The two are kept in sync by passing the
  /// payload size at the same call site that owns the frame.
  ///
  /// Returns `Err(frame)` (handing the value back) if the
  /// queue is at its preallocated capacity. Codex round 27
  /// [accepted]: `VecDeque::push_back` is panic-on-OOM, and
  /// callers run on output / copy-completion paths that
  /// already hold sizable JS / Rust allocations — a queue
  /// growth abort would tear the wasm tab down instead of
  /// going through `record_close`. Callers handle the `Err`
  /// the same way they handle `PendingPushMap::try_insert`
  /// failure: `record_close` + `wake_all`.
  pub fn push_queue(
    &mut self,
    frame: DecodedFrame<F>,
    byte_size: u64,
  ) -> Result<(), DecodedFrame<F>> {
    if self.queue.len() >= self.queue.capacity() {
      return Err(frame);
    }
    self.queue_bytes = self.queue_bytes.saturating_add(byte_size);
    self.queue.push_back(frame);
    Ok(())
  }

  /// Pop the oldest decoded frame and decrement
  /// [`Self::queue_bytes`] by the same amount it was pushed
  /// with (`byte_size`). Returns `None` if the queue is empty.
  pub fn pop_queue(&mut self, byte_size: u64) -> Option<DecodedFrame<F>> {
    let frame = self.queue.pop_front()?;
    self.queue_bytes = self.queue_bytes.saturating_sub(byte_size);
    Some(frame)
  }

  /// Borrow the head of the queue without removing it. Used
  /// by callers that need to read a backend-specific byte
  /// size off the body before calling [`Self::pop_queue`].
  pub fn peek_queue_head(&self) -> Option<&F> {
    self.queue.front().map(DecodedFrame::frame)
  }

  /// Total bytes pinned across the queued decoded frames.
  pub const fn queue_bytes(&self) -> u64 {
    self.queue_bytes
  }

  // -------- last_error ----------

  /// `true` if the decoder has been marked closed by a fatal
  /// error path.
  pub const fn is_closed(&self) -> bool {
    self.last_error.is_some()
  }

  /// Clone the last fatal error (commonly returned to the
  /// user as `VideoDecodeError::Closed(err)`).
  pub fn last_error_clone(&self) -> Option<Error> {
    self.last_error.clone()
  }

  /// Clear the closed marker. Used by `flush()` only after
  /// `reset()` and `configure()` both succeed — clearing
  /// before that would let `receive_frame` park indefinitely
  /// on a permanently dead decoder.
  pub fn clear_last_error(&mut self) {
    self.last_error = None;
  }

  // -------- pending_outputs ----------

  /// Allocate the next submission ID. Strictly monotonic so two
  /// chunks across different generations can't collide.
  pub fn next_submission_id(&mut self) -> i64 {
    let id = self.next_submission_id;
    self.next_submission_id = self.next_submission_id.wrapping_add(1);
    id
  }

  /// Insert a `PendingOutput` record. Fail-closed when the
  /// side map is already at [`MAX_PENDING_OUTPUTS`]: run
  /// `record_close` and return `Err`, so the caller drains the
  /// underlying JS decoder and surfaces `Closed` to the user.
  ///
  /// The cap is genuinely a failsafe — under normal operation
  /// every input has one matching output, and even under the
  /// timestamp-rebase regime (Chrome's PCM decoders emit
  /// microsecond `AudioData.timestamp` / `VideoFrame.timestamp`
  /// rather than echoing the synthetic submission ID we stamp
  /// on the chunk), the output callback drains the side map by
  /// FIFO-popping the oldest record on every miss (see
  /// `pop_oldest_pending_output`). The cap therefore only
  /// trips when admission has genuinely outpaced output for
  /// long enough that 256 chunks are simultaneously unmatched
  /// — which in turn means the browser is holding compressed
  /// bytes we never accounted for. Silent eviction with
  /// byte-counter decrement (an earlier attempt) would have
  /// hidden that retention.
  pub fn insert_pending_output(&mut self, id: i64, record: PendingOutput) -> Result<(), Error> {
    if self.pending_outputs.len() >= MAX_PENDING_OUTPUTS {
      let err = Error::from_js(JsValue::from_str(
        "WebCodecs pending_outputs reached MAX_PENDING_OUTPUTS; \
         decoder is producing far fewer outputs than inputs",
      ));
      self.record_close(err.clone());
      return Err(err);
    }
    self.pending_input_bytes = self
      .pending_input_bytes
      .saturating_add(record.input_bytes as u64);
    self.pending_outputs.insert(id, record);
    Ok(())
  }

  /// Remove the pending-output record for `id`, decrementing
  /// [`Self::pending_input_bytes`] in lockstep. Use this in
  /// place of `pending_outputs.remove(&id)` everywhere — the
  /// counter would otherwise drift by the byte size of any
  /// chunk pulled out individually (output callback,
  /// `send_packet` rejection cleanup).
  pub fn remove_pending_output(&mut self, id: i64) -> Option<PendingOutput> {
    let record = self.pending_outputs.remove(id)?;
    self.pending_input_bytes = self
      .pending_input_bytes
      .saturating_sub(record.input_bytes as u64);
    Some(record)
  }

  /// Pop the oldest pending-output record (smallest submission
  /// ID), decrementing [`Self::pending_input_bytes`] in
  /// lockstep. Used by the output callback when a current-
  /// generation timestamp lookup misses: WebCodecs
  /// implementations may rebase `AudioData.timestamp` /
  /// `VideoFrame.timestamp` onto their own clock (Chrome's
  /// PCM decoders do this — they emit microsecond stamps
  /// rather than echoing the synthetic submission ID we put
  /// on the chunk), so the side-map key never matches. Under
  /// that regime, output order matches input order (PCM is
  /// non-reordering), so popping the oldest pending record
  /// retires the right one — it preserves the user's PTS for
  /// the matching frame *and* keeps `pending_input_bytes`
  /// honest. Returns `None` if the map is empty (a genuinely
  /// unsolicited callback we should drop silently).
  pub fn pop_oldest_pending_output(&mut self) -> Option<(i64, PendingOutput)> {
    let (id, record) = self.pending_outputs.pop_first()?;
    self.pending_input_bytes = self
      .pending_input_bytes
      .saturating_sub(record.input_bytes as u64);
    Some((id, record))
  }

  /// Aggregate compressed input bytes pinned in JS heap
  /// across `pending_outputs`. See the cap-check site in
  /// `video.rs::send_packet`.
  pub const fn pending_input_bytes(&self) -> u64 {
    self.pending_input_bytes
  }

  // -------- pending_copies ----------

  /// Number of in-flight `copyTo` operations spawned from the
  /// output callback. EOF gates on this reaching zero
  /// alongside an empty queue.
  pub const fn pending_copies(&self) -> u32 {
    self.pending_copies
  }

  /// Total bytes reserved by in-flight copy tasks (the sum of
  /// `allocation_size_with_options` captured at admission per
  /// task).
  pub const fn pending_copy_bytes(&self) -> u64 {
    self.pending_copy_bytes
  }

  /// Total bytes pinned across `Ready` entries in
  /// `pending_pushes` waiting on an earlier sequence to drain.
  /// Admission and completion paths add this to
  /// `queue_bytes()` and `pending_copy_bytes()` so the total
  /// memory accounted under `MAX_INFLIGHT_BYTES` reflects the
  /// limbo state too.
  pub const fn pending_push_bytes(&self) -> u64 {
    self.pending_push_bytes
  }

  /// Number of [`PendingPush`] records currently parked in
  /// the reorder buffer. Folded into the admission gate
  /// alongside `decode_queue_size` and `pending_copies` so
  /// total in-flight count is bounded by [`MAX_PENDING_DECODE`]
  /// (codex round 25): a stalled head copy plus many tiny
  /// later completions can't grow this past the cap.
  pub fn pending_pushes_len(&self) -> usize {
    self.pending_pushes.len()
  }

  /// Admit a new in-flight copy: increment count and add
  /// `byte_estimate` to [`Self::pending_copy_bytes`]. The
  /// matching [`Self::sub_pending_copy`] must subtract the
  /// same value at task completion to keep the counter
  /// honest. Also stamps `byte_estimate` as
  /// [`Self::last_measured_frame_bytes`] so the next
  /// admission can budget against an actual decoded size
  /// rather than an open-time projection.
  pub fn add_pending_copy(&mut self, byte_estimate: u64) {
    self.pending_copies = self.pending_copies.saturating_add(1);
    self.pending_copy_bytes = self.pending_copy_bytes.saturating_add(byte_estimate);
    self.last_measured_frame_bytes = byte_estimate;
  }

  /// Most recent measured frame peak (or 0 if no output has
  /// arrived yet). Used by `await_decode_room` as a
  /// data-driven headroom estimate.
  pub const fn last_measured_frame_bytes(&self) -> u64 {
    self.last_measured_frame_bytes
  }

  /// Lowest submission ID that belongs to the current epoch.
  /// IDs `< epoch_id_floor()` were issued before the most
  /// recent `bump_epoch` and should be treated as stale by
  /// output callbacks even when the epoch tag on a still-
  /// present record happens to match.
  pub const fn epoch_id_floor(&self) -> i64 {
    self.epoch_id_floor
  }

  /// Account for a completed copy task: decrement count and
  /// subtract `byte_estimate` from [`Self::pending_copy_bytes`].
  pub fn sub_pending_copy(&mut self, byte_estimate: u64) {
    self.pending_copies = self.pending_copies.saturating_sub(1);
    self.pending_copy_bytes = self.pending_copy_bytes.saturating_sub(byte_estimate);
  }

  // -------- epoch ----------

  /// Current decoder generation. Bumped by `flush()` and at
  /// decoder Drop via [`SharedState::bump_epoch`]; copy tasks
  /// capture this at spawn and discard results when it
  /// advances.
  pub const fn epoch(&self) -> u64 {
    self.epoch
  }

  // -------- wakers ----------

  /// Register the receiver waker (called by the
  /// `receive_frame` poll_fn from inside its borrow_mut).
  pub fn set_receiver_waker(&mut self, waker: Waker) {
    self.receiver_waker = Some(waker);
  }

  /// Register the dequeue waker.
  pub fn set_dequeue_waker(&mut self, waker: Waker) {
    self.dequeue_waker = Some(waker);
  }

  /// Clear the dequeue waker (`await_decode_room` drops the
  /// waker from inside its borrow when it observes the wait
  /// condition has changed).
  pub fn clear_dequeue_waker(&mut self) {
    self.dequeue_waker = None;
  }

  // -------- pending_outputs (auxiliary) ----------

  /// Drop every pending side-map record. Used by `send_eof`'s
  /// success path (residual entries are zero-output chunks).
  /// Decrements `pending_input_bytes` to zero in lockstep so
  /// the input-byte counter doesn't drift.
  pub fn clear_pending_outputs(&mut self) {
    self.pending_outputs.clear();
    self.pending_input_bytes = 0;
  }

  /// Record a fatal close on the decoder, releasing
  /// per-generation buffers that callers can no longer reach.
  ///
  /// Once `last_error` is set, every public entry point
  /// (`receive_frame`, `send_packet`, `send_eof`, …) checks it
  /// before touching `queue`, so any frames already pushed onto
  /// `queue` become unreachable from that point on. Each
  /// queued [`DecodedFrame`] holds a `WebCodecsBuffer` whose
  /// `Arc<[u8]>` keeps a sizeable wasm allocation alive
  /// (full-frame video planes), so leaving them parked there
  /// pins memory in the browser tab until the decoder is
  /// dropped or `flush()` is called. `pending_outputs` shares
  /// the same fate — its records are only ever consumed by
  /// the output callback's matching path. This helper clears
  /// both. It does **not** clear `pending_copies`: live copy
  /// tasks decrement it themselves on completion regardless of
  /// `last_error`, so resetting it here would race with those
  /// tasks and double-decrement on completion.
  ///
  /// Caller is responsible for dropping the borrow and calling
  /// [`SharedState::wake_all`] afterwards so any parked waiter
  /// observes the closed state. The boolean return is `true`
  /// if `last_error` transitioned from unset to set on this
  /// call (i.e. this is the first close), allowing the caller
  /// to fire one-shot side effects like
  /// [`SharedState::invoke_close_hook`] without
  /// double-invocation when overlapping callbacks race to
  /// close the decoder.
  pub fn record_close(&mut self, err: Error) -> bool {
    let just_closed = self.last_error.is_none();
    self.last_error.get_or_insert(err);
    self.queue.clear();
    self.queue_bytes = 0;
    self.pending_outputs.clear();
    self.pending_input_bytes = 0;
    // Drop buffered out-of-order video copies; with the
    // decoder closed they're unreachable, and codex round 19
    // flagged that leaving them parked would pin
    // `Arc<[u8]>` memory until the next flush/drop. Reset
    // the sequence cursors too so a re-opened decoder (via
    // flush) starts fresh from 0/0.
    self.pending_pushes.clear();
    self.pending_push_bytes = 0;
    self.next_output_sequence = 0;
    self.next_push_sequence = 0;
    just_closed
  }
}

/// Hard cap on the [`Inner::pending_outputs`] side map. The map
/// holds one entry per `decoder.decode()` call until the
/// matching output callback fires (or `bump_epoch` /
/// `send_eof` clears it). For typical streams the live size
/// stays around `MAX_PENDING_DECODE` plus the codec's reorder
/// buffer (≤ 32 frames for h264 / hevc), so the cap is only
/// hit by adversarial streams that accept chunks without ever
/// emitting outputs. On overflow we surface an observable
/// error rather than silently dropping records.
pub const MAX_PENDING_OUTPUTS: usize = 256;

/// Hard cap on the [`Inner::pending_pushes`] reorder buffer.
/// Sized to match `MAX_PENDING_DECODE` because the admission
/// gate (`video.rs::pending_decode`) bounds the *total*
/// in-flight count (`decode_queue_size + pending_copies +
/// pending_pushes_len`) by `MAX_PENDING_DECODE`, so
/// `pending_pushes_len` alone is bounded by the same number.
/// Codex round 25 [accepted].
pub const MAX_PENDING_PUSHES: usize = 32;

/// Lower bound of the synthetic submission-ID namespace.
/// `next_submission_id` starts here and increments from there,
/// so every chunk the adapter admits carries a timestamp
/// (`EncodedAudioChunk.timestamp` / `EncodedVideoChunk.timestamp`)
/// of at least `1 << 50` ≈ 1.1 × 10¹⁵.
///
/// The point is anti-aliasing. Chrome's PCM `AudioDecoder`
/// rebases output `AudioData.timestamp` onto its own clock —
/// it emits microsecond values rather than echoing the input
/// chunk's timestamp. With submission IDs starting at 0 those
/// microsecond values could *coincidentally* match a future
/// chunk's submission ID, and `remove_pending_output` would
/// pull the wrong record (incorrect PTS/key, mis-counted
/// `pending_input_bytes`). With submission IDs >= `2^50`,
/// rebased microsecond timestamps (always small positives —
/// one quintillion microseconds is ~31 700 years) cannot
/// physically reach this range, so a positive value below the
/// base is unambiguously "not one of ours" and triggers the
/// FIFO-pop fallback in the output callback.
///
/// `f64` round-trips integers up to `2^53` losslessly, so
/// this base + counter range stays exact when crossing the JS
/// boundary as `set_timestamp_f64`.
pub const BASE_SUBMISSION_ID: i64 = 1 << 50;

/// Hook target that fires `reset()` on the underlying
/// WebCodecs decoder when [`SharedState::invoke_close_hook`]
/// runs. A concrete enum (rather than the previous
/// `Box<dyn Fn()>`) so installation is allocation-free —
/// `Box::new` was the last infallible alloc on the open path
/// (codex round 30 [accepted]).
#[derive(Default, Clone)]
pub(crate) enum CloseHookTarget {
  /// No hook installed yet; `invoke_close_hook` is a no-op.
  #[default]
  None,
  /// Video decoder hook: `reset()` drops the encoded-chunk
  /// queue and pending decode work, leaving the decoder in
  /// the recoverable "unconfigured" state.
  Video(web_sys::VideoDecoder),
  /// Audio decoder hook: same `reset()` semantics as
  /// [`CloseHookTarget::Video`].
  Audio(web_sys::AudioDecoder),
}

impl CloseHookTarget {
  /// Fire the hook (no-op if no decoder is installed).
  fn fire(&self) {
    match self {
      Self::None => {}
      Self::Video(d) => {
        let _ = d.reset();
      }
      Self::Audio(d) => {
        let _ = d.reset();
      }
    }
  }
}

/// Refcounted handle around the shared state.
pub(crate) struct SharedState<F> {
  inner: Rc<RefCell<Inner<F>>>,
  /// Hook that closes the underlying WebCodecs decoder. Set
  /// once by the adapter constructor (after the JS decoder is
  /// built) via [`SharedState::set_close_hook_video`] /
  /// [`SharedState::set_close_hook_audio`].
  ///
  /// Without this, an adapter-internal fatal close (output
  /// burst exceeded a cap, queue overflowed at copy
  /// completion, …) flips the Rust-side closed flag while
  /// leaving the underlying `web_sys::VideoDecoder` /
  /// `AudioDecoder` running. The browser's decoder keeps
  /// holding the encoded-chunk queue (each chunk a
  /// `Uint8Array` copy of the packet bytes) and its pending
  /// decode work in JS / GPU memory until the user reacts to
  /// `Closed` by calling `flush()` (which `reset()`s the
  /// decoder) or dropping the adapter. In a long-lived
  /// browser tab — the typical host for this crate — that
  /// gap is observable wasm-and-JS-heap retention.
  ///
  /// Stored separately from [`Inner`] (i.e. NOT behind the
  /// same `RefCell`) so [`SharedState::record_close`] can
  /// invoke it after dropping the inner borrow, sidestepping
  /// any reentrance worries from the JS bridge.
  close_hook: Rc<RefCell<CloseHookTarget>>,
}

impl<F> SharedState<F> {
  /// Construct a new shared-state handle, fallibly. `max_queue`
  /// is the preallocated capacity for [`Inner::queue`] —
  /// callers pass `MAX_QUEUED_OUTPUT + MAX_PENDING_DECODE`
  /// (the most the consumer-side queue can grow before the
  /// admission gate forces backpressure). See [`Inner::push_queue`]
  /// for the rationale (codex round 27).
  ///
  /// Returns `Err` if any of the three preallocations
  /// (`queue`, `pending_outputs`, `pending_pushes`) fails.
  /// Codex round 29 [accepted]: previously this used
  /// `*::with_capacity` which is panic-on-OOM; the open-time
  /// alloc must surface as an `Err` so the wasm tab observes
  /// a `*DecodeError::Js` instead of an abort under memory
  /// pressure during decoder construction.
  pub fn try_new(max_queue: usize) -> Result<Self, Error> {
    Ok(Self {
      inner: Rc::new(RefCell::new(Inner::try_new(max_queue)?)),
      close_hook: Rc::new(RefCell::new(CloseHookTarget::None)),
    })
  }

  /// Register the video-decoder close hook. Called once by
  /// the adapter constructor right after the JS decoder is
  /// built; subsequent calls overwrite (the adapter only
  /// builds one decoder, so this is effectively set-once).
  /// Allocation-free — the previous `Box<dyn Fn()>` install
  /// path was the last infallible heap alloc on `open` (codex
  /// round 30).
  pub fn set_close_hook_video(&self, decoder: web_sys::VideoDecoder) {
    *self.close_hook.borrow_mut() = CloseHookTarget::Video(decoder);
  }

  /// Register the audio-decoder close hook. See
  /// [`Self::set_close_hook_video`].
  pub fn set_close_hook_audio(&self, decoder: web_sys::AudioDecoder) {
    *self.close_hook.borrow_mut() = CloseHookTarget::Audio(decoder);
  }

  /// Invoke the registered close hook (if any) at a safe
  /// point — i.e. after the inner borrow has been dropped.
  /// Call sites typically run [`Inner::record_close`] inline
  /// inside a larger `borrow_mut` block and use its boolean
  /// `just_closed` return to fire this hook *once* per
  /// close, so the JS-side decoder close step lands in the
  /// browser without double round-trips.
  pub fn invoke_close_hook(&self) {
    self.close_hook.borrow().fire();
  }

  /// Drop the registered close hook, releasing the decoder
  /// `JsValue` handle it captured. The adapter's `Drop` calls
  /// this so the wasm-bindgen object-table slot for that
  /// handle is freed immediately — without it, an in-flight
  /// spawned copy task that still holds a `SharedState`
  /// clone would keep the close-hook `Rc` alive, and through
  /// it the decoder handle, until the task finishes. The JS
  /// decoder is already `close()`d at adapter Drop, so the
  /// retained handle pins only a Rust-side slot rather than
  /// browser GPU resources, but in long-lived host pages
  /// even that slot is worth releasing eagerly.
  pub fn clear_close_hook(&self) {
    *self.close_hook.borrow_mut() = CloseHookTarget::None;
  }

  pub fn borrow(&self) -> std::cell::Ref<'_, Inner<F>> {
    self.inner.borrow()
  }

  pub fn borrow_mut(&self) -> std::cell::RefMut<'_, Inner<F>> {
    self.inner.borrow_mut()
  }

  /// Snapshot the current epoch — copy tasks capture this at spawn.
  pub fn epoch(&self) -> u64 {
    self.inner.borrow().epoch
  }

  /// Bump the epoch and clear queued frames. Used by `flush()`
  /// and decoder `Drop`. Returns the new epoch.
  ///
  /// **Does not reset `pending_copies`** — the JS-side copy
  /// tasks spawned in prior generations are still alive, holding
  /// GPU surfaces and JS allocations until their `copyTo`
  /// Promises settle. Forcing the counter to zero on `flush`
  /// would let new-generation producers admit a fresh
  /// `MAX_INFLIGHT` worth of work on top of those stale
  /// copies, defeating the cap. Each task decrements
  /// `pending_copies` itself on completion regardless of
  /// epoch, and stale tasks discard their result without
  /// pushing it onto the queue.
  ///
  /// **Does not reset `last_error`** — `flush()`'s `reset()` /
  /// `configure()` calls can fail (especially when the decoder
  /// has already been closed by a prior fatal error), and
  /// silently dropping the closed marker before retrying would
  /// leave a subsequent `receive_frame` parked forever on a
  /// permanently dead decoder. `flush()` clears `last_error`
  /// itself only after `reset()` and `configure()` both
  /// succeed.
  ///
  /// Clears `pending_outputs` — `decoder.reset()` (called by
  /// `flush()`) drops every queued and in-flight chunk, so the
  /// prior generation's submission records are no longer
  /// expecting outputs. A late callback that fires anyway will
  /// either miss the (now-empty) map or find an entry from a
  /// future generation that doesn't match its old `epoch`,
  /// either way getting discarded.
  pub fn bump_epoch(&self) -> u64 {
    let mut inner = self.inner.borrow_mut();
    inner.epoch = inner.epoch.wrapping_add(1);
    inner.queue.clear();
    inner.queue_bytes = 0;
    inner.pending_outputs.clear();
    inner.pending_input_bytes = 0;
    inner.pending_pushes.clear();
    inner.pending_push_bytes = 0;
    inner.next_output_sequence = 0;
    inner.next_push_sequence = 0;
    // Snapshot the next-id boundary: IDs below this point
    // were issued in the prior generation, IDs from this
    // point onward are current-epoch.
    inner.epoch_id_floor = inner.next_submission_id;
    inner.epoch
  }

  /// Allocate the next output-callback sequence number. Used
  /// by the video adapter to preserve `output()` order across
  /// async copy completions — see the `pending_pushes` field
  /// doc on [`Inner`] and the call sites in `video.rs`.
  pub fn allocate_output_sequence(&self) -> u32 {
    let mut inner = self.inner.borrow_mut();
    let s = inner.next_output_sequence;
    inner.next_output_sequence = inner.next_output_sequence.wrapping_add(1);
    s
  }

  /// Deliver a completed copy task's outcome at its assigned
  /// `sequence` and drain any frames waiting on now-available
  /// earlier sequences. The drain is in-order: while
  /// `pending_pushes` contains an entry at
  /// `next_push_sequence`, pop it and either push the frame
  /// onto the queue (charging `byte_size`) or skip it,
  /// advancing the pointer in either case.
  ///
  /// Wakes the receiver waker if at least one `Ready` push
  /// landed on the queue.
  pub fn deliver_pending_push(&self, sequence: u32, push: PendingPush<F>) {
    // `overflow_close` is `Some(just_closed)` if we hit the
    // queue cap during drain — the caller invokes the close
    // hook (gated on `just_closed`) and runs `wake_all` after
    // dropping the borrow.
    let (pushed_any, overflow_close): (bool, Option<bool>) = {
      let mut inner = self.inner.borrow_mut();
      // Closed-state guard: codex round 19 flagged that a
      // late `Ready` delivery could otherwise pin
      // `Arc<[u8]>` memory in a map no public method drains
      // (and a later `Skipped` could even drag earlier
      // `Ready`s into `queue` *behind* `last_error`).
      // `record_close` clears `pending_pushes`, so any
      // delivery arriving afterwards is dropped here. The
      // `Ready` variant's frame falls out of scope at end
      // of this branch, releasing its allocation.
      if inner.is_closed() {
        (false, None)
      } else {
        // Track bytes pinned in the map so admission paths
        // see them too. Tracked *before* the insert so a
        // capacity overflow leaves bookkeeping consistent
        // with the rolled-back insert.
        let added_bytes = match &push {
          PendingPush::Ready(_, byte_size) => *byte_size,
          PendingPush::Skipped => 0,
        };
        inner.pending_push_bytes = inner.pending_push_bytes.saturating_add(added_bytes);
        // Codex round 25: bounded reorder buffer. The
        // admission gate (video.rs `pending_decode`) folds in
        // `pending_pushes_len()` so this branch should never
        // exceed capacity in practice — but if it does, fail
        // closed via `record_close` rather than aborting on
        // an infallible BTreeMap insert.
        if let Err(_returned) = inner.pending_pushes.try_insert(sequence, push) {
          inner.pending_push_bytes = inner.pending_push_bytes.saturating_sub(added_bytes);
          let err = Error::from_static(
            "WebCodecs pending_pushes reached capacity; \
             admission gate failed to bound reorder buffer",
          );
          let just_closed = inner.record_close(err);
          drop(inner);
          // Codex round 27 [accepted]: invoke the close hook
          // *before* `wake_all` so the JS decoder is reset
          // before any inline-polling waker can wake user
          // code. Otherwise a woken `receive_frame` future
          // could observe `Closed`, call `flush()` (which
          // re-`configure`s the decoder), and *then* this
          // overflow handler's close-hook would fire and
          // reset the freshly recovered decoder. Matches
          // the order used by every other fatal-close site
          // (e.g. `video.rs` output-callback close paths).
          //
          // Codex round 26 [accepted]: `record_close` only
          // stamps the closed flag — without `wake_all` a
          // `receive_frame` future already parked on
          // `receiver_waker` (or `send_packet` parked on
          // `dequeue_waker`) would sleep forever, since no
          // further browser callbacks are guaranteed after
          // the underlying `reset()`.
          if just_closed {
            self.invoke_close_hook();
          }
          self.wake_all();
          return;
        }
        let mut pushed = false;
        let mut queue_overflow: Option<bool> = None;
        loop {
          // Local copy of the cursor so the `remove` call
          // below doesn't double-borrow `inner`.
          let next = inner.next_push_sequence;
          let Some(entry) = inner.pending_pushes.remove(next) else {
            break;
          };
          match entry {
            PendingPush::Ready(frame, byte_size) => {
              inner.pending_push_bytes = inner.pending_push_bytes.saturating_sub(byte_size);
              if let Err(_returned) = inner.push_queue(frame, byte_size) {
                // Codex round 27 [accepted]: queue at its
                // preallocated cap. Admission gating
                // (`MAX_QUEUED_OUTPUT + MAX_PENDING_DECODE`)
                // should make this unreachable, but if it
                // happens fail-closed rather than fall through
                // to a `VecDeque::push_back` OOM panic.
                let err = Error::from_static(
                  "WebCodecs decoded-frame queue reached capacity; \
                   admission gate failed to bound the output queue",
                );
                queue_overflow = Some(inner.record_close(err));
                break;
              }
              pushed = true;
            }
            PendingPush::Skipped => {}
          }
          inner.next_push_sequence = next.wrapping_add(1);
        }
        (pushed, queue_overflow)
      }
    };
    if let Some(just_closed) = overflow_close {
      // Same fatal-close ordering as the pending_pushes
      // overflow path above: hook before wake.
      if just_closed {
        self.invoke_close_hook();
      }
      self.wake_all();
      return;
    }
    if pushed_any {
      self.wake_receiver();
    }
  }

  /// Wake the receiver waker (if any). Borrow-released before
  /// the wake to avoid re-borrow panics on inline-polling
  /// executors.
  fn wake_receiver(&self) {
    let waker = self.inner.borrow_mut().receiver_waker.take();
    if let Some(w) = waker {
      w.wake();
    }
  }

  /// Take the dequeue (backpressure) waker (if any) and wake it.
  /// The borrow is released before `Waker::wake()` runs, so an
  /// executor that inlines polling can re-enter the state without
  /// tripping a `RefCell` re-borrow panic.
  pub fn wake_dequeue(&self) {
    let waker = self.inner.borrow_mut().dequeue_waker.take();
    if let Some(w) = waker {
      w.wake();
    }
  }

  /// Wake both wakers — used by error paths so a producer
  /// blocked on backpressure unblocks alongside the consumer.
  /// Without this, a fatal error while `send_packet` is parked
  /// at the queue cap (and no future `dequeue` event ever
  /// fires, because the decoder is closed) would strand the
  /// producer future indefinitely.
  pub fn wake_all(&self) {
    let (rx, dq) = {
      let mut inner = self.inner.borrow_mut();
      (inner.receiver_waker.take(), inner.dequeue_waker.take())
    };
    if let Some(w) = rx {
      w.wake();
    }
    if let Some(w) = dq {
      w.wake();
    }
  }
}

impl<F> Clone for SharedState<F> {
  fn clone(&self) -> Self {
    Self {
      inner: Rc::clone(&self.inner),
      close_hook: Rc::clone(&self.close_hook),
    }
  }
}

/// RAII guard that clears `receiver_waker` on drop.
///
/// `poll_fn`-based waiters store `cx.waker().clone()` into the
/// shared state and yield. If the awaiting future is cancelled
/// (e.g. wrapped in a `tokio::time::timeout` that fires, or
/// dropped because the parent task aborted) the waker stays in
/// the slot, keeping the cancelled task's resources alive until
/// some unrelated decoder event eventually clears it. Wrapping
/// each `await` site in this guard means the slot is cleared on
/// every exit path — wake-then-poll, ready-without-storing, and
/// cancellation alike.
pub(crate) struct ReceiverWakerGuard<'a, F> {
  state: &'a SharedState<F>,
}

impl<F> Drop for ReceiverWakerGuard<'_, F> {
  fn drop(&mut self) {
    // Idempotent: if the wake path already took the waker, the
    // slot is `None` and this is a no-op.
    self.state.inner.borrow_mut().receiver_waker = None;
  }
}

/// RAII guard that clears `dequeue_waker` on drop. Same shape
/// as [`ReceiverWakerGuard`].
pub(crate) struct DequeueWakerGuard<'a, F> {
  state: &'a SharedState<F>,
}

impl<F> Drop for DequeueWakerGuard<'_, F> {
  fn drop(&mut self) {
    self.state.inner.borrow_mut().dequeue_waker = None;
  }
}

impl<F> SharedState<F> {
  /// Construct a [`ReceiverWakerGuard`] tied to this state. The
  /// guard's lifetime borrows `self` so the caller cannot
  /// accidentally drop the state while the guard is live.
  pub fn receiver_waker_guard(&self) -> ReceiverWakerGuard<'_, F> {
    ReceiverWakerGuard { state: self }
  }

  /// Construct a [`DequeueWakerGuard`] tied to this state.
  pub fn dequeue_waker_guard(&self) -> DequeueWakerGuard<'_, F> {
    DequeueWakerGuard { state: self }
  }
}

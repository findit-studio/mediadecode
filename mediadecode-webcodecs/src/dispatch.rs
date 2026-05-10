//! Stable JS trampolines for WebCodecs callbacks.
//!
//! See the design discussion at audio.rs / video.rs Drop —
//! the short version: each callback handler lives in a
//! thread-local slot map keyed by an ID, and the JS function
//! the WebCodecs decoder actually holds is a plain trampoline
//! that calls a once-leaked dispatcher with the slot ID baked
//! in. The trampolines and dispatchers are never invalidated;
//! decoder Drop / flush rebuild just removes the slot and any
//! late callback observes an empty entry and orphan-closes
//! its `AudioData` / `VideoFrame`.
//!
//! Properties:
//! - **No invalidation hazard**: trampolines and dispatchers
//!   are stable JS values for the process lifetime.
//! - **No timing assumption**: slot removal is synchronous
//!   and immediately effective.
//! - **Bounded leak**: exactly two `Closure`s leak at first
//!   use (one per callback shape), regardless of decoder
//!   count. Per-decoder allocations live in the slot map and
//!   free deterministically.
//!
//! Re-entrancy: handler bodies often need to wake user
//! tasks (`state.wake_all()`), and those wakes may inline-
//! poll a task whose body in turn drops or flushes the
//! decoder — triggering `free_*_handler` for the very slot
//! we're dispatching from. To avoid re-borrowing the slot
//! map's `RefCell` while a handler runs (which would panic),
//! the dispatcher uses a take-and-put pattern: it `take()`s
//! the handler out of its `Option<Box<...>>` slot under a
//! short borrow, drops the borrow, runs the handler, then
//! re-acquires the borrow to put the handler back — *only
//! if* the slot still exists (i.e., the user didn't free it
//! during the call). A user `free_*_handler` that races with
//! dispatch finds the slot present (with `None` value, since
//! we took the handler out) and removes the entry; the
//! dispatcher's re-insert sees the missing entry and drops
//! the handler instead.

use std::{cell::RefCell, collections::HashMap};

use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::error::Error;

thread_local! {
  /// `Option<Box<...>>`: `Some` while the handler is parked,
  /// `None` while the dispatcher has it taken out. The slot's
  /// presence in the map is the "is this slot live?" signal;
  /// `free_*_handler` removes the entry entirely.
  static VALUE_HANDLERS: RefCell<HashMap<u64, Option<Box<dyn FnMut(JsValue)>>>> =
    RefCell::new(HashMap::new());
  static VOID_HANDLERS: RefCell<HashMap<u64, Option<Box<dyn FnMut()>>>> =
    RefCell::new(HashMap::new());
  /// Codex round 28 [accepted]: previously `u32` with
  /// `wrapping_add`. After ≈4 billion decoder lifecycles a
  /// freed slot ID could be reused, and a stale callback
  /// from an old decoder would invoke a *different* decoder's
  /// handler — same-base submission IDs make a side-map match
  /// plausible, so the stale frame would be published with the
  /// new stream's metadata. With `u64`, exhaustion would take
  /// >580 years at 1B allocations/sec, well past any realistic
  /// browser-tab lifetime; the increment uses `checked_add` so
  /// pathological cases panic deterministically rather than
  /// wrap silently.
  static NEXT_SLOT_ID: RefCell<u64> = const { RefCell::new(0) };
  static VALUE_DISPATCHER: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
  static VOID_DISPATCHER: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
}

/// Allocate a slot containing `handler` and return its ID.
/// Pair every allocation with a matching [`free_value_handler`]
/// at decoder Drop / replacement.
///
/// Returns `Err(Error)` if the slot map cannot reserve room
/// for one more entry — caller is expected to free any other
/// slots already allocated for the same decoder build and
/// surface the error to the user. Codex round 28 [accepted]:
/// `HashMap::insert` is a panic-on-OOM allocation, and the
/// open / rebuild path allocates three slots in sequence; an
/// OOM on slot 2 or 3 left earlier slots installed without an
/// owner to free them, or aborted the tab before the decoder
/// could return an `Err`.
pub(crate) fn allocate_value_handler(handler: Box<dyn FnMut(JsValue)>) -> Result<u64, Error> {
  let id = next_slot_id();
  VALUE_HANDLERS.with(|h| -> Result<(), Error> {
    let mut h = h.borrow_mut();
    h.try_reserve(1)
      .map_err(|_| Error::from_static("out of memory: dispatch value handler slot"))?;
    h.insert(id, Some(handler));
    Ok(())
  })?;
  Ok(id)
}

/// Fallible counterpart of [`allocate_value_handler`] for
/// `Fn()` handlers. Same OOM contract.
pub(crate) fn allocate_void_handler(handler: Box<dyn FnMut()>) -> Result<u64, Error> {
  let id = next_slot_id();
  VOID_HANDLERS.with(|h| -> Result<(), Error> {
    let mut h = h.borrow_mut();
    h.try_reserve(1)
      .map_err(|_| Error::from_static("out of memory: dispatch void handler slot"))?;
    h.insert(id, Some(handler));
    Ok(())
  })?;
  Ok(id)
}

/// Remove the value handler at `slot_id`. Subsequent JS-side
/// callbacks for this slot find an empty entry and orphan-
/// close their `AudioData` / `VideoFrame`.
pub(crate) fn free_value_handler(slot_id: u64) {
  VALUE_HANDLERS.with(|h| {
    h.borrow_mut().remove(&slot_id);
  });
}

/// Remove the void handler at `slot_id`.
pub(crate) fn free_void_handler(slot_id: u64) {
  VOID_HANDLERS.with(|h| {
    h.borrow_mut().remove(&slot_id);
  });
}

fn next_slot_id() -> u64 {
  NEXT_SLOT_ID.with(|n| {
    let mut n = n.borrow_mut();
    let id = *n;
    // u64 makes wrap practically impossible; `checked_add` makes
    // any pathological exhaustion deterministic rather than
    // silently aliasing freed slots.
    *n = n
      .checked_add(1)
      .expect("dispatch slot ID counter exhausted (>2^64 allocations)");
    id
  })
}

/// Build a per-decoder trampoline JS function for a value
/// handler at `slot_id`.
pub(crate) fn make_value_trampoline(slot_id: u64) -> js_sys::Function {
  let dispatcher = value_dispatcher();
  _mediadecode_value_trampoline(slot_id, &dispatcher)
}

/// Build a per-decoder trampoline JS function for a void
/// handler at `slot_id`.
pub(crate) fn make_void_trampoline(slot_id: u64) -> js_sys::Function {
  let dispatcher = void_dispatcher();
  _mediadecode_void_trampoline(slot_id, &dispatcher)
}

fn value_dispatcher() -> js_sys::Function {
  VALUE_DISPATCHER.with(|cell| {
    let mut slot = cell.borrow_mut();
    if let Some(f) = slot.as_ref() {
      return f.clone();
    }
    // First-use lazy init: build the dispatcher Closure once
    // and forget it. The forgotten Closure leaks but it's
    // *one* for the entire process — not per-decoder.
    let cb = Closure::<dyn FnMut(u64, JsValue)>::new(|slot_id: u64, value: JsValue| {
      // Take-and-put dispatch — see the re-entrancy note in
      // the module header. Phase 1: take the handler out of
      // its slot (or observe that the slot is empty).
      let taken: Option<Box<dyn FnMut(JsValue)>> =
        VALUE_HANDLERS.with(|h| h.borrow_mut().get_mut(&slot_id).and_then(|opt| opt.take()));
      let Some(mut handler) = taken else {
        // No live handler — orphan-close. Either the slot
        // was already freed, or a re-entrant dispatch is
        // currently running for the same slot (concurrent
        // same-slot dispatch shouldn't happen for a
        // serialised WebCodecs decoder, but the empty-slot
        // outcome is correct either way).
        close_orphan_value(value);
        return;
      };
      // Phase 2: invoke. The slot map is NOT borrowed across
      // this call, so the handler is free to call wakers
      // that re-enter the dispatch module via
      // `free_*_handler`.
      handler(value);
      // Phase 3: re-park the handler iff the slot still
      // exists. A `free_value_handler` call from inside the
      // handler removed the slot entry (it observed the
      // entry's presence with our `None` placeholder and
      // removed it whole), so this branch drops the handler.
      VALUE_HANDLERS.with(|h| {
        let mut h = h.borrow_mut();
        if let Some(opt) = h.get_mut(&slot_id) {
          *opt = Some(handler);
        }
        // else: slot freed during dispatch — handler drops
        // when this scope ends.
      });
    });
    let f: js_sys::Function = cb.as_ref().unchecked_ref::<js_sys::Function>().clone();
    cb.forget();
    *slot = Some(f.clone());
    f
  })
}

fn void_dispatcher() -> js_sys::Function {
  VOID_DISPATCHER.with(|cell| {
    let mut slot = cell.borrow_mut();
    if let Some(f) = slot.as_ref() {
      return f.clone();
    }
    let cb = Closure::<dyn FnMut(u64)>::new(|slot_id: u64| {
      // Take-and-put — see the value-dispatcher comment.
      let taken: Option<Box<dyn FnMut()>> =
        VOID_HANDLERS.with(|h| h.borrow_mut().get_mut(&slot_id).and_then(|opt| opt.take()));
      let Some(mut handler) = taken else {
        return;
      };
      handler();
      VOID_HANDLERS.with(|h| {
        let mut h = h.borrow_mut();
        if let Some(opt) = h.get_mut(&slot_id) {
          *opt = Some(handler);
        }
      });
    });
    let f: js_sys::Function = cb.as_ref().unchecked_ref::<js_sys::Function>().clone();
    cb.forget();
    *slot = Some(f.clone());
    f
  })
}

/// Best-effort cleanup for an orphan output that arrived
/// after its slot was freed. Tries the WebCodecs types we
/// know about (`AudioData`, `VideoFrame`); anything else is
/// dropped silently.
fn close_orphan_value(value: JsValue) {
  if let Ok(data) = value.clone().dyn_into::<web_sys::AudioData>() {
    data.close();
    return;
  }
  if let Ok(frame) = value.dyn_into::<web_sys::VideoFrame>() {
    frame.close();
  }
}

#[wasm_bindgen(inline_js = "
export function _mediadecode_value_trampoline(slotId, dispatcher) {
  return function(value) { dispatcher(slotId, value); };
}
export function _mediadecode_void_trampoline(slotId, dispatcher) {
  return function() { dispatcher(slotId); };
}
")]
extern "C" {
  fn _mediadecode_value_trampoline(slot_id: u64, dispatcher: &js_sys::Function)
  -> js_sys::Function;
  fn _mediadecode_void_trampoline(slot_id: u64, dispatcher: &js_sys::Function) -> js_sys::Function;
}

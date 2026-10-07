//! The lib test binary's allocator: [`System`], with a count of what one
//! thread allocates inside a measured window ([`measured`]).
//!
//! **Per thread, not per process**: the test harness allocates on its own
//! threads while a lane runs, so a process-wide count measures whoever is
//! busy. Only the thread inside the window counts — the lesson
//! `tests/metadata_allocation.rs` learned first. `Cell`s in `const`-
//! initialised thread-locals neither allocate nor register a destructor,
//! so the instrument cannot perturb what it measures.

use std::{
  alloc::{GlobalAlloc, Layout, System},
  cell::Cell,
};

thread_local! {
  static WATCHING: Cell<bool> = const { Cell::new(false) };
  static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
  static BYTES: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

fn record(size: usize) {
  // `try_with`, because a thread tearing down has already destroyed its
  // locals and must not be made to resurrect them.
  if WATCHING.try_with(Cell::get).unwrap_or(false) {
    let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
    let _ = BYTES.try_with(|bytes| bytes.set(bytes.get() + size));
  }
}

// SAFETY: every method forwards to `System`, which is a correct allocator;
// the bookkeeping reads and writes `Cell`s in `const`-initialised
// thread-locals, which allocate nothing and so cannot recurse into this
// allocator.
unsafe impl GlobalAlloc for Counting {
  unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
    record(layout.size());
    unsafe { System.alloc(layout) }
  }
  unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
    record(layout.size());
    unsafe { System.alloc_zeroed(layout) }
  }
  unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
    unsafe { System.dealloc(ptr, layout) }
  }
  unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    record(new_size);
    unsafe { System.realloc(ptr, layout, new_size) }
  }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Runs `body` with this thread's allocations counted, answering `(value,
/// allocations, bytes)`.
pub(crate) fn measured<T>(body: impl FnOnce() -> T) -> (T, usize, usize) {
  ALLOCATIONS.set(0);
  BYTES.set(0);
  WATCHING.set(true);
  let out = body();
  WATCHING.set(false);
  (out, ALLOCATIONS.get(), BYTES.get())
}

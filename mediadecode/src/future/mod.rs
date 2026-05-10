//! Async variants of the decoder / frame-source traits — gated
//! behind the `future` feature.
//!
//! Two parallel sub-modules expose the same trait names with
//! different `Send` semantics:
//!
//! - [`local`] — futures may be `!Send`. Right for browser /
//!   thread-pinned backends ([WebCodecs](crate) — `JsValue` is
//!   `!Send`; CUDA streams; VideoToolbox sessions).
//! - [`send`] — futures are `+ Send`. Right for multi-threaded
//!   executors (`tokio` multi-thread runtime, `async-std`,
//!   `smol::Executor`).
//!
//! Both are generated from a single source via
//! [`trait_variant`](https://docs.rs/trait_variant): the trait is
//! written with native `async fn` in [`local`] and the macro emits
//! the `Send`-bounded sibling. Pick the variant that matches your
//! runtime; backends typically implement only one of the two.
//!
//! # Pattern
//!
//! Implementers commonly pair the sync trait (for the fast path
//! when data is already ready) with the async trait (for the slow
//! path that yields to a host completion event). The WebCodecs
//! adapter is the canonical example — sync `receive_frame`
//! returns `NoFrameReady` when the queue is empty; async
//! `receive_frame` registers a waker and yields until the next
//! `output` callback fires.

pub mod local;
pub mod send;

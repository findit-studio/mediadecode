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
//! adapter is the canonical example — its `receive_frame` registers
//! a waker and yields until the next `output` callback fires, and
//! resolves to [`Received::NeedsInput`](crate::Received::NeedsInput)
//! rather than parking when nothing is in flight and only the caller
//! can supply more.
//!
//! # The answers are the same ones the sync faces give
//!
//! Awaiting changes *when* a call answers, not *which* answers
//! exist: every `send_packet` / `send_eof` here returns
//! [`Sent`](crate::Sent) and every `receive_frame` returns
//! [`Received`](crate::Received), exactly as their
//! [`crate::decoder`] counterparts do. An async face that hid
//! end-of-stream — or back pressure — in its error type would be a
//! second protocol for the same decoder.
//!
//! [`Sent::MustDrain`](crate::Sent::MustDrain) survives the move to
//! `async` for a concrete reason rather than for symmetry: it is the
//! one kind of pressure awaiting **cannot** resolve. Only the caller's
//! own `receive_frame` relieves it, and both methods hold `&mut self`,
//! so a `send_packet` that parked instead of answering would deadlock
//! the caller. Host-side pressure that the browser drains by itself is
//! awaited; pressure that needs the caller is reported.

pub mod local;
pub mod send;

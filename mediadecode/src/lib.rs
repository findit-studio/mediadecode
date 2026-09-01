#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, allow(unused_attributes))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![allow(clippy::type_complexity)]

// Workspace pattern (mirrors mediatime / colconv / scenesdetect) — alias
// `alloc` as `std` so `std::vec::Vec` etc. resolves in alloc-only builds.
// `unused_extern_crates` is suppressed because the public API currently
// uses only `core::` paths.
#[cfg(all(not(feature = "std"), feature = "alloc"))]
#[allow(unused_extern_crates)]
extern crate alloc as std;

#[cfg(feature = "std")]
extern crate std;

pub mod adapter;
pub mod cfa;
pub mod color;
pub mod decoder;
pub mod demuxer;
pub mod frame;
pub mod packet;
pub mod pixel_format;
pub mod resampler;
pub mod rhythm;
pub mod subtitle;

#[cfg(feature = "future")]
#[cfg_attr(docsrs, doc(cfg(feature = "future")))]
pub mod future;

// The framework faces this crate's own vocabulary types carry — impls
// and their reasoning, no items of its own. A module rather than a set
// of `#[cfg]`s scattered beside the types, because what a citizenship
// costs and what it deliberately leaves out is one argument and belongs
// in one place.
#[cfg(feature = "ingraph")]
#[cfg_attr(docsrs, doc(cfg(feature = "ingraph")))]
pub mod ingraph;

pub use pixel_format::PixelFormat;
pub use rhythm::{Received, Sent};

// Re-export the time primitives so consumers don't have to add a
// separate `mediatime` dependency.
pub use mediatime::{TimeRange, Timebase, Timestamp};

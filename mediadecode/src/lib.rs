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
pub mod frame;
pub mod packet;
pub mod pixel_format;
pub mod subtitle;

#[cfg(feature = "future")]
#[cfg_attr(docsrs, doc(cfg(feature = "future")))]
pub mod future;

pub use pixel_format::PixelFormat;

// Re-export the time primitives so consumers don't have to add a
// separate `mediatime` dependency.
pub use mediatime::{TimeRange, Timebase, Timestamp};

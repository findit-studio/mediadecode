#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

#[cfg(all(not(feature = "std"), feature = "alloc"))]
extern crate alloc as std;

#[cfg(feature = "std")]
extern crate std;

#[cfg(any(feature = "std", feature = "alloc"))]
mod clip;
#[cfg(any(feature = "std", feature = "alloc"))]
mod diff;
#[cfg(any(feature = "std", feature = "alloc"))]
mod layout;
#[cfg(any(feature = "std", feature = "alloc"))]
mod metadata;
#[cfg(any(feature = "std", feature = "alloc"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
pub mod otio;
#[cfg(any(feature = "std", feature = "alloc"))]
mod schema;
#[cfg(any(feature = "std", feature = "alloc"))]
mod time;
#[cfg(any(feature = "std", feature = "alloc"))]
mod timeline;
#[cfg(any(feature = "std", feature = "alloc"))]
mod transition;
#[cfg(any(feature = "std", feature = "alloc"))]
mod validate;
#[cfg(any(feature = "std", feature = "alloc"))]
mod wire;

#[cfg(any(feature = "std", feature = "alloc"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
pub use clip::{Clip, ClipId, Fade, FadeShape, Fades, Gain, MediaRef};
#[cfg(any(feature = "std", feature = "alloc"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
pub use diff::{Ambiguous, Change, ChangeKind, Delta, Side, diff};
#[cfg(any(feature = "std", feature = "alloc"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
pub use layout::{Item, Layout, TrackLayout, layout};
#[cfg(any(feature = "std", feature = "alloc"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
pub use metadata::Metadata;
#[cfg(any(feature = "std", feature = "alloc"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
pub use schema::Schema;
#[cfg(any(feature = "std", feature = "alloc"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
pub use timeline::{Timeline, Track, TrackKind};
#[cfg(any(feature = "std", feature = "alloc"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
pub use transition::{Transition, TransitionKind};
#[cfg(any(feature = "std", feature = "alloc"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
pub use validate::{
  ClipAt, ClipPair, ClipRange, Edge, EdgeAt, IdClash, Mismatch, Place, RangeAt, Refusal,
  TransitionAt, validate,
};

// The time words the model is written in, so a caller needs no `mediatime`
// dependency of its own.
pub use mediatime::{Duration, Rate, TimeRange, Timebase, Timestamp};

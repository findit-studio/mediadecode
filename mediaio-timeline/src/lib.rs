//! The editing timeline as data: tracks of clips at explicit record
//! positions, with gain, fades and dissolves.
#![cfg_attr(not(test), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

extern crate alloc;

mod clip;
mod diff;
mod layout;
mod metadata;
mod schema;
mod time;
mod timeline;
mod transition;
mod validate;

pub use clip::{Clip, Fade, FadeShape, Fades, Gain, MediaRef};
pub use diff::{Change, ChangeKind, Delta, diff};
pub use layout::{Item, Layout, TrackLayout, layout};
pub use metadata::Metadata;
pub use schema::Schema;
pub use timeline::{Timeline, Track, TrackKind};
pub use transition::{Transition, TransitionKind};
pub use validate::{
  ClipAt, ClipPair, Edge, EdgeAt, Mismatch, Place, Refusal, TransitionAt, validate,
};

// The time words the model is written in, so a caller needs no `mediatime`
// dependency of its own.
pub use mediatime::{Duration, Rate, TimeRange, Timebase, Timestamp};

//! Color metadata: re-exported from `mediaframe::color`.
//!
//! mediadecode used to define these enums locally (per ITU-T H.273);
//! they now live in the lowest-layer `mediaframe` crate so colconv,
//! mediadecode, and scenesdetect share a single canonical definition.
//!
//! Upstream renamed `Color{Matrix,Primaries,Transfer,Range,Info}` to
//! `{Matrix,Primaries,Transfer,DynamicRange,Info}` during the
//! `videoframe → mediaframe` rename; the disambiguated `Color*` names
//! are kept here as re-export aliases so mediadecode's public surface
//! and every downstream consumer (`mediadecode-ffmpeg`, …) stay stable.
pub use mediaframe::color::{
  ChromaLocation, DynamicRange as ColorRange, Info as ColorInfo, Matrix as ColorMatrix,
  Primaries as ColorPrimaries, Transfer as ColorTransfer,
};

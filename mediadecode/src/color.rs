//! Color metadata: re-exported from `videoframe::color`.
//!
//! mediadecode used to define these enums locally (per ITU-T H.273);
//! they now live in the lowest-layer `videoframe` crate so colconv,
//! mediadecode, and scenesdetect share a single canonical definition.
pub use videoframe::color::{
  ChromaLocation, ColorInfo, ColorMatrix, ColorPrimaries, ColorRange, ColorTransfer,
};

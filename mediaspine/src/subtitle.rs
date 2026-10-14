//! Decoded subtitle payload.
//!
//! Mirrors `AVSubtitle`'s text-or-bitmap split. `Text` works under
//! pure `core`; `Bitmap` requires the `alloc` feature because it
//! holds a `Vec<BitmapRegion>` (FFmpeg subtitles can carry many
//! rectangles per frame, so a fixed-size array is impractical).

use derive_more::{IsVariant, TryUnwrap, Unwrap};

#[cfg(any(feature = "std", feature = "alloc"))]
extern crate alloc;

/// One bitmap subtitle region (rectangle of paletted pixels).
///
/// Mirrors `AVSubtitleRect` for bitmap subtitles. `palette` and
/// `data` use the buffer type `B` so callers can pick the storage.
/// Plane stride and palette length are stored as `u32` for parity
/// with the rest of the crate's geometry conventions.
#[cfg(any(feature = "std", feature = "alloc"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
#[derive(Debug, Clone)]
pub struct BitmapRegion<B> {
  x: u32,
  y: u32,
  width: u32,
  height: u32,
  /// Bytes per row of `data`.
  stride: u32,
  /// Paletted pixel data; one byte per pixel, indices into `palette`.
  data: B,
  /// RGBA palette (4 bytes per entry).
  palette: B,
}

#[cfg(any(feature = "std", feature = "alloc"))]
impl<B> BitmapRegion<B> {
  /// Constructs a `BitmapRegion`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    stride: u32,
    data: B,
    palette: B,
  ) -> Self {
    Self {
      x,
      y,
      width,
      height,
      stride,
      data,
      palette,
    }
  }

  /// Returns the X coordinate of the region's top-left.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn x(&self) -> u32 {
    self.x
  }
  /// Returns the Y coordinate of the region's top-left.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn y(&self) -> u32 {
    self.y
  }
  /// Returns the region's width.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn width(&self) -> u32 {
    self.width
  }
  /// Returns the region's height.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn height(&self) -> u32 {
    self.height
  }
  /// Returns the stride in bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stride(&self) -> u32 {
    self.stride
  }
  /// Returns the paletted pixel data.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn data(&self) -> &B {
    &self.data
  }
  /// Returns the RGBA palette.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn palette(&self) -> &B {
    &self.palette
  }
}

/// Payload for [`SubtitlePayload::Text`].
#[derive(Debug, Clone)]
pub struct Text<B> {
  text: B,
  language: Option<[u8; 3]>,
}

impl<B> Text<B> {
  /// Constructs a `Text` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(text: B, language: Option<[u8; 3]>) -> Self {
    Self { text, language }
  }

  /// Returns the UTF-8 text payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn text(&self) -> &B {
    &self.text
  }
  /// Returns the ISO 639-2/T language tag, or `None` if unspecified.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn language(&self) -> Option<[u8; 3]> {
    self.language
  }
}

/// Payload for [`SubtitlePayload::Bitmap`].
#[cfg(any(feature = "std", feature = "alloc"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
#[derive(Debug, Clone)]
pub struct Bitmap<B> {
  regions: alloc::vec::Vec<BitmapRegion<B>>,
}

#[cfg(any(feature = "std", feature = "alloc"))]
impl<B> Bitmap<B> {
  /// Constructs a `Bitmap` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(regions: alloc::vec::Vec<BitmapRegion<B>>) -> Self {
    Self { regions }
  }

  /// Returns the bitmap's rectangles. FFmpeg subtitles often carry
  /// several.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn regions(&self) -> &[BitmapRegion<B>] {
    &self.regions
  }
}

/// Decoded subtitle payload — text or bitmap regions.
#[derive(Debug, Clone, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
pub enum SubtitlePayload<B> {
  /// Text subtitle (UTF-8 in `text`; ISO 639-2 language tag optional).
  Text(Text<B>),
  /// Bitmap subtitle — one or more rectangles of paletted pixels.
  /// Available only with the `alloc` feature.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
  Bitmap(Bitmap<B>),
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn text_payload_constructs() {
    let p: SubtitlePayload<&[u8]> = SubtitlePayload::Text(Text::new(b"hello", Some(*b"eng")));
    match p {
      SubtitlePayload::Text(payload) => {
        assert_eq!(payload.text(), b"hello");
        assert_eq!(payload.language(), Some(*b"eng"));
      }
      #[cfg(any(feature = "std", feature = "alloc"))]
      _ => panic!("unexpected variant"),
    }
  }

  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn bitmap_region_construction() {
    let data: &[u8] = &[0; 16];
    let pal: &[u8] = &[0; 16];
    let r = BitmapRegion::new(10, 20, 4, 4, 4, data, pal);
    assert_eq!(r.x(), 10);
    assert_eq!(r.width(), 4);
    assert_eq!(*r.data(), data);
  }

  #[cfg(any(feature = "std", feature = "alloc"))]
  #[test]
  fn bitmap_payload_constructs() {
    let data: &[u8] = &[0; 16];
    let pal: &[u8] = &[0; 16];
    let p: SubtitlePayload<&[u8]> =
      SubtitlePayload::Bitmap(Bitmap::new(alloc::vec![BitmapRegion::new(
        0, 0, 4, 4, 4, data, pal
      )]));
    if let SubtitlePayload::Bitmap(payload) = p {
      assert_eq!(payload.regions().len(), 1);
    } else {
      panic!("unexpected variant");
    }
  }
}

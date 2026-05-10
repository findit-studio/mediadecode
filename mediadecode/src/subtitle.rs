//! Decoded subtitle payload.
//!
//! Mirrors `AVSubtitle`'s text-or-bitmap split. `Text` works under
//! pure `core`; `Bitmap` requires the `alloc` feature because it
//! holds a `Vec<BitmapRegion>` (FFmpeg subtitles can carry many
//! rectangles per frame, so a fixed-size array is impractical).

#[cfg(any(feature = "std", feature = "alloc"))]
extern crate alloc;

use core::fmt::Debug;

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

/// Decoded subtitle payload — text or bitmap regions.
pub enum SubtitlePayload<B> {
  /// Text subtitle (UTF-8 in `text`; ISO 639-2 language tag optional).
  Text {
    /// UTF-8 text payload.
    text: B,
    /// ISO 639-2/T language tag, or `None` if unspecified.
    language: Option<[u8; 3]>,
  },
  /// Bitmap subtitle — one or more rectangles of paletted pixels.
  /// Available only with the `alloc` feature.
  #[cfg(any(feature = "std", feature = "alloc"))]
  #[cfg_attr(docsrs, doc(cfg(any(feature = "std", feature = "alloc"))))]
  Bitmap {
    /// One or more rectangles. FFmpeg subtitles often carry several.
    regions: alloc::vec::Vec<BitmapRegion<B>>,
  },
}

impl<B: Debug> Debug for SubtitlePayload<B> {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::Text { text, language } => f
        .debug_struct("SubtitlePayload::Text")
        .field("text", text)
        .field("language", language)
        .finish(),
      #[cfg(any(feature = "std", feature = "alloc"))]
      Self::Bitmap { regions } => f
        .debug_struct("SubtitlePayload::Bitmap")
        .field("regions", &regions.len())
        .finish(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn text_payload_constructs() {
    let p: SubtitlePayload<&[u8]> = SubtitlePayload::Text {
      text: b"hello",
      language: Some(*b"eng"),
    };
    match p {
      SubtitlePayload::Text { text, language } => {
        assert_eq!(text, b"hello");
        assert_eq!(language, Some(*b"eng"));
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
    let p: SubtitlePayload<&[u8]> = SubtitlePayload::Bitmap {
      regions: alloc::vec![BitmapRegion::new(0, 0, 4, 4, 4, data, pal)],
    };
    if let SubtitlePayload::Bitmap { regions } = p {
      assert_eq!(regions.len(), 1);
    } else {
      panic!("unexpected variant");
    }
  }
}

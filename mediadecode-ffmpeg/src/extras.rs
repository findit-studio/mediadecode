//! Backend-specific `*Extra` carriers used as the
//! `mediadecode::*Adapter::*Extra` associated types.
//!
//! These are plain data records (public fields) since they're carried
//! verbatim through the abstraction layer and most consumers will read
//! fields directly. The structs are kept small and `Default`-able so
//! callers who don't care can pass `Default::default()`.

use std::vec::Vec;

/// Per-`VideoPacket` extras.
#[derive(Clone, Debug, Default)]
pub struct VideoPacketExtra {
  /// `AVStream.index` of the source stream.
  pub stream_index: i32,
  /// Byte position of the packet in the input file, or `None` if unknown.
  pub byte_pos: Option<i64>,
  /// Raw side-data entries from `AVPacket.side_data` (type id + bytes).
  pub side_data: Vec<SideDataEntry>,
}

/// Per-`VideoFrame` extras carrying everything the unified
/// `mediadecode::ColorInfo` doesn't already cover.
#[derive(Clone, Debug, Default)]
pub struct VideoFrameExtra {
  /// Sample aspect ratio (par numerator / denominator), `None` if 1:1
  /// or unspecified.
  pub sample_aspect_ratio: Option<(u32, u32)>,
  /// Frame picture type (I/P/B/etc.).
  pub picture_type: PictureType,
  /// `True` if this frame is a key frame.
  pub key_frame: bool,
  /// `True` if the frame is interlaced.
  pub interlaced: bool,
  /// `True` if the top field is first (only meaningful with `interlaced`).
  pub top_field_first: bool,
  /// FFmpeg's heuristic best-effort PTS, or `None` if unknown.
  pub best_effort_timestamp: Option<i64>,
  /// HDR10 mastering-display metadata (`AV_FRAME_DATA_MASTERING_DISPLAY_METADATA`),
  /// if present on the source frame.
  pub mastering_display: Option<MasteringDisplay>,
  /// HDR10 content-light-level (`AV_FRAME_DATA_CONTENT_LIGHT_LEVEL`).
  pub content_light_level: Option<ContentLightLevel>,
  /// SMPTE ST 12-M timecode (`AV_FRAME_DATA_S12M_TIMECODE`) as raw
  /// 32-bit BCD-packed values, up to 3 timecodes.
  pub smpte_timecode: Vec<u32>,
  /// Raw side-data entries from `AVFrame.side_data` (type id + bytes).
  /// Entries that have first-class fields above are still mirrored here
  /// for callers that want the unparsed buffer.
  pub side_data: Vec<SideDataEntry>,
}

/// Per-`AudioPacket` extras.
#[derive(Clone, Debug, Default)]
pub struct AudioPacketExtra {
  /// `AVStream.index` of the source stream.
  pub stream_index: i32,
  /// Byte position of the packet in the input file, or `None` if unknown.
  pub byte_pos: Option<i64>,
  /// Raw side-data entries.
  pub side_data: Vec<SideDataEntry>,
}

/// Per-`AudioFrame` extras.
#[derive(Clone, Debug, Default)]
pub struct AudioFrameExtra {
  /// FFmpeg's heuristic best-effort PTS, or `None` if unknown.
  pub best_effort_timestamp: Option<i64>,
  /// Raw side-data entries.
  pub side_data: Vec<SideDataEntry>,
}

/// Per-`SubtitlePacket` extras.
#[derive(Clone, Debug, Default)]
pub struct SubtitlePacketExtra {
  /// `AVStream.index` of the source stream.
  pub stream_index: i32,
  /// ISO 639-2/T language tag (3 bytes), if known.
  pub language: Option<[u8; 3]>,
  /// `True` if this subtitle stream is marked "forced" (per MOV / MKV
  /// metadata).
  pub forced: bool,
}

/// Per-`SubtitleFrame` extras.
#[derive(Clone, Debug, Default)]
pub struct SubtitleFrameExtra {
  /// `AVSubtitle.start_display_time` — milliseconds from `pts`.
  pub start_display_time: u32,
  /// `AVSubtitle.end_display_time` — milliseconds from `pts`.
  pub end_display_time: u32,
}

/// Picture type per `AVFrame.pict_type`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum PictureType {
  /// Unspecified / unset.
  #[default]
  Unspecified,
  /// Intra (I-frame).
  I,
  /// Predicted (P-frame).
  P,
  /// Bi-directional predicted (B-frame).
  B,
  /// S(GMC)-VOP from MPEG-4.
  S,
  /// Switching Intra (H.264).
  Si,
  /// Switching Predicted (H.264).
  Sp,
  /// Bi-predicted intra (BI-frame).
  Bi,
}

/// Raw side-data entry carrying the FFmpeg type id and the unparsed
/// byte buffer. Type ids correspond to FFmpeg's
/// `AV_FRAME_DATA_*` / `AV_PKT_DATA_*` constants — see
/// `libavutil/frame.h` and `libavcodec/packet.h`.
#[derive(Clone, Debug)]
pub struct SideDataEntry {
  /// FFmpeg side-data type id (`AVFrameSideDataType` /
  /// `AVPacketSideDataType` raw integer).
  pub kind: i32,
  /// Side-data payload as raw bytes.
  pub data: Vec<u8>,
}

/// HDR10 mastering display metadata.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MasteringDisplay {
  /// Display primary chromaticities `(x, y)` for R, G, B in CIE 1931
  /// (each as `(num, den)` rational, with `den` non-zero).
  pub display_primaries: [(u32, u32); 3 /* R, G, B */],
  /// White-point chromaticity `(x, y)` as rationals.
  pub white_point: (u32, u32),
  /// Maximum luminance in `0.0001 cd/m²` units (rational `(num, den)`).
  pub max_luminance: (u32, u32),
  /// Minimum luminance in `0.0001 cd/m²` units.
  pub min_luminance: (u32, u32),
}

/// HDR10 content light level (`AV_FRAME_DATA_CONTENT_LIGHT_LEVEL`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct ContentLightLevel {
  /// Maximum content light level (cd/m²).
  pub max_cll: u32,
  /// Maximum frame-average light level (cd/m²).
  pub max_fall: u32,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn defaults_construct() {
    let v = VideoPacketExtra::default();
    assert_eq!(v.stream_index, 0);
    assert!(v.side_data.is_empty());

    let f = VideoFrameExtra::default();
    assert_eq!(f.picture_type, PictureType::Unspecified);
    assert!(!f.key_frame);
    assert!(f.mastering_display.is_none());

    let s = SubtitleFrameExtra::default();
    assert_eq!(s.start_display_time, 0);
    assert_eq!(s.end_display_time, 0);
  }

  #[test]
  fn picture_type_default_is_unspecified() {
    assert_eq!(PictureType::default(), PictureType::Unspecified);
  }

  #[test]
  fn side_data_entry_carries_bytes() {
    let entry = SideDataEntry {
      kind: 12345,
      data: vec![1, 2, 3, 4],
    };
    assert_eq!(entry.kind, 12345);
    assert_eq!(entry.data, vec![1, 2, 3, 4]);
  }

  #[test]
  fn content_light_level_default_is_zero() {
    let cll = ContentLightLevel::default();
    assert_eq!(cll.max_cll, 0);
    assert_eq!(cll.max_fall, 0);
  }
}

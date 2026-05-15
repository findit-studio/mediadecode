//! Backend-specific `*Extra` carriers used as the
//! `mediadecode::*Adapter::*Extra` associated types.
//!
//! Fields are private; values are read through getters and set through
//! `with_*` (consuming builders) / `set_*` (in-place mutators) — the
//! crate-wide encapsulation convention. `const fn` is used wherever
//! the field type permits (i.e. anything but `Vec`).

use std::vec::Vec;

/// Per-`VideoPacket` extras.
#[derive(Clone, Debug, Default)]
pub struct VideoPacketExtra {
  stream_index: i32,
  byte_pos: Option<i64>,
  side_data: Vec<SideDataEntry>,
}

impl VideoPacketExtra {
  /// Constructs a `VideoPacketExtra` with the given stream index.
  /// `byte_pos` defaults to `None` and `side_data` to empty.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: i32) -> Self {
    Self {
      stream_index,
      byte_pos: None,
      side_data: Vec::new(),
    }
  }

  /// Returns the source `AVStream.index`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> i32 {
    self.stream_index
  }

  /// Returns the byte position of the packet in the input file, or
  /// `None` if unknown.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn byte_pos(&self) -> Option<i64> {
    self.byte_pos
  }

  /// Returns the raw side-data entries from `AVPacket.side_data`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data(&self) -> &[SideDataEntry] {
    self.side_data.as_slice()
  }

  /// Sets the stream index (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_stream_index(mut self, value: i32) -> Self {
    self.stream_index = value;
    self
  }
  /// Sets the byte position (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_byte_pos(mut self, value: Option<i64>) -> Self {
    self.byte_pos = value;
    self
  }
  /// Sets the side-data list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_side_data(mut self, value: Vec<SideDataEntry>) -> Self {
    self.side_data = value;
    self
  }

  /// Sets the stream index in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_stream_index(&mut self, value: i32) -> &mut Self {
    self.stream_index = value;
    self
  }
  /// Sets the byte position in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_byte_pos(&mut self, value: Option<i64>) -> &mut Self {
    self.byte_pos = value;
    self
  }
  /// Sets the side-data list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_side_data(&mut self, value: Vec<SideDataEntry>) -> &mut Self {
    self.side_data = value;
    self
  }
}

/// Per-`VideoFrame` extras carrying everything the unified
/// `mediadecode::ColorInfo` doesn't already cover.
#[derive(Clone, Debug, Default)]
pub struct VideoFrameExtra {
  sample_aspect_ratio: Option<(u32, u32)>,
  picture_type: PictureType,
  key_frame: bool,
  interlaced: bool,
  top_field_first: bool,
  best_effort_timestamp: Option<i64>,
  mastering_display: Option<MasteringDisplay>,
  content_light_level: Option<ContentLightLevel>,
  smpte_timecode: Vec<u32>,
  side_data: Vec<SideDataEntry>,
}

impl VideoFrameExtra {
  /// Constructs an empty `VideoFrameExtra` (all fields at default).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      sample_aspect_ratio: None,
      picture_type: PictureType::Unspecified,
      key_frame: false,
      interlaced: false,
      top_field_first: false,
      best_effort_timestamp: None,
      mastering_display: None,
      content_light_level: None,
      smpte_timecode: Vec::new(),
      side_data: Vec::new(),
    }
  }

  /// Sample aspect ratio (par numerator / denominator), `None` if 1:1
  /// or unspecified.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn sample_aspect_ratio(&self) -> Option<(u32, u32)> {
    self.sample_aspect_ratio
  }
  /// Frame picture type (I/P/B/etc.).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn picture_type(&self) -> PictureType {
    self.picture_type
  }
  /// `True` if this frame is a key frame.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn key_frame(&self) -> bool {
    self.key_frame
  }
  /// `True` if the frame is interlaced.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn interlaced(&self) -> bool {
    self.interlaced
  }
  /// `True` if the top field is first (only meaningful with `interlaced`).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn top_field_first(&self) -> bool {
    self.top_field_first
  }
  /// FFmpeg's heuristic best-effort PTS, or `None` if unknown.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn best_effort_timestamp(&self) -> Option<i64> {
    self.best_effort_timestamp
  }
  /// HDR10 mastering-display metadata, if present on the source frame.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn mastering_display(&self) -> Option<MasteringDisplay> {
    self.mastering_display
  }
  /// HDR10 content-light-level.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn content_light_level(&self) -> Option<ContentLightLevel> {
    self.content_light_level
  }
  /// SMPTE ST 12-M timecode entries (raw 32-bit BCD-packed values).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn smpte_timecode(&self) -> &[u32] {
    self.smpte_timecode.as_slice()
  }
  /// Raw side-data entries from `AVFrame.side_data`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data(&self) -> &[SideDataEntry] {
    self.side_data.as_slice()
  }

  /// Sets the sample aspect ratio (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_sample_aspect_ratio(mut self, value: Option<(u32, u32)>) -> Self {
    self.sample_aspect_ratio = value;
    self
  }
  /// Sets the picture type (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_picture_type(mut self, value: PictureType) -> Self {
    self.picture_type = value;
    self
  }
  /// Sets the key-frame flag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_key_frame(mut self, value: bool) -> Self {
    self.key_frame = value;
    self
  }
  /// Sets the interlaced flag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_interlaced(mut self, value: bool) -> Self {
    self.interlaced = value;
    self
  }
  /// Sets the top-field-first flag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_top_field_first(mut self, value: bool) -> Self {
    self.top_field_first = value;
    self
  }
  /// Sets the best-effort timestamp (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_best_effort_timestamp(mut self, value: Option<i64>) -> Self {
    self.best_effort_timestamp = value;
    self
  }
  /// Sets the mastering-display metadata (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_mastering_display(mut self, value: Option<MasteringDisplay>) -> Self {
    self.mastering_display = value;
    self
  }
  /// Sets the content-light-level metadata (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_content_light_level(mut self, value: Option<ContentLightLevel>) -> Self {
    self.content_light_level = value;
    self
  }
  /// Sets the SMPTE timecode list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_smpte_timecode(mut self, value: Vec<u32>) -> Self {
    self.smpte_timecode = value;
    self
  }
  /// Sets the side-data list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_side_data(mut self, value: Vec<SideDataEntry>) -> Self {
    self.side_data = value;
    self
  }

  /// Sets the sample aspect ratio in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_sample_aspect_ratio(&mut self, value: Option<(u32, u32)>) -> &mut Self {
    self.sample_aspect_ratio = value;
    self
  }
  /// Sets the picture type in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_picture_type(&mut self, value: PictureType) -> &mut Self {
    self.picture_type = value;
    self
  }
  /// Sets the key-frame flag in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_key_frame(&mut self, value: bool) -> &mut Self {
    self.key_frame = value;
    self
  }
  /// Sets the interlaced flag in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_interlaced(&mut self, value: bool) -> &mut Self {
    self.interlaced = value;
    self
  }
  /// Sets the top-field-first flag in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_top_field_first(&mut self, value: bool) -> &mut Self {
    self.top_field_first = value;
    self
  }
  /// Sets the best-effort timestamp in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_best_effort_timestamp(&mut self, value: Option<i64>) -> &mut Self {
    self.best_effort_timestamp = value;
    self
  }
  /// Sets the mastering-display metadata in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_mastering_display(&mut self, value: Option<MasteringDisplay>) -> &mut Self {
    self.mastering_display = value;
    self
  }
  /// Sets the content-light-level metadata in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_content_light_level(&mut self, value: Option<ContentLightLevel>) -> &mut Self {
    self.content_light_level = value;
    self
  }
  /// Sets the SMPTE timecode list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_smpte_timecode(&mut self, value: Vec<u32>) -> &mut Self {
    self.smpte_timecode = value;
    self
  }
  /// Sets the side-data list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_side_data(&mut self, value: Vec<SideDataEntry>) -> &mut Self {
    self.side_data = value;
    self
  }
}

/// Per-`AudioPacket` extras.
#[derive(Clone, Debug, Default)]
pub struct AudioPacketExtra {
  stream_index: i32,
  byte_pos: Option<i64>,
  side_data: Vec<SideDataEntry>,
}

impl AudioPacketExtra {
  /// Constructs an `AudioPacketExtra` with the given stream index.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: i32) -> Self {
    Self {
      stream_index,
      byte_pos: None,
      side_data: Vec::new(),
    }
  }

  /// Returns the source `AVStream.index`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> i32 {
    self.stream_index
  }
  /// Returns the byte position, or `None` if unknown.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn byte_pos(&self) -> Option<i64> {
    self.byte_pos
  }
  /// Returns the raw side-data entries.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data(&self) -> &[SideDataEntry] {
    self.side_data.as_slice()
  }

  /// Sets the stream index (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_stream_index(mut self, value: i32) -> Self {
    self.stream_index = value;
    self
  }
  /// Sets the byte position (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_byte_pos(mut self, value: Option<i64>) -> Self {
    self.byte_pos = value;
    self
  }
  /// Sets the side-data list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_side_data(mut self, value: Vec<SideDataEntry>) -> Self {
    self.side_data = value;
    self
  }

  /// Sets the stream index in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_stream_index(&mut self, value: i32) -> &mut Self {
    self.stream_index = value;
    self
  }
  /// Sets the byte position in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_byte_pos(&mut self, value: Option<i64>) -> &mut Self {
    self.byte_pos = value;
    self
  }
  /// Sets the side-data list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_side_data(&mut self, value: Vec<SideDataEntry>) -> &mut Self {
    self.side_data = value;
    self
  }
}

/// Per-`AudioFrame` extras.
#[derive(Clone, Debug, Default)]
pub struct AudioFrameExtra {
  best_effort_timestamp: Option<i64>,
  side_data: Vec<SideDataEntry>,
}

impl AudioFrameExtra {
  /// Constructs an empty `AudioFrameExtra`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new() -> Self {
    Self {
      best_effort_timestamp: None,
      side_data: Vec::new(),
    }
  }

  /// FFmpeg's heuristic best-effort PTS, or `None` if unknown.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn best_effort_timestamp(&self) -> Option<i64> {
    self.best_effort_timestamp
  }
  /// Returns the raw side-data entries.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn side_data(&self) -> &[SideDataEntry] {
    self.side_data.as_slice()
  }

  /// Sets the best-effort timestamp (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_best_effort_timestamp(mut self, value: Option<i64>) -> Self {
    self.best_effort_timestamp = value;
    self
  }
  /// Sets the side-data list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_side_data(mut self, value: Vec<SideDataEntry>) -> Self {
    self.side_data = value;
    self
  }

  /// Sets the best-effort timestamp in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_best_effort_timestamp(&mut self, value: Option<i64>) -> &mut Self {
    self.best_effort_timestamp = value;
    self
  }
  /// Sets the side-data list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_side_data(&mut self, value: Vec<SideDataEntry>) -> &mut Self {
    self.side_data = value;
    self
  }
}

/// Per-`SubtitlePacket` extras.
#[derive(Clone, Debug, Default)]
pub struct SubtitlePacketExtra {
  stream_index: i32,
  language: Option<[u8; 3]>,
  forced: bool,
}

impl SubtitlePacketExtra {
  /// Constructs a `SubtitlePacketExtra` with the given stream index.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: i32) -> Self {
    Self {
      stream_index,
      language: None,
      forced: false,
    }
  }

  /// Returns the source `AVStream.index`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> i32 {
    self.stream_index
  }
  /// Returns the ISO 639-2/T language tag, or `None` if unspecified.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn language(&self) -> Option<[u8; 3]> {
    self.language
  }
  /// Returns whether this subtitle stream is marked "forced".
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn forced(&self) -> bool {
    self.forced
  }

  /// Sets the stream index (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_stream_index(mut self, value: i32) -> Self {
    self.stream_index = value;
    self
  }
  /// Sets the language tag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_language(mut self, value: Option<[u8; 3]>) -> Self {
    self.language = value;
    self
  }
  /// Sets the forced flag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_forced(mut self, value: bool) -> Self {
    self.forced = value;
    self
  }

  /// Sets the stream index in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_stream_index(&mut self, value: i32) -> &mut Self {
    self.stream_index = value;
    self
  }
  /// Sets the language tag in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_language(&mut self, value: Option<[u8; 3]>) -> &mut Self {
    self.language = value;
    self
  }
  /// Sets the forced flag in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_forced(&mut self, value: bool) -> &mut Self {
    self.forced = value;
    self
  }
}

/// Per-`SubtitleFrame` extras.
#[derive(Clone, Debug, Default)]
pub struct SubtitleFrameExtra {
  start_display_time: u32,
  end_display_time: u32,
}

impl SubtitleFrameExtra {
  /// Constructs a `SubtitleFrameExtra`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(start_display_time: u32, end_display_time: u32) -> Self {
    Self {
      start_display_time,
      end_display_time,
    }
  }

  /// `AVSubtitle.start_display_time` — milliseconds from `pts`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn start_display_time(&self) -> u32 {
    self.start_display_time
  }
  /// `AVSubtitle.end_display_time` — milliseconds from `pts`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn end_display_time(&self) -> u32 {
    self.end_display_time
  }

  /// Sets the start display time (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_start_display_time(mut self, value: u32) -> Self {
    self.start_display_time = value;
    self
  }
  /// Sets the end display time (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_end_display_time(mut self, value: u32) -> Self {
    self.end_display_time = value;
    self
  }

  /// Sets the start display time in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_start_display_time(&mut self, value: u32) -> &mut Self {
    self.start_display_time = value;
    self
  }
  /// Sets the end display time in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_end_display_time(&mut self, value: u32) -> &mut Self {
    self.end_display_time = value;
    self
  }
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
  kind: i32,
  data: Vec<u8>,
}

impl SideDataEntry {
  /// Constructs a `SideDataEntry`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(kind: i32, data: Vec<u8>) -> Self {
    Self { kind, data }
  }

  /// FFmpeg side-data type id.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> i32 {
    self.kind
  }
  /// Side-data payload as raw bytes.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn data(&self) -> &[u8] {
    self.data.as_slice()
  }

  /// Sets the type id (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_kind(mut self, value: i32) -> Self {
    self.kind = value;
    self
  }
  /// Sets the payload (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_data(mut self, value: Vec<u8>) -> Self {
    self.data = value;
    self
  }

  /// Sets the type id in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_kind(&mut self, value: i32) -> &mut Self {
    self.kind = value;
    self
  }
  /// Sets the payload in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_data(&mut self, value: Vec<u8>) -> &mut Self {
    self.data = value;
    self
  }
}

/// HDR10 mastering display metadata.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MasteringDisplay {
  display_primaries: [(u32, u32); 3],
  white_point: (u32, u32),
  max_luminance: (u32, u32),
  min_luminance: (u32, u32),
}

impl MasteringDisplay {
  /// Constructs a `MasteringDisplay`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(
    display_primaries: [(u32, u32); 3],
    white_point: (u32, u32),
    max_luminance: (u32, u32),
    min_luminance: (u32, u32),
  ) -> Self {
    Self {
      display_primaries,
      white_point,
      max_luminance,
      min_luminance,
    }
  }

  /// Display primary chromaticities `(x, y)` for R, G, B in CIE 1931
  /// (each as `(num, den)` rational, with `den` non-zero).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn display_primaries(&self) -> [(u32, u32); 3] {
    self.display_primaries
  }
  /// White-point chromaticity `(x, y)` as rationals.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn white_point(&self) -> (u32, u32) {
    self.white_point
  }
  /// Maximum luminance in `0.0001 cd/m²` units (rational `(num, den)`).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_luminance(&self) -> (u32, u32) {
    self.max_luminance
  }
  /// Minimum luminance in `0.0001 cd/m²` units.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn min_luminance(&self) -> (u32, u32) {
    self.min_luminance
  }

  /// Sets the display primaries (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_display_primaries(mut self, value: [(u32, u32); 3]) -> Self {
    self.display_primaries = value;
    self
  }
  /// Sets the white point (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_white_point(mut self, value: (u32, u32)) -> Self {
    self.white_point = value;
    self
  }
  /// Sets the max luminance (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_max_luminance(mut self, value: (u32, u32)) -> Self {
    self.max_luminance = value;
    self
  }
  /// Sets the min luminance (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn with_min_luminance(mut self, value: (u32, u32)) -> Self {
    self.min_luminance = value;
    self
  }

  /// Sets the display primaries in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_display_primaries(&mut self, value: [(u32, u32); 3]) -> &mut Self {
    self.display_primaries = value;
    self
  }
  /// Sets the white point in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_white_point(&mut self, value: (u32, u32)) -> &mut Self {
    self.white_point = value;
    self
  }
  /// Sets the max luminance in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_luminance(&mut self, value: (u32, u32)) -> &mut Self {
    self.max_luminance = value;
    self
  }
  /// Sets the min luminance in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_min_luminance(&mut self, value: (u32, u32)) -> &mut Self {
    self.min_luminance = value;
    self
  }
}

/// HDR10 content light level (`AV_FRAME_DATA_CONTENT_LIGHT_LEVEL`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct ContentLightLevel {
  max_cll: u32,
  max_fall: u32,
}

impl ContentLightLevel {
  /// Constructs a `ContentLightLevel`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(max_cll: u32, max_fall: u32) -> Self {
    Self { max_cll, max_fall }
  }

  /// Maximum content light level (cd/m²).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_cll(&self) -> u32 {
    self.max_cll
  }
  /// Maximum frame-average light level (cd/m²).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn max_fall(&self) -> u32 {
    self.max_fall
  }

  /// Sets `max_cll` (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_cll(mut self, value: u32) -> Self {
    self.max_cll = value;
    self
  }
  /// Sets `max_fall` (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_max_fall(mut self, value: u32) -> Self {
    self.max_fall = value;
    self
  }

  /// Sets `max_cll` in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_cll(&mut self, value: u32) -> &mut Self {
    self.max_cll = value;
    self
  }
  /// Sets `max_fall` in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_max_fall(&mut self, value: u32) -> &mut Self {
    self.max_fall = value;
    self
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn defaults_construct() {
    let v = VideoPacketExtra::default();
    assert_eq!(v.stream_index(), 0);
    assert!(v.side_data().is_empty());

    let f = VideoFrameExtra::default();
    assert_eq!(f.picture_type(), PictureType::Unspecified);
    assert!(!f.key_frame());
    assert!(f.mastering_display().is_none());

    let s = SubtitleFrameExtra::default();
    assert_eq!(s.start_display_time(), 0);
    assert_eq!(s.end_display_time(), 0);
  }

  #[test]
  fn picture_type_default_is_unspecified() {
    assert_eq!(PictureType::default(), PictureType::Unspecified);
  }

  #[test]
  fn side_data_entry_carries_bytes() {
    let entry = SideDataEntry::new(12345, vec![1, 2, 3, 4]);
    assert_eq!(entry.kind(), 12345);
    assert_eq!(entry.data(), &[1, 2, 3, 4]);
  }

  #[test]
  fn content_light_level_default_is_zero() {
    let cll = ContentLightLevel::default();
    assert_eq!(cll.max_cll(), 0);
    assert_eq!(cll.max_fall(), 0);
  }

  #[test]
  fn builders_chain() {
    let v = VideoPacketExtra::new(7)
      .with_byte_pos(Some(1234))
      .with_side_data(vec![SideDataEntry::new(1, vec![0xAB])]);
    assert_eq!(v.stream_index(), 7);
    assert_eq!(v.byte_pos(), Some(1234));
    assert_eq!(v.side_data().len(), 1);
  }

  #[test]
  fn setters_chain() {
    let mut v = VideoPacketExtra::default();
    v.set_stream_index(3).set_byte_pos(Some(99));
    assert_eq!(v.stream_index(), 3);
    assert_eq!(v.byte_pos(), Some(99));
  }
}

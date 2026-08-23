//! Backend-specific `*Extra` carriers used as the
//! `mediadecode::*Adapter::*Extra` associated types.
//!
//! Fields are private; values are read through getters and set through
//! `with_*` (consuming builders) / `set_*` (in-place mutators) — the
//! crate-wide encapsulation convention. `const fn` is used wherever
//! the field type permits (i.e. anything but `Vec`).

use std::vec::Vec;

use ffmpeg_next::{codec::Parameters, ffi::avcodec_parameters_copy};

use crate::demuxer::DemuxError;

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
  side_data: Vec<SideDataEntry>,
}

impl SubtitlePacketExtra {
  /// Constructs a `SubtitlePacketExtra` with the given stream index.
  /// `side_data` defaults to empty.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: i32) -> Self {
    Self {
      stream_index,
      language: None,
      forced: false,
      side_data: Vec::new(),
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
  /// Returns the raw side-data entries from `AVPacket.side_data`.
  ///
  /// A subtitle packet's side data is rare but not absent — and a
  /// packet that carries *nothing else* is exactly the case this seat
  /// exists for: with no seat, a side-data-only packet has nowhere to
  /// put its only content.
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
  /// Sets the language tag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_language(mut self, value: Option<[u8; 3]>) -> Self {
    self.language = value;
    self
  }
  /// Sets the side-data list (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub fn with_side_data(mut self, value: Vec<SideDataEntry>) -> Self {
    self.side_data = value;
    self
  }
  /// Sets the side-data list in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub fn set_side_data(&mut self, value: Vec<SideDataEntry>) -> &mut Self {
    self.side_data = value;
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

// ---------------------------------------------------------------------------
//  The demux tier's carriers.
// ---------------------------------------------------------------------------

/// Per-`DataPacket` extras — timecode, KLV, timed ID3.
///
/// The same three seats as [`VideoPacketExtra`]. The side-data list was
/// left off at first — data demuxers carry their whole payload in the
/// packet body — and then earned its place: a packet with no body and
/// only side data is a real packet, and without this seat its only
/// content would have nowhere to go.
#[derive(Clone, Debug, Default)]
pub struct DataPacketExtra {
  stream_index: i32,
  byte_pos: Option<i64>,
  side_data: Vec<SideDataEntry>,
}

impl DataPacketExtra {
  /// Constructs a `DataPacketExtra` with the given stream index.
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

/// Per-`AttachmentPacket` extras — fonts, cover art.
///
/// `synthesized` records where the payload came from, which is not a
/// detail: an attachment track's single packet is either a real packet
/// the container stores (cover art, which libavformat parks in
/// `AVStream.attached_pic`) or one this crate builds out of the
/// track's codec extradata (fonts, whose bytes never appear in the
/// packet stream at all). A consumer chasing a payload that looks
/// wrong needs to know which.
#[derive(Clone, Debug, Default)]
pub struct AttachmentPacketExtra {
  stream_index: i32,
  synthesized: bool,
}

impl AttachmentPacketExtra {
  /// Constructs an `AttachmentPacketExtra` with the given stream index.
  /// `synthesized` defaults to `false`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: i32) -> Self {
    Self {
      stream_index,
      synthesized: false,
    }
  }

  /// Returns the source `AVStream.index`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> i32 {
    self.stream_index
  }
  /// `true` when the payload was built from the track's codec
  /// extradata rather than taken from a packet the container stores.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn synthesized(&self) -> bool {
    self.synthesized
  }

  /// Sets the stream index (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_stream_index(mut self, value: i32) -> Self {
    self.stream_index = value;
    self
  }
  /// Sets the synthesized flag (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_synthesized(mut self, value: bool) -> Self {
    self.synthesized = value;
    self
  }

  /// Sets the stream index in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_stream_index(&mut self, value: i32) -> &mut Self {
    self.stream_index = value;
    self
  }
  /// Sets the synthesized flag in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_synthesized(&mut self, value: bool) -> &mut Self {
    self.synthesized = value;
    self
  }
}

/// A deep copy of codec parameters, with both fallible steps checked.
///
/// `ffmpeg_next`'s `Clone` for `Parameters` checks neither.
/// `Parameters::new` does not test `avcodec_parameters_alloc` for null
/// and the copy dereferences the result immediately — measured under a
/// capped allocator, that is a SIGSEGV — while
/// `avcodec_parameters_copy`'s return value is discarded, so a copy
/// that failed part way yields parameters that look complete and open a
/// decoder wrong.
///
/// A partial copy leaves with `out`'s own destructor:
/// `avcodec_parameters_copy` resets the destination before it starts,
/// so whatever it managed to allocate belongs to `out`.
pub(crate) fn clone_parameters(
  source: &Parameters,
  stream_index: usize,
) -> Result<Parameters, DemuxError> {
  // The *source* first. `Parameters::new()` and `Parameters::default()`
  // hand back a value whose pointer is null when
  // `avcodec_parameters_alloc` failed — safe code, no error, no way to
  // tell — and `avcodec_parameters_copy` dereferences its source. So a
  // copier that checks only what it allocates still crashes, one
  // allocator recovery later, on a `Parameters` that never allocated.
  // SAFETY: reading the pointer without dereferencing it.
  if unsafe { source.as_ptr() }.is_null() {
    return Err(DemuxError::ParametersMissing { stream_index });
  }
  let mut out = Parameters::new();
  // SAFETY: reading the pointer the constructor stored without
  // dereferencing it — which is exactly what the check is for.
  if unsafe { out.as_ptr() }.is_null() {
    return Err(DemuxError::ParametersAlloc { stream_index });
  }
  // SAFETY: both pointers are live `AVCodecParameters` — the
  // destination freshly allocated and non-null, the source owned by its
  // holder for the duration of this call.
  let rc = unsafe { avcodec_parameters_copy(out.as_mut_ptr(), source.as_ptr()) };
  if rc < 0 {
    return Err(DemuxError::ParametersCopy {
      stream_index,
      source: ffmpeg_next::Error::from(rc),
    });
  }
  Ok(out)
}

/// Per-`TrackInfo` extras — the FFmpeg side of one track-table row.
///
/// Carries the stream's [`Parameters`], which is what opens a decoder
/// for the track — through [`Self::clone_parameters`], which is a deep
/// `avcodec_parameters_copy` with no tie back to the format context, so
/// a decoder outlives the demuxer that named it.
///
/// **`Clone` is hand-written, not derived, and there is still no
/// `Default`.** A derived `Clone` would go through `ffmpeg_next`'s own
/// `Clone` for [`Parameters`], which checks neither the allocation nor
/// the copy: safe public code could dereference a null destination or
/// receive parameters that are quietly incomplete. This crate shipped
/// exactly that bug once — `TrackExtra` derived `Clone` over the
/// unchecked path, reachable from safe code that just copied a track
/// row — and closed it by removing the derive (see
/// `demuxer::tests::the_public_track_extra_copies_are_checked_too`).
///
/// `Clone::clone` here instead calls [`Self::try_clone`] — the checked
/// copy — and panics on its `Err`. That `Err` is reachable only as an
/// allocation failure in `avcodec_parameters_alloc` or a negative
/// `avcodec_parameters_copy` return, both allocator-exhaustion cases;
/// never from a malformed source, since a `TrackExtra` cannot exist
/// over null-backed parameters (see [`Self::new`]). Panicking there is
/// the same trade every allocation-backed `Clone` in `std` already
/// makes (`Vec`, `Box`, `String`, …) — a controlled panic on
/// exhaustion, not the silent corruption or null dereference the
/// derive reached. A caller that must not unwind on that failure calls
/// [`Self::try_clone`] directly; [`Self::clone_parameters`] is the
/// handoff a decoder actually needs.
///
/// `Default` stays absent: there is no checked substitute for
/// `Parameters::default()` to route through, and an empty codec
/// parameters set is not a per-track state this crate wants to hand
/// out.
///
/// `disposition` is the raw `AV_DISPOSITION_*` bit set, not
/// `ffmpeg_next::format::stream::Disposition`. That type's
/// `from_bits_truncate` drops bits the linked build has no constant
/// for, and this crate's stance on bit sets is that every pattern is a
/// value — the same reason `PacketFlags` reaches the wire as a number.
pub struct TrackExtra {
  stream_index: i32,
  disposition: i32,
  start_time: Option<i64>,
  frame_count: Option<i64>,
  parameters: Parameters,
}

impl TrackExtra {
  /// Constructs a `TrackExtra` from the stream index and its codec
  /// parameters. Everything else starts absent.
  ///
  /// **Fallible, and that is the point.** `Parameters::new()` and
  /// `Parameters::default()` are safe constructors that hand back a
  /// null-backed value when `avcodec_parameters_alloc` fails, saying
  /// nothing; accepting one here would store a landmine that goes off
  /// later, in a copy, on a thread that has forgotten the allocator
  /// ever failed. Refusing it at the door is what lets every other
  /// method on this type — and every reader of
  /// [`Self::parameters`] — rely on there being parameters at all.
  ///
  /// Not `const fn`: [`Parameters`] owns a heap allocation.
  pub fn new(stream_index: i32, parameters: Parameters) -> Result<Self, DemuxError> {
    // SAFETY: reading the pointer without dereferencing it.
    if unsafe { parameters.as_ptr() }.is_null() {
      return Err(DemuxError::ParametersMissing {
        stream_index: stream_index.max(0) as usize,
      });
    }
    Ok(Self {
      stream_index,
      disposition: 0,
      start_time: None,
      frame_count: None,
      parameters,
    })
  }

  /// A deep copy of this row, with the codec-parameter copy checked.
  ///
  /// The fallible counterpart of the `Clone` this type deliberately
  /// does not implement — see the type's own documentation for why.
  pub fn try_clone(&self) -> Result<Self, DemuxError> {
    // No re-check: `self` cannot exist over null-backed parameters, and
    // `clone_parameters` never returns one.
    Ok(Self {
      stream_index: self.stream_index,
      disposition: self.disposition,
      start_time: self.start_time,
      frame_count: self.frame_count,
      parameters: self.clone_parameters()?,
    })
  }

  /// An owned deep copy of the track's codec parameters — the handoff
  /// that opens a decoder for this track.
  ///
  /// `FfmpegAudioStreamDecoder::open(track.extra().clone_parameters()?,
  /// track.timebase())`. Fallible because the copy is: an allocation
  /// failure here is the difference between a decoder that is not
  /// opened and one opened on parameters that are not the file's.
  pub fn clone_parameters(&self) -> Result<Parameters, DemuxError> {
    clone_parameters(&self.parameters, self.stream_index.max(0) as usize)
  }

  /// Returns the source `AVStream.index`.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> i32 {
    self.stream_index
  }
  /// Returns the raw `AVStream.disposition` bit set.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn disposition(&self) -> i32 {
    self.disposition
  }
  /// Returns the stream's start time in the track's timebase, or
  /// `None` when the container does not carry one.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn start_time(&self) -> Option<i64> {
    self.start_time
  }
  /// Returns `AVStream.nb_frames` when the container carries it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn frame_count(&self) -> Option<i64> {
    self.frame_count
  }
  /// Returns the stream's codec parameters — the handle a decoder is
  /// opened from.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn parameters(&self) -> &Parameters {
    &self.parameters
  }

  /// Sets the disposition bits (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_disposition(mut self, value: i32) -> Self {
    self.disposition = value;
    self
  }
  /// Sets the start time (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_start_time(mut self, value: Option<i64>) -> Self {
    self.start_time = value;
    self
  }
  /// Sets the frame count (consuming builder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  #[must_use]
  pub const fn with_frame_count(mut self, value: Option<i64>) -> Self {
    self.frame_count = value;
    self
  }

  /// Sets the disposition bits in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_disposition(&mut self, value: i32) -> &mut Self {
    self.disposition = value;
    self
  }
  /// Sets the start time in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_start_time(&mut self, value: Option<i64>) -> &mut Self {
    self.start_time = value;
    self
  }
  /// Sets the frame count in place.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn set_frame_count(&mut self, value: Option<i64>) -> &mut Self {
    self.frame_count = value;
    self
  }
}

impl Clone for TrackExtra {
  /// Clones through [`Self::try_clone`] — the checked
  /// `avcodec_parameters_copy` this type requires — and panics on its
  /// `Err`. See the type's own docs for why this is hand-written
  /// rather than derived, and why a panic is the trade this makes
  /// rather than reintroducing the unchecked copy.
  ///
  /// # Panics
  ///
  /// Panics if the checked copy fails: allocation failure in
  /// `avcodec_parameters_alloc`, or a negative `avcodec_parameters_copy`
  /// return. Both are reachable only under allocator exhaustion — never
  /// from a malformed source, since a `TrackExtra` cannot exist over
  /// null-backed parameters (see [`Self::new`]). Callers that must not
  /// unwind on that failure should call [`Self::try_clone`] instead.
  fn clone(&self) -> Self {
    self.try_clone().unwrap_or_else(|error| {
      panic!(
        "TrackExtra::clone: checked codec-parameter copy failed ({error}) — \
         use TrackExtra::try_clone to handle this without panicking"
      )
    })
  }
}

impl std::fmt::Debug for TrackExtra {
  /// Hand-written because [`Parameters`] does not derive `Debug`. The
  /// medium and codec id are the two fields worth printing; the rest of
  /// `AVCodecParameters` is per-kind detail the track row already
  /// carries in typed form.
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TrackExtra")
      .field("stream_index", &self.stream_index)
      .field("disposition", &format_args!("{:#x}", self.disposition))
      .field("start_time", &self.start_time)
      .field("frame_count", &self.frame_count)
      .field(
        "parameters",
        &format_args!("{:?}", self.parameters.medium()),
      )
      .finish()
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

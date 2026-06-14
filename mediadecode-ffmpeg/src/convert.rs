//! Conversion helpers from FFmpeg `AVFrame` / `AVPacket` to the
//! `mediadecode` types parameterized by [`crate::Ffmpeg`] and
//! [`crate::FfmpegBuffer`].
//!
//! The video-frame conversion is **zero-copy**: each plane is exposed
//! as an `FfmpegBuffer` view into the underlying `AVBufferRef`, so the
//! FFmpeg-allocated pixel memory is shared between the source frame
//! and the produced `VideoFrame`. Cloning the resulting `VideoFrame`
//! bumps refcounts; dropping releases them.
use core::ptr::{addr_of, read_unaligned};

use ffmpeg_next::ffi::{
  AV_NOPTS_VALUE, AVChromaLocation, AVColorPrimaries, AVColorRange, AVColorSpace,
  AVColorTransferCharacteristic, AVFrame, AVPictureType, AVSubtitleType, av_buffer_alloc,
};
use mediadecode::{
  PixelFormat, Timebase, Timestamp,
  channel::AudioChannelLayout,
  color::{ChromaLocation, ColorInfo, ColorMatrix, ColorPrimaries, ColorRange, ColorTransfer},
  frame::{AudioFrame, Dimensions, Plane, Rect, SubtitleFrame, VideoFrame},
  subtitle::SubtitlePayload,
};

use crate::{
  FfmpegBuffer, boundary,
  extras::{AudioFrameExtra, PictureType, SideDataEntry, SubtitleFrameExtra, VideoFrameExtra},
  frame::{is_supported_cpu_pix_fmt, plane_height_for, plane_row_bytes_for},
  sample_format::SampleFormat,
};

/// Errors from [`av_frame_to_video_frame`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ConvertError {
  /// `av_frame` was null.
  NullFrame,
  /// The frame's pixel format isn't in the closed CPU-format set this
  /// crate supports for safe per-plane access.
  UnsupportedPixelFormat(PixelFormat),
  /// A plane reported `linesize <= 0` or otherwise inconsistent layout.
  InvalidPlaneLayout {
    /// Plane index.
    plane: usize,
  },
  /// Failed to acquire an `AVBufferRef` for a plane (out of memory, or
  /// the frame's `data[i]` pointer doesn't lie inside any of `buf[]`).
  BufferAcquireFailed {
    /// Plane index whose buffer couldn't be acquired.
    plane: usize,
  },
}

impl core::fmt::Display for ConvertError {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::NullFrame => write!(f, "convert: AVFrame pointer was null"),
      Self::UnsupportedPixelFormat(pf) => {
        write!(f, "convert: unsupported pixel format {pf:?}")
      }
      Self::InvalidPlaneLayout { plane } => {
        write!(f, "convert: invalid layout on plane {plane}")
      }
      Self::BufferAcquireFailed { plane } => {
        write!(f, "convert: could not acquire buffer ref for plane {plane}")
      }
    }
  }
}

impl core::error::Error for ConvertError {}

/// Safe wrapper around [`av_frame_to_video_frame`] taking a borrowed
/// [`ffmpeg::Frame`](ffmpeg_next::Frame). Recommended entry point for
/// most callers — equivalent to passing `frame.as_ptr()` to the
/// unsafe variant, but the FFmpeg side keeps the frame alive for the
/// duration of the call so the safety contract is satisfied
/// internally.
pub fn video_frame_from(
  frame: &ffmpeg_next::Frame,
  time_base: Timebase,
) -> Result<VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, FfmpegBuffer>, ConvertError> {
  // SAFETY: `&frame` keeps the AVFrame alive for the duration of this
  // call; the unsafe convert just reads through the pointer.
  unsafe { av_frame_to_video_frame(frame.as_ptr(), time_base) }
}

/// Safe wrapper around [`av_frame_to_audio_frame`] taking a borrowed
/// [`ffmpeg::frame::Audio`](ffmpeg_next::frame::Audio).
pub fn audio_frame_from(
  frame: &ffmpeg_next::frame::Audio,
  time_base: Timebase,
) -> Result<AudioFrame<SampleFormat, AudioChannelLayout, AudioFrameExtra, FfmpegBuffer>, ConvertError>
{
  // SAFETY: `&frame` keeps the AVFrame alive for the duration of this
  // call.
  unsafe { av_frame_to_audio_frame(frame.as_ptr(), time_base) }
}

/// Safe wrapper around [`av_subtitle_to_subtitle_frame`] taking a
/// borrowed [`ffmpeg::Subtitle`](ffmpeg_next::Subtitle).
pub fn subtitle_frame_from(
  subtitle: &ffmpeg_next::Subtitle,
  time_base: Timebase,
) -> Result<SubtitleFrame<SubtitleFrameExtra, FfmpegBuffer>, ConvertError> {
  // SAFETY: `&subtitle` keeps the AVSubtitle alive for the duration
  // of this call.
  unsafe { av_subtitle_to_subtitle_frame(subtitle.as_ptr(), time_base) }
}

/// Converts an FFmpeg `AVFrame` (CPU-side, post-`av_hwframe_transfer_data`
/// or from a software decoder) into a `mediadecode::VideoFrame`
/// parameterized by [`crate::Ffmpeg`] / [`crate::FfmpegBuffer`].
///
/// `time_base` is the source stream's time base, used to label
/// `pts`/`duration` as mediatime [`Timestamp`]s.
///
/// # Safety
///
/// `av_frame` must be a live `*const AVFrame` for the duration of this
/// call. The frame's `buf[]` references are not consumed; the produced
/// `VideoFrame` holds its own refcounts on each underlying buffer.
pub unsafe fn av_frame_to_video_frame(
  av_frame: *const AVFrame,
  time_base: Timebase,
) -> Result<VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, FfmpegBuffer>, ConvertError> {
  if av_frame.is_null() {
    return Err(ConvertError::NullFrame);
  }
  // We deliberately never form `&*av_frame` — `AVFrame` contains
  // bindgen-enum fields (`pict_type`, `color_primaries`, `colorspace`,
  // `color_trc`, `color_range`, `chroma_location`, and an embedded
  // `AVChannelLayout` whose `order` is also enum-typed). If FFmpeg
  // (or a hostile decoder) writes a value outside our bindgen's
  // discriminant set, the `&AVFrame` reference itself would be
  // immediate UB before any field access. Working through the raw
  // pointer with field-by-field reads (and `addr_of!` for the
  // enum-typed fields) sidesteps this whole class.

  // Non-enum primitives are safe to read via `(*av_frame).field`
  // because validity for `i32`/`i64`/pointer types is just
  // "initialized bytes"; the surrounding struct's enum fields don't
  // contaminate this read.
  let format_raw = unsafe { (*av_frame).format };
  let width_raw = unsafe { (*av_frame).width };
  let height_raw = unsafe { (*av_frame).height };
  let pts_raw = unsafe { (*av_frame).pts };
  let duration_raw = unsafe { (*av_frame).duration };
  let pix_fmt = boundary::from_av_pixel_format(format_raw);
  let width = width_raw.max(0) as u32;
  let height = height_raw.max(0) as u32;

  // Build planes. We support the closed CPU-format set for which we
  // know the per-plane height (NV*, P0xx/P2xx/P4xx). Unknown formats
  // would let us read garbage `linesize * height` bytes — refuse.
  if !is_supported_cpu_pix_fmt(pix_fmt) {
    return Err(ConvertError::UnsupportedPixelFormat(pix_fmt));
  }

  let mut planes_out: [Plane<FfmpegBuffer>; 4] = [
    plane_placeholder()?,
    plane_placeholder()?,
    plane_placeholder()?,
    plane_placeholder()?,
  ];
  let mut plane_count: u8 = 0;

  // The loop body indexes `planes_out`, the AVFrame's `linesize`, and
  // its `data` array all by `plane_idx`. None of these are slices we
  // can iterate via `iter_mut().enumerate()` — `linesize` / `data` are
  // raw `[T; 8]` fields read through `(*av_frame).field[plane_idx]`,
  // and `planes_out` is also indexed by the same key for symmetry —
  // so the index-based loop is the natural shape.
  #[allow(clippy::needless_range_loop)]
  for plane_idx in 0..4 {
    // Read per-plane fields through the raw pointer (no `&AVFrame`
    // formed). `linesize` is `[c_int; 8]` and `data` is `[*mut u8; 8]`.
    let linesize = unsafe { (*av_frame).linesize[plane_idx] };
    if linesize <= 0 {
      // Either we ran past the active plane count (linesize == 0) or
      // the frame uses negative-stride vertical-flip (which our safe
      // accessors refuse).
      if linesize == 0 {
        break;
      }
      return Err(ConvertError::InvalidPlaneLayout { plane: plane_idx });
    }
    let data_ptr = unsafe { (*av_frame).data[plane_idx] };
    if data_ptr.is_null() {
      return Err(ConvertError::InvalidPlaneLayout { plane: plane_idx });
    }
    let plane_h = plane_height_for(pix_fmt, plane_idx, height as usize)
      .ok_or(ConvertError::InvalidPlaneLayout { plane: plane_idx })?;
    let row_bytes = plane_row_bytes_for(pix_fmt, plane_idx, width as usize)
      .ok_or(ConvertError::InvalidPlaneLayout { plane: plane_idx })?;
    if row_bytes > linesize as usize {
      return Err(ConvertError::InvalidPlaneLayout { plane: plane_idx });
    }
    // Safe-API stance for stride padding:
    //
    // Each row in the AVBufferRef is `linesize` bytes wide but only the
    // first `row_bytes` of them are guaranteed-initialized (the
    // codec's actual output). The remaining `linesize - row_bytes`
    // bytes per row are FFmpeg-allocator scratch — `av_malloc`'d, not
    // necessarily written by the decoder. Exposing those bytes as
    // part of an `&[u8]` slice is UB even if no consumer reads them.
    //
    // - When `linesize == row_bytes` (no padding), zero-copy: refcount
    //   the AVBufferRef and expose the full plane.
    // - When `linesize > row_bytes`, we copy each row tightly into a
    //   fresh AVBufferRef and expose that — `stride` becomes
    //   `row_bytes` and the buffer's length is `row_bytes * plane_h`
    //   with every byte initialized.
    let (view, exported_stride) = if (linesize as usize) == row_bytes {
      let plane_bytes = (plane_h)
        .checked_mul(linesize as usize)
        .ok_or(ConvertError::InvalidPlaneLayout { plane: plane_idx })?;
      let buf = unsafe { find_backing_buffer(av_frame, data_ptr, plane_bytes) }
        .ok_or(ConvertError::BufferAcquireFailed { plane: plane_idx })?;
      // Plain address subtraction (avoids `offset_from`'s
      // strict-provenance requirement; the pointers are independent
      // C-side casts).
      let offset = unsafe { (data_ptr as usize).wrapping_sub((*buf).data as usize) };
      // SAFETY: `buf` is non-null and live; offset + plane_bytes <= buf.size
      // by find_backing_buffer's check.
      let view = unsafe { FfmpegBuffer::from_ref_view(buf, offset, plane_bytes) }
        .ok_or(ConvertError::BufferAcquireFailed { plane: plane_idx })?;
      (view, linesize as u32)
    } else {
      let total_bytes = row_bytes
        .checked_mul(plane_h)
        .ok_or(ConvertError::InvalidPlaneLayout { plane: plane_idx })?;
      // Bound-check the readable extent in the source AVBufferRef
      // BEFORE we start dereferencing per-row offsets. The zero-copy
      // branch above did this implicitly by passing `plane_bytes` to
      // `find_backing_buffer`; the copy branch must do the same — a
      // buggy or hostile decoder/filter could hand us a `data_ptr`
      // backed by a buffer too small for `(plane_h - 1) * linesize +
      // row_bytes`, in which case `from_raw_parts` on the last few
      // rows would form a slice over invalid memory (immediate UB,
      // before any read).
      let last_row_offset = (plane_h.saturating_sub(1))
        .checked_mul(linesize as usize)
        .ok_or(ConvertError::InvalidPlaneLayout { plane: plane_idx })?;
      let readable_extent = last_row_offset
        .checked_add(row_bytes)
        .ok_or(ConvertError::InvalidPlaneLayout { plane: plane_idx })?;
      // `find_backing_buffer` confirms the AVBufferRef in `(*av_frame).buf[]`
      // that contains `data_ptr` covers at least `readable_extent`
      // bytes from the data pointer. We don't need the returned ptr;
      // we just need the existence guarantee.
      unsafe { find_backing_buffer(av_frame, data_ptr, readable_extent) }
        .ok_or(ConvertError::BufferAcquireFailed { plane: plane_idx })?;
      let mut packed: std::vec::Vec<u8> = std::vec::Vec::new();
      packed
        .try_reserve_exact(total_bytes)
        .map_err(|_| ConvertError::BufferAcquireFailed { plane: plane_idx })?;
      for row_idx in 0..plane_h {
        let row_offset = (row_idx)
          .checked_mul(linesize as usize)
          .ok_or(ConvertError::InvalidPlaneLayout { plane: plane_idx })?;
        // SAFETY: bounds-checked above via `find_backing_buffer`;
        // `row_offset + row_bytes <= readable_extent <= buf.size`.
        // Each per-row slice is the part the decoder writes
        // (initialized).
        let row_slice =
          unsafe { core::slice::from_raw_parts(data_ptr.add(row_offset) as *const u8, row_bytes) };
        packed.extend_from_slice(row_slice);
      }
      let buf = FfmpegBuffer::copy_from_slice(&packed)
        .ok_or(ConvertError::BufferAcquireFailed { plane: plane_idx })?;
      (buf, row_bytes as u32)
    };

    planes_out[plane_idx] = Plane::new(view, exported_stride);
    plane_count = (plane_idx + 1) as u8;
  }

  // pts / duration / time_base
  let pts = if pts_raw != AV_NOPTS_VALUE {
    Some(Timestamp::new(pts_raw, time_base))
  } else {
    None
  };
  let duration = if duration_raw > 0 {
    Some(Timestamp::new(duration_raw, time_base))
  } else {
    None
  };

  // Visible rect (FFmpeg crop).
  let visible_rect = unsafe { build_visible_rect(av_frame, width, height) };

  // Color metadata (the universal cross-backend bits). We read each
  // bindgen enum-typed field through a raw `i32` window — even
  // referencing an out-of-range enum value is UB before any cast can
  // run, so we never let Rust assume the field actually inhabits the
  // enum's discriminant set. FFmpeg version skew or a buggy decoder
  // can put unknown values into these fields.

  // SAFETY: `av_frame` points at a live AVFrame; `addr_of!` computes
  // the address without forming a reference, and `read_unaligned::<i32>`
  // is sound because each of these enum types has the layout of
  // `c_int` (i32) per FFmpeg's bindgen output.
  let color_primaries_raw =
    unsafe { read_unaligned(addr_of!((*av_frame).color_primaries) as *const i32) };
  let color_trc_raw = unsafe { read_unaligned(addr_of!((*av_frame).color_trc) as *const i32) };
  let colorspace_raw = unsafe { read_unaligned(addr_of!((*av_frame).colorspace) as *const i32) };
  let color_range_raw = unsafe { read_unaligned(addr_of!((*av_frame).color_range) as *const i32) };
  let chroma_location_raw =
    unsafe { read_unaligned(addr_of!((*av_frame).chroma_location) as *const i32) };
  let color = ColorInfo::UNSPECIFIED
    .with_primaries(map_primaries(color_primaries_raw))
    .with_transfer(map_transfer(color_trc_raw))
    .with_matrix(map_matrix(colorspace_raw))
    .with_range(map_range(color_range_raw))
    .with_chroma_location(map_chroma_loc(chroma_location_raw));

  // Backend-specific extras.
  let extra = unsafe { build_video_frame_extra(av_frame) };

  // pix_fmt is already mediadecode::PixelFormat thanks to the boundary
  // function above, so we just pass it through.
  let mut out = VideoFrame::new(
    Dimensions::new(width, height),
    pix_fmt,
    planes_out,
    plane_count,
    extra,
  )
  .with_pts(pts)
  .with_duration(duration)
  .with_color(color);
  if let Some(r) = visible_rect {
    out = out.with_visible_rect(Some(r));
  }
  Ok(out)
}

fn plane_placeholder() -> Result<Plane<FfmpegBuffer>, ConvertError> {
  // Allocate a zero-byte AVBufferRef as a placeholder for unused plane
  // slots. `[Plane<B>; 4]` requires four populated entries; we only
  // expose `plane_count` of them through `VideoFrame::planes()`.
  let raw = unsafe { av_buffer_alloc(0) };
  // `av_buffer_alloc(0)` is allowed to return null on some platforms;
  // fall back to allocating 1 byte if so.
  let raw = if raw.is_null() {
    unsafe { av_buffer_alloc(1) }
  } else {
    raw
  };
  if raw.is_null() {
    // Truly OOM. Return an error by way of a poisoned plane.
    return Err(ConvertError::BufferAcquireFailed { plane: 4 });
  }
  let buf =
    unsafe { FfmpegBuffer::take(raw) }.ok_or(ConvertError::BufferAcquireFailed { plane: 4 })?;
  Ok(Plane::new(buf, 0))
}

/// # Safety
/// `av_frame` must be a live `*const AVFrame` for the duration of this
/// call. The function reads only `crop_*` fields through the raw
/// pointer — it never forms `&AVFrame`, so unrelated invalid enum
/// fields elsewhere in the struct don't matter.
unsafe fn build_visible_rect(av_frame: *const AVFrame, width: u32, height: u32) -> Option<Rect> {
  let crop_left = unsafe { (*av_frame).crop_left } as u32;
  let crop_top = unsafe { (*av_frame).crop_top } as u32;
  let crop_right = unsafe { (*av_frame).crop_right } as u32;
  let crop_bottom = unsafe { (*av_frame).crop_bottom } as u32;
  if crop_left == 0 && crop_top == 0 && crop_right == 0 && crop_bottom == 0 {
    return None;
  }
  let x = crop_left;
  let y = crop_top;
  let w = width.saturating_sub(crop_left).saturating_sub(crop_right);
  let h = height.saturating_sub(crop_top).saturating_sub(crop_bottom);
  Some(Rect::new(x, y, w, h))
}

/// # Safety
/// `av_frame` must be a live `*const AVFrame` for the duration of this
/// call. Reads each individual field through the raw pointer; never
/// forms a `&AVFrame` reference.
unsafe fn build_video_frame_extra(av_frame: *const AVFrame) -> VideoFrameExtra {
  let mut out = VideoFrameExtra::default();
  // SAR.
  let sar_num = unsafe { (*av_frame).sample_aspect_ratio.num };
  let sar_den = unsafe { (*av_frame).sample_aspect_ratio.den };
  if sar_num > 0 && sar_den > 0 && (sar_num != 1 || sar_den != 1) {
    out.set_sample_aspect_ratio(Some((sar_num as u32, sar_den as u32)));
  }
  // Picture type — read raw to avoid bindgen-enum UB if FFmpeg writes
  // an out-of-range value (version skew / hostile decoder).

  // SAFETY: `av_frame` is live; reading `pict_type` as `i32` matches
  // the bindgen enum's underlying `c_int` storage.
  let pict_type_raw = unsafe { read_unaligned(addr_of!((*av_frame).pict_type) as *const i32) };
  out.set_picture_type(map_picture_type_raw(pict_type_raw));
  // Key frame and interlace flags. AVFrame.flags has dedicated bits
  // for these in recent FFmpeg; the deprecated fields (key_frame,
  // interlaced_frame, top_field_first) still mirror them.
  let flags = unsafe { (*av_frame).flags };
  out.set_key_frame(flags & ffmpeg_next::ffi::AV_FRAME_FLAG_KEY != 0);
  out.set_interlaced(flags & ffmpeg_next::ffi::AV_FRAME_FLAG_INTERLACED != 0);
  out.set_top_field_first(flags & ffmpeg_next::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST != 0);
  // Best-effort timestamp.
  let bet = unsafe { (*av_frame).best_effort_timestamp };
  if bet != AV_NOPTS_VALUE {
    out.set_best_effort_timestamp(Some(bet));
  }
  // Side data — passthrough as raw bytes.
  out.set_side_data(unsafe { collect_side_data(av_frame) });
  out
}

/// Maximum number of `AVFrameSideData` entries we will copy out of
/// a single AVFrame. Realistic streams attach a handful (mastering
/// display, content light level, dynamic HDR metadata, S12M
/// timecodes, A53 captions, …) — usually < 8. The cap exists so a
/// crafted stream can't drive the safe converter into a long
/// per-frame entry-allocation loop.
const SIDE_DATA_MAX_ENTRIES: usize = 64;
/// Per-AVFrame total side-data byte cap. HDR / dynamic-metadata
/// payloads are typically a few hundred bytes; A53 captions can run
/// to a few kilobytes; SEI dumps in pathological streams have been
/// observed in the tens of kilobytes. 256 KiB is two orders of
/// magnitude over the realistic upper bound while still bounded
/// enough that an attacker-driven OOM via metadata is impossible.
const SIDE_DATA_MAX_TOTAL_BYTES: usize = 256 * 1024;

/// Maximum number of `AVSubtitleRect` entries we copy from a single
/// AVSubtitle. Realistic subtitles attach 1–4 rects per cue; 64
/// gives two orders of magnitude of headroom.
const SUBTITLE_MAX_RECTS: usize = 64;
/// Per-rect text/ASS payload byte cap. ASS lines exceeding this
/// are unrealistic; the cap exists to defeat a malicious decoder
/// attaching a multi-megabyte "subtitle" string.
const SUBTITLE_MAX_TEXT_BYTES_PER_RECT: usize = 64 * 1024;
/// Total text/ASS payload byte cap across all rects of a single
/// AVSubtitle, including newline separators.
const SUBTITLE_MAX_TEXT_TOTAL_BYTES: usize = 256 * 1024;
/// Per-rect bitmap (`linesize * height`) byte cap. DVB / PGS
/// subtitles realistically run to ~256 KiB on full-HD overlays;
/// 16 MiB is two orders of magnitude over.
const SUBTITLE_MAX_BITMAP_BYTES_PER_RECT: usize = 16 * 1024 * 1024;
/// Total bitmap byte cap across all rects of a single AVSubtitle.
const SUBTITLE_MAX_BITMAP_TOTAL_BYTES: usize = 32 * 1024 * 1024;

/// Bounded counterpart to `CStr::from_ptr(p).to_bytes()`. Reads at
/// most `cap + 1` bytes from `ptr` looking for a NUL terminator;
/// returns `Some(slice)` of the bytes preceding the NUL on success,
/// or `None` if no NUL was found within the window (the input was
/// either too long or missing its required terminator entirely).
///
/// `CStr::from_ptr` walks until it hits a NUL — a valid-but-
/// pathological string makes that scan unbounded, and a missing
/// NUL is an outright UB precondition violation. This helper bounds
/// both at `cap + 1` bytes.
///
/// # Safety
/// `ptr` must be non-null and valid for reads of at least
/// `min(cap + 1, length-until-NUL)` bytes. FFmpeg subtitle/text
/// pointers satisfy this when `(*rect).text` / `.ass` is non-null
/// (per FFmpeg's contract — though the contract itself doesn't
/// bound the length).
unsafe fn bounded_cstr_bytes<'a>(ptr: *const core::ffi::c_char, cap: usize) -> Option<&'a [u8]> {
  // Read up to `cap + 1` bytes; the +1 lets a string exactly `cap`
  // bytes long (with a NUL at index `cap`) succeed.
  let max = cap.saturating_add(1);
  for i in 0..max {
    // SAFETY: Caller guarantees `ptr` is valid for reads of bytes
    // until the NUL or `max`. We stop at the first NUL within the
    // window.
    let byte = unsafe { *(ptr.add(i) as *const u8) };
    if byte == 0 {
      // SAFETY: `ptr` is valid for `i` byte reads (we just walked
      // them above). The slice doesn't include the NUL.
      return Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, i) });
    }
  }
  // No NUL found within `cap + 1` bytes — input is too long or
  // missing its terminator. Reject.
  None
}

/// # Safety
/// `av_frame` must be a live `*const AVFrame`. The function reads
/// `nb_side_data` and `side_data[]` through the raw pointer; each
/// `AVFrameSideData.type_` is read raw (it's a bindgen enum), and
/// each `data` payload is bounds-checked before slicing.
///
/// Memory-safety stance: this function is called on every decoded
/// frame, on data the decoder controls. Side-data is bounded by
/// [`SIDE_DATA_MAX_ENTRIES`] entries and [`SIDE_DATA_MAX_TOTAL_BYTES`]
/// total bytes; once either cap is reached we stop copying further
/// entries and a `tracing::warn!` is emitted at most once per call.
/// Allocations use `try_reserve_exact` so OOM surfaces as a dropped
/// entry rather than a process abort.
unsafe fn collect_side_data(av_frame: *const AVFrame) -> std::vec::Vec<SideDataEntry> {
  // Read `nb_side_data` as the bindgen `c_int` and clamp non-
  // positive values BEFORE casting to `usize`. A negative value
  // (corrupt / version-skew decoder output) cast directly to
  // `usize` becomes a huge positive count and would walk OOB
  // memory below; treat it as "no side data".
  let nb_side_data_raw = unsafe { (*av_frame).nb_side_data };
  let side_data = unsafe { (*av_frame).side_data };
  if nb_side_data_raw <= 0 || side_data.is_null() {
    return Vec::new();
  }
  let count_raw = nb_side_data_raw as usize;
  let count = count_raw.min(SIDE_DATA_MAX_ENTRIES);
  if count_raw > SIDE_DATA_MAX_ENTRIES {
    tracing::warn!(
      cap = SIDE_DATA_MAX_ENTRIES,
      requested = count_raw,
      "mediadecode-ffmpeg: AVFrame.nb_side_data exceeds entry cap; truncating",
    );
  }
  let mut out: Vec<SideDataEntry> = Vec::new();
  if out.try_reserve_exact(count).is_err() {
    return Vec::new();
  }
  let mut total_bytes: usize = 0;
  for i in 0..count {
    let sd = unsafe { *side_data.add(i) };
    if sd.is_null() {
      continue;
    }
    // `AVFrameSideData.type_` is `AVFrameSideDataType` — bindgen
    // enum. Read raw to avoid forming an invalid value if FFmpeg
    // writes an unknown discriminant (version skew).
    let kind = unsafe { read_unaligned(addr_of!((*sd).type_) as *const i32) };
    let size = unsafe { (*sd).size };
    let data_ptr = unsafe { (*sd).data };
    let data_slice = if size == 0 || data_ptr.is_null() {
      Vec::new()
    } else {
      // Byte-budget check: stop copying further side-data entries
      // once we've reached the per-frame cap. Earlier entries
      // already in `out` stay; later entries are dropped.
      let projected = total_bytes.saturating_add(size);
      if projected > SIDE_DATA_MAX_TOTAL_BYTES {
        tracing::warn!(
          cap = SIDE_DATA_MAX_TOTAL_BYTES,
          projected,
          "mediadecode-ffmpeg: AVFrame side-data byte cap reached; dropping remaining entries",
        );
        break;
      }
      total_bytes = projected;
      // Fallible copy. `try_reserve_exact` lets OOM surface as a
      // dropped entry rather than a process abort.
      let mut buf: Vec<u8> = Vec::new();
      if buf.try_reserve_exact(size).is_err() {
        continue;
      }
      // SAFETY: `data_ptr` is documented as valid for `size` bytes
      // per FFmpeg's AVFrameSideData contract.
      let src = unsafe { core::slice::from_raw_parts(data_ptr, size) };
      buf.extend_from_slice(src);
      buf
    };
    out.push(SideDataEntry::new(kind, data_slice));
  }
  out
}

/// Locate the `AVBufferRef` in `(*av_frame).buf[]` that backs
/// `data_ptr`, confirming the requested `bytes` fit inside the buffer.
/// Returns `None` on no match, null/empty `buf` entries, or any
/// arithmetic that would overflow `usize`.
///
/// # Safety
/// `av_frame` must be a live `*const AVFrame`. Reads `buf[]` (an
/// array of pointers — no bindgen-enum validity hazards).
unsafe fn find_backing_buffer(
  av_frame: *const AVFrame,
  data_ptr: *const u8,
  bytes: usize,
) -> Option<*mut ffmpeg_next::ffi::AVBufferRef> {
  let buf_array_len = unsafe { (*av_frame).buf.len() };
  for i in 0..buf_array_len {
    let buf = unsafe { (*av_frame).buf[i] };
    if buf.is_null() {
      continue;
    }
    let buf_data = unsafe { (*buf).data as *const u8 };
    let buf_size = unsafe { (*buf).size };
    if buf_data.is_null() {
      continue;
    }
    let start = buf_data as usize;
    let Some(end) = start.checked_add(buf_size) else {
      continue;
    };
    let dp = data_ptr as usize;
    let Some(dp_end) = dp.checked_add(bytes) else {
      continue;
    };
    if dp >= start && dp_end <= end {
      return Some(buf);
    }
  }
  None
}

fn map_primaries(raw: i32) -> ColorPrimaries {
  match raw {
    x if x == AVColorPrimaries::AVCOL_PRI_BT709 as i32 => ColorPrimaries::Bt709,
    x if x == AVColorPrimaries::AVCOL_PRI_UNSPECIFIED as i32 => ColorPrimaries::Unspecified,
    x if x == AVColorPrimaries::AVCOL_PRI_BT470M as i32 => ColorPrimaries::Bt470M,
    x if x == AVColorPrimaries::AVCOL_PRI_BT470BG as i32 => ColorPrimaries::Bt470Bg,
    x if x == AVColorPrimaries::AVCOL_PRI_SMPTE170M as i32 => ColorPrimaries::Smpte170M,
    x if x == AVColorPrimaries::AVCOL_PRI_SMPTE240M as i32 => ColorPrimaries::Smpte240M,
    x if x == AVColorPrimaries::AVCOL_PRI_FILM as i32 => ColorPrimaries::Film,
    x if x == AVColorPrimaries::AVCOL_PRI_BT2020 as i32 => ColorPrimaries::Bt2020,
    x if x == AVColorPrimaries::AVCOL_PRI_SMPTE428 as i32 => ColorPrimaries::SmpteSt428,
    x if x == AVColorPrimaries::AVCOL_PRI_SMPTE431 as i32 => ColorPrimaries::SmpteRp431,
    x if x == AVColorPrimaries::AVCOL_PRI_SMPTE432 as i32 => ColorPrimaries::SmpteEg432,
    x if x == AVColorPrimaries::AVCOL_PRI_EBU3213 as i32 => ColorPrimaries::Ebu3213E,
    _ => ColorPrimaries::Unspecified,
  }
}

fn map_transfer(raw: i32) -> ColorTransfer {
  match raw {
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_BT709 as i32 => ColorTransfer::Bt709,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_UNSPECIFIED as i32 => {
      ColorTransfer::Unspecified
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_GAMMA22 as i32 => ColorTransfer::Gamma22,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_GAMMA28 as i32 => ColorTransfer::Gamma28,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_SMPTE170M as i32 => ColorTransfer::Smpte170M,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_SMPTE240M as i32 => ColorTransfer::Smpte240M,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_LINEAR as i32 => ColorTransfer::Linear,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_LOG as i32 => ColorTransfer::Log100,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_LOG_SQRT as i32 => ColorTransfer::Log316,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_IEC61966_2_4 as i32 => {
      ColorTransfer::Iec6196624
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_BT1361_ECG as i32 => {
      ColorTransfer::Bt1361Ecg
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_IEC61966_2_1 as i32 => {
      ColorTransfer::Iec6196621
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_BT2020_10 as i32 => {
      ColorTransfer::Bt2020_10Bit
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_BT2020_12 as i32 => {
      ColorTransfer::Bt2020_12Bit
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_SMPTE2084 as i32 => {
      ColorTransfer::SmpteSt2084Pq
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_SMPTE428 as i32 => ColorTransfer::SmpteSt428,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_ARIB_STD_B67 as i32 => {
      ColorTransfer::AribStdB67Hlg
    }
    _ => ColorTransfer::Unspecified,
  }
}

fn map_matrix(raw: i32) -> ColorMatrix {
  match raw {
    x if x == AVColorSpace::AVCOL_SPC_BT709 as i32 => ColorMatrix::Bt709,
    x if x == AVColorSpace::AVCOL_SPC_BT2020_NCL as i32 => ColorMatrix::Bt2020Ncl,
    x if x == AVColorSpace::AVCOL_SPC_SMPTE170M as i32 => ColorMatrix::Bt601,
    x if x == AVColorSpace::AVCOL_SPC_BT470BG as i32 => ColorMatrix::Bt601,
    x if x == AVColorSpace::AVCOL_SPC_SMPTE240M as i32 => ColorMatrix::Smpte240m,
    x if x == AVColorSpace::AVCOL_SPC_FCC as i32 => ColorMatrix::Fcc,
    x if x == AVColorSpace::AVCOL_SPC_YCGCO as i32 => ColorMatrix::YCgCo,
    _ => ColorMatrix::Bt709, // ColorMatrix has no Unspecified; Bt709 is FFmpeg's height>=720 default
  }
}

fn map_range(raw: i32) -> ColorRange {
  match raw {
    x if x == AVColorRange::AVCOL_RANGE_JPEG as i32 => ColorRange::Full,
    x if x == AVColorRange::AVCOL_RANGE_MPEG as i32 => ColorRange::Limited,
    _ => ColorRange::Unspecified,
  }
}

fn map_chroma_loc(raw: i32) -> ChromaLocation {
  match raw {
    x if x == AVChromaLocation::AVCHROMA_LOC_LEFT as i32 => ChromaLocation::Left,
    x if x == AVChromaLocation::AVCHROMA_LOC_CENTER as i32 => ChromaLocation::Center,
    x if x == AVChromaLocation::AVCHROMA_LOC_TOPLEFT as i32 => ChromaLocation::TopLeft,
    x if x == AVChromaLocation::AVCHROMA_LOC_TOP as i32 => ChromaLocation::Top,
    x if x == AVChromaLocation::AVCHROMA_LOC_BOTTOMLEFT as i32 => ChromaLocation::BottomLeft,
    x if x == AVChromaLocation::AVCHROMA_LOC_BOTTOM as i32 => ChromaLocation::Bottom,
    _ => ChromaLocation::Unspecified,
  }
}

/// Converts an FFmpeg audio `AVFrame` into a `mediadecode::AudioFrame`.
///
/// The plane payloads are zero-copy views into the source frame's
/// `AVBufferRef` entries (the corresponding `data[i]` is always
/// covered by exactly one of `buf[i]` per FFmpeg's contract). Channel
/// counts above 8 (which would spill into `extended_buf`) are clamped
/// to 8 — the rare cases where this matters can read the source
/// `AVFrame` directly.
///
/// # Safety
///
/// `av_frame` must be a live `*const AVFrame` for the duration of this
/// call and must describe an audio frame (`format` is an
/// `AVSampleFormat`, `nb_samples > 0`, and `data[]` / `buf[]` populated).
pub unsafe fn av_frame_to_audio_frame(
  av_frame: *const AVFrame,
  time_base: Timebase,
) -> Result<AudioFrame<SampleFormat, AudioChannelLayout, AudioFrameExtra, FfmpegBuffer>, ConvertError>
{
  if av_frame.is_null() {
    return Err(ConvertError::NullFrame);
  }
  // Same stance as `av_frame_to_video_frame`: never form `&AVFrame`.
  // Read every field through the raw pointer; for `ch_layout` (which
  // contains an `order: AVChannelOrder` enum) we hand the raw pointer
  // straight into `channel_layout::audio_channel_layout_from_raw_ptr`,
  // which validates `order` as `i32` before constructing any
  // `AVChannelOrder` value.
  let format_raw = unsafe { (*av_frame).format };
  let sample_rate_raw = unsafe { (*av_frame).sample_rate };
  let nb_samples_raw = unsafe { (*av_frame).nb_samples };
  let pts_raw = unsafe { (*av_frame).pts };
  let duration_raw = unsafe { (*av_frame).duration };
  let bet_raw = unsafe { (*av_frame).best_effort_timestamp };

  let sample_format = SampleFormat::from_raw(format_raw);
  let sample_rate = sample_rate_raw.max(0) as u32;
  let nb_samples = nb_samples_raw.max(0) as u32;

  // SAFETY: `av_frame` is a live `*const AVFrame`; passing the
  // address of the embedded ch_layout as `*const AVChannelLayout`
  // is sound because `addr_of!` doesn't form a reference.
  let ch_layout_ptr = unsafe { addr_of!((*av_frame).ch_layout) };
  let channel_layout =
    unsafe { crate::channel_layout::audio_channel_layout_from_raw_ptr(ch_layout_ptr) };
  let channel_count_full = channel_layout.channels();
  let channel_count = channel_count_full.min(255) as u8;

  // Plane count: 1 for packed, channel_count for planar.
  let is_planar = sample_format.is_planar();
  let plane_count_full = if is_planar { channel_count as usize } else { 1 };
  // mediadecode's `AudioFrame` carries up to 8 plane slots
  // (matching `AV_NUM_DATA_POINTERS`). Planar audio with more than
  // 8 channels uses `AVFrame.extended_data[]` / `extended_buf[]`,
  // which we don't yet plumb through. Refuse the frame rather than
  // silently truncating to the first 8 channels and returning an
  // `AudioFrame` whose advertised `channel_count` exceeds its
  // populated plane count.
  if plane_count_full > 8 {
    return Err(ConvertError::InvalidPlaneLayout { plane: 8 });
  }
  let plane_count = plane_count_full as u8;

  // Per-plane size in bytes. For audio, FFmpeg only sets `linesize[0]`;
  // every planar plane has the same size, every packed buffer is the
  // total size for all channels. Validate against the format's
  // expected minimum so a hostile/buggy decoder can't smuggle a
  // shrunk linesize past us (which would let consumers read past
  // valid bytes when they trust `nb_samples`).
  let linesize0 = unsafe { (*av_frame).linesize[0] };
  if nb_samples > 0 && linesize0 <= 0 {
    return Err(ConvertError::InvalidPlaneLayout { plane: 0 });
  }
  let plane_bytes = linesize0.max(0) as usize;
  if nb_samples > 0 {
    let bytes_per_sample = sample_format
      .bytes_per_sample()
      .ok_or(ConvertError::InvalidPlaneLayout { plane: 0 })? as usize;
    let expected_per_plane = if is_planar {
      // Planar: each plane carries `nb_samples * bytes_per_sample`.
      (nb_samples as usize)
        .checked_mul(bytes_per_sample)
        .ok_or(ConvertError::InvalidPlaneLayout { plane: 0 })?
    } else {
      // Packed: the single plane interleaves all channels.
      (nb_samples as usize)
        .checked_mul(bytes_per_sample)
        .and_then(|x| x.checked_mul(channel_count.max(1) as usize))
        .ok_or(ConvertError::InvalidPlaneLayout { plane: 0 })?
    };
    if plane_bytes < expected_per_plane {
      return Err(ConvertError::InvalidPlaneLayout { plane: 0 });
    }
  }

  let mut planes_out: [Plane<FfmpegBuffer>; 8] = [
    audio_plane_placeholder()?,
    audio_plane_placeholder()?,
    audio_plane_placeholder()?,
    audio_plane_placeholder()?,
    audio_plane_placeholder()?,
    audio_plane_placeholder()?,
    audio_plane_placeholder()?,
    audio_plane_placeholder()?,
  ];

  // Same rationale as in the video path — index-by-key over three
  // unrelated raw arrays (`planes_out`, `(*av_frame).data`, and the
  // implicit per-plane bookkeeping); no slice iteration applies.
  #[allow(clippy::needless_range_loop)]
  for plane_idx in 0..plane_count as usize {
    let data_ptr = unsafe { (*av_frame).data[plane_idx] };
    if data_ptr.is_null() {
      // A null plane in a planar layout (or the sole plane in a
      // packed layout) means the decoder produced an incomplete
      // frame — surface as an error rather than returning a frame
      // whose `planes()` exposes empty placeholder channels for
      // the missing data.
      return Err(ConvertError::InvalidPlaneLayout { plane: plane_idx });
    }
    let buf = unsafe { find_audio_backing_buffer(av_frame, data_ptr, plane_bytes) }
      .ok_or(ConvertError::BufferAcquireFailed { plane: plane_idx })?;
    // See `av_frame_to_video_frame` for the rationale on plain
    // address subtraction over `offset_from`.
    let offset = unsafe { (data_ptr as usize).wrapping_sub((*buf).data as usize) };
    // SAFETY: `buf` is non-null and live; offset + plane_bytes <= buf.size
    // by find_audio_backing_buffer's bounds check.
    let view = unsafe { FfmpegBuffer::from_ref_view(buf, offset, plane_bytes) }
      .ok_or(ConvertError::BufferAcquireFailed { plane: plane_idx })?;
    planes_out[plane_idx] = Plane::new(view, plane_bytes as u32);
  }

  let pts = if pts_raw != AV_NOPTS_VALUE {
    Some(Timestamp::new(pts_raw, time_base))
  } else {
    None
  };
  let duration = if duration_raw > 0 {
    Some(Timestamp::new(duration_raw, time_base))
  } else {
    None
  };

  let mut extra = AudioFrameExtra::default();
  if bet_raw != AV_NOPTS_VALUE {
    extra.set_best_effort_timestamp(Some(bet_raw));
  }
  // SAFETY: caller upholds liveness for the duration of the call;
  // collect_side_data reads enum-typed `type_` raw and bounds-checks
  // each entry's data slice.
  extra.set_side_data(unsafe { collect_side_data(av_frame) });

  Ok(
    AudioFrame::new(
      sample_rate,
      nb_samples,
      channel_count,
      sample_format,
      channel_layout,
      planes_out,
      plane_count,
      extra,
    )
    .with_pts(pts)
    .with_duration(duration),
  )
}

fn audio_plane_placeholder() -> Result<Plane<FfmpegBuffer>, ConvertError> {
  let raw = unsafe { av_buffer_alloc(1) };
  if raw.is_null() {
    return Err(ConvertError::BufferAcquireFailed { plane: 8 });
  }
  let buf =
    unsafe { FfmpegBuffer::take(raw) }.ok_or(ConvertError::BufferAcquireFailed { plane: 8 })?;
  Ok(Plane::new(buf, 0))
}

/// # Safety
/// `av_frame` must be a live `*const AVFrame`.
unsafe fn find_audio_backing_buffer(
  av_frame: *const AVFrame,
  data_ptr: *const u8,
  bytes: usize,
) -> Option<*mut ffmpeg_next::ffi::AVBufferRef> {
  // Audio frames pack each plane into a separate AVBufferRef in buf[].
  // Same scan as the video path — finds whichever buffer's data range
  // contains data_ptr. Overflow-safe arithmetic per
  // `find_backing_buffer`'s rationale.
  let buf_array_len = unsafe { (*av_frame).buf.len() };
  for i in 0..buf_array_len {
    let buf = unsafe { (*av_frame).buf[i] };
    if buf.is_null() {
      continue;
    }
    let buf_data = unsafe { (*buf).data as *const u8 };
    let buf_size = unsafe { (*buf).size };
    if buf_data.is_null() {
      continue;
    }
    let start = buf_data as usize;
    let Some(end) = start.checked_add(buf_size) else {
      continue;
    };
    let dp = data_ptr as usize;
    let Some(dp_end) = dp.checked_add(bytes) else {
      continue;
    };
    if dp >= start && dp_end <= end {
      return Some(buf);
    }
  }
  None
}

/// Converts an FFmpeg `AVSubtitle` into a `mediadecode::SubtitleFrame`.
///
/// Strategy:
/// - If the subtitle contains any text/ASS rects, produce a
///   [`SubtitlePayload::Text`] whose buffer is the concatenation of
///   their UTF-8 contents (newline-separated).
/// - Otherwise, if the subtitle contains bitmap rects, produce a
///   [`SubtitlePayload::Bitmap`] with one [`mediadecode::subtitle::BitmapRegion`]
///   per rect (paletted indices and RGBA palette copied into fresh
///   refcounted FfmpegBuffers, since `AVSubtitleRect` data is not
///   refcounted).
/// - An empty subtitle (no rects) becomes an empty `Text` payload.
///
/// `time_base` is the source stream's time base, used to label
/// `pts` / `duration`. The duration is computed as
/// `(end_display_time - start_display_time)` in milliseconds, then
/// rescaled into `time_base`.
///
/// # Safety
///
/// `av_subtitle` must be a live `*const AVSubtitle` for the duration
/// of this call; the rect array (`av_subtitle.rects`) must be valid
/// for `av_subtitle.num_rects` entries.
pub unsafe fn av_subtitle_to_subtitle_frame(
  av_subtitle: *const ffmpeg_next::ffi::AVSubtitle,
  time_base: Timebase,
) -> Result<SubtitleFrame<SubtitleFrameExtra, FfmpegBuffer>, ConvertError> {
  if av_subtitle.is_null() {
    return Err(ConvertError::NullFrame);
  }
  // Same stance as `av_frame_to_video_frame`: never form `&AVSubtitle`
  // or `&AVSubtitleRect` (both contain `type_: AVSubtitleType` enum
  // fields). Read every field through the raw pointer.

  let mut text_chunks: std::vec::Vec<u8> = std::vec::Vec::new();
  let mut bitmap_regions: std::vec::Vec<mediadecode::subtitle::BitmapRegion<FfmpegBuffer>> =
    std::vec::Vec::new();

  let count_raw = unsafe { (*av_subtitle).num_rects } as usize;
  let rects_ptr = unsafe { (*av_subtitle).rects };
  // Defensive: `num_rects > 0` with `rects == null` would be a malformed
  // AVSubtitle, but a hostile decoder could produce one — bail rather
  // than dereferencing.
  if count_raw > 0 && rects_ptr.is_null() {
    return Err(ConvertError::NullFrame);
  }
  // Cap rect count, total text bytes, and total bitmap bytes
  // against decoder-controlled metadata. Realistic subtitles carry
  // a handful of rects (typically 1–4 per displayed cue), text
  // payloads in the low kilobytes (ASS lines), and bitmap
  // payloads in the low hundreds of KiB (DVB / PGS). These caps
  // are two orders of magnitude over realistic ceilings; their
  // job is to bound a malicious / corrupt stream's allocation
  // budget, not to limit legitimate use.
  let count = count_raw.min(SUBTITLE_MAX_RECTS);
  if count_raw > SUBTITLE_MAX_RECTS {
    tracing::warn!(
      cap = SUBTITLE_MAX_RECTS,
      requested = count_raw,
      "mediadecode-ffmpeg: AVSubtitle.num_rects exceeds rect cap; truncating",
    );
  }
  let mut text_total_bytes: usize = 0;
  let mut bitmap_total_bytes: usize = 0;

  let text_kind = AVSubtitleType::SUBTITLE_TEXT as i32;
  let ass_kind = AVSubtitleType::SUBTITLE_ASS as i32;
  let bitmap_kind = AVSubtitleType::SUBTITLE_BITMAP as i32;
  for i in 0..count {
    // SAFETY: rects_ptr is non-null (checked above) and points to
    // num_rects valid `*mut AVSubtitleRect` entries per FFmpeg's
    // contract; `i < count == num_rects`, so the offset is in-bounds.
    let rect_ptr = unsafe { *rects_ptr.add(i) };
    if rect_ptr.is_null() {
      continue;
    }
    // Read `type_` raw — avoid forming `&AVSubtitleRect` (which
    // would require type_ to be a valid AVSubtitleType variant).
    // SAFETY: `rect_ptr` is a live `*mut AVSubtitleRect`; `addr_of!`
    // computes the field address without forming a reference;
    // reading as `i32` matches the bindgen enum's `c_int` storage.
    let rect_type_raw = unsafe { read_unaligned(addr_of!((*rect_ptr).type_) as *const i32) };
    // Pre-read primitive fields we'll use later (no `&AVSubtitleRect`
    // ever formed).
    let rect_text_ptr = unsafe { (*rect_ptr).text };
    let rect_ass_ptr = unsafe { (*rect_ptr).ass };
    let rect_data0_ptr = unsafe { (*rect_ptr).data[0] };
    let rect_data1_ptr = unsafe { (*rect_ptr).data[1] };
    let rect_linesize0 = unsafe { (*rect_ptr).linesize[0] };
    let rect_w = unsafe { (*rect_ptr).w };
    let rect_h = unsafe { (*rect_ptr).h };
    let rect_x = unsafe { (*rect_ptr).x };
    let rect_y = unsafe { (*rect_ptr).y };

    match rect_type_raw {
      x if x == text_kind && !rect_text_ptr.is_null() => {
        // SAFETY: `text` is documented as a 0-terminated UTF-8
        // string, owned by FFmpeg for the lifetime of the AVSubtitle.
        // We use a *bounded* NUL search instead of `CStr::from_ptr`
        // — the latter walks until it finds a NUL, which a valid-
        // but-pathological string makes unbounded, and a missing
        // NUL violates the `CStr::from_ptr` precondition outright.
        // `bounded_cstr_bytes` searches at most
        // `SUBTITLE_MAX_TEXT_BYTES_PER_RECT + 1` bytes; if no NUL
        // is found inside that window the rect is rejected.
        let bytes = unsafe { bounded_cstr_bytes(rect_text_ptr, SUBTITLE_MAX_TEXT_BYTES_PER_RECT) }
          .ok_or(ConvertError::InvalidPlaneLayout { plane: 0 })?;
        // The cap is now enforced inside `bounded_cstr_bytes` (no
        // NUL within `cap + 1` ⇒ rejection); a redundant length
        // check is unnecessary but kept as documentation.
        if bytes.len() > SUBTITLE_MAX_TEXT_BYTES_PER_RECT {
          return Err(ConvertError::InvalidPlaneLayout { plane: 0 });
        }
        let separator = if text_chunks.is_empty() { 0 } else { 1 };
        let projected = text_total_bytes
          .saturating_add(bytes.len())
          .saturating_add(separator);
        if projected > SUBTITLE_MAX_TEXT_TOTAL_BYTES {
          return Err(ConvertError::InvalidPlaneLayout { plane: 0 });
        }
        if separator == 1 {
          text_chunks.push(b'\n');
        }
        text_chunks.extend_from_slice(bytes);
        text_total_bytes = projected;
      }
      x if x == ass_kind && !rect_ass_ptr.is_null() => {
        // SAFETY: `ass` is documented as 0-terminated UTF-8.
        // Same bounded-scan rationale as the TEXT branch above.
        let bytes = unsafe { bounded_cstr_bytes(rect_ass_ptr, SUBTITLE_MAX_TEXT_BYTES_PER_RECT) }
          .ok_or(ConvertError::InvalidPlaneLayout { plane: 0 })?;
        if bytes.len() > SUBTITLE_MAX_TEXT_BYTES_PER_RECT {
          return Err(ConvertError::InvalidPlaneLayout { plane: 0 });
        }
        let separator = if text_chunks.is_empty() { 0 } else { 1 };
        let projected = text_total_bytes
          .saturating_add(bytes.len())
          .saturating_add(separator);
        if projected > SUBTITLE_MAX_TEXT_TOTAL_BYTES {
          return Err(ConvertError::InvalidPlaneLayout { plane: 0 });
        }
        if separator == 1 {
          text_chunks.push(b'\n');
        }
        text_chunks.extend_from_slice(bytes);
        text_total_bytes = projected;
      }
      x if x == bitmap_kind => {
        // Bitmap region. data[0] = paletted indices, data[1] = RGBA
        // palette (256 entries × 4 bytes = 1024 bytes). Both are
        // owned by FFmpeg and not refcounted; copy into fresh buffers.
        let w = rect_w.max(0) as u32;
        let h = rect_h.max(0) as u32;
        let stride = rect_linesize0.max(0) as u32;
        if rect_data0_ptr.is_null() || stride == 0 || h == 0 {
          continue;
        }
        // `checked_mul` so a corrupt rect can't drive
        // `from_raw_parts` to an address-space-spanning length (UB
        // even before any deref).
        let data_len = (stride as usize)
          .checked_mul(h as usize)
          .ok_or(ConvertError::InvalidPlaneLayout { plane: 0 })?;
        // Per-rect bitmap byte cap (defends against a single
        // attacker rect larger than realistic DVB / PGS subtitles
        // by a wide margin).
        if data_len > SUBTITLE_MAX_BITMAP_BYTES_PER_RECT {
          return Err(ConvertError::InvalidPlaneLayout { plane: 0 });
        }
        let projected_total = bitmap_total_bytes.saturating_add(data_len);
        if projected_total > SUBTITLE_MAX_BITMAP_TOTAL_BYTES {
          return Err(ConvertError::InvalidPlaneLayout { plane: 0 });
        }
        // SAFETY: data[0] is valid for `linesize[0] * h` bytes per
        // FFmpeg's contract; the multiplication is checked above.
        let data_slice = unsafe { core::slice::from_raw_parts(rect_data0_ptr, data_len) };
        let data_buf = FfmpegBuffer::copy_from_slice(data_slice)
          .ok_or(ConvertError::BufferAcquireFailed { plane: 0 })?;
        let palette_len = 256 * 4;
        let palette_buf = if rect_data1_ptr.is_null() {
          FfmpegBuffer::copy_from_slice(&[])
            .ok_or(ConvertError::BufferAcquireFailed { plane: 1 })?
        } else {
          // SAFETY: palette buffer is 256*4 bytes per FFmpeg's contract.
          let p = unsafe { core::slice::from_raw_parts(rect_data1_ptr, palette_len) };
          FfmpegBuffer::copy_from_slice(p).ok_or(ConvertError::BufferAcquireFailed { plane: 1 })?
        };
        bitmap_regions.push(mediadecode::subtitle::BitmapRegion::new(
          rect_x.max(0) as u32,
          rect_y.max(0) as u32,
          w,
          h,
          stride,
          data_buf,
          palette_buf,
        ));
        bitmap_total_bytes = projected_total;
      }
      _ => {}
    }
  }

  let payload = if !text_chunks.is_empty() {
    let buf = FfmpegBuffer::copy_from_slice(&text_chunks)
      .ok_or(ConvertError::BufferAcquireFailed { plane: 0 })?;
    SubtitlePayload::Text {
      text: buf,
      language: None,
    }
  } else if !bitmap_regions.is_empty() {
    SubtitlePayload::Bitmap {
      regions: bitmap_regions,
    }
  } else {
    // No rects (or only `None`-typed) — empty text payload.
    let buf =
      FfmpegBuffer::copy_from_slice(&[]).ok_or(ConvertError::BufferAcquireFailed { plane: 0 })?;
    SubtitlePayload::Text {
      text: buf,
      language: None,
    }
  };

  let sub_pts = unsafe { (*av_subtitle).pts };
  let pts = if sub_pts != AV_NOPTS_VALUE {
    Some(Timestamp::new(sub_pts, time_base))
  } else {
    None
  };

  let extra = SubtitleFrameExtra::new(unsafe { (*av_subtitle).start_display_time }, unsafe {
    (*av_subtitle).end_display_time
  });

  Ok(SubtitleFrame::new(payload, extra).with_pts(pts))
}

fn map_picture_type_raw(raw: i32) -> PictureType {
  match raw {
    x if x == AVPictureType::AV_PICTURE_TYPE_I as i32 => PictureType::I,
    x if x == AVPictureType::AV_PICTURE_TYPE_P as i32 => PictureType::P,
    x if x == AVPictureType::AV_PICTURE_TYPE_B as i32 => PictureType::B,
    x if x == AVPictureType::AV_PICTURE_TYPE_S as i32 => PictureType::S,
    x if x == AVPictureType::AV_PICTURE_TYPE_SI as i32 => PictureType::Si,
    x if x == AVPictureType::AV_PICTURE_TYPE_SP as i32 => PictureType::Sp,
    x if x == AVPictureType::AV_PICTURE_TYPE_BI as i32 => PictureType::Bi,
    _ => PictureType::Unspecified,
  }
}

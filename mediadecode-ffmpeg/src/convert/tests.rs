use super::*;

use ffmpeg_next::ffi::AVColorRange;

/// The `yuvj*` family is JPEG full-range *by definition*. `map_range_for`
/// must report [`ColorRange::Full`] for every one of the five variants —
/// including when the frame's `color_range` field is
/// `AVCOL_RANGE_UNSPECIFIED` (the common MJPEG/JPEG case), which is the
/// exact regression: deriving the range from the field alone would yield
/// `Unspecified`, silently demoting a full-swing frame.
#[test]
fn map_range_for_forces_full_for_every_yuvj_variant() {
  let yuvj = [
    PixelFormat::Yuvj411p,
    PixelFormat::Yuvj420p,
    PixelFormat::Yuvj422p,
    PixelFormat::Yuvj440p,
    PixelFormat::Yuvj444p,
  ];
  let unspecified = AVColorRange::AVCOL_RANGE_UNSPECIFIED as i32;
  let mpeg = AVColorRange::AVCOL_RANGE_MPEG as i32;
  let jpeg = AVColorRange::AVCOL_RANGE_JPEG as i32;
  for pf in &yuvj {
    // Unspecified field — the regression scenario.
    assert_eq!(
      map_range_for(pf, unspecified),
      ColorRange::Full,
      "{pf:?} with UNSPECIFIED color_range must deliver Full"
    );
    // Even a (spurious) explicit MPEG/limited tag is overridden: a yuvj
    // frame is full-range regardless of what the field claims.
    assert_eq!(
      map_range_for(pf, mpeg),
      ColorRange::Full,
      "{pf:?} is full-range by definition even if color_range says MPEG"
    );
    // An explicit JPEG tag agrees.
    assert_eq!(map_range_for(pf, jpeg), ColorRange::Full);
  }
}

/// Non-`yuvj` formats defer entirely to the frame's `color_range` field —
/// the override is scoped to the `yuvj*` family and does **not** generalize
/// to RGB. A `ColorRange` describes the swing of a YUV luma/chroma signal;
/// RGB has no such swing, so an RGB (or plain YUV) frame with an
/// unspecified range stays `Unspecified` rather than being speculatively
/// relabeled `Full`.
#[test]
fn map_range_for_defers_for_non_yuvj() {
  let unspecified = AVColorRange::AVCOL_RANGE_UNSPECIFIED as i32;
  let jpeg = AVColorRange::AVCOL_RANGE_JPEG as i32;
  let mpeg = AVColorRange::AVCOL_RANGE_MPEG as i32;

  // Plain limited-range YUV: unspecified stays unspecified, explicit tags
  // pass through untouched.
  assert_eq!(
    map_range_for(&PixelFormat::Yuv420p, unspecified),
    ColorRange::Unspecified
  );
  assert_eq!(map_range_for(&PixelFormat::Yuv420p, jpeg), ColorRange::Full);
  assert_eq!(
    map_range_for(&PixelFormat::Yuv420p, mpeg),
    ColorRange::Limited
  );

  // RGB is deliberately NOT forced to Full: range is a YUV property, and
  // mediaframe makes no full-range claim about RGB formats.
  assert_eq!(
    map_range_for(&PixelFormat::Rgb24, unspecified),
    ColorRange::Unspecified
  );
  assert_eq!(
    map_range_for(&PixelFormat::Rgba, unspecified),
    ColorRange::Unspecified
  );
}

/// `is_yuvj` recognizes exactly the five JPEG-range planar YUV formats and
/// nothing else — a guard so a newly-added `yuvj*` variant (or an
/// accidental inclusion of a non-`yuvj` format) is caught here.
#[test]
fn is_yuvj_covers_exactly_the_five_variants() {
  for pf in &[
    PixelFormat::Yuvj411p,
    PixelFormat::Yuvj420p,
    PixelFormat::Yuvj422p,
    PixelFormat::Yuvj440p,
    PixelFormat::Yuvj444p,
  ] {
    assert!(is_yuvj(pf), "{pf:?} should be recognized as yuvj");
  }
  // Their non-JPEG siblings and unrelated families are not yuvj.
  for pf in &[
    PixelFormat::Yuv411p,
    PixelFormat::Yuv420p,
    PixelFormat::Yuv422p,
    PixelFormat::Yuv440p,
    PixelFormat::Yuv444p,
    PixelFormat::Nv12,
    PixelFormat::Rgb24,
    PixelFormat::Gray8,
  ] {
    assert!(!is_yuvj(pf), "{pf:?} must not be classified yuvj");
  }
}

/// End-to-end regression: a real `AV_PIX_FMT_YUVJ420P` frame whose
/// `color_range` is left `AVCOL_RANGE_UNSPECIFIED` (exactly what FFmpeg's
/// MJPEG/JPEG decode paths and `av_frame_get_buffer` produce) must be
/// delivered with [`ColorRange::Full`], not `Unspecified`. Before the fix
/// `map_range` derived the range from the field alone and this returned
/// `Unspecified` — a silent full-range mislabel.
#[test]
fn yuvj420p_unspecified_range_delivers_full() {
  // `Video::new` allocates real plane buffers via `av_frame_get_buffer`
  // and leaves `color_range` at its zero default (== UNSPECIFIED).
  let mut frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::YUVJ420P, 64, 48);

  // Pin the regression precondition explicitly: color_range is UNSPECIFIED.
  // SAFETY: `frame` is a live, uniquely-owned AVFrame; we only write the
  // `color_range` scalar field through the raw pointer.
  unsafe {
    (*frame.as_mut_ptr()).color_range = AVColorRange::AVCOL_RANGE_UNSPECIFIED;
  }

  let out = video_frame_from(&frame, Timebase::default())
    .expect("YUVJ420P frame should convert to a VideoFrame");

  assert_eq!(*out.pixel_format(), PixelFormat::Yuvj420p);
  assert_eq!(
    out.color().range(),
    ColorRange::Full,
    "YUVJ420P with UNSPECIFIED color_range must be delivered as Full"
  );
}

/// Builds an unallocated `AVFrame` carrying `raw` in its `format` field.
///
/// The point is to reach the convert path with an integer no CPU layout
/// can be derived from — including integers outside our bindgen's
/// `AVPixelFormat` discriminant set, which is precisely the case the
/// restored diagnostic exists for. `AVFrame.format` is a plain C `int`,
/// so writing one is not a detour around the enum; there is no enum
/// here to begin with, which is why a raw id was always available at
/// this boundary and only the error had stopped carrying it.
fn frame_with_raw_format(raw: i32) -> ffmpeg_next::frame::Video {
  let mut frame = ffmpeg_next::frame::Video::empty();
  // SAFETY: `frame` is a live, uniquely-owned AVFrame; these are three
  // scalar field writes through the raw pointer.
  unsafe {
    let frame_ptr = frame.as_mut_ptr();
    (*frame_ptr).format = raw;
    (*frame_ptr).width = 64;
    (*frame_ptr).height = 48;
  }
  frame
}

/// A hardware surface has no CPU pixel data, so the unified vocabulary's
/// answer for it is `PixelFormat::None` — and before this the whole error
/// message was that `None`, naming nothing. The raw id and FFmpeg's own
/// name for it now ride along.
#[test]
fn an_unsupported_format_is_named_in_the_error() {
  use ffmpeg_next::ffi::AVPixelFormat;

  let raw = AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32;
  let frame = frame_with_raw_format(raw);

  // `VideoFrame` has no `Debug`, so `expect_err` is unavailable.
  let Err(err) = video_frame_from(&frame, Timebase::default()) else {
    panic!("a hardware surface carries no deliverable CPU layout");
  };

  let ConvertError::UnsupportedPixelFormat {
    format,
    raw: got,
    name,
  } = &err
  else {
    panic!("expected UnsupportedPixelFormat, got {err:?}");
  };
  // The *value* is unchanged — this restores the diagnostic, not the
  // struck `Unknown(u32)` variant.
  assert_eq!(*format, PixelFormat::None);
  assert_eq!(*got, raw);
  assert_eq!(name.as_deref(), Some("videotoolbox_vld"));

  let rendered = err.to_string();
  assert!(
    rendered.contains(&raw.to_string()),
    "the raw id is missing from {rendered:?}"
  );
  assert!(
    rendered.contains("videotoolbox_vld"),
    "the FFmpeg name is missing from {rendered:?}"
  );
}

/// An integer libavutil cannot describe — a corrupt read, or a format
/// from a newer library than the one linked. The name is absent and the
/// message says so, but the raw id is still there, which is the whole
/// point of carrying it separately.
#[test]
fn an_unnameable_format_still_reports_its_raw_id() {
  let raw = 99_999;
  let frame = frame_with_raw_format(raw);

  let Err(err) = video_frame_from(&frame, Timebase::default()) else {
    panic!("an unmappable format integer has no deliverable layout");
  };

  let ConvertError::UnsupportedPixelFormat {
    format,
    raw: got,
    name,
  } = &err
  else {
    panic!("expected UnsupportedPixelFormat, got {err:?}");
  };
  assert_eq!(*format, PixelFormat::None);
  assert_eq!(*got, raw);
  assert_eq!(*name, None);

  let rendered = err.to_string();
  assert!(
    rendered.contains("99999"),
    "the raw id is missing from {rendered:?}"
  );
  assert!(
    rendered.contains("unnamed by libavutil"),
    "the absent-name case is not spelled out in {rendered:?}"
  );
}

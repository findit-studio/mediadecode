//! The verification law: every estimate is compared against the real
//! `av_frame_get_buffer(0)` allocation, and must dominate it.

use ffmpeg_next::ffi;

use super::{audio_frame_bytes, video_frame_bytes};

/// The real allocation: the sum of every `AVBufferRef.size` the frame
/// ends up holding.
fn actual_video(format_raw: i32, width: i32, height: i32) -> usize {
  // SAFETY: a fresh `AVFrame` is allocated, sized, and freed here; every
  // field written is a plain integer and the return code is checked.
  unsafe {
    let f = ffi::av_frame_alloc();
    assert!(!f.is_null());
    (*f).format = format_raw;
    (*f).width = width;
    (*f).height = height;
    // A format or shape the allocator refuses carries no CPU bytes to
    // dominate; the sweeps skip those cells rather than fail on them.
    if ffi::av_frame_get_buffer(f, 0) != 0 {
      ffi::av_frame_free(&mut (f as *mut _));
      return 0;
    }
    let mut total = 0usize;
    for i in 0..8 {
      if !(*f).buf[i].is_null() {
        total += (*(*f).buf[i]).size;
      }
    }
    ffi::av_frame_free(&mut (f as *mut _));
    total
  }
}

/// As [`actual_video`], for audio — including the `extended_buf` planes
/// a frame past eight channels spills into.
fn actual_audio(format_raw: i32, nb_samples: i32, channels: i32) -> usize {
  // SAFETY: as above; `av_channel_layout_default` fills the layout.
  unsafe {
    let f = ffi::av_frame_alloc();
    assert!(!f.is_null());
    (*f).format = format_raw;
    (*f).nb_samples = nb_samples;
    ffi::av_channel_layout_default(core::ptr::addr_of_mut!((*f).ch_layout), channels);
    if ffi::av_frame_get_buffer(f, 0) != 0 {
      ffi::av_frame_free(&mut (f as *mut _));
      return 0;
    }
    let mut total = 0usize;
    for i in 0..8 {
      if !(*f).buf[i].is_null() {
        total += (*(*f).buf[i]).size;
      }
    }
    if !(*f).extended_buf.is_null() {
      for i in 0..(*f).nb_extended_buf as isize {
        let b = *(*f).extended_buf.offset(i);
        if !b.is_null() {
          total += (*b).size;
        }
      }
    }
    ffi::av_frame_free(&mut (f as *mut _));
    total
  }
}

// # Why a matrix and not a table
//
// The first version of these lanes was a **curated list** — the shapes
// the findings happened to name, plus a couple of ordinary ones — and
// it passed while the packed-audio formula was twenty-five times
// under. It priced `s16` at eight channels (576 against a real 544,
// dominated) and never asked about `dbl` at eight channels (576
// against a real 2,080). The bug lived in the *order* of the
// arithmetic, so it only showed once the per-sample width grew, and no
// curated row grew it.
//
// **A curated row proves the case it names and nothing else.** These
// are exhaustive instead: every sample format this build names crossed
// with a channel and sample-count spread, and every CPU pixel format
// crossed with degenerate, odd and ordinary shapes. The cost is a few
// thousand real allocations per run, which is cheap next to shipping a
// ceiling that does not hold.

/// Channel counts spanning mono, stereo, the plane-array boundary, and
/// the largest count this crate will carry.
const CHANNEL_SPREAD: [i32; 5] = [1, 2, 8, 32, 255];

/// Sample counts spanning the minimal frame, an ordinary one, and
/// FLAC's largest block.
const SAMPLE_SPREAD: [i32; 3] = [1, 1024, 65_535];

#[test]
fn the_audio_pricer_dominates_every_format_the_build_names() {
  ffmpeg_next::init().expect("ffmpeg init");
  let mut cells = 0usize;
  let mut skipped = 0usize;

  // Every sample format libavutil names, walked as the integer it is —
  // the same open-C-enum discipline the rest of the crate keeps, and
  // the reason this sweep stays correct when FFmpeg adds a format.
  for format_raw in 0..64i32 {
    // SAFETY: the width query takes the format as an integer and
    // answers 0 for anything it does not name.
    let width = unsafe { crate::decoder::c_shims::av_get_bytes_per_sample(format_raw) };
    if width <= 0 {
      continue;
    }
    for channels in CHANNEL_SPREAD {
      for nb_samples in SAMPLE_SPREAD {
        let actual = actual_audio(format_raw, nb_samples, channels);
        if actual == 0 {
          // The allocator declined this shape, so there is nothing to
          // dominate.
          skipped += 1;
          continue;
        }
        let estimate = audio_frame_bytes(format_raw, nb_samples as usize, channels as usize)
          .unwrap_or_else(|| {
            panic!("format {format_raw} ch={channels} nb={nb_samples}: priced as unpriceable")
          });
        assert!(
          estimate >= actual,
          "format {format_raw} ch={channels} nb={nb_samples}: \
           priced {estimate} against an actual allocation of {actual}",
        );
        cells += 1;
      }
    }
  }

  // The sweep has to have actually swept: twelve sample formats across
  // five channel counts and three sample counts.
  assert!(
    cells >= 150,
    "the matrix collapsed to {cells} cells ({skipped} skipped)",
  );

  assert!(
    audio_frame_bytes(ffi::AVSampleFormat::AV_SAMPLE_FMT_NONE as i32, 1024, 2).is_none(),
    "a format with no byte width must fail closed",
  );
}

/// Shapes for the video sweep: the degenerate ones where alignment
/// dominates, the odd ones that land off every boundary, and ordinary
/// pictures. Kept modest in area because this runs against every format
/// the build names and each cell is a real allocation.
const VIDEO_SHAPES: [(i32, i32); 8] = [
  (1, 1),
  (1, 64),
  (64, 1),
  (16, 16),
  (17, 17),
  (33, 17),
  (129, 129),
  (640, 480),
];

#[test]
fn the_video_pricer_dominates_every_format_the_build_names() {
  ffmpeg_next::init().expect("ffmpeg init");
  let mut cells = 0usize;

  let mut desc: *const ffi::AVPixFmtDescriptor = core::ptr::null();
  loop {
    // SAFETY: walks libavutil's own static descriptor table; the id is
    // read through the `c_int` shim and never becomes a Rust enum.
    desc = unsafe { ffi::av_pix_fmt_desc_next(desc) };
    if desc.is_null() {
      break;
    }
    let format_raw = unsafe { crate::decoder::c_shims::av_pix_fmt_desc_get_id(desc) };
    for (w, h) in VIDEO_SHAPES {
      let actual = actual_video(format_raw, w, h);
      if actual == 0 {
        // Hardware surfaces and formats the allocator refuses at this
        // shape carry no CPU bytes to dominate.
        continue;
      }
      let estimate = video_frame_bytes(format_raw, w, h)
        .unwrap_or_else(|| panic!("format {format_raw} {w}x{h}: priced as unpriceable"));
      assert!(
        estimate >= actual,
        "format {format_raw} {w}x{h}: priced {estimate} against an actual allocation of {actual}",
      );
      cells += 1;
    }
  }
  assert!(cells >= 800, "the matrix collapsed to {cells} cells");

  // And the degenerate shapes that motivated the alignment charge, on
  // the formats where they are cheap enough to allocate.
  for (format_raw, w, h) in [
    (ffi::AVPixelFormat::AV_PIX_FMT_GRAY8 as i32, 65_536, 1),
    (ffi::AVPixelFormat::AV_PIX_FMT_GRAY8 as i32, 1, 65_536),
    (ffi::AVPixelFormat::AV_PIX_FMT_YUV420P as i32, 1920, 1080),
    (ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32, 1920, 1080),
  ] {
    let actual = actual_video(format_raw, w, h);
    let estimate = video_frame_bytes(format_raw, w, h).expect("priceable");
    assert!(
      estimate >= actual,
      "format {format_raw} {w}x{h}: priced {estimate} against {actual}",
    );
  }

  // Unpriceable shapes fail closed rather than answering zero.
  assert!(video_frame_bytes(ffi::AVPixelFormat::AV_PIX_FMT_NV12 as i32, 0, 16).is_none());
  assert!(video_frame_bytes(-1, 16, 16).is_none());
}

#[test]
fn the_unpriceable_bound_dominates_the_priceable_estimate() {
  ffmpeg_next::init().expect("ffmpeg init");
  use super::video_frame_bytes_upper_bound;

  // The bound exists for judges that have dimensions but no priceable
  // layout. It has to dominate what the accurate path would have said
  // for the same extent in *any* format — otherwise the "conservative"
  // fallback is the generous one.
  //
  // It also has to dominate the bare `w * h * 16` it replaces, which
  // omitted the alignment and the plane slack: for a 65x65 frame that
  // multiply says 67,600 while the real worst layout aligns to 128x128
  // and costs 262,144.
  for (w, h) in [
    (1, 1),
    (16, 16),
    (65, 65),
    (65_536, 1),
    (1, 65_536),
    (1920, 1080),
  ] {
    let bound = video_frame_bytes_upper_bound(w, h).expect("a picture");
    let bare = (w as usize) * (h as usize) * 16;
    assert!(
      bound >= bare,
      "{w}x{h}: the bound {bound} is below the bare multiply {bare} it replaces",
    );
    // And above every format the build can actually price at that
    // extent, which is the property the judges rely on.
    let mut desc: *const ffi::AVPixFmtDescriptor = core::ptr::null();
    loop {
      desc = unsafe { ffi::av_pix_fmt_desc_next(desc) };
      if desc.is_null() {
        break;
      }
      let id = unsafe { crate::decoder::c_shims::av_pix_fmt_desc_get_id(desc) };
      if let Some(priced) = video_frame_bytes(id, w, h) {
        assert!(
          bound >= priced,
          "{w}x{h}: bound {bound} below format {id} priced at {priced}",
        );
      }
    }
  }

  assert!(video_frame_bytes_upper_bound(0, 16).is_none());
}

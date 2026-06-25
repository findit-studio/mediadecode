use super::*;

use ffmpeg_next::ffi::{
  AV_PIX_FMT_FLAG_BAYER, AV_PIX_FMT_FLAG_BITSTREAM, AV_PIX_FMT_FLAG_HWACCEL, AV_PIX_FMT_FLAG_PAL,
};

use crate::boundary::from_av_pixel_format;

/// Every non-`Unknown` `mediaframe::PixelFormat` variant. Drives the
/// round-trip and deliverability sweeps so a newly-added variant can't
/// silently escape coverage.
const ALL_PIXEL_FORMATS: &[PixelFormat] = &[
  PixelFormat::Yuv420p,
  PixelFormat::Yuv422p,
  PixelFormat::Yuv440p,
  PixelFormat::Yuv444p,
  PixelFormat::Yuv411p,
  PixelFormat::Yuv410p,
  PixelFormat::Yuvj411p,
  PixelFormat::Yuvj420p,
  PixelFormat::Yuvj422p,
  PixelFormat::Yuvj440p,
  PixelFormat::Yuvj444p,
  PixelFormat::Yuv420p9Le,
  PixelFormat::Yuv420p9Be,
  PixelFormat::Yuv420p10Le,
  PixelFormat::Yuv420p10Be,
  PixelFormat::Yuv420p12Le,
  PixelFormat::Yuv420p12Be,
  PixelFormat::Yuv420p14Le,
  PixelFormat::Yuv420p14Be,
  PixelFormat::Yuv420p16Le,
  PixelFormat::Yuv420p16Be,
  PixelFormat::Yuv422p9Le,
  PixelFormat::Yuv422p9Be,
  PixelFormat::Yuv422p10Le,
  PixelFormat::Yuv422p10Be,
  PixelFormat::Yuv422p12Le,
  PixelFormat::Yuv422p12Be,
  PixelFormat::Yuv422p14Le,
  PixelFormat::Yuv422p14Be,
  PixelFormat::Yuv422p16Le,
  PixelFormat::Yuv422p16Be,
  PixelFormat::Yuv440p10Le,
  PixelFormat::Yuv440p10Be,
  PixelFormat::Yuv440p12Le,
  PixelFormat::Yuv440p12Be,
  PixelFormat::Yuv444p9Le,
  PixelFormat::Yuv444p9Be,
  PixelFormat::Yuv444p10Le,
  PixelFormat::Yuv444p10Be,
  PixelFormat::Yuv444p12Le,
  PixelFormat::Yuv444p12Be,
  PixelFormat::Yuv444p14Le,
  PixelFormat::Yuv444p14Be,
  PixelFormat::Yuv444p16Le,
  PixelFormat::Yuv444p16Be,
  PixelFormat::Yuv444p10MsbLe,
  PixelFormat::Yuv444p10MsbBe,
  PixelFormat::Yuv444p12MsbLe,
  PixelFormat::Yuv444p12MsbBe,
  PixelFormat::Yuva420p,
  PixelFormat::Yuva422p,
  PixelFormat::Yuva444p,
  PixelFormat::Yuva420p9Le,
  PixelFormat::Yuva420p9Be,
  PixelFormat::Yuva422p9Le,
  PixelFormat::Yuva422p9Be,
  PixelFormat::Yuva444p9Le,
  PixelFormat::Yuva444p9Be,
  PixelFormat::Yuva420p10Le,
  PixelFormat::Yuva420p10Be,
  PixelFormat::Yuva422p10Le,
  PixelFormat::Yuva422p10Be,
  PixelFormat::Yuva444p10Le,
  PixelFormat::Yuva444p10Be,
  PixelFormat::Yuva420p12Le,
  PixelFormat::Yuva422p12Le,
  PixelFormat::Yuva422p12Be,
  PixelFormat::Yuva444p12Le,
  PixelFormat::Yuva444p12Be,
  PixelFormat::Yuva444p14Le,
  PixelFormat::Yuva420p16Le,
  PixelFormat::Yuva420p16Be,
  PixelFormat::Yuva422p16Le,
  PixelFormat::Yuva422p16Be,
  PixelFormat::Yuva444p16Le,
  PixelFormat::Yuva444p16Be,
  PixelFormat::Nv12,
  PixelFormat::Nv21,
  PixelFormat::Nv16,
  PixelFormat::Nv24,
  PixelFormat::Nv42,
  PixelFormat::Nv20Le,
  PixelFormat::Nv20Be,
  PixelFormat::P010Le,
  PixelFormat::P010Be,
  PixelFormat::P012Le,
  PixelFormat::P012Be,
  PixelFormat::P016Le,
  PixelFormat::P016Be,
  PixelFormat::P210Le,
  PixelFormat::P210Be,
  PixelFormat::P212Le,
  PixelFormat::P212Be,
  PixelFormat::P216Le,
  PixelFormat::P216Be,
  PixelFormat::P410Le,
  PixelFormat::P410Be,
  PixelFormat::P412Le,
  PixelFormat::P412Be,
  PixelFormat::P416Le,
  PixelFormat::P416Be,
  PixelFormat::Yuyv422,
  PixelFormat::Uyvy422,
  PixelFormat::Yvyu422,
  PixelFormat::Uyyvyy411,
  PixelFormat::Y210Le,
  PixelFormat::Y210Be,
  PixelFormat::Y212Le,
  PixelFormat::Y212Be,
  PixelFormat::Y216Le,
  PixelFormat::Y216Be,
  PixelFormat::V210,
  PixelFormat::V410Le,
  PixelFormat::Xv30Le,
  PixelFormat::Xv30Be,
  PixelFormat::V30xLe,
  PixelFormat::V30xBe,
  PixelFormat::Xv36Le,
  PixelFormat::Xv36Be,
  PixelFormat::Xv48Le,
  PixelFormat::Xv48Be,
  PixelFormat::Vuya,
  PixelFormat::Vuyx,
  PixelFormat::Ayuv,
  PixelFormat::Ayuv64Le,
  PixelFormat::Ayuv64Be,
  PixelFormat::Uyva,
  PixelFormat::Vyu444,
  PixelFormat::Xyz12Le,
  PixelFormat::Xyz12Be,
  PixelFormat::Rgb24,
  PixelFormat::Bgr24,
  PixelFormat::Rgba,
  PixelFormat::Bgra,
  PixelFormat::Argb,
  PixelFormat::Abgr,
  PixelFormat::Rgbx,
  PixelFormat::Bgrx,
  PixelFormat::Xrgb,
  PixelFormat::Xbgr,
  PixelFormat::X2Rgb10Le,
  PixelFormat::X2Rgb10Be,
  PixelFormat::X2Bgr10Le,
  PixelFormat::X2Bgr10Be,
  PixelFormat::Gbr24p,
  PixelFormat::Rgb4,
  PixelFormat::Rgb4Byte,
  PixelFormat::Rgb8,
  PixelFormat::Bgr4,
  PixelFormat::Bgr4Byte,
  PixelFormat::Bgr8,
  PixelFormat::Rgb444Le,
  PixelFormat::Rgb444Be,
  PixelFormat::Bgr444Le,
  PixelFormat::Bgr444Be,
  PixelFormat::Rgb555Le,
  PixelFormat::Rgb555Be,
  PixelFormat::Bgr555Le,
  PixelFormat::Bgr555Be,
  PixelFormat::Rgb565Le,
  PixelFormat::Rgb565Be,
  PixelFormat::Bgr565Le,
  PixelFormat::Bgr565Be,
  PixelFormat::Rgb48Le,
  PixelFormat::Rgb48Be,
  PixelFormat::Bgr48Le,
  PixelFormat::Bgr48Be,
  PixelFormat::Rgba64Le,
  PixelFormat::Rgba64Be,
  PixelFormat::Bgra64Le,
  PixelFormat::Bgra64Be,
  PixelFormat::Rgb96Le,
  PixelFormat::Rgb96Be,
  PixelFormat::Rgba128Le,
  PixelFormat::Rgba128Be,
  PixelFormat::Rgbf16Le,
  PixelFormat::Rgbf16Be,
  PixelFormat::Rgbf32Le,
  PixelFormat::Rgbf32Be,
  PixelFormat::Rgbaf16Le,
  PixelFormat::Rgbaf16Be,
  PixelFormat::Rgbaf32Le,
  PixelFormat::Rgbaf32Be,
  PixelFormat::Gbrp,
  PixelFormat::Gbrp9Le,
  PixelFormat::Gbrp9Be,
  PixelFormat::Gbrp10Le,
  PixelFormat::Gbrp10Be,
  PixelFormat::Gbrp10MsbLe,
  PixelFormat::Gbrp10MsbBe,
  PixelFormat::Gbrp12Le,
  PixelFormat::Gbrp12Be,
  PixelFormat::Gbrp12MsbLe,
  PixelFormat::Gbrp12MsbBe,
  PixelFormat::Gbrp14Le,
  PixelFormat::Gbrp14Be,
  PixelFormat::Gbrp16Le,
  PixelFormat::Gbrp16Be,
  PixelFormat::Gbrpf16Le,
  PixelFormat::Gbrpf16Be,
  PixelFormat::Gbrpf32Le,
  PixelFormat::Gbrpf32Be,
  PixelFormat::Gbrap,
  PixelFormat::Gbrap10Le,
  PixelFormat::Gbrap10Be,
  PixelFormat::Gbrap12Le,
  PixelFormat::Gbrap12Be,
  PixelFormat::Gbrap14Le,
  PixelFormat::Gbrap14Be,
  PixelFormat::Gbrap16Le,
  PixelFormat::Gbrap16Be,
  PixelFormat::Gbrap32Le,
  PixelFormat::Gbrap32Be,
  PixelFormat::Gbrapf16Le,
  PixelFormat::Gbrapf16Be,
  PixelFormat::Gbrapf32Le,
  PixelFormat::Gbrapf32Be,
  PixelFormat::Gray8,
  PixelFormat::Gray8a,
  PixelFormat::Gray9Le,
  PixelFormat::Gray9Be,
  PixelFormat::Gray10Le,
  PixelFormat::Gray10Be,
  PixelFormat::Gray12Le,
  PixelFormat::Gray12Be,
  PixelFormat::Gray14Le,
  PixelFormat::Gray14Be,
  PixelFormat::Gray16Le,
  PixelFormat::Gray16Be,
  PixelFormat::Gray32Le,
  PixelFormat::Gray32Be,
  PixelFormat::Grayf32Le,
  PixelFormat::Grayf32Be,
  PixelFormat::Grayf16Le,
  PixelFormat::Grayf16Be,
  PixelFormat::Ya8,
  PixelFormat::Y400a,
  PixelFormat::Ya16Le,
  PixelFormat::Ya16Be,
  PixelFormat::Yaf16Le,
  PixelFormat::Yaf16Be,
  PixelFormat::Yaf32Le,
  PixelFormat::Yaf32Be,
  PixelFormat::Monowhite,
  PixelFormat::Monoblack,
  PixelFormat::Pal8,
  PixelFormat::BayerBggr8,
  PixelFormat::BayerRggb8,
  PixelFormat::BayerGbrg8,
  PixelFormat::BayerGrbg8,
  PixelFormat::BayerBggr10Le,
  PixelFormat::BayerRggb10Le,
  PixelFormat::BayerGbrg10Le,
  PixelFormat::BayerGrbg10Le,
  PixelFormat::BayerBggr12Le,
  PixelFormat::BayerRggb12Le,
  PixelFormat::BayerGbrg12Le,
  PixelFormat::BayerGrbg12Le,
  PixelFormat::BayerBggr14Le,
  PixelFormat::BayerRggb14Le,
  PixelFormat::BayerGbrg14Le,
  PixelFormat::BayerGrbg14Le,
  PixelFormat::BayerBggr16Le,
  PixelFormat::BayerBggr16Be,
  PixelFormat::BayerRggb16Le,
  PixelFormat::BayerRggb16Be,
  PixelFormat::BayerGbrg16Le,
  PixelFormat::BayerGbrg16Be,
  PixelFormat::BayerGrbg16Le,
  PixelFormat::BayerGrbg16Be,
];

/// mediaframe variants whose FFmpeg constant is a *discriminant alias*
/// of a canonical sibling, so a frame tagged with that wire value
/// decodes to the canonical variant — never back to the alias. The
/// exact `from_av(to_av(pf)) == pf` round trip therefore does not hold
/// for these; the discriminant-stable round trip (and the alias-
/// collapse assertion) covers them instead.
///
/// * `Gbr24p`  → `AV_PIX_FMT_GBR24P == AV_PIX_FMT_GBRP` → `Gbrp`.
/// * `Gray8a`  → `AV_PIX_FMT_GRAY8A == AV_PIX_FMT_YA8`  → `Ya8`.
/// * `Y400a`   → `AV_PIX_FMT_Y400A  == AV_PIX_FMT_YA8`  → `Ya8`.
const ALIAS_VARIANTS: &[(PixelFormat, PixelFormat)] = &[
  (PixelFormat::Gbr24p, PixelFormat::Gbrp),
  (PixelFormat::Gray8a, PixelFormat::Ya8),
  (PixelFormat::Y400a, PixelFormat::Ya8),
];

fn is_alias(pf: PixelFormat) -> bool {
  ALIAS_VARIANTS.iter().any(|&(a, _)| a == pf)
}

/// Independent geometry oracle: derives per-plane row-bytes and row
/// counts straight from libavutil's image-fill helpers, mirroring what
/// `plane_geometry` does but written out longhand in the test so the
/// plane-count derivation, the `size / linesize` division, and the
/// deliverability gate are exercised by a second code path rather than
/// asserted against themselves. Returns `None` exactly when the format
/// is not deliverable (no descriptor / rejected flag) or the dimensions
/// are out of range — matching `plane_geometry`'s contract.
fn oracle_geometry(pf: PixelFormat, w: usize, h: usize) -> Option<PlaneGeometry> {
  let av = to_av_pixel_format(pf)?;
  // SAFETY: `av` is a known `AV_PIX_FMT_*` constant.
  let desc = unsafe { ffmpeg_next::ffi::av_pix_fmt_desc_get(av) };
  if desc.is_null() {
    return None;
  }
  // SAFETY: non-null per the check; static libavutil descriptor.
  let flags = unsafe { (*desc).flags };
  let nb_components = unsafe { (*desc).nb_components };
  let rejected = (AV_PIX_FMT_FLAG_HWACCEL
    | AV_PIX_FMT_FLAG_BAYER
    | AV_PIX_FMT_FLAG_PAL
    | AV_PIX_FMT_FLAG_BITSTREAM) as u64;
  if flags & rejected != 0 || nb_components == 0 {
    return None;
  }

  let w_i = i32::try_from(w).ok()?;
  let h_i = i32::try_from(h).ok()?;
  if w_i <= 0 || h_i <= 0 {
    return None;
  }

  // SAFETY: `av` is a known constant.
  let count_raw = unsafe { ffmpeg_next::ffi::av_pix_fmt_count_planes(av) };
  if count_raw <= 0 || count_raw as usize > MAX_PLANES {
    return None;
  }
  let count = count_raw as usize;

  let mut linesizes: [core::ffi::c_int; MAX_PLANES] = [0; MAX_PLANES];
  // SAFETY: `linesizes` is a live `[c_int; 4]`; `av` is a known
  // constant; `w_i > 0`.
  let ret = unsafe { ffmpeg_next::ffi::av_image_fill_linesizes(linesizes.as_mut_ptr(), av, w_i) };
  if ret < 0 {
    return None;
  }
  let mut ls_isize: [isize; MAX_PLANES] = [0; MAX_PLANES];
  for i in 0..MAX_PLANES {
    if linesizes[i] < 0 {
      return None;
    }
    ls_isize[i] = linesizes[i] as isize;
  }

  let mut sizes: [usize; MAX_PLANES] = [0; MAX_PLANES];
  // SAFETY: `sizes` is a live `[usize; 4]`; `ls_isize` is a live
  // read-only `[isize; 4]`; `av` known; `h_i > 0`.
  let ret = unsafe {
    ffmpeg_next::ffi::av_image_fill_plane_sizes(sizes.as_mut_ptr(), av, h_i, ls_isize.as_ptr())
  };
  if ret < 0 {
    return None;
  }

  let mut row_bytes = [0usize; MAX_PLANES];
  let mut height = [0usize; MAX_PLANES];
  for i in 0..count {
    let ls = linesizes[i] as usize;
    if ls == 0 || !sizes[i].is_multiple_of(ls) {
      return None;
    }
    row_bytes[i] = ls;
    height[i] = sizes[i] / ls;
    if height[i] == 0 {
      return None;
    }
  }
  Some(PlaneGeometry {
    count,
    row_bytes,
    height,
  })
}

/// A spread of formats across every structural family the descriptor
/// route has to size correctly: planar YUV at 8/10/16-bit and both
/// endiannesses, semi-planar NV12 / P010, packed YUYV / V210-family
/// (V210 itself has no FFmpeg constant in this build, so XV30/V30X
/// stand in for the 3-samples-per-word packed case), high-bit packed
/// YUV (Y210), planar GBR, packed RGB / RGBA / RGB48, and greyscale.
/// Odd dimensions exercise the chroma `AV_CEIL_RSHIFT` rounding.
const ORACLE_FORMATS: &[PixelFormat] = &[
  // planar YUV 8 / 10 / 16-bit, LE + BE
  PixelFormat::Yuv420p,
  PixelFormat::Yuv422p,
  PixelFormat::Yuv444p,
  PixelFormat::Yuv420p10Le,
  PixelFormat::Yuv420p10Be,
  PixelFormat::Yuv444p16Le,
  PixelFormat::Yuv444p16Be,
  // semi-planar
  PixelFormat::Nv12,
  PixelFormat::Nv21,
  PixelFormat::Nv24,
  PixelFormat::P010Le,
  PixelFormat::P016Be,
  PixelFormat::P410Le,
  // packed YUV 8-bit + high-bit + 3-per-word
  PixelFormat::Yuyv422,
  PixelFormat::Uyvy422,
  PixelFormat::Y210Le,
  PixelFormat::Xv30Le,
  PixelFormat::V30xLe,
  // GBR planar
  PixelFormat::Gbrp,
  PixelFormat::Gbrp16Le,
  PixelFormat::Gbrap,
  // RGB / RGBA / RGB48
  PixelFormat::Rgb24,
  PixelFormat::Bgr24,
  PixelFormat::Rgba,
  PixelFormat::Argb,
  PixelFormat::Rgb48Le,
  PixelFormat::Rgba64Be,
  // greyscale
  PixelFormat::Gray8,
  PixelFormat::Gray16Le,
];

/// Dimensions covering even, odd-width, odd-height, and odd-both — the
/// cases where chroma sub-sampling rounds a plane's row count / byte
/// width up.
const ORACLE_DIMS: &[(usize, usize)] =
  &[(1920, 1080), (1921, 1080), (1920, 1081), (641, 481), (2, 2)];

#[test]
fn plane_geometry_matches_libavutil_oracle() {
  for &pf in ORACLE_FORMATS {
    for &(w, h) in ORACLE_DIMS {
      let got = plane_geometry(pf, w, h);
      let want = oracle_geometry(pf, w, h);
      assert_eq!(
        got, want,
        "plane_geometry({pf:?}, {w}, {h}) disagreed with the libavutil oracle"
      );
      // Every format in this set is deliverable, so both must be Some.
      assert!(
        got.is_some(),
        "{pf:?} at {w}x{h} should yield geometry but plane_geometry returned None"
      );
    }
  }
}

/// `plane_geometry` and the hand-written `frame::plane_*_for` tables
/// must agree on every format both cover. The convert path now sources
/// geometry from `plane_geometry` while the safe `Frame::row` /
/// `Frame::as_ptr` accessors still use the hand tables; a divergence
/// would let the two APIs expose different plane extents for the same
/// frame (a latent UB / truncation hazard). This pins them together.
#[test]
fn plane_geometry_agrees_with_frame_hand_tables() {
  use crate::frame::{plane_height_for, plane_row_bytes_for};
  // The formats the hand tables cover (the HW-output + SW-fallback set).
  let hand_formats = [
    PixelFormat::Nv12,
    PixelFormat::Nv21,
    PixelFormat::Nv16,
    PixelFormat::Nv24,
    PixelFormat::P010Le,
    PixelFormat::P012Le,
    PixelFormat::P016Le,
    PixelFormat::P210Le,
    PixelFormat::P212Le,
    PixelFormat::P216Le,
    PixelFormat::P410Le,
    PixelFormat::P412Le,
    PixelFormat::P416Le,
    PixelFormat::Yuv420p,
    PixelFormat::Yuv422p,
    PixelFormat::Yuv444p,
    PixelFormat::Yuv420p10Le,
    PixelFormat::Yuv420p12Le,
    PixelFormat::Yuv420p16Le,
    PixelFormat::Yuv422p10Le,
    PixelFormat::Yuv422p12Le,
    PixelFormat::Yuv422p16Le,
    PixelFormat::Yuv444p10Le,
    PixelFormat::Yuv444p12Le,
    PixelFormat::Yuv444p16Le,
    PixelFormat::Rgb24,
    PixelFormat::Bgr24,
    PixelFormat::Rgba,
    PixelFormat::Bgra,
    PixelFormat::Argb,
    PixelFormat::Abgr,
    PixelFormat::Gray8,
    PixelFormat::Gray16Le,
  ];
  for pf in hand_formats {
    for &(w, h) in ORACLE_DIMS {
      let geom = plane_geometry(pf, w, h)
        .unwrap_or_else(|| panic!("plane_geometry returned None for hand-table format {pf:?}"));
      for plane in 0..geom.count {
        let hand_h = plane_height_for(pf, plane, h)
          .unwrap_or_else(|| panic!("plane_height_for None for {pf:?} plane {plane}"));
        let hand_rb = plane_row_bytes_for(pf, plane, w)
          .unwrap_or_else(|| panic!("plane_row_bytes_for None for {pf:?} plane {plane}"));
        assert_eq!(
          geom.height[plane], hand_h,
          "row-count mismatch {pf:?} plane {plane} at {w}x{h}: descriptor={} hand={hand_h}",
          geom.height[plane]
        );
        assert_eq!(
          geom.row_bytes[plane], hand_rb,
          "row-bytes mismatch {pf:?} plane {plane} at {w}x{h}: descriptor={} hand={hand_rb}",
          geom.row_bytes[plane]
        );
      }
      // The hand table must not advertise more planes than the
      // descriptor (it would index a plane the descriptor says is
      // absent).
      assert!(
        plane_height_for(pf, geom.count, h).is_none(),
        "hand table reports a plane {} that the descriptor ({} planes) does not for {pf:?}",
        geom.count,
        geom.count
      );
    }
  }
}

/// Spot-check concrete geometry values for the formats whose layout is
/// most often gotten wrong by hand.
#[test]
fn plane_geometry_known_values() {
  // NV12: Y full, interleaved UV at half height, 2 bytes per chroma pair.
  let nv12 = plane_geometry(PixelFormat::Nv12, 1920, 1080).unwrap();
  assert_eq!(nv12.count, 2);
  assert_eq!(nv12.row_bytes[0], 1920);
  assert_eq!(nv12.height[0], 1080);
  assert_eq!(nv12.row_bytes[1], 1920); // ceil(1920/2)*2 U+V bytes
  assert_eq!(nv12.height[1], 540);

  // NV12 odd width rounds the chroma row up.
  let nv12_odd = plane_geometry(PixelFormat::Nv12, 1921, 1080).unwrap();
  assert_eq!(nv12_odd.row_bytes[0], 1921);
  assert_eq!(nv12_odd.row_bytes[1], 1922);

  // P010: 2 bytes/sample. Y row = 2*W, chroma row = ceil(W/2)*4.
  let p010 = plane_geometry(PixelFormat::P010Le, 1920, 1080).unwrap();
  assert_eq!(p010.count, 2);
  assert_eq!(p010.row_bytes[0], 3840);
  assert_eq!(p010.height[0], 1080);
  assert_eq!(p010.row_bytes[1], 3840);
  assert_eq!(p010.height[1], 540);

  // Planar YUV 4:2:0 has three planes; chroma at half resolution.
  let yuv420 = plane_geometry(PixelFormat::Yuv420p, 1920, 1080).unwrap();
  assert_eq!(yuv420.count, 3);
  assert_eq!(yuv420.row_bytes[0], 1920);
  assert_eq!(yuv420.height[0], 1080);
  assert_eq!(yuv420.row_bytes[1], 960);
  assert_eq!(yuv420.height[1], 540);
  assert_eq!(yuv420.row_bytes[2], 960);
  assert_eq!(yuv420.height[2], 540);

  // Packed RGB24: single plane, 3 bytes/pixel.
  let rgb24 = plane_geometry(PixelFormat::Rgb24, 640, 480).unwrap();
  assert_eq!(rgb24.count, 1);
  assert_eq!(rgb24.row_bytes[0], 640 * 3);
  assert_eq!(rgb24.height[0], 480);

  // GBR planar: three full-resolution planes.
  let gbrp = plane_geometry(PixelFormat::Gbrp, 640, 480).unwrap();
  assert_eq!(gbrp.count, 3);
  for p in 0..3 {
    assert_eq!(gbrp.row_bytes[p], 640);
    assert_eq!(gbrp.height[p], 480);
  }
}

/// Zero / overflowing dimensions are refused rather than producing a
/// degenerate geometry the unsafe convert path would trust.
#[test]
fn plane_geometry_rejects_bad_dimensions() {
  assert!(plane_geometry(PixelFormat::Nv12, 0, 1080).is_none());
  assert!(plane_geometry(PixelFormat::Nv12, 1920, 0).is_none());
  // Beyond c_int::MAX — libavutil's params are `int`.
  assert!(plane_geometry(PixelFormat::Nv12, usize::MAX, 1080).is_none());
}

/// For every deliverable format the boundary round trip is
/// discriminant-stable: re-deriving the FFmpeg constant from the
/// `PixelFormat` that `from_av` produced lands on the same wire value.
/// For the non-alias formats it is the stronger exact identity; the
/// three discriminant-alias variants collapse to their canonical
/// sibling instead (asserted explicitly).
#[test]
fn round_trip_deliverable_formats() {
  let mut deliverable = 0usize;
  for &pf in ALL_PIXEL_FORMATS {
    if !is_deliverable(pf) {
      continue;
    }
    deliverable += 1;
    let av = to_av_pixel_format(pf).expect("deliverable format must map to a constant");
    let back = from_av_pixel_format(av as i32);
    if is_alias(pf) {
      let &(_, canonical) = ALIAS_VARIANTS.iter().find(|&&(a, _)| a == pf).unwrap();
      assert_eq!(
        back, canonical,
        "{pf:?} should collapse to its canonical sibling {canonical:?} on round trip"
      );
    } else {
      assert_eq!(
        back, pf,
        "exact round trip failed: from_av(to_av({pf:?})) == {back:?}"
      );
    }
    // Discriminant-stable for all (including aliases): re-encoding the
    // decoded variant reproduces the original wire value.
    let reencoded = to_av_pixel_format(back).expect("decoded format must re-encode");
    assert_eq!(
      reencoded as i32, av as i32,
      "round trip not discriminant-stable for {pf:?}"
    );
  }
  // Guard against the sweep silently covering nothing.
  assert!(
    deliverable > 200,
    "expected >200 deliverable formats, found {deliverable}"
  );
}

/// Non-deliverable format classes must be rejected by `is_deliverable`
/// (so the convert path refuses them) even though several still map to
/// a `PixelFormat` variant and an FFmpeg constant.
#[test]
fn excludes_hwaccel_bayer_pal_mono() {
  // Hardware-surface formats: not deliverable, and the boundary maps
  // their wire value to `Unknown` (they're not CPU pixel data).
  for hw in [
    ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX,
    ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI,
    ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_CUDA,
    ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_D3D11,
  ] {
    let mapped = from_av_pixel_format(hw as i32);
    assert!(
      matches!(mapped, PixelFormat::Unknown(_)),
      "GPU surface {hw:?} should map to Unknown, got {mapped:?}"
    );
    assert!(
      !is_deliverable(mapped),
      "GPU surface {hw:?} must not be deliverable"
    );
  }

  // Bayer: maps to a Bayer `PixelFormat` variant but is rejected by the
  // BAYER flag (deferred to colconv demosaic).
  for bayer in [
    PixelFormat::BayerBggr8,
    PixelFormat::BayerRggb8,
    PixelFormat::BayerGbrg16Le,
    PixelFormat::BayerGrbg16Be,
  ] {
    assert!(
      !is_deliverable(bayer),
      "Bayer {bayer:?} must be rejected (BAYER flag)"
    );
    assert!(plane_geometry(bayer, 1920, 1080).is_none());
  }

  // Paletted: rejected by the PAL flag.
  assert!(!is_deliverable(PixelFormat::Pal8));
  assert!(plane_geometry(PixelFormat::Pal8, 1920, 1080).is_none());

  // Monochrome (sub-byte bitstream): rejected by the BITSTREAM flag.
  assert!(!is_deliverable(PixelFormat::Monowhite));
  assert!(!is_deliverable(PixelFormat::Monoblack));
  assert!(plane_geometry(PixelFormat::Monowhite, 1920, 1080).is_none());

  // Sub-byte packed RGB (also BITSTREAM): rejected.
  assert!(!is_deliverable(PixelFormat::Rgb4));
  assert!(!is_deliverable(PixelFormat::Bgr4));

  // Unknown is never deliverable and never maps to a constant.
  assert!(!is_deliverable(PixelFormat::Unknown(0)));
  assert!(to_av_pixel_format(PixelFormat::Unknown(123)).is_none());
}

/// The four mediaframe variants whose FFmpeg constant is absent from
/// the linked build (`V210`, `V410Le`, `Yuva420p12Le`, `Yuva444p14Le`)
/// must produce no constant — `to_av_pixel_format` returns `None` and
/// the format is consequently not deliverable.
#[test]
fn formats_without_linked_constant_are_unmapped() {
  for pf in [
    PixelFormat::V210,
    PixelFormat::V410Le,
    PixelFormat::Yuva420p12Le,
    PixelFormat::Yuva444p14Le,
  ] {
    assert!(
      to_av_pixel_format(pf).is_none(),
      "{pf:?} has no constant in this FFmpeg build and must map to None"
    );
    assert!(!is_deliverable(pf));
    assert!(plane_geometry(pf, 1920, 1080).is_none());
  }
}

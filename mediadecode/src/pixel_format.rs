//! Pixel format identifier — comprehensive coverage of FFmpeg's
//! `AVPixelFormat` enum plus Bayer mosaic and cinema-RAW formats.
//!
//! Naming convention: each variant's [`Display`] form is the
//! lowercase FFmpeg name where one exists (`yuv420p`, `nv12`, `p010le`,
//! …) so logs / wire formats line up with FFmpeg / `colconv`. The
//! variant identifier is the FFmpeg name in PascalCase
//! (`Yuv420p`, `Nv12`, `P010Le`, …).
//!
//! The enum covers:
//! - **Planar YUV** at 4:2:0 / 4:2:2 / 4:4:0 / 4:4:4, 8-bit and
//!   high-bit-depth (9 / 10 / 12 / 14 / 16-bit).
//! - **Planar YUVA** (with alpha) at the same subsampling × bit-depth.
//! - **Semi-planar YUV** (NV-family) at 4:2:0 / 4:2:2 / 4:4:4, 8-bit
//!   and 10 / 12 / 16-bit (P0xx / P2xx / P4xx).
//! - **Packed YUV** (yuyv / uyvy / yvyu / v210 / v410 / xv36 / Y2xx /
//!   ayuv64 / vuya / vuyx).
//! - **Packed RGB** at 8-bit (rgb24 / bgr24 / rgba / bgra / argb /
//!   abgr / rgbx / bgrx / xrgb / xbgr), low-bit (rgb444 / 555 / 565,
//!   bgr444 / 555 / 565), and high-bit (rgb48 / bgr48 / rgba64 / bgra64
//!   / x2rgb10 / x2bgr10), plus float (rgbf16 / rgbf32).
//! - **Planar GBR / GBRA** at 8-bit + high-bit + float.
//! - **Greyscale** (gray8 / 9 / 10 / 12 / 14 / 16 / f32) and
//!   greyscale-with-alpha (ya8 / ya16) and monochrome 1-bit
//!   (monowhite / monoblack).
//! - **Bayer** (BGGR / RGGB / GBRG / GRBG) at 8 / 10 / 12 / 14 / 16-bit.
//! - **Paletted** (pal8).
//!
//! Hardware-frame markers (FFmpeg's `AV_PIX_FMT_VIDEOTOOLBOX` /
//! `_VAAPI` / `_CUDA` / `_D3D11` / `_DRM_PRIME` / `_MEDIACODEC` /
//! `_VULKAN`) are intentionally **not** in this enum: the unified
//! vocabulary describes CPU-side decoded pixel data, and a frame
//! carrying GPU-resident buffers must be transferred to a CPU format
//! before reaching a `mediadecode::VideoFrame` consumer. Backend
//! crates handle the HW path internally.
//!
//! Stable wire format: [`Self::to_u32`] returns the underlying
//! discriminant (this enum is `#[repr(u32)]`); [`Self::from_u32`]
//! reverses the mapping. Unrecognised values map to [`Self::Unknown`].

use derive_more::{Display, IsVariant};

/// Pixel format identifier covering FFmpeg + Bayer + cinema-RAW.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, IsVariant)]
#[non_exhaustive]
#[repr(u32)]
pub enum PixelFormat {
  /// Unknown / unset format.
  #[display("unknown")]
  Unknown = 0,

  // ===================================================================
  // Planar YUV 8-bit
  // ===================================================================
  /// Planar 4:2:0 YUV, 8-bit (`AV_PIX_FMT_YUV420P`).
  #[display("yuv420p")]
  Yuv420p = 100,
  /// Planar 4:2:2 YUV, 8-bit.
  #[display("yuv422p")]
  Yuv422p = 101,
  /// Planar 4:4:0 YUV, 8-bit (vertically subsampled chroma).
  #[display("yuv440p")]
  Yuv440p = 102,
  /// Planar 4:4:4 YUV, 8-bit.
  #[display("yuv444p")]
  Yuv444p = 103,
  /// Planar 4:1:1 YUV, 8-bit.
  #[display("yuv411p")]
  Yuv411p = 104,
  /// Planar 4:1:0 YUV, 8-bit.
  #[display("yuv410p")]
  Yuv410p = 105,

  // ===================================================================
  // Planar YUV high-bit-depth (4:2:0)
  // ===================================================================
  /// Planar 4:2:0 YUV, 9-bit little-endian.
  #[display("yuv420p9le")]
  Yuv420p9Le = 110,
  /// Planar 4:2:0 YUV, 9-bit big-endian.
  #[display("yuv420p9be")]
  Yuv420p9Be = 111,
  /// Planar 4:2:0 YUV, 10-bit little-endian.
  #[display("yuv420p10le")]
  Yuv420p10Le = 112,
  /// Planar 4:2:0 YUV, 10-bit big-endian.
  #[display("yuv420p10be")]
  Yuv420p10Be = 113,
  /// Planar 4:2:0 YUV, 12-bit little-endian.
  #[display("yuv420p12le")]
  Yuv420p12Le = 114,
  /// Planar 4:2:0 YUV, 12-bit big-endian.
  #[display("yuv420p12be")]
  Yuv420p12Be = 115,
  /// Planar 4:2:0 YUV, 14-bit little-endian.
  #[display("yuv420p14le")]
  Yuv420p14Le = 116,
  /// Planar 4:2:0 YUV, 14-bit big-endian.
  #[display("yuv420p14be")]
  Yuv420p14Be = 117,
  /// Planar 4:2:0 YUV, 16-bit little-endian.
  #[display("yuv420p16le")]
  Yuv420p16Le = 118,
  /// Planar 4:2:0 YUV, 16-bit big-endian.
  #[display("yuv420p16be")]
  Yuv420p16Be = 119,

  // ===================================================================
  // Planar YUV high-bit-depth (4:2:2)
  // ===================================================================
  /// Planar 4:2:2 YUV, 9-bit little-endian.
  #[display("yuv422p9le")]
  Yuv422p9Le = 120,
  /// Planar 4:2:2 YUV, 9-bit big-endian.
  #[display("yuv422p9be")]
  Yuv422p9Be = 121,
  /// Planar 4:2:2 YUV, 10-bit little-endian.
  #[display("yuv422p10le")]
  Yuv422p10Le = 122,
  /// Planar 4:2:2 YUV, 10-bit big-endian.
  #[display("yuv422p10be")]
  Yuv422p10Be = 123,
  /// Planar 4:2:2 YUV, 12-bit little-endian.
  #[display("yuv422p12le")]
  Yuv422p12Le = 124,
  /// Planar 4:2:2 YUV, 12-bit big-endian.
  #[display("yuv422p12be")]
  Yuv422p12Be = 125,
  /// Planar 4:2:2 YUV, 14-bit little-endian.
  #[display("yuv422p14le")]
  Yuv422p14Le = 126,
  /// Planar 4:2:2 YUV, 14-bit big-endian.
  #[display("yuv422p14be")]
  Yuv422p14Be = 127,
  /// Planar 4:2:2 YUV, 16-bit little-endian.
  #[display("yuv422p16le")]
  Yuv422p16Le = 128,
  /// Planar 4:2:2 YUV, 16-bit big-endian.
  #[display("yuv422p16be")]
  Yuv422p16Be = 129,

  // ===================================================================
  // Planar YUV high-bit-depth (4:4:0)
  // ===================================================================
  /// Planar 4:4:0 YUV, 10-bit little-endian.
  #[display("yuv440p10le")]
  Yuv440p10Le = 130,
  /// Planar 4:4:0 YUV, 12-bit little-endian.
  #[display("yuv440p12le")]
  Yuv440p12Le = 131,

  // ===================================================================
  // Planar YUV high-bit-depth (4:4:4)
  // ===================================================================
  /// Planar 4:4:4 YUV, 9-bit little-endian.
  #[display("yuv444p9le")]
  Yuv444p9Le = 140,
  /// Planar 4:4:4 YUV, 9-bit big-endian.
  #[display("yuv444p9be")]
  Yuv444p9Be = 141,
  /// Planar 4:4:4 YUV, 10-bit little-endian.
  #[display("yuv444p10le")]
  Yuv444p10Le = 142,
  /// Planar 4:4:4 YUV, 10-bit big-endian.
  #[display("yuv444p10be")]
  Yuv444p10Be = 143,
  /// Planar 4:4:4 YUV, 12-bit little-endian.
  #[display("yuv444p12le")]
  Yuv444p12Le = 144,
  /// Planar 4:4:4 YUV, 12-bit big-endian.
  #[display("yuv444p12be")]
  Yuv444p12Be = 145,
  /// Planar 4:4:4 YUV, 14-bit little-endian.
  #[display("yuv444p14le")]
  Yuv444p14Le = 146,
  /// Planar 4:4:4 YUV, 14-bit big-endian.
  #[display("yuv444p14be")]
  Yuv444p14Be = 147,
  /// Planar 4:4:4 YUV, 16-bit little-endian.
  #[display("yuv444p16le")]
  Yuv444p16Le = 148,
  /// Planar 4:4:4 YUV, 16-bit big-endian.
  #[display("yuv444p16be")]
  Yuv444p16Be = 149,

  // ===================================================================
  // Planar YUVA (with alpha)
  // ===================================================================
  /// Planar 4:2:0 YUVA, 8-bit.
  #[display("yuva420p")]
  Yuva420p = 200,
  /// Planar 4:2:2 YUVA, 8-bit.
  #[display("yuva422p")]
  Yuva422p = 201,
  /// Planar 4:4:4 YUVA, 8-bit.
  #[display("yuva444p")]
  Yuva444p = 202,
  /// Planar 4:2:0 YUVA, 9-bit little-endian.
  #[display("yuva420p9le")]
  Yuva420p9Le = 203,
  /// Planar 4:2:2 YUVA, 9-bit little-endian.
  #[display("yuva422p9le")]
  Yuva422p9Le = 204,
  /// Planar 4:4:4 YUVA, 9-bit little-endian.
  #[display("yuva444p9le")]
  Yuva444p9Le = 205,
  /// Planar 4:2:0 YUVA, 10-bit little-endian.
  #[display("yuva420p10le")]
  Yuva420p10Le = 206,
  /// Planar 4:2:2 YUVA, 10-bit little-endian.
  #[display("yuva422p10le")]
  Yuva422p10Le = 207,
  /// Planar 4:4:4 YUVA, 10-bit little-endian.
  #[display("yuva444p10le")]
  Yuva444p10Le = 208,
  /// Planar 4:2:2 YUVA, 12-bit little-endian.
  #[display("yuva422p12le")]
  Yuva422p12Le = 209,
  /// Planar 4:4:4 YUVA, 12-bit little-endian.
  #[display("yuva444p12le")]
  Yuva444p12Le = 210,
  /// Planar 4:4:4 YUVA, 14-bit little-endian.
  #[display("yuva444p14le")]
  Yuva444p14Le = 211,
  /// Planar 4:2:0 YUVA, 16-bit little-endian.
  #[display("yuva420p16le")]
  Yuva420p16Le = 212,
  /// Planar 4:2:2 YUVA, 16-bit little-endian.
  #[display("yuva422p16le")]
  Yuva422p16Le = 213,
  /// Planar 4:4:4 YUVA, 16-bit little-endian.
  #[display("yuva444p16le")]
  Yuva444p16Le = 214,

  // ===================================================================
  // Semi-planar YUV (NV-family) — 8-bit
  // ===================================================================
  /// 4:2:0 semi-planar Y plane + interleaved Cb/Cr (`AV_PIX_FMT_NV12`).
  #[display("nv12")]
  Nv12 = 300,
  /// 4:2:0 semi-planar Y + interleaved Cr/Cb (`AV_PIX_FMT_NV21`).
  #[display("nv21")]
  Nv21 = 301,
  /// 4:2:2 semi-planar Y + interleaved Cb/Cr.
  #[display("nv16")]
  Nv16 = 302,
  /// 4:4:4 semi-planar Y + interleaved Cb/Cr.
  #[display("nv24")]
  Nv24 = 303,
  /// 4:4:4 semi-planar Y + interleaved Cr/Cb.
  #[display("nv42")]
  Nv42 = 304,

  // ===================================================================
  // Semi-planar YUV high-bit-depth (P0xx / P2xx / P4xx)
  // ===================================================================
  /// 4:2:0 semi-planar 10-bit, little-endian (`AV_PIX_FMT_P010LE`).
  #[display("p010le")]
  P010Le = 310,
  /// 4:2:0 semi-planar 10-bit, big-endian.
  #[display("p010be")]
  P010Be = 311,
  /// 4:2:0 semi-planar 12-bit, little-endian.
  #[display("p012le")]
  P012Le = 312,
  /// 4:2:0 semi-planar 16-bit, little-endian.
  #[display("p016le")]
  P016Le = 313,
  /// 4:2:2 semi-planar 10-bit, little-endian.
  #[display("p210le")]
  P210Le = 314,
  /// 4:2:2 semi-planar 12-bit, little-endian (FFmpeg 5.1+).
  #[display("p212le")]
  P212Le = 315,
  /// 4:2:2 semi-planar 16-bit, little-endian.
  #[display("p216le")]
  P216Le = 316,
  /// 4:4:4 semi-planar 10-bit, little-endian.
  #[display("p410le")]
  P410Le = 317,
  /// 4:4:4 semi-planar 12-bit, little-endian (FFmpeg 5.1+).
  #[display("p412le")]
  P412Le = 318,
  /// 4:4:4 semi-planar 16-bit, little-endian.
  #[display("p416le")]
  P416Le = 319,

  // ===================================================================
  // Packed YUV 8-bit
  // ===================================================================
  /// 4:2:2 packed YUV: Y0 U Y1 V (`AV_PIX_FMT_YUYV422`).
  #[display("yuyv422")]
  Yuyv422 = 400,
  /// 4:2:2 packed YUV: U Y0 V Y1 (`AV_PIX_FMT_UYVY422`).
  #[display("uyvy422")]
  Uyvy422 = 401,
  /// 4:2:2 packed YUV: Y0 V Y1 U (`AV_PIX_FMT_YVYU422`).
  #[display("yvyu422")]
  Yvyu422 = 402,

  // ===================================================================
  // Packed YUV high-bit-depth
  // ===================================================================
  /// 4:2:2 packed YUV 10-bit (`AV_PIX_FMT_Y210LE`).
  #[display("y210le")]
  Y210Le = 410,
  /// 4:2:2 packed YUV 12-bit (`AV_PIX_FMT_Y212LE`).
  #[display("y212le")]
  Y212Le = 411,
  /// 4:2:2 packed YUV 16-bit (`AV_PIX_FMT_Y216LE`).
  #[display("y216le")]
  Y216Le = 412,
  /// 4:2:2 packed 10-bit, 3 samples per 32-bit word (`AV_PIX_FMT_V210`).
  #[display("v210")]
  V210 = 413,
  /// 4:4:4 packed 10-bit, one 32-bit word per sample (`AV_PIX_FMT_V410LE`).
  #[display("v410le")]
  V410Le = 414,
  /// 4:4:4 packed 10-bit, alternative layout.
  #[display("v30xle")]
  V30xLe = 415,
  /// 4:4:4 packed 12-bit, one 16-bit word per channel (`AV_PIX_FMT_XV36LE`).
  #[display("xv36le")]
  Xv36Le = 416,
  /// 4:4:4 packed 8-bit byte quadruple V, U, Y, A (`AV_PIX_FMT_VUYA`).
  #[display("vuya")]
  Vuya = 417,
  /// 4:4:4 packed 8-bit V, U, Y, X (alpha-as-padding).
  #[display("vuyx")]
  Vuyx = 418,
  /// 4:4:4 packed 16-bit word quadruple A, Y, U, V (`AV_PIX_FMT_AYUV64LE`).
  #[display("ayuv64le")]
  Ayuv64Le = 419,

  // ===================================================================
  // Packed RGB 8-bit
  // ===================================================================
  /// 24-bit packed RGB (`AV_PIX_FMT_RGB24`).
  #[display("rgb24")]
  Rgb24 = 500,
  /// 24-bit packed BGR.
  #[display("bgr24")]
  Bgr24 = 501,
  /// 32-bit packed RGBA.
  #[display("rgba")]
  Rgba = 502,
  /// 32-bit packed BGRA.
  #[display("bgra")]
  Bgra = 503,
  /// 32-bit packed ARGB.
  #[display("argb")]
  Argb = 504,
  /// 32-bit packed ABGR.
  #[display("abgr")]
  Abgr = 505,
  /// 32-bit packed RGB with X (unused) byte.
  #[display("rgbx")]
  Rgbx = 506,
  /// 32-bit packed BGR with X (unused) byte.
  #[display("bgrx")]
  Bgrx = 507,
  /// 32-bit packed XRGB (X unused, then RGB).
  #[display("xrgb")]
  Xrgb = 508,
  /// 32-bit packed XBGR.
  #[display("xbgr")]
  Xbgr = 509,
  /// 32-bit RGB10 in low bits, 2 bits unused (`AV_PIX_FMT_X2RGB10LE`).
  #[display("x2rgb10le")]
  X2Rgb10Le = 510,
  /// 32-bit BGR10 in low bits, 2 bits unused.
  #[display("x2bgr10le")]
  X2Bgr10Le = 511,

  // ===================================================================
  // Packed RGB low-bit
  // ===================================================================
  /// 16-bit packed RGB, 4 bits per channel + 4 unused.
  #[display("rgb444le")]
  Rgb444Le = 520,
  /// 16-bit packed BGR, 4 bits per channel + 4 unused.
  #[display("bgr444le")]
  Bgr444Le = 521,
  /// 16-bit packed RGB, 5/5/5 layout.
  #[display("rgb555le")]
  Rgb555Le = 522,
  /// 16-bit packed BGR, 5/5/5 layout.
  #[display("bgr555le")]
  Bgr555Le = 523,
  /// 16-bit packed RGB, 5/6/5 layout.
  #[display("rgb565le")]
  Rgb565Le = 524,
  /// 16-bit packed BGR, 5/6/5 layout.
  #[display("bgr565le")]
  Bgr565Le = 525,

  // ===================================================================
  // Packed RGB high-bit-depth
  // ===================================================================
  /// 48-bit packed RGB, 16 bits per channel, little-endian.
  #[display("rgb48le")]
  Rgb48Le = 530,
  /// 48-bit packed BGR, 16 bits per channel, little-endian.
  #[display("bgr48le")]
  Bgr48Le = 531,
  /// 64-bit packed RGBA, 16 bits per channel.
  #[display("rgba64le")]
  Rgba64Le = 532,
  /// 64-bit packed BGRA, 16 bits per channel.
  #[display("bgra64le")]
  Bgra64Le = 533,

  // ===================================================================
  // Packed RGB float
  // ===================================================================
  /// 48-bit packed RGB, 16-bit half-float per channel.
  #[display("rgbf16")]
  Rgbf16 = 540,
  /// 96-bit packed RGB, 32-bit float per channel.
  #[display("rgbf32")]
  Rgbf32 = 541,

  // ===================================================================
  // Planar GBR 8-bit
  // ===================================================================
  /// Planar 4:4:4 G/B/R, 8-bit.
  #[display("gbrp")]
  Gbrp = 600,
  /// Planar 4:4:4 G/B/R, 9-bit little-endian.
  #[display("gbrp9le")]
  Gbrp9Le = 601,
  /// Planar 4:4:4 G/B/R, 10-bit little-endian.
  #[display("gbrp10le")]
  Gbrp10Le = 602,
  /// Planar 4:4:4 G/B/R, 12-bit little-endian.
  #[display("gbrp12le")]
  Gbrp12Le = 603,
  /// Planar 4:4:4 G/B/R, 14-bit little-endian.
  #[display("gbrp14le")]
  Gbrp14Le = 604,
  /// Planar 4:4:4 G/B/R, 16-bit little-endian.
  #[display("gbrp16le")]
  Gbrp16Le = 605,
  /// Planar 4:4:4 G/B/R, 16-bit half-float.
  #[display("gbrpf16")]
  Gbrpf16 = 606,
  /// Planar 4:4:4 G/B/R, 32-bit float.
  #[display("gbrpf32")]
  Gbrpf32 = 607,

  // ===================================================================
  // Planar GBRA (with alpha)
  // ===================================================================
  /// Planar 4:4:4 G/B/R/A, 8-bit.
  #[display("gbrap")]
  Gbrap = 620,
  /// Planar 4:4:4 G/B/R/A, 10-bit little-endian.
  #[display("gbrap10le")]
  Gbrap10Le = 621,
  /// Planar 4:4:4 G/B/R/A, 12-bit little-endian.
  #[display("gbrap12le")]
  Gbrap12Le = 622,
  /// Planar 4:4:4 G/B/R/A, 14-bit little-endian.
  #[display("gbrap14le")]
  Gbrap14Le = 623,
  /// Planar 4:4:4 G/B/R/A, 16-bit little-endian.
  #[display("gbrap16le")]
  Gbrap16Le = 624,
  /// Planar 4:4:4 G/B/R/A, 16-bit half-float.
  #[display("gbrapf16")]
  Gbrapf16 = 625,
  /// Planar 4:4:4 G/B/R/A, 32-bit float.
  #[display("gbrapf32")]
  Gbrapf32 = 626,

  // ===================================================================
  // Greyscale
  // ===================================================================
  /// 8-bit greyscale (`AV_PIX_FMT_GRAY8`).
  #[display("gray8")]
  Gray8 = 700,
  /// 9-bit greyscale, little-endian.
  #[display("gray9le")]
  Gray9Le = 701,
  /// 10-bit greyscale, little-endian.
  #[display("gray10le")]
  Gray10Le = 702,
  /// 12-bit greyscale, little-endian.
  #[display("gray12le")]
  Gray12Le = 703,
  /// 14-bit greyscale, little-endian.
  #[display("gray14le")]
  Gray14Le = 704,
  /// 16-bit greyscale, little-endian.
  #[display("gray16le")]
  Gray16Le = 705,
  /// 32-bit float greyscale.
  #[display("grayf32")]
  Grayf32 = 706,
  /// 16-bit greyscale-with-alpha.
  #[display("ya8")]
  Ya8 = 710,
  /// 32-bit greyscale-with-alpha.
  #[display("ya16le")]
  Ya16Le = 711,

  // ===================================================================
  // Monochrome 1-bit
  // ===================================================================
  /// 1-bit monochrome, white = 0 (`AV_PIX_FMT_MONOWHITE`).
  #[display("monowhite")]
  Monowhite = 720,
  /// 1-bit monochrome, black = 0 (`AV_PIX_FMT_MONOBLACK`).
  #[display("monoblack")]
  Monoblack = 721,

  // ===================================================================
  // Paletted
  // ===================================================================
  /// Paletted 8-bit (`AV_PIX_FMT_PAL8`).
  #[display("pal8")]
  Pal8 = 800,

  // ===================================================================
  // Bayer
  // ===================================================================
  /// Bayer BGGR pattern, 8-bit.
  #[display("bayer_bggr8")]
  BayerBggr8 = 900,
  /// Bayer RGGB pattern, 8-bit.
  #[display("bayer_rggb8")]
  BayerRggb8 = 901,
  /// Bayer GBRG pattern, 8-bit.
  #[display("bayer_gbrg8")]
  BayerGbrg8 = 902,
  /// Bayer GRBG pattern, 8-bit.
  #[display("bayer_grbg8")]
  BayerGrbg8 = 903,
  /// Bayer BGGR pattern, 10-bit little-endian (low-packed in u16).
  #[display("bayer_bggr10le")]
  BayerBggr10Le = 910,
  /// Bayer RGGB pattern, 10-bit little-endian.
  #[display("bayer_rggb10le")]
  BayerRggb10Le = 911,
  /// Bayer GBRG pattern, 10-bit little-endian.
  #[display("bayer_gbrg10le")]
  BayerGbrg10Le = 912,
  /// Bayer GRBG pattern, 10-bit little-endian.
  #[display("bayer_grbg10le")]
  BayerGrbg10Le = 913,
  /// Bayer BGGR pattern, 12-bit little-endian.
  #[display("bayer_bggr12le")]
  BayerBggr12Le = 920,
  /// Bayer RGGB pattern, 12-bit little-endian.
  #[display("bayer_rggb12le")]
  BayerRggb12Le = 921,
  /// Bayer GBRG pattern, 12-bit little-endian.
  #[display("bayer_gbrg12le")]
  BayerGbrg12Le = 922,
  /// Bayer GRBG pattern, 12-bit little-endian.
  #[display("bayer_grbg12le")]
  BayerGrbg12Le = 923,
  /// Bayer BGGR pattern, 14-bit little-endian.
  #[display("bayer_bggr14le")]
  BayerBggr14Le = 930,
  /// Bayer RGGB pattern, 14-bit little-endian.
  #[display("bayer_rggb14le")]
  BayerRggb14Le = 931,
  /// Bayer GBRG pattern, 14-bit little-endian.
  #[display("bayer_gbrg14le")]
  BayerGbrg14Le = 932,
  /// Bayer GRBG pattern, 14-bit little-endian.
  #[display("bayer_grbg14le")]
  BayerGrbg14Le = 933,
  /// Bayer BGGR pattern, 16-bit little-endian.
  #[display("bayer_bggr16le")]
  BayerBggr16Le = 940,
  /// Bayer RGGB pattern, 16-bit little-endian.
  #[display("bayer_rggb16le")]
  BayerRggb16Le = 941,
  /// Bayer GBRG pattern, 16-bit little-endian.
  #[display("bayer_gbrg16le")]
  BayerGbrg16Le = 942,
  /// Bayer GRBG pattern, 16-bit little-endian.
  #[display("bayer_grbg16le")]
  BayerGrbg16Le = 943,
}

impl Default for PixelFormat {
  #[inline]
  fn default() -> Self {
    Self::Unknown
  }
}

impl PixelFormat {
  /// Stable wire representation. Returns the underlying `repr(u32)`
  /// discriminant.
  #[inline]
  pub const fn to_u32(self) -> u32 {
    self as u32
  }

  /// Decodes from the stable `u32` representation produced by
  /// [`Self::to_u32`]. Unrecognised values map to [`Self::Unknown`].
  #[inline]
  pub const fn from_u32(value: u32) -> Self {
    match value {
      // Planar YUV 8-bit.
      100 => Self::Yuv420p,
      101 => Self::Yuv422p,
      102 => Self::Yuv440p,
      103 => Self::Yuv444p,
      104 => Self::Yuv411p,
      105 => Self::Yuv410p,
      // Planar YUV high-bit-depth (4:2:0).
      110 => Self::Yuv420p9Le,
      111 => Self::Yuv420p9Be,
      112 => Self::Yuv420p10Le,
      113 => Self::Yuv420p10Be,
      114 => Self::Yuv420p12Le,
      115 => Self::Yuv420p12Be,
      116 => Self::Yuv420p14Le,
      117 => Self::Yuv420p14Be,
      118 => Self::Yuv420p16Le,
      119 => Self::Yuv420p16Be,
      // Planar YUV high-bit-depth (4:2:2).
      120 => Self::Yuv422p9Le,
      121 => Self::Yuv422p9Be,
      122 => Self::Yuv422p10Le,
      123 => Self::Yuv422p10Be,
      124 => Self::Yuv422p12Le,
      125 => Self::Yuv422p12Be,
      126 => Self::Yuv422p14Le,
      127 => Self::Yuv422p14Be,
      128 => Self::Yuv422p16Le,
      129 => Self::Yuv422p16Be,
      // Planar YUV (4:4:0).
      130 => Self::Yuv440p10Le,
      131 => Self::Yuv440p12Le,
      // Planar YUV high-bit-depth (4:4:4).
      140 => Self::Yuv444p9Le,
      141 => Self::Yuv444p9Be,
      142 => Self::Yuv444p10Le,
      143 => Self::Yuv444p10Be,
      144 => Self::Yuv444p12Le,
      145 => Self::Yuv444p12Be,
      146 => Self::Yuv444p14Le,
      147 => Self::Yuv444p14Be,
      148 => Self::Yuv444p16Le,
      149 => Self::Yuv444p16Be,
      // Planar YUVA.
      200 => Self::Yuva420p,
      201 => Self::Yuva422p,
      202 => Self::Yuva444p,
      203 => Self::Yuva420p9Le,
      204 => Self::Yuva422p9Le,
      205 => Self::Yuva444p9Le,
      206 => Self::Yuva420p10Le,
      207 => Self::Yuva422p10Le,
      208 => Self::Yuva444p10Le,
      209 => Self::Yuva422p12Le,
      210 => Self::Yuva444p12Le,
      211 => Self::Yuva444p14Le,
      212 => Self::Yuva420p16Le,
      213 => Self::Yuva422p16Le,
      214 => Self::Yuva444p16Le,
      // Semi-planar YUV.
      300 => Self::Nv12,
      301 => Self::Nv21,
      302 => Self::Nv16,
      303 => Self::Nv24,
      304 => Self::Nv42,
      // Semi-planar YUV high-bit-depth.
      310 => Self::P010Le,
      311 => Self::P010Be,
      312 => Self::P012Le,
      313 => Self::P016Le,
      314 => Self::P210Le,
      315 => Self::P212Le,
      316 => Self::P216Le,
      317 => Self::P410Le,
      318 => Self::P412Le,
      319 => Self::P416Le,
      // Packed YUV 8-bit.
      400 => Self::Yuyv422,
      401 => Self::Uyvy422,
      402 => Self::Yvyu422,
      // Packed YUV high-bit-depth.
      410 => Self::Y210Le,
      411 => Self::Y212Le,
      412 => Self::Y216Le,
      413 => Self::V210,
      414 => Self::V410Le,
      415 => Self::V30xLe,
      416 => Self::Xv36Le,
      417 => Self::Vuya,
      418 => Self::Vuyx,
      419 => Self::Ayuv64Le,
      // Packed RGB 8-bit.
      500 => Self::Rgb24,
      501 => Self::Bgr24,
      502 => Self::Rgba,
      503 => Self::Bgra,
      504 => Self::Argb,
      505 => Self::Abgr,
      506 => Self::Rgbx,
      507 => Self::Bgrx,
      508 => Self::Xrgb,
      509 => Self::Xbgr,
      510 => Self::X2Rgb10Le,
      511 => Self::X2Bgr10Le,
      // Packed RGB low-bit.
      520 => Self::Rgb444Le,
      521 => Self::Bgr444Le,
      522 => Self::Rgb555Le,
      523 => Self::Bgr555Le,
      524 => Self::Rgb565Le,
      525 => Self::Bgr565Le,
      // Packed RGB high-bit.
      530 => Self::Rgb48Le,
      531 => Self::Bgr48Le,
      532 => Self::Rgba64Le,
      533 => Self::Bgra64Le,
      // Packed RGB float.
      540 => Self::Rgbf16,
      541 => Self::Rgbf32,
      // Planar GBR.
      600 => Self::Gbrp,
      601 => Self::Gbrp9Le,
      602 => Self::Gbrp10Le,
      603 => Self::Gbrp12Le,
      604 => Self::Gbrp14Le,
      605 => Self::Gbrp16Le,
      606 => Self::Gbrpf16,
      607 => Self::Gbrpf32,
      // Planar GBRA.
      620 => Self::Gbrap,
      621 => Self::Gbrap10Le,
      622 => Self::Gbrap12Le,
      623 => Self::Gbrap14Le,
      624 => Self::Gbrap16Le,
      625 => Self::Gbrapf16,
      626 => Self::Gbrapf32,
      // Greyscale.
      700 => Self::Gray8,
      701 => Self::Gray9Le,
      702 => Self::Gray10Le,
      703 => Self::Gray12Le,
      704 => Self::Gray14Le,
      705 => Self::Gray16Le,
      706 => Self::Grayf32,
      710 => Self::Ya8,
      711 => Self::Ya16Le,
      // Monochrome.
      720 => Self::Monowhite,
      721 => Self::Monoblack,
      // Paletted.
      800 => Self::Pal8,
      // Bayer.
      900 => Self::BayerBggr8,
      901 => Self::BayerRggb8,
      902 => Self::BayerGbrg8,
      903 => Self::BayerGrbg8,
      910 => Self::BayerBggr10Le,
      911 => Self::BayerRggb10Le,
      912 => Self::BayerGbrg10Le,
      913 => Self::BayerGrbg10Le,
      920 => Self::BayerBggr12Le,
      921 => Self::BayerRggb12Le,
      922 => Self::BayerGbrg12Le,
      923 => Self::BayerGrbg12Le,
      930 => Self::BayerBggr14Le,
      931 => Self::BayerRggb14Le,
      932 => Self::BayerGbrg14Le,
      933 => Self::BayerGrbg14Le,
      940 => Self::BayerBggr16Le,
      941 => Self::BayerRggb16Le,
      942 => Self::BayerGbrg16Le,
      943 => Self::BayerGrbg16Le,
      _ => Self::Unknown,
    }
  }

  /// Returns `true` for Bayer-mosaic formats (any pattern, any bit
  /// depth). Bayer frames carry undebayered sensor data; downstream
  /// consumers (e.g. `colconv::raw`) demosaic + white-balance + colour-
  /// correct to produce RGB.
  #[inline]
  pub const fn is_bayer(self) -> bool {
    matches!(
      self,
      Self::BayerBggr8
        | Self::BayerRggb8
        | Self::BayerGbrg8
        | Self::BayerGrbg8
        | Self::BayerBggr10Le
        | Self::BayerRggb10Le
        | Self::BayerGbrg10Le
        | Self::BayerGrbg10Le
        | Self::BayerBggr12Le
        | Self::BayerRggb12Le
        | Self::BayerGbrg12Le
        | Self::BayerGrbg12Le
        | Self::BayerBggr14Le
        | Self::BayerRggb14Le
        | Self::BayerGbrg14Le
        | Self::BayerGrbg14Le
        | Self::BayerBggr16Le
        | Self::BayerRggb16Le
        | Self::BayerGbrg16Le
        | Self::BayerGrbg16Le,
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_is_unknown() {
    assert!(matches!(PixelFormat::default(), PixelFormat::Unknown));
  }

  #[test]
  fn round_trip_u32_for_known_variants() {
    let all = [
      PixelFormat::Unknown,
      PixelFormat::Yuv420p,
      PixelFormat::Yuv444p,
      PixelFormat::Yuv420p10Le,
      PixelFormat::Yuv422p16Le,
      PixelFormat::Yuva444p,
      PixelFormat::Nv12,
      PixelFormat::P010Le,
      PixelFormat::P416Le,
      PixelFormat::Yuyv422,
      PixelFormat::V210,
      PixelFormat::Ayuv64Le,
      PixelFormat::Rgb24,
      PixelFormat::Bgra,
      PixelFormat::Rgb565Le,
      PixelFormat::Rgba64Le,
      PixelFormat::Rgbf32,
      PixelFormat::Gbrp,
      PixelFormat::Gbrap16Le,
      PixelFormat::Gbrapf32,
      PixelFormat::Gray8,
      PixelFormat::Gray16Le,
      PixelFormat::Ya16Le,
      PixelFormat::Monowhite,
      PixelFormat::Pal8,
      PixelFormat::BayerBggr8,
      PixelFormat::BayerRggb16Le,
    ];
    for fmt in all {
      assert_eq!(
        PixelFormat::from_u32(fmt.to_u32()),
        fmt,
        "round-trip failed for {fmt:?}"
      );
    }
  }

  #[test]
  fn unknown_for_garbage_u32() {
    assert_eq!(PixelFormat::from_u32(99_999), PixelFormat::Unknown);
    assert_eq!(PixelFormat::from_u32(1), PixelFormat::Unknown);
  }

  // `format!` requires an allocator; gate to alloc-or-std builds.
  // The `Display` impl itself works in bare-core mode via
  // `write!`-style sinks — only this test's assertion strategy needs
  // alloc.
  #[cfg(any(feature = "alloc", feature = "std"))]
  #[test]
  fn display_uses_ffmpeg_lowercase_names() {
    assert_eq!(format!("{}", PixelFormat::Yuv420p), "yuv420p");
    assert_eq!(format!("{}", PixelFormat::Nv12), "nv12");
    assert_eq!(format!("{}", PixelFormat::P010Le), "p010le");
    assert_eq!(format!("{}", PixelFormat::Rgba64Le), "rgba64le");
    assert_eq!(format!("{}", PixelFormat::BayerBggr12Le), "bayer_bggr12le");
    assert_eq!(format!("{}", PixelFormat::Unknown), "unknown");
  }

  #[test]
  fn is_bayer_partition() {
    assert!(PixelFormat::BayerBggr8.is_bayer());
    assert!(PixelFormat::BayerRggb16Le.is_bayer());
    assert!(PixelFormat::BayerGrbg12Le.is_bayer());
    assert!(!PixelFormat::Yuv420p.is_bayer());
    assert!(!PixelFormat::Rgb24.is_bayer());
    assert!(!PixelFormat::Unknown.is_bayer());
  }

  #[test]
  fn is_variant_helpers_compile() {
    assert!(PixelFormat::Yuv420p.is_yuv_420_p());
    assert!(PixelFormat::Nv12.is_nv_12());
    assert!(PixelFormat::P010Le.is_p_010_le());
    assert!(!PixelFormat::Yuv420p.is_unknown());
  }

  #[test]
  fn copy_and_eq() {
    let p = PixelFormat::Nv12;
    let q = p; // Copy
    assert_eq!(p, q);
    assert_ne!(p, PixelFormat::Yuv420p);
  }
}

//! Boundary conversions between FFmpeg's bindgen integers and the
//! unified [`mediadecode`] vocabulary.
//!
//! Centralised so the rest of the crate never compares raw
//! `AVPixelFormat` integers against literals or transmutes back into
//! the bindgen enum (UB hazard when the value isn't in the enum's
//! discriminant set).

use core::ffi::c_int;

use ffmpeg_next::{Packet, ffi::AVPixelFormat};
use mediadecode::{
  PixelFormat, Timestamp,
  channel::AudioChannelLayout,
  frame::{AudioFrame, Dimensions, Plane, SubtitleFrame, VideoFrame},
  packet::{AudioPacket, PacketFlags as MdPacketFlags, SubtitlePacket, VideoPacket},
  subtitle::SubtitlePayload,
};

use crate::{
  FfmpegBuffer,
  extras::{
    AudioFrameExtra, AudioPacketExtra, SubtitleFrameExtra, SubtitlePacketExtra, VideoFrameExtra,
    VideoPacketExtra,
  },
  sample_format::SampleFormat,
};

/// Maps a raw `AVFrame.format` integer (i.e. the value of an
/// `AVPixelFormat` enum variant) onto [`mediadecode::PixelFormat`].
///
/// Returns [`PixelFormat::Unknown`] for raw integers we don't have a
/// mapping for — including hardware-frame markers
/// (`AV_PIX_FMT_VIDEOTOOLBOX` / `_VAAPI` / `_CUDA` / `_D3D11` / …)
/// since those never describe CPU-side pixel data and the unified
/// enum intentionally doesn't carry them. Use [`is_hardware_pix_fmt`]
/// to identify HW frames before transferring to a CPU format.
///
/// The match never constructs an `AVPixelFormat` from a runtime
/// value; it compares the input against `AVPixelFormat::AV_PIX_FMT_X
/// as i32` constants. Sound regardless of which discriminant set the
/// linked FFmpeg version exposes.
pub const fn from_av_pixel_format(raw: i32) -> PixelFormat {
  // Mirrors `crate::pixdesc::to_av_pixel_format` arm-for-arm (its
  // inverse). Every deliverable CPU format plus the non-deliverable
  // formats `to_av` still resolves a constant for (monochrome / PAL /
  // sub-byte-packed RGB / Bayer) is mapped here, so a frame's raw
  // `format` integer always lands on the same `PixelFormat` the round
  // trip would produce. Deliverability (HWACCEL / BAYER / PAL /
  // BITSTREAM rejection) is enforced separately by
  // `pixdesc::is_deliverable` / the convert layer — this boundary is
  // identity-only.
  //
  // BE-tagged formats map to mediadecode's distinct `*Be` variants
  // (never folded onto the LE canonical). Folding BE onto LE silently
  // corrupted pixel data: each >8-bit sample is byte-swapped between
  // BE and LE, and the convert path exports the AVBufferRef bytes
  // verbatim with no endian conversion, so a consumer reading a
  // BE-tagged frame's planes as LE samples would see every sample
  // byte-reversed. Mapping to the `*Be` variant keeps the format
  // distinct so the convert layer can handle (or reject) it correctly.
  //
  // The match never constructs an `AVPixelFormat` from a runtime
  // value; it compares the input against `AVPixelFormat::AV_PIX_FMT_X
  // as i32` constants. Sound regardless of which discriminant set the
  // linked FFmpeg version exposes.
  match raw {
    // Planar YUV 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P as i32 => PixelFormat::Yuv420p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P as i32 => PixelFormat::Yuv422p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV440P as i32 => PixelFormat::Yuv440p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P as i32 => PixelFormat::Yuv444p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV411P as i32 => PixelFormat::Yuv411p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV410P as i32 => PixelFormat::Yuv410p,
    // Deprecated JPEG-range planar YUV (yuvj-family).
    x if x == AVPixelFormat::AV_PIX_FMT_YUVJ411P as i32 => PixelFormat::Yuvj411p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVJ420P as i32 => PixelFormat::Yuvj420p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVJ422P as i32 => PixelFormat::Yuvj422p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVJ440P as i32 => PixelFormat::Yuvj440p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVJ444P as i32 => PixelFormat::Yuvj444p,
    // Planar YUV 4:2:0 high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P9LE as i32 => PixelFormat::Yuv420p9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P9BE as i32 => PixelFormat::Yuv420p9Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P10LE as i32 => PixelFormat::Yuv420p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P10BE as i32 => PixelFormat::Yuv420p10Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P12LE as i32 => PixelFormat::Yuv420p12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P12BE as i32 => PixelFormat::Yuv420p12Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P14LE as i32 => PixelFormat::Yuv420p14Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P14BE as i32 => PixelFormat::Yuv420p14Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P16LE as i32 => PixelFormat::Yuv420p16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P16BE as i32 => PixelFormat::Yuv420p16Be,
    // Planar YUV 4:2:2 high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P9LE as i32 => PixelFormat::Yuv422p9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P9BE as i32 => PixelFormat::Yuv422p9Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P10LE as i32 => PixelFormat::Yuv422p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P10BE as i32 => PixelFormat::Yuv422p10Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P12LE as i32 => PixelFormat::Yuv422p12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P12BE as i32 => PixelFormat::Yuv422p12Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P14LE as i32 => PixelFormat::Yuv422p14Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P14BE as i32 => PixelFormat::Yuv422p14Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P16LE as i32 => PixelFormat::Yuv422p16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P16BE as i32 => PixelFormat::Yuv422p16Be,
    // Planar YUV 4:4:0 high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV440P10LE as i32 => PixelFormat::Yuv440p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV440P10BE as i32 => PixelFormat::Yuv440p10Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV440P12LE as i32 => PixelFormat::Yuv440p12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV440P12BE as i32 => PixelFormat::Yuv440p12Be,
    // Planar YUV 4:4:4 high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P9LE as i32 => PixelFormat::Yuv444p9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P9BE as i32 => PixelFormat::Yuv444p9Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P10LE as i32 => PixelFormat::Yuv444p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P10BE as i32 => PixelFormat::Yuv444p10Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P12LE as i32 => PixelFormat::Yuv444p12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P12BE as i32 => PixelFormat::Yuv444p12Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P14LE as i32 => PixelFormat::Yuv444p14Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P14BE as i32 => PixelFormat::Yuv444p14Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P16LE as i32 => PixelFormat::Yuv444p16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P16BE as i32 => PixelFormat::Yuv444p16Be,
    // MSB-packed YUV 4:4:4.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P10MSBLE as i32 => PixelFormat::Yuv444p10MsbLe,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P10MSBBE as i32 => PixelFormat::Yuv444p10MsbBe,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P12MSBLE as i32 => PixelFormat::Yuv444p12MsbLe,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P12MSBBE as i32 => PixelFormat::Yuv444p12MsbBe,
    // Planar YUVA.
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA420P as i32 => PixelFormat::Yuva420p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA422P as i32 => PixelFormat::Yuva422p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA444P as i32 => PixelFormat::Yuva444p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA420P9LE as i32 => PixelFormat::Yuva420p9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA420P9BE as i32 => PixelFormat::Yuva420p9Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA422P9LE as i32 => PixelFormat::Yuva422p9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA422P9BE as i32 => PixelFormat::Yuva422p9Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA444P9LE as i32 => PixelFormat::Yuva444p9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA444P9BE as i32 => PixelFormat::Yuva444p9Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA420P10LE as i32 => PixelFormat::Yuva420p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA420P10BE as i32 => PixelFormat::Yuva420p10Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA422P10LE as i32 => PixelFormat::Yuva422p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA422P10BE as i32 => PixelFormat::Yuva422p10Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA444P10LE as i32 => PixelFormat::Yuva444p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA444P10BE as i32 => PixelFormat::Yuva444p10Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA422P12LE as i32 => PixelFormat::Yuva422p12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA422P12BE as i32 => PixelFormat::Yuva422p12Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA444P12LE as i32 => PixelFormat::Yuva444p12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA444P12BE as i32 => PixelFormat::Yuva444p12Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA420P16LE as i32 => PixelFormat::Yuva420p16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA420P16BE as i32 => PixelFormat::Yuva420p16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA422P16LE as i32 => PixelFormat::Yuva422p16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA422P16BE as i32 => PixelFormat::Yuva422p16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA444P16LE as i32 => PixelFormat::Yuva444p16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA444P16BE as i32 => PixelFormat::Yuva444p16Be,
    // Semi-planar YUV 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_NV12 as i32 => PixelFormat::Nv12,
    x if x == AVPixelFormat::AV_PIX_FMT_NV21 as i32 => PixelFormat::Nv21,
    x if x == AVPixelFormat::AV_PIX_FMT_NV16 as i32 => PixelFormat::Nv16,
    x if x == AVPixelFormat::AV_PIX_FMT_NV24 as i32 => PixelFormat::Nv24,
    x if x == AVPixelFormat::AV_PIX_FMT_NV42 as i32 => PixelFormat::Nv42,
    x if x == AVPixelFormat::AV_PIX_FMT_NV20LE as i32 => PixelFormat::Nv20Le,
    x if x == AVPixelFormat::AV_PIX_FMT_NV20BE as i32 => PixelFormat::Nv20Be,
    // Semi-planar YUV high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_P010LE as i32 => PixelFormat::P010Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P010BE as i32 => PixelFormat::P010Be,
    x if x == AVPixelFormat::AV_PIX_FMT_P012LE as i32 => PixelFormat::P012Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P012BE as i32 => PixelFormat::P012Be,
    x if x == AVPixelFormat::AV_PIX_FMT_P016LE as i32 => PixelFormat::P016Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P016BE as i32 => PixelFormat::P016Be,
    x if x == AVPixelFormat::AV_PIX_FMT_P210LE as i32 => PixelFormat::P210Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P210BE as i32 => PixelFormat::P210Be,
    x if x == AVPixelFormat::AV_PIX_FMT_P212LE as i32 => PixelFormat::P212Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P212BE as i32 => PixelFormat::P212Be,
    x if x == AVPixelFormat::AV_PIX_FMT_P216LE as i32 => PixelFormat::P216Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P216BE as i32 => PixelFormat::P216Be,
    x if x == AVPixelFormat::AV_PIX_FMT_P410LE as i32 => PixelFormat::P410Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P410BE as i32 => PixelFormat::P410Be,
    x if x == AVPixelFormat::AV_PIX_FMT_P412LE as i32 => PixelFormat::P412Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P412BE as i32 => PixelFormat::P412Be,
    x if x == AVPixelFormat::AV_PIX_FMT_P416LE as i32 => PixelFormat::P416Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P416BE as i32 => PixelFormat::P416Be,
    // Packed YUV 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUYV422 as i32 => PixelFormat::Yuyv422,
    x if x == AVPixelFormat::AV_PIX_FMT_UYVY422 as i32 => PixelFormat::Uyvy422,
    x if x == AVPixelFormat::AV_PIX_FMT_YVYU422 as i32 => PixelFormat::Yvyu422,
    x if x == AVPixelFormat::AV_PIX_FMT_UYYVYY411 as i32 => PixelFormat::Uyyvyy411,
    // Packed YUV high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_Y210LE as i32 => PixelFormat::Y210Le,
    x if x == AVPixelFormat::AV_PIX_FMT_Y210BE as i32 => PixelFormat::Y210Be,
    x if x == AVPixelFormat::AV_PIX_FMT_Y212LE as i32 => PixelFormat::Y212Le,
    x if x == AVPixelFormat::AV_PIX_FMT_Y212BE as i32 => PixelFormat::Y212Be,
    x if x == AVPixelFormat::AV_PIX_FMT_Y216LE as i32 => PixelFormat::Y216Le,
    x if x == AVPixelFormat::AV_PIX_FMT_Y216BE as i32 => PixelFormat::Y216Be,
    x if x == AVPixelFormat::AV_PIX_FMT_XV30LE as i32 => PixelFormat::Xv30Le,
    x if x == AVPixelFormat::AV_PIX_FMT_XV30BE as i32 => PixelFormat::Xv30Be,
    x if x == AVPixelFormat::AV_PIX_FMT_V30XLE as i32 => PixelFormat::V30xLe,
    x if x == AVPixelFormat::AV_PIX_FMT_V30XBE as i32 => PixelFormat::V30xBe,
    x if x == AVPixelFormat::AV_PIX_FMT_XV36LE as i32 => PixelFormat::Xv36Le,
    x if x == AVPixelFormat::AV_PIX_FMT_XV36BE as i32 => PixelFormat::Xv36Be,
    x if x == AVPixelFormat::AV_PIX_FMT_XV48LE as i32 => PixelFormat::Xv48Le,
    x if x == AVPixelFormat::AV_PIX_FMT_XV48BE as i32 => PixelFormat::Xv48Be,
    x if x == AVPixelFormat::AV_PIX_FMT_VUYA as i32 => PixelFormat::Vuya,
    x if x == AVPixelFormat::AV_PIX_FMT_VUYX as i32 => PixelFormat::Vuyx,
    x if x == AVPixelFormat::AV_PIX_FMT_AYUV as i32 => PixelFormat::Ayuv,
    x if x == AVPixelFormat::AV_PIX_FMT_AYUV64LE as i32 => PixelFormat::Ayuv64Le,
    x if x == AVPixelFormat::AV_PIX_FMT_AYUV64BE as i32 => PixelFormat::Ayuv64Be,
    x if x == AVPixelFormat::AV_PIX_FMT_UYVA as i32 => PixelFormat::Uyva,
    x if x == AVPixelFormat::AV_PIX_FMT_VYU444 as i32 => PixelFormat::Vyu444,
    // XYZ.
    x if x == AVPixelFormat::AV_PIX_FMT_XYZ12LE as i32 => PixelFormat::Xyz12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_XYZ12BE as i32 => PixelFormat::Xyz12Be,
    // Packed RGB 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_RGB24 as i32 => PixelFormat::Rgb24,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR24 as i32 => PixelFormat::Bgr24,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBA as i32 => PixelFormat::Rgba,
    x if x == AVPixelFormat::AV_PIX_FMT_BGRA as i32 => PixelFormat::Bgra,
    x if x == AVPixelFormat::AV_PIX_FMT_ARGB as i32 => PixelFormat::Argb,
    x if x == AVPixelFormat::AV_PIX_FMT_ABGR as i32 => PixelFormat::Abgr,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB0 as i32 => PixelFormat::Rgbx,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR0 as i32 => PixelFormat::Bgrx,
    x if x == AVPixelFormat::AV_PIX_FMT_0RGB as i32 => PixelFormat::Xrgb,
    x if x == AVPixelFormat::AV_PIX_FMT_0BGR as i32 => PixelFormat::Xbgr,
    x if x == AVPixelFormat::AV_PIX_FMT_X2RGB10LE as i32 => PixelFormat::X2Rgb10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_X2RGB10BE as i32 => PixelFormat::X2Rgb10Be,
    x if x == AVPixelFormat::AV_PIX_FMT_X2BGR10LE as i32 => PixelFormat::X2Bgr10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_X2BGR10BE as i32 => PixelFormat::X2Bgr10Be,
    // Gbr24p shares AV_PIX_FMT_GBRP's discriminant; mapped to Gbrp above.
    // Packed RGB high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_RGB48LE as i32 => PixelFormat::Rgb48Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB48BE as i32 => PixelFormat::Rgb48Be,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR48LE as i32 => PixelFormat::Bgr48Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR48BE as i32 => PixelFormat::Bgr48Be,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBA64LE as i32 => PixelFormat::Rgba64Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBA64BE as i32 => PixelFormat::Rgba64Be,
    x if x == AVPixelFormat::AV_PIX_FMT_BGRA64LE as i32 => PixelFormat::Bgra64Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BGRA64BE as i32 => PixelFormat::Bgra64Be,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB96LE as i32 => PixelFormat::Rgb96Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB96BE as i32 => PixelFormat::Rgb96Be,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBA128LE as i32 => PixelFormat::Rgba128Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBA128BE as i32 => PixelFormat::Rgba128Be,
    // Packed RGB float / half-float.
    x if x == AVPixelFormat::AV_PIX_FMT_RGBF16LE as i32 => PixelFormat::Rgbf16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBF16BE as i32 => PixelFormat::Rgbf16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBF32LE as i32 => PixelFormat::Rgbf32Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBF32BE as i32 => PixelFormat::Rgbf32Be,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBAF16LE as i32 => PixelFormat::Rgbaf16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBAF16BE as i32 => PixelFormat::Rgbaf16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBAF32LE as i32 => PixelFormat::Rgbaf32Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBAF32BE as i32 => PixelFormat::Rgbaf32Be,
    // Planar GBR.
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP as i32 => PixelFormat::Gbrp,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP9LE as i32 => PixelFormat::Gbrp9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP9BE as i32 => PixelFormat::Gbrp9Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP10LE as i32 => PixelFormat::Gbrp10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP10BE as i32 => PixelFormat::Gbrp10Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP10MSBLE as i32 => PixelFormat::Gbrp10MsbLe,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP10MSBBE as i32 => PixelFormat::Gbrp10MsbBe,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP12LE as i32 => PixelFormat::Gbrp12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP12BE as i32 => PixelFormat::Gbrp12Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP12MSBLE as i32 => PixelFormat::Gbrp12MsbLe,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP12MSBBE as i32 => PixelFormat::Gbrp12MsbBe,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP14LE as i32 => PixelFormat::Gbrp14Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP14BE as i32 => PixelFormat::Gbrp14Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP16LE as i32 => PixelFormat::Gbrp16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRP16BE as i32 => PixelFormat::Gbrp16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRPF16LE as i32 => PixelFormat::Gbrpf16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRPF16BE as i32 => PixelFormat::Gbrpf16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRPF32LE as i32 => PixelFormat::Gbrpf32Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRPF32BE as i32 => PixelFormat::Gbrpf32Be,
    // Planar GBRA.
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAP as i32 => PixelFormat::Gbrap,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAP10LE as i32 => PixelFormat::Gbrap10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAP10BE as i32 => PixelFormat::Gbrap10Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAP12LE as i32 => PixelFormat::Gbrap12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAP12BE as i32 => PixelFormat::Gbrap12Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAP14LE as i32 => PixelFormat::Gbrap14Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAP14BE as i32 => PixelFormat::Gbrap14Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAP16LE as i32 => PixelFormat::Gbrap16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAP16BE as i32 => PixelFormat::Gbrap16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAP32LE as i32 => PixelFormat::Gbrap32Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAP32BE as i32 => PixelFormat::Gbrap32Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAPF16LE as i32 => PixelFormat::Gbrapf16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAPF16BE as i32 => PixelFormat::Gbrapf16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAPF32LE as i32 => PixelFormat::Gbrapf32Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GBRAPF32BE as i32 => PixelFormat::Gbrapf32Be,
    // Greyscale.
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY8 as i32 => PixelFormat::Gray8,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY9LE as i32 => PixelFormat::Gray9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY9BE as i32 => PixelFormat::Gray9Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY10LE as i32 => PixelFormat::Gray10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY10BE as i32 => PixelFormat::Gray10Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY12LE as i32 => PixelFormat::Gray12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY12BE as i32 => PixelFormat::Gray12Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY14LE as i32 => PixelFormat::Gray14Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY14BE as i32 => PixelFormat::Gray14Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY16LE as i32 => PixelFormat::Gray16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY16BE as i32 => PixelFormat::Gray16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY32LE as i32 => PixelFormat::Gray32Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY32BE as i32 => PixelFormat::Gray32Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAYF16LE as i32 => PixelFormat::Grayf16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAYF16BE as i32 => PixelFormat::Grayf16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAYF32LE as i32 => PixelFormat::Grayf32Le,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAYF32BE as i32 => PixelFormat::Grayf32Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YA8 as i32 => PixelFormat::Ya8,
    x if x == AVPixelFormat::AV_PIX_FMT_YA16LE as i32 => PixelFormat::Ya16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YA16BE as i32 => PixelFormat::Ya16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YAF16LE as i32 => PixelFormat::Yaf16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YAF16BE as i32 => PixelFormat::Yaf16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_YAF32LE as i32 => PixelFormat::Yaf32Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YAF32BE as i32 => PixelFormat::Yaf32Be,
    x if x == AVPixelFormat::AV_PIX_FMT_MONOWHITE as i32 => PixelFormat::Monowhite,
    x if x == AVPixelFormat::AV_PIX_FMT_MONOBLACK as i32 => PixelFormat::Monoblack,
    x if x == AVPixelFormat::AV_PIX_FMT_PAL8 as i32 => PixelFormat::Pal8,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB4 as i32 => PixelFormat::Rgb4,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB4_BYTE as i32 => PixelFormat::Rgb4Byte,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB8 as i32 => PixelFormat::Rgb8,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR4 as i32 => PixelFormat::Bgr4,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR4_BYTE as i32 => PixelFormat::Bgr4Byte,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR8 as i32 => PixelFormat::Bgr8,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB444LE as i32 => PixelFormat::Rgb444Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB444BE as i32 => PixelFormat::Rgb444Be,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR444LE as i32 => PixelFormat::Bgr444Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR444BE as i32 => PixelFormat::Bgr444Be,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB555LE as i32 => PixelFormat::Rgb555Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB555BE as i32 => PixelFormat::Rgb555Be,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR555LE as i32 => PixelFormat::Bgr555Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR555BE as i32 => PixelFormat::Bgr555Be,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB565LE as i32 => PixelFormat::Rgb565Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGB565BE as i32 => PixelFormat::Rgb565Be,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR565LE as i32 => PixelFormat::Bgr565Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR565BE as i32 => PixelFormat::Bgr565Be,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_BGGR8 as i32 => PixelFormat::BayerBggr8,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_RGGB8 as i32 => PixelFormat::BayerRggb8,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GBRG8 as i32 => PixelFormat::BayerGbrg8,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GRBG8 as i32 => PixelFormat::BayerGrbg8,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_BGGR16LE as i32 => PixelFormat::BayerBggr16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_BGGR16BE as i32 => PixelFormat::BayerBggr16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_RGGB16LE as i32 => PixelFormat::BayerRggb16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_RGGB16BE as i32 => PixelFormat::BayerRggb16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GBRG16LE as i32 => PixelFormat::BayerGbrg16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GBRG16BE as i32 => PixelFormat::BayerGbrg16Be,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GRBG16LE as i32 => PixelFormat::BayerGrbg16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GRBG16BE as i32 => PixelFormat::BayerGrbg16Be,
    _ => PixelFormat::Unknown(raw as u32),
  }
}

/// Returns `true` when `raw` is one of FFmpeg's hardware-frame markers
/// (`AV_PIX_FMT_VIDEOTOOLBOX` / `_VAAPI` / `_CUDA` / `_D3D11` /
/// `_DRM_PRIME` / `_MEDIACODEC` / `_VULKAN`). Used by the HW probe to
/// identify GPU-resident frames before triggering
/// `av_hwframe_transfer_data`.
pub const fn is_hardware_pix_fmt(raw: i32) -> bool {
  matches!(
    raw,
    x if x == AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32
      || x == AVPixelFormat::AV_PIX_FMT_VAAPI as i32
      || x == AVPixelFormat::AV_PIX_FMT_CUDA as i32
      || x == AVPixelFormat::AV_PIX_FMT_D3D11 as i32
      || x == AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32
      || x == AVPixelFormat::AV_PIX_FMT_MEDIACODEC as i32
      || x == AVPixelFormat::AV_PIX_FMT_VULKAN as i32
  )
}

/// Fallible counterpart to ffmpeg-next's `Packet::copy`.
///
/// The upstream helper calls `Packet::new(size)` (which silently
/// truncates `size` to `c_int` and ignores `av_new_packet`'s return
/// code) and then panics via `data_mut().unwrap().write_all(...).unwrap()`
/// if the allocation failed. From a safe public decoder API we want
/// the OOM / oversized-payload paths to surface as
/// `ffmpeg_next::Error` rather than aborting the process — every
/// `send_packet` path goes through this helper.
///
/// Failure modes:
/// * payload larger than `c_int::MAX` (would overflow `AVPacket.size`)
///   → `ffmpeg_next::Error::Other { errno: libc::EINVAL }`.
/// * `av_new_packet` allocation failure (signalled by `data_mut()`
///   returning `None`) → `ffmpeg_next::Error::Other { errno:
///   libc::ENOMEM }`.
fn try_packet_copy(data: &[u8]) -> std::result::Result<Packet, ffmpeg_next::Error> {
  // FFmpeg's `AVPacket.size` is `c_int`. A payload larger than that
  // can't fit in a single packet — refuse rather than truncate via
  // `as c_int` inside `Packet::new`.
  if data.len() > c_int::MAX as usize {
    return Err(ffmpeg_next::Error::Other {
      errno: libc::EINVAL,
    });
  }
  // `Packet::new(size)` calls `av_new_packet(&mut pkt, size as
  // c_int)` and ignores the return code; on OOM it returns a
  // `Packet` whose `.data` is null. We detect that via
  // `data_mut()` (returns `None` on null) and copy via
  // `copy_nonoverlapping` so we never go through `data_mut()
  // .unwrap().write_all().unwrap()` — the upstream `Packet::copy`'s
  // double panic.
  let mut pkt = Packet::new(data.len());
  match pkt.data_mut() {
    Some(slot) if slot.len() == data.len() => {
      // SAFETY: `slot` is a `&mut [u8]` of `data.len()` bytes;
      // `data` is a `&[u8]` of the same length. Non-overlapping
      // because `slot` is a fresh allocation.
      if !data.is_empty() {
        unsafe {
          core::ptr::copy_nonoverlapping(data.as_ptr(), slot.as_mut_ptr(), data.len());
        }
      }
      Ok(pkt)
    }
    _ => Err(ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    }),
  }
}

/// Centralised mediadecode→AV packet flag mapping so the three
/// packet-conversion helpers stay aligned.
fn map_md_flags_to_av(flags: MdPacketFlags) -> ffmpeg_next::packet::Flags {
  let mut av_flags = ffmpeg_next::packet::Flags::empty();
  if flags.contains(MdPacketFlags::KEY) {
    av_flags |= ffmpeg_next::packet::Flags::KEY;
  }
  if flags.contains(MdPacketFlags::CORRUPT) {
    av_flags |= ffmpeg_next::packet::Flags::CORRUPT;
  }
  // ffmpeg-next 8.x doesn't expose a DISCARD flag constant on
  // `packet::Flags`; the upstream `AV_PKT_FLAG_DISCARD` bit is
  // documented as a demuxer hint and rarely set on packets passed
  // to a decoder. We forward KEY and CORRUPT (the meaningful subset)
  // and silently drop DISCARD until ffmpeg-next adds it.
  av_flags
}

/// Builds an `ffmpeg::Packet` from a [`mediadecode::VideoPacket`]
/// parameterized by [`crate::extras::VideoPacketExtra`] and
/// [`crate::FfmpegBuffer`].
///
/// The compressed bytes are **copied** into a new packet allocation —
/// zero-copy passthrough of the FfmpegBuffer's underlying AVBufferRef
/// is a future optimization (would need to wire an `AVBufferRef` into
/// `AVPacket.buf` directly via `av_packet_alloc` + manual buffer set).
/// PTS / DTS / duration / flags / stream_index are propagated.
///
/// Returns `Err(ffmpeg_next::Error)` on:
/// * payload larger than `c_int::MAX` (would overflow `AVPacket.size`);
/// * `av_new_packet` allocation failure (OOM).
pub fn ffmpeg_packet_from_video_packet(
  packet: &mediadecode::packet::VideoPacket<VideoPacketExtra, FfmpegBuffer>,
) -> std::result::Result<Packet, ffmpeg_next::Error> {
  let mut out = try_packet_copy(packet.data().as_ref())?;
  if let Some(ts) = packet.pts() {
    out.set_pts(Some(ts.pts()));
  }
  if let Some(ts) = packet.dts() {
    out.set_dts(Some(ts.pts()));
  }
  if let Some(d) = packet.duration() {
    out.set_duration(d.pts());
  }
  out.set_flags(map_md_flags_to_av(packet.flags()));
  out.set_stream(packet.extra().stream_index() as usize);
  Ok(out)
}

/// Builds an `ffmpeg::Packet` from a [`mediadecode::AudioPacket`].
/// Same shape as [`ffmpeg_packet_from_video_packet`] — bytes are
/// copied; pts/dts/duration/flags/stream_index are forwarded. Same
/// failure modes.
pub fn ffmpeg_packet_from_audio_packet(
  packet: &mediadecode::packet::AudioPacket<AudioPacketExtra, FfmpegBuffer>,
) -> std::result::Result<Packet, ffmpeg_next::Error> {
  let mut out = try_packet_copy(packet.data().as_ref())?;
  if let Some(ts) = packet.pts() {
    out.set_pts(Some(ts.pts()));
  }
  if let Some(ts) = packet.dts() {
    out.set_dts(Some(ts.pts()));
  }
  if let Some(d) = packet.duration() {
    out.set_duration(d.pts());
  }
  out.set_flags(map_md_flags_to_av(packet.flags()));
  out.set_stream(packet.extra().stream_index() as usize);
  Ok(out)
}

/// Builds an `ffmpeg::Packet` from a [`mediadecode::SubtitlePacket`].
/// Bytes copied; pts/duration/flags/stream_index forwarded. Subtitle
/// packets have no `dts` in the mediadecode model. Same failure
/// modes as [`ffmpeg_packet_from_video_packet`].
pub fn ffmpeg_packet_from_subtitle_packet(
  packet: &mediadecode::packet::SubtitlePacket<SubtitlePacketExtra, FfmpegBuffer>,
) -> std::result::Result<Packet, ffmpeg_next::Error> {
  let mut out = try_packet_copy(packet.data().as_ref())?;
  if let Some(ts) = packet.pts() {
    out.set_pts(Some(ts.pts()));
  }
  if let Some(d) = packet.duration() {
    out.set_duration(d.pts());
  }
  out.set_flags(map_md_flags_to_av(packet.flags()));
  out.set_stream(packet.extra().stream_index() as usize);
  Ok(out)
}

// ---------------------------------------------------------------------------
//  Safe wrappers — `&ffmpeg::Packet` → `mediadecode::*Packet`.
// ---------------------------------------------------------------------------

/// Wraps a borrowed [`ffmpeg::Packet`] as a
/// [`mediadecode::packet::VideoPacket`]. The compressed payload is
/// shared with the source `AVPacket` via refcount bump (no copy).
/// Timestamps, duration, key/corrupt flags, and the source stream
/// index are forwarded to the produced packet.
///
/// Returns `None` when the source packet has no buffer attached
/// (empty packet — typical after EOF). Caller can also fill in
/// [`VideoPacketExtra::byte_pos`] / `side_data` post-construction
/// if they need those.
pub fn video_packet_from_ffmpeg(
  packet: &Packet,
) -> Option<VideoPacket<VideoPacketExtra, FfmpegBuffer>> {
  let buf = FfmpegBuffer::from_packet(packet)?;
  let mut out = VideoPacket::new(buf, VideoPacketExtra::new(packet.stream() as i32))
    .with_flags(md_flags_from_av(packet.flags()));
  if let Some(p) = packet.pts() {
    out = out.with_pts(Some(Timestamp::new(p, mediadecode::Timebase::default())));
  }
  if let Some(d) = packet.dts() {
    out = out.with_dts(Some(Timestamp::new(d, mediadecode::Timebase::default())));
  }
  let dur = packet.duration();
  if dur > 0 {
    out = out.with_duration(Some(Timestamp::new(dur, mediadecode::Timebase::default())));
  }
  Some(out)
}

/// Wraps a borrowed [`ffmpeg::Packet`] as a
/// [`mediadecode::packet::AudioPacket`]. Same shape as
/// [`video_packet_from_ffmpeg`] — refcounted payload, forwarded
/// metadata.
pub fn audio_packet_from_ffmpeg(
  packet: &Packet,
) -> Option<AudioPacket<AudioPacketExtra, FfmpegBuffer>> {
  let buf = FfmpegBuffer::from_packet(packet)?;
  let mut out = AudioPacket::new(buf, AudioPacketExtra::new(packet.stream() as i32))
    .with_flags(md_flags_from_av(packet.flags()));
  if let Some(p) = packet.pts() {
    out = out.with_pts(Some(Timestamp::new(p, mediadecode::Timebase::default())));
  }
  if let Some(d) = packet.dts() {
    out = out.with_dts(Some(Timestamp::new(d, mediadecode::Timebase::default())));
  }
  let dur = packet.duration();
  if dur > 0 {
    out = out.with_duration(Some(Timestamp::new(dur, mediadecode::Timebase::default())));
  }
  Some(out)
}

/// Wraps a borrowed [`ffmpeg::Packet`] as a
/// [`mediadecode::packet::SubtitlePacket`]. Subtitle packets have no
/// `dts` in the mediadecode model; everything else mirrors
/// [`video_packet_from_ffmpeg`].
pub fn subtitle_packet_from_ffmpeg(
  packet: &Packet,
) -> Option<SubtitlePacket<SubtitlePacketExtra, FfmpegBuffer>> {
  let buf = FfmpegBuffer::from_packet(packet)?;
  let mut out = SubtitlePacket::new(buf, SubtitlePacketExtra::new(packet.stream() as i32))
    .with_flags(md_flags_from_av(packet.flags()));
  if let Some(p) = packet.pts() {
    out = out.with_pts(Some(Timestamp::new(p, mediadecode::Timebase::default())));
  }
  let dur = packet.duration();
  if dur > 0 {
    out = out.with_duration(Some(Timestamp::new(dur, mediadecode::Timebase::default())));
  }
  Some(out)
}

fn md_flags_from_av(flags: ffmpeg_next::packet::Flags) -> MdPacketFlags {
  let mut out = MdPacketFlags::empty();
  if flags.contains(ffmpeg_next::packet::Flags::KEY) {
    out |= MdPacketFlags::KEY;
  }
  if flags.contains(ffmpeg_next::packet::Flags::CORRUPT) {
    out |= MdPacketFlags::CORRUPT;
  }
  out
}

// ---------------------------------------------------------------------------
//  Empty-frame placeholders for `receive_frame` destinations.
// ---------------------------------------------------------------------------

/// Constructs an empty [`mediadecode::frame::VideoFrame`] suitable as
/// the destination argument to
/// [`mediadecode::decoder::VideoStreamDecoder::receive_frame`]. The
/// decoder overwrites the frame on success; this just provides a
/// well-formed slot.
///
/// All four plane slots get a 1-byte `FfmpegBuffer` placeholder
/// (the array shape requires a buffer in every slot, but
/// `plane_count = 0` reports them as inactive).
///
/// # Panics
///
/// Panics on FFmpeg-side OOM (the per-plane 1-byte allocation
/// failed). Callers who need to recover from OOM should use
/// [`try_empty_video_frame`].
pub fn empty_video_frame() -> VideoFrame<PixelFormat, VideoFrameExtra, FfmpegBuffer> {
  try_empty_video_frame().expect("empty_video_frame: av_buffer_alloc returned null (OOM)")
}

/// Fallible counterpart to [`empty_video_frame`]. Returns `None` if
/// any of the four placeholder allocations fails.
pub fn try_empty_video_frame() -> Option<VideoFrame<PixelFormat, VideoFrameExtra, FfmpegBuffer>> {
  let planes = [
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
  ];
  Some(VideoFrame::new(
    Dimensions::new(0, 0),
    PixelFormat::Unknown(0),
    planes,
    0,
    VideoFrameExtra::default(),
  ))
}

/// Constructs an empty [`mediadecode::frame::AudioFrame`] suitable as
/// the destination argument to
/// [`mediadecode::decoder::AudioStreamDecoder::receive_frame`]. Same
/// behaviour as [`empty_video_frame`] — eight 1-byte plane
/// placeholders, `plane_count = 0`.
///
/// # Panics
///
/// Panics on FFmpeg-side OOM. See [`try_empty_audio_frame`] for the
/// fallible variant.
pub fn empty_audio_frame()
-> AudioFrame<SampleFormat, AudioChannelLayout, AudioFrameExtra, FfmpegBuffer> {
  try_empty_audio_frame().expect("empty_audio_frame: av_buffer_alloc returned null (OOM)")
}

/// Fallible counterpart to [`empty_audio_frame`]. Returns `None` if
/// any of the eight placeholder allocations fails.
pub fn try_empty_audio_frame()
-> Option<AudioFrame<SampleFormat, AudioChannelLayout, AudioFrameExtra, FfmpegBuffer>> {
  let planes = [
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
    Plane::new(FfmpegBuffer::try_empty()?, 0),
  ];
  Some(AudioFrame::new(
    0,
    0,
    0,
    SampleFormat::NONE,
    AudioChannelLayout::default(),
    planes,
    0,
    AudioFrameExtra::default(),
  ))
}

/// Constructs an empty [`mediadecode::frame::SubtitleFrame`] suitable
/// as the destination argument to
/// [`mediadecode::decoder::SubtitleDecoder::receive_frame`]. The
/// payload is an empty `Text` placeholder; the decoder overwrites
/// it on success.
///
/// # Panics
///
/// Panics on FFmpeg-side OOM. See [`try_empty_subtitle_frame`] for
/// the fallible variant.
pub fn empty_subtitle_frame() -> SubtitleFrame<SubtitleFrameExtra, FfmpegBuffer> {
  try_empty_subtitle_frame().expect("empty_subtitle_frame: av_buffer_alloc returned null (OOM)")
}

/// Fallible counterpart to [`empty_subtitle_frame`]. Returns `None`
/// if the placeholder allocation fails.
pub fn try_empty_subtitle_frame() -> Option<SubtitleFrame<SubtitleFrameExtra, FfmpegBuffer>> {
  let buf = FfmpegBuffer::copy_from_slice(&[]).or_else(FfmpegBuffer::try_empty)?;
  Some(SubtitleFrame::new(
    SubtitlePayload::Text {
      text: buf,
      language: None,
    },
    SubtitleFrameExtra::default(),
  ))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn nv12_round_trips() {
    assert_eq!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_NV12 as i32),
      PixelFormat::Nv12,
    );
  }

  #[test]
  fn p010be_maps_to_p010be() {
    // BE must map to the BE variant — the previous "fold to LE"
    // mapping silently corrupted P010BE pixel data via the safe
    // export path. The unsupported-format gate in `convert::av_frame_to_video_frame`
    // is the right place to reject BE today.
    assert_eq!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_P010BE as i32),
      PixelFormat::P010Be,
    );
  }

  #[test]
  fn unknown_for_garbage_value() {
    assert!(matches!(
      from_av_pixel_format(-99_999),
      PixelFormat::Unknown(_)
    ));
  }

  #[test]
  fn hw_formats_detected() {
    assert!(is_hardware_pix_fmt(
      AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32
    ));
    assert!(is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_VAAPI as i32));
    assert!(is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_CUDA as i32));
    assert!(is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_D3D11 as i32));
  }

  #[test]
  fn cpu_formats_not_detected_as_hw() {
    assert!(!is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_NV12 as i32));
    assert!(!is_hardware_pix_fmt(
      AVPixelFormat::AV_PIX_FMT_YUV420P as i32
    ));
    assert!(!is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_NONE as i32));
  }

  #[test]
  fn hw_formats_map_to_unknown_in_pixel_format() {
    // HW sentinels intentionally don't have a mediadecode::PixelFormat
    // representation — they're not CPU pixel data.
    assert!(matches!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32),
      PixelFormat::Unknown(_)
    ));
    assert!(matches!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_VAAPI as i32),
      PixelFormat::Unknown(_)
    ));
  }
}

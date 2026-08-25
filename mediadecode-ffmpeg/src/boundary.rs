//! Boundary conversions between FFmpeg's bindgen integers and the
//! unified [`mediadecode`] vocabulary.
//!
//! Centralised so the rest of the crate never compares raw
//! `AVPixelFormat` integers against literals or transmutes back into
//! the bindgen enum (UB hazard when the value isn't in the enum's
//! discriminant set).

use core::{
  ffi::c_int,
  ptr::{addr_of, read_unaligned},
};

use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::{Packet, ffi::AVPixelFormat};
use mediadecode::{
  PixelFormat, Timestamp,
  demuxer::{AttachmentPacket, DataPacket},
  frame::{AudioFrame, Dimensions, Plane, SubtitleFrame, VideoFrame},
  packet::{AudioPacket, PacketFlags as MdPacketFlags, SubtitlePacket, VideoPacket},
  subtitle::{SubtitlePayload, Text as SubtitleText},
};
use mediaframe::audio::ChannelLayoutDescription;

use crate::{
  // `buffer::SideDataAlloc` is aliased: this module also defines its own
  // `PacketBuildError::SideDataAlloc` payload (the write-side failure —
  // FFmpeg refusing an allocation while rebuilding an `AVPacket`'s side
  // data), which keeps the bare name since it is native to this file.
  // `BufferSideDataAlloc` is `PacketBufferError`'s read-side counterpart
  // (out of memory copying a side-data entry *out of* an `AVPacket`) —
  // same short name, different struct, different direction.
  buffer::{
    FfmpegBytes, PacketBufferError, SideDataAlloc as BufferSideDataAlloc, SideDataArray,
    SideDataBytes, SideDataEntries, SideDataPayload, UnrepresentableFlags,
  },
  carrier::BodyRoute,
  convert::{SIDE_DATA_MAX_ENTRIES, SIDE_DATA_MAX_TOTAL_BYTES},
  extras::{
    AttachmentPacketExtra, AudioFrameExtra, AudioPacketExtra, DataPacketExtra, SideDataEntry,
    SubtitleFrameExtra, SubtitlePacketExtra, VideoFrameExtra, VideoPacketExtra,
  },
  limits::PacketLimits,
  sample_format::SampleFormat,
};

/// Maps a raw `AVFrame.format` integer (i.e. the value of an
/// `AVPixelFormat` enum variant) onto [`mediadecode::PixelFormat`].
///
/// Returns [`PixelFormat::None`] for raw integers we don't have a
/// mapping for — including `AV_PIX_FMT_NONE` itself and the
/// hardware-frame markers (`AV_PIX_FMT_VIDEOTOOLBOX` / `_VAAPI` /
/// `_CUDA` / `_D3D11` / …), since those never describe CPU-side pixel
/// data and the unified enum intentionally doesn't carry them. Use
/// [`is_hardware_pix_fmt`] to identify HW frames before transferring
/// to a CPU format.
///
/// mediaframe 0.3 struck `PixelFormat::Unknown(u32)`, so the raw
/// integer no longer rides along in the returned value; the caller's
/// own `raw` is the place it survives. Every consumer in this crate
/// already treats the fall-through as "not a deliverable CPU format"
/// (`pixdesc::to_av_pixel_format`, `is_supported_cpu_pix_fmt` and the
/// geometry tables all reject it), so the rejection is unchanged.
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
    _ => PixelFormat::None,
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
pub(crate) fn try_packet_copy(data: &[u8]) -> std::result::Result<Packet, ffmpeg_next::Error> {
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

/// Writes every flag the portable packet carries onto the `AVPacket`
/// being rebuilt.
///
/// The one place this crate writes packet flags into FFmpeg. The three
/// stream families rebuild through it, and so does the one-shot image
/// road — which for two releases forgot to, and sent every attachment
/// to libavcodec with a zeroed `flags` field.
///
/// Not through `Packet::set_flags`, which takes `ffmpeg_next`'s `Flags`
/// and so can only spell `KEY` and `CORRUPT`: the bits are written to
/// `AVPacket.flags` directly, so `DISCARD` — and anything else the
/// forward direction retained — reaches the decoder that has to obey
/// it. `PacketFlags` is a `u8` bit set and the field is a `c_int`, so
/// the widening is total and nothing needs deciding here.
///
/// # Safety
///
/// `packet` must own a live `AVPacket`.
pub(crate) unsafe fn write_md_flags(packet: &mut Packet, flags: MdPacketFlags) {
  use ffmpeg_next::packet::Mut;
  unsafe {
    (*packet.as_mut_ptr()).flags = c_int::from(flags.bits());
  }
}

/// Payload for [`PacketBuildError::UnknownSideData`].
///
/// A side-data entry whose type this build of FFmpeg does not name.
///
/// Refused rather than dropped: this crate carries side-data types as
/// the raw integers they are on the wire, and handing an unknown one
/// to C would either form an invalid enum discriminant or attach a
/// type nothing downstream can read. Everything the demuxer captured
/// came from this same build and is in range, so this only answers a
/// hand-built entry.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("side-data type {kind} is not one this FFmpeg build names (0..{limit})")]
pub struct UnknownSideData {
  kind: i32,
  limit: i32,
}

impl UnknownSideData {
  /// Constructs an `UnknownSideData` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(kind: i32, limit: i32) -> Self {
    Self { kind, limit }
  }
  /// The type integer the entry carried.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> i32 {
    self.kind
  }
  /// How many side-data types this build names.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> i32 {
    self.limit
  }
}

/// Payload for [`PacketBuildError::SideDataAlloc`].
///
/// FFmpeg refused the side-data allocation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("out of memory attaching {size} bytes of side data of type {kind}")]
pub struct SideDataAlloc {
  kind: i32,
  size: usize,
}

impl SideDataAlloc {
  /// Constructs a `SideDataAlloc` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(kind: i32, size: usize) -> Self {
    Self { kind, size }
  }
  /// The entry's type integer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> i32 {
    self.kind
  }
  /// The entry's payload length.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn size(&self) -> usize {
    self.size
  }
}

/// Payload for [`PacketBuildError::SendPayloadTooLarge`].
///
/// A compressed payload on its way **into** FFmpeg is larger than the
/// session's budget allows.
///
/// Named for the direction: this is the send leg. Its outbound twin is
/// [`PacketBufferError::PacketTooLarge`](crate::PacketBufferError),
/// which judges the same quantity coming the other way, against the
/// same seat.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a {bytes}-byte packet payload exceeds the {limit}-byte budget on the way into FFmpeg")]
pub struct SendPayloadTooLarge {
  bytes: usize,
  limit: usize,
}

impl SendPayloadTooLarge {
  /// Constructs a `SendPayloadTooLarge` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(bytes: usize, limit: usize) -> Self {
    Self { bytes, limit }
  }
  /// The payload length the caller handed over.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Why a portable packet could not be rebuilt as an `AVPacket`.
///
/// The reverse direction is what feeds a decoder, so everything the
/// forward direction captured has to survive it or the capture was
/// theatre.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
pub enum PacketBuildError {
  /// The payload is larger than the session's budget allows. Refused
  /// **before** the `AVPacket` is allocated — the into-FFmpeg leg of
  /// the budget the outbound leg already kept.
  #[error(transparent)]
  SendPayloadTooLarge(#[from] SendPayloadTooLarge),

  /// The packet body could not be allocated or is larger than
  /// `AVPacket.size` can hold.
  #[error(transparent)]
  Ffmpeg(#[from] ffmpeg_next::Error),

  /// A side-data entry whose type this build of FFmpeg does not name.
  #[error(transparent)]
  UnknownSideData(#[from] UnknownSideData),

  /// FFmpeg refused the side-data allocation.
  #[error(transparent)]
  SideDataAlloc(#[from] SideDataAlloc),

  /// The packet is marked `AV_PKT_FLAG_TRUSTED`. See
  /// [`crate::buffer::TrustedPayload`] — the other leg of the same
  /// refusal.
  #[error(transparent)]
  TrustedPayload(#[from] crate::buffer::TrustedPayload),

  /// The packet's side-data list is over the entry or byte cap the read
  /// side has always applied. See [`SendSideDataTooLarge`].
  #[error(transparent)]
  SendSideDataTooLarge(#[from] SendSideDataTooLarge),
}

/// Judges the **whole** side-data list before a byte of it is
/// allocated.
///
/// # One seat, both directions
///
/// The read side has capped frame and packet side data at 64 entries
/// and 256 KiB since it was written; the send side had no cap at all.
/// The preflight checked `body.len()` and then `attach_side_data`
/// allocated every entry the caller handed over, one
/// `av_packet_new_side_data` at a time, with nothing bounding the count
/// or the total — so bytes the demux boundary would have refused coming
/// *out* of a container went straight into libavcodec going *in*. That
/// is the same asymmetry `PacketLimits` was introduced to close for the
/// packet body, one field along.
///
/// So the caps are the read side's, applied here, before anything is
/// allocated — including the body, because a list that cannot be
/// carried should not cost a packet allocation first.
fn check_side_data_budget(entries: &[SideDataEntry]) -> Result<(), PacketBuildError> {
  use crate::convert::{SIDE_DATA_MAX_ENTRIES, SIDE_DATA_MAX_TOTAL_BYTES};

  if entries.len() > SIDE_DATA_MAX_ENTRIES {
    return Err(PacketBuildError::SendSideDataTooLarge(
      SendSideDataTooLarge::new_entries(entries.len(), SIDE_DATA_MAX_ENTRIES),
    ));
  }
  let mut total: usize = 0;
  for entry in entries {
    total = total.saturating_add(entry.data().len());
    if total > SIDE_DATA_MAX_TOTAL_BYTES {
      return Err(PacketBuildError::SendSideDataTooLarge(
        SendSideDataTooLarge::new_bytes(total, SIDE_DATA_MAX_TOTAL_BYTES),
      ));
    }
  }
  Ok(())
}

/// Payload for [`PacketBuildError::SendSideDataTooLarge`].
///
/// A side-data list too long or too large to carry into a decoder.
/// Carries whichever of the two caps was reached, so a caller can tell
/// "too many annotations" from "too much annotation".
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("packet side data is over the {what} ceiling: {value} against {limit}")]
pub struct SendSideDataTooLarge {
  what: &'static str,
  value: usize,
  limit: usize,
}

impl SendSideDataTooLarge {
  /// The entry-count cap was reached.
  #[inline]
  pub const fn new_entries(value: usize, limit: usize) -> Self {
    Self {
      what: "entry-count",
      value,
      limit,
    }
  }
  /// The aggregate-byte cap was reached.
  #[inline]
  pub const fn new_bytes(value: usize, limit: usize) -> Self {
    Self {
      what: "byte",
      value,
      limit,
    }
  }
  /// The count or byte total the list reached.
  #[inline]
  pub const fn value(&self) -> usize {
    self.value
  }
  /// The cap in force.
  #[inline]
  pub const fn limit(&self) -> usize {
    self.limit
  }
  /// Which cap: `"entry-count"` or `"byte"`.
  #[inline]
  pub const fn what(&self) -> &'static str {
    self.what
  }
}

/// Refuses a portable packet whose flags carry `AV_PKT_FLAG_TRUSTED`.
///
/// The rebuild leg of the refusal `payload_of` makes on the way out.
/// Closing only the copy-out leg would leave the loop open: a flag that
/// reached a portable packet by some other route (a caller composing
/// one by hand, a future producer, a graph that round-trips flags it
/// does not interpret) would be written back onto a fresh `AVPacket` by
/// `write_md_flags` and handed to a decoder entitled to believe it.
///
/// See [`crate::buffer::TrustedPayload`] for why the flag makes the
/// payload uncarriable rather than merely suspicious.
fn refuse_trusted(flags: MdPacketFlags, len: usize) -> std::result::Result<(), PacketBuildError> {
  if flags.bits() & crate::buffer::TRUSTED_BIT != 0 {
    return Err(PacketBuildError::TrustedPayload(
      crate::buffer::TrustedPayload::new(len),
    ));
  }
  Ok(())
}

/// Copies `entries` onto an `AVPacket` under construction.
///
/// A decoder learns things only this way: `AV_PKT_DATA_NEW_EXTRADATA`
/// replaces its parameters mid-stream, `AV_PKT_DATA_PARAM_CHANGE` moves
/// its rate or layout, `AV_PKT_DATA_SKIP_SAMPLES` is the encoder-delay
/// trim without which a gapless stream is not gapless. Rebuilding a
/// packet without them hands the decoder a body and a lie.
fn attach_side_data(out: &mut Packet, entries: &[SideDataEntry]) -> Result<(), PacketBuildError> {
  use ffmpeg_next::packet::Mut;
  for entry in entries {
    let kind = entry.kind();
    let size = entry.data().len();
    // SAFETY: `out` owns a live `AVPacket`; `packet_new_side_data`
    // validates the type against this build's own range before handing
    // it to C and reports a failed allocation as `None`.
    let slot = unsafe { crate::ffi::packet_new_side_data(out.as_mut_ptr(), kind, size) };
    let Some(slot) = slot else {
      let limit = crate::ffi::side_data_type_count();
      return Err(if kind < 0 || kind >= limit {
        PacketBuildError::UnknownSideData(UnknownSideData::new(kind, limit))
      } else {
        PacketBuildError::SideDataAlloc(SideDataAlloc::new(kind, size))
      });
    };
    if size > 0 {
      // SAFETY: FFmpeg just allocated `size` bytes at `slot` (plus its
      // padding), and `entry.data()` is a `&[u8]` of exactly that
      // length; the two regions belong to different allocations.
      unsafe { core::ptr::copy_nonoverlapping(entry.data().as_ptr(), slot, size) };
    }
  }
  Ok(())
}

/// Builds an `AVPacket` that **shares** a view carrier's buffer instead
/// of copying it — the send half of the zero-copy chain.
///
/// # What the census found
///
/// 0.8 did not have this. Its reverse builders called the same
/// `try_packet_copy` the owned lane uses, so a packet demuxed
/// zero-copy was copied again on its way back into a decoder. The
/// zero-copy chain 0.8 actually shipped was demux to consumer, and
/// stopped there.
///
/// # The padding proof
///
/// libavcodec is entitled to read `AV_INPUT_BUFFER_PADDING_SIZE` bytes
/// **past** a packet's payload — bitstream readers do it routinely, and
/// libavformat allocates that slack behind every packet it produces. A
/// carrier viewing such a packet inherits the slack; a carrier this
/// crate minted from a slice through
/// [`FfmpegCarrier::from_bytes`](crate::FfmpegCarrier::from_bytes) does
/// not.
///
/// So sharing is offered only where the slack is **provable**, and
/// proving it takes two facts, not one:
///
/// * **provenance** — the carrier must be a payload captured out of an
///   `AVPacket`'s own buffer, which is the only place libavformat's
///   padding contract applies. Trailing *capacity* is not padding: a
///   video plane has more pixels after it and a resampler's output
///   frame has more samples, and a bitstream reader running past the
///   payload would eat either as though it were bitstream. Provenance
///   is recorded at capture, where it is known, rather than inferred
///   here from a size comparison that cannot tell padding from a
///   neighbour;
/// * **extent** — the view must still leave at least the padding
///   between its end and the end of the buffer, because a payload can
///   be narrowed after capture.
///
/// Where either fails, the packet is copied, which is what
/// `try_packet_copy` allocates the padding for. Handing a decoder an
/// unpadded buffer would be an out-of-bounds read inside somebody
/// else's bitstream reader, found by nobody.
pub(crate) fn share_or_copy(
  body: &crate::FfmpegBuffer,
) -> std::result::Result<Packet, ffmpeg_next::Error> {
  use ffmpeg_next::packet::Mut;

  const PADDING: usize = ffmpeg_next::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;

  // **Empty first, before anything is dereferenced.** The empty carrier
  // is deliberately backed by no buffer at all — that is what makes a
  // placeholder plane slot free — so `as_av_buffer_ref` answers null
  // for it, and reading `size` through that would be undefined on the
  // most ordinary packet there is: a side-data-only one.
  if body.is_empty() {
    return try_packet_copy(&[]);
  }

  // Provenance before extent: the cheaper question, and the one that
  // decides whether the other is worth asking.
  if body.origin() != crate::view::Origin::PacketPayload {
    return try_packet_copy(body.as_ref());
  }

  let buf = body.as_av_buffer_ref();
  if buf.is_null() {
    return try_packet_copy(body.as_ref());
  }
  // SAFETY: the carrier holds a live reference, just checked non-null;
  // `size` is a public field on the `AVBufferRef` it names.
  let capacity = unsafe { (*buf).size };
  let end = body.offset().saturating_add(body.len());
  let padded = capacity
    .checked_sub(end)
    .is_some_and(|slack| slack >= PADDING);

  if !padded {
    // No provable slack behind the view — copy, which allocates the
    // padding libavcodec expects.
    return try_packet_copy(body.as_ref());
  }

  let mut out = Packet::empty();
  // SAFETY: `out` owns a live, zeroed `AVPacket`. `av_buffer_ref`
  // returns a new reference to the same allocation — the packet owns
  // that one and releases it on drop, while `body` keeps its own — and
  // `data`/`size` are set to the view, which was proved to lie inside
  // the buffer when the carrier was constructed.
  unsafe {
    let raw = out.as_mut_ptr();
    let shared = ffmpeg_next::ffi::av_buffer_ref(buf.cast_mut());
    if shared.is_null() {
      return Err(ffmpeg_next::Error::Other {
        errno: libc::ENOMEM,
      });
    }
    (*raw).buf = shared;
    (*raw).data = (*shared).data.add(body.offset());
    (*raw).size = c_int::try_from(body.len()).map_err(|_| ffmpeg_next::Error::Other {
      errno: libc::EINVAL,
    })?;
  }
  Ok(out)
}

/// Builds an `ffmpeg::Packet` from a [`mediadecode::VideoPacket`]
/// parameterized by [`crate::extras::VideoPacketExtra`] and
/// `FfmpegBytes`.
///
/// The compressed bytes are **copied** into a new packet allocation.
/// The return leg copies for the same reason the outbound one does:
/// the carrier is Rust-owned memory with no `AVBufferRef` behind it to
/// hand back.
/// PTS / DTS / duration / flags / stream_index are propagated.
///
/// Side data on the extras is reattached to the rebuilt packet — see
/// [`attach_side_data`] for why that is not optional.
///
/// Returns [`PacketBuildError`] on:
/// * payload larger than `c_int::MAX` (would overflow `AVPacket.size`);
/// * `av_new_packet` allocation failure (OOM);
/// * a side-data entry this build of FFmpeg cannot name, or one whose
///   allocation failed.
pub fn ffmpeg_packet_from_video_packet(
  packet: &mediadecode::packet::VideoPacket<VideoPacketExtra, crate::FfmpegBuffer>,
  limits: PacketLimits,
) -> std::result::Result<Packet, PacketBuildError> {
  build_video_packet::<crate::View>(packet, limits, BodyRoute::Copy)
}

/// [`ffmpeg_packet_from_video_packet`] on the owned lane.
pub fn ffmpeg_packet_from_owned_video_packet(
  packet: &mediadecode::packet::VideoPacket<VideoPacketExtra, FfmpegBytes>,
  limits: PacketLimits,
) -> std::result::Result<Packet, PacketBuildError> {
  build_video_packet::<crate::Owned>(packet, limits, BodyRoute::Copy)
}

/// [`ffmpeg_packet_from_video_packet`] built for **submission**, and
/// scoped so the packet cannot outlive the call.
///
/// This is the only road on which the view lane may hand a decoder
/// its own buffer rather than a copy, and the scoping is why it may.
/// An `ffmpeg_next::Packet` lends `&mut [u8]` through `data_mut`,
/// while the carrier it was built from still lends `&[u8]` — so a
/// shared body reachable from a value a caller *holds* is two live
/// references to one allocation with one of them mutable, out of
/// entirely safe code, and `!Sync` would not save it either. Here the
/// packet is built, lent to `submit` as `&Packet`, and dropped before
/// this function returns: no value that can produce a `&mut` into the
/// shared bytes ever exists.
///
/// **Which is why the route is the caller's to choose.** "Dropped
/// before this returns" is a claim about this function, and a decoder
/// that *records* what it is sent makes it false: the video decoder's
/// hardware probe `av_packet_ref`s every accepted packet into a rescue
/// history, and `FallbackFailed::unconsumed_packets` hands those
/// recordings to the caller as owned, **mutable** `Packet`s. A
/// submission that could be recorded must therefore carry
/// [`BodyRoute::Copy`], so that what enters the history is storage
/// nobody else reads. Callers with no history — every software road,
/// and the hardware road after commit — pass
/// [`BodyRoute::Submission`] and keep the zero-copy send.
pub(crate) fn with_ffmpeg_video_packet<C: crate::FfmpegCarrier + crate::CarrierOps, T>(
  packet: &mediadecode::packet::VideoPacket<VideoPacketExtra, C::Buffer>,
  limits: PacketLimits,
  route: BodyRoute,
  submit: impl FnOnce(&Packet) -> T,
) -> std::result::Result<T, PacketBuildError> {
  let av_packet = build_video_packet::<C>(packet, limits, route)?;
  let out = submit(&av_packet);
  drop(av_packet);
  Ok(out)
}

fn build_video_packet<C: crate::FfmpegCarrier + crate::CarrierOps>(
  packet: &mediadecode::packet::VideoPacket<VideoPacketExtra, C::Buffer>,
  limits: PacketLimits,
  route: BodyRoute,
) -> std::result::Result<Packet, PacketBuildError> {
  let body = packet.data().as_ref();
  // Before the budget and before the allocation: an uncarriable
  // payload is not made carriable by fitting.
  refuse_trusted(packet.flags(), body.len())?;
  // And the annotations, judged whole before the body is allocated —
  // a list that cannot be carried should not cost a packet first.
  check_side_data_budget(packet.extra().side_data())?;
  // **The into-FFmpeg budget, before the allocation.** `try_packet_copy`
  // duplicates these bytes into a fresh `AVPacket` and checks only that
  // they fit `c_int`. Without this the configured ceiling was dead on
  // the road a caller feeds a decoder directly: bytes the demux
  // boundary would have refused went straight into libavcodec.
  if body.len() > limits.max_packet_bytes() {
    return Err(PacketBuildError::SendPayloadTooLarge(
      SendPayloadTooLarge::new(body.len(), limits.max_packet_bytes()),
    ));
  }
  let mut out = C::packet_body(packet.data(), route)?;
  attach_side_data(&mut out, packet.extra().side_data())?;
  if let Some(ts) = packet.pts() {
    out.set_pts(Some(ts.pts()));
  }
  if let Some(ts) = packet.dts() {
    out.set_dts(Some(ts.pts()));
  }
  if let Some(d) = packet.duration() {
    out.set_duration(d.pts());
  }
  // SAFETY: `out` owns the `AVPacket` `try_packet_copy` just built.
  unsafe { write_md_flags(&mut out, packet.flags()) };
  out.set_stream(packet.extra().stream_index() as usize);
  Ok(out)
}

/// Builds an `ffmpeg::Packet` from a [`mediadecode::AudioPacket`].
/// Same shape as [`ffmpeg_packet_from_video_packet`] — bytes are
/// copied; pts/dts/duration/flags/stream_index and side data are
/// forwarded. Same failure modes.
pub fn ffmpeg_packet_from_audio_packet(
  packet: &mediadecode::packet::AudioPacket<AudioPacketExtra, crate::FfmpegBuffer>,
  limits: PacketLimits,
) -> std::result::Result<Packet, PacketBuildError> {
  build_audio_packet::<crate::View>(packet, limits, BodyRoute::Copy)
}

/// [`ffmpeg_packet_from_audio_packet`] on the owned lane.
pub fn ffmpeg_packet_from_owned_audio_packet(
  packet: &mediadecode::packet::AudioPacket<AudioPacketExtra, FfmpegBytes>,
  limits: PacketLimits,
) -> std::result::Result<Packet, PacketBuildError> {
  build_audio_packet::<crate::Owned>(packet, limits, BodyRoute::Copy)
}

/// [`ffmpeg_packet_from_audio_packet`] built for **submission**, and
/// scoped so the packet cannot outlive the call.
///
/// This is the only road on which the view lane may hand a decoder
/// its own buffer rather than a copy, and the scoping is why it may.
/// An `ffmpeg_next::Packet` lends `&mut [u8]` through `data_mut`,
/// while the carrier it was built from still lends `&[u8]` — so a
/// shared body reachable from a value a caller *holds* is two live
/// references to one allocation with one of them mutable, out of
/// entirely safe code, and `!Sync` would not save it either. Here the
/// packet is built, lent to `submit` as `&Packet`, and dropped before
/// this function returns: no value that can produce a `&mut` into the
/// shared bytes ever exists.
pub(crate) fn with_ffmpeg_audio_packet<C: crate::FfmpegCarrier + crate::CarrierOps, T>(
  packet: &mediadecode::packet::AudioPacket<AudioPacketExtra, C::Buffer>,
  limits: PacketLimits,
  route: BodyRoute,
  submit: impl FnOnce(&Packet) -> T,
) -> std::result::Result<T, PacketBuildError> {
  let av_packet = build_audio_packet::<C>(packet, limits, route)?;
  let out = submit(&av_packet);
  drop(av_packet);
  Ok(out)
}

fn build_audio_packet<C: crate::FfmpegCarrier + crate::CarrierOps>(
  packet: &mediadecode::packet::AudioPacket<AudioPacketExtra, C::Buffer>,
  limits: PacketLimits,
  route: BodyRoute,
) -> std::result::Result<Packet, PacketBuildError> {
  let body = packet.data().as_ref();
  // Before the budget and before the allocation: an uncarriable
  // payload is not made carriable by fitting.
  refuse_trusted(packet.flags(), body.len())?;
  // And the annotations, judged whole before the body is allocated —
  // a list that cannot be carried should not cost a packet first.
  check_side_data_budget(packet.extra().side_data())?;
  // **The into-FFmpeg budget, before the allocation.** `try_packet_copy`
  // duplicates these bytes into a fresh `AVPacket` and checks only that
  // they fit `c_int`. Without this the configured ceiling was dead on
  // the road a caller feeds a decoder directly: bytes the demux
  // boundary would have refused went straight into libavcodec.
  if body.len() > limits.max_packet_bytes() {
    return Err(PacketBuildError::SendPayloadTooLarge(
      SendPayloadTooLarge::new(body.len(), limits.max_packet_bytes()),
    ));
  }
  let mut out = C::packet_body(packet.data(), route)?;
  attach_side_data(&mut out, packet.extra().side_data())?;
  if let Some(ts) = packet.pts() {
    out.set_pts(Some(ts.pts()));
  }
  if let Some(ts) = packet.dts() {
    out.set_dts(Some(ts.pts()));
  }
  if let Some(d) = packet.duration() {
    out.set_duration(d.pts());
  }
  // SAFETY: `out` owns the `AVPacket` `try_packet_copy` just built.
  unsafe { write_md_flags(&mut out, packet.flags()) };
  out.set_stream(packet.extra().stream_index() as usize);
  Ok(out)
}

/// Builds an `ffmpeg::Packet` from a [`mediadecode::SubtitlePacket`].
/// Bytes copied; pts/duration/flags/stream_index and side data
/// forwarded. Subtitle packets have no `dts` in the mediadecode model.
/// Same failure modes as [`ffmpeg_packet_from_video_packet`].
pub fn ffmpeg_packet_from_subtitle_packet(
  packet: &mediadecode::packet::SubtitlePacket<SubtitlePacketExtra, crate::FfmpegBuffer>,
  limits: PacketLimits,
) -> std::result::Result<Packet, PacketBuildError> {
  build_subtitle_packet::<crate::View>(packet, limits, BodyRoute::Copy)
}

/// [`ffmpeg_packet_from_subtitle_packet`] on the owned lane.
pub fn ffmpeg_packet_from_owned_subtitle_packet(
  packet: &mediadecode::packet::SubtitlePacket<SubtitlePacketExtra, FfmpegBytes>,
  limits: PacketLimits,
) -> std::result::Result<Packet, PacketBuildError> {
  build_subtitle_packet::<crate::Owned>(packet, limits, BodyRoute::Copy)
}

/// [`ffmpeg_packet_from_subtitle_packet`] built for **submission**, and
/// scoped so the packet cannot outlive the call.
///
/// This is the only road on which the view lane may hand a decoder
/// its own buffer rather than a copy, and the scoping is why it may.
/// An `ffmpeg_next::Packet` lends `&mut [u8]` through `data_mut`,
/// while the carrier it was built from still lends `&[u8]` — so a
/// shared body reachable from a value a caller *holds* is two live
/// references to one allocation with one of them mutable, out of
/// entirely safe code, and `!Sync` would not save it either. Here the
/// packet is built, lent to `submit` as `&Packet`, and dropped before
/// this function returns: no value that can produce a `&mut` into the
/// shared bytes ever exists.
pub(crate) fn with_ffmpeg_subtitle_packet<C: crate::FfmpegCarrier + crate::CarrierOps, T>(
  packet: &mediadecode::packet::SubtitlePacket<SubtitlePacketExtra, C::Buffer>,
  limits: PacketLimits,
  route: BodyRoute,
  submit: impl FnOnce(&Packet) -> T,
) -> std::result::Result<T, PacketBuildError> {
  let av_packet = build_subtitle_packet::<C>(packet, limits, route)?;
  let out = submit(&av_packet);
  drop(av_packet);
  Ok(out)
}

fn build_subtitle_packet<C: crate::FfmpegCarrier + crate::CarrierOps>(
  packet: &mediadecode::packet::SubtitlePacket<SubtitlePacketExtra, C::Buffer>,
  limits: PacketLimits,
  route: BodyRoute,
) -> std::result::Result<Packet, PacketBuildError> {
  let body = packet.data().as_ref();
  // Before the budget and before the allocation: an uncarriable
  // payload is not made carriable by fitting.
  refuse_trusted(packet.flags(), body.len())?;
  // And the annotations, judged whole before the body is allocated —
  // a list that cannot be carried should not cost a packet first.
  check_side_data_budget(packet.extra().side_data())?;
  // **The into-FFmpeg budget, before the allocation.** `try_packet_copy`
  // duplicates these bytes into a fresh `AVPacket` and checks only that
  // they fit `c_int`. Without this the configured ceiling was dead on
  // the road a caller feeds a decoder directly: bytes the demux
  // boundary would have refused went straight into libavcodec.
  if body.len() > limits.max_packet_bytes() {
    return Err(PacketBuildError::SendPayloadTooLarge(
      SendPayloadTooLarge::new(body.len(), limits.max_packet_bytes()),
    ));
  }
  let mut out = C::packet_body(packet.data(), route)?;
  attach_side_data(&mut out, packet.extra().side_data())?;
  if let Some(ts) = packet.pts() {
    out.set_pts(Some(ts.pts()));
  }
  if let Some(d) = packet.duration() {
    out.set_duration(d.pts());
  }
  // SAFETY: `out` owns the `AVPacket` `try_packet_copy` just built.
  unsafe { write_md_flags(&mut out, packet.flags()) };
  out.set_stream(packet.extra().stream_index() as usize);
  Ok(out)
}

// ---------------------------------------------------------------------------
//  Safe wrappers — `&ffmpeg::Packet` → `mediadecode::*Packet`.
// ---------------------------------------------------------------------------

/// Carries a borrowed [`ffmpeg::Packet`] out as a
/// [`mediadecode::packet::VideoPacket`] on the **view** lane: the
/// payload is a refcounted window onto the source `AVPacket`'s own
/// buffer, which is why the delivered packet's lifetime is answerable
/// to libavformat's — see [the two carrier lanes][lanes]. For a packet
/// that must outlive the demuxer, ask for the owned lane by name:
/// `video_packet_from_ffmpeg_as::<Owned>`, whose payload is copied and
/// which the [D-seat amputation contract][law] governs.
///
/// Timestamps, duration, key/corrupt flags, and the source stream index
/// are forwarded to the produced packet.
///
/// Uses the default [`PacketLimits`]; the `_in` sibling takes them
/// explicitly, alongside the stream's timebase.
///
/// [lanes]: mediadecode::adapter#the-two-carrier-lanes
///
/// Returns `Ok(None)` when the source packet has no payload at all
/// (an empty packet — typical after EOF), and [`PacketBufferError`]
/// when a payload that *is* there could not be carried — over budget,
/// or claiming bytes outside its own buffer. Those are never the same
/// answer. Caller can also fill in [`VideoPacketExtra::byte_pos`] /
/// `side_data` post-construction if they need those.
///
/// [law]: mediadecode::adapter#the-d-seat-amputation-contract
pub fn video_packet_from_ffmpeg(
  packet: &Packet,
) -> Result<Option<VideoPacket<VideoPacketExtra, FfmpegBytes>>, PacketBufferError> {
  video_packet_from_borrowed::<crate::Owned>(
    packet,
    mediadecode::Timebase::default(),
    PacketLimits::default(),
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

/// Carries a borrowed [`ffmpeg::Packet`] out as a
/// [`mediadecode::packet::AudioPacket`]. Same shape as
/// [`video_packet_from_ffmpeg`] — shared payload, forwarded metadata,
/// default budgets.
pub fn audio_packet_from_ffmpeg(
  packet: &Packet,
) -> Result<Option<AudioPacket<AudioPacketExtra, FfmpegBytes>>, PacketBufferError> {
  audio_packet_from_borrowed::<crate::Owned>(
    packet,
    mediadecode::Timebase::default(),
    PacketLimits::default(),
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

/// Carries a borrowed [`ffmpeg::Packet`] out as a
/// [`mediadecode::packet::SubtitlePacket`]. Subtitle packets have no
/// `dts` in the mediadecode model; everything else mirrors
/// [`video_packet_from_ffmpeg`], shared payload included.
pub fn subtitle_packet_from_ffmpeg(
  packet: &Packet,
) -> Result<Option<SubtitlePacket<SubtitlePacketExtra, FfmpegBytes>>, PacketBufferError> {
  subtitle_packet_from_borrowed::<crate::Owned>(
    packet,
    mediadecode::Timebase::default(),
    PacketLimits::default(),
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

/// The most side-data entries this crate will walk on one packet.
///
/// The floor is [`SIDE_DATA_MAX_ENTRIES`], the same bound the
/// frame-side collector uses; it rises with `AV_PKT_DATA_NB` so a
/// future FFmpeg that names more side-data types than the floor cannot
/// turn a legitimate packet into a refusal. Measured: FFmpeg's own
/// packet API cannot exceed one entry per named type — both
/// `av_packet_new_side_data` and `av_packet_add_side_data` replace an
/// existing entry of the same type — so a packet over this cap is one
/// no FFmpeg call produced.
fn side_data_entry_cap() -> usize {
  SIDE_DATA_MAX_ENTRIES.max(crate::ffi::side_data_type_count().max(0) as usize)
}

/// The side-data entries an `AVPacket` carries, copied into owned
/// values — **all of them, or none and an error**.
///
/// The packet twin of `convert::collect_side_data`, and bounded the
/// same way: at most [`side_data_entry_cap`] entries and
/// [`SIDE_DATA_MAX_TOTAL_BYTES`] bytes per packet, allocated through
/// `try_reserve_exact`. What is *not* the same is what happens when a
/// bound is reached. The frame collector truncates and warns, which it
/// can afford to — frame side data is descriptive, the frame is
/// delivered either way, and nothing downstream acts on it. Packet side
/// data is the opposite: `NEW_EXTRADATA` replaces a decoder's
/// parameters, `PARAM_CHANGE` moves its rate, `SKIP_SAMPLES` trims the
/// stream, and the codec acts on every one. A truncated copy is a
/// decoder quietly running on stale parameters, and a truncated copy of
/// a side-data-only packet is `Ok(None)` — the packet vanishing
/// entirely, which is the very defect this seam was built to close. So
/// every bound here is an error, and a caller either gets a packet with
/// all of its side data or a `DemuxError` naming what stopped it.
///
/// `AVPacket.side_data` is a flat array of `AVPacketSideData` (not the
/// array of pointers an `AVFrame` keeps), and its `type_` is read as
/// the integer it is on the wire — a discriminant this build has no
/// name for would be undefined behaviour the moment it existed as an
/// `AVPacketSideDataType`.
fn packet_side_data(packet: &Packet) -> Result<Vec<SideDataEntry>, PacketBufferError> {
  use ffmpeg_next::packet::Ref;
  // SAFETY: `packet` keeps the `AVPacket` live; `side_data` and
  // `side_data_elems` are public fields.
  let count_raw = unsafe { (*packet.as_ptr()).side_data_elems };
  let entries = unsafe { (*packet.as_ptr()).side_data };
  // Zero entries is the only shape that means "no side data". Every
  // other reading of that answer — a malformed count, a missing array —
  // is judged, and judged *before* the pointer: a count this crate
  // cannot walk stays an error whether or not the array happens to be
  // null, and a null array with entries to read is malformed rather
  // than empty. Both used to leave here as `Ok(vec![])`, which is the
  // silent loss the caps taught us to name, reached through the
  // pointer instead of the budget.
  if count_raw == 0 {
    return Ok(Vec::new());
  }
  let cap = side_data_entry_cap();
  if count_raw < 0 || count_raw as usize > cap {
    return Err(PacketBufferError::SideDataEntries(SideDataEntries::new(
      count_raw, cap,
    )));
  }
  if entries.is_null() {
    return Err(PacketBufferError::SideDataArray(SideDataArray::new(
      count_raw,
    )));
  }
  let count = count_raw as usize;
  let mut out: Vec<SideDataEntry> = Vec::new();
  if out.try_reserve_exact(count).is_err() {
    return Err(PacketBufferError::SideDataAlloc(BufferSideDataAlloc::new(
      count * core::mem::size_of::<SideDataEntry>(),
    )));
  }
  let mut total_bytes: usize = 0;
  for index in 0..count {
    // SAFETY: `entries` is valid for `count_raw` contiguous
    // `AVPacketSideData` values per FFmpeg's contract, and `index` is
    // below that count.
    let entry = unsafe { entries.add(index) };
    let kind = unsafe { read_unaligned(addr_of!((*entry).type_).cast::<i32>()) };
    let size = unsafe { (*entry).size };
    let data_ptr = unsafe { (*entry).data };
    let data = if size == 0 {
      // A marker entry: a type and no bytes. FFmpeg emits these, and
      // there is nothing to carry or to charge the budget for.
      FfmpegBytes::empty()
    } else if data_ptr.is_null() {
      // Bytes declared and not carried. Reading this as an empty entry
      // delivered a packet whose side data was a lie, and charged the
      // budget nothing for it.
      return Err(PacketBufferError::SideDataPayload(SideDataPayload::new(
        index, size,
      )));
    } else {
      total_bytes = total_bytes.saturating_add(size);
      if total_bytes > SIDE_DATA_MAX_TOTAL_BYTES {
        return Err(PacketBufferError::SideDataBytes(SideDataBytes::new(
          total_bytes,
          SIDE_DATA_MAX_TOTAL_BYTES,
        )));
      }
      let mut buf: Vec<u8> = Vec::new();
      if buf.try_reserve_exact(size).is_err() {
        return Err(PacketBufferError::SideDataAlloc(BufferSideDataAlloc::new(
          size,
        )));
      }
      // SAFETY: `data` is valid for `size` bytes per FFmpeg's
      // `AVPacketSideData` contract.
      buf.extend_from_slice(unsafe { core::slice::from_raw_parts(data_ptr, size) });
      // Staged through the `Vec` so `try_reserve_exact` keeps *one* of
      // the two payload-sized allocations a named refusal rather than
      // an abort. The carrier copy that follows is a second full
      // allocation of the same size — not a header — and it is
      // infallible, so what the staging really buys is that the first
      // and larger risk is reportable and the second is asked for a
      // size the allocator has just proved it has. The doubling is
      // affordable only because side data is capped at
      // [`SIDE_DATA_MAX_TOTAL_BYTES`]; the plane paths, which are not
      // small, use the one-allocation road instead.
      FfmpegBytes::copy_from_slice(&buf)
    };
    out.push(SideDataEntry::new(kind, data));
  }
  Ok(out)
}

/// The buffer a timed packet is delivered with, or `None` when there is
/// no packet to deliver at all.
///
/// **`size == 0` is not the same as "nothing".** A packet with no body
/// and one or more side-data entries is a real packet: FFmpeg uses that
/// shape for `AV_PKT_DATA_NEW_EXTRADATA` and for a parameter change,
/// and a decoder that never sees it keeps decoding on parameters the
/// container has already replaced. Such a packet is delivered with an
/// owned empty buffer — zero bytes, but a buffer, so the packet exists
/// and its side data rides the extras.
///
/// `Ok(None)` therefore means the packet carried neither a payload nor
/// side data: the empty marker, and the only thing a pull loop may skip.
fn delivered_payload<C: crate::FfmpegCarrier + crate::CarrierOps>(
  packet: &Packet,
  side_data: &[SideDataEntry],
  limits: PacketLimits,
  provenance: crate::buffer::PayloadProvenance,
) -> Result<Option<C::Buffer>, PacketBufferError> {
  use ffmpeg_next::packet::Ref;
  // SAFETY: `packet` keeps the AVPacket live for the duration of this
  // call, which is all `payload_of` requires.
  if let Some(bytes) = unsafe {
    crate::buffer::payload_of::<C>(packet.as_ptr(), limits.max_packet_bytes(), provenance)
  }? {
    return Ok(Some(bytes));
  }
  if side_data.is_empty() {
    return Ok(None);
  }
  Ok(Some(C::empty()))
}

// ---------------------------------------------------------------------------
//  Timebase-carrying variants.
//
//  An `AVPacket`'s timestamps are integers in its *stream's* timebase,
//  which the packet does not carry — the four functions above therefore
//  stamp `Timebase::default()` (1/1), leaving the caller to know what
//  the ticks meant. A demuxer knows: it holds the track table. These
//  variants take that timebase, so the produced `Timestamp` is a
//  complete, self-describing value.
// ---------------------------------------------------------------------------

/// [`video_packet_from_ffmpeg`], with the stream's timebase stamped
/// onto every timestamp instead of the 1/1 placeholder.
pub(crate) fn video_packet_from_ffmpeg_as<C: crate::FfmpegCarrier + crate::CarrierOps>(
  source: Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
  provenance: crate::buffer::PayloadProvenance,
) -> Result<Option<VideoPacket<VideoPacketExtra, C::Buffer>>, PacketBufferError> {
  // **The source is consumed.** A borrowed `AVPacket` cannot be
  // safely viewed: `ffmpeg_next::Packet` lends `&mut [u8]` through
  // `data_mut` and shares its buffer by refcount with no
  // copy-on-write, so a caller who kept the packet would hold a
  // mutable alias of every byte the returned carrier reads — and
  // both sides are `Send`. Taking it by value is what makes that
  // unconstructible; the packet is released when this returns, and
  // the view lane's carrier keeps the buffer alive by its own
  // reference. See the borrowed siblings for the owned lane, which
  // copies and so may borrow.
  video_packet_from_borrowed::<C>(&source, time_base, limits, provenance)
}

/// The shared implementation of the video road.
///
/// Crate-private, and borrowed: the two public doors differ only in
/// what they owe the borrow checker. The consuming one may be asked
/// for either lane; the borrowing one is the owned lane, where the
/// bytes are copied and the source is nobody's concern afterwards.
pub(crate) fn video_packet_from_borrowed<C: crate::FfmpegCarrier + crate::CarrierOps>(
  packet: &Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
  provenance: crate::buffer::PayloadProvenance,
) -> Result<Option<VideoPacket<VideoPacketExtra, C::Buffer>>, PacketBufferError> {
  let side_data = packet_side_data(packet)?;
  let Some(buf) = delivered_payload::<C>(packet, &side_data, limits, provenance)? else {
    return Ok(None);
  };
  let extra = VideoPacketExtra::new(packet.stream() as i32).with_side_data(side_data);
  let mut out = VideoPacket::new(buf, extra)
    .with_flags(md_flags_from_packet(packet)?)
    .with_pts(packet.pts().map(|p| Timestamp::new(p, time_base)))
    .with_dts(packet.dts().map(|d| Timestamp::new(d, time_base)));
  let dur = packet.duration();
  if dur > 0 {
    out = out.with_duration(Some(Timestamp::new(dur, time_base)));
  }
  Ok(Some(out))
}

/// [`audio_packet_from_ffmpeg`], with the stream's timebase stamped
/// onto every timestamp instead of the 1/1 placeholder.
pub(crate) fn audio_packet_from_ffmpeg_as<C: crate::FfmpegCarrier + crate::CarrierOps>(
  source: Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
  provenance: crate::buffer::PayloadProvenance,
) -> Result<Option<AudioPacket<AudioPacketExtra, C::Buffer>>, PacketBufferError> {
  // **The source is consumed.** A borrowed `AVPacket` cannot be
  // safely viewed: `ffmpeg_next::Packet` lends `&mut [u8]` through
  // `data_mut` and shares its buffer by refcount with no
  // copy-on-write, so a caller who kept the packet would hold a
  // mutable alias of every byte the returned carrier reads — and
  // both sides are `Send`. Taking it by value is what makes that
  // unconstructible; the packet is released when this returns, and
  // the view lane's carrier keeps the buffer alive by its own
  // reference. See the borrowed siblings for the owned lane, which
  // copies and so may borrow.
  audio_packet_from_borrowed::<C>(&source, time_base, limits, provenance)
}

/// The shared implementation of the audio road.
///
/// Crate-private, and borrowed: the two public doors differ only in
/// what they owe the borrow checker. The consuming one may be asked
/// for either lane; the borrowing one is the owned lane, where the
/// bytes are copied and the source is nobody's concern afterwards.
pub(crate) fn audio_packet_from_borrowed<C: crate::FfmpegCarrier + crate::CarrierOps>(
  packet: &Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
  provenance: crate::buffer::PayloadProvenance,
) -> Result<Option<AudioPacket<AudioPacketExtra, C::Buffer>>, PacketBufferError> {
  let side_data = packet_side_data(packet)?;
  let Some(buf) = delivered_payload::<C>(packet, &side_data, limits, provenance)? else {
    return Ok(None);
  };
  let extra = AudioPacketExtra::new(packet.stream() as i32).with_side_data(side_data);
  let mut out = AudioPacket::new(buf, extra)
    .with_flags(md_flags_from_packet(packet)?)
    .with_pts(packet.pts().map(|p| Timestamp::new(p, time_base)))
    .with_dts(packet.dts().map(|d| Timestamp::new(d, time_base)));
  let dur = packet.duration();
  if dur > 0 {
    out = out.with_duration(Some(Timestamp::new(dur, time_base)));
  }
  Ok(Some(out))
}

/// [`subtitle_packet_from_ffmpeg`], with the stream's timebase stamped
/// onto every timestamp instead of the 1/1 placeholder.
pub(crate) fn subtitle_packet_from_ffmpeg_as<C: crate::FfmpegCarrier + crate::CarrierOps>(
  source: Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
  provenance: crate::buffer::PayloadProvenance,
) -> Result<Option<SubtitlePacket<SubtitlePacketExtra, C::Buffer>>, PacketBufferError> {
  // **The source is consumed.** A borrowed `AVPacket` cannot be
  // safely viewed: `ffmpeg_next::Packet` lends `&mut [u8]` through
  // `data_mut` and shares its buffer by refcount with no
  // copy-on-write, so a caller who kept the packet would hold a
  // mutable alias of every byte the returned carrier reads — and
  // both sides are `Send`. Taking it by value is what makes that
  // unconstructible; the packet is released when this returns, and
  // the view lane's carrier keeps the buffer alive by its own
  // reference. See the borrowed siblings for the owned lane, which
  // copies and so may borrow.
  subtitle_packet_from_borrowed::<C>(&source, time_base, limits, provenance)
}

/// The shared implementation of the subtitle road.
///
/// Crate-private, and borrowed: the two public doors differ only in
/// what they owe the borrow checker. The consuming one may be asked
/// for either lane; the borrowing one is the owned lane, where the
/// bytes are copied and the source is nobody's concern afterwards.
pub(crate) fn subtitle_packet_from_borrowed<C: crate::FfmpegCarrier + crate::CarrierOps>(
  packet: &Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
  provenance: crate::buffer::PayloadProvenance,
) -> Result<Option<SubtitlePacket<SubtitlePacketExtra, C::Buffer>>, PacketBufferError> {
  let side_data = packet_side_data(packet)?;
  let Some(buf) = delivered_payload::<C>(packet, &side_data, limits, provenance)? else {
    return Ok(None);
  };
  let extra = SubtitlePacketExtra::new(packet.stream() as i32).with_side_data(side_data);
  let mut out = SubtitlePacket::new(buf, extra)
    .with_flags(md_flags_from_packet(packet)?)
    .with_pts(packet.pts().map(|p| Timestamp::new(p, time_base)));
  let dur = packet.duration();
  if dur > 0 {
    out = out.with_duration(Some(Timestamp::new(dur, time_base)));
  }
  Ok(Some(out))
}

/// Wraps a borrowed [`ffmpeg::Packet`] from a **data** track — timecode,
/// KLV, timed ID3 — as a [`mediadecode::demuxer::DataPacket`], with the
/// stream's timebase stamped onto every timestamp.
///
/// Data packets are never reordered, so the mediadecode model gives
/// them no `dts` seat; everything else mirrors
/// [`video_packet_from_ffmpeg_in`]. `byte_pos` is forwarded from
/// `AVPacket.pos`, which data consumers use to correlate a payload with
/// its position in the file.
pub(crate) fn data_packet_from_ffmpeg_as<C: crate::FfmpegCarrier + crate::CarrierOps>(
  source: Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
  provenance: crate::buffer::PayloadProvenance,
) -> Result<Option<DataPacket<DataPacketExtra, C::Buffer>>, PacketBufferError> {
  // **The source is consumed.** A borrowed `AVPacket` cannot be
  // safely viewed: `ffmpeg_next::Packet` lends `&mut [u8]` through
  // `data_mut` and shares its buffer by refcount with no
  // copy-on-write, so a caller who kept the packet would hold a
  // mutable alias of every byte the returned carrier reads — and
  // both sides are `Send`. Taking it by value is what makes that
  // unconstructible; the packet is released when this returns, and
  // the view lane's carrier keeps the buffer alive by its own
  // reference. See the borrowed siblings for the owned lane, which
  // copies and so may borrow.
  data_packet_from_borrowed::<C>(&source, time_base, limits, provenance)
}

/// The shared implementation of the data road.
///
/// Crate-private, and borrowed: the two public doors differ only in
/// what they owe the borrow checker. The consuming one may be asked
/// for either lane; the borrowing one is the owned lane, where the
/// bytes are copied and the source is nobody's concern afterwards.
pub(crate) fn data_packet_from_borrowed<C: crate::FfmpegCarrier + crate::CarrierOps>(
  packet: &Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
  provenance: crate::buffer::PayloadProvenance,
) -> Result<Option<DataPacket<DataPacketExtra, C::Buffer>>, PacketBufferError> {
  let side_data = packet_side_data(packet)?;
  let Some(buf) = delivered_payload::<C>(packet, &side_data, limits, provenance)? else {
    return Ok(None);
  };
  let pos = packet.position();
  let extra = DataPacketExtra::new(packet.stream() as i32)
    .with_byte_pos((pos >= 0).then_some(pos as i64))
    .with_side_data(side_data);
  let mut out = DataPacket::new(buf, extra)
    .with_flags(md_flags_from_packet(packet)?)
    .with_pts(packet.pts().map(|p| Timestamp::new(p, time_base)));
  let dur = packet.duration();
  if dur > 0 {
    out = out.with_duration(Some(Timestamp::new(dur, time_base)));
  }
  Ok(Some(out))
}

/// Wraps a borrowed [`ffmpeg::Packet`] from an **attachment** track —
/// cover art that the container really does store as a packet — as a
/// [`mediadecode::demuxer::AttachmentPacket`].
///
/// No timestamps are forwarded, and there is nowhere to put them: an
/// attachment is not on the timeline. `synthesized` is `false`, because
/// this payload came from a real packet; the demuxer sets it `true` for
/// the packets it builds out of codec extradata.
///
/// **The one arm where a payload-less packet really is nothing.** The
/// four timed conversions deliver a side-data-only packet, because
/// codec-control side data is content a decoder must see. An attachment
/// is not decoded and is not on a timeline: it *is* its bytes, so a
/// packet carrying none carries no attachment, and this answers
/// `Ok(None)`. `AttachmentPacketExtra` accordingly has no side-data
/// seat — a deliberate absence, not an oversight.
pub(crate) fn attachment_packet_from_ffmpeg_as<C: crate::FfmpegCarrier + crate::CarrierOps>(
  source: Packet,
  limits: PacketLimits,
  provenance: crate::buffer::PayloadProvenance,
) -> Result<Option<AttachmentPacket<AttachmentPacketExtra, C::Buffer>>, PacketBufferError> {
  // **The source is consumed.** A borrowed `AVPacket` cannot be
  // safely viewed: `ffmpeg_next::Packet` lends `&mut [u8]` through
  // `data_mut` and shares its buffer by refcount with no
  // copy-on-write, so a caller who kept the packet would hold a
  // mutable alias of every byte the returned carrier reads — and
  // both sides are `Send`. Taking it by value is what makes that
  // unconstructible; the packet is released when this returns, and
  // the view lane's carrier keeps the buffer alive by its own
  // reference. See the borrowed siblings for the owned lane, which
  // copies and so may borrow.
  attachment_packet_from_borrowed::<C>(&source, limits, provenance)
}

/// The shared implementation of the attachment road.
///
/// Crate-private, and borrowed: the two public doors differ only in
/// what they owe the borrow checker. The consuming one may be asked
/// for either lane; the borrowing one is the owned lane, where the
/// bytes are copied and the source is nobody's concern afterwards.
pub(crate) fn attachment_packet_from_borrowed<C: crate::FfmpegCarrier + crate::CarrierOps>(
  packet: &Packet,
  limits: PacketLimits,
  provenance: crate::buffer::PayloadProvenance,
) -> Result<Option<AttachmentPacket<AttachmentPacketExtra, C::Buffer>>, PacketBufferError> {
  use ffmpeg_next::packet::Ref;
  // SAFETY: `packet` keeps the AVPacket live for the duration of this
  // call, which is all `payload_of` requires.
  let Some(buf) = (unsafe {
    crate::buffer::payload_of::<C>(packet.as_ptr(), limits.max_packet_bytes(), provenance)
  })?
  else {
    return Ok(None);
  };
  Ok(Some(
    AttachmentPacket::new(buf, AttachmentPacketExtra::new(packet.stream() as i32))
      .with_flags(md_flags_from_packet(packet)?),
  ))
}

/// Every flag the packet really carries.
///
/// Read from `AVPacket.flags` as the raw integer rather than through
/// `ffmpeg_next`'s `Packet::flags()`, whose `Flags` bit set names only
/// `KEY` and `CORRUPT` and drops the rest in `from_bits_truncate`.
/// `AV_PKT_FLAG_DISCARD` is among the dropped: it tells a consumer that
/// a packet must be fed to the decoder and its output thrown away, and
/// losing it makes preroll output look like something to keep.
///
/// `PacketFlags` is a bit set whose documented lossless door is
/// `from_bits_retain`, and every packet flag FFmpeg names lives inside
/// the byte it carries — including the three bits nothing names yet.
/// A bit outside that byte cannot be carried at all, and is refused
/// rather than dropped; the assertion below states the fact that keeps
/// the refusal unreachable against this build.
fn md_flags_from_packet(packet: &Packet) -> Result<MdPacketFlags, PacketBufferError> {
  use ffmpeg_next::packet::Ref;
  // SAFETY: `packet` keeps the `AVPacket` live for the call, which is
  // all the raw reader asks.
  unsafe { md_flags_from_av_packet(packet.as_ptr()) }
}

/// [`md_flags_from_packet`] for an `AVPacket` that has no safe wrapper
/// — the one libavformat embeds in an `AVStream` for cover art.
///
/// The demuxer hoists that packet by hand, and hoisting it *without*
/// its flags was how an attached picture arrived with none: FFmpeg
/// marks it `AV_PKT_FLAG_KEY`, which is the one thing a still image is
/// certain to be. One reader, so a second construction site cannot
/// quietly disagree with the five that go through the boundary.
///
/// # Safety
///
/// `pkt` must be a live `*const AVPacket` for the duration of this
/// call.
pub(crate) unsafe fn md_flags_from_av_packet(
  pkt: *const ffmpeg_next::ffi::AVPacket,
) -> Result<MdPacketFlags, PacketBufferError> {
  // SAFETY: `pkt` is live per the contract above; `flags` is a public
  // field and a plain `c_int`.
  let raw = unsafe { (*pkt).flags };
  let carried = i32::from(u8::MAX);
  if raw & !carried != 0 {
    return Err(PacketBufferError::UnrepresentableFlags(
      UnrepresentableFlags::new(raw),
    ));
  }
  Ok(MdPacketFlags::from_bits_retain(raw as u8))
}

/// What a track is, folded from `AVCodecParameters.codec_type` read as
/// the integer it is on the wire.
///
/// The dependency-API half of this crate's open-C-enum discipline.
/// `ffmpeg_next::codec::Parameters::medium()` materialises an
/// `AVMediaType` out of FFmpeg memory to answer the same question, and
/// a value outside this build's discriminant set is undefined behaviour
/// the moment it exists — before any `match` on it can run. That the
/// set is small and has been stable for years is a reason it has not
/// bitten, not a reason it cannot.
///
/// So the read is raw and the fold is total: anything this build does
/// not name becomes [`Unknown`](Self::Unknown), which the callers
/// already had to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, IsVariant)]
pub enum MediaKind {
  /// `AVMEDIA_TYPE_VIDEO`.
  Video,
  /// `AVMEDIA_TYPE_AUDIO`.
  Audio,
  /// `AVMEDIA_TYPE_SUBTITLE`.
  Subtitle,
  /// `AVMEDIA_TYPE_DATA`.
  Data,
  /// `AVMEDIA_TYPE_ATTACHMENT`.
  Attachment,
  /// Anything this build of FFmpeg does not name, `AVMEDIA_TYPE_UNKNOWN`
  /// included.
  Unknown,
}

/// Folds a raw `AVMediaType` integer into [`MediaKind`].
pub(crate) fn media_kind_from_raw(raw: i32) -> MediaKind {
  use ffmpeg_next::ffi::AVMediaType::*;
  match raw {
    x if x == AVMEDIA_TYPE_VIDEO as i32 => MediaKind::Video,
    x if x == AVMEDIA_TYPE_AUDIO as i32 => MediaKind::Audio,
    x if x == AVMEDIA_TYPE_SUBTITLE as i32 => MediaKind::Subtitle,
    x if x == AVMEDIA_TYPE_DATA as i32 => MediaKind::Data,
    x if x == AVMEDIA_TYPE_ATTACHMENT as i32 => MediaKind::Attachment,
    _ => MediaKind::Unknown,
  }
}

/// The medium a set of codec parameters declares, without forming an
/// `AVMediaType`. Replaces every `Parameters::medium()` call in this
/// crate — see [`MediaKind`].
pub(crate) fn media_kind_of(parameters: &ffmpeg_next::codec::Parameters) -> MediaKind {
  // SAFETY: `as_ptr` is `unsafe` only because the pointer must not
  // outlive `parameters`; it is used and discarded inside this call.
  let ptr = unsafe { parameters.as_ptr() };
  if ptr.is_null() {
    return MediaKind::Unknown;
  }
  // SAFETY: `ptr` is a live `*const AVCodecParameters`; `addr_of!`
  // computes the field address without forming a reference, and reading
  // as `i32` matches the bindgen enum's `c_int` storage.
  let raw = unsafe { read_unaligned(addr_of!((*ptr).codec_type) as *const i32) };
  media_kind_from_raw(raw)
}

/// Every packet flag this build of FFmpeg names fits the byte
/// `PacketFlags` carries. If that stops being true, this fails the
/// build rather than letting a flag go missing at run time.
const _: () = {
  assert!(
    (ffmpeg_next::ffi::AV_PKT_FLAG_KEY
      | ffmpeg_next::ffi::AV_PKT_FLAG_CORRUPT
      | ffmpeg_next::ffi::AV_PKT_FLAG_DISCARD
      | ffmpeg_next::ffi::AV_PKT_FLAG_TRUSTED
      | ffmpeg_next::ffi::AV_PKT_FLAG_DISPOSABLE)
      <= u8::MAX as c_int,
    "FFmpeg names a packet flag outside the byte `PacketFlags` carries",
  );
};

// ---------------------------------------------------------------------------
//  Empty-frame placeholders for `receive_frame` destinations.
// ---------------------------------------------------------------------------

/// Constructs an empty [`mediadecode::frame::VideoFrame`] suitable as
/// the destination argument to
/// [`mediadecode::decoder::VideoStreamDecoder::receive_frame`]. The
/// decoder overwrites the frame on success; this just provides a
/// well-formed slot.
///
/// All four plane slots hold the shared empty carrier (the array shape
/// requires a buffer in every slot, but `plane_count = 0` reports them
/// as inactive).
///
/// **Infallible on both lanes, and the `try_` sibling is gone.**
/// Through 0.8 each slot was its own one-byte `AVBufferRef`, so
/// building a placeholder was four FFmpeg allocations that could fail —
/// hence a `try_empty_video_frame` returning `Option` and an
/// `empty_video_frame` that panicked on `None`. Neither lane allocates
/// here now: the owned lane's empty carrier is made once for the
/// process and cloned by refcount, and the view lane's is a null-backed
/// zero-length view, which is what lets the *view* lane keep the same
/// infallible constructor rather than reintroducing 0.8's failure mode
/// under a new name. A constructor with no failure mode does not get to
/// keep a `Result`-shaped door.
pub(crate) fn empty_video_frame_as<C: crate::FfmpegCarrier + crate::CarrierOps>()
-> VideoFrame<PixelFormat, VideoFrameExtra, C::Buffer> {
  VideoFrame::new(
    Dimensions::new(0, 0),
    // mediaframe 0.3's named "no format yet" member, and its
    // `Default` — the state a descriptor is in before a decoder has
    // said what it produces, which is exactly this placeholder.
    PixelFormat::None,
    core::array::from_fn(|_| Plane::new(C::empty(), 0)),
    0,
    VideoFrameExtra::default(),
  )
}

/// Constructs an empty [`mediadecode::frame::AudioFrame`] suitable as
/// the destination argument to
/// [`mediadecode::decoder::AudioStreamDecoder::receive_frame`]. Same
/// behaviour as [`empty_video_frame`] — eight shared empty plane
/// carriers, `plane_count = 0`, and no way to fail.
pub(crate) fn empty_audio_frame_as<C: crate::FfmpegCarrier + crate::CarrierOps>()
-> AudioFrame<SampleFormat, ChannelLayoutDescription, AudioFrameExtra, C::Buffer> {
  AudioFrame::new(
    0,
    0,
    0,
    SampleFormat::NONE,
    ChannelLayoutDescription::default(),
    core::array::from_fn(|_| Plane::new(C::empty(), 0)),
    0,
    AudioFrameExtra::default(),
  )
}

/// Constructs an empty [`mediadecode::frame::SubtitleFrame`] suitable
/// as the destination argument to
/// [`mediadecode::decoder::SubtitleDecoder::receive_frame`]. The
/// payload is an empty `Text` placeholder; the decoder overwrites it
/// on success. Infallible, as its two siblings are.
pub(crate) fn empty_subtitle_frame_as<C: crate::FfmpegCarrier + crate::CarrierOps>()
-> SubtitleFrame<SubtitleFrameExtra, C::Buffer> {
  SubtitleFrame::new(
    SubtitlePayload::Text(SubtitleText::new(C::empty(), None)),
    SubtitleFrameExtra::default(),
  )
}

// --- The bare names, on the view lane --------------------------------
//
// **The signature says the lane, and the compiler enforces it.** A
// conversion that *borrows* its `AVPacket` can only copy: the packet
// type lends `&mut [u8]` and shares its buffer by refcount with no
// copy-on-write, so a caller who still holds the packet holds a mutable
// alias of anything a view would read. A conversion that *consumes* its
// packet can share, because after it returns there is no other handle.
//
// So: **borrow in, owned lane out; move in, either lane out.** The
// `_in` names below take the packet by value and answer with the view
// lane — the ordinary road, where a direct consumer reads a packet in
// place and drops it — while the lane-generic `_as` workers, also
// by value, let a caller ask for either. The borrowing doors are the
// bare `*_packet_from_ffmpeg` names, and they are the owned lane.
//
// A generic function has no default parameter to fall back on (defaults
// are used when a type is *written*, not inferred from a call), so
// these are monomorphic wrappers rather than a default that would never
// apply.

/// [`video_packet_from_ffmpeg_as`] on the view lane, with the
/// stream's timebase stamped onto every timestamp.
///
/// **Takes the packet by value**, and that is the safety property, not
/// a style choice. The program below is the alias this signature
/// forbids — it was accepted when the parameter was a reference, and
/// `data_mut` handed out a `&mut [u8]` over bytes the returned carrier
/// was still lending as `&[u8]`:
///
/// ```compile_fail,E0382
/// use ffmpeg_next::packet::Mut;
/// use mediadecode::Timebase;
/// use mediadecode_ffmpeg::{PacketLimits, video_packet_from_ffmpeg_in};
///
/// let mut packet = ffmpeg_next::Packet::copy(&[0u8; 64]);
/// let viewed = video_packet_from_ffmpeg_in(packet, Timebase::default(), PacketLimits::default());
/// // The source is gone, so this cannot be written.
/// let _aliased = packet.data_mut();
/// ```
///
/// A caller who wants to keep the packet wants the owned lane, which
/// copies and may therefore borrow:
/// [`owned_video_packet_from_ffmpeg_in`].
pub fn video_packet_from_ffmpeg_in(
  packet: Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
) -> Result<Option<VideoPacket<VideoPacketExtra, crate::FfmpegBuffer>>, PacketBufferError> {
  video_packet_from_ffmpeg_as::<crate::View>(
    packet,
    time_base,
    limits,
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

/// [`audio_packet_from_ffmpeg_as`] on the view lane.
pub fn audio_packet_from_ffmpeg_in(
  packet: Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
) -> Result<Option<AudioPacket<AudioPacketExtra, crate::FfmpegBuffer>>, PacketBufferError> {
  audio_packet_from_ffmpeg_as::<crate::View>(
    packet,
    time_base,
    limits,
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

/// [`subtitle_packet_from_ffmpeg_as`] on the view lane.
pub fn subtitle_packet_from_ffmpeg_in(
  packet: Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
) -> Result<Option<SubtitlePacket<SubtitlePacketExtra, crate::FfmpegBuffer>>, PacketBufferError> {
  subtitle_packet_from_ffmpeg_as::<crate::View>(
    packet,
    time_base,
    limits,
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

/// [`data_packet_from_ffmpeg_as`] on the view lane.
pub fn data_packet_from_ffmpeg_in(
  packet: Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
) -> Result<Option<DataPacket<DataPacketExtra, crate::FfmpegBuffer>>, PacketBufferError> {
  data_packet_from_ffmpeg_as::<crate::View>(
    packet,
    time_base,
    limits,
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

// --- The borrowing doors, on the owned lane --------------------------
//
// The owned lane may borrow because it copies: nothing it returns
// points at the source, so the source's fate is its own business.
// These are the shapes the view lane cannot offer, and the reason it
// cannot is the whole of the split — see the note above the `_in`
// family.

/// [`video_packet_from_ffmpeg`] with the stream's timebase and explicit
/// budgets.
pub fn owned_video_packet_from_ffmpeg_in(
  packet: &Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
) -> Result<Option<VideoPacket<VideoPacketExtra, FfmpegBytes>>, PacketBufferError> {
  video_packet_from_borrowed::<crate::Owned>(
    packet,
    time_base,
    limits,
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

/// [`audio_packet_from_ffmpeg`] with the stream's timebase and explicit
/// budgets.
pub fn owned_audio_packet_from_ffmpeg_in(
  packet: &Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
) -> Result<Option<AudioPacket<AudioPacketExtra, FfmpegBytes>>, PacketBufferError> {
  audio_packet_from_borrowed::<crate::Owned>(
    packet,
    time_base,
    limits,
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

/// [`subtitle_packet_from_ffmpeg`] with the stream's timebase and
/// explicit budgets.
pub fn owned_subtitle_packet_from_ffmpeg_in(
  packet: &Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
) -> Result<Option<SubtitlePacket<SubtitlePacketExtra, FfmpegBytes>>, PacketBufferError> {
  subtitle_packet_from_borrowed::<crate::Owned>(
    packet,
    time_base,
    limits,
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

/// [`data_packet_from_ffmpeg_in`] on the owned lane, borrowing its
/// source.
pub fn owned_data_packet_from_ffmpeg_in(
  packet: &Packet,
  time_base: mediadecode::Timebase,
  limits: PacketLimits,
) -> Result<Option<DataPacket<DataPacketExtra, FfmpegBytes>>, PacketBufferError> {
  data_packet_from_borrowed::<crate::Owned>(
    packet,
    time_base,
    limits,
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

/// [`attachment_packet_from_ffmpeg`] on the owned lane, borrowing its
/// source.
pub fn owned_attachment_packet_from_ffmpeg(
  packet: &Packet,
  limits: PacketLimits,
) -> Result<Option<AttachmentPacket<AttachmentPacketExtra, FfmpegBytes>>, PacketBufferError> {
  attachment_packet_from_borrowed::<crate::Owned>(
    packet,
    limits,
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

/// [`empty_video_frame_as`] on the view lane — the destination a
/// [`FfmpegVideoStreamDecoder`](crate::FfmpegVideoStreamDecoder) fills.
#[must_use]
pub fn empty_video_frame() -> VideoFrame<PixelFormat, VideoFrameExtra, crate::FfmpegBuffer> {
  empty_video_frame_as::<crate::View>()
}

/// [`empty_video_frame_as`] on the owned lane.
#[must_use]
pub fn empty_owned_video_frame() -> VideoFrame<PixelFormat, VideoFrameExtra, FfmpegBytes> {
  empty_video_frame_as::<crate::Owned>()
}

/// [`empty_audio_frame_as`] on the view lane.
#[must_use]
pub fn empty_audio_frame()
-> AudioFrame<SampleFormat, ChannelLayoutDescription, AudioFrameExtra, crate::FfmpegBuffer> {
  empty_audio_frame_as::<crate::View>()
}

/// [`empty_audio_frame_as`] on the owned lane.
#[must_use]
pub fn empty_owned_audio_frame()
-> AudioFrame<SampleFormat, ChannelLayoutDescription, AudioFrameExtra, FfmpegBytes> {
  empty_audio_frame_as::<crate::Owned>()
}

/// [`empty_subtitle_frame_as`] on the view lane.
#[must_use]
pub fn empty_subtitle_frame() -> SubtitleFrame<SubtitleFrameExtra, crate::FfmpegBuffer> {
  empty_subtitle_frame_as::<crate::View>()
}

/// [`empty_subtitle_frame_as`] on the owned lane.
#[must_use]
pub fn empty_owned_subtitle_frame() -> SubtitleFrame<SubtitleFrameExtra, FfmpegBytes> {
  empty_subtitle_frame_as::<crate::Owned>()
}

/// [`attachment_packet_from_ffmpeg_as`] on the view lane.
pub fn attachment_packet_from_ffmpeg(
  packet: Packet,
  limits: PacketLimits,
) -> Result<Option<AttachmentPacket<AttachmentPacketExtra, crate::FfmpegBuffer>>, PacketBufferError>
{
  attachment_packet_from_ffmpeg_as::<crate::View>(
    packet,
    limits,
    crate::buffer::PayloadProvenance::CallerSupplied,
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A refcounted packet whose header claims a payload larger than the
  /// buffer behind it — the shape a malformed container, or an
  /// `av_packet_split_side_data` gone wrong, produces. Its payload is
  /// *there* and cannot be wrapped, which is exactly the case that must
  /// not read as "empty".
  fn out_of_bounds_packet() -> Packet {
    use ffmpeg_next::packet::Mut;
    let mut packet = Packet::copy(&[1u8, 2, 3, 4]);
    // SAFETY: `packet` owns a live `AVPacket` with a refcounted buffer;
    // `size` is a public field.
    unsafe {
      (*packet.as_mut_ptr()).size = 1 << 20;
    }
    packet
  }

  #[test]
  fn a_payload_that_cannot_be_wrapped_is_an_error_on_every_arm() {
    // All five delivery arms plus the attachment path go through the
    // same wrapper. If one of them still folded the failure into
    // `None`, the demuxer would drop that kind of packet in silence.
    let forged = out_of_bounds_packet();
    let tb = mediadecode::Timebase::default();
    assert!(matches!(
      video_packet_from_borrowed::<crate::Owned>(
        &forged,
        tb,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied
      ),
      Err(PacketBufferError::Bounds(_)),
    ));
    assert!(matches!(
      audio_packet_from_borrowed::<crate::Owned>(
        &forged,
        tb,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied
      ),
      Err(PacketBufferError::Bounds(_)),
    ));
    assert!(matches!(
      subtitle_packet_from_borrowed::<crate::Owned>(
        &forged,
        tb,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied
      ),
      Err(PacketBufferError::Bounds(_)),
    ));
    assert!(matches!(
      data_packet_from_borrowed::<crate::Owned>(
        &forged,
        tb,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied
      ),
      Err(PacketBufferError::Bounds(_)),
    ));
    assert!(matches!(
      attachment_packet_from_borrowed::<crate::Owned>(
        &forged,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied
      ),
      Err(PacketBufferError::Bounds(_)),
    ));
  }

  /// A packet with **no body** and one side-data entry — the shape
  /// FFmpeg uses to hand a decoder new extradata or a parameter change.
  /// `size` is 0 and `buf` is null, which is exactly what used to read
  /// as "empty, skip it".
  fn side_data_only_packet() -> Packet {
    use ffmpeg_next::{
      ffi::{AVPacketSideDataType, av_packet_new_side_data},
      packet::Mut,
    };
    let mut packet = Packet::empty();
    // SAFETY: `packet` owns a live `AVPacket`; the side-data type is a
    // compile-time constant of this build, so no invalid discriminant
    // is formed. The returned pointer is valid for the four bytes just
    // allocated.
    unsafe {
      let ptr = av_packet_new_side_data(
        packet.as_mut_ptr(),
        AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
        4,
      );
      assert!(!ptr.is_null(), "av_packet_new_side_data");
      core::ptr::copy_nonoverlapping([1u8, 2, 3, 4].as_ptr(), ptr, 4);
    }
    packet
  }

  const NEW_EXTRADATA: i32 =
    ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA as i32;

  use ffmpeg_next::ffi::{AVPacketSideData, AVPacketSideDataType, av_malloc};

  /// A packet carrying one side-data entry of exactly `size` bytes,
  /// with a body when `body` is set.
  fn packet_with_side_data(size: usize, body: bool) -> Packet {
    use ffmpeg_next::{
      ffi::{AVPacketSideDataType, av_packet_new_side_data},
      packet::Mut,
    };
    let mut packet = if body {
      Packet::copy(&[1u8, 2, 3])
    } else {
      Packet::empty()
    };
    // SAFETY: `packet` owns a live `AVPacket` and the type is a
    // compile-time constant of this build.
    let ptr = unsafe {
      av_packet_new_side_data(
        packet.as_mut_ptr(),
        AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
        size,
      )
    };
    assert!(!ptr.is_null(), "av_packet_new_side_data({size})");
    packet
  }

  /// A packet whose side-data array has been forged into a shape
  /// FFmpeg's own API cannot produce, and cannot free either: `Drop`
  /// puts the header back into a state `av_packet_free_side_data` can
  /// walk, so a fixture for a malformed packet cannot take the test
  /// process down with it.
  ///
  /// Both `av_packet_new_side_data` and `av_packet_add_side_data`
  /// replace an existing entry of the same type (measured: seventy
  /// calls leave one entry), so every shape below — an over-cap count, a
  /// negative count, a missing array, an entry that declares bytes it
  /// does not carry — is one no FFmpeg call produces. That is exactly
  /// what the validation is for.
  struct Forged {
    packet: Packet,
    freeable: i32,
  }

  impl Drop for Forged {
    fn drop(&mut self) {
      use ffmpeg_next::packet::Mut;
      // SAFETY: `packet` owns a live `AVPacket`; putting the count back
      // to what the array really holds is what makes the free sound.
      unsafe { (*self.packet.as_mut_ptr()).side_data_elems = self.freeable };
    }
  }

  impl Forged {
    /// `count` real entries of four bytes each.
    fn entries(count: usize, body: bool) -> Self {
      let mut packet = Self::carrier(body);
      // SAFETY: the array and every entry payload come from FFmpeg's
      // own allocator and are handed to the packet, which frees them in
      // `av_packet_free_side_data`. The carrier has no side data of its
      // own, so the overwrite leaks nothing.
      unsafe {
        let array = Self::array(count);
        for index in 0..count {
          let data = av_malloc(4) as *mut u8;
          assert!(!data.is_null(), "av_malloc");
          core::ptr::write_bytes(data, 7, 4);
          (*array.add(index)).data = data;
          (*array.add(index)).size = 4;
          (*array.add(index)).type_ = AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA;
        }
        Self::attach(&mut packet, array, count as i32);
      }
      Self {
        packet,
        freeable: count as i32,
      }
    }

    /// A count with no array behind it at all.
    fn null_array(count: i32, body: bool) -> Self {
      let mut packet = Self::carrier(body);
      use ffmpeg_next::packet::Mut;
      // SAFETY: the packet's side data is null already; only the count
      // is forged, and `Drop` puts it back to zero before the free.
      unsafe { (*packet.as_mut_ptr()).side_data_elems = count };
      Self {
        packet,
        freeable: 0,
      }
    }

    /// One entry declaring `size` bytes it does not carry.
    fn null_entry_data(size: usize, body: bool) -> Self {
      let mut packet = Self::carrier(body);
      // SAFETY: as in `entries`, except the payload pointer is left
      // null — `av_freep(&NULL)` is a no-op, so the free stays sound.
      unsafe {
        let array = Self::array(1);
        (*array).data = core::ptr::null_mut();
        (*array).size = size;
        (*array).type_ = AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA;
        Self::attach(&mut packet, array, 1);
      }
      Self {
        packet,
        freeable: 1,
      }
    }

    /// Overrides the declared count, keeping the array intact.
    fn with_declared_count(mut self, count: i32) -> Self {
      use ffmpeg_next::packet::Mut;
      // SAFETY: `Drop` restores `freeable`, which still describes the
      // array really attached.
      unsafe { (*self.packet.as_mut_ptr()).side_data_elems = count };
      self
    }

    fn carrier(body: bool) -> Packet {
      if body {
        Packet::copy(&[1u8, 2, 3])
      } else {
        Packet::empty()
      }
    }

    /// # Safety
    /// The returned array is FFmpeg-allocated and uninitialised.
    unsafe fn array(count: usize) -> *mut AVPacketSideData {
      let array = unsafe { av_malloc(count * core::mem::size_of::<AVPacketSideData>()) }
        as *mut AVPacketSideData;
      assert!(!array.is_null(), "av_malloc");
      array
    }

    /// # Safety
    /// `array` must hold `count` initialised entries owned by FFmpeg.
    unsafe fn attach(packet: &mut Packet, array: *mut AVPacketSideData, count: i32) {
      use ffmpeg_next::packet::Mut;
      unsafe {
        (*packet.as_mut_ptr()).side_data = array;
        (*packet.as_mut_ptr()).side_data_elems = count;
      }
    }
  }

  /// Runs one packet through all four timed conversions, returning what
  /// each answered — the arms share a collector, and a fix that misses
  /// one of them is a fix that misses.
  fn every_timed_arm(packet: &Packet) -> [(&'static str, Result<bool, PacketBufferError>); 4] {
    let tb = mediadecode::Timebase::default();
    [
      (
        "video",
        video_packet_from_borrowed::<crate::Owned>(
          packet,
          tb,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
      (
        "audio",
        audio_packet_from_borrowed::<crate::Owned>(
          packet,
          tb,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
      (
        "subtitle",
        subtitle_packet_from_borrowed::<crate::Owned>(
          packet,
          tb,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
      (
        "data",
        data_packet_from_borrowed::<crate::Owned>(
          packet,
          tb,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
    ]
  }

  #[test]
  fn side_data_a_packet_declares_and_does_not_carry_refuses_it() {
    // Two pointer paths walked straight past the all-or-error rule: a
    // count with no array behind it returned "no side data", and an
    // entry declaring bytes it did not carry became an empty entry —
    // charged nothing, delivered as though the packet had said so. Both
    // are how a body-bearing packet reached a decoder stripped of its
    // control data, and how a side-data-only packet became `None` and
    // was skipped.
    for body in [true, false] {
      let forged = Forged::null_array(3, body);
      for (arm, result) in every_timed_arm(&forged.packet) {
        match result {
          Err(PacketBufferError::SideDataArray(p)) => assert_eq!(p.count(), 3, "{arm}"),
          other => panic!("{arm} (body={body}): expected SideDataArray, got {other:?}"),
        }
      }

      let forged = Forged::null_entry_data(64, body);
      for (arm, result) in every_timed_arm(&forged.packet) {
        match result {
          Err(PacketBufferError::SideDataPayload(p)) => {
            assert_eq!((p.index(), p.size()), (0, 64), "{arm}");
          }
          other => panic!("{arm} (body={body}): expected SideDataPayload, got {other:?}"),
        }
      }

      // And the malformed count keeps being refused when the array is
      // missing too — the count is judged on its own, before the
      // pointer, so one cannot excuse the other.
      let forged = Forged::null_array(-3, body);
      for (arm, result) in every_timed_arm(&forged.packet) {
        assert!(
          matches!(
            result,
            Err(PacketBufferError::SideDataEntries(p)) if p.count() == -3
          ),
          "{arm} (body={body}): {result:?}",
        );
      }
      let forged = Forged::null_array(i32::MAX, body);
      for (arm, result) in every_timed_arm(&forged.packet) {
        assert!(
          matches!(result, Err(PacketBufferError::SideDataEntries(_))),
          "{arm} (body={body}): {result:?}",
        );
      }
    }

    // A zero-size entry is a marker, not a lie: a type with no bytes is
    // exactly what FFmpeg emits for some side data, and it stays
    // welcome.
    let marker = Forged::null_entry_data(0, true);
    let packet = video_packet_from_borrowed::<crate::Owned>(
      &marker.packet,
      mediadecode::Timebase::default(),
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("a marker entry is carriable")
    .expect("present");
    assert_eq!(packet.extra().side_data().len(), 1);
    assert!(packet.extra().side_data()[0].data().is_empty());
  }

  #[test]
  fn side_data_that_cannot_be_carried_whole_refuses_the_packet() {
    let tb = mediadecode::Timebase::default();

    // A body-bearing packet whose side data is over the byte cap. The
    // caps used to truncate and warn, which handed the codec a packet
    // that looked complete and was not: `NEW_EXTRADATA` gone, a decoder
    // left on parameters the container had already replaced.
    let oversized = packet_with_side_data(SIDE_DATA_MAX_TOTAL_BYTES + 1, true);
    for (arm, result) in [
      (
        "video",
        video_packet_from_borrowed::<crate::Owned>(
          &oversized,
          tb,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
      (
        "audio",
        audio_packet_from_borrowed::<crate::Owned>(
          &oversized,
          tb,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
      (
        "subtitle",
        subtitle_packet_from_borrowed::<crate::Owned>(
          &oversized,
          tb,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
      (
        "data",
        data_packet_from_borrowed::<crate::Owned>(
          &oversized,
          tb,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
    ] {
      match result {
        Err(PacketBufferError::SideDataBytes(p)) => {
          assert_eq!(p.cap(), SIDE_DATA_MAX_TOTAL_BYTES);
          assert!(p.bytes() > p.cap(), "{arm}");
        }
        other => panic!("{arm}: expected SideDataBytes, got {other:?}"),
      }
    }

    // And the same packet with no body at all. This one used to
    // collapse twice over: the side data was dropped, the packet
    // therefore looked empty, and the demuxer skipped it in silence.
    let oversized = packet_with_side_data(SIDE_DATA_MAX_TOTAL_BYTES + 1, false);
    assert!(matches!(
      video_packet_from_borrowed::<crate::Owned>(
        &oversized,
        tb,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied
      )
      .map(|p| p.is_some()),
      Err(PacketBufferError::SideDataBytes(_)),
    ));
  }

  #[test]
  fn the_side_data_caps_are_refusals_at_the_boundary_not_before_it() {
    let tb = mediadecode::Timebase::default();

    // Exactly at the byte cap: carried, whole.
    let at_cap = packet_with_side_data(SIDE_DATA_MAX_TOTAL_BYTES, true);
    let packet = video_packet_from_borrowed::<crate::Owned>(
      &at_cap,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("exactly at the cap is not over it")
    .expect("present");
    assert_eq!(packet.extra().side_data().len(), 1);
    assert_eq!(
      packet.extra().side_data()[0].data().len(),
      SIDE_DATA_MAX_TOTAL_BYTES,
    );

    // One byte past: refused.
    let past = packet_with_side_data(SIDE_DATA_MAX_TOTAL_BYTES + 1, true);
    assert!(matches!(
      video_packet_from_borrowed::<crate::Owned>(
        &past,
        tb,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied
      )
      .map(|p| p.is_some()),
      Err(PacketBufferError::SideDataBytes(_)),
    ));

    // The entry cap, both sides of it. FFmpeg names fewer types than
    // the floor today, so the effective cap is the floor; if a future
    // build names more, this assertion moves the boundary rather than
    // letting the lane quietly test nothing.
    let cap = side_data_entry_cap();
    assert_eq!(cap, SIDE_DATA_MAX_ENTRIES, "the cap this lane straddles");

    let at_cap = Forged::entries(cap, true);
    let packet = video_packet_from_borrowed::<crate::Owned>(
      &at_cap.packet,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("exactly at the cap is not over it")
    .expect("present");
    assert_eq!(packet.extra().side_data().len(), cap);

    let past = Forged::entries(cap + 1, true);
    match video_packet_from_borrowed::<crate::Owned>(
      &past.packet,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .map(|p| p.is_some())
    {
      Err(PacketBufferError::SideDataEntries(p)) => {
        assert_eq!(p.count() as usize, cap + 1);
        assert_eq!(p.cap(), cap);
      }
      other => panic!("expected SideDataEntries, got {other:?}"),
    }

    // A negative count is malformed, not empty — reading it as "no side
    // data" would be the same silent loss by another route.
    let corrupt = Forged::entries(1, true).with_declared_count(-3);
    assert!(matches!(
      video_packet_from_borrowed::<crate::Owned>(&corrupt.packet, tb, PacketLimits::default(), crate::buffer::PayloadProvenance::CallerSupplied).map(|p| p.is_some()),
      Err(PacketBufferError::SideDataEntries(p)) if p.count() == -3,
    ));
  }

  #[test]
  fn a_trusted_packet_is_refused_on_both_legs() {
    use ffmpeg_next::packet::Mut;
    let tb = mediadecode::Timebase::default();

    // Copy-out: the leg such a packet would enter the graph on.
    let mut packet = Packet::copy(&[1u8, 2, 3]);
    // SAFETY: `packet` owns a live `AVPacket`; `flags` is a public field
    // and this bit has no `ffmpeg_next::Flags` spelling.
    unsafe {
      (*packet.as_mut_ptr()).flags =
        ffmpeg_next::ffi::AV_PKT_FLAG_KEY | ffmpeg_next::ffi::AV_PKT_FLAG_TRUSTED;
    };
    for (arm, taken) in [
      (
        "video",
        video_packet_from_borrowed::<crate::Owned>(
          &packet,
          tb,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
      (
        "audio",
        audio_packet_from_borrowed::<crate::Owned>(
          &packet,
          tb,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
      (
        "subtitle",
        subtitle_packet_from_borrowed::<crate::Owned>(
          &packet,
          tb,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
      (
        "data",
        data_packet_from_borrowed::<crate::Owned>(
          &packet,
          tb,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
      (
        "attachment",
        attachment_packet_from_borrowed::<crate::Owned>(
          &packet,
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        )
        .map(|p| p.is_some()),
      ),
    ] {
      match taken {
        Err(PacketBufferError::TrustedPayload(p)) => assert_eq!(p.len(), 3, "{arm}"),
        other => panic!("{arm}: a TRUSTED payload must not be copied out, got {other:?}"),
      }
    }

    // Rebuild: the leg a flag that arrived by some other route would be
    // handed back to a decoder on. Built by hand, because the copy-out
    // leg above will no longer produce one.
    let clean = Packet::copy(&[1u8, 2, 3]);
    let video = video_packet_from_borrowed::<crate::Owned>(
      &clean,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("a clean packet is carriable")
    .expect("present");
    let trusted = video
      .clone()
      .with_flags(MdPacketFlags::from_bits_retain(crate::buffer::TRUSTED_BIT));
    assert!(
      matches!(
        ffmpeg_packet_from_owned_video_packet(&trusted, PacketLimits::default()),
        Err(PacketBuildError::TrustedPayload(_)),
      ),
      "a TRUSTED flag must not be written back onto an AVPacket",
    );
  }

  #[test]
  fn every_flag_bit_survives_both_directions() {
    // `PacketFlags` is a bit set whose documented lossless door is
    // `from_bits_retain`, and both directions used to squeeze it
    // through `ffmpeg_next`'s `Flags`, which names `KEY` and `CORRUPT`
    // and truncates the rest away. `AV_PKT_FLAG_DISCARD` is the one
    // that matters most: it tells a consumer to decode a packet and
    // throw its output away, and without it preroll output looks like
    // something to keep.
    use ffmpeg_next::packet::{Mut, Ref};
    const DISCARD: i32 = ffmpeg_next::ffi::AV_PKT_FLAG_DISCARD;
    const TRUSTED: i32 = ffmpeg_next::ffi::AV_PKT_FLAG_TRUSTED;
    const DISPOSABLE: i32 = ffmpeg_next::ffi::AV_PKT_FLAG_DISPOSABLE;
    const UNNAMED: i32 = 0b0010_0000; // nothing names this bit yet
    // `TRUSTED` is deliberately **not** in this set, and the constant is
    // kept only to say so. It used to be — this lane asserted it made
    // the round trip — and that assertion was the bug: a `TRUSTED`
    // payload may be a structure of pointers into other live objects,
    // so copying it mints an owned-looking carrier that dangles when
    // its source drops. It is now refused on both legs, which
    // `a_trusted_packet_is_refused_on_both_legs` pins.
    let _ = TRUSTED;
    let raw = ffmpeg_next::ffi::AV_PKT_FLAG_KEY | DISCARD | DISPOSABLE | UNNAMED;

    let mut packet = Packet::copy(&[1u8, 2, 3]);
    // SAFETY: `packet` owns a live `AVPacket`; `flags` is a public
    // field and this is the only way to set a bit `ffmpeg_next`'s
    // `Flags` cannot spell.
    unsafe { (*packet.as_mut_ptr()).flags = raw };
    assert_ne!(
      packet.flags().bits(),
      raw,
      "the wrapper's own accessor is what loses them",
    );

    let tb = mediadecode::Timebase::default();
    let expected = MdPacketFlags::from_bits_retain(raw as u8);

    let video = video_packet_from_borrowed::<crate::Owned>(
      &packet,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("wrappable")
    .expect("present");
    assert_eq!(video.flags(), expected);
    assert!(video.flags().contains(MdPacketFlags::DISCARD));
    let audio = audio_packet_from_borrowed::<crate::Owned>(
      &packet,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("wrappable")
    .expect("present");
    assert_eq!(audio.flags(), expected);
    let subtitle = subtitle_packet_from_borrowed::<crate::Owned>(
      &packet,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("wrappable")
    .expect("present");
    assert_eq!(subtitle.flags(), expected);
    let data = data_packet_from_borrowed::<crate::Owned>(
      &packet,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("wrappable")
    .expect("present");
    assert_eq!(data.flags(), expected);
    let attachment = attachment_packet_from_borrowed::<crate::Owned>(
      &packet,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("wrappable")
    .expect("present");
    assert_eq!(attachment.flags(), expected);

    // And back out again, on the three paths a decoder is fed from.
    for (arm, rebuilt) in [
      (
        "video",
        ffmpeg_packet_from_owned_video_packet(&video, PacketLimits::default()).expect("rebuilt"),
      ),
      (
        "audio",
        ffmpeg_packet_from_owned_audio_packet(&audio, PacketLimits::default()).expect("rebuilt"),
      ),
      (
        "subtitle",
        ffmpeg_packet_from_owned_subtitle_packet(&subtitle, PacketLimits::default())
          .expect("rebuilt"),
      ),
    ] {
      // SAFETY: `rebuilt` owns a live `AVPacket`.
      let carried = unsafe { (*rebuilt.as_ptr()).flags };
      assert_eq!(carried, raw, "{arm} rebuilt {carried:#x} from {raw:#x}");
    }
  }

  #[test]
  fn a_flag_bit_the_vocabulary_cannot_hold_refuses_the_packet() {
    // Unreachable against this build — every flag FFmpeg names lives in
    // the byte `PacketFlags` carries, and a compile-time assertion says
    // so. It is here because the day that stops being true, a packet
    // must be refused rather than delivered with a bit missing.
    use ffmpeg_next::packet::Mut;
    let mut packet = Packet::copy(&[1u8]);
    // SAFETY: `packet` owns a live `AVPacket`.
    unsafe { (*packet.as_mut_ptr()).flags = 0x1_00 };
    let tb = mediadecode::Timebase::default();
    for (arm, result) in every_timed_arm(&packet) {
      assert!(
        matches!(
          result,
          Err(PacketBufferError::UnrepresentableFlags(p)) if p.raw() == 0x1_00
        ),
        "{arm}: {result:?}",
      );
    }
    assert!(matches!(
      attachment_packet_from_borrowed::<crate::Owned>(
        &packet,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied
      )
      .map(|p| p.is_some()),
      Err(PacketBufferError::UnrepresentableFlags(_)),
    ));
    let _ = tb;
  }

  /// A live `AVBufferRef` of `len` bytes, released when the guard drops.
  struct TestBuffer(*mut ffmpeg_next::ffi::AVBufferRef);

  impl TestBuffer {
    fn new(len: usize) -> Self {
      // SAFETY: a plain allocation, checked for null, written through
      // its own `data` pointer.
      unsafe {
        let raw = ffmpeg_next::ffi::av_buffer_alloc(len as _);
        assert!(!raw.is_null(), "av_buffer_alloc");
        core::ptr::write_bytes((*raw).data, 0xAB, len);
        Self(raw)
      }
    }
  }

  impl Drop for TestBuffer {
    fn drop(&mut self) {
      // SAFETY: the guard owns exactly one reference, released once.
      unsafe { ffmpeg_next::ffi::av_buffer_unref(&mut self.0) };
    }
  }

  /// The payload pointer a packet's buffer currently holds.
  fn payload_address(packet: &Packet) -> usize {
    use ffmpeg_next::packet::Ref;
    // SAFETY: the packet is live; `data` is a public field.
    unsafe { (*packet.as_ptr()).data as usize }
  }

  /// The refcount of a packet's payload buffer.
  fn payload_references(packet: &Packet) -> i32 {
    use ffmpeg_next::packet::Ref;
    // SAFETY: the packet is live and refcounted; `buf` is a public
    // field and `av_buffer_get_ref_count` only reads the atomic.
    unsafe {
      let buf = (*packet.as_ptr()).buf;
      assert!(!buf.is_null(), "the fixture must be refcounted");
      ffmpeg_next::ffi::av_buffer_get_ref_count(buf)
    }
  }

  #[test]
  fn a_shared_packet_buffer_is_refused_without_being_read() {
    // **The consumption argument has a hole a dependency can open, and
    // the obvious patch had a worse one.** Taking the packet by value
    // proves no *other* handle survives only if the packet's buffer was
    // the caller's alone to give. An earlier round answered a shared
    // buffer with a silent copy — but a copy needs a read, and a
    // refcount above one is exactly the state in which somebody else
    // may be writing those bytes, from safe code, on another thread.
    // The read *is* the race. So the answer is a refusal that touches
    // nothing.
    let mut original = Packet::copy(&[7u8; 4096]);
    let mut shared = Packet::empty();
    // SAFETY: both packets are live; `av_packet_ref` takes a reference
    // to `original`'s buffer without copying it.
    unsafe {
      use ffmpeg_next::packet::{Mut, Ref};
      assert_eq!(
        ffmpeg_next::ffi::av_packet_ref(shared.as_mut_ptr(), original.as_ptr()),
        0,
      );
    }
    assert_eq!(
      payload_address(&shared),
      payload_address(&original),
      "the premise: the two packets share one allocation",
    );
    assert_eq!(payload_references(&original), 2);

    // The refusal is lane-independent, because the hazard is: both
    // lanes read the payload, one to view it and one to copy it.
    match video_packet_from_ffmpeg_as::<crate::View>(
      shared,
      mediadecode::Timebase::default(),
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    ) {
      Err(PacketBufferError::SharedPayload(refused)) => {
        assert_eq!(refused.references(), 2);
      }
      other => panic!("a shared payload must be refused by name, got {other:?}"),
    }

    // Nothing was read and nothing was carried: the packet the caller
    // kept is still theirs, still writable, and now sole owner again.
    {
      let slot = original.data_mut().expect("a writable payload");
      slot[0] = 0xEE;
    }
    assert_eq!(
      payload_references(&original),
      1,
      "the refusal must not have taken a reference of its own",
    );

    // And the owned lane answers the same way, for the same reason: its
    // copy is a read too.
    let mut shared_again = Packet::empty();
    // SAFETY: as above.
    unsafe {
      use ffmpeg_next::packet::{Mut, Ref};
      assert_eq!(
        ffmpeg_next::ffi::av_packet_ref(shared_again.as_mut_ptr(), original.as_ptr()),
        0,
      );
    }
    assert!(matches!(
      owned_video_packet_from_ffmpeg_in(
        &shared_again,
        mediadecode::Timebase::default(),
        PacketLimits::default(),
      ),
      Err(PacketBufferError::SharedPayload(_)),
    ));
  }

  #[test]
  fn a_clone_that_silently_shared_is_refused_by_name() {
    // The exact path: `ffmpeg_next::Packet::clone` calls
    // `av_packet_ref` then `av_packet_make_writable` and **ignores both
    // return codes**. Under allocation failure the second one leaves
    // the "clone" sharing the source's buffer, and nothing says so.
    //
    // The cap is process-global, so the manufacture runs alone. It is
    // lifted before the conversion, because what is being tested is the
    // conversion's answer to a shared buffer — not its behaviour under
    // memory pressure, which the other lanes cover.
    crate::fault_subprocess::in_subprocess(
      "boundary::tests::a_clone_that_silently_shared_is_refused_by_name",
      || {
        let mut original = Packet::copy(&[3u8; 8192]);

        // Small allocations still succeed, so `av_packet_ref` gets its
        // `AVBufferRef`; a payload-sized one does not, so
        // `av_packet_make_writable` fails and is ignored.
        crate::fault_subprocess::cap_ffmpeg_allocations(512);
        let clone = original.clone();
        crate::fault_subprocess::uncap_ffmpeg_allocations();

        assert_eq!(
          payload_address(&clone),
          payload_address(&original),
          "the premise this test exists for: a failed `make_writable` \
           leaves the clone sharing, silently",
        );
        assert_eq!(payload_references(&original), 2);

        match video_packet_from_ffmpeg_as::<crate::View>(
          clone,
          mediadecode::Timebase::default(),
          PacketLimits::default(),
          crate::buffer::PayloadProvenance::CallerSupplied,
        ) {
          Err(PacketBufferError::SharedPayload(refused)) => {
            assert_eq!(refused.references(), 2);
          }
          other => panic!("expected a named refusal, got {other:?}"),
        }

        // The handle the caller kept is untouched and unaliased.
        {
          let slot = original.data_mut().expect("a writable payload");
          slot[0] = 0x5A;
        }
        assert_eq!(payload_references(&original), 1);
      },
    );
  }

  #[test]
  fn only_a_packet_payload_with_provable_padding_is_shared_into_a_decoder() {
    use crate::view::Origin;
    use ffmpeg_next::packet::Ref;

    const PADDING: usize = ffmpeg_next::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
    let src = TestBuffer::new(512);
    let body_len = 512 - PADDING;

    // The one shape that may share: a payload captured out of an
    // `AVPacket`'s own buffer, with libavformat's padding behind it.
    // SAFETY: `src` is live for the test and the extent is inside it.
    let payload =
      unsafe { crate::FfmpegBuffer::view_of(src.0, 0, body_len, Origin::PacketPayload) }
        .expect("a view");
    let shared = share_or_copy(&payload).expect("built");
    // SAFETY: both are live; `data` is a public field.
    assert_eq!(
      unsafe { (*shared.as_ptr()).data as usize },
      payload.as_ref().as_ptr() as usize,
      "a packet payload with provable padding must be shared",
    );

    // **The same bytes, the same slack, no provenance.** This is the
    // frame-origin case: a decoded plane or a resampler's output reused
    // as a packet body. What follows it is more pixels or more samples,
    // and a bitstream reader running past the payload would eat them.
    // SAFETY: as above.
    let plane =
      unsafe { crate::FfmpegBuffer::view_of(src.0, 0, body_len, Origin::Foreign) }.expect("a view");
    let copied = share_or_copy(&plane).expect("built");
    // SAFETY: both are live.
    assert_ne!(
      unsafe { (*copied.as_ptr()).data as usize },
      plane.as_ref().as_ptr() as usize,
      "trailing capacity is not padding provenance — this must be copied",
    );
    assert_eq!(copied.data().unwrap_or(&[]), plane.as_ref());

    // And a payload whose slack is short of the padding is copied too,
    // provenance or not: the extent has to hold as well.
    // SAFETY: as above.
    let tight = unsafe { crate::FfmpegBuffer::view_of(src.0, 0, 511, Origin::PacketPayload) }
      .expect("a view");
    let copied = share_or_copy(&tight).expect("built");
    // SAFETY: both are live.
    assert_ne!(
      unsafe { (*copied.as_ptr()).data as usize },
      tight.as_ref().as_ptr() as usize,
      "a payload with less than the padding behind it must be copied",
    );

    // A narrowed payload has lost its claim, and is copied.
    let mut narrowed = payload.clone();
    narrowed.shrink_to(16);
    let copied = share_or_copy(&narrowed).expect("built");
    // SAFETY: both are live.
    assert_ne!(
      unsafe { (*copied.as_ptr()).data as usize },
      narrowed.as_ref().as_ptr() as usize,
    );
  }

  #[test]
  fn an_empty_view_carrier_builds_a_payload_less_packet() {
    // The empty carrier is backed by **no buffer at all** — that is
    // what makes a placeholder plane slot free — so anything that
    // reads its `AVBufferRef` before asking whether it is empty
    // dereferences null. A side-data-only packet is the ordinary way
    // to get here, not an exotic one.
    let empty = crate::FfmpegBuffer::empty();
    assert!(empty.as_av_buffer_ref().is_null(), "the premise");
    let built = share_or_copy(&empty).expect("a payload-less packet");
    assert_eq!(built.size(), 0);
    // And it is a packet side data can be attached to.
    let mut built = built;
    attach_side_data(
      &mut built,
      &[SideDataEntry::new(
        NEW_EXTRADATA,
        FfmpegBytes::copy_from_slice(&[1u8, 2, 3, 4]),
      )],
    )
    .expect("side data attaches to a payload-less packet");
  }

  #[test]
  fn a_side_data_only_packet_round_trips_on_every_view_decoder_family() {
    // Every family, both roads: the public builder a caller can call,
    // and the scoped submission a decoder goes through. Neither may
    // touch the empty carrier's absent buffer.
    let tb = mediadecode::Timebase::default();
    let source = side_data_only_packet();
    let limits = PacketLimits::default();

    let video = video_packet_from_ffmpeg_as::<crate::View>(
      source.clone(),
      tb,
      limits,
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("carried")
    .expect("a side-data-only packet is a packet");
    assert!(video.data().as_ref().is_empty(), "the premise: no payload");
    assert_eq!(
      ffmpeg_packet_from_video_packet(&video, limits)
        .expect("built")
        .size(),
      0,
    );
    with_ffmpeg_video_packet::<crate::View, _>(&video, limits, BodyRoute::Submission, |av| {
      assert_eq!(av.size(), 0);
    })
    .expect("submitted");

    let audio = audio_packet_from_ffmpeg_as::<crate::View>(
      source.clone(),
      tb,
      limits,
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("carried")
    .expect("a side-data-only packet is a packet");
    assert!(audio.data().as_ref().is_empty());
    assert_eq!(
      ffmpeg_packet_from_audio_packet(&audio, limits)
        .expect("built")
        .size(),
      0,
    );
    with_ffmpeg_audio_packet::<crate::View, _>(&audio, limits, BodyRoute::Submission, |av| {
      assert_eq!(av.size(), 0);
    })
    .expect("submitted");

    let subtitle = subtitle_packet_from_ffmpeg_as::<crate::View>(
      source.clone(),
      tb,
      limits,
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("carried")
    .expect("a side-data-only packet is a packet");
    assert!(subtitle.data().as_ref().is_empty());
    assert_eq!(
      ffmpeg_packet_from_subtitle_packet(&subtitle, limits)
        .expect("built")
        .size(),
      0,
    );
    with_ffmpeg_subtitle_packet::<crate::View, _>(&subtitle, limits, BodyRoute::Submission, |av| {
      assert_eq!(av.size(), 0);
    })
    .expect("submitted");
  }

  #[test]
  fn side_data_survives_the_round_trip_on_every_decoder_bound_arm() {
    // The forward direction captures side data; the reverse direction
    // is what a decoder is actually handed. Capturing without
    // reattaching is theatre: `NEW_EXTRADATA` never reaches the codec,
    // `SKIP_SAMPLES` never trims, and nothing says so.
    let tb = mediadecode::Timebase::default();
    let mut with_body = Packet::copy(&[4u8, 5, 6]);
    {
      use ffmpeg_next::{
        ffi::{AVPacketSideDataType, av_packet_new_side_data},
        packet::Mut,
      };
      unsafe {
        let ptr = av_packet_new_side_data(
          with_body.as_mut_ptr(),
          AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES,
          3,
        );
        assert!(!ptr.is_null());
        core::ptr::copy_nonoverlapping([1u8, 2, 3].as_ptr(), ptr, 3);
      }
    }
    const SKIP_SAMPLES: i32 =
      ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES as i32;

    for (name, source) in [
      ("body plus side data", &with_body),
      ("side data only", &side_data_only_packet()),
    ] {
      let expected_kind = if name == "side data only" {
        NEW_EXTRADATA
      } else {
        SKIP_SAMPLES
      };

      let video = video_packet_from_borrowed::<crate::Owned>(
        source,
        tb,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied,
      )
      .expect("wrappable")
      .expect("present");
      let rebuilt =
        ffmpeg_packet_from_owned_video_packet(&video, PacketLimits::default()).expect("rebuilt");
      let carried = packet_side_data(&rebuilt).expect("readable");
      assert_eq!(carried.len(), 1, "{name}: video lost its side data");
      assert_eq!(carried[0].kind(), expected_kind, "{name}: video");
      assert_eq!(carried[0].data(), video.extra().side_data()[0].data());

      let audio = audio_packet_from_borrowed::<crate::Owned>(
        source,
        tb,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied,
      )
      .expect("wrappable")
      .expect("present");
      let rebuilt =
        ffmpeg_packet_from_owned_audio_packet(&audio, PacketLimits::default()).expect("rebuilt");
      let carried = packet_side_data(&rebuilt).expect("readable");
      assert_eq!(carried.len(), 1, "{name}: audio lost its side data");
      assert_eq!(carried[0].data(), audio.extra().side_data()[0].data());

      let subtitle = subtitle_packet_from_borrowed::<crate::Owned>(
        source,
        tb,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied,
      )
      .expect("wrappable")
      .expect("present");
      let rebuilt = ffmpeg_packet_from_owned_subtitle_packet(&subtitle, PacketLimits::default())
        .expect("rebuilt");
      let carried = packet_side_data(&rebuilt).expect("readable");
      assert_eq!(carried.len(), 1, "{name}: subtitle lost its side data");
      assert_eq!(carried[0].data(), subtitle.extra().side_data()[0].data());
    }
  }

  #[test]
  fn a_side_data_type_this_build_cannot_name_is_refused_not_dropped() {
    // This crate carries side-data types as the raw integers they are
    // on the wire, so a hand-built entry can name anything. Handing an
    // unknown one to C would either form an invalid discriminant or
    // attach a type nothing downstream reads — and dropping it quietly
    // is the very defect this whole seam exists to close.
    let limit = crate::ffi::side_data_type_count();
    let packet = mediadecode::packet::VideoPacket::new(
      FfmpegBytes::copy_from_slice(&[1u8]),
      VideoPacketExtra::new(0).with_side_data(vec![SideDataEntry::new(
        limit,
        FfmpegBytes::copy_from_slice(&[9u8]),
      )]),
    );
    match ffmpeg_packet_from_owned_video_packet(&packet, PacketLimits::default()).map(|_| ()) {
      Err(PacketBuildError::UnknownSideData(p)) => {
        assert_eq!(p.kind(), limit);
        assert_eq!(p.limit(), limit);
      }
      other => panic!("expected UnknownSideData, got {other:?}"),
    }
    assert!(matches!(
      ffmpeg_packet_from_owned_video_packet(&mediadecode::packet::VideoPacket::new(
        FfmpegBytes::copy_from_slice(&[1u8]),
        VideoPacketExtra::new(0).with_side_data(vec![SideDataEntry::new(-1, FfmpegBytes::copy_from_slice(&[9u8]))]),
      ), PacketLimits::default())
      .map(|_| ()),
      Err(PacketBuildError::UnknownSideData(p)) if p.kind() == -1,
    ));
  }

  #[test]
  fn a_side_data_only_packet_is_delivered_on_every_timed_arm() {
    // Codec-control data with no body is still a packet. Dropped as an
    // "empty marker", it leaves a decoder running on parameters the
    // container has already replaced — and says nothing about it.
    let packet = side_data_only_packet();
    let tb = mediadecode::Timebase::default();

    let video = video_packet_from_borrowed::<crate::Owned>(
      &packet,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("wrappable")
    .expect("a side-data-only packet is a packet");
    assert!(video.data().as_ref().is_empty(), "no body, but a buffer");
    assert_eq!(video.extra().side_data().len(), 1);
    assert_eq!(video.extra().side_data()[0].kind(), NEW_EXTRADATA);
    assert_eq!(video.extra().side_data()[0].data(), &[1, 2, 3, 4]);

    let audio = audio_packet_from_borrowed::<crate::Owned>(
      &packet,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("wrappable")
    .expect("present");
    assert!(audio.data().as_ref().is_empty());
    assert_eq!(audio.extra().side_data()[0].data(), &[1, 2, 3, 4]);

    let subtitle = subtitle_packet_from_borrowed::<crate::Owned>(
      &packet,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("wrappable")
    .expect("present");
    assert!(subtitle.data().as_ref().is_empty());
    assert_eq!(subtitle.extra().side_data()[0].data(), &[1, 2, 3, 4]);

    let data = data_packet_from_borrowed::<crate::Owned>(
      &packet,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("wrappable")
    .expect("present");
    assert!(data.data().as_ref().is_empty());
    assert_eq!(data.extra().side_data()[0].data(), &[1, 2, 3, 4]);

    // The one arm where a payload-less packet really is nothing: an
    // attachment is its bytes, and there are none.
    assert!(
      attachment_packet_from_borrowed::<crate::Owned>(
        &packet,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied
      )
      .expect("wrappable")
      .is_none(),
      "an attachment with no bytes is no attachment",
    );
  }

  #[test]
  fn side_data_rides_a_packet_that_has_a_body_too() {
    // The seat's documented promise — "the raw side-data entries from
    // `AVPacket.side_data`" — was never kept by the conversion before.
    use ffmpeg_next::{
      ffi::{AVPacketSideDataType, av_packet_new_side_data},
      packet::Mut,
    };
    let mut packet = Packet::copy(&[7u8, 7, 7]);
    unsafe {
      let ptr = av_packet_new_side_data(
        packet.as_mut_ptr(),
        AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
        2,
      );
      assert!(!ptr.is_null());
      core::ptr::copy_nonoverlapping([9u8, 9].as_ptr(), ptr, 2);
    }
    let video = video_packet_from_borrowed::<crate::Owned>(
      &packet,
      mediadecode::Timebase::default(),
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("wrappable")
    .expect("present");
    assert_eq!(video.data().as_ref(), &[7, 7, 7]);
    assert_eq!(video.extra().side_data()[0].data(), &[9, 9]);
  }

  #[test]
  fn an_empty_packet_is_absent_and_a_real_one_is_present() {
    let tb = mediadecode::Timebase::default();
    // No payload at all: the marker some demuxers emit. Absent, not an
    // error — this is the only thing a pull loop may skip.
    let empty = Packet::empty();
    assert!(
      video_packet_from_borrowed::<crate::Owned>(
        &empty,
        tb,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied
      )
      .expect("not a failure")
      .is_none()
    );
    assert!(
      attachment_packet_from_borrowed::<crate::Owned>(
        &empty,
        PacketLimits::default(),
        crate::buffer::PayloadProvenance::CallerSupplied
      )
      .expect("not a failure")
      .is_none()
    );

    let real = Packet::copy(&[9u8, 8, 7]);
    let wrapped = video_packet_from_borrowed::<crate::Owned>(
      &real,
      tb,
      PacketLimits::default(),
      crate::buffer::PayloadProvenance::CallerSupplied,
    )
    .expect("wrappable")
    .expect("present");
    assert_eq!(wrapped.data().as_ref(), &[9, 8, 7]);
  }

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
  fn unnamed_raw_maps_to_none() {
    assert_eq!(from_av_pixel_format(-99_999), PixelFormat::None);
  }

  #[test]
  fn av_pix_fmt_none_maps_to_none() {
    assert_eq!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_NONE as i32),
      PixelFormat::None,
    );
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
  fn hw_formats_map_to_none_in_pixel_format() {
    // HW sentinels intentionally don't have a mediadecode::PixelFormat
    // representation — they're not CPU pixel data.
    assert_eq!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32),
      PixelFormat::None,
    );
    assert_eq!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_VAAPI as i32),
      PixelFormat::None,
    );
  }
}

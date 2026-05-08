//! Boundary conversions between FFmpeg's bindgen integers and the
//! unified [`mediadecode`] vocabulary.
//!
//! Centralised so the rest of the crate never compares raw
//! `AVPixelFormat` integers against literals or transmutes back into
//! the bindgen enum (UB hazard when the value isn't in the enum's
//! discriminant set).

use ffmpeg_next::Packet;
use ffmpeg_next::ffi::AVPixelFormat;
use mediadecode::{PixelFormat, packet::PacketFlags as MdPacketFlags};

use crate::FfmpegBuffer;
use crate::extras::VideoPacketExtra;

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
  match raw {
    // Semi-planar YUV 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_NV12 as i32 => PixelFormat::Nv12,
    x if x == AVPixelFormat::AV_PIX_FMT_NV21 as i32 => PixelFormat::Nv21,
    x if x == AVPixelFormat::AV_PIX_FMT_NV16 as i32 => PixelFormat::Nv16,
    x if x == AVPixelFormat::AV_PIX_FMT_NV24 as i32 => PixelFormat::Nv24,
    x if x == AVPixelFormat::AV_PIX_FMT_NV42 as i32 => PixelFormat::Nv42,
    // Semi-planar YUV high-bit-depth.
    x if x == AVPixelFormat::AV_PIX_FMT_P010LE as i32 => PixelFormat::P010Le,
    // BE folds onto the LE-canonical enum.
    x if x == AVPixelFormat::AV_PIX_FMT_P010BE as i32 => PixelFormat::P010Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P012LE as i32 => PixelFormat::P012Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P016LE as i32 => PixelFormat::P016Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P210LE as i32 => PixelFormat::P210Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P212LE as i32 => PixelFormat::P212Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P216LE as i32 => PixelFormat::P216Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P410LE as i32 => PixelFormat::P410Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P412LE as i32 => PixelFormat::P412Le,
    x if x == AVPixelFormat::AV_PIX_FMT_P416LE as i32 => PixelFormat::P416Le,
    // Planar YUV 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P as i32 => PixelFormat::Yuv420p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P as i32 => PixelFormat::Yuv422p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV440P as i32 => PixelFormat::Yuv440p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P as i32 => PixelFormat::Yuv444p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV411P as i32 => PixelFormat::Yuv411p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV410P as i32 => PixelFormat::Yuv410p,
    // Planar YUV 4:2:0 high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P9LE as i32 => PixelFormat::Yuv420p9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P10LE as i32 => PixelFormat::Yuv420p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P12LE as i32 => PixelFormat::Yuv420p12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P14LE as i32 => PixelFormat::Yuv420p14Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV420P16LE as i32 => PixelFormat::Yuv420p16Le,
    // Planar YUV 4:2:2 high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P9LE as i32 => PixelFormat::Yuv422p9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P10LE as i32 => PixelFormat::Yuv422p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P12LE as i32 => PixelFormat::Yuv422p12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P14LE as i32 => PixelFormat::Yuv422p14Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV422P16LE as i32 => PixelFormat::Yuv422p16Le,
    // Planar YUV 4:4:4 high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P9LE as i32 => PixelFormat::Yuv444p9Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P10LE as i32 => PixelFormat::Yuv444p10Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P12LE as i32 => PixelFormat::Yuv444p12Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P14LE as i32 => PixelFormat::Yuv444p14Le,
    x if x == AVPixelFormat::AV_PIX_FMT_YUV444P16LE as i32 => PixelFormat::Yuv444p16Le,
    // Planar YUVA 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA420P as i32 => PixelFormat::Yuva420p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA422P as i32 => PixelFormat::Yuva422p,
    x if x == AVPixelFormat::AV_PIX_FMT_YUVA444P as i32 => PixelFormat::Yuva444p,
    // Packed YUV 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_YUYV422 as i32 => PixelFormat::Yuyv422,
    x if x == AVPixelFormat::AV_PIX_FMT_UYVY422 as i32 => PixelFormat::Uyvy422,
    x if x == AVPixelFormat::AV_PIX_FMT_YVYU422 as i32 => PixelFormat::Yvyu422,
    // Packed RGB 8-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_RGB24 as i32 => PixelFormat::Rgb24,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR24 as i32 => PixelFormat::Bgr24,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBA as i32 => PixelFormat::Rgba,
    x if x == AVPixelFormat::AV_PIX_FMT_BGRA as i32 => PixelFormat::Bgra,
    x if x == AVPixelFormat::AV_PIX_FMT_ARGB as i32 => PixelFormat::Argb,
    x if x == AVPixelFormat::AV_PIX_FMT_ABGR as i32 => PixelFormat::Abgr,
    // Packed RGB high-bit.
    x if x == AVPixelFormat::AV_PIX_FMT_RGB48LE as i32 => PixelFormat::Rgb48Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BGR48LE as i32 => PixelFormat::Bgr48Le,
    x if x == AVPixelFormat::AV_PIX_FMT_RGBA64LE as i32 => PixelFormat::Rgba64Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BGRA64LE as i32 => PixelFormat::Bgra64Le,
    // Greyscale.
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY8 as i32 => PixelFormat::Gray8,
    x if x == AVPixelFormat::AV_PIX_FMT_GRAY16LE as i32 => PixelFormat::Gray16Le,
    // Bayer.
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_BGGR8 as i32 => PixelFormat::BayerBggr8,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_RGGB8 as i32 => PixelFormat::BayerRggb8,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GBRG8 as i32 => PixelFormat::BayerGbrg8,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GRBG8 as i32 => PixelFormat::BayerGrbg8,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_BGGR16LE as i32 => PixelFormat::BayerBggr16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_RGGB16LE as i32 => PixelFormat::BayerRggb16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GBRG16LE as i32 => PixelFormat::BayerGbrg16Le,
    x if x == AVPixelFormat::AV_PIX_FMT_BAYER_GRBG16LE as i32 => PixelFormat::BayerGrbg16Le,
    _ => PixelFormat::Unknown,
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

/// Builds an `ffmpeg::Packet` from a [`mediadecode::VideoPacket`]
/// parameterized by [`crate::extras::VideoPacketExtra`] and
/// [`crate::FfmpegBuffer`].
///
/// The compressed bytes are **copied** into a new packet allocation —
/// zero-copy passthrough of the FfmpegBuffer's underlying AVBufferRef
/// is a future optimization (would need to wire an `AVBufferRef` into
/// `AVPacket.buf` directly via `av_packet_alloc` + manual buffer set).
/// PTS / DTS / duration / flags / stream_index are propagated.
pub fn ffmpeg_packet_from_video_packet(
  packet: &mediadecode::packet::VideoPacket<VideoPacketExtra, FfmpegBuffer>,
) -> Packet {
  let data = packet.data().as_ref();
  let mut out = Packet::copy(data);
  if let Some(ts) = packet.pts() {
    out.set_pts(Some(ts.pts()));
  }
  if let Some(ts) = packet.dts() {
    out.set_dts(Some(ts.pts()));
  }
  if let Some(d) = packet.duration() {
    out.set_duration(d.pts());
  }
  // Map flags. `ffmpeg_next::packet::Flags` is a bitflags wrapper around
  // AV_PKT_FLAG_*; the bit values match.
  let flags = packet.flags();
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
  out.set_flags(av_flags);
  // Stream index from the extras (stays 0 if the caller didn't set it).
  out.set_stream(packet.extra().stream_index as usize);
  out
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
  fn p010be_folds_to_p010le() {
    assert_eq!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_P010BE as i32),
      PixelFormat::P010Le,
    );
  }

  #[test]
  fn unknown_for_garbage_value() {
    assert_eq!(from_av_pixel_format(-99_999), PixelFormat::Unknown);
  }

  #[test]
  fn hw_formats_detected() {
    assert!(is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32));
    assert!(is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_VAAPI as i32));
    assert!(is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_CUDA as i32));
    assert!(is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_D3D11 as i32));
  }

  #[test]
  fn cpu_formats_not_detected_as_hw() {
    assert!(!is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_NV12 as i32));
    assert!(!is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_YUV420P as i32));
    assert!(!is_hardware_pix_fmt(AVPixelFormat::AV_PIX_FMT_NONE as i32));
  }

  #[test]
  fn hw_formats_map_to_unknown_in_pixel_format() {
    // HW sentinels intentionally don't have a mediadecode::PixelFormat
    // representation — they're not CPU pixel data.
    assert_eq!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as i32),
      PixelFormat::Unknown,
    );
    assert_eq!(
      from_av_pixel_format(AVPixelFormat::AV_PIX_FMT_VAAPI as i32),
      PixelFormat::Unknown,
    );
  }
}

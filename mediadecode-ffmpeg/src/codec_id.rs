//! `CodecId` newtype wrapping FFmpeg's `AVCodecID` discriminant.
//!
//! Constructed from hardcoded `AVCodecID` enum variants in our build's
//! bindgen-generated bindings, so we never cast an arbitrary `i32` into
//! the bindgen enum (that cast is UB when the value isn't in the enum's
//! discriminant set — the same hazard `crate::pix_fmt` documents). The
//! raw `i32` stored inside is what ends up passed to FFmpeg's C API
//! (which declares the codec id as `c_int`), so the boundary is sound.

use core::fmt;

use ffmpeg_next::ffi::AVCodecID;

/// Codec identifier. Wraps the integer value of an `AVCodecID` enum
/// variant; comparisons and storage work without ever transmuting back
/// into the bindgen enum.
#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct CodecId(i32);

impl CodecId {
  /// Constructs a `CodecId` from the raw integer FFmpeg uses for
  /// `AVCodecContext::codec_id` etc. Use this only when you have a
  /// value that came from FFmpeg or that you know maps to a real
  /// codec; arbitrary integers are still legal but `Debug` will fall
  /// back to printing the raw value.
  #[inline]
  pub const fn from_raw(raw: i32) -> Self {
    Self(raw)
  }

  /// Returns the underlying integer.
  #[inline]
  pub const fn raw(self) -> i32 {
    self.0
  }

  // --- Sentinels -------------------------------------------------------

  /// `AV_CODEC_ID_NONE` — sentinel for "no codec."
  pub const NONE: Self = Self(AVCodecID::AV_CODEC_ID_NONE as i32);

  // --- Video codecs ----------------------------------------------------

  /// H.264 / AVC (ITU-T H.264 / ISO/IEC 14496-10).
  pub const H264: Self = Self(AVCodecID::AV_CODEC_ID_H264 as i32);
  /// H.265 / HEVC (ITU-T H.265 / ISO/IEC 23008-2).
  pub const HEVC: Self = Self(AVCodecID::AV_CODEC_ID_HEVC as i32);
  /// AV1 (Alliance for Open Media).
  pub const AV1: Self = Self(AVCodecID::AV_CODEC_ID_AV1 as i32);
  /// VP9 (Google).
  pub const VP9: Self = Self(AVCodecID::AV_CODEC_ID_VP9 as i32);
  /// VP8 (Google).
  pub const VP8: Self = Self(AVCodecID::AV_CODEC_ID_VP8 as i32);
  /// MPEG-2 Video (ITU-T H.262 / ISO/IEC 13818-2).
  pub const MPEG2VIDEO: Self = Self(AVCodecID::AV_CODEC_ID_MPEG2VIDEO as i32);
  /// MPEG-4 Part 2 Visual (ISO/IEC 14496-2).
  pub const MPEG4: Self = Self(AVCodecID::AV_CODEC_ID_MPEG4 as i32);
  /// Apple ProRes.
  pub const PRORES: Self = Self(AVCodecID::AV_CODEC_ID_PRORES as i32);
  /// Avid DNxHD / DNxHR (SMPTE VC-3).
  pub const DNXHD: Self = Self(AVCodecID::AV_CODEC_ID_DNXHD as i32);
  /// FFV1 — lossless intra-frame.
  pub const FFV1: Self = Self(AVCodecID::AV_CODEC_ID_FFV1 as i32);
  /// JPEG 2000.
  pub const JPEG2000: Self = Self(AVCodecID::AV_CODEC_ID_JPEG2000 as i32);
  /// MJPEG.
  pub const MJPEG: Self = Self(AVCodecID::AV_CODEC_ID_MJPEG as i32);
  /// VC-1 (SMPTE 421M, Microsoft Windows Media Video 9).
  pub const VC1: Self = Self(AVCodecID::AV_CODEC_ID_VC1 as i32);
  /// VVC / H.266 (ITU-T H.266).
  pub const VVC: Self = Self(AVCodecID::AV_CODEC_ID_VVC as i32);

  // --- Audio codecs ----------------------------------------------------

  /// AAC (ISO/IEC 14496-3).
  pub const AAC: Self = Self(AVCodecID::AV_CODEC_ID_AAC as i32);
  /// MP3 (MPEG-1/2 Audio Layer III).
  pub const MP3: Self = Self(AVCodecID::AV_CODEC_ID_MP3 as i32);
  /// Opus (RFC 6716).
  pub const OPUS: Self = Self(AVCodecID::AV_CODEC_ID_OPUS as i32);
  /// FLAC — Free Lossless Audio Codec.
  pub const FLAC: Self = Self(AVCodecID::AV_CODEC_ID_FLAC as i32);
  /// AC-3 (ATSC A/52, Dolby Digital).
  pub const AC3: Self = Self(AVCodecID::AV_CODEC_ID_AC3 as i32);
  /// E-AC-3 (Dolby Digital Plus).
  pub const EAC3: Self = Self(AVCodecID::AV_CODEC_ID_EAC3 as i32);
  /// Apple Lossless Audio Codec.
  pub const ALAC: Self = Self(AVCodecID::AV_CODEC_ID_ALAC as i32);
  /// DTS / DTS-HD.
  pub const DTS: Self = Self(AVCodecID::AV_CODEC_ID_DTS as i32);
  /// Vorbis.
  pub const VORBIS: Self = Self(AVCodecID::AV_CODEC_ID_VORBIS as i32);
  /// PCM signed 16-bit little-endian.
  pub const PCM_S16LE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_S16LE as i32);
  /// PCM signed 16-bit big-endian.
  pub const PCM_S16BE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_S16BE as i32);
  /// PCM signed 24-bit little-endian.
  pub const PCM_S24LE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_S24LE as i32);
  /// PCM signed 32-bit little-endian.
  pub const PCM_S32LE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_S32LE as i32);
  /// PCM 32-bit float little-endian.
  pub const PCM_F32LE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_F32LE as i32);
  /// PCM 64-bit float little-endian.
  pub const PCM_F64LE: Self = Self(AVCodecID::AV_CODEC_ID_PCM_F64LE as i32);

  // --- Subtitle codecs -------------------------------------------------

  /// SubRip (.srt).
  pub const SUBRIP: Self = Self(AVCodecID::AV_CODEC_ID_SUBRIP as i32);
  /// Advanced SubStation Alpha (.ass / .ssa).
  pub const ASS: Self = Self(AVCodecID::AV_CODEC_ID_ASS as i32);
  /// WebVTT (.vtt).
  pub const WEBVTT: Self = Self(AVCodecID::AV_CODEC_ID_WEBVTT as i32);
  /// 3GPP Timed Text / MOV text track.
  pub const MOV_TEXT: Self = Self(AVCodecID::AV_CODEC_ID_MOV_TEXT as i32);
  /// DVB subtitle (bitmap).
  pub const DVB_SUBTITLE: Self = Self(AVCodecID::AV_CODEC_ID_DVB_SUBTITLE as i32);
  /// HDMV / Blu-ray PGS subtitle (bitmap).
  pub const HDMV_PGS_SUBTITLE: Self = Self(AVCodecID::AV_CODEC_ID_HDMV_PGS_SUBTITLE as i32);
  /// DVD VOBSUB subtitle (bitmap).
  pub const DVD_SUBTITLE: Self = Self(AVCodecID::AV_CODEC_ID_DVD_SUBTITLE as i32);
}

impl fmt::Debug for CodecId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let name = match *self {
      Self::NONE => "NONE",
      Self::H264 => "H264",
      Self::HEVC => "HEVC",
      Self::AV1 => "AV1",
      Self::VP9 => "VP9",
      Self::VP8 => "VP8",
      Self::MPEG2VIDEO => "MPEG2VIDEO",
      Self::MPEG4 => "MPEG4",
      Self::PRORES => "PRORES",
      Self::DNXHD => "DNXHD",
      Self::FFV1 => "FFV1",
      Self::JPEG2000 => "JPEG2000",
      Self::MJPEG => "MJPEG",
      Self::VC1 => "VC1",
      Self::VVC => "VVC",
      Self::AAC => "AAC",
      Self::MP3 => "MP3",
      Self::OPUS => "OPUS",
      Self::FLAC => "FLAC",
      Self::AC3 => "AC3",
      Self::EAC3 => "EAC3",
      Self::ALAC => "ALAC",
      Self::DTS => "DTS",
      Self::VORBIS => "VORBIS",
      Self::PCM_S16LE => "PCM_S16LE",
      Self::PCM_S16BE => "PCM_S16BE",
      Self::PCM_S24LE => "PCM_S24LE",
      Self::PCM_S32LE => "PCM_S32LE",
      Self::PCM_F32LE => "PCM_F32LE",
      Self::PCM_F64LE => "PCM_F64LE",
      Self::SUBRIP => "SUBRIP",
      Self::ASS => "ASS",
      Self::WEBVTT => "WEBVTT",
      Self::MOV_TEXT => "MOV_TEXT",
      Self::DVB_SUBTITLE => "DVB_SUBTITLE",
      Self::HDMV_PGS_SUBTITLE => "HDMV_PGS_SUBTITLE",
      Self::DVD_SUBTITLE => "DVD_SUBTITLE",
      _ => return write!(f, "CodecId({})", self.0),
    };
    write!(f, "CodecId::{name}")
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn from_raw_round_trips() {
    let id = CodecId::from_raw(27);
    assert_eq!(id.raw(), 27);
  }

  #[test]
  fn known_constants_match_av_values() {
    assert_eq!(CodecId::H264.raw(), AVCodecID::AV_CODEC_ID_H264 as i32);
    assert_eq!(CodecId::AAC.raw(), AVCodecID::AV_CODEC_ID_AAC as i32);
    assert_eq!(CodecId::SUBRIP.raw(), AVCodecID::AV_CODEC_ID_SUBRIP as i32);
  }

  #[test]
  fn debug_uses_name_for_known_codecs() {
    assert_eq!(format!("{:?}", CodecId::H264), "CodecId::H264");
    assert_eq!(format!("{:?}", CodecId::AAC), "CodecId::AAC");
  }

  #[test]
  fn debug_falls_back_to_raw_for_unknown() {
    let unknown = CodecId::from_raw(-99_999);
    assert_eq!(format!("{:?}", unknown), "CodecId(-99999)");
  }

  #[test]
  fn equality_is_value_based() {
    assert_eq!(CodecId::H264, CodecId::from_raw(CodecId::H264.raw()));
    assert_ne!(CodecId::H264, CodecId::HEVC);
  }
}

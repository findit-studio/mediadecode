//! WebCodecs codec identifiers.
//!
//! WebCodecs identifies codecs by RFC 6381 / ISOBMFF MIME-style
//! strings (`avc1.640028`, `vp09.00.10.08.420.0.1.1.1`,
//! `hvc1.1.6.L93.B0`, `av01.0.05M.08`, `opus`, `mp4a.40.2`).
//! Building those strings precisely needs codec-specific metadata
//! parsing (SPS for H.264, sequence header for VP9 / AV1, …).
//!
//! [`VideoCodecId`] / [`AudioCodecId`] enumerate the codecs this
//! crate knows by name; the [`crate::codec_string`] module
//! converts them to WebCodecs strings, falling back to
//! `Err(VideoDecodeError::UnsupportedCodec)` when the variant
//! genuinely needs caller-supplied bytes (e.g. SPS) that we don't
//! yet parse. Callers in that situation use the
//! `*_with_codec_string` constructors and supply the string
//! themselves (typically from a JS-side demuxer such as
//! `mp4box.js`).

/// A WebCodecs video codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VideoCodecId {
  /// H.264 / AVC (`avc1.*`). Codec string requires SPS parsing.
  H264,
  /// H.265 / HEVC (`hvc1.*`). Codec string requires VPS / SPS parsing.
  Hevc,
  /// VP8 (`vp8`). Codec string is fixed.
  Vp8,
  /// VP9 (`vp09.*`). Codec string requires sequence-header parsing.
  Vp9,
  /// AV1 (`av01.*`). Codec string requires sequence-OBU parsing.
  Av1,
}

/// A WebCodecs audio codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AudioCodecId {
  /// Opus (`opus`). Codec string is fixed.
  Opus,
  /// AAC (`mp4a.40.2`, `mp4a.40.5`, …). Codec string requires
  /// AudioSpecificConfig object-type parsing; default is LC
  /// (`mp4a.40.2`).
  Aac,
  /// 16-bit linear PCM (`pcm-s16`). WebCodecs spec name.
  PcmS16,
  /// FLAC (`flac`).
  Flac,
  /// Vorbis (`vorbis`).
  Vorbis,
  /// μ-law (`ulaw`).
  Ulaw,
  /// A-law (`alaw`).
  Alaw,
}

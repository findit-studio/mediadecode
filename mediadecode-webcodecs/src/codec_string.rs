//! WebCodecs codec-string builders.
//!
//! `web_sys::VideoDecoderConfig` requires a codec string in the
//! format defined by [RFC 6381] — the same string used by the
//! `<video>` element's `canPlayType`. For codecs whose string is
//! fully determined by their identifier (VP8, Opus, FLAC, …) this
//! module returns the canonical string. For codecs whose string
//! depends on bytes from the bitstream (H.264 SPS, HEVC VPS / SPS,
//! VP9 sequence header, AV1 sequence OBU, AAC AudioSpecificConfig)
//! the function returns `Err(UnsupportedCodec)` and callers must
//! use the `*_with_codec_string` constructors and supply the string
//! themselves (for instance from `mp4box.js` on the JS side).
//!
//! [RFC 6381]: https://datatracker.ietf.org/doc/html/rfc6381

use crate::{
  codec_id::{AudioCodecId, VideoCodecId},
  error::{AudioDecodeError, VideoDecodeError},
};

/// Build the WebCodecs codec string for a video codec.
///
/// `extradata` is the codec-private data (e.g. AVC `extradata` /
/// AVCC, HEVC `hvcC`, VP9 sequence header). The bytes are
/// inspected for codecs whose string is parameterized by them;
/// codecs that don't need extradata ignore the slice.
///
/// Returns `Err(VideoDecodeError::UnsupportedCodec)` for codecs we
/// don't yet have a parser for. The error message names the codec
/// so callers can swap to a `*_with_codec_string` constructor.
pub fn for_video(
  codec: VideoCodecId,
  _extradata: Option<&[u8]>,
) -> Result<String, VideoDecodeError> {
  match codec {
    VideoCodecId::Vp8 => Ok("vp8".into()),
    VideoCodecId::H264 | VideoCodecId::Hevc | VideoCodecId::Vp9 | VideoCodecId::Av1 => {
      Err(VideoDecodeError::UnsupportedCodec(format!(
        "{codec:?} requires extradata parsing; use open_with_codec_string"
      )))
    }
  }
}

/// Build the WebCodecs codec string for an audio codec.
///
/// `audio_specific_config` is the AAC `AudioSpecificConfig` blob;
/// other codecs ignore it. AAC defaults to LC (`mp4a.40.2`) when
/// the config is absent.
pub fn for_audio(
  codec: AudioCodecId,
  audio_specific_config: Option<&[u8]>,
) -> Result<String, AudioDecodeError> {
  match codec {
    AudioCodecId::Opus => Ok("opus".into()),
    AudioCodecId::PcmS16 => Ok("pcm-s16".into()),
    AudioCodecId::Flac => Ok("flac".into()),
    AudioCodecId::Vorbis => Ok("vorbis".into()),
    AudioCodecId::Ulaw => Ok("ulaw".into()),
    AudioCodecId::Alaw => Ok("alaw".into()),
    AudioCodecId::Aac => {
      // Top 5 bits of byte 0 carry the AOT (Audio Object Type).
      // 1=Main, 2=LC, 3=SSR, 4=LTP, 5=SBR/HE, 29=PS/HEv2, …
      let aot = audio_specific_config
        .and_then(|b| b.first().copied())
        .map(|b| b >> 3)
        .unwrap_or(2);
      Ok(format!("mp4a.40.{aot}"))
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn fixed_video_strings() {
    assert_eq!(for_video(VideoCodecId::Vp8, None).unwrap(), "vp8");
  }

  #[test]
  fn extradata_required_codecs_error() {
    assert!(matches!(
      for_video(VideoCodecId::H264, None),
      Err(VideoDecodeError::UnsupportedCodec(_))
    ));
  }

  #[test]
  fn fixed_audio_strings() {
    assert_eq!(for_audio(AudioCodecId::Opus, None).unwrap(), "opus");
    assert_eq!(for_audio(AudioCodecId::Flac, None).unwrap(), "flac");
  }

  #[test]
  fn aac_default_is_lc() {
    assert_eq!(for_audio(AudioCodecId::Aac, None).unwrap(), "mp4a.40.2");
  }

  #[test]
  fn aac_aot_extracted_from_config() {
    // 0b0010_1000 → AOT 5 (HE-AAC / SBR)
    let cfg = [0b0010_1000_u8];
    assert_eq!(
      for_audio(AudioCodecId::Aac, Some(&cfg)).unwrap(),
      "mp4a.40.5"
    );
  }
}

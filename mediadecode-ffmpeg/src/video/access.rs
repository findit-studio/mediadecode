//! **Where a decoder can start without losing a picture**: the clean
//! random access points at which a software session returns to its threads
//! after a fallback (see `CarrierVideoStreamDecoder::restore_session_threads`).
//!
//! A keyframe is a *clean* random access point when nothing after it in
//! decode order references a picture before it. A decoder opened there
//! then decodes every picture a decoder running through it would. An
//! open-GOP keyframe is not one. That covers an HEVC CRA, whose RASL
//! leading pictures reference the GOP before it, and an H.264 I-frame that
//! is a recovery point rather than an IDR. A decoder opened at such a
//! keyframe drops or conceals those pictures, so switching decoders there
//! would trade pictures for threads.

use crate::CodecId;

#[cfg(test)]
mod tests;

/// How a stream's keyframes are read for cleanliness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyframeRule {
  /// H.264: a keyframe is clean when it carries an IDR slice. Its NAL
  /// units are length-prefixed by `nal_length` bytes (`avcC`), or
  /// start-coded (Annex B) when `None`.
  H264 {
    /// The NAL length field's width, from the `avcC` record.
    nal_length: Option<usize>,
  },
  /// HEVC: clean when its first picture is an IDR or a BLA (NAL types
  /// 16–20). A BLA's RASL pictures are discarded by every decoder, so a
  /// new one loses nothing there. A CRA (21) is never clean.
  Hevc {
    /// The NAL length field's width, from the `hvcC` record.
    nal_length: Option<usize>,
  },
  /// VP8, VP9 and AV1: a keyframe refreshes every reference, and nothing
  /// leads it.
  Resets,
  /// Every other codec: clean only while the decoder reorders nothing
  /// (`has_b_frames == 0`), so no picture can lead a keyframe. This crate
  /// does not read those codecs' GOP headers, so it does not try to tell
  /// a closed GOP from an open one where pictures do reorder.
  Reordering,
}

impl KeyframeRule {
  /// The rule for a stream of `codec_id` whose codec parameters carry
  /// `extradata`.
  pub(crate) fn of(codec_id: i32, extradata: &[u8]) -> Self {
    if codec_id == CodecId::H264.raw() {
      // FFmpeg's own test (`h264_decode_extradata`): `avcC` opens with
      // its version byte, 1.
      let nal_length = (extradata.first() == Some(&1) && extradata.len() >= 5)
        .then(|| usize::from(extradata[4] & 3) + 1);
      Self::H264 { nal_length }
    } else if codec_id == CodecId::HEVC.raw() {
      // FFmpeg's own test (`hevc_decode_extradata`): extradata that does
      // not open with a start code is an `hvcC` record.
      let start_coded = extradata.starts_with(&[0, 0, 1]) || extradata.starts_with(&[0, 0, 0, 1]);
      let nal_length =
        (extradata.len() >= 23 && !start_coded).then(|| usize::from(extradata[21] & 3) + 1);
      Self::Hevc { nal_length }
    } else if codec_id == CodecId::VP8.raw()
      || codec_id == CodecId::VP9.raw()
      || codec_id == CodecId::AV1.raw()
    {
      Self::Resets
    } else {
      Self::Reordering
    }
  }

  /// Whether the keyframe `data` is a clean random access point, for a
  /// decoder that `reorders` pictures. Bytes that do not parse are not
  /// clean: nothing is proved about them.
  pub(crate) fn is_clean(self, data: &[u8], reorders: bool) -> bool {
    match self {
      Self::H264 { nal_length } => {
        nal_headers(data, nal_length).is_some_and(|headers| headers.iter().any(|&h| h & 0x1f == 5))
      }
      Self::Hevc { nal_length } => nal_headers(data, nal_length).is_some_and(|headers| {
        headers
          .iter()
          .map(|&h| (h >> 1) & 0x3f)
          .find(|&kind| kind < 32)
          .is_some_and(|kind| (16..=20).contains(&kind))
      }),
      Self::Resets => true,
      Self::Reordering => !reorders,
    }
  }

  /// Why a keyframe this rule reads may not be clean, for the warning a
  /// session gives when a fallback has run on one thread for a minute.
  pub(crate) const fn reason(self) -> &'static str {
    match self {
      Self::H264 { .. } => {
        "its keyframes are recovery points rather than IDR pictures, and pictures after them may reference the GOP before"
      }
      Self::Hevc { .. } => {
        "its keyframes are CRA pictures, whose RASL leading pictures reference the GOP before them"
      }
      Self::Resets => "its keyframes reset every reference",
      Self::Reordering => {
        "it reorders pictures, and this crate does not read its GOP headers to tell a closed GOP from an open one"
      }
    }
  }
}

/// The first byte of every NAL unit in `data`: length-prefixed by
/// `nal_length` bytes, or start-coded when `None`. `None` when a
/// length-prefixed unit runs past the end.
fn nal_headers(data: &[u8], nal_length: Option<usize>) -> Option<Vec<u8>> {
  let mut headers = Vec::new();
  match nal_length {
    Some(width) => {
      let mut at = 0usize;
      while at < data.len() {
        let field = data.get(at..at.checked_add(width)?)?;
        let length = field
          .iter()
          .fold(0usize, |length, &byte| (length << 8) | usize::from(byte));
        at += width;
        let unit = data.get(at..at.checked_add(length)?)?;
        if let Some(&header) = unit.first() {
          headers.push(header);
        }
        at += length;
      }
    }
    None => {
      let mut at = 0usize;
      while at + 3 <= data.len() {
        if data[at..at + 3] == [0, 0, 1] {
          if let Some(&header) = data.get(at + 3) {
            headers.push(header);
          }
          at += 3;
        } else {
          at += 1;
        }
      }
    }
  }
  Some(headers)
}

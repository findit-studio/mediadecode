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
  /// clean: nothing is proved about them. A NAL unit is read whole —
  /// every header byte present and valid, a picture's unit carrying
  /// payload past its header — or the access unit is not clean.
  pub(crate) fn is_clean(self, data: &[u8], reorders: bool) -> bool {
    match self {
      Self::H264 { nal_length } => nal_units(data, nal_length).is_some_and(|units| {
        let mut idr = false;
        for unit in units {
          // `forbidden_zero_bit` (1) · `nal_ref_idc` (2) · `nal_unit_type` (5).
          let Some(&header) = unit.first() else {
            return false;
          };
          if header & 0x80 != 0 {
            return false;
          }
          if header & 0x1f == 5 {
            // An IDR slice: its header byte and a slice header after it.
            if unit.len() < 2 {
              return false;
            }
            idr = true;
          }
        }
        idr
      }),
      Self::Hevc { nal_length } => nal_units(data, nal_length).is_some_and(|units| {
        for unit in units {
          let [first, second, ..] = unit else {
            return false;
          };
          if first & 0x80 != 0 || second & 0x07 == 0 {
            return false;
          }
          let kind = (first >> 1) & 0x3f;
          if kind < 32 {
            // The first picture's unit decides: it must carry a slice
            // header past its two header bytes.
            return unit.len() > 2 && (16..=20).contains(&kind);
          }
        }
        false
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

/// Every NAL unit in `data`, whole: length-prefixed by `nal_length`
/// bytes, or start-coded when `None`. `None` when a length-prefixed unit
/// runs past the end or is empty, or a start code ends the data.
fn nal_units(data: &[u8], nal_length: Option<usize>) -> Option<Vec<&[u8]>> {
  let mut units = Vec::new();
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
        if unit.is_empty() {
          return None;
        }
        units.push(unit);
        at += length;
      }
    }
    None => {
      // Every start code's position, then each unit runs to the next one.
      let mut starts = Vec::new();
      let mut at = 0usize;
      while at + 3 <= data.len() {
        if data[at..at + 3] == [0, 0, 1] {
          starts.push(at + 3);
          at += 3;
        } else {
          at += 1;
        }
      }
      for (index, &start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).map_or(data.len(), |&next| next - 3);
        let unit = data.get(start..end)?;
        if unit.is_empty() {
          return None;
        }
        units.push(unit);
      }
    }
  }
  Some(units)
}

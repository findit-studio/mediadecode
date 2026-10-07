//! **Where a decoder can start without losing a picture**: the clean
//! random access points at which a software session returns to its threads
//! after a fallback, and at which a post-commit resync is anchored (see
//! `CarrierVideoStreamDecoder::send_on_software`).
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
  /// H.264: a keyframe is clean when its first picture's NAL unit (types
  /// 1–5) is an IDR slice (5). A packet with any picture before the IDR is
  /// not: a decoder started there would begin on a picture that references
  /// what it never saw. Its NAL units are length-prefixed by `nal_length`
  /// bytes (`avcC`), or start-coded (Annex B) when `None`.
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
  /// clean: nothing is proved about them. EVERY NAL unit is read whole —
  /// every header byte present and valid, a picture's unit carrying
  /// payload past its header — or the access unit is not clean; the first
  /// picture's unit then decides.
  pub(crate) fn is_clean(self, data: &[u8], reorders: bool) -> bool {
    match self {
      Self::H264 { nal_length } => nal_units(data, nal_length).is_some_and(|units| {
        // Every unit whole; the first picture's unit decides, as HEVC's does.
        let mut first_picture = None;
        for unit in units {
          // `forbidden_zero_bit` (1) · `nal_ref_idc` (2) · `nal_unit_type` (5).
          let Some(&header) = unit.first() else {
            return false;
          };
          if header & 0x80 != 0 {
            return false;
          }
          let kind = header & 0x1f;
          // A prefix unit and a slice extension carry three more header
          // bytes; a picture's unit (1–5) carries a slice header past its own.
          let picture = (1..=5).contains(&kind);
          let header_bytes = if matches!(kind, 14 | 20 | 21) { 4 } else { 1 };
          if unit.len() < header_bytes + usize::from(picture) {
            return false;
          }
          // An IDR picture is a reference picture: `nal_ref_idc` is not zero.
          if kind == 5 && header & 0x60 == 0 {
            return false;
          }
          if picture {
            first_picture.get_or_insert(kind);
          }
        }
        first_picture == Some(5)
      }),
      Self::Hevc { nal_length } => nal_units(data, nal_length).is_some_and(|units| {
        // Every unit whole; the first picture's unit decides.
        let mut first_picture = None;
        for unit in units {
          // `forbidden_zero_bit` (1) · `nal_unit_type` (6) · `nuh_layer_id`
          // (6) · `nuh_temporal_id_plus1` (3), which is never zero.
          let [first, second, ..] = unit else {
            return false;
          };
          if first & 0x80 != 0 || second & 0x07 == 0 {
            return false;
          }
          let kind = (first >> 1) & 0x3f;
          if kind < 32 {
            // A picture's unit carries a slice segment header past its two
            // header bytes, and an IRAP picture (16–23) a temporal id of 0.
            if unit.len() <= 2 || ((16..=23).contains(&kind) && second & 0x07 != 1) {
              return false;
            }
            first_picture.get_or_insert(kind);
          }
        }
        first_picture.is_some_and(|kind| (16..=20).contains(&kind))
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
/// bytes, or start-coded (Annex B) when `None`. `None` when a
/// length-prefixed unit runs past the end or is empty, when anything but
/// zeros precedes the first start code, or when a start-coded unit is empty
/// once its `trailing_zero_8bits` are stripped — a start code ending the
/// data among them.
///
/// A four-byte start code (`00 00 00 01`) is read whole before a three-byte
/// one, so its leading zero is never left on the unit before it; and the
/// zero bytes Annex B allows after a unit (`trailing_zero_8bits`) are
/// stripped from it — a NAL unit never ends in a zero byte, since one whose
/// data would is given a final `03`.
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
      // Every start code — where it begins and where its unit does — the
      // four-byte prefix read before the three-byte one.
      let mut codes: Vec<(usize, usize)> = Vec::new();
      let mut at = 0usize;
      while at + 3 <= data.len() {
        if data[at..].starts_with(&[0, 0, 0, 1]) {
          codes.push((at, at + 4));
          at += 4;
        } else if data[at..].starts_with(&[0, 0, 1]) {
          codes.push((at, at + 3));
          at += 3;
        } else {
          at += 1;
        }
      }
      // Only zeros (`leading_zero_8bits`) may come before the first.
      let first = codes.first().map_or(data.len(), |&(code, _)| code);
      if data[..first].iter().any(|&byte| byte != 0) {
        return None;
      }
      // Each unit runs to the next start code, less its trailing zeros.
      for (index, &(_, start)) in codes.iter().enumerate() {
        let end = codes.get(index + 1).map_or(data.len(), |&(next, _)| next);
        let mut unit = data.get(start..end)?;
        while let [rest @ .., 0] = unit {
          unit = rest;
        }
        if unit.is_empty() {
          return None;
        }
        units.push(unit);
      }
    }
  }
  Some(units)
}

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
  /// picture's unit decides.
  ///
  /// The units are walked one at a time ([`NalUnits`]) and never collected,
  /// so the memory a packet costs here does not grow with how many units it
  /// packs: a hostile keyframe of one-byte units costs a walk, not a slice
  /// entry per unit.
  pub(crate) fn is_clean(self, data: &[u8], reorders: bool) -> bool {
    match self {
      Self::H264 { nal_length } => {
        first_h264_picture(data, nal_length).is_some_and(|(kind, _)| kind == 5)
      }
      Self::Hevc { nal_length } => {
        first_hevc_picture(data, nal_length).is_some_and(|kind| (16..=20).contains(&kind))
      }
      Self::Resets => true,
      Self::Reordering => !reorders,
    }
  }

  /// Whether the key-flagged packet `data` anchors a post-commit resync:
  /// its FIRST picture is proved a random-access picture by the bitstream,
  /// where the codec lets this crate read it. A packet whose first picture
  /// is not one does not anchor, whatever its flag says — the pictures
  /// before its random-access picture are no part of the reorder bound.
  ///
  /// - **H.264:** the first picture's NAL unit is an IDR slice (5), or a
  ///   non-IDR slice (1) whose header says I or SI — `first_mb_in_slice`
  ///   then `slice_type`, two exp-Golomb codes: the I picture of an open
  ///   GOP, a recovery point.
  /// - **HEVC:** the first picture's NAL unit is an IRAP picture (16–23), a
  ///   CRA (21) among them: the decoder resyncing kept its references, and
  ///   the reorder bound covers the leading pictures a CRA has.
  /// - **Every other codec** — one picture per packet: MPEG-4 part 2, VP8,
  ///   VP9, AV1 and the rest — **the key flag FFmpeg's parser set from the
  ///   bitstream is the proof.** That is the trust boundary: this crate
  ///   reads no picture header of theirs.
  ///
  /// Every NAL unit is read whole, as for [`Self::is_clean`]; bytes that do
  /// not parse anchor nothing.
  pub(crate) fn anchors(self, data: &[u8]) -> bool {
    match self {
      Self::H264 { nal_length } => first_h264_picture(data, nal_length)
        .is_some_and(|(kind, unit)| kind == 5 || (kind == 1 && intra_slice(unit))),
      Self::Hevc { nal_length } => {
        first_hevc_picture(data, nal_length).is_some_and(|kind| (16..=23).contains(&kind))
      }
      Self::Resets | Self::Reordering => true,
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

/// The first picture of an H.264 access unit — its NAL unit type (1–5) and
/// its unit — once EVERY unit is read whole and valid; `None` when one is
/// not, or when no picture is there. `forbidden_zero_bit` (1) ·
/// `nal_ref_idc` (2) · `nal_unit_type` (5); a prefix unit and a slice
/// extension carry three more header bytes; a picture's unit carries a
/// slice header past its own; an IDR picture is a reference picture.
fn first_h264_picture(data: &[u8], nal_length: Option<usize>) -> Option<(u8, &[u8])> {
  let mut first = None;
  for unit in NalUnits::new(data, nal_length) {
    let unit = unit.ok()?;
    let &header = unit.first()?;
    if header & 0x80 != 0 {
      return None;
    }
    let kind = header & 0x1f;
    let picture = (1..=5).contains(&kind);
    let header_bytes = if matches!(kind, 14 | 20 | 21) { 4 } else { 1 };
    if unit.len() < header_bytes + usize::from(picture) {
      return None;
    }
    if kind == 5 && header & 0x60 == 0 {
      return None;
    }
    if picture && first.is_none() {
      first = Some((kind, unit));
    }
  }
  first
}

/// The first picture's NAL unit type of an HEVC access unit, once EVERY
/// unit is read whole and valid; `None` when one is not, or when no picture
/// is there. `forbidden_zero_bit` (1) · `nal_unit_type` (6) · `nuh_layer_id`
/// (6) · `nuh_temporal_id_plus1` (3), which is never zero; a picture's unit
/// carries a slice segment header past its two header bytes, and an IRAP
/// picture (16–23) a temporal id of 0.
fn first_hevc_picture(data: &[u8], nal_length: Option<usize>) -> Option<u8> {
  let mut first = None;
  for unit in NalUnits::new(data, nal_length) {
    let unit = unit.ok()?;
    let [head, second, ..] = unit else {
      return None;
    };
    if head & 0x80 != 0 || second & 0x07 == 0 {
      return None;
    }
    let kind = (head >> 1) & 0x3f;
    if kind < 32 {
      if unit.len() <= 2 || ((16..=23).contains(&kind) && second & 0x07 != 1) {
        return None;
      }
      first.get_or_insert(kind);
    }
  }
  first
}

/// Whether the H.264 slice whose NAL unit is `unit` is an I or SI slice:
/// its header's `first_mb_in_slice` read past, its `slice_type` (0–9, the
/// upper five meaning every slice of the picture is that type) read off —
/// I is 2 and 7, SI is 4 and 9. A header that does not parse says neither.
fn intra_slice(unit: &[u8]) -> bool {
  let mut bits = RbspBits::new(unit.get(1..).unwrap_or_default());
  bits.ue().is_some()
    && bits
      .ue()
      .is_some_and(|slice_type| matches!(slice_type % 5, 2 | 4))
}

/// The bits of an H.264 raw byte sequence payload, most significant first,
/// its emulation prevention bytes — the `03` of a `00 00 03` — skipped.
struct RbspBits<'a> {
  bytes: &'a [u8],
  at: usize,
  bit: u8,
  zeros: usize,
}

impl<'a> RbspBits<'a> {
  fn new(bytes: &'a [u8]) -> Self {
    Self {
      bytes,
      at: 0,
      bit: 0,
      zeros: 0,
    }
  }

  fn next_bit(&mut self) -> Option<bool> {
    if self.bit == 0 {
      // A new byte: an emulation prevention byte is not payload.
      if self.zeros >= 2 && self.bytes.get(self.at) == Some(&3) {
        self.at += 1;
        self.zeros = 0;
      }
      let &byte = self.bytes.get(self.at)?;
      self.zeros = if byte == 0 { self.zeros + 1 } else { 0 };
    }
    let byte = *self.bytes.get(self.at)?;
    let value = byte & (0x80 >> self.bit) != 0;
    self.bit += 1;
    if self.bit == 8 {
      self.bit = 0;
      self.at += 1;
    }
    Some(value)
  }

  /// An unsigned exp-Golomb code, `ue(v)`: `n` zeros, a one, `n` more bits.
  fn ue(&mut self) -> Option<u32> {
    let mut zeros = 0u32;
    while !self.next_bit()? {
      zeros += 1;
      if zeros > 31 {
        return None;
      }
    }
    let mut value = 0u32;
    for _ in 0..zeros {
      value = (value << 1) | u32::from(self.next_bit()?);
    }
    Some((1u32 << zeros) - 1 + value)
  }
}

/// A unit that does not parse whole: the walk ends there, and the access
/// unit is not clean.
struct Malformed;

/// **Every NAL unit in `data`, whole, one at a time**: length-prefixed by
/// `nal_length` bytes, or start-coded (Annex B) when `None`. Each unit is
/// found as the walk reaches it and nothing is collected, so a walk takes
/// the same memory for one unit as for a billion.
///
/// The walk answers [`Malformed`] and ends when a length-prefixed unit runs
/// past the end or is empty, when anything but zeros precedes the first
/// start code, or when a start-coded unit is empty once its
/// `trailing_zero_8bits` are stripped — a start code ending the data among
/// them. Data with no start code at all, zeros only, has no unit.
///
/// A four-byte start code (`00 00 00 01`) is read whole before a three-byte
/// one, so its leading zero is never left on the unit before it; and the
/// zero bytes Annex B allows after a unit (`trailing_zero_8bits`) are
/// stripped from it — a NAL unit never ends in a zero byte, since one whose
/// data would is given a final `03`.
struct NalUnits<'a> {
  data: &'a [u8],
  /// The NAL length field's width, or `None` for start codes.
  nal_length: Option<usize>,
  walk: Walk,
}

/// Where a walk over NAL units stands.
#[derive(Clone, Copy)]
enum Walk {
  /// The next unit begins here: at its length field, or after its start
  /// code.
  At(usize),
  /// The next item is [`Malformed`], and the walk ends with it.
  Malformed,
  /// The walk is over.
  Done,
}

impl<'a> NalUnits<'a> {
  fn new(data: &'a [u8], nal_length: Option<usize>) -> Self {
    let walk = if nal_length.is_some() {
      Walk::At(0)
    } else {
      // The first unit begins after the first start code, and only zeros
      // (`leading_zero_8bits`) may come before it.
      let first = start_code(data, 0);
      let leading = first.map_or(data.len(), |(code, _)| code);
      if data[..leading].iter().any(|&byte| byte != 0) {
        Walk::Malformed
      } else {
        first.map_or(Walk::Done, |(_, unit)| Walk::At(unit))
      }
    };
    Self {
      data,
      nal_length,
      walk,
    }
  }
}

impl<'a> Iterator for NalUnits<'a> {
  type Item = Result<&'a [u8], Malformed>;

  fn next(&mut self) -> Option<Self::Item> {
    let at = match self.walk {
      Walk::At(at) => at,
      Walk::Malformed => {
        self.walk = Walk::Done;
        return Some(Err(Malformed));
      }
      Walk::Done => return None,
    };
    let data = self.data;
    let unit = match self.nal_length {
      Some(width) => {
        if at == data.len() {
          self.walk = Walk::Done;
          return None;
        }
        // The length field, then that many bytes.
        let start = at.checked_add(width);
        let length = start.and_then(|start| data.get(at..start)).map(|field| {
          field
            .iter()
            .fold(0usize, |length, &byte| (length << 8) | usize::from(byte))
        });
        match (start, length) {
          (Some(start), Some(length)) => start.checked_add(length).and_then(|end| {
            self.walk = Walk::At(end);
            data.get(start..end)
          }),
          _ => None,
        }
      }
      None => {
        // To the next start code, or to the end; the zeros that trail the
        // unit are not its own.
        let end = match start_code(data, at) {
          Some((code, next)) => {
            self.walk = Walk::At(next);
            code
          }
          None => {
            self.walk = Walk::Done;
            data.len()
          }
        };
        let mut unit = &data[at..end];
        while let [rest @ .., 0] = unit {
          unit = rest;
        }
        Some(unit)
      }
    };
    match unit {
      Some(unit) if !unit.is_empty() => Some(Ok(unit)),
      _ => {
        self.walk = Walk::Done;
        Some(Err(Malformed))
      }
    }
  }
}

/// The first start code at or after `from`: where it begins and where its
/// unit does, a four-byte prefix read before a three-byte one.
fn start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
  let mut at = from;
  while at + 3 <= data.len() {
    if data[at..].starts_with(&[0, 0, 0, 1]) {
      return Some((at, at + 4));
    }
    if data[at..].starts_with(&[0, 0, 1]) {
      return Some((at, at + 3));
    }
    at += 1;
  }
  None
}

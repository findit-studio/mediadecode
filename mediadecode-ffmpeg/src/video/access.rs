//! **Where a decoder can start without losing a picture**: the clean
//! random access points at which a software session returns to its threads
//! after a fallback, and the random-access pictures at which a post-commit
//! resync is anchored (see `CarrierVideoStreamDecoder::send_on_software`) —
//! each proved by the bitstream, never inferred from a decoder's state.
//!
//! A keyframe is a *clean* random access point when nothing after it in
//! decode order references a picture before it. A decoder opened there
//! then decodes every picture a decoder running through it would. An
//! open-GOP keyframe is not one. That covers an HEVC CRA, whose RASL
//! leading pictures reference the GOP before it, and an H.264 I-frame that
//! is a recovery point rather than an IDR. A decoder opened at such a
//! keyframe drops or conceals those pictures, so switching decoders there
//! would trade pictures for threads.
//!
//! A codec that codes every picture alone — its descriptor carries
//! `AV_CODEC_PROP_INTRA_ONLY`: ProRes, DNxHD, MJPEG, … — has no reference
//! to lose, so every one of its packets is both, its key flag or not.

use crate::CodecId;

#[cfg(test)]
mod tests;

/// How a stream's keyframes are read for cleanliness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyframeRule {
  /// H.264: a keyframe is clean when its first picture's NAL unit (types
  /// 1–5) is an IDR slice (5) that starts its picture — its
  /// `first_mb_in_slice` is 0. A packet with any picture before the IDR is
  /// not: a decoder started there would begin on a picture that references
  /// what it never saw; nor is one that opens on a later slice of the IDR
  /// picture, whose leading slices a decoder started there never sees. Its
  /// NAL units are length-prefixed by `nal_length` bytes (`avcC`), or
  /// start-coded (Annex B) when `None`.
  H264 {
    /// The NAL length field's width, from the `avcC` record.
    nal_length: Option<usize>,
  },
  /// HEVC: clean when its first picture is an IDR or a BLA (NAL types
  /// 16–20) whose first slice segment starts it
  /// (`first_slice_segment_in_pic_flag`). A BLA's RASL pictures are
  /// discarded by every decoder, so a new one loses nothing there. A CRA
  /// (21) is never clean.
  Hevc {
    /// The NAL length field's width, from the `hvcC` record.
    nal_length: Option<usize>,
  },
  /// MPEG-1 and MPEG-2 video: a keyframe is clean when a group-of-pictures
  /// header before its first picture says the GOP is closed (`closed_gop`):
  /// no B picture after the I picture references the GOP before it. A packet
  /// with no GOP header before its picture proves nothing.
  Mpeg12,
  /// VP8, VP9 and AV1: a keyframe refreshes every reference, and nothing
  /// leads it.
  Resets,
  /// A codec that codes every picture alone — its descriptor carries
  /// `AV_CODEC_PROP_INTRA_ONLY`: ProRes, DNxHD, MJPEG, Ut Video, … No
  /// picture references another, so EVERY packet is a clean random access
  /// point and a resync anchor, whatever its key flag says; the warning a
  /// fallback on one thread gives after a minute never fires for it.
  IntraOnly,
  /// Every other codec: never clean mid-stream. This crate reads none of
  /// its picture headers, and a decoder's `has_b_frames` before a keyframe
  /// proves nothing — FFmpeg raises it when it meets reordering, which an
  /// open GOP can introduce at that very keyframe — so the session returns to
  /// its threads only at a seek, whose first keyframe is a switch point
  /// whatever its kind.
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
    } else if codec_id == CodecId::MPEG2VIDEO.raw()
      || codec_id == ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_MPEG1VIDEO as i32
    {
      Self::Mpeg12
    } else if codec_id == CodecId::VP8.raw()
      || codec_id == CodecId::VP9.raw()
      || codec_id == CodecId::AV1.raw()
    {
      Self::Resets
    } else if CodecId::from_raw(codec_id).intra_only() {
      Self::IntraOnly
    } else {
      Self::Reordering
    }
  }

  /// Whether the keyframe `data` is a clean random access point: proved so by
  /// its bitstream, never by the state of the decoder it would replace. Bytes
  /// that do not parse are not clean: nothing is proved about them. EVERY NAL unit is read whole —
  /// every header byte present and valid, a picture's unit carrying
  /// payload past its header — or the access unit is not clean; the first
  /// picture's unit decides, and it must START its picture
  /// ([`h264_slice_starts_picture`], [`hevc_segment_starts_picture`]): a
  /// packet that opens on a later slice of a picture, or whose first
  /// picture's unit is cut before its slice header says so, is not clean.
  ///
  /// The units are walked one at a time ([`NalUnits`]) and never collected,
  /// so the memory a packet costs here does not grow with how many units it
  /// packs: a hostile keyframe of one-byte units costs a walk, not a slice
  /// entry per unit.
  pub(crate) fn is_clean(self, data: &[u8]) -> bool {
    match self {
      Self::H264 { nal_length } => first_h264_picture(data, nal_length)
        .is_some_and(|(kind, unit)| kind == 5 && h264_slice_starts_picture(unit)),
      Self::Hevc { nal_length } => first_hevc_picture(data, nal_length)
        .is_some_and(|(kind, unit)| (16..=20).contains(&kind) && hevc_segment_starts_picture(unit)),
      Self::Mpeg12 => closed_gop(data),
      Self::Resets | Self::IntraOnly => true,
      Self::Reordering => false,
    }
  }

  /// Whether every packet of the stream is a random access point, key flag
  /// or not: a codec that codes every picture alone.
  pub(crate) const fn every_packet(self) -> bool {
    matches!(self, Self::IntraOnly)
  }

  /// What the key-flagged packet `data` is as a post-commit resync anchor:
  /// `Some` where the bitstream proves it a random-access point, where the
  /// codec lets this crate read it; `None` otherwise, whatever its flag
  /// says. The anchor is [definitive](Anchor::definitive) where nothing after
  /// it references anything before it ([`Self::is_clean`]).
  ///
  /// - **H.264:** the first picture is an IDR picture (NAL unit 5); or the
  ///   access unit carries, before its first picture, a recovery point SEI
  ///   message (NAL unit 6, payload type 6), which the anchor carries
  ///   ([`RecoveryPoint`]). Either way the first picture's slice header
  ///   parses and starts the picture: `first_mb_in_slice` is 0, `slice_type`
  ///   at most 9 ([`h264_slice_starts_picture`]). An I picture with no such
  ///   message anchors nothing: a slice type describes that slice alone, a
  ///   non-IDR intra picture resets no reference, H.264 lets the pictures
  ///   after it reference what the decoder never saw until the signalled
  ///   recovery point, and FFmpeg's parser flags some such pictures key by
  ///   heuristic.
  /// - **HEVC:** the first picture's NAL unit is an IRAP picture (16–23), a
  ///   CRA (21) among them, whose first slice segment starts it
  ///   ([`hevc_segment_starts_picture`]): the decoder resyncing kept its
  ///   references, and the reorder bound ([`Proof::ReorderBound`]) covers
  ///   the leading pictures a CRA has.
  /// - **Every other codec** — one picture per packet: MPEG-4 part 2, VP8,
  ///   VP9, AV1 and the rest — **the key flag FFmpeg's parser set from the
  ///   bitstream is the proof.** That is the trust boundary: this crate
  ///   reads no picture header of theirs.
  ///
  /// Every NAL unit is read whole, as for [`Self::is_clean`], and every SEI
  /// message before the first picture walked by its size over the raw byte
  /// sequence payload; bytes that do not parse anchor nothing.
  pub(crate) fn anchor(self, data: &[u8]) -> Option<Anchor> {
    let anchor = |recovery| Anchor {
      definitive: self.is_clean(data),
      recovery,
    };
    match self {
      Self::H264 { nal_length } => {
        let (kind, unit) = first_h264_picture(data, nal_length)?;
        if !h264_slice_starts_picture(unit) {
          return None;
        }
        match kind {
          5 => Some(anchor(None)),
          1 | 2 => recovery_point(data, nal_length).map(|point| anchor(Some(point))),
          _ => None,
        }
      }
      Self::Hevc { nal_length } => first_hevc_picture(data, nal_length)
        .is_some_and(|(kind, unit)| (16..=23).contains(&kind) && hevc_segment_starts_picture(unit))
        .then(|| anchor(None)),
      Self::Mpeg12 | Self::Resets | Self::IntraOnly | Self::Reordering => Some(anchor(None)),
    }
  }

  /// How a post-commit resync anchored on this rule's stream is proved, where
  /// libavcodec's own decoder for the codec decodes it. Both proofs are
  /// invariants of those decoders: the session proves nothing on an
  /// implementation that wraps another — `h264_cuvid`, `h264_qsv`,
  /// `libdav1d`, … — and refuses a post-commit fallback onto one, and an
  /// H.264 anchor after a decode error across the gap takes the reorder
  /// bound (the session's proof table, `resync_proof`).
  ///
  /// - **H.264: [`Proof::Withheld`].** FFmpeg's H.264 decoder outputs only
  ///   pictures its own recovery tracking has marked recovered — an IDR
  ///   picture and every picture after it in decode order, a recovery
  ///   point's recovery and every picture after it in output order — as long
  ///   as neither `AV_CODEC_FLAG_OUTPUT_CORRUPT` nor
  ///   `AV_CODEC_FLAG2_SHOW_ALL` is set, which the session's software
  ///   decoders refuse at the open. A decoder opened cold across the gap
  ///   starts with nothing recovered, so every picture it delivers is one its
  ///   tracking recovered: the first out after the anchor closes the gap, and
  ///   a picture from before the anchor that comes out after it is one of
  ///   those. That tracking is FFmpeg's, and this crate takes its word, as it
  ///   takes the parser's key flag for the codecs whose pictures it does not
  ///   read.
  /// - **Every other codec: [`Proof::ReorderBound`].** FFmpeg's HEVC decoder
  ///   keeps no such gate for a stream it is already decoding, and this
  ///   crate reads nothing of the other codecs' recovery.
  pub(crate) const fn proof(self) -> Proof {
    match self {
      Self::H264 { .. } => Proof::Withheld,
      Self::Hevc { .. } | Self::Mpeg12 | Self::Resets | Self::IntraOnly | Self::Reordering => {
        Proof::ReorderBound
      }
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
      Self::Mpeg12 => {
        "its GOPs are open (no GOP header before the keyframe says closed_gop), so B pictures after a keyframe may reference the GOP before it"
      }
      Self::Resets => "its keyframes reset every reference",
      Self::IntraOnly => "it codes every picture alone, so every packet is a clean point",
      Self::Reordering => {
        "this crate cannot prove its keyframes clean from its bitstream, so the session returns to its threads only at a seek"
      }
    }
  }
}

/// How a post-commit resync is proved once a packet has anchored it
/// ([`KeyframeRule::proof`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Proof {
  /// **The decoder withholds every picture it has not recovered**, so the
  /// first picture out after the anchor closes the gap.
  Withheld,
  /// **The reorder bound**: the pictures from before the anchor that can
  /// still come out are the ones its reorder buffer holds, so the gap closes
  /// at the picture out past them — the first for a stream that does not
  /// reorder (VP8, VP9, AV1).
  ReorderBound,
}

/// A post-commit resync anchor, as its bitstream proves it
/// ([`KeyframeRule::anchor`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Anchor {
  definitive: bool,
  recovery: Option<RecoveryPoint>,
}

impl Anchor {
  /// Whether nothing after the anchor references anything before it — a
  /// clean random access point ([`KeyframeRule::is_clean`]): an H.264 IDR
  /// picture, an HEVC IDR or BLA picture, a closed MPEG-1/2 GOP, a VP8, VP9
  /// or AV1 keyframe, any packet of a codec that codes every picture alone.
  /// Such an anchor supersedes one taken across the same gap that is not.
  pub(crate) const fn definitive(self) -> bool {
    self.definitive
  }

  /// The H.264 recovery point the anchor stands on, if any — reported,
  /// never counted: FFmpeg's decoder withholds the pictures before the
  /// recovery it signals itself ([`Proof::Withheld`]).
  pub(crate) const fn recovery(self) -> Option<RecoveryPoint> {
    self.recovery
  }
}

/// An H.264 recovery point SEI message's account of its recovery (H.264
/// D.2.8), read field by field and carried for reporting.
///
/// **What anchoring at one proves**, exact or approximate: the pictures from
/// the anchor on are the pictures a decoder STARTED at this random-access
/// point produces — FFmpeg starts at recovery points whatever the message's
/// flags say — not that they match a decode that ran through the gap bit for
/// bit. An approximate recovery point ([`Self::exact_match`] `false`) says
/// they need not; it anchors all the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecoveryPoint {
  frames: u32,
  exact_match: bool,
  broken_link: bool,
}

impl RecoveryPoint {
  /// `recovery_frame_cnt`: the pictures are recovered from the recovery
  /// point on, that many frames after it in output order.
  pub(crate) const fn frames(self) -> u32 {
    self.frames
  }

  /// `exact_match_flag`: whether the pictures from the recovery on match a
  /// decode that started before the recovery point exactly — `false` for an
  /// approximate recovery, whose pictures need not.
  pub(crate) const fn exact_match(self) -> bool {
    self.exact_match
  }

  /// `broken_link_flag`: whether the pictures at the recovery point may hold
  /// serious visual artefacts from what came before it — the stream was
  /// spliced there. Nothing forward changes for it.
  pub(crate) const fn broken_link(self) -> bool {
    self.broken_link
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

/// The first picture of an HEVC access unit — its NAL unit type and its
/// unit — once EVERY unit is read whole and valid; `None` when one is not,
/// or when no picture is there. `forbidden_zero_bit` (1) · `nal_unit_type`
/// (6) · `nuh_layer_id` (6) · `nuh_temporal_id_plus1` (3), which is never
/// zero; a picture's unit carries a slice segment header past its two
/// header bytes, and an IRAP picture (16–23) a temporal id of 0.
fn first_hevc_picture(data: &[u8], nal_length: Option<usize>) -> Option<(u8, &[u8])> {
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
      first.get_or_insert((kind, unit));
    }
  }
  first
}

/// Whether the H.264 slice whose NAL unit is `unit` starts its picture: its
/// header opens as a slice header must — `first_mb_in_slice`, then
/// `slice_type`, two exp-Golomb codes, the type one of the ten there are
/// (0–9; the upper five say every slice of the picture is that type) — and
/// `first_mb_in_slice` is 0, the slice holding the picture's first
/// macroblock. A later slice of the picture (what opens a packet that holds
/// the rest of a picture split across packets), or a unit cut before its
/// slice type, does not: a decoder started there would never see the slices
/// before it. A stream coded with arbitrary slice order, whose picture may
/// open on another slice, is read as starting none — never clean, never an
/// anchor.
fn h264_slice_starts_picture(unit: &[u8]) -> bool {
  let mut bits = RbspBits::new(unit.get(1..).unwrap_or_default());
  bits.ue() == Some(0) && bits.ue().is_some_and(|slice_type| slice_type <= 9)
}

/// Whether the HEVC slice segment whose NAL unit is `unit` starts its
/// picture: the first bit of its slice segment header,
/// `first_slice_segment_in_pic_flag`, is 1. The header's first byte follows
/// the two NAL header bytes, the second never zero, so no emulation
/// prevention byte can stand there.
fn hevc_segment_starts_picture(unit: &[u8]) -> bool {
  unit.get(2).is_some_and(|byte| byte & 0x80 != 0)
}

/// The recovery point SEI message an H.264 access unit carries before its
/// first picture; `None` where none does, or where an SEI unit before it
/// does not parse. The units are read as [`first_h264_picture`] reads them,
/// one at a time.
fn recovery_point(data: &[u8], nal_length: Option<usize>) -> Option<RecoveryPoint> {
  for unit in NalUnits::new(data, nal_length) {
    let unit = unit.ok()?;
    let kind = unit.first()? & 0x1f;
    if (1..=5).contains(&kind) {
      return None;
    }
    if kind == 6
      && let Some(frames) = sei_recovery_point(unit.get(1..)?)?
    {
      return Some(frames);
    }
  }
  None
}

/// The SEI payload type of a recovery point message (H.264 D.1.8).
const RECOVERY_POINT: u32 = 6;

/// The recovery point an SEI unit's raw byte sequence payload `rbsp` states:
/// `Some(Some(point))` for its first recovery point message, `Some(None)`
/// where it has none, `None` where its messages do not parse.
/// Every `sei_message` is walked whole by its `payload_size` — its type and
/// size each a run of `FF` bytes and a last byte — over the payload with its
/// emulation prevention bytes removed, up to the `rbsp_trailing_bits`; a
/// payload that runs past the unit does not parse. A recovery point's
/// `recovery_frame_cnt` (`ue(v)`), `exact_match_flag`, `broken_link_flag`
/// and `changing_slice_group_idc` (two bits) lie within its payload.
fn sei_recovery_point(rbsp: &[u8]) -> Option<Option<RecoveryPoint>> {
  let mut bits = RbspBits::new(rbsp);
  let mut found = None;
  while !bits.at_trailing_bits() {
    let payload_type = bits.sei_value()?;
    let payload_size = bits.sei_value()?;
    let end = bits
      .read()
      .checked_add(usize::try_from(payload_size).ok()?.checked_mul(8)?)?;
    if payload_type == RECOVERY_POINT && found.is_none() {
      let frames = bits.ue()?;
      let exact_match = bits.next_bit()?;
      let broken_link = bits.next_bit()?;
      // `changing_slice_group_idc`, two bits.
      bits.next_bit()?;
      bits.next_bit()?;
      if bits.read() > end {
        return None;
      }
      found = Some(RecoveryPoint {
        frames,
        exact_match,
        broken_link,
      });
    }
    while bits.read() < end {
      bits.next_bit()?;
    }
  }
  Some(found)
}

/// The bits of an H.264 raw byte sequence payload, most significant first,
/// its emulation prevention bytes — the `03` of a `00 00 03` — skipped.
struct RbspBits<'a> {
  bytes: &'a [u8],
  at: usize,
  bit: u8,
  zeros: usize,
  /// The payload bits read so far, emulation prevention bytes not among
  /// them.
  read: usize,
}

impl<'a> RbspBits<'a> {
  fn new(bytes: &'a [u8]) -> Self {
    Self {
      bytes,
      at: 0,
      bit: 0,
      zeros: 0,
      read: 0,
    }
  }

  /// The payload bits read so far.
  const fn read(&self) -> usize {
    self.read
  }

  /// Whether only the `rbsp_trailing_bits` are left, at a byte boundary:
  /// the stop bit's `80`, or nothing — a unit never ends in a zero byte.
  fn at_trailing_bits(&self) -> bool {
    self.bit == 0 && matches!(self.bytes.get(self.at..), Some([] | [0x80]) | None)
  }

  /// An SEI message's payload type or size: a run of `FF` bytes, each 255,
  /// and a last byte.
  fn sei_value(&mut self) -> Option<u32> {
    let mut value = 0u32;
    loop {
      let mut byte = 0u32;
      for _ in 0..8 {
        byte = (byte << 1) | u32::from(self.next_bit()?);
      }
      value = value.checked_add(byte)?;
      if byte != 0xff {
        return Some(value);
      }
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
    self.read += 1;
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

/// Whether an MPEG-1 or MPEG-2 video packet carries, before its first
/// picture (start code `00`), a group-of-pictures header (`B8`) whose
/// `closed_gop` flag is set: `time_code` is the header's first 25 bits, and
/// `closed_gop` the next. Start codes are walked one at a time, in constant
/// memory.
fn closed_gop(data: &[u8]) -> bool {
  let mut closed = false;
  let mut at = 0usize;
  while let Some((_, value)) = start_code(data, at) {
    match data.get(value) {
      Some(0x00) => return closed,
      Some(0xb8) => {
        // The start code's value, then `time_code`'s 25 bits: `closed_gop`
        // is the second bit of the fourth byte after the value.
        let Some(&flags) = data.get(value + 4) else {
          return false;
        };
        closed = flags & 0x40 != 0;
      }
      _ => {}
    }
    at = value;
  }
  false
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

//! What a packet's H.264 or HEVC bitstream says about its picture: whether
//! anything references it, so that dropping the packet loses that picture
//! and no other.
//!
//! [`NalStream::read`] reads one packet's NAL unit headers and answers
//! [`Verdict::Disposable`] only where the bitstream itself proves that
//! nothing references the packet's picture, and that nothing else the
//! packet carries is needed later. Anything it cannot prove keeps the
//! packet ([`Verdict::Kept`]), with the reason. A demuxer ORs the verdict
//! into [`PacketFlags::DISPOSABLE`](crate::packet::PacketFlags::DISPOSABLE)
//! beside the container's own mark, so the fact is the same whichever
//! container carried the stream.
//!
//! It reads nothing but headers — a byte for H.264, two for HEVC, and the
//! first bytes of an HEVC parameter set — and allocates nothing.
//!
//! # H.264
//!
//! A non-reference picture — one coded with `nal_ref_idc` equal to 0 — "is
//! not used for inter prediction of any other pictures" (H.264, 3.104), and
//! a slice or slice data partition of `nal_ref_idc` 0 is part of one
//! (7.4.1). An IDR slice's `nal_ref_idc` is never 0, nor a parameter set's
//! (7.4.1). So a packet is disposable iff it holds at least one coded slice
//! (NAL unit types 1–5, Table 7-1), every slice has `nal_ref_idc` equal to
//! 0, and none is an IDR slice (type 5). Every slice is checked, so a
//! packet that carries a field pair is disposable only where both fields
//! are. Beside the slices, only SEI (6), access unit delimiters (9) and
//! filler data (12) may ride: a parameter set (7, 8, 13, 15), an end of
//! sequence or of stream (10, 11), and any type this module does not read
//! keep the packet, because a later picture may need them; and a NAL unit
//! of the scalable, multiview or 3-D extensions (14, 20, 21) keeps it too,
//! because a multiview view component of `nal_ref_idc` 0 whose
//! `inter_view_flag` is 1 "may be used for inter-view prediction by other
//! view components in the current access unit" (G.7.4.1.1).
//!
//! FFmpeg's `skip_frame = AVDISCARD_NONREF` skips every NAL unit of
//! `nal_ref_idc` 0 but SEI, one at a time (`libavcodec/h264dec.c` 626–628
//! in 9.0.1); this decides for the whole packet, the unit a consumer drops.
//!
//! # HEVC
//!
//! A sub-layer non-reference (SLNR) picture — NAL unit types TRAIL_N, TSA_N,
//! STSA_N, RADL_N, RASL_N and RSV_VCL_N10, N12 and N14 (H.265, 7.4.2.2) —
//! holds samples no later picture **of the same sub-layer** uses, but a
//! picture of a higher sub-layer may (3.162 and its note). A picture never
//! references one of a higher sub-layer, nor an SLNR picture of its own
//! (8.3.2). So an SLNR picture is disposable at the **highest** temporal
//! sub-layer the stream declares, and only there: every slice an SLNR type
//! of layer 0 at a TemporalId equal to that highest sub-layer.
//!
//! The highest sub-layer is the most any parameter set declares:
//! `sps_max_sub_layers_minus1` (7.4.3.2.1), `vps_max_sub_layers_minus1`,
//! which bounds it, and a configuration record's `numTemporalLayers` less
//! one where it is not 0. It only ever rises, so a reading that is off can
//! only fail to mark. Nothing is disposable before a sequence parameter set
//! has been seen, in the configuration record or in the stream.
//!
//! Beside the slices, only access unit delimiters (35), filler data (38)
//! and SEI (39, 40) may ride: a parameter set (32–34), an end of sequence
//! or of bitstream (36, 37), a reserved or unspecified type, and any NAL
//! unit of a layer above 0 keep the packet. An IRAP picture is never
//! disposable.
//!
//! **FFmpeg's own `AVDISCARD_NONREF` has no temporal guard**: it skips
//! every SLNR NAL unit at every sub-layer (`libavcodec/hevc/hevcdec.c`
//! 3766–3767, the eight types of `ff_hevc_nal_is_nonref`,
//! `libavcodec/hevc/hevcdec.h` 653–667). On a temporally scalable stream
//! it can drop a picture a higher sub-layer references; this module does
//! not.
//!
//! # Framing
//!
//! A track's packets carry their NAL units either each after a big-endian
//! length or each after a start code (Annex B). The configuration record
//! decides, as FFmpeg decides it: an `avcC` is extradata whose first byte
//! is 1 (`libavcodec/h264_parse.c` 466–516), an `hvcC` extradata of at
//! least 23 bytes whose first byte is 1, or 0 with a nonzero second byte
//! or a third above 1 (`libavcodec/hevc/parse.c` 79–136); each gives the
//! length's size, its `lengthSizeMinusOne` plus one. Anything else, and no
//! extradata at all, is Annex B. A record whose length size is 3, or which
//! ends before its own parameter sets do, frames nothing this module
//! reads, and every packet is kept ([`KeptReason::Unframed`]).
//!
//! Where the length is four bytes, FFmpeg's H.264 decoder also reads a
//! packet that begins with a four-byte start code, and whose next four
//! bytes are no length that fits, as Annex B, and reads it so until a
//! packet's first four bytes are a length that fits again
//! (`libavcodec/h264dec.c` 602–607). This module follows the same rule.
//!
//! # What is never marked
//!
//! A codec other than H.264 and HEVC is never read: its packets carry the
//! container's mark alone. And a packet that does not parse — a NAL unit
//! whose forbidden bit is set, a length of 0 or one past the packet's end
//! (which FFmpeg refuses too, `libavcodec/h2645_parse.h` 130–148), bytes
//! that form no NAL unit, an HEVC TemporalId of −1 or past the highest
//! sub-layer, a parameter set too short to read — is kept as
//! [`KeptReason::Malformed`]: a packet this module cannot read is one it
//! cannot prove disposable.
//!
//! The citations are FFmpeg 9.0.1's sources, and Rec. ITU-T H.264 (V16,
//! 06/2026) and H.265 (V11, 01/2026) by clause.

use core::fmt;

/// The codecs whose bitstream [`NalStream`] reads.
///
/// Every other codec is unclassified: whether its packets are disposable
/// is the container's to say, and a demuxer marks them from the container
/// alone.
///
/// Marked `#[non_exhaustive]`: a later codec joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NalCodec {
  /// H.264 / AVC.
  H264,
  /// H.265 / HEVC.
  Hevc,
}

/// How a track's packets carry their NAL units.
///
/// Marked `#[non_exhaustive]`: a later framing joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Framing {
  /// Each NAL unit after a big-endian length of this many bytes, as an
  /// `avcC` or `hvcC` configuration record declares.
  Length(LengthSize),
  /// Each NAL unit after a start code, `00 00 01` (Annex B).
  AnnexB,
}

/// The size of the length before each NAL unit: a configuration record's
/// `lengthSizeMinusOne` plus one. A record of 3 frames nothing a decoder
/// reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LengthSize {
  /// One byte.
  One,
  /// Two bytes.
  Two,
  /// Four bytes.
  Four,
}

impl LengthSize {
  /// The size, in bytes.
  pub const fn bytes(self) -> usize {
    match self {
      Self::One => 1,
      Self::Two => 2,
      Self::Four => 4,
    }
  }

  /// From a record's `lengthSizeMinusOne` field, its low two bits; `None`
  /// for 2, a length of three bytes.
  const fn from_minus_one(bits: u8) -> Option<Self> {
    match bits & 3 {
      0 => Some(Self::One),
      1 => Some(Self::Two),
      3 => Some(Self::Four),
      _ => None,
    }
  }
}

/// What [`NalStream::read`] answers for one packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
  /// Nothing references the packet's picture, and nothing else in it is
  /// needed later: dropping it loses that picture and no other.
  Disposable,
  /// The packet is kept, for this reason.
  Kept(KeptReason),
}

impl Verdict {
  /// Whether the packet is disposable.
  pub const fn is_disposable(self) -> bool {
    matches!(self, Self::Disposable)
  }
}

/// Why [`NalStream::read`] keeps a packet: the first NAL unit, in the
/// packet's order, that keeps it, or what about the packet does.
///
/// Marked `#[non_exhaustive]`: a later reason joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KeptReason {
  /// The packet holds no picture: no coded slice.
  NoPicture,
  /// A picture something may reference: an H.264 slice whose
  /// `nal_ref_idc` is not 0, or an HEVC sub-layer reference picture.
  Referenced,
  /// A random-access picture: an H.264 IDR slice, or an HEVC IRAP one.
  RandomAccess,
  /// A NAL unit beside the picture that a later picture may need, of this
  /// `nal_unit_type`: a parameter set, an end of sequence or of stream, a
  /// reserved type, or any other than SEI, an access unit delimiter or
  /// filler data.
  Companion(u8),
  /// A NAL unit of an extension: H.264's scalable, multiview or 3-D slices
  /// and prefixes (14, 20, 21), or any HEVC NAL unit of a layer above 0.
  Extension,
  /// An HEVC sub-layer non-reference picture below the highest temporal
  /// sub-layer, which a picture of a higher sub-layer may reference.
  BelowHighestSubLayer,
  /// An HEVC picture before any sequence parameter set: the stream's
  /// highest temporal sub-layer is not yet known.
  SubLayersUnknown,
  /// The packet does not parse as its track's framing says.
  Malformed,
  /// The track's configuration record frames nothing this module reads: a
  /// length of three bytes, or a record that ends early.
  Unframed,
}

impl fmt::Display for KeptReason {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::NoPicture => f.write_str("no picture"),
      Self::Referenced => f.write_str("a picture something may reference"),
      Self::RandomAccess => f.write_str("a random-access picture"),
      Self::Companion(nal_unit_type) => {
        write!(
          f,
          "a NAL unit of type {nal_unit_type} a later picture may need"
        )
      }
      Self::Extension => f.write_str("a NAL unit of an extension"),
      Self::BelowHighestSubLayer => f.write_str("a picture below the highest temporal sub-layer"),
      Self::SubLayersUnknown => f.write_str("a picture before any sequence parameter set"),
      Self::Malformed => f.write_str("a packet that does not parse"),
      Self::Unframed => f.write_str("a configuration record that frames nothing"),
    }
  }
}

/// One track's reading of its H.264 or HEVC bitstream: its codec, its
/// framing, and for HEVC the highest temporal sub-layer any parameter set
/// has declared so far.
///
/// Built from the track's configuration record ([`Self::new`]), rebuilt
/// where the configuration changes mid-stream ([`Self::reconfigure`]), and
/// fed each of the track's packets in decoding order ([`Self::read`]),
/// which learns from every parameter set it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NalStream {
  codec: NalCodec,
  /// `None` where the configuration record frames nothing this module
  /// reads.
  framing: Option<Framing>,
  /// What the configuration record declared, before FFmpeg's H.264
  /// per-packet reading moves [`Self::framing`].
  configured: Option<Framing>,
  /// The most any parameter set or configuration record has declared.
  highest_sub_layer: u8,
  /// Whether a sequence parameter set has been read.
  sps_seen: bool,
}

impl NalStream {
  /// The reading of a track of `codec` whose configuration record — its
  /// extradata — is `extradata`, empty for none.
  pub fn new(codec: NalCodec, extradata: &[u8]) -> Self {
    let mut stream = Self {
      codec,
      framing: None,
      configured: None,
      highest_sub_layer: 0,
      sps_seen: false,
    };
    stream.reconfigure(extradata);
    stream
  }

  /// Reads a new configuration record — a mid-stream change of the
  /// track's extradata — for its framing, and learns from its parameter
  /// sets. What earlier parameter sets declared is kept: the highest
  /// sub-layer only rises.
  pub fn reconfigure(&mut self, extradata: &[u8]) {
    self.framing = match self.codec {
      NalCodec::H264 => avc_record(extradata),
      NalCodec::Hevc => self.hevc_record(extradata),
    };
    self.configured = self.framing;
  }

  /// The codec read.
  pub const fn codec(&self) -> NalCodec {
    self.codec
  }

  /// The framing the next packet is read in; `None` where the
  /// configuration record frames nothing this module reads.
  pub const fn framing(&self) -> Option<Framing> {
    self.framing
  }

  /// For HEVC, the highest temporal sub-layer any parameter set has
  /// declared so far; `None` before a sequence parameter set has been read,
  /// and for H.264.
  pub const fn highest_sub_layer(&self) -> Option<u8> {
    match self.codec {
      NalCodec::Hevc if self.sps_seen => Some(self.highest_sub_layer),
      _ => None,
    }
  }

  /// Reads one packet's NAL unit headers: whether nothing references its
  /// picture. Packets are read in decoding order; every parameter set read
  /// raises what later packets are held to.
  pub fn read(&mut self, packet: &[u8]) -> Verdict {
    let Some(mut framing) = self.framing else {
      return Verdict::Kept(KeptReason::Unframed);
    };
    if self.codec == NalCodec::H264 && self.configured == Some(Framing::Length(LengthSize::Four)) {
      framing = h264_packet_framing(packet, framing);
      self.framing = Some(framing);
    }
    let mut kept = None;
    let mut pictures = 0_usize;
    let mut units = Units::new(packet, framing);
    loop {
      match units.next() {
        Unit::End => break,
        Unit::Broken => return Verdict::Kept(KeptReason::Malformed),
        Unit::Nal(nal) => {
          let read = match self.codec {
            NalCodec::H264 => h264_unit(nal),
            NalCodec::Hevc => self.hevc_unit(nal),
          };
          match read {
            Read::Picture => pictures += 1,
            Read::Beside => {}
            Read::Keeps(KeptReason::Malformed) => {
              return Verdict::Kept(KeptReason::Malformed);
            }
            Read::Keeps(reason) => {
              kept.get_or_insert(reason);
            }
          }
        }
      }
    }
    match kept {
      Some(reason) => Verdict::Kept(reason),
      None if pictures == 0 => Verdict::Kept(KeptReason::NoPicture),
      None => Verdict::Disposable,
    }
  }

  /// An HEVC configuration record's framing, FFmpeg's test, learning the
  /// sub-layers its parameter sets and `numTemporalLayers` declare.
  fn hevc_record(&mut self, extradata: &[u8]) -> Option<Framing> {
    let record = extradata.len() >= 23
      && (extradata[0] == 1 || (extradata[0] == 0 && (extradata[1] != 0 || extradata[2] > 1)));
    if !record {
      // Annex B extradata: its parameter sets are read as a packet's are.
      if !extradata.is_empty() {
        let mut units = Units::new(extradata, Framing::AnnexB);
        while let Unit::Nal(nal) = units.next() {
          let _ = self.hevc_unit(nal);
        }
      }
      return Some(Framing::AnnexB);
    }
    // Byte 21: `constantFrameRate` (2), `numTemporalLayers` (3),
    // `temporalIdNested` (1), `lengthSizeMinusOne` (2)
    // (`libavformat/hevc.c` 1159–1167).
    let layers = (extradata[21] >> 3) & 7;
    let length = LengthSize::from_minus_one(extradata[21])?;
    // The arrays, from byte 23: each a type byte, a 16-bit count, then
    // that many NAL units, each after a 16-bit length.
    let count = usize::from(extradata[22]);
    let mut at = 23;
    for _ in 0..count {
      let units = u16::from_be_bytes([*extradata.get(at + 1)?, *extradata.get(at + 2)?]);
      at += 3;
      for _ in 0..units {
        let size = usize::from(u16::from_be_bytes([
          *extradata.get(at)?,
          *extradata.get(at + 1)?,
        ]));
        at += 2;
        let nal = extradata.get(at..at + size)?;
        at += size;
        if self.hevc_unit(nal) == Read::Keeps(KeptReason::Malformed) {
          return None;
        }
      }
    }
    if layers > 0 {
      self.raise(layers - 1);
    }
    Some(Framing::Length(length))
  }

  /// Raises the highest sub-layer to `declared`; it never falls.
  fn raise(&mut self, declared: u8) {
    if declared > self.highest_sub_layer {
      self.highest_sub_layer = declared;
    }
  }

  /// One HEVC NAL unit, read and learned from.
  fn hevc_unit(&mut self, nal: &[u8]) -> Read {
    let &[first, second, ..] = nal else {
      return Read::Keeps(KeptReason::Malformed);
    };
    if first & 0x80 != 0 {
      return Read::Keeps(KeptReason::Malformed);
    }
    let nal_unit_type = (first >> 1) & 0x3F;
    let layer = ((first & 1) << 5) | (second >> 3);
    let Some(temporal_id) = (second & 7).checked_sub(1) else {
      return Read::Keeps(KeptReason::Malformed);
    };
    if layer != 0 {
      return Read::Keeps(KeptReason::Extension);
    }
    match nal_unit_type {
      // The SLNR types: TRAIL_N, TSA_N, STSA_N, RADL_N, RASL_N and
      // RSV_VCL_N10, N12, N14 (7.4.2.2).
      0 | 2 | 4 | 6 | 8 | 10 | 12 | 14 => {
        if !self.sps_seen {
          Read::Keeps(KeptReason::SubLayersUnknown)
        } else if temporal_id < self.highest_sub_layer {
          Read::Keeps(KeptReason::BelowHighestSubLayer)
        } else if temporal_id > self.highest_sub_layer {
          // No parameter set declares the sub-layer: not as the stream
          // says it is.
          Read::Keeps(KeptReason::Malformed)
        } else {
          Read::Picture
        }
      }
      // The sub-layer reference types (TRAIL_R … RSV_VCL_R15).
      1 | 3 | 5 | 7 | 9 | 11 | 13 | 15 => Read::Keeps(KeptReason::Referenced),
      // IRAP: BLA, IDR, CRA, and the reserved IRAP types 22 and 23.
      16..=23 => Read::Keeps(KeptReason::RandomAccess),
      // VPS: `vps_max_sub_layers_minus1`, three bits of the second byte
      // after the header.
      32 => match nal.get(3) {
        Some(&byte) => self.declare((byte >> 1) & 7, false),
        None => Read::Keeps(KeptReason::Malformed),
      },
      // SPS: `sps_max_sub_layers_minus1`, three bits of the first byte
      // after the header (7.3.2.2.1).
      33 => match nal.get(2) {
        Some(&byte) => self.declare((byte >> 1) & 7, true),
        None => Read::Keeps(KeptReason::Malformed),
      },
      // AUD, FD, prefix and suffix SEI.
      35 | 38 | 39 | 40 => Read::Beside,
      // PPS, EOS, EOB, the reserved VCL types 24–31, the reserved non-VCL
      // types 41–47 and the unspecified 48–63.
      other => Read::Keeps(KeptReason::Companion(other)),
    }
  }

  /// A parameter set's declared `max_sub_layers_minus1`, learned; the
  /// unit keeps its packet as the parameter set it is.
  fn declare(&mut self, max_sub_layers_minus1: u8, sps: bool) -> Read {
    // The field lies in 0..=6 (7.4.3.1, 7.4.3.2.1).
    if max_sub_layers_minus1 > 6 {
      return Read::Keeps(KeptReason::Malformed);
    }
    self.raise(max_sub_layers_minus1);
    if sps {
      self.sps_seen = true;
    }
    Read::Keeps(KeptReason::Companion(if sps { 33 } else { 32 }))
  }
}

/// An H.264 configuration record's framing, FFmpeg's test.
fn avc_record(extradata: &[u8]) -> Option<Framing> {
  if extradata.first() != Some(&1) {
    return Some(Framing::AnnexB);
  }
  // `avcC`: version, profile, compatibility, level, then six reserved bits
  // and `lengthSizeMinusOne`, then the parameter sets.
  if !avc_record_whole(extradata) {
    return None;
  }
  LengthSize::from_minus_one(extradata[4]).map(Framing::Length)
}

/// Whether an `avcC` holds every parameter set it counts, as FFmpeg reads
/// it: at least seven bytes, then each count's units within the record
/// (`libavcodec/h264_parse.c` 482–513).
fn avc_record_whole(record: &[u8]) -> bool {
  if record.len() < 7 {
    return false;
  }
  let mut at = 6;
  let sets = |count: usize, at: &mut usize| -> bool {
    for _ in 0..count {
      let (Some(&high), Some(&low)) = (record.get(*at), record.get(*at + 1)) else {
        return false;
      };
      let size = usize::from(u16::from_be_bytes([high, low])) + 2;
      if size > record.len() - *at {
        return false;
      }
      *at += size;
    }
    true
  };
  if !sets(usize::from(record[5] & 0x1F), &mut at) {
    return false;
  }
  let Some(&count) = record.get(at) else {
    return false;
  };
  at += 1;
  sets(usize::from(count), &mut at)
}

/// The framing FFmpeg's H.264 decoder reads a packet in where the record
/// declares four-byte lengths (`libavcodec/h264dec.c` 602–607): Annex B for
/// a packet that begins with a four-byte start code whose next four bytes
/// are a length past the packet; lengths again for one whose first four
/// bytes are a length that fits; else as the packet before it.
fn h264_packet_framing(packet: &[u8], current: Framing) -> Framing {
  let word = |at: usize| -> Option<u32> {
    let bytes = packet.get(at..at + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
  };
  let size = packet.len();
  if size > 8 && word(0) == Some(1) && word(5).is_some_and(|next| next as usize > size) {
    Framing::AnnexB
  } else if size > 3 && word(0).is_some_and(|first| first > 1 && first as usize <= size) {
    Framing::Length(LengthSize::Four)
  } else {
    current
  }
}

/// One H.264 NAL unit, read.
fn h264_unit(nal: &[u8]) -> Read {
  let Some(&header) = nal.first() else {
    return Read::Keeps(KeptReason::Malformed);
  };
  if header & 0x80 != 0 {
    return Read::Keeps(KeptReason::Malformed);
  }
  let nal_ref_idc = (header >> 5) & 3;
  match header & 0x1F {
    // An IDR slice.
    5 => Read::Keeps(KeptReason::RandomAccess),
    // A slice, or a slice data partition A, B or C.
    1..=4 if nal_ref_idc != 0 => Read::Keeps(KeptReason::Referenced),
    1..=4 => Read::Picture,
    // SEI, an access unit delimiter, filler data.
    6 | 9 | 12 => Read::Beside,
    // A prefix NAL unit, a coded slice extension (scalable or multiview),
    // a 3-D AVC slice extension.
    14 | 20 | 21 => Read::Keeps(KeptReason::Extension),
    // SPS, PPS, the end of a sequence or of the stream, an SPS extension, a
    // subset SPS, a depth parameter set, an auxiliary slice, and every
    // reserved or unspecified type.
    other => Read::Keeps(KeptReason::Companion(other)),
  }
}

/// What one NAL unit says about its packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
  /// A slice of a picture nothing references.
  Picture,
  /// A NAL unit that may ride beside such a picture.
  Beside,
  /// A NAL unit that keeps the packet.
  Keeps(KeptReason),
}

/// The NAL units of a packet, one at a time.
struct Units<'a> {
  rest: &'a [u8],
  framing: Framing,
  /// Annex B: whether the first start code has been read.
  started: bool,
  /// Annex B: whether the last bytes read were a start code, which a NAL
  /// unit must follow.
  after_start_code: bool,
}

/// What [`Units::next`] yields.
enum Unit<'a> {
  /// A NAL unit, its header first.
  Nal(&'a [u8]),
  /// The packet's end.
  End,
  /// Bytes the framing does not account for.
  Broken,
}

impl<'a> Units<'a> {
  const fn new(packet: &'a [u8], framing: Framing) -> Self {
    Self {
      rest: packet,
      framing,
      started: false,
      after_start_code: false,
    }
  }

  fn next(&mut self) -> Unit<'a> {
    match self.framing {
      Framing::Length(size) => self.next_after_length(size.bytes()),
      Framing::AnnexB => self.next_after_start_code(),
    }
  }

  /// The next NAL unit after its length: a length of 0, one past the
  /// packet, or bytes too few for a length are broken.
  fn next_after_length(&mut self, size: usize) -> Unit<'a> {
    if self.rest.is_empty() {
      return Unit::End;
    }
    let Some((prefix, rest)) = self.rest.split_at_checked(size) else {
      return Unit::Broken;
    };
    let length = prefix
      .iter()
      .fold(0_usize, |length, &byte| (length << 8) | usize::from(byte));
    if length == 0 {
      return Unit::Broken;
    }
    let Some((nal, rest)) = rest.split_at_checked(length) else {
      return Unit::Broken;
    };
    self.rest = rest;
    Unit::Nal(nal)
  }

  /// The next NAL unit after its start code, to the next start code or the
  /// packet's end, its trailing zero bytes dropped. Zero bytes may lead the
  /// first start code; any other byte before it is broken, and so is a
  /// packet with no start code, and a start code no NAL unit follows.
  fn next_after_start_code(&mut self) -> Unit<'a> {
    if !self.started {
      let lead = self.rest.iter().take_while(|&&byte| byte == 0).count();
      if lead < 2 || self.rest.get(lead) != Some(&1) {
        return Unit::Broken;
      }
      self.rest = &self.rest[lead + 1..];
      self.started = true;
    } else if self.rest.is_empty() {
      return if self.after_start_code {
        Unit::Broken
      } else {
        Unit::End
      };
    }
    // The unit runs to the next `00 00 01`; the zeros before it are the
    // next start code's, or trailing zero bytes.
    let (unit, rest) = match find_start_code(self.rest) {
      Some(at) => {
        self.after_start_code = true;
        (&self.rest[..at], &self.rest[at + 3..])
      }
      None => {
        self.after_start_code = false;
        (self.rest, &self.rest[self.rest.len()..])
      }
    };
    self.rest = rest;
    let trailing = unit.iter().rev().take_while(|&&byte| byte == 0).count();
    let nal = &unit[..unit.len() - trailing];
    if nal.is_empty() {
      return Unit::Broken;
    }
    Unit::Nal(nal)
  }
}

/// Where the next `00 00 01` begins in `bytes`.
fn find_start_code(bytes: &[u8]) -> Option<usize> {
  bytes.windows(3).position(|window| window == [0, 0, 1])
}

// The laws build packets in a `Vec`, so they need an allocator.
#[cfg(all(test, any(feature = "std", feature = "alloc")))]
mod tests;

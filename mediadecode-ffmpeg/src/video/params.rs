//! **How FFmpeg 9 reads an H.264 or HEVC parameter set**, mirrored where the
//! session must agree with the decoder it feeds about what that decoder
//! adopted (libavcodec 63.1.101, FFmpeg 9.0.1).
//!
//! FFmpeg's parameter-set parsers read through its bit reader
//! (`GetBitContext`, get_bits.h) and its exp-Golomb readers (golomb.h),
//! over NAL units its splitter cut out of a buffer
//! (`ff_h2645_packet_split`, h2645_parse.c). Each of the three has
//! behaviour a cleaner reader would not: the safe reader goes on reading
//! past a unit's last bit into whatever memory follows it, its index held
//! eight bits past the end; the exp-Golomb readers answer a long code with
//! an error value the parsers then compare as a number; the splitter hands
//! a unit without emulation prevention bytes over in place, so the memory a
//! reader runs on into is the rest of the buffer. A parser's verdict on a
//! malformed set depends on all of it, so this module reproduces each, and
//! the parsers above them read exactly as FFmpeg's do.
//!
//! The build this mirrors is a 64-bit one, as every target this crate's CI
//! builds is: `fast_64bit` (configure: `fast_64bit_if_any`, aarch64 and
//! x86_64 among them) reads a long field in one 64-bit load, and
//! `safe_bitstream_reader` is enabled by default (configure:
//! `enable safe_bitstream_reader`); none of the parsers mirrored here opts
//! out of it (`UNCHECKED_BITSTREAM_READER` is defined by `h264dec.c`,
//! `h264_cavlc.c`, `h264_cabac.c` and `h264_parser.c` alone).

#[cfg(test)]
mod tests;

/// `AVERROR_INVALIDDATA`, the value FFmpeg's exp-Golomb readers answer in
/// place of a code they cannot read (golomb.h), which a parser then
/// compares, or adds to, as the number it is.
const INVALIDDATA: i32 = -1_094_995_529;

/// The bytes FFmpeg leaves behind every buffer it hands a parser:
/// `AV_INPUT_BUFFER_PADDING_SIZE`, zeroed — by `av_packet_new_side_data`
/// and `av_new_packet` (packet.c), by the splitter behind each unit it
/// copies (h2645_parse.c `ff_h2645_extract_rbsp`).
const PADDING: usize = 64;

/// The three tables of golomb.c, as `get_ue_golomb`, `get_ue_golomb_31`
/// and `get_se_golomb` look up the next nine bits in them: the code's
/// length, its unsigned value and its signed value. A code of at most nine
/// bits — four leading zeros or fewer — is read whole; a longer one is
/// given a length of twice its leading zeros plus one and a value of 32
/// (`ue`) or 17 (`se`), save the prefix `000001000`, given 31 and 16.
struct Golomb {
  len: [u8; 512],
  ue: [u8; 512],
  se: [i8; 512],
}

const GOLOMB: Golomb = {
  let mut len = [0u8; 512];
  let mut ue = [0u8; 512];
  let mut se = [0i8; 512];
  let mut prefix = 0usize;
  while prefix < 512 {
    let zeros = if prefix == 0 {
      9
    } else {
      (prefix as u32).leading_zeros() - (32 - 9)
    };
    len[prefix] = (2 * zeros + 1) as u8;
    if zeros <= 4 {
      let value = (prefix >> (9 - (2 * zeros + 1))) - 1;
      ue[prefix] = value as u8;
      se[prefix] = if value % 2 == 1 {
        value.div_ceil(2) as i8
      } else {
        -((value / 2) as i8)
      };
    } else if prefix == 8 {
      ue[prefix] = 31;
      se[prefix] = 16;
    } else {
      ue[prefix] = 32;
      se[prefix] = 17;
    }
    prefix += 1;
  }
  Golomb { len, ue, se }
};

/// `av_log2`: the index of the highest set bit, 0 for 0 (`ff_log2`).
const fn log2(value: u32) -> u32 {
  31 - (value | 1).leading_zeros()
}

/// **FFmpeg's bit reader** (`GetBitContext`, get_bits.h), safe and on a
/// 64-bit build: `size` bits from `mem`'s first byte, read most significant
/// first, an index FFmpeg keeps in bits.
///
/// - **Past the end it reads on.** A read takes the bits at the index from
///   memory, whatever they are, and advances the index at most to eight
///   bits past `size` (`SKIP_COUNTER`'s clamp); from there every read reads
///   the same bits again. `mem` is the memory FFmpeg's reader sees from the
///   buffer's start — the rest of the buffer a unit was cut from, when the
///   splitter handed it over in place — and past `mem`, zero bytes: the
///   padding FFmpeg zeroes behind every buffer it hands a parser.
/// - **A field of up to 25 bits** (`get_bits`) is read from a 32-bit word,
///   one of 26 to 32 (`get_bits_long`) from a 64-bit one: either way the
///   bits as they stand.
/// - **The exp-Golomb readers** read the 32-bit word `UPDATE_CACHE` fills —
///   shifted left by the index's bit, zeros, not the stream, shifted in
///   behind it — and look its first nine bits up in golomb.c's tables, or
///   count its leading zeros; a code they cannot read is answered with
///   `AVERROR_INVALIDDATA`, as there.
#[derive(Clone)]
pub(super) struct Reader<'a> {
  mem: &'a [u8],
  index: u64,
  size: u64,
}

impl<'a> Reader<'a> {
  /// A reader at the first bit of `mem`, over `size` bits (`init_get_bits`).
  pub(super) const fn new(mem: &'a [u8], size: u64) -> Self {
    Self {
      mem,
      index: 0,
      size,
    }
  }

  fn byte(&self, at: u64) -> u8 {
    usize::try_from(at)
      .ok()
      .and_then(|at| self.mem.get(at))
      .copied()
      .unwrap_or(0)
  }

  fn word(&self, at: u64, bytes: u64) -> u64 {
    (0..bytes).fold(0, |word, offset| {
      (word << 8) | u64::from(self.byte(at + offset))
    })
  }

  /// The index advanced by `n`, held at eight bits past the end.
  fn advance(&mut self, n: u64) {
    self.index = self.index.saturating_add(n).min(self.size + 8);
  }

  /// The next `n` bits (1 to 32) as they stand: the 64-bit load
  /// `get_bits_long` makes, which agrees with `get_bits`'s 32-bit one for
  /// every field it reads.
  fn show(&self, n: u32) -> u32 {
    let word = self.word(self.index >> 3, 8) << (self.index & 7);
    ((word >> 32) as u32) >> (32 - n)
  }

  /// The 32-bit word `UPDATE_CACHE` fills: the four bytes from the index's,
  /// shifted left by its bit.
  fn cache(&self) -> u32 {
    (self.word(self.index >> 3, 4) as u32) << (self.index & 7)
  }

  /// `get_bits(n)` and `get_bits_long(n)`, `n` from 1 to 32.
  pub(super) fn bits(&mut self, n: u32) -> u32 {
    let value = self.show(n);
    self.advance(n.into());
    value
  }

  /// `get_bits1`: the bit at the index, the index advanced while it is
  /// short of eight bits past the end.
  pub(super) fn bit(&mut self) -> bool {
    let bit = (self.byte(self.index >> 3) << (self.index & 7)) >> 7 != 0;
    if self.index < self.size + 8 {
      self.index += 1;
    }
    bit
  }

  /// `show_bits1`: the bit at the index, the index kept.
  pub(super) fn show_bit(&self) -> bool {
    self.show(1) != 0
  }

  /// `skip_bits` and `skip_bits_long`.
  pub(super) fn skip(&mut self, n: u64) {
    self.advance(n);
  }

  /// `get_bits_left`: negative once the index is past the end.
  pub(super) fn left(&self) -> i64 {
    self.size as i64 - self.index as i64
  }

  /// `get_bits_count`: the index.
  pub(super) const fn count(&self) -> u64 {
    self.index
  }

  /// `get_ue_golomb`: a code of up to 12 leading zeros, read from the
  /// cache; `AVERROR_INVALIDDATA` for a longer one, past which the index
  /// still moves.
  pub(super) fn ue(&mut self) -> i32 {
    let cache = self.cache();
    if cache >= 1 << 27 {
      let prefix = (cache >> 23) as usize;
      self.advance(u64::from(GOLOMB.len[prefix]));
      i32::from(GOLOMB.ue[prefix])
    } else {
      let log = 2 * log2(cache) as i32 - 31;
      self.advance((32 - log) as u64);
      if log < 7 {
        INVALIDDATA
      } else {
        ((cache >> log) - 1) as i32
      }
    }
  }

  /// `get_ue_golomb_31`: the table's answer for the next nine bits — 31 or
  /// 32 for a code too long to read.
  pub(super) fn ue_31(&mut self) -> i32 {
    let prefix = (self.cache() >> 23) as usize;
    self.advance(u64::from(GOLOMB.len[prefix]));
    i32::from(GOLOMB.ue[prefix])
  }

  /// `get_ue_golomb_long`: up to 31 leading zeros, read from the stream.
  pub(super) fn ue_long(&mut self) -> u32 {
    let log = 31 - log2(self.show(32));
    self.advance(log.into());
    self.bits(log + 1).wrapping_sub(1)
  }

  /// `get_se_golomb`: the signed code of up to nine bits from the table;
  /// a longer one read in two cache loads, as there.
  pub(super) fn se(&mut self) -> i32 {
    let cache = self.cache();
    if cache >= 1 << 27 {
      let prefix = (cache >> 23) as usize;
      self.advance(u64::from(GOLOMB.len[prefix]));
      i32::from(GOLOMB.se[prefix])
    } else {
      let log = log2(cache);
      self.advance(u64::from(31 - log));
      let value = self.cache() >> log;
      self.advance(u64::from(32 - log));
      let sign = 0u32.wrapping_sub(value & 1);
      ((value >> 1) ^ sign).wrapping_sub(sign) as i32
    }
  }

  /// `get_se_golomb_long`.
  pub(super) fn se_long(&mut self) -> i32 {
    let value = self.ue_long();
    let sign = ((value & 1) as i32 - 1) as u32;
    ((value >> 1) ^ sign).wrapping_add(1) as i32
  }
}

/// The codec whose NAL unit headers a split reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Codec {
  H264,
  Hevc,
}

/// Where a unit's bytes are, as the splitter hands them over.
#[derive(Clone, Copy, Debug)]
enum Region {
  /// In place, at this offset into the buffer: the unit had no emulation
  /// prevention byte, and the reader runs on into the buffer behind it.
  Source(usize),
  /// Copied without its emulation prevention bytes, at this offset into
  /// the splitter's own buffer, zeros behind it.
  Rbsp(usize),
}

/// One NAL unit as FFmpeg's splitter hands it on (`H2645NAL`).
#[derive(Clone, Copy, Debug)]
pub(super) struct Nal {
  /// `nal_unit_type`.
  pub(super) kind: u8,
  /// HEVC's `nuh_layer_id`; 0 for H.264.
  pub(super) layer: u8,
  data: Region,
  /// `nal->size_bits`: its payload bits, to the stop bit (`get_bit_length`).
  pub(super) size_bits: u64,
  /// `nal->raw_data`, as an offset into the buffer, and `nal->raw_size`.
  raw_at: usize,
  raw_size: usize,
  /// The bits its header took: the reader's index once it is read.
  header_bits: u64,
}

/// **A buffer as `ff_h2645_packet_split` cuts it** (h2645_parse.c): its NAL
/// units, and the memory each one's reader sees.
pub(super) struct Split<'m> {
  mem: &'m [u8],
  rbsp: Vec<u8>,
  pub(super) nals: Vec<Nal>,
}

impl Split<'_> {
  fn data(&self, nal: &Nal) -> &[u8] {
    match nal.data {
      Region::Source(at) => self.mem.get(at..).unwrap_or_default(),
      Region::Rbsp(at) => self.rbsp.get(at..).unwrap_or_default(),
    }
  }

  /// `nal->gb` as the splitter leaves it: over the unit's payload bits, its
  /// index past the header.
  pub(super) fn reader(&self, nal: &Nal) -> Reader<'_> {
    Reader {
      mem: self.data(nal),
      index: nal.header_bits,
      size: nal.size_bits,
    }
  }

  /// A reader over the unit's raw bytes after its first —
  /// `init_get_bits8(nal->raw_data + 1, nal->raw_size - 1)`, the second of
  /// the three readings `decode_extradata_ps` gives a sequence parameter set
  /// (h264_parse.c).
  pub(super) fn raw_reader(&self, nal: &Nal) -> Reader<'_> {
    Reader::new(
      self.mem.get(nal.raw_at + 1..).unwrap_or_default(),
      (nal.raw_size.saturating_sub(1) * 8) as u64,
    )
  }
}

/// **`ff_h2645_packet_split`** over the first `length` bytes of `mem` —
/// `mem` holding what follows them in memory too, which a unit handed over
/// in place shows its reader. NAL units are length-prefixed by
/// `nal_length_size` bytes where `nalff` (`H2645_FLAG_IS_NALFF`), or
/// start-coded; `small_padding` (`H2645_FLAG_SMALL_PADDING`) lets a unit
/// with no emulation prevention byte stay in place. `None` where FFmpeg's
/// split fails: a length field past the end, or no start code at all.
///
/// A unit whose header does not parse — a forbidden bit set, an HEVC
/// temporal id of -1 — or of HEVC layer 63 is dropped, as there, and so is
/// one with no payload bits.
pub(super) fn split(
  mem: &[u8],
  length: usize,
  nal_length_size: usize,
  codec: Codec,
  nalff: bool,
  small_padding: bool,
) -> Option<Split<'_>> {
  let length = length.min(mem.len());
  let mut out = Split {
    mem,
    rbsp: vec![0; length + PADDING],
    nals: Vec::new(),
  };
  let mut rbsp_size = 0usize;
  let mut next_avc = if nalff { 0 } else { length };
  let mut at = 0usize;
  while length - at >= 4 {
    let extract_length;
    let mut skip_trailing_zeros = true;
    if at == next_avc {
      // `get_nalsize`: the field must leave a byte, and the unit fit.
      let left = length - at;
      if left <= nal_length_size {
        return None;
      }
      let size = mem[at..at + nal_length_size]
        .iter()
        .fold(0u64, |size, &byte| (size << 8) | u64::from(byte));
      if size == 0 || size > (left - nal_length_size) as u64 {
        return None;
      }
      extract_length = size as usize;
      at += nal_length_size;
      next_avc = at + extract_length;
    } else {
      // `find_next_start_code`, bounded by the next length field.
      let bound = next_avc.saturating_sub(at);
      let skip = if bound <= 3 {
        bound
      } else {
        let mut offset = 0;
        while offset + 3 < bound {
          if mem[at + offset..at + offset + 3] == [0, 0, 1] {
            break;
          }
          offset += 1;
        }
        offset + 3
      };
      at += skip;
      if at >= length {
        return if out.nals.is_empty() { None } else { Some(out) };
      }
      extract_length = (length - at).min(next_avc - at);
      if at >= next_avc {
        continue;
      }
    }
    let (consumed, data, size, raw_size) = extract(
      &mem[at..],
      extract_length,
      small_padding,
      &mut out.rbsp,
      &mut rbsp_size,
      at,
    );
    let raw_at = at;
    at += consumed;
    // "see commit 3566042a0": a unit followed by `00 00 01 E0` keeps its
    // trailing zeros.
    if length - at >= 4 && mem[at..at + 4] == [0, 0, 1, 0xE0] {
      skip_trailing_zeros = false;
    }
    let bytes = match data {
      Region::Source(offset) => &mem[offset..offset + size],
      Region::Rbsp(offset) => &out.rbsp[offset..offset + size],
    };
    let min_size = 1 + usize::from(codec == Codec::Hevc);
    let Some(size_bits) = bit_length(bytes, min_size, skip_trailing_zeros) else {
      continue;
    };
    if size == 0 || size_bits == 0 {
      continue;
    }
    let mut nal = Nal {
      kind: 0,
      layer: 0,
      data,
      size_bits,
      raw_at,
      raw_size,
      header_bits: 0,
    };
    let mut header = out.reader(&nal);
    let parsed = match codec {
      Codec::H264 => {
        let forbidden = header.bit();
        header.bits(2); // nal_ref_idc
        nal.kind = header.bits(5) as u8;
        !forbidden
      }
      Codec::Hevc => {
        let forbidden = header.bit();
        nal.kind = header.bits(6) as u8;
        nal.layer = header.bits(6) as u8;
        let temporal_id_plus1 = header.bits(3);
        !forbidden && temporal_id_plus1 != 0
      }
    };
    nal.header_bits = header.count();
    if codec == Codec::Hevc && nal.layer == 63 {
      continue;
    }
    if parsed {
      out.nals.push(nal);
    }
  }
  Some(out)
}

/// **`ff_h2645_extract_rbsp`** for the unit at the start of `src`, at most
/// `length` bytes long: cut at the first start code (`00 00 01`), its
/// emulation prevention bytes (`00 00 03`'s `03`) removed. A unit with
/// neither stays in place under `small_padding`; any other is copied to the
/// splitter's buffer at `rbsp_size`, and 64 zero bytes written behind it —
/// where a later unit's copy may land, at the raw bytes the earlier one took.
/// Answers the raw bytes consumed, where the unit is, its size and its raw
/// size. `at` is `src`'s offset into the split buffer.
fn extract(
  src: &[u8],
  length: usize,
  small_padding: bool,
  rbsp: &mut [u8],
  rbsp_size: &mut usize,
  at: usize,
) -> (usize, Region, usize, usize) {
  let byte = |index: usize| src.get(index).copied().unwrap_or(0);
  let mut length = length;
  // The scan for the first `00 00 01` or `00 00 03` wholly inside.
  let mut found = length;
  let mut index = 0;
  while index + 2 < length {
    if byte(index) == 0 && byte(index + 1) == 0 && matches!(byte(index + 2), 1 | 3) {
      if byte(index + 2) == 1 {
        length = index;
      }
      found = index;
      break;
    }
    index += 1;
  }
  if found + 1 >= length && small_padding {
    return (length, Region::Source(at), length, length);
  }
  let start = *rbsp_size;
  let copied = found.min(length);
  rbsp[start..start + copied].copy_from_slice(&src[..copied]);
  let (mut si, mut di) = (copied, copied);
  let mut cut = false;
  while si + 2 < length {
    if byte(si + 2) > 3 {
      rbsp[start + di] = byte(si);
      rbsp[start + di + 1] = byte(si + 1);
      di += 2;
      si += 2;
    } else if byte(si) == 0 && byte(si + 1) == 0 && byte(si + 2) != 0 {
      if byte(si + 2) == 3 {
        rbsp[start + di] = 0;
        rbsp[start + di + 1] = 0;
        di += 2;
        si += 3;
        continue;
      }
      cut = true;
      break;
    }
    rbsp[start + di] = byte(si);
    di += 1;
    si += 1;
  }
  if !cut {
    while si < length {
      rbsp[start + di] = byte(si);
      di += 1;
      si += 1;
    }
  }
  let end = (start + di + PADDING).min(rbsp.len());
  rbsp[start + di..end].fill(0);
  *rbsp_size += si;
  (si, Region::Rbsp(start), di, si)
}

/// **`get_bit_length`** (h2645_parse.c): a unit's payload bits — its
/// trailing zero bytes stripped (unless kept), then its last byte's stop
/// bit and the zero bits after it; a unit no longer than `min_size` bytes,
/// its header alone, keeps them. `None` where FFmpeg answers an error, and
/// the unit is dropped.
fn bit_length(data: &[u8], min_size: usize, skip_trailing_zeros: bool) -> Option<u64> {
  let mut size = data.len();
  while skip_trailing_zeros && size > 0 && data[size - 1] == 0 {
    size -= 1;
  }
  if size == 0 {
    return Some(0);
  }
  let mut trailing = 0u64;
  if size <= min_size {
    if data.len() < min_size {
      return None;
    }
    size = min_size;
  } else {
    let last = data[size - 1];
    if last != 0 {
      trailing = u64::from(last.trailing_zeros()) + 1;
    }
  }
  Some(size as u64 * 8 - trailing)
}

/// What a sequence parameter set FFmpeg stores says to a picture parameter
/// set read after it (`ff_h264_decode_picture_parameter_set`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Sps {
  profile_idc: i32,
  constraint_set_flags: i32,
  chroma_format_idc: i32,
  bit_depth_luma: i32,
}

/// **`ff_h264_decode_seq_parameter_set`** (h264_ps.c:284-594) read from
/// `r`, past the unit's header: the set's id and what it says, where FFmpeg
/// stores it; `None` where it fails it. `ignore_truncation` lets a reading
/// that ran past the unit stand (h264_ps.c:535-541). The decoder's context
/// is the one this crate opens: no `AV_CODEC_FLAG2_IGNORE_CROP`.
fn h264_sps(r: &mut Reader<'_>, ignore_truncation: bool) -> Option<(usize, Sps)> {
  let profile_idc = r.bits(8) as i32;
  let mut constraint_set_flags = 0i32;
  for flag in 0..6 {
    constraint_set_flags |= i32::from(r.bit()) << flag;
  }
  r.skip(2); // reserved_zero_2bits
  r.bits(8); // level_idc
  let sps_id = r.ue_31() as u32;
  if sps_id >= 32 {
    return None;
  }
  let (chroma_format_idc, bit_depth_luma) = if matches!(
    profile_idc,
    100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 144
  ) {
    let chroma_format_idc = r.ue_31();
    if chroma_format_idc as u32 > 3 {
      return None;
    }
    // residual_color_transform_flag: separate colour planes, unsupported.
    if chroma_format_idc == 3 && r.bit() {
      return None;
    }
    let luma = r.ue_31() + 8;
    let chroma = r.ue_31() + 8;
    if chroma != luma || !(8..=14).contains(&luma) || !(8..=14).contains(&chroma) {
      return None;
    }
    r.bit(); // transform_bypass
    let present = r.bit();
    if !scaling_matrices(r, true, present, false, chroma_format_idc) {
      return None;
    }
    (chroma_format_idc, luma)
  } else {
    (1, 8)
  };
  // log2_max_frame_num_minus4, from 0 to 12.
  if !(0..=12).contains(&r.ue_31()) {
    return None;
  }
  match r.ue_31() {
    // pic_order_cnt_type
    0 => {
      // log2_max_pic_order_cnt_lsb_minus4, at most 12.
      if r.ue_31() as u32 > 12 {
        return None;
      }
    }
    1 => {
      r.bit(); // delta_pic_order_always_zero_flag
      if r.se_long() == i32::MIN || r.se_long() == i32::MIN {
        return None;
      }
      let cycle = r.ue();
      if cycle as u32 >= 256 {
        return None;
      }
      for _ in 0..cycle {
        if r.se_long() == i32::MIN {
          return None;
        }
      }
    }
    2 => {}
    _ => return None,
  }
  // max_num_ref_frames, at most H264_MAX_DPB_FRAMES.
  if r.ue_31() > 16 {
    return None;
  }
  r.bit(); // gaps_in_frame_num_allowed_flag
  let mb_width = r.ue().wrapping_add(1);
  let mut mb_height = r.ue().wrapping_add(1);
  let frame_mbs_only = r.bit();
  if mb_height as u32 >= (i32::MAX as u32) / 2 {
    return None;
  }
  mb_height *= 2 - i32::from(frame_mbs_only);
  if !frame_mbs_only {
    r.bit(); // mb_adaptive_frame_field_flag
  }
  let limit = (i32::MAX / 16) as u32;
  if mb_width as u32 >= limit
    || mb_height as u32 >= limit
    || !image_size_valid((16 * mb_width) as u32, (16 * mb_height) as u32)
  {
    return None;
  }
  r.bit(); // direct_8x8_inference_flag
  if r.bit() {
    // frame_cropping_flag: offsets in chroma units that must leave a
    // picture.
    let (left, right, top, bottom) = (r.ue() as u32, r.ue() as u32, r.ue() as u32, r.ue() as u32);
    let vsub = u32::from(chroma_format_idc == 1);
    let hsub = u32::from(chroma_format_idc == 1 || chroma_format_idc == 2);
    let step_x = 1u32 << hsub;
    let step_y = (2 - u32::from(frame_mbs_only)) << vsub;
    let bound = |step: u32| (i32::MAX as u32) / 4 / step;
    if left > bound(step_x)
      || right > bound(step_x)
      || top > bound(step_y)
      || bottom > bound(step_y)
      || left.wrapping_add(right).wrapping_mul(step_x) >= (16 * mb_width) as u32
      || top.wrapping_add(bottom).wrapping_mul(step_y) >= (16 * mb_height) as u32
    {
      return None;
    }
  }
  if r.bit() && !h264_vui(r) {
    // vui_parameters_present_flag
    return None;
  }
  if r.left() < 0 && !ignore_truncation {
    return None;
  }
  Some((
    sps_id as usize,
    Sps {
      profile_idc,
      constraint_set_flags,
      chroma_format_idc,
      bit_depth_luma,
    },
  ))
}

/// `av_image_check_size(w, h)`: `av_image_check_size2` with no pixel
/// format, whose stride is taken as eight bytes a pixel, plus 1024
/// (imgutils.c:289-316).
fn image_size_valid(width: u32, height: u32) -> bool {
  let stride = 8 * u64::from(width) + 128 * 8;
  let limit = i32::MAX as u64;
  !(width == 0
    || height == 0
    || width > i32::MAX as u32
    || height > i32::MAX as u32
    || stride >= limit
    || stride * (u64::from(height) + 128) >= limit)
}

/// The VUI of an H.264 sequence parameter set: `ff_h2645_decode_common_vui_params`
/// (h2645_vui.c:37-100), which reads past its fields and fails nothing,
/// then `decode_vui_parameters` (h264_ps.c:133-199). `false` where it fails
/// the set.
fn h264_vui(r: &mut Reader<'_>) -> bool {
  if r.bit() {
    // aspect_ratio_idc, and the SAR it extends to.
    if r.bits(8) == 255 {
      r.bits(16);
      r.bits(16);
    }
  }
  if r.bit() {
    r.bit(); // overscan_appropriate_flag
  }
  if r.bit() {
    // video_signal_type_present_flag
    r.bits(3);
    r.bit();
    if r.bit() {
      r.bits(8);
      r.bits(8);
      r.bits(8);
    }
  }
  if r.bit() {
    // chroma_loc_info_present_flag
    r.ue_31();
    r.ue_31();
  }
  // A VUI cut short is taken as it stands.
  if r.show_bit() && r.left() < 10 {
    return true;
  }
  if r.bit() {
    // timing_info_present_flag
    r.bits(32);
    r.bits(32);
    r.bit();
  }
  let nal = r.bit();
  if nal && !h264_hrd(r) {
    return false;
  }
  let vcl = r.bit();
  if vcl && !h264_hrd(r) {
    return false;
  }
  if nal || vcl {
    r.bit(); // low_delay_hrd_flag
  }
  r.bit(); // pic_struct_present_flag
  if r.left() == 0 {
    return true;
  }
  if r.bit() {
    // bitstream_restriction_flag
    r.bit();
    for _ in 0..4 {
      r.ue_31();
    }
    let mut num_reorder_frames = r.ue_31();
    r.ue_31(); // max_dec_frame_buffering
    if r.left() < 0 {
      num_reorder_frames = 0;
    }
    if num_reorder_frames as u32 > 16 {
      return false;
    }
  }
  true
}

/// `decode_hrd_parameters` (h264_ps.c:106-131): `false` where it fails.
fn h264_hrd(r: &mut Reader<'_>) -> bool {
  let cpb_count = r.ue_31() + 1;
  if cpb_count as u32 > 32 {
    return false;
  }
  r.bits(4);
  r.bits(4);
  for _ in 0..cpb_count {
    r.ue_long();
    r.ue_long();
    r.bit();
  }
  for _ in 0..4 {
    r.bits(5);
  }
  true
}

/// `decode_scaling_matrices` (h264_ps.c:231-268), as far as its verdict
/// goes: `false` where a list fails. Every list is read, as there, a failed
/// one's among them.
fn scaling_matrices(
  r: &mut Reader<'_>,
  sps: bool,
  present: bool,
  transform_8x8: bool,
  chroma_format_idc: i32,
) -> bool {
  if !present {
    return true;
  }
  let mut valid = true;
  for _ in 0..6 {
    valid &= scaling_list(r, 16);
  }
  if sps || transform_8x8 {
    let lists = if chroma_format_idc == 3 { 6 } else { 2 };
    for _ in 0..lists {
      valid &= scaling_list(r, 64);
    }
  }
  valid
}

/// `decode_scaling_list` (h264_ps.c:201-228): `false` where a delta falls
/// outside -128 to 127.
fn scaling_list(r: &mut Reader<'_>, size: usize) -> bool {
  if !r.bit() {
    return true;
  }
  let (mut last, mut next) = (8i32, 8i32);
  for index in 0..size {
    if next != 0 {
      let delta = r.se();
      if !(-128..=127).contains(&delta) {
        return false;
      }
      next = (last + delta) & 0xff;
    }
    if index == 0 && next == 0 {
      break;
    }
    if next != 0 {
      last = next;
    }
  }
  true
}

/// What FFmpeg does with an H.264 picture parameter set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pps {
  Stored,
  Failed,
  /// It refers to a sequence parameter set the reading has not met: FFmpeg's
  /// answer depends on what the decoder holds already.
  Unresolved,
}

/// **`ff_h264_decode_picture_parameter_set`** (h264_ps.c:698-847) read from
/// `r`, past the unit's header, over a unit of `bit_length` payload bits,
/// against the sequence parameter sets `sps` holds.
fn h264_pps(r: &mut Reader<'_>, bit_length: u64, sps: &[Option<Sps>; 32]) -> Pps {
  if r.ue() as u32 >= 256 {
    // pps_id
    return Pps::Failed;
  }
  let sps_id = r.ue_31() as u32;
  if sps_id >= 32 {
    return Pps::Failed;
  }
  let Some(sps) = sps[sps_id as usize] else {
    return Pps::Unresolved;
  };
  if sps.bit_depth_luma > 14 || matches!(sps.bit_depth_luma, 11 | 13) {
    return Pps::Failed;
  }
  r.bit(); // entropy_coding_mode_flag
  r.bit(); // bottom_field_pic_order_in_frame_present_flag
  if r.ue().wrapping_add(1) > 1 {
    // num_slice_groups_minus1: slice groups (FMO) are not implemented.
    r.ue();
    return Pps::Failed;
  }
  let l0 = r.ue().wrapping_add(1) as u32;
  let l1 = r.ue().wrapping_add(1) as u32;
  if l0.wrapping_sub(1) > 31 || l1.wrapping_sub(1) > 31 {
    return Pps::Failed;
  }
  r.bit(); // weighted_pred_flag
  r.bits(2); // weighted_bipred_idc
  r.se(); // pic_init_qp_minus26
  r.se(); // pic_init_qs_minus26
  if !(-12..=12).contains(&r.se()) {
    // chroma_qp_index_offset
    return Pps::Failed;
  }
  r.bit();
  r.bit();
  r.bit();
  let bits_left = bit_length as i64 - r.count() as i64;
  // `more_rbsp_data_in_pps`: none for Baseline, Main and Extended under
  // constraint_set0, 1 or 2.
  let more = !(matches!(sps.profile_idc, 66 | 77 | 88) && sps.constraint_set_flags & 7 != 0);
  if bits_left > 0 && more {
    let transform_8x8 = r.bit();
    let present = r.bit();
    if !scaling_matrices(r, false, present, transform_8x8, sps.chroma_format_idc) {
      return Pps::Failed;
    }
    if !(-12..=12).contains(&r.se()) {
      // second_chroma_qp_index_offset
      return Pps::Failed;
    }
  }
  Pps::Stored
}

/// A parameter set FFmpeg did not store, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unstored {
  Failed(crate::ParameterSet),
  Unresolved,
}

/// **`decode_extradata_ps`** (h264_parse.c:367-415) over the first `length`
/// bytes of `mem`, length-prefixed by two bytes where `nalff` (an `avcC`
/// entry) or start-coded: each sequence parameter set read the three ways
/// it reads one — the unit, its raw bytes, the unit with truncation let
/// stand — each picture parameter set once, every other unit passed over.
/// The first set it does not store ends the reading, as there; a split that
/// fails stores nothing and answers success, as there.
fn h264_sets(
  mem: &[u8],
  length: usize,
  nalff: bool,
  sps: &mut [Option<Sps>; 32],
) -> Result<(), Unstored> {
  let Some(split) = split(mem, length, 2, Codec::H264, nalff, true) else {
    return Ok(());
  };
  for nal in &split.nals {
    match nal.kind {
      7 => {
        let stored = h264_sps(&mut split.reader(nal), false)
          .or_else(|| h264_sps(&mut split.raw_reader(nal), false))
          .or_else(|| h264_sps(&mut split.reader(nal), true));
        let Some((id, set)) = stored else {
          return Err(Unstored::Failed(crate::ParameterSet::Sequence));
        };
        sps[id] = Some(set);
      }
      8 => match h264_pps(&mut split.reader(nal), nal.size_bits, sps) {
        Pps::Stored => {}
        Pps::Failed => return Err(Unstored::Failed(crate::ParameterSet::Picture)),
        Pps::Unresolved => return Err(Unstored::Unresolved),
      },
      _ => {}
    }
  }
  Ok(())
}

/// An `avcC` entry FFmpeg's escaping retry would take past a 16-bit length
/// (`decode_extradata_ps_mp4`, h264_parse.c:436-437): `nalsize / 2` at or
/// over `(INT16_MAX - AV_INPUT_BUFFER_PADDING_SIZE) / 3`.
const ESCAPE_LIMIT: usize = (i16::MAX as usize - PADDING) / 3;

/// **`decode_extradata_ps_mp4`** (h264_parse.c:421-464) for the `avcC`
/// entry at the start of `mem`, `nalsize` bytes with its length field: read
/// as it stands, and where a set in it is not stored — FFmpeg's decoders
/// never set `AV_EF_EXPLODE` here — read again with emulation prevention
/// bytes put in, for records whose sets were stored unescaped. Answers what
/// the retry did not store, or the record's rejection where the entry is
/// too large to retry.
fn h264_entry(
  mem: &[u8],
  nalsize: usize,
  sps: &mut [Option<Sps>; 32],
) -> Result<(), crate::ExtradataRejection> {
  let Err(first) = h264_sets(mem, nalsize, true, sps) else {
    return Ok(());
  };
  if nalsize / 2 >= ESCAPE_LIMIT {
    return Err(crate::ExtradataRejection::Oversized(match first {
      Unstored::Failed(set) => set,
      Unstored::Unresolved => crate::ParameterSet::Picture,
    }));
  }
  let escaped = escape(&mem[..nalsize]);
  h264_sets(&escaped, escaped.len(), true, sps).map_err(rejection)
}

/// The escaping `decode_extradata_ps_mp4` gives an entry: an `03` put in
/// before the third byte of each `00 00 0x` with `x` at most 3, then the
/// length field rewritten for the escaped size.
fn escape(entry: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(entry.len() * 3 / 2 + PADDING);
  let mut at = 0;
  while at < entry.len() {
    if entry.len() - at >= 3 && entry[at] == 0 && entry[at + 1] == 0 && entry[at + 2] <= 3 {
      out.extend_from_slice(&[0, 0, 3]);
      at += 2;
    } else {
      out.push(entry[at]);
      at += 1;
    }
  }
  let size = (out.len() - 2) as u16;
  out[..2].copy_from_slice(&size.to_be_bytes());
  out
}

fn rejection(unstored: Unstored) -> crate::ExtradataRejection {
  match unstored {
    Unstored::Failed(set) => crate::ExtradataRejection::Unparsed(set),
    Unstored::Unresolved => crate::ExtradataRejection::Unresolved,
  }
}

/// **FFmpeg's verdict on an H.264 extradata a packet carries** — what
/// `ff_h264_decode_extradata` (h264_parse.c:466-524) makes of it, whose
/// answer `h264_decode_frame` drops (h264dec.c:1038-1044): `Ok` where it
/// applies the record whole — the `avcC` framing and NAL length size, or
/// Annex B, and every parameter set it carries stored — and the reason
/// otherwise:
///
/// - **An `avcC` record** (its first byte 1) **shorter than seven bytes** is
///   rejected before anything is read (h264_parse.c:481-484): the decoder
///   keeps its NAL length size, its parameter sets.
/// - **An entry whose length runs past the record** rejects the record
///   there (h264_parse.c:491-492, 505-506): the sets before it stored, the
///   NAL length size not.
/// - **A set FFmpeg cannot parse** — read as it stands, as its raw bytes,
///   with truncation let stand, and for an `avcC` entry escaped too — is
///   skipped while the rest of the record applies (h264_parse.c:385-404,
///   426-463): the decoder keeps the set it had of that id. An entry too
///   large for the escaping retry rejects the record (h264_parse.c:436-437).
/// - **A picture parameter set referring to a sequence parameter set the
///   record does not carry** stands or falls on what the decoder holds
///   already, which this crate does not read.
///
/// Any of them leaves the decoder on parameters other than the record's.
pub(crate) fn h264_record(record: &[u8]) -> Result<(), crate::ExtradataRejection> {
  let mut sps = [None; 32];
  if record.first() != Some(&1) {
    return h264_sets(record, record.len(), false, &mut sps).map_err(rejection);
  }
  if record.len() < 7 {
    return Err(crate::ExtradataRejection::TooShort { size: record.len() });
  }
  let byte = |at: usize| record.get(at).copied().unwrap_or(0);
  let mut at = 6usize;
  let mut entries = |at: &mut usize,
                     count: usize,
                     set: crate::ParameterSet|
   -> Result<(), crate::ExtradataRejection> {
    for _ in 0..count {
      let nalsize = ((usize::from(byte(*at)) << 8) | usize::from(byte(*at + 1))) + 2;
      if nalsize > record.len().saturating_sub(*at) {
        return Err(crate::ExtradataRejection::Overrun(set));
      }
      h264_entry(&record[*at..], nalsize, &mut sps)?;
      *at += nalsize;
    }
    Ok(())
  };
  entries(
    &mut at,
    usize::from(record[5] & 0x1f),
    crate::ParameterSet::Sequence,
  )?;
  let pictures = usize::from(byte(at));
  at += 1;
  entries(&mut at, pictures, crate::ParameterSet::Picture)
}

/// Whether an `avcC` record's entries hold a sequence parameter set that
/// permits arbitrary slice order (`access::sps_permits_aso`), each entry
/// read as `ff_h264_decode_extradata` walks them, stopping where one runs
/// past the record: the profile and constraint bytes follow the unit's
/// header.
pub(super) fn avcc_units<'r>(record: &'r [u8]) -> impl Iterator<Item = &'r [u8]> + 'r {
  let byte = move |at: usize| record.get(at).copied().unwrap_or(0);
  let mut at = 6usize;
  let mut left = usize::from(byte(5) & 0x1f);
  let mut pictures_read = false;
  core::iter::from_fn(move || {
    if record.first() != Some(&1) || record.len() < 7 {
      return None;
    }
    while left == 0 {
      if pictures_read {
        return None;
      }
      pictures_read = true;
      left = usize::from(byte(at));
      at += 1;
    }
    left -= 1;
    let length = (usize::from(byte(at)) << 8) | usize::from(byte(at + 1));
    let unit = record.get(at + 2..at + 2 + length)?;
    at += 2 + length;
    Some(unit)
  })
}

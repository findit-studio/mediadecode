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

/// The codec whose NAL unit headers a walk reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Codec {
  H264,
  Hevc,
}

/// One NAL unit as FFmpeg's splitter cuts it out of a buffer (`H2645NAL`),
/// found without anything collected or copied.
#[derive(Clone, Copy, Debug)]
pub(super) struct Unit<'m> {
  /// `nal_unit_type`.
  pub(super) kind: u8,
  /// HEVC's `nuh_layer_id`; 0 for H.264.
  pub(super) layer: u8,
  /// The unit as it stands in the buffer, from its header to where the
  /// splitter cut it.
  raw: &'m [u8],
  /// Where its first `00 00 03`, or the `00 00 01` that cut it, stands in
  /// `raw` — what a copy begins removing emulation prevention from; `raw`'s
  /// length where there is neither.
  found: usize,
  /// Whether the splitter hands it over copied, its emulation prevention
  /// bytes removed, rather than in place (`ff_h2645_extract_rbsp`).
  copied: bool,
  /// `nal->raw_size`: the raw bytes the splitter took for it.
  consumed: usize,
  /// `nal->size`: its bytes as handed over.
  size: usize,
  /// `nal->size_bits`: its payload bits, to the stop bit (`get_bit_length`).
  pub(super) size_bits: u64,
  /// The bits its header took: a reader's index once it is read.
  header_bits: u64,
  /// Its offset in the buffer.
  at: usize,
}

/// A walk FFmpeg's splitter refuses: a length field past the buffer, or no
/// start code at all — the decoder then reads none of the buffer's units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Refused;

/// **`ff_h2645_packet_split`** (h2645_parse.c:527-665) over the first
/// `length` bytes of `mem`, one unit at a time and in constant memory —
/// `mem` holding what follows them too, which a unit handed over in place
/// shows its reader. Units are length-prefixed by `nal_length_size` bytes
/// where `nalff` (`H2645_FLAG_IS_NALFF`), or start-coded; `small_padding`
/// (`H2645_FLAG_SMALL_PADDING`) lets a unit with no emulation prevention byte
/// stay in place, where without it every unit is copied. A unit whose header
/// does not parse — a forbidden bit set, an HEVC temporal id of -1 — or of
/// HEVC layer 63, or with no payload bits, is passed over, as there.
///
/// FFmpeg cuts the whole buffer before its parser reads a unit, so a walk
/// that ends [`Refused`] gives the parser nothing: a reader of the units
/// walks to the end first ([`Self::accepted`]).
#[derive(Clone)]
pub(super) struct Walk<'m> {
  mem: &'m [u8],
  length: usize,
  nal_length_size: usize,
  codec: Codec,
  small_padding: bool,
  at: usize,
  next_avc: usize,
  /// Units handed out (`nb_nals`).
  units: usize,
  over: bool,
}

impl<'m> Walk<'m> {
  pub(super) fn new(
    mem: &'m [u8],
    length: usize,
    nal_length_size: usize,
    codec: Codec,
    nalff: bool,
    small_padding: bool,
  ) -> Self {
    let length = length.min(mem.len());
    Self {
      mem,
      length,
      nal_length_size,
      codec,
      small_padding,
      at: 0,
      next_avc: if nalff { 0 } else { length },
      units: 0,
      over: false,
    }
  }

  /// Whether FFmpeg's splitter cuts the whole buffer: its parser reads the
  /// units only then.
  pub(super) fn accepted(&self) -> bool {
    self.clone().all(|unit| unit.is_ok())
  }

  /// The memory a reader of `unit` — a unit this walk handed out, the walk
  /// standing just past it — sees from the unit's first byte: in place, the
  /// buffer from there on; copied, the copy, then zeros up to where the next
  /// copied unit lands — the raw bytes the unit took on — and that unit's
  /// copy, and so on as far as a reader reads past a unit's end (the safe
  /// reader stops eight bits past it and loads eight bytes there). Built in
  /// `scratch` where the unit is copied.
  pub(super) fn memory<'s>(&self, unit: &Unit<'m>, scratch: &'s mut Vec<u8>) -> &'s [u8]
  where
    'm: 's,
  {
    if !unit.copied {
      return self.mem.get(unit.at..).unwrap_or_default();
    }
    scratch.clear();
    copy_unit(unit.raw, unit.found, |byte| scratch.push(byte));
    let reach = unit.size + 16;
    scratch.resize(unit.consumed.max(scratch.len()), 0);
    let mut rest = self.clone();
    while scratch.len() < reach {
      match rest.next_cut() {
        Some(Ok(next)) if next.copied => {
          let start = scratch.len();
          copy_unit(next.raw, next.found, |byte| scratch.push(byte));
          scratch.resize(start + next.consumed, 0);
        }
        Some(Ok(_)) => {}
        Some(Err(Refused)) | None => break,
      }
    }
    scratch
  }

  /// The next unit the splitter cuts, its header unread: every unit
  /// FFmpeg extracts, the ones it then passes over among them.
  fn next_cut(&mut self) -> Option<Result<Unit<'m>, Refused>> {
    loop {
      if self.over || self.length - self.at < 4 {
        self.over = true;
        return None;
      }
      let mem = self.mem;
      let extract_length;
      if self.at == self.next_avc {
        // `get_nalsize`: the field must leave a byte, and the unit fit.
        let left = self.length - self.at;
        if left <= self.nal_length_size {
          self.over = true;
          return Some(Err(Refused));
        }
        let size = mem[self.at..self.at + self.nal_length_size]
          .iter()
          .fold(0u64, |size, &byte| (size << 8) | u64::from(byte));
        if size == 0 || size > (left - self.nal_length_size) as u64 {
          self.over = true;
          return Some(Err(Refused));
        }
        extract_length = size as usize;
        self.at += self.nal_length_size;
        self.next_avc = self.at + extract_length;
      } else {
        // `find_next_start_code`, bounded by the next length field.
        let bound = self.next_avc.saturating_sub(self.at);
        let skip = if bound <= 3 {
          bound
        } else {
          let mut offset = 0;
          while offset + 3 < bound {
            if mem[self.at + offset..self.at + offset + 3] == [0, 0, 1] {
              break;
            }
            offset += 1;
          }
          offset + 3
        };
        self.at += skip;
        if self.at >= self.length {
          self.over = true;
          return (self.units == 0).then_some(Err(Refused));
        }
        extract_length = (self.length - self.at).min(self.next_avc - self.at);
        if self.at >= self.next_avc {
          continue;
        }
      }
      let unit = cut(&mem[self.at..], extract_length, self.small_padding, self.at);
      self.at += unit.consumed;
      return Some(Ok(unit));
    }
  }
}

impl<'m> Iterator for Walk<'m> {
  type Item = Result<Unit<'m>, Refused>;

  fn next(&mut self) -> Option<Self::Item> {
    loop {
      let mut unit = match self.next_cut()? {
        Ok(unit) => unit,
        Err(refused) => return Some(Err(refused)),
      };
      // "see commit 3566042a0": a unit followed by `00 00 01 E0` keeps its
      // trailing zeros.
      let skip_trailing_zeros =
        !(self.length - self.at >= 4 && self.mem[self.at..self.at + 4] == [0, 0, 1, 0xE0]);
      let min_size = 1 + usize::from(self.codec == Codec::Hevc);
      let mut stats = Stats::default();
      if unit.copied {
        copy_unit(unit.raw, unit.found, |byte| stats.push(byte));
      } else {
        unit.raw.iter().for_each(|&byte| stats.push(byte));
      }
      unit.size = stats.size;
      let Some(size_bits) = stats.bit_length(min_size, skip_trailing_zeros) else {
        continue;
      };
      if unit.size == 0 || size_bits == 0 {
        continue;
      }
      unit.size_bits = size_bits;
      let [head, second] = stats.head;
      let parsed = match self.codec {
        Codec::H264 => {
          unit.kind = head & 0x1f;
          unit.header_bits = 8;
          head & 0x80 == 0
        }
        Codec::Hevc => {
          unit.kind = (head >> 1) & 0x3f;
          unit.layer = ((head & 1) << 5) | (second >> 3);
          unit.header_bits = 16;
          head & 0x80 == 0 && second & 0x07 != 0
        }
      };
      if self.codec == Codec::Hevc && unit.layer == 63 {
        continue;
      }
      if parsed {
        self.units += 1;
        return Some(Ok(unit));
      }
    }
  }
}

impl Unit<'_> {
  /// `nal->gb` as the splitter leaves it, over `memory` ([`Walk::memory`]):
  /// the unit's payload bits, the index past its header.
  pub(super) fn reader<'s>(&self, memory: &'s [u8]) -> Reader<'s> {
    Reader {
      mem: memory,
      index: self.header_bits,
      size: self.size_bits,
    }
  }

  /// A reader over the unit's raw bytes after its first —
  /// `init_get_bits8(nal->raw_data + 1, nal->raw_size - 1)`, the second of
  /// the three readings `decode_extradata_ps` gives a sequence parameter set
  /// (h264_parse.c:390) — the buffer, `mem`, read on past them.
  pub(super) fn raw_reader<'s>(&self, mem: &'s [u8]) -> Reader<'s> {
    Reader::new(
      mem.get(self.at + 1..).unwrap_or_default(),
      (self.consumed.saturating_sub(1) * 8) as u64,
    )
  }

  /// The first `N` bytes the unit is handed over as — its header and what
  /// follows — zeros past its end.
  pub(super) fn head<const N: usize>(&self) -> [u8; N] {
    let mut head = [0u8; N];
    let mut at = 0;
    let mut put = |byte| {
      if at < N {
        head[at] = byte;
        at += 1;
      }
    };
    if self.copied {
      copy_unit(self.raw, self.found, put);
    } else {
      self.raw.iter().take(N).for_each(|&byte| put(byte));
    }
    head
  }
}

/// What a unit's bytes say of its size and payload bits, gathered as they
/// are handed over.
#[derive(Default)]
struct Stats {
  size: usize,
  head: [u8; 2],
  /// The last byte that is not zero, and where: `get_bit_length`'s stop bit.
  last_nonzero: Option<(usize, u8)>,
  last: u8,
}

impl Stats {
  fn push(&mut self, byte: u8) {
    if self.size < 2 {
      self.head[self.size] = byte;
    }
    if byte != 0 {
      self.last_nonzero = Some((self.size, byte));
    }
    self.last = byte;
    self.size += 1;
  }

  /// **`get_bit_length`** (h2645_parse.c:348-376): the payload bits — the
  /// trailing zero bytes stripped (unless kept), then the last byte's stop
  /// bit and the zero bits after it; a unit no longer than `min_size` bytes,
  /// its header alone, keeps them. `None` where FFmpeg answers an error,
  /// and the unit is passed over.
  fn bit_length(&self, min_size: usize, skip_trailing_zeros: bool) -> Option<u64> {
    let stripped = if skip_trailing_zeros {
      self.last_nonzero.map_or(0, |(at, _)| at + 1)
    } else {
      self.size
    };
    if stripped == 0 {
      return Some(0);
    }
    if stripped <= min_size {
      if self.size < min_size {
        return None;
      }
      return Some(min_size as u64 * 8);
    }
    let last = if skip_trailing_zeros {
      self.last_nonzero.map_or(0, |(_, byte)| byte)
    } else {
      self.last
    };
    let trailing = if last != 0 {
      u64::from(last.trailing_zeros()) + 1
    } else {
      0
    };
    Some(stripped as u64 * 8 - trailing)
  }
}

/// **`ff_h2645_extract_rbsp`** (h2645_parse.c:37-150) for the unit at the
/// start of `src`, at most `length` bytes long, its bytes not yet read: cut
/// at the first `00 00 01` wholly inside, and copied where a `00 00 03`
/// comes first — or wherever `small_padding` is not set — else handed over
/// in place. `at` is `src`'s offset into the buffer.
fn cut(src: &[u8], length: usize, small_padding: bool, at: usize) -> Unit<'_> {
  let byte = |index: usize| src.get(index).copied().unwrap_or(0);
  let mut length = length;
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
  let raw = &src[..length.min(src.len())];
  let copied = !(found + 1 >= length && small_padding);
  let consumed = if copied {
    copy_unit(raw, found, |_| {})
  } else {
    length
  };
  Unit {
    kind: 0,
    layer: 0,
    raw,
    found,
    copied,
    consumed,
    size: 0,
    size_bits: 0,
    header_bits: 0,
    at,
  }
}

/// The bytes FFmpeg's splitter writes for a unit it copies, handed to
/// `sink` one at a time: `raw[..found]` as it stands, then each `00 00 03`'s
/// `03` dropped, the copy ending at a `00 00 01` or `00 00 02`
/// (h2645_parse.c:99-138). Answers the raw bytes it took (`si`).
fn copy_unit(raw: &[u8], found: usize, mut sink: impl FnMut(u8)) -> usize {
  let length = raw.len();
  let byte = |index: usize| raw.get(index).copied().unwrap_or(0);
  let start = found.min(length);
  raw[..start].iter().for_each(|&byte| sink(byte));
  let mut si = start;
  while si + 2 < length {
    if byte(si + 2) > 3 {
      sink(byte(si));
      sink(byte(si + 1));
      si += 2;
    } else if byte(si) == 0 && byte(si + 1) == 0 && byte(si + 2) != 0 {
      if byte(si + 2) == 3 {
        sink(0);
        sink(0);
        si += 3;
        continue;
      }
      return si;
    }
    sink(byte(si));
    si += 1;
  }
  while si < length {
    sink(byte(si));
    si += 1;
  }
  si
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
  let mut units = Walk::new(mem, length, 2, Codec::H264, nalff, true);
  if !units.accepted() {
    return Ok(());
  }
  let mut scratch = Vec::new();
  while let Some(Ok(unit)) = units.next() {
    match unit.kind {
      7 => {
        let memory = units.memory(&unit, &mut scratch);
        let stored = h264_sps(&mut unit.reader(memory), false)
          .or_else(|| h264_sps(&mut unit.raw_reader(mem), false))
          .or_else(|| h264_sps(&mut unit.reader(memory), true));
        let Some((id, set)) = stored else {
          return Err(Unstored::Failed(crate::ParameterSet::Sequence));
        };
        sps[id] = Some(set);
      }
      8 => {
        let memory = units.memory(&unit, &mut scratch);
        match h264_pps(&mut unit.reader(memory), unit.size_bits, sps) {
          Pps::Stored => {}
          Pps::Failed => return Err(Unstored::Failed(crate::ParameterSet::Picture)),
          Pps::Unresolved => return Err(Unstored::Unresolved),
        }
      }
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

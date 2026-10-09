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

  /// `get_bits64(n)`, `n` from 1 to 64: past 32, the high bits and then 32
  /// more, each read clamped on its own.
  pub(super) fn bits64(&mut self, n: u32) -> u64 {
    if n <= 32 {
      u64::from(self.bits(n))
    } else {
      let high = u64::from(self.bits(n - 32));
      (high << 32) | u64::from(self.bits(32))
    }
  }

  /// `skip_bits` and `skip_bits_long`.
  pub(super) fn skip(&mut self, n: u64) {
    self.advance(n);
  }

  /// `align_get_bits`: to the next byte boundary.
  pub(super) fn align(&mut self) {
    let n = self.index.wrapping_neg() & 7;
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

impl<'m> Unit<'m> {
  /// The unit's raw bytes as the splitter took them — `nal->raw_data` for
  /// `nal->raw_size` bytes, its header first, emulation prevention bytes in
  /// place.
  pub(super) fn raw(&self) -> &'m [u8] {
    let raw: &'m [u8] = self.raw;
    &raw[..self.consumed.min(raw.len())]
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
  /// Stored under `id`, read against the sequence parameter set held under
  /// `sps` (`pps->sps`, h264_ps.c:738); `past_end` where its reading ran past
  /// its payload — FFmpeg's reader reads on into what follows, and nothing
  /// fails the set for it.
  Stored {
    id: usize,
    sps: usize,
    past_end: bool,
  },
  Failed,
  /// It refers to a sequence parameter set the decoder does not hold.
  Unresolved,
}

/// **`ff_h264_decode_picture_parameter_set`** (h264_ps.c:698-847) read from
/// `r`, past the unit's header, over a unit of `bit_length` payload bits,
/// against the sequence parameter sets `sets` holds.
fn h264_pps(r: &mut Reader<'_>, bit_length: u64, sets: &impl H264Sets) -> Pps {
  let id = r.ue() as u32;
  if id >= 256 {
    // pps_id
    return Pps::Failed;
  }
  let sps_id = r.ue_31() as u32;
  if sps_id >= 32 {
    return Pps::Failed;
  }
  let Some(sps) = sets.sps(sps_id as usize) else {
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
  Pps::Stored {
    id: id as usize,
    sps: sps_id as usize,
    past_end: r.left() < 0,
  }
}

/// **Where a reading of H.264 parameter sets puts what FFmpeg stores**: the
/// sequence parameter sets a picture parameter set is read against
/// (`ps->sps_list`, h264_ps.c:731-738), and every set FFmpeg stores, with
/// the unit it read it off. The bare table of facts keeps the facts alone,
/// for a verdict; the sets a decoder holds keep the units too
/// (`super::held`).
pub(super) trait H264Sets {
  /// What the sequence parameter set held under `id` says to a picture
  /// parameter set read now; `None` where none is held.
  fn sps(&self, id: usize) -> Option<Sps>;
  /// FFmpeg stores `sps` under `id`, read off `unit` the `reading`-th of the
  /// three ways it reads one: 1, the unit; 2, its raw bytes after its
  /// header; 3, the unit, truncation let stand (h264_parse.c:383-397,
  /// h264dec.c:699-715).
  fn store_sps(&mut self, id: usize, sps: Sps, unit: &Unit<'_>, reading: u8);
  /// FFmpeg stores the picture parameter set `unit` under `id`, read against
  /// the sequence parameter set held under `sps`; `past_end` where its
  /// reading ran past its payload.
  fn store_pps(&mut self, id: usize, sps: usize, unit: &Unit<'_>, past_end: bool);
}

impl H264Sets for [Option<Sps>; 32] {
  fn sps(&self, id: usize) -> Option<Sps> {
    self.get(id).copied().flatten()
  }

  fn store_sps(&mut self, id: usize, sps: Sps, _: &Unit<'_>, _: u8) {
    self[id] = Some(sps);
  }

  fn store_pps(&mut self, _: usize, _: usize, _: &Unit<'_>, _: bool) {}
}

/// A parameter set FFmpeg did not store, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unstored {
  Failed(crate::ParameterSet),
  Unresolved,
}

/// **The three readings FFmpeg gives a sequence parameter set** — the unit,
/// its raw bytes after its header, the unit with truncation let stand
/// (`decode_extradata_ps`, h264_parse.c:383-397; `decode_nal_units`,
/// h264dec.c:699-715) — `unit` read over `memory` ([`Walk::memory`]) and its
/// raw bytes over `mem`, the buffer it was cut from: the set's id, what it
/// says and which reading stored it; `None` where none does.
fn h264_sps_readings(unit: &Unit<'_>, memory: &[u8], mem: &[u8]) -> Option<(usize, Sps, u8)> {
  h264_sps(&mut unit.reader(memory), false)
    .map(|(id, sps)| (id, sps, 1))
    .or_else(|| h264_sps(&mut unit.raw_reader(mem), false).map(|(id, sps)| (id, sps, 2)))
    .or_else(|| h264_sps(&mut unit.reader(memory), true).map(|(id, sps)| (id, sps, 3)))
}

/// **`decode_extradata_ps`** (h264_parse.c:367-415) over the first `length`
/// bytes of `mem`, length-prefixed by two bytes where `nalff` (an `avcC`
/// entry) or start-coded: each sequence parameter set read the three ways
/// it reads one ([`h264_sps_readings`]), each picture parameter set once,
/// every other unit passed over, each set stored in `sets` as FFmpeg stores
/// it. The first set it does not store ends the reading, as there; a split
/// that fails stores nothing and answers success, as there.
fn h264_sets<S: H264Sets>(
  mem: &[u8],
  length: usize,
  nalff: bool,
  sets: &mut S,
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
        let Some((id, sps, reading)) = h264_sps_readings(&unit, memory, mem) else {
          return Err(Unstored::Failed(crate::ParameterSet::Sequence));
        };
        sets.store_sps(id, sps, &unit, reading);
      }
      8 => {
        let memory = units.memory(&unit, &mut scratch);
        match h264_pps(&mut unit.reader(memory), unit.size_bits, sets) {
          Pps::Stored { id, sps, past_end } => sets.store_pps(id, sps, &unit, past_end),
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
fn h264_entry<S: H264Sets>(
  mem: &[u8],
  nalsize: usize,
  sets: &mut S,
) -> Result<(), crate::ExtradataRejection> {
  let Err(first) = h264_sets(mem, nalsize, true, sets) else {
    return Ok(());
  };
  if nalsize / 2 >= ESCAPE_LIMIT {
    return Err(crate::ExtradataRejection::Oversized(match first {
      Unstored::Failed(set) => set,
      Unstored::Unresolved => crate::ParameterSet::Picture,
    }));
  }
  let escaped = escape(&mem[..nalsize]);
  h264_sets(&escaped, escaped.len(), true, sets).map_err(rejection)
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

/// What **`ff_h264_decode_extradata`** (h264_parse.c:466-524) did to a
/// decoder: its verdict, and the framing it left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Extradata {
  /// `Ok` where the record applied whole; the reason otherwise
  /// ([`h264_record`]).
  pub(super) verdict: Result<(), crate::ExtradataRejection>,
  /// `is_avc` as it left it: set by an `avcC` record before anything else is
  /// read (h264_parse.c:477-479), cleared by any other (518).
  pub(super) is_avc: bool,
  /// The NAL length size it stored: an `avcC` record's, where its reading
  /// reached the end (516); `None` where it stored none.
  pub(super) nal_length_size: Option<u8>,
}

/// **`ff_h264_decode_extradata`** (h264_parse.c:466-524) applying `record`
/// to `sets`, every set FFmpeg stores stored there, and what it did besides
/// ([`Extradata`]). A set it fails to parse is skipped and the reading goes
/// on, as there; an `avcC` entry running past the record, or too large for
/// the escaping retry, ends it, the sets before it stored and the NAL length
/// size not. The verdict is the first thing that kept the record from
/// applying whole.
pub(super) fn h264_extradata<S: H264Sets>(record: &[u8], sets: &mut S) -> Extradata {
  if record.first() != Some(&1) {
    return Extradata {
      verdict: h264_sets(record, record.len(), false, sets).map_err(rejection),
      is_avc: false,
      nal_length_size: None,
    };
  }
  let ended = |verdict| Extradata {
    verdict: Err(verdict),
    is_avc: true,
    nal_length_size: None,
  };
  if record.len() < 7 {
    return ended(crate::ExtradataRejection::TooShort { size: record.len() });
  }
  let mut at = 6usize;
  let mut first = None;
  let sequence = usize::from(record[5] & 0x1f);
  if let Some(end) = h264_entries(
    record,
    &mut at,
    sequence,
    crate::ParameterSet::Sequence,
    sets,
    &mut first,
  ) {
    return ended(end);
  }
  // The picture parameter sets' count is the byte after the sequence
  // parameter sets: a padding zero where the record ends there.
  let pictures = usize::from(record.get(at).copied().unwrap_or(0));
  at += 1;
  if let Some(end) = h264_entries(
    record,
    &mut at,
    pictures,
    crate::ParameterSet::Picture,
    sets,
    &mut first,
  ) {
    return ended(end);
  }
  Extradata {
    verdict: first.map_or(Ok(()), Err),
    is_avc: true,
    nal_length_size: Some((record[4] & 3) + 1),
  }
}

/// `count` `avcC` entries of kind `set` from `at` in `record`, each read
/// by [`h264_entry`] into `sets` and `at` moved past it; the first that
/// FFmpeg does not apply whole recorded in `first`. Answers the verdict
/// that ends the record's reading — an entry running past the record, or
/// too large for the escaping retry (h264_parse.c:489-498, 503-512) — where
/// one does.
fn h264_entries<S: H264Sets>(
  record: &[u8],
  at: &mut usize,
  count: usize,
  set: crate::ParameterSet,
  sets: &mut S,
  first: &mut Option<crate::ExtradataRejection>,
) -> Option<crate::ExtradataRejection> {
  let byte = |at: usize| record.get(at).copied().unwrap_or(0);
  for _ in 0..count {
    let nalsize = ((usize::from(byte(*at)) << 8) | usize::from(byte(*at + 1))) + 2;
    if nalsize > record.len().saturating_sub(*at) {
      return Some(first.unwrap_or(crate::ExtradataRejection::Overrun(set)));
    }
    match h264_entry(&record[*at..], nalsize, sets) {
      Ok(()) => {}
      Err(oversized @ crate::ExtradataRejection::Oversized(_)) => {
        return Some(first.unwrap_or(oversized));
      }
      Err(skipped) => {
        first.get_or_insert(skipped);
      }
    }
    *at += nalsize;
  }
  None
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
///   record does not carry** is read against the sets the decoder holds
///   already (`h264_ps.c:731-738`): here, none — the session reads it against
///   what it holds ([`super::held::Held::h264_verdict`]).
///
/// Any of them leaves the decoder on parameters other than the record's.
#[cfg(test)]
pub(crate) fn h264_record(record: &[u8]) -> Result<(), crate::ExtradataRejection> {
  h264_extradata(record, &mut [None; 32]).verdict
}

/// **Whether FFmpeg's H.264 decoder reads a packet's body as an `avcC`
/// record** where its framing is `avcC` (`is_avc`) — `h264_decode_frame`
/// (h264dec.c:1045-1050), which then applies the body as extradata
/// (`ff_h264_decode_extradata`) and decodes no slice of it: nine bytes or
/// more, the version byte 1, a zero third byte and the reserved bits of the
/// NAL length size byte set, and `is_avcc_extradata` (899-921) — at least one
/// sequence and one picture parameter set entry, each within the buffer, whose
/// header byte, the forbidden bit and type read under `0x9F`, says SPS (7) and
/// PPS (8). A byte past the buffer reads as the zero of its padding, as there.
pub(super) fn avcc_body(buf: &[u8]) -> bool {
  let byte = |at: usize| buf.get(at).copied().unwrap_or(0);
  if buf.len() < 9 || buf[0] != 1 || buf[2] != 0 || buf[4] & 0xfc != 0xfc {
    return false;
  }
  let entries = |at: &mut usize, count: usize, kind: u8| {
    count > 0
      && (0..count).all(|_| {
        let nalsize = ((usize::from(byte(*at)) << 8) | usize::from(byte(*at + 1))) + 2;
        let fits = nalsize <= buf.len() - *at && byte(*at + 2) & 0x9f == kind;
        *at += nalsize;
        fits
      })
  };
  let mut at = 6usize;
  entries(&mut at, usize::from(byte(5) & 0x1f), 7) && {
    let pictures = usize::from(byte(at));
    at += 1;
    entries(&mut at, pictures, 8)
  }
}

/// **The framing FFmpeg's H.264 decoder re-guesses for a packet** where its
/// NAL length size is four (`decode_nal_units`, h264dec.c:602-607): start
/// codes (`false`) where the packet, over eight bytes, opens on a four-byte
/// start code and the 32 bits after the byte that follows it read as more
/// than its size; `avcC` (`true`) where its first four bytes, over three,
/// read as more than 1 and no more than its size; `None` where neither, the
/// framing it held kept. Both compare as unsigned 32-bit numbers, as there.
pub(super) fn h264_reguess(data: &[u8]) -> Option<bool> {
  let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
  let word = |at: usize| {
    data
      .get(at..at + 4)
      .map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
  };
  if data.len() > 8 && word(0) == Some(1) && word(5).is_some_and(|after| after > size) {
    Some(false)
  } else if data.len() > 3 && word(0).is_some_and(|first| first > 1 && first <= size) {
    Some(true)
  } else {
    None
  }
}

/// **The parameter sets FFmpeg's H.264 decoder reads off a packet's
/// units**, stored in `sets` as it stores them — `decode_nal_units`
/// (h264dec.c:583-824) over a packet split as the decoder's framing says,
/// length-prefixed by `nal_length_size` bytes where `is_avc`, start-coded
/// otherwise, every unit copied (`ff_h2645_packet_split` without
/// `H2645_FLAG_SMALL_PADDING`, h264dec.c:609-610): each sequence parameter
/// set read the three ways FFmpeg reads one (h264dec.c:699-715) and each
/// picture parameter set once (717-728), a set none of them stores passed
/// over; nothing where the split fails (609-615); and the reading ending at
/// an IDR slice whose header reads as a P slice's, "Invalid inter IDR frame"
/// (637-641). A hardware accelerator's `decode_params` (701-706, 718-723) is
/// taken to pass: VideoToolbox's copies the unit (videotoolbox.c:434-450).
pub(super) fn h264_packet<S: H264Sets>(
  data: &[u8],
  is_avc: bool,
  nal_length_size: usize,
  sets: &mut S,
) {
  let mut units = Walk::new(
    data,
    data.len(),
    nal_length_size,
    Codec::H264,
    is_avc,
    false,
  );
  if !units.accepted() {
    return;
  }
  let mut scratch = Vec::new();
  while let Some(Ok(unit)) = units.next() {
    match unit.kind {
      5 if unit.head::<2>()[1] & 0xfc == 0x98 => return,
      7 => {
        let memory = units.memory(&unit, &mut scratch);
        if let Some((id, sps, reading)) = h264_sps_readings(&unit, memory, data) {
          sets.store_sps(id, sps, &unit, reading);
        }
      }
      8 => {
        let memory = units.memory(&unit, &mut scratch);
        if let Pps::Stored { id, sps, past_end } =
          h264_pps(&mut unit.reader(memory), unit.size_bits, sets)
        {
          sets.store_pps(id, sps, &unit, past_end);
        }
      }
      _ => {}
    }
  }
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

/// What `ff_hevc_decode_nal_vps` (hevc/ps.c:786-959) does with a video
/// parameter set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Vps {
  /// Refused: the decoder keeps the set it held of that id. `aborts` where
  /// the error is other than invalid data — the base layer flags'
  /// `AVERROR_PATCHWELCOME` (ps.c:818-824) — which ends a decoder's reading
  /// of the packet's units (`decode_nal_unit`, hevc/hevcdec.c:3665-3672).
  Refused { aborts: bool },
  /// The very bytes of the set held of its id: nothing changes
  /// (ps.c:797-802).
  Kept,
  /// Stored as the set of its id; `alpha` where FFmpeg reads it as alpha
  /// video (`ff_hevc_is_alpha_video`, hevc/hevcdec.c:440-457): two layers,
  /// the second's `nuh_layer_id` not 0, the auxiliary scalability type set.
  Stored { alpha: bool },
}

/// The auxiliary scalability type's flag in `scalability_mask_flag`
/// (`HEVC_SCALABILITY_AUXILIARY`, hevc/hevc.h:169).
const SCALABILITY_AUXILIARY: u32 = 1 << (15 - 3);

/// The multiview scalability type's flag (`HEVC_SCALABILITY_MULTIVIEW`,
/// hevc/hevc.h:167).
const SCALABILITY_MULTIVIEW: u32 = 1 << (15 - 1);

/// **The video parameter sets an HEVC decoder holds, by id, as far as a
/// reading knows them**: each one's bytes, for the identity test
/// `ff_hevc_decode_nal_vps` makes before it reads a set (ps.c:797-802), or
/// `None` for an id the reading does not know the decoder to hold. A set
/// that runs past its unit is stored only under an id the decoder holds
/// nothing of (ps.c:944-949); an id this table does not hold is taken as
/// one the decoder holds nothing of, so a set FFmpeg might refuse there is
/// read as stored — the reading errs toward alpha, never away from it. So
/// the table holds only what the decoder certainly holds: a set read after
/// a unit whose reading may have ended the buffer's — a sequence or picture
/// parameter set, an SEI message, a slice, which this crate does not parse
/// — is read for alpha and not kept.
#[derive(Clone, Default)]
pub(super) struct VpsTable {
  held: [Option<Vec<u8>>; 16],
}

impl VpsTable {
  /// FFmpeg's reading of `unit`, a video parameter set `walk` handed out
  /// (the walk standing past it), this table updated as the decoder's
  /// would be where the decoder `certainly` reads it.
  fn read(
    &mut self,
    walk: &Walk<'_>,
    unit: &Unit<'_>,
    scratch: &mut Vec<u8>,
    certainly: bool,
  ) -> Vps {
    let memory = walk.memory(unit, scratch);
    let mut reader = unit.reader(memory);
    let id = reader.bits(4) as usize;
    let size = ((unit.size_bits + 7) >> 3) as usize;
    let bytes = memory.get(..size).unwrap_or(memory);
    if self.held[id].as_deref() == Some(bytes) {
      return Vps::Kept;
    }
    let read = hevc_vps(&mut reader);
    let alpha = match read {
      Err(aborts) => return Vps::Refused { aborts },
      Ok(read) => read.alpha,
    };
    // Read past its unit: kept only where nothing of its id is held.
    if reader.left() < 0 && self.held[id].is_some() {
      return Vps::Refused { aborts: false };
    }
    if certainly {
      self.held[id] = Some(bytes.to_vec());
    }
    Vps::Stored { alpha }
  }

  /// **The video parameter sets FFmpeg's HEVC decoder stores from codec
  /// extradata** — at its open, or from a packet's `AV_PKT_DATA_NEW_EXTRADATA`
  /// (`hevc_decode_extradata`, hevc/hevcdec.c:3795-3825) — read as
  /// `ff_hevc_decode_extradata` reads them (hevc/parse.c:79-140): an `hvcC`
  /// record (23 bytes or more, its first byte 1, or 0 with a second or third
  /// byte that is not a start code's) entry by entry, each its own buffer,
  /// a set FFmpeg refuses ending its entry's reading; anything else as a
  /// start-coded buffer, a set FFmpeg refuses ending its reading. An entry
  /// running past the record ends the reading of the record. Answers whether
  /// a set stored is alpha video, this table updated.
  pub(super) fn read_extradata(&mut self, record: &[u8]) -> bool {
    let size = record.len();
    let hvcc =
      size >= 23 && (record[0] == 1 || (record[0] == 0 && (record[1] != 0 || record[2] > 1)));
    if !hvcc {
      return self.read_units(record, record.len(), None, true);
    }
    let byte = |at: usize| record.get(at).copied().unwrap_or(0);
    let mut at = 23usize;
    let mut alpha = false;
    for _ in 0..byte(22) {
      at = (at + 1).min(size); // the array's type
      // `bytestream2_get_be16`: 0, at the end, where fewer than two bytes
      // are left.
      let count = if size - at < 2 {
        0
      } else {
        (usize::from(byte(at)) << 8) | usize::from(byte(at + 1))
      };
      at = (at + 2).min(size);
      for _ in 0..count {
        let nalsize = ((usize::from(byte(at)) << 8) | usize::from(byte(at + 1))) + 2;
        if size - at < nalsize {
          return alpha;
        }
        alpha |= self.read_units(&record[at..], nalsize, Some(2), true);
        at += nalsize;
      }
    }
    alpha
  }

  /// **The video parameter sets FFmpeg's HEVC decoder stores from a packet**,
  /// its units framed as `nal_length` says — length-prefixed by so many
  /// bytes, or start-coded — read as `decode_nal_units` reads them
  /// (hevc/hevcdec.c:3681-3776): nothing where FFmpeg's split refuses the
  /// packet; a set refused for invalid data passed over, one refused for
  /// another reason ending the packet's reading. Answers whether a set
  /// stored is alpha video, this table updated.
  pub(super) fn read_packet(&mut self, data: &[u8], nal_length: Option<usize>) -> bool {
    self.read_units(data, data.len(), nal_length, false)
  }

  /// The units of the first `length` bytes of `mem`, as one of FFmpeg's
  /// readings splits them (`H2645_FLAG_SMALL_PADDING`); `extradata`, as
  /// `hevc_decode_nal_units` (hevc/parse.c:24-77), where any refusal ends the
  /// reading, or as a packet's, where only one other than invalid data does.
  /// A unit of another kind is passed over as though its parser met
  /// nothing — on, whatever it met — and from the first one whose parser may
  /// end the reading (a sequence or picture parameter set, an SEI message,
  /// in a packet a slice too) the sets read after are read for alpha and not
  /// kept ([`Self::read`]). A set a hardware accelerator's `decode_params`
  /// refuses (hevc/hevcdec.c:3597-3607) is taken as read: VideoToolbox's only
  /// copies the unit (videotoolbox.c:1122-1128).
  fn read_units(
    &mut self,
    mem: &[u8],
    length: usize,
    nal_length: Option<usize>,
    extradata: bool,
  ) -> bool {
    let mut units = Walk::new(
      mem,
      length,
      nal_length.unwrap_or(0),
      Codec::Hevc,
      nal_length.is_some(),
      true,
    );
    if !units.accepted() {
      return false;
    }
    let mut scratch = Vec::new();
    let mut alpha = false;
    let mut certainly = true;
    while let Some(Ok(unit)) = units.next() {
      if unit.kind != 32 {
        // SPS, PPS, the SEI messages; in a packet the slices (`decode_slice`).
        certainly &= !(matches!(unit.kind, 33 | 34 | 39 | 40)
          || (!extradata && matches!(unit.kind, 0..=9 | 16..=21)));
        continue;
      }
      match self.read(&units, &unit, &mut scratch, certainly) {
        Vps::Stored { alpha: true } => alpha = true,
        Vps::Refused { aborts } if extradata || aborts => break,
        Vps::Stored { alpha: false } | Vps::Refused { .. } | Vps::Kept => {}
      }
    }
    alpha
  }
}

/// What `ff_hevc_decode_nal_vps` stores of a video parameter set that a
/// reading after it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct VpsRead {
  /// Whether FFmpeg reads it as alpha video (`ff_hevc_is_alpha_video`,
  /// hevc/hevcdec.c:440-457).
  pub(super) alpha: bool,
  /// `vps_max_sub_layers`, which bounds a sequence parameter set's own
  /// (hevc/ps.c:1262-1278).
  pub(super) max_sub_layers: u8,
}

/// **`ff_hevc_decode_nal_vps`** (hevc/ps.c:786-959) from the reader past the
/// set's id, as far as its verdict goes: `Ok` with what the set it stores
/// says ([`VpsRead`]), `Err` with whether its refusal ends a decoder's
/// reading of the packet. The test for a reading run past the unit, which
/// needs the table, is the caller's ([`VpsTable::read`]).
pub(super) fn hevc_vps(r: &mut Reader<'_>) -> Result<VpsRead, bool> {
  const INVALID: Result<VpsRead, bool> = Err(false);
  // vps_base_layer_internal_flag, vps_base_layer_available_flag.
  let (internal, available) = (r.bit(), r.bit());
  if !internal || !available {
    return Err(true);
  }
  let max_layers = r.bits(6) as i32 + 1;
  let max_sub_layers = r.bits(3) as i32 + 1;
  r.bit(); // vps_temporal_id_nesting_flag
  if r.bits(16) != 0xffff || max_sub_layers > 7 || !parse_ptl(r, true, max_sub_layers) {
    return INVALID;
  }
  let ordering = r.bit();
  for _ in (if ordering { 0 } else { max_sub_layers - 1 })..max_sub_layers {
    // vps_max_dec_pic_buffering, an unsigned of minus1 + 1, from 1 to 16;
    // vps_max_num_reorder_pics over its bound is a warning only.
    let buffering = r.ue_long().wrapping_add(1);
    r.ue_long();
    r.ue_long();
    if buffering > 16 || buffering == 0 {
      return INVALID;
    }
  }
  let max_layer_id = i64::from(r.bits(6));
  let layer_sets = r.ue_long().wrapping_add(1) as i32;
  if !(1..=1024).contains(&layer_sets)
    || (i64::from(layer_sets) - 1) * (max_layer_id + 1) > r.left()
  {
    return INVALID;
  }
  let layer1_included = if layer_sets > 1 {
    r.bits64((max_layer_id + 1) as u32)
  } else {
    0
  };
  if layer_sets > 2 {
    r.skip((i64::from(layer_sets) - 2) as u64 * (max_layer_id + 1) as u64);
  }
  if r.bit() {
    // vps_timing_info_present_flag
    r.bits(32);
    r.bits(32);
    if r.bit() {
      r.ue_long(); // vps_num_ticks_poc_diff_one_minus1
    }
    let hrd_sets = r.ue_long() as i32;
    if hrd_sets as u32 > layer_sets as u32 {
      return INVALID;
    }
    for index in 0..hrd_sets {
      r.ue_long(); // hrd_layer_set_idx
      let common = index == 0 || r.bit();
      hevc_hrd(r, common, max_sub_layers);
    }
  }
  if max_layers > 1 && r.bit() {
    // vps_extension_flag
    let mut ext = Extension {
      layers: 1,
      mask: 0,
      layer_id: 0,
    };
    let read = |alpha| VpsRead {
      alpha,
      max_sub_layers: max_sub_layers as u8,
    };
    match vps_extension(
      r,
      &mut ext,
      max_layers,
      max_sub_layers,
      layer_sets,
      layer1_included,
    ) {
      Ok(()) => return Ok(read(ext.alpha())),
      // "Broken VPS extension, treating as alpha video" where two layers,
      // the second's id and the auxiliary type were read; one layer
      // otherwise (ps.c:921-939).
      Err(Unsupported::PatchWelcome) => return Ok(read(ext.alpha())),
      Err(Unsupported::Invalid) => return INVALID,
    }
  }
  Ok(VpsRead {
    alpha: false,
    max_sub_layers: max_sub_layers as u8,
  })
}

/// What `decode_vps_ext` set before it returned: the layers, the mask and
/// the second layer's `nuh_layer_id`.
struct Extension {
  layers: i32,
  mask: u32,
  layer_id: u32,
}

impl Extension {
  /// `ff_hevc_is_alpha_video` over what was set.
  const fn alpha(&self) -> bool {
    self.layers == 2 && self.layer_id != 0 && self.mask & SCALABILITY_AUXILIARY != 0
  }
}

/// How `decode_vps_ext` failed.
enum Unsupported {
  PatchWelcome,
  Invalid,
}

/// **`decode_vps_ext`** (hevc/ps.c:483-784), recording in `ext` what it sets
/// as it sets it.
fn vps_extension(
  r: &mut Reader<'_>,
  ext: &mut Extension,
  max_layers: i32,
  max_sub_layers: i32,
  layer_sets: i32,
  layer1_included: u64,
) -> Result<(), Unsupported> {
  use Unsupported::{Invalid, PatchWelcome};
  if max_layers > 2 || layer_sets > 2 {
    return Err(PatchWelcome);
  }
  r.align();
  ext.layers = 2;
  if !parse_ptl(r, false, max_sub_layers) {
    return Err(Invalid);
  }
  let splitting = r.bit();
  ext.mask = r.bits(16);
  let types = ext.mask.count_ones() as i32;
  if types == 0 {
    return Err(Invalid);
  }
  if ext.mask & (SCALABILITY_MULTIVIEW | SCALABILITY_AUXILIARY) == 0 {
    return Err(PatchWelcome);
  }
  let mut lengths = [0u32; 16];
  for length in lengths
    .iter_mut()
    .take((types - i32::from(splitting)).max(0) as usize)
  {
    *length = r.bits(3) + 1; // dimension_id_len_minus1
  }
  ext.layer_id = if r.bit() {
    // vps_nuh_layer_id_present_flag, then layer_id_in_nuh[1]
    let id = r.bits(6);
    if id > 62 {
      return Err(Invalid);
    }
    id
  } else {
    1
  };
  if !splitting {
    let mut dimensions = [0u32; 16];
    for (dimension, &length) in dimensions.iter_mut().zip(&lengths).take(types as usize) {
      *dimension = r.bits(length);
    }
    let index = usize::from(ext.mask & SCALABILITY_MULTIVIEW != 0);
    if ext.mask & SCALABILITY_AUXILIARY != 0 && dimensions[index] != 1 {
      // AuxId 1 is alpha; another is unsupported (ps.c:603-610).
      return Err(PatchWelcome);
    }
  }
  let view_id_len = r.bits(4);
  if view_id_len != 0 {
    let views = if ext.mask & SCALABILITY_MULTIVIEW != 0 {
      2
    } else {
      1
    };
    for _ in 0..views {
      r.bits(view_id_len);
    }
  }
  let direct_dependency = r.bit();
  let mut add_layer_sets = 0u8;
  if !direct_dependency {
    // An `uint8_t`: an error value of `get_ue_golomb` truncated.
    add_layer_sets = r.ue() as u8;
    if add_layer_sets > 1 {
      return Err(PatchWelcome);
    }
    if add_layer_sets != 0 && !r.bit() {
      // highest_layer_idx_plus1
      return Err(PatchWelcome);
    }
  }
  if (layer_sets + i32::from(add_layer_sets)) as u8 != 2 {
    // num_output_layer_sets, an `uint8_t`
    return Err(PatchWelcome);
  }
  let mut sub_layers = [1u32; 2];
  if r.bit() {
    // vps_sub_layers_max_minus1_present_flag
    for count in &mut sub_layers {
      *count = r.bits(3) + 1;
    }
  }
  if r.bit() {
    // max_tid_ref_present_flag
    r.skip(3);
  }
  r.bit(); // default_ref_layers_active_flag
  let profiles = r.ue().wrapping_add(1);
  for _ in 2..profiles {
    let present = r.bit();
    if !parse_ptl(r, present, max_sub_layers) {
      return Err(Invalid);
    }
  }
  if r.ue() != 0 {
    // num_add_olss
    return Err(PatchWelcome);
  }
  if r.bits(2) != 0 {
    // default_output_layer_idc
    return Err(PatchWelcome);
  }
  if layer1_included != 0 && layer1_included != (1 | (1u64 << ext.layer_id)) {
    return Err(PatchWelcome);
  }
  let output_layers = if layer1_included == 0 { 1 } else { 2 };
  if layer_sets == 1 {
    r.bit();
  }
  if profiles > 1 {
    let width = log2((profiles as u32 - 1) << 1);
    for _ in 0..output_layers {
      if r.bits(width) as i32 >= profiles {
        // profile_tier_level_idx
        return Err(Invalid);
      }
    }
  }
  if r.ue_31() != 0 {
    // vps_num_rep_formats_minus1
    return Err(PatchWelcome);
  }
  let width = i64::from(r.bits(16));
  let height = i64::from(r.bits(16));
  if !r.bit() {
    // chroma_and_bit_depth_vps_present_flag
    return Err(Invalid);
  }
  let chroma_format_idc = r.bits(2) as usize;
  if chroma_format_idc == 3 {
    r.bit(); // separate_colour_plane_flag
  }
  let luma = r.bits(4) + 8;
  let chroma = r.bits(4) + 8;
  if luma > 16 || chroma > 16 || luma != chroma {
    return Err(PatchWelcome);
  }
  if r.bit() {
    // conformance_window_vps_flag: `read_window` (ps.c:66-87), offsets in
    // chroma units that must leave a picture.
    const SUB_WIDTH: [i64; 4] = [1, 2, 2, 1];
    const SUB_HEIGHT: [i64; 4] = [1, 2, 1, 1];
    let left = i64::from(r.ue_long()) * SUB_WIDTH[chroma_format_idc];
    let right = i64::from(r.ue_long()) * SUB_WIDTH[chroma_format_idc];
    let top = i64::from(r.ue_long()) * SUB_HEIGHT[chroma_format_idc];
    let bottom = i64::from(r.ue_long()) * SUB_HEIGHT[chroma_format_idc];
    if width <= left + right || height <= top + bottom {
      return Err(Invalid);
    }
  }
  r.bit(); // max_one_active_ref_layer_flag
  r.bit(); // vps_poc_lsb_aligned_flag
  if !direct_dependency {
    r.bit(); // poc_lsb_not_present_flag
  }
  let sub_layer_flag_info = r.bit();
  for sub_layer in 0..sub_layers[0].max(sub_layers[1]) {
    if sub_layer == 0 || !sub_layer_flag_info || r.bit() {
      for _ in 0..output_layers {
        r.ue_long(); // max_vps_dec_pic_buffering_minus1
      }
      r.ue_long(); // max_vps_num_reorder_pics
      r.ue_long(); // max_vps_latency_increase_plus1
    }
  }
  let dependency_type_len = r.ue_31() + 2;
  if dependency_type_len > 32 {
    return Err(Invalid);
  }
  if r.bit() && r.bits(dependency_type_len as u32) > 2 {
    // direct_dependency_all_layers_flag, then direct_dependency_all_layers_type
    return Err(PatchWelcome);
  }
  let non_vui_extension_length = r.ue() as u32;
  if non_vui_extension_length > 4096 {
    return Err(Invalid);
  }
  r.skip(u64::from(non_vui_extension_length) * 8);
  r.bit(); // vps_vui_present_flag
  Ok(())
}

/// **`parse_ptl`** (hevc/ps.c:337-381), its general and sub-layer
/// `profile_tier_level`s (`decode_profile_tier_level`, ps.c:262-335, 88 bits
/// whichever profile it names) each refused where fewer bits are left than
/// it reads: `false` where it fails.
pub(super) fn parse_ptl(r: &mut Reader<'_>, profile_present: bool, max_sub_layers: i32) -> bool {
  let common = |r: &mut Reader<'_>| {
    if r.left() < 88 {
      return false;
    }
    r.skip(88);
    true
  };
  if profile_present && !common(r) {
    return false;
  }
  let sub_layers = (max_sub_layers - 1).max(0) as usize;
  if r.left() < 8 + if sub_layers > 0 { 16 } else { 0 } {
    return false;
  }
  r.bits(8); // general_level_idc
  let mut present = [(false, false); 6];
  for flags in present.iter_mut().take(sub_layers) {
    *flags = (r.bit(), r.bit());
  }
  if sub_layers > 0 {
    r.skip(2 * (8 - sub_layers) as u64); // reserved_zero_2bits
  }
  for (profile, level) in present.into_iter().take(sub_layers) {
    if profile && !common(r) {
      return false;
    }
    if level {
      if r.left() < 8 {
        return false;
      }
      r.bits(8);
    }
  }
  true
}

/// **`decode_hrd`** (hevc/ps.c:401-467), whose answer the video parameter
/// set's reading does not look at (ps.c:909-910): a sub-layer counting more
/// than 32 CPBs stops it there.
fn hevc_hrd(r: &mut Reader<'_>, common: bool, max_sub_layers: i32) {
  let (mut nal, mut vcl, mut sub_picture) = (false, false, false);
  if common {
    nal = r.bit();
    vcl = r.bit();
    if nal || vcl {
      sub_picture = r.bit();
      if sub_picture {
        r.bits(8);
        r.bits(5);
        r.bit();
        r.bits(5);
      }
      r.bits(4);
      r.bits(4);
      if sub_picture {
        r.bits(4);
      }
      r.bits(5);
      r.bits(5);
      r.bits(5);
    }
  }
  for _ in 0..max_sub_layers {
    let fixed_general = r.bit();
    let fixed_within = !fixed_general && r.bit();
    let mut low_delay = false;
    if fixed_general || fixed_within {
      r.ue_long(); // elemental_duration_in_tc_minus1
    } else {
      low_delay = r.bit();
    }
    // `cpb_cnt_minus1`, zeroed with the set's HRD parameters, is kept where
    // a low-delay sub-layer reads none.
    let mut cpbs = 1u32;
    if !low_delay {
      let minus1 = r.ue_long();
      if minus1 > 31 {
        return;
      }
      cpbs = minus1 + 1;
    }
    for _ in 0..(u32::from(nal) + u32::from(vcl)) * cpbs {
      r.ue_long(); // bit_rate_value_minus1
      r.ue_long(); // cpb_size_value_minus1
      if sub_picture {
        r.ue_long(); // cpb_size_du_value_minus1
        r.ue_long(); // bit_rate_du_value_minus1
      }
      r.bit(); // cbr_flag
    }
  }
}

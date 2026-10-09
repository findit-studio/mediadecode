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

  /// `get_bits(n)` where `n` may be 0, which advances nothing; its value
  /// unread. FFmpeg reads a field this wide only where it reads a value
  /// nothing after it looks at.
  fn skip_field(&mut self, n: u32) {
    if n > 0 {
      self.bits(n);
    }
  }

  /// `show_bits(n)`, `n` from 1 to 25: the next `n` bits, the index kept.
  fn peek(&self, n: u32) -> u32 {
    self.show(n)
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

/// **What FFmpeg's `SPS` holds of a set once
/// `ff_h264_decode_seq_parameter_set` stores it, `data` aside**
/// (h264_ps.h:44-105): every field its reading sets — the set's own
/// (h264_ps.c:284-594), its VUI's (`ff_h2645_decode_common_vui_params`,
/// h2645_vui.c:37-100; `decode_vui_parameters`, h264_ps.c:133-199) and its
/// HRD's (`decode_hrd_parameters`, 106-131) — which FFmpeg compares with
/// `data` to keep an identical set in place, `memcmp` of the whole structure
/// (578-587). A field a value read past the 4096 bytes `data` keeps can set
/// — the end of the VUI's HRD, its bitstream restriction — is held as FFmpeg
/// stores it, and the fields FFmpeg does not store are not held. Where
/// FFmpeg stores a value it maps from the one it reads through a table or a
/// rule of its decoder's — the scaling matrices from their deltas (201-268),
/// the aspect ratio, colour description and chroma location
/// (h2645_vui.c:41-99), the reference frame count under the `SMV2` codec tag
/// (h264_ps.c:440-441), the reorder depth from the level where the bitstream
/// restriction is absent (543-555) — the value read is held: alike values
/// store alike fields. Every one of those is read within the first 3,200
/// bytes of any set FFmpeg stores, the longest codes it takes for each field
/// before the timing information's end summed, so two sets whose values read
/// there differ differ in `data` as well.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct SpsFields {
  sps_id: u32,
  profile_idc: i32,
  level_idc: i32,
  constraint_set_flags: i32,
  chroma_format_idc: i32,
  residual_color_transform_flag: bool,
  bit_depth_luma: i32,
  bit_depth_chroma: i32,
  transform_bypass: bool,
  /// `decode_scaling_matrices`' reading: its present flag, then each list's
  /// flag and every delta read for it.
  scaling: Vec<i32>,
  log2_max_frame_num: i32,
  poc_type: i32,
  log2_max_poc_lsb: i32,
  delta_pic_order_always_zero_flag: bool,
  offset_for_non_ref_pic: i32,
  offset_for_top_to_bottom_field: i32,
  /// One for each of `poc_cycle_length`.
  offset_for_ref_frame: Vec<i32>,
  ref_frame_count: i32,
  gaps_in_frame_num_allowed_flag: bool,
  mb_width: i32,
  mb_height: i32,
  frame_mbs_only_flag: bool,
  mb_aff: bool,
  direct_8x8_inference_flag: bool,
  crop: bool,
  /// Left, right, top and bottom, in luma samples.
  crop_offsets: [u32; 4],
  vui_parameters_present_flag: bool,
  vui: CommonVui,
  timing_info_present_flag: bool,
  num_units_in_tick: u32,
  time_scale: u32,
  fixed_frame_rate_flag: bool,
  nal_hrd_parameters_present_flag: bool,
  vcl_hrd_parameters_present_flag: bool,
  cpb_cnt: i32,
  bit_rate_scale: u32,
  bit_rate_value: [u32; 32],
  cpb_size_value: [u32; 32],
  cpr_flag: u32,
  initial_cpb_removal_delay_length: u32,
  cpb_removal_delay_length: u32,
  dpb_output_delay_length: u32,
  time_offset_length: u32,
  pic_struct_present_flag: bool,
  bitstream_restriction_flag: bool,
  num_reorder_frames: i32,
  max_dec_frame_buffering: i32,
}

/// What `ff_h2645_decode_common_vui_params` (h2645_vui.c:37-100) reads: each
/// flag, and the values behind it as read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CommonVui {
  /// `aspect_ratio_idc`, and an extended aspect ratio's width and height.
  aspect_ratio: Option<(u32, Option<(u32, u32)>)>,
  /// `overscan_appropriate_flag`.
  overscan: Option<bool>,
  /// `video_format`, `video_full_range_flag`, and the colour description's
  /// primaries, transfer characteristics and matrix coefficients.
  video_signal: Option<(u32, bool, Option<[u32; 3]>)>,
  /// The chroma sample location types of the top and bottom fields.
  chroma_loc: Option<(i32, i32)>,
}

/// **`ff_h264_decode_seq_parameter_set`** (h264_ps.c:284-594) read from
/// `r`, past the unit's header: the set's id, what it says to a picture
/// parameter set and what FFmpeg's `SPS` holds of it ([`SpsFields`]), where
/// FFmpeg stores it; `None` where it fails it. `ignore_truncation` lets a
/// reading that ran past the unit stand (h264_ps.c:535-541). The decoder's
/// context is the one this crate opens: no `AV_CODEC_FLAG2_IGNORE_CROP`.
fn h264_sps(r: &mut Reader<'_>, ignore_truncation: bool) -> Option<(usize, Sps, SpsFields)> {
  let mut f = SpsFields {
    time_offset_length: 24,
    ..SpsFields::default()
  };
  f.profile_idc = r.bits(8) as i32;
  for flag in 0..6 {
    f.constraint_set_flags |= i32::from(r.bit()) << flag;
  }
  r.skip(2); // reserved_zero_2bits
  f.level_idc = r.bits(8) as i32;
  f.sps_id = r.ue_31() as u32;
  if f.sps_id >= 32 {
    return None;
  }
  if matches!(
    f.profile_idc,
    100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 144
  ) {
    f.chroma_format_idc = r.ue_31();
    if f.chroma_format_idc as u32 > 3 {
      return None;
    }
    // residual_color_transform_flag: separate colour planes, unsupported.
    if f.chroma_format_idc == 3 && r.bit() {
      return None;
    }
    f.bit_depth_luma = r.ue_31() + 8;
    f.bit_depth_chroma = r.ue_31() + 8;
    if f.bit_depth_chroma != f.bit_depth_luma
      || !(8..=14).contains(&f.bit_depth_luma)
      || !(8..=14).contains(&f.bit_depth_chroma)
    {
      return None;
    }
    f.transform_bypass = r.bit();
    let present = r.bit();
    f.scaling.push(i32::from(present));
    if !scaling_matrices(r, true, present, false, f.chroma_format_idc, &mut f.scaling) {
      return None;
    }
  } else {
    f.chroma_format_idc = 1;
    f.bit_depth_luma = 8;
    f.bit_depth_chroma = 8;
  }
  // log2_max_frame_num_minus4, from 0 to 12.
  let log2_max_frame_num_minus4 = r.ue_31();
  if !(0..=12).contains(&log2_max_frame_num_minus4) {
    return None;
  }
  f.log2_max_frame_num = log2_max_frame_num_minus4 + 4;
  f.poc_type = r.ue_31();
  match f.poc_type {
    0 => {
      // log2_max_pic_order_cnt_lsb_minus4, at most 12.
      let minus4 = r.ue_31();
      if minus4 as u32 > 12 {
        return None;
      }
      f.log2_max_poc_lsb = minus4 + 4;
    }
    1 => {
      f.delta_pic_order_always_zero_flag = r.bit();
      f.offset_for_non_ref_pic = r.se_long();
      f.offset_for_top_to_bottom_field = r.se_long();
      if f.offset_for_non_ref_pic == i32::MIN || f.offset_for_top_to_bottom_field == i32::MIN {
        return None;
      }
      let cycle = r.ue();
      if cycle as u32 >= 256 {
        return None;
      }
      for _ in 0..cycle {
        let offset = r.se_long();
        if offset == i32::MIN {
          return None;
        }
        f.offset_for_ref_frame.push(offset);
      }
    }
    2 => {}
    _ => return None,
  }
  // max_num_ref_frames, at most H264_MAX_DPB_FRAMES.
  f.ref_frame_count = r.ue_31();
  if f.ref_frame_count > 16 {
    return None;
  }
  f.gaps_in_frame_num_allowed_flag = r.bit();
  f.mb_width = r.ue().wrapping_add(1);
  let mut mb_height = r.ue().wrapping_add(1);
  f.frame_mbs_only_flag = r.bit();
  if mb_height as u32 >= (i32::MAX as u32) / 2 {
    return None;
  }
  mb_height *= 2 - i32::from(f.frame_mbs_only_flag);
  f.mb_height = mb_height;
  f.mb_aff = !f.frame_mbs_only_flag && r.bit();
  let limit = (i32::MAX / 16) as u32;
  if f.mb_width as u32 >= limit
    || f.mb_height as u32 >= limit
    || !image_size_valid((16 * f.mb_width) as u32, (16 * f.mb_height) as u32)
  {
    return None;
  }
  f.direct_8x8_inference_flag = r.bit();
  f.crop = r.bit();
  if f.crop {
    // Offsets in chroma units that must leave a picture.
    let (left, right, top, bottom) = (r.ue() as u32, r.ue() as u32, r.ue() as u32, r.ue() as u32);
    let vsub = u32::from(f.chroma_format_idc == 1);
    let hsub = u32::from(f.chroma_format_idc == 1 || f.chroma_format_idc == 2);
    let step_x = 1u32 << hsub;
    let step_y = (2 - u32::from(f.frame_mbs_only_flag)) << vsub;
    let bound = |step: u32| (i32::MAX as u32) / 4 / step;
    if left > bound(step_x)
      || right > bound(step_x)
      || top > bound(step_y)
      || bottom > bound(step_y)
      || left.wrapping_add(right).wrapping_mul(step_x) >= (16 * f.mb_width) as u32
      || top.wrapping_add(bottom).wrapping_mul(step_y) >= (16 * f.mb_height) as u32
    {
      return None;
    }
    f.crop_offsets = [left * step_x, right * step_x, top * step_y, bottom * step_y];
  }
  f.vui_parameters_present_flag = r.bit();
  if f.vui_parameters_present_flag && !h264_vui(r, &mut f) {
    return None;
  }
  if r.left() < 0 && !ignore_truncation {
    return None;
  }
  let sps = Sps {
    profile_idc: f.profile_idc,
    constraint_set_flags: f.constraint_set_flags,
    chroma_format_idc: f.chroma_format_idc,
    bit_depth_luma: f.bit_depth_luma,
  };
  Some((f.sps_id as usize, sps, f))
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
/// then `decode_vui_parameters` (h264_ps.c:133-199), each field FFmpeg
/// stores set in `f`. `false` where it fails the set.
fn h264_vui(r: &mut Reader<'_>, f: &mut SpsFields) -> bool {
  f.vui = common_vui(r);
  // A VUI cut short is taken as it stands.
  if r.show_bit() && r.left() < 10 {
    return true;
  }
  if r.bit() {
    // timing_info_present_flag, cleared where either value is 0.
    let (num_units_in_tick, time_scale) = (r.bits(32), r.bits(32));
    if num_units_in_tick != 0 && time_scale != 0 {
      f.timing_info_present_flag = true;
      f.num_units_in_tick = num_units_in_tick;
      f.time_scale = time_scale;
    }
    f.fixed_frame_rate_flag = r.bit();
  }
  f.nal_hrd_parameters_present_flag = r.bit();
  if f.nal_hrd_parameters_present_flag && !h264_hrd(r, f) {
    return false;
  }
  f.vcl_hrd_parameters_present_flag = r.bit();
  if f.vcl_hrd_parameters_present_flag && !h264_hrd(r, f) {
    return false;
  }
  if f.nal_hrd_parameters_present_flag || f.vcl_hrd_parameters_present_flag {
    r.bit(); // low_delay_hrd_flag
  }
  f.pic_struct_present_flag = r.bit();
  if r.left() == 0 {
    return true;
  }
  f.bitstream_restriction_flag = r.bit();
  if f.bitstream_restriction_flag {
    r.bit(); // motion_vectors_over_pic_boundaries_flag
    for _ in 0..4 {
      // max_bytes_per_pic_denom, max_bits_per_mb_denom and the largest
      // motion vectors' lengths.
      r.ue_31();
    }
    f.num_reorder_frames = r.ue_31();
    f.max_dec_frame_buffering = r.ue_31();
    if r.left() < 0 {
      f.num_reorder_frames = 0;
      f.bitstream_restriction_flag = false;
    }
    if f.num_reorder_frames as u32 > 16 {
      return false;
    }
  }
  true
}

/// `ff_h2645_decode_common_vui_params` (h2645_vui.c:37-100), the start of
/// both codecs' VUI, which fails nothing: what it reads.
fn common_vui(r: &mut Reader<'_>) -> CommonVui {
  let mut vui = CommonVui::default();
  if r.bit() {
    // aspect_ratio_idc, and the SAR it extends to.
    let idc = r.bits(8);
    let extended = (idc == 255).then(|| (r.bits(16), r.bits(16)));
    vui.aspect_ratio = Some((idc, extended));
  }
  if r.bit() {
    vui.overscan = Some(r.bit());
  }
  if r.bit() {
    // video_signal_type_present_flag
    let video_format = r.bits(3);
    let full_range = r.bit();
    let colour = r.bit().then(|| [r.bits(8), r.bits(8), r.bits(8)]);
    vui.video_signal = Some((video_format, full_range, colour));
  }
  if r.bit() {
    // chroma_loc_info_present_flag
    vui.chroma_loc = Some((r.ue_31(), r.ue_31()));
  }
  vui
}

/// `decode_hrd_parameters` (h264_ps.c:106-131), each field FFmpeg stores set
/// in `f` — a second HRD over the first, its entries past the second's count
/// the first's: `false` where it fails.
fn h264_hrd(r: &mut Reader<'_>, f: &mut SpsFields) -> bool {
  let cpb_count = r.ue_31() + 1;
  if cpb_count as u32 > 32 {
    return false;
  }
  f.cpr_flag = 0;
  f.bit_rate_scale = r.bits(4);
  r.bits(4); // cpb_size_scale
  for index in 0..cpb_count as usize {
    f.bit_rate_value[index] = r.ue_long().wrapping_add(1);
    f.cpb_size_value[index] = r.ue_long().wrapping_add(1);
    f.cpr_flag |= u32::from(r.bit()) << index;
  }
  f.initial_cpb_removal_delay_length = r.bits(5) + 1;
  f.cpb_removal_delay_length = r.bits(5) + 1;
  f.dpb_output_delay_length = r.bits(5) + 1;
  f.time_offset_length = r.bits(5);
  f.cpb_cnt = cpb_count;
  true
}

/// `decode_scaling_matrices` (h264_ps.c:231-268), as far as its verdict
/// goes: `false` where a list fails. Every list is read, as there, a failed
/// one's among them, and each list's flag and the deltas read for it pushed
/// to `read`.
fn scaling_matrices(
  r: &mut Reader<'_>,
  sps: bool,
  present: bool,
  transform_8x8: bool,
  chroma_format_idc: i32,
  read: &mut Vec<i32>,
) -> bool {
  if !present {
    return true;
  }
  let mut valid = true;
  for _ in 0..6 {
    valid &= scaling_list(r, 16, read);
  }
  if sps || transform_8x8 {
    let lists = if chroma_format_idc == 3 { 6 } else { 2 };
    for _ in 0..lists {
      valid &= scaling_list(r, 64, read);
    }
  }
  valid
}

/// `decode_scaling_list` (h264_ps.c:201-228): `false` where a delta falls
/// outside -128 to 127. Its flag and each delta read are pushed to `read`.
fn scaling_list(r: &mut Reader<'_>, size: usize, read: &mut Vec<i32>) -> bool {
  let present = r.bit();
  read.push(i32::from(present));
  if !present {
    return true;
  }
  let (mut last, mut next) = (8i32, 8i32);
  for index in 0..size {
    if next != 0 {
      let delta = r.se();
      read.push(delta);
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
    if !scaling_matrices(
      r,
      false,
      present,
      transform_8x8,
      sps.chroma_format_idc,
      &mut Vec::new(),
    ) {
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
  /// FFmpeg stores `sps` under `id`, its `SPS` holding `fields`, read off
  /// `unit` the `reading`-th of the three ways it reads one: 1, the unit; 2,
  /// its raw bytes after its header; 3, the unit, truncation let stand
  /// (h264_parse.c:383-397, h264dec.c:699-715). `memory` is the unit as
  /// handed over ([`Walk::memory`]).
  fn store_sps(
    &mut self,
    id: usize,
    sps: Sps,
    fields: SpsFields,
    unit: &Unit<'_>,
    memory: &[u8],
    reading: u8,
  );
  /// FFmpeg stores the picture parameter set `unit` under `id`, read against
  /// the sequence parameter set held under `sps`; `past_end` where its
  /// reading ran past its payload.
  fn store_pps(&mut self, id: usize, sps: usize, unit: &Unit<'_>, memory: &[u8], past_end: bool);
}

impl H264Sets for [Option<Sps>; 32] {
  fn sps(&self, id: usize) -> Option<Sps> {
    self.get(id).copied().flatten()
  }

  fn store_sps(&mut self, id: usize, sps: Sps, _: SpsFields, _: &Unit<'_>, _: &[u8], _: u8) {
    self[id] = Some(sps);
  }

  fn store_pps(&mut self, _: usize, _: usize, _: &Unit<'_>, _: &[u8], _: bool) {}
}

/// The bytes of a set's `data` FFmpeg's `SPS` and `PPS` keep (h264_ps.h:103,
/// 133).
pub(super) const H264_DATA: usize = 4096;

/// **What FFmpeg's H.264 decoder keeps of a parameter set it stores, and
/// compares** — `data` (`ff_h264_decode_seq_parameter_set`,
/// h264_ps.c:297-306; `ff_h264_decode_picture_parameter_set`, 717-728): the
/// bytes its reader starts at, through the last its payload bits reach, cut
/// at [`H264_DATA`], then the stop bit put back where it filled a byte of its
/// own and the cut left room for it. For the first and third readings of a
/// sequence parameter set and for a picture parameter set, the unit as
/// handed over (`memory`) from its header, to its payload's end
/// (`nal->size_bits`): its trailing zeros, which the splitter drops, not
/// among them. For the second, the unit's raw bytes after its header, all of
/// them, a whole number of bytes whose stop bit is put back after them.
pub(super) fn h264_identity(unit: &Unit<'_>, memory: &[u8], reading: u8) -> Vec<u8> {
  let (bytes, stop_bit_alone) = if reading == 2 {
    (unit.raw().get(1..).unwrap_or_default(), true)
  } else {
    let size = usize::try_from(unit.size_bits.div_ceil(8)).unwrap_or(usize::MAX);
    (
      memory.get(..size).unwrap_or(memory),
      unit.size_bits.is_multiple_of(8),
    )
  };
  let mut data = bytes[..bytes.len().min(H264_DATA)].to_vec();
  if stop_bit_alone && data.len() < H264_DATA {
    data.push(0x80);
  }
  data
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
/// says, what FFmpeg's `SPS` holds of it and which reading stored it; `None`
/// where none does.
fn h264_sps_readings(
  unit: &Unit<'_>,
  memory: &[u8],
  mem: &[u8],
) -> Option<(usize, Sps, SpsFields, u8)> {
  h264_sps(&mut unit.reader(memory), false)
    .map(|(id, sps, fields)| (id, sps, fields, 1))
    .or_else(|| {
      h264_sps(&mut unit.raw_reader(mem), false).map(|(id, sps, fields)| (id, sps, fields, 2))
    })
    .or_else(|| {
      h264_sps(&mut unit.reader(memory), true).map(|(id, sps, fields)| (id, sps, fields, 3))
    })
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
        let Some((id, sps, fields, reading)) = h264_sps_readings(&unit, memory, mem) else {
          return Err(Unstored::Failed(crate::ParameterSet::Sequence));
        };
        sets.store_sps(id, sps, fields, &unit, memory, reading);
      }
      8 => {
        let memory = units.memory(&unit, &mut scratch);
        match h264_pps(&mut unit.reader(memory), unit.size_bits, sets) {
          Pps::Stored { id, sps, past_end } => sets.store_pps(id, sps, &unit, memory, past_end),
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
        if let Some((id, sps, fields, reading)) = h264_sps_readings(&unit, memory, data) {
          sets.store_sps(id, sps, fields, &unit, memory, reading);
        }
      }
      8 => {
        let memory = units.memory(&unit, &mut scratch);
        if let Pps::Stored { id, sps, past_end } =
          h264_pps(&mut unit.reader(memory), unit.size_bits, sets)
        {
          sets.store_pps(id, sps, &unit, memory, past_end);
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
  #[cfg(test)]
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
  /// `vps_max_layers`, which bounds a picture parameter set's reference
  /// layer offsets (hevc/ps.c:1888-1890).
  max_layers: u8,
  /// `nb_layers`: two where `decode_vps_ext` read the second layer, or the
  /// broken extension is kept as alpha video, one otherwise
  /// (hevc/ps.c:548, 919-939); a sequence parameter set of the second layer
  /// referring to a set of one is refused (1299-1303).
  nb_layers: u8,
  /// `rep_format` as `decode_vps_ext` left it (hevc/ps.c:705-734), what a
  /// sequence parameter set of the second layer takes for its own
  /// (1296-1324).
  rep: RepFormat,
  /// Whether FFmpeg stored it with a warning that `AV_EF_EXPLODE` turns into
  /// its refusal: reordered pictures past the decoded picture buffer
  /// (`vps_max_num_reorder_pics`, hevc/ps.c:858-862).
  pub(super) warned: bool,
}

impl VpsRead {
  /// `vps_max_layers`.
  pub(super) const fn max_layers(&self) -> u8 {
    self.max_layers
  }
}

/// `rep_format` (`RepFormat`, hevc/ps.h): the first representation format of
/// a video parameter set's extension, each field as `decode_vps_ext` set it
/// before it returned — zeros past where it stopped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RepFormat {
  width: u16,
  height: u16,
  chroma_format_idc: u8,
  separate_colour_plane: bool,
  bit_depth_luma: u8,
  /// The conformance window, in luma samples.
  window: Window,
}

/// A conformance window's offsets in luma samples — left, right, top,
/// bottom (`HEVCWindow`, its fields `unsigned int`).
type Window = [u32; 4];

/// **`read_window`** (hevc/ps.c:66-87): four offsets in chroma units, scaled
/// to luma samples by the chroma format, which must leave a picture of
/// `width` by `height`; `None` where they do not, FFmpeg's
/// `AVERROR_INVALIDDATA`.
fn read_window(
  r: &mut Reader<'_>,
  chroma_format_idc: usize,
  width: i64,
  height: i64,
) -> Option<Window> {
  const SUB_WIDTH: [i64; 4] = [1, 2, 2, 1];
  const SUB_HEIGHT: [i64; 4] = [1, 2, 1, 1];
  let left = i64::from(r.ue_long()) * SUB_WIDTH[chroma_format_idc];
  let right = i64::from(r.ue_long()) * SUB_WIDTH[chroma_format_idc];
  let top = i64::from(r.ue_long()) * SUB_HEIGHT[chroma_format_idc];
  let bottom = i64::from(r.ue_long()) * SUB_HEIGHT[chroma_format_idc];
  if width <= left + right || height <= top + bottom {
    return None;
  }
  Some([left as u32, right as u32, top as u32, bottom as u32])
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
  let mut warned = false;
  for _ in (if ordering { 0 } else { max_sub_layers - 1 })..max_sub_layers {
    // vps_max_dec_pic_buffering, an unsigned of minus1 + 1, from 1 to 16;
    // vps_max_num_reorder_pics past it a warning, refused only under
    // AV_EF_EXPLODE (ps.c:858-862).
    let buffering = r.ue_long().wrapping_add(1);
    let reorder = r.ue_long();
    r.ue_long();
    if buffering > 16 || buffering == 0 {
      return INVALID;
    }
    warned |= reorder > buffering - 1;
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
      // Each set's HRD parameters zeroed (`av_calloc`).
      hevc_hrd(r, common, max_sub_layers, &mut [0; 7]);
    }
  }
  let mut ext = Extension {
    layers: 1,
    mask: 0,
    layer_id: 0,
    rep: RepFormat::default(),
  };
  if max_layers > 1 && r.bit() {
    // vps_extension_flag
    match vps_extension(
      r,
      &mut ext,
      max_layers,
      max_sub_layers,
      layer_sets,
      layer1_included,
    ) {
      Ok(()) => {}
      // "Broken VPS extension, treating as alpha video" where two layers,
      // the second's id and the auxiliary type were read; one layer
      // otherwise, "Ignoring unsupported VPS extension" (ps.c:921-939).
      Err(Unsupported::PatchWelcome) => {
        if !ext.alpha() {
          ext.layers = 1;
        }
      }
      Err(Unsupported::Invalid) => return INVALID,
    }
  }
  Ok(VpsRead {
    alpha: ext.alpha(),
    max_sub_layers: max_sub_layers as u8,
    max_layers: max_layers as u8,
    nb_layers: ext.layers as u8,
    rep: ext.rep,
    warned,
  })
}

/// What `decode_vps_ext` set before it returned: the layers, the mask, the
/// second layer's `nuh_layer_id` and the representation format.
struct Extension {
  layers: i32,
  mask: u32,
  layer_id: u32,
  rep: RepFormat,
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
  // The representation format, each field kept as it is set (ps.c:709-734).
  ext.rep.width = r.bits(16) as u16;
  ext.rep.height = r.bits(16) as u16;
  if !r.bit() {
    // chroma_and_bit_depth_vps_present_flag
    return Err(Invalid);
  }
  let chroma_format_idc = r.bits(2) as usize;
  ext.rep.chroma_format_idc = chroma_format_idc as u8;
  if chroma_format_idc == 3 {
    ext.rep.separate_colour_plane = r.bit();
  }
  let luma = r.bits(4) + 8;
  let chroma = r.bits(4) + 8;
  ext.rep.bit_depth_luma = luma as u8;
  if luma > 16 || chroma > 16 || luma != chroma {
    return Err(PatchWelcome);
  }
  if r.bit() {
    // conformance_window_vps_flag: offsets in chroma units that must leave a
    // picture.
    ext.rep.window = read_window(
      r,
      chroma_format_idc,
      i64::from(ext.rep.width),
      i64::from(ext.rep.height),
    )
    .ok_or(Invalid)?;
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
  parse_ptl_profile(r, profile_present, max_sub_layers).is_some()
}

/// [`parse_ptl`], answering the general `profile_idc` it stores — 0 where
/// the profile is not present, `memset` (ps.c:343-345) — or `None` where it
/// fails. A `profile_idc` of 0 takes the first compatibility flag set after
/// the first (`decode_profile_tier_level`, ps.c:285-290).
fn parse_ptl_profile(r: &mut Reader<'_>, profile_present: bool, max_sub_layers: i32) -> Option<u8> {
  let common = |r: &mut Reader<'_>| {
    if r.left() < 88 {
      return false;
    }
    r.skip(88);
    true
  };
  let mut profile_idc = 0u8;
  if profile_present {
    if r.left() < 88 {
      return None;
    }
    r.bits(2); // general_profile_space
    r.bit(); // general_tier_flag
    profile_idc = r.bits(5) as u8;
    for flag in 0..32u8 {
      if r.bit() && profile_idc == 0 && flag > 0 {
        profile_idc = flag;
      }
    }
    // The source and constraint flags, 48 bits whichever profile it names.
    r.skip(48);
  }
  let sub_layers = (max_sub_layers - 1).max(0) as usize;
  if r.left() < 8 + if sub_layers > 0 { 16 } else { 0 } {
    return None;
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
      return None;
    }
    if level {
      if r.left() < 8 {
        return None;
      }
      r.bits(8);
    }
  }
  Some(profile_idc)
}

/// **`decode_hrd`** (hevc/ps.c:401-467), whose answer neither the video
/// parameter set's reading nor the VUI's looks at (ps.c:909-910, 1047-1048):
/// a sub-layer counting more than 32 CPBs stops it there. `cpb_cnt` is the
/// parameters' `cpb_cnt_minus1`, which a low-delay sub-layer reads none of
/// and keeps — zeroed with the parameters, and kept across a VUI read again
/// from its timing information, whose `sps->hdr` the retry does not restore
/// (ps.c:1032-1033).
fn hevc_hrd(r: &mut Reader<'_>, common: bool, max_sub_layers: i32, cpb_cnt: &mut [u8; 7]) {
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
  for count in cpb_cnt.iter_mut().take(max_sub_layers.max(0) as usize) {
    let fixed_general = r.bit();
    let fixed_within = !fixed_general && r.bit();
    let mut low_delay = false;
    if fixed_general || fixed_within {
      r.ue_long(); // elemental_duration_in_tc_minus1
    } else {
      low_delay = r.bit();
    }
    if !low_delay {
      let minus1 = r.ue_long();
      if minus1 > 31 {
        return;
      }
      *count = minus1 as u8;
    }
    let cpbs = u32::from(*count) + 1;
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

// ---------------------------------------------------------------------------
//  HEVC sequence and picture parameter sets
// ---------------------------------------------------------------------------

/// What `ff_hevc_parse_sps` stores of a sequence parameter set that a
/// picture parameter set read after it is read under
/// ([`hevc_pps`]), and whether FFmpeg stored it with a warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HevcSps {
  /// `sps_seq_parameter_set_id`.
  pub(super) id: u8,
  bit_depth: i32,
  /// 0 for a set of the second layer, which takes its format from the video
  /// parameter set's and never sets this (hevc/ps.c:1296-1324).
  bit_depth_chroma: i32,
  /// The general `profile_idc`, 0 for a set of the second layer, which
  /// reads no profile (ps.c:1280-1288).
  profile_idc: u8,
  log2_diff_max_min_coding_block_size: u32,
  log2_ctb_size: u32,
  ctb_width: i32,
  ctb_height: i32,
  /// Whether FFmpeg stored it with a warning that `AV_EF_EXPLODE` turns into
  /// its refusal: reordered pictures past the decoded picture buffer
  /// (`sps_max_num_reorder_pics`, ps.c:1416-1424), or an output window that
  /// leaves no picture (1640-1654), which the conformance window's own test
  /// rules out where the default display window is not applied — FFmpeg's
  /// default, which this crate keeps (`apply_defdispwin`,
  /// hevc/hevcdec.c:4204-4205; ps.c:75-80, 1389, 1633-1638).
  pub(super) warned: bool,
}

/// A sequence parameter set FFmpeg refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SpsRefused {
  /// Whether the error is other than `AVERROR_INVALIDDATA`, which ends a
  /// decoder's reading of the packet (`decode_nal_unit`,
  /// hevc/hevcdec.c:3665-3672).
  pub(super) aborts: bool,
  /// The set's id, where the reading reached it.
  pub(super) id: Option<u8>,
}

/// **`ff_hevc_parse_sps`** (hevc/ps.c:1239-1718) from the reader past the
/// set's `sps_video_parameter_set_id`, for a unit of `nuh_layer_id` `layer`,
/// against `vps`, the video parameter set the decoder holds under that id:
/// what FFmpeg stores ([`HevcSps`]), or its refusal. The decoder's context is
/// the one this crate opens: `err_recognition` 0, no
/// `AV_CODEC_FLAG2_IGNORE_CROP`, `apply_defdispwin` 0. The C widths of the
/// fields its tests compare are kept: `log2_max_poc_lsb` and the
/// `log2_*_size`s `unsigned`, a sub-layer's buffering and reordering `int`,
/// `num_long_term_ref_pics_sps` an `uint8_t`.
pub(super) fn hevc_sps(
  r: &mut Reader<'_>,
  layer: u8,
  vps: &VpsRead,
) -> Result<HevcSps, SpsRefused> {
  let invalid = |id| Err(SpsRefused { aborts: false, id });
  let aborts = |id| Err(SpsRefused { aborts: true, id });
  let mut max_sub_layers = r.bits(3) as i32 + 1;
  // A set of a layer above the base whose sub-layers read 8 takes the
  // video parameter set's (ps.c:1262-1278).
  let multi_layer = layer > 0 && max_sub_layers == 8;
  if multi_layer {
    max_sub_layers = i32::from(vps.max_sub_layers);
  }
  if max_sub_layers > i32::from(vps.max_sub_layers) {
    return invalid(None);
  }
  let mut profile_idc = 0;
  if !multi_layer {
    r.bit(); // sps_temporal_id_nesting_flag
    // `parse_ptl`'s -1 is the set's error (ps.c:1283-1284).
    match parse_ptl_profile(r, true, max_sub_layers) {
      Some(idc) => profile_idc = idc,
      None => return aborts(None),
    }
  }
  let id = r.ue_long();
  if id >= 16 {
    return invalid(None);
  }
  let at = Some(id as u8);
  let (chroma_format_idc, bit_depth, bit_depth_chroma, width, height, window);
  if multi_layer {
    // "SPS %d references an unsupported VPS extension", AVERROR(ENOSYS);
    // a representation format other than the first, AVERROR_PATCHWELCOME
    // (ps.c:1296-1309).
    if vps.nb_layers == 1 {
      return aborts(at);
    }
    if r.bit() && r.bits(8) != 0 {
      return aborts(at);
    }
    let rep = vps.rep;
    chroma_format_idc = if rep.separate_colour_plane {
      0
    } else {
      i32::from(rep.chroma_format_idc)
    };
    bit_depth = i32::from(rep.bit_depth_luma);
    bit_depth_chroma = 0;
    width = i32::from(rep.width);
    height = i32::from(rep.height);
    // `av_image_check_size`'s AVERROR(EINVAL).
    if !image_size_valid(width as u32, height as u32) {
      return aborts(at);
    }
    window = rep.window;
  } else {
    let format = r.ue_long();
    if format > 3 {
      return invalid(at);
    }
    // separate_colour_plane_flag: coded as monochrome.
    chroma_format_idc = if format == 3 && r.bit() {
      0
    } else {
      format as i32
    };
    width = r.ue_long() as i32;
    height = r.ue_long() as i32;
    if !image_size_valid(width as u32, height as u32) {
      return aborts(at);
    }
    window = if r.bit() {
      // conformance_window_flag
      match read_window(
        r,
        chroma_format_idc as usize,
        i64::from(width),
        i64::from(height),
      ) {
        Some(window) => window,
        None => return invalid(at),
      }
    } else {
      [0; 4]
    };
    bit_depth = r.ue_31() + 8;
    if bit_depth > 16 {
      return invalid(at);
    }
    bit_depth_chroma = r.ue_31() + 8;
    if bit_depth_chroma > 16 || (chroma_format_idc != 0 && bit_depth_chroma != bit_depth) {
      return invalid(at);
    }
  }
  // `map_pixel_format` (ps.c:1190-1237): 8, 9, 10 or 12 bits.
  if !matches!(bit_depth, 8 | 9 | 10 | 12) {
    return invalid(at);
  }
  let log2_max_poc_lsb = r.ue_long().wrapping_add(4);
  if log2_max_poc_lsb > 16 {
    return invalid(at);
  }
  let mut warned = false;
  if !multi_layer {
    let start = if r.bit() { 0 } else { max_sub_layers - 1 };
    for _ in start..max_sub_layers {
      let buffering = r.ue_long().wrapping_add(1) as i32;
      let reorder = r.ue_long() as i32;
      r.ue_long(); // sps_max_latency_increase_plus1
      if buffering as u32 > 16 {
        return invalid(at);
      }
      // "sps_max_num_reorder_pics out of range": the buffering raised to
      // fit, refused only under AV_EF_EXPLODE or past the buffer's 16
      // (ps.c:1416-1424).
      if reorder > buffering - 1 {
        if reorder > 15 {
          return invalid(at);
        }
        warned = true;
      }
    }
  }
  let log2_min_cb_size = r.ue_long().wrapping_add(3);
  let log2_diff_max_min_cb = r.ue_long();
  let log2_min_tb_size = r.ue_long().wrapping_add(2);
  let log2_diff_max_min_tb = r.ue_long();
  let log2_max_trafo_size = log2_diff_max_min_tb.wrapping_add(log2_min_tb_size);
  if !(3..=30).contains(&log2_min_cb_size)
    || log2_diff_max_min_cb > 30
    || log2_min_tb_size >= log2_min_cb_size
    || log2_min_tb_size < 2
    || log2_diff_max_min_tb > 30
  {
    return invalid(at);
  }
  let depth_inter = r.ue_long() as i32;
  let depth_intra = r.ue_long() as i32;
  if r.bit() {
    // scaling_list_enabled_flag; sps_infer_scaling_list_flag, read on the
    // second layer alone, AVERROR_PATCHWELCOME (ps.c:1473-1480).
    if multi_layer && r.bit() {
      return aborts(at);
    }
    if r.bit() && !scaling_list_data(r) {
      return invalid(at);
    }
  }
  r.bit(); // amp_enabled_flag
  r.bit(); // sample_adaptive_offset_enabled_flag
  if r.bit() {
    // pcm_enabled_flag: its sample bit depths within the set's.
    let luma = r.bits(4) + 1;
    let chroma = r.bits(4) + 1;
    r.ue_long(); // log2_min_pcm_luma_coding_block_size_minus3
    r.ue_long(); // log2_diff_max_min_pcm_luma_coding_block_size
    if luma.max(chroma) as i32 > bit_depth {
      return invalid(at);
    }
    r.bit(); // pcm_loop_filter_disabled_flag
  }
  let short_term_sets = r.ue_long();
  if short_term_sets > 64 {
    return invalid(at);
  }
  let mut num_delta_pocs = [0u8; 64];
  for index in 0..short_term_sets as usize {
    match short_term_rps(r, index, &num_delta_pocs) {
      Some(count) => num_delta_pocs[index] = count,
      None => return invalid(at),
    }
  }
  if r.bit() {
    // long_term_ref_pics_present_flag; the count an `uint8_t`.
    let count = r.ue_long() as u8;
    if count > 32 {
      return invalid(at);
    }
    for _ in 0..count {
      r.skip_field(log2_max_poc_lsb); // lt_ref_pic_poc_lsb_sps
      r.bit(); // used_by_curr_pic_lt_sps_flag
    }
  }
  r.bit(); // sps_temporal_mvp_enabled_flag
  r.bit(); // strong_intra_smoothing_enabled_flag
  if r.bit() {
    hevc_vui(r, max_sub_layers);
  }
  if r.bit() {
    // sps_extension_present_flag
    let range = r.bit();
    let multilayer = r.bit();
    let three_d = r.bit();
    let scc = r.bit();
    r.skip(4); // sps_extension_4bits
    if range {
      r.skip(9);
    }
    if multilayer {
      r.skip(1); // inter_view_mv_vert_constraint_flag
    }
    if three_d {
      for view in 0..2 {
        r.skip(2 + u64::from(view == 1));
        r.ue_long(); // log2_ivmc_sub_pb_size_minus3
        r.skip(if view == 0 { 4 } else { 5 });
      }
    }
    if scc {
      r.bit(); // curr_pic_ref_enabled_flag
      if r.bit() {
        // palette_mode_enabled_flag
        r.ue(); // palette_max_size
        r.ue(); // delta_palette_max_predictor_size
        if r.bit() {
          // sps_palette_predictor_initializers_present_flag, its count an
          // `int` of `get_ue_golomb` plus 1.
          let count = r.ue().wrapping_add(1);
          if count > 128 {
            return invalid(at);
          }
          let components = if chroma_format_idc == 0 { 1 } else { 3 };
          for component in 0..components {
            let depth = if component == 0 {
              bit_depth
            } else {
              bit_depth_chroma
            };
            for _ in 0..count.max(0) {
              r.skip_field(depth as u32);
            }
          }
        }
      }
      r.bits(2); // motion_vector_resolution_control_idc
      r.bit(); // intra_boundary_filtering_disabled_flag
    }
  }
  // The output window, the conformance window alone: a warning, refused
  // only under AV_EF_EXPLODE (ps.c:1640-1654). `unsigned` arithmetic.
  let [left, right, top, bottom] = window;
  if left >= (i32::MAX as u32).wrapping_sub(right)
    || top >= (i32::MAX as u32).wrapping_sub(bottom)
    || left.wrapping_add(right) >= width as u32
    || top.wrapping_add(bottom) >= height as u32
  {
    warned = true;
  }
  let log2_ctb_size = log2_min_cb_size.wrapping_add(log2_diff_max_min_cb);
  if !(4..=6).contains(&log2_ctb_size) {
    return invalid(at);
  }
  let ctb_width = (width + (1 << log2_ctb_size) - 1) >> log2_ctb_size;
  let ctb_height = (height + (1 << log2_ctb_size) - 1) >> log2_ctb_size;
  let mask = (1u32 << log2_min_cb_size) - 1;
  if width as u32 & mask != 0 || height as u32 & mask != 0 {
    return invalid(at);
  }
  // `int`s compared with an `unsigned` difference.
  let depth = log2_ctb_size - log2_min_tb_size;
  if depth_inter as u32 > depth
    || depth_intra as u32 > depth
    || log2_max_trafo_size > log2_ctb_size.min(5)
  {
    return invalid(at);
  }
  // "Overread SPS": refused (ps.c:1711-1715).
  if r.left() < 0 {
    return invalid(at);
  }
  Ok(HevcSps {
    id: id as u8,
    bit_depth,
    bit_depth_chroma,
    profile_idc,
    log2_diff_max_min_coding_block_size: log2_diff_max_min_cb,
    log2_ctb_size,
    ctb_width,
    ctb_height,
    warned,
  })
}

/// **`ff_hevc_decode_short_term_rps`** (hevc/ps.c:113-260) for the
/// `index`-th set of a sequence parameter set's, `num_delta_pocs` the counts
/// of those before it: its own count (`uint8_t`), `None` where FFmpeg
/// refuses it. Predicted from the set before it — every set but the first —
/// it reads two flags at most for each of that set's pictures and one more;
/// written out, its counts and each picture's distance. `abs_delta_rps` is an
/// `uint16_t` and `num_negative_pics` an `uint8_t`, as there.
fn short_term_rps(r: &mut Reader<'_>, index: usize, num_delta_pocs: &[u8; 64]) -> Option<u8> {
  if index > 0 && r.bit() {
    // inter_ref_pic_set_prediction_flag
    let reference = num_delta_pocs[index - 1];
    r.bit(); // delta_rps_sign
    let abs_delta_rps = r.ue_long().wrapping_add(1) as u16;
    if abs_delta_rps > 32768 {
      return None;
    }
    let mut count = 0u32;
    for _ in 0..=reference {
      let used = r.bit();
      if used || r.bit() {
        count += 1;
      }
    }
    return (count < 32).then_some(count as u8);
  }
  let negative = r.ue_long() as u8;
  let positive = r.ue_long();
  if negative >= 16 || positive >= 16 {
    return None;
  }
  for _ in 0..u32::from(negative) + positive {
    let delta = r.ue_long().wrapping_add(1) as i32;
    if !(1..=32768).contains(&delta) {
      return None;
    }
    r.bit(); // used_by_curr_pic_flag
  }
  Some(negative + positive as u8)
}

/// **`scaling_list_data`** (hevc/ps.c:1113-1188), as far as its verdict goes:
/// `false` where FFmpeg refuses it — a list copied from one that is not
/// before it, a DC coefficient outside -7 to 247.
fn scaling_list_data(r: &mut Reader<'_>) -> bool {
  for size_id in 0..4u32 {
    let step = if size_id == 3 { 3 } else { 1 };
    let mut matrix_id = 0u32;
    while matrix_id < 6 {
      if r.bit() {
        // scaling_list_pred_mode_flag: the list itself.
        if size_id > 1 && !(-7..=247).contains(&r.se()) {
          return false;
        }
        for _ in 0..64.min(1 << (4 + (size_id << 1))) {
          r.se(); // scaling_list_delta_coef
        }
      } else {
        // A copy of an earlier list, `unsigned` arithmetic.
        let delta = r.ue_long().wrapping_mul(step);
        if delta != 0 && matrix_id < delta {
          return false;
        }
      }
      matrix_id += step;
    }
  }
  true
}

/// **`decode_vui`** (hevc/ps.c:961-1081): the VUI of a sequence parameter
/// set, which fails nothing, but whose reading FFmpeg starts again from the
/// default display window as from its timing information where the timing
/// information or the bitstream restriction runs short, or the VUI runs to
/// the end of the set (1027-1036, 1051-1060, 1072-1080): the reader goes back,
/// once. The default display window is read for its length alone
/// (`read_window`'s answer is not looked at, 1003-1004), and a window that
/// opens on 21 bits reading 0x100000, with 68 bits or more left, is taken as
/// absent (997-1001).
fn hevc_vui(r: &mut Reader<'_>, max_sub_layers: i32) {
  common_vui(r);
  r.bit(); // neutral_chroma_indication_flag
  r.bit(); // field_seq_flag
  r.bit(); // frame_field_info_present_flag
  let backup = r.clone();
  if !(r.left() >= 68 && r.peek(21) == 0x10_0000) && r.bit() {
    // default_display_window_flag
    for _ in 0..4 {
      r.ue_long();
    }
  }
  let mut cpb_cnt = [0u8; 7];
  let mut retried = false;
  loop {
    if r.bit() {
      // vui_timing_info_present_flag
      if r.left() < 66 && !retried {
        *r = backup.clone();
        retried = true;
        continue;
      }
      r.bits(32); // vui_num_units_in_tick
      r.bits(32); // vui_time_scale
      if r.bit() {
        r.ue_long(); // vui_num_ticks_poc_diff_one_minus1
      }
      if r.bit() {
        hevc_hrd(r, true, max_sub_layers, &mut cpb_cnt);
      }
    }
    if r.bit() {
      // bitstream_restriction_flag
      if r.left() < 8 && !retried {
        *r = backup.clone();
        retried = true;
        continue;
      }
      r.skip(3);
      for _ in 0..5 {
        r.ue_long();
      }
    }
    if r.left() < 1 && !retried {
      *r = backup.clone();
      retried = true;
      continue;
    }
    return;
  }
}

/// What `ff_hevc_decode_nal_pps` does with a picture parameter set it reads
/// past its ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HevcPps {
  /// Stored; `past_end` where its reading ran past its payload — "Overread
  /// PPS", a warning, the set stored with its last fields read off the bytes
  /// after it (hevc/ps.c:2458-2461).
  Stored { past_end: bool },
  /// Refused, `AVERROR_INVALIDDATA`.
  Refused,
  /// Not stored, and no error: more default reference pictures than 15
  /// leave the set at `goto err` with `ret` still 0 (ps.c:2275-2280).
  Dropped,
}

/// **`ff_hevc_decode_nal_pps`** (hevc/ps.c:2201-2471) from the reader past
/// the set's `pps_seq_parameter_set_id`, against `sps`, the sequence
/// parameter set the decoder holds under it, and `vps_max_layers`, the
/// `vps_max_layers` of the video parameter set held under that one's id
/// (2260-2261). Every field is read as FFmpeg reads it, its extensions among
/// them; `setup_pps`, which reads nothing and fails only an allocation, is
/// taken to pass (2069-2199). The C widths kept: `num_ref_loc_offsets`,
/// `num_cm_ref_layers`, the colour mapping bit depths and
/// `pps_num_palette_predictor_initializers` `uint8_t`s, the colour transform
/// offsets `int8_t`s.
pub(super) fn hevc_pps(r: &mut Reader<'_>, sps: &HevcSps, vps_max_layers: u8) -> HevcPps {
  use HevcPps::{Dropped, Refused};
  r.bit(); // dependent_slice_segments_enabled_flag
  r.bit(); // output_flag_present_flag
  r.bits(3); // num_extra_slice_header_bits
  r.bit(); // sign_data_hiding_enabled_flag
  r.bit(); // cabac_init_present_flag
  let l0 = r.ue_31() + 1;
  let l1 = r.ue_31() + 1;
  if l0 >= 16 || l1 >= 16 {
    return Dropped;
  }
  r.se(); // init_qp_minus26
  r.bit(); // constrained_intra_pred_flag
  let transform_skip = r.bit();
  let depth = if r.bit() {
    // cu_qp_delta_enabled_flag
    r.ue_long() as i32
  } else {
    0
  };
  if depth < 0 || depth as u32 > sps.log2_diff_max_min_coding_block_size {
    return Refused;
  }
  if !(-12..=12).contains(&r.se()) || !(-12..=12).contains(&r.se()) {
    // pps_cb_qp_offset, pps_cr_qp_offset
    return Refused;
  }
  r.bit(); // pps_slice_chroma_qp_offsets_present_flag
  r.bit(); // weighted_pred_flag
  r.bit(); // weighted_bipred_flag
  r.bit(); // transquant_bypass_enabled_flag
  let tiles = r.bit();
  r.bit(); // entropy_coding_sync_enabled_flag
  if tiles {
    let columns = r.ue();
    let rows = r.ue();
    if columns < 0 || columns >= sps.ctb_width || rows < 0 || rows >= sps.ctb_height {
      return Refused;
    }
    if !r.bit() {
      // uniform_spacing_flag clear: each column's and row's width but the
      // last, which must leave it one.
      for (count, ctbs) in [(columns, sps.ctb_width), (rows, sps.ctb_height)] {
        let mut sum = 0u64;
        for _ in 0..count {
          sum += u64::from(r.ue_long().wrapping_add(1));
        }
        if sum >= ctbs as u64 {
          return Refused;
        }
      }
    }
    r.bit(); // loop_filter_across_tiles_enabled_flag
  }
  r.bit(); // pps_loop_filter_across_slices_enabled_flag
  if r.bit() {
    // deblocking_filter_control_present_flag
    r.bit(); // deblocking_filter_override_enabled_flag
    if !r.bit() {
      // pps_deblocking_filter_disabled_flag clear
      let beta = r.se();
      let tc = r.se();
      if !(-6..=6).contains(&beta) || !(-6..=6).contains(&tc) {
        return Refused;
      }
    }
  }
  if r.bit() && !scaling_list_data(r) {
    // pps_scaling_list_data_present_flag
    return Refused;
  }
  r.bit(); // lists_modification_present_flag
  if r.ue_long() > sps.log2_ctb_size {
    // log2_parallel_merge_level_minus2
    return Refused;
  }
  r.bit(); // slice_segment_header_extension_present_flag
  if r.bit() {
    // pps_extension_present_flag
    let range = r.bit();
    let multilayer = r.bit();
    let three_d = r.bit();
    let scc = r.bit();
    r.skip(4); // pps_extension_4bits
    // The range extension read only under a profile of 4 or more,
    // AV_PROFILE_HEVC_REXT (ps.c:2433-2436).
    if sps.profile_idc >= 4 && range && !pps_range_extension(r, transform_skip, sps) {
      return Refused;
    }
    if multilayer && !pps_multilayer_extension(r, vps_max_layers) {
      return Refused;
    }
    if three_d {
      pps_3d_extension(r);
    }
    if scc && !pps_scc_extension(r, sps) {
      return Refused;
    }
  }
  HevcPps::Stored {
    past_end: r.left() < 0,
  }
}

/// `pps_range_extensions` (hevc/ps.c:1975-2013): `false` where refused.
fn pps_range_extension(r: &mut Reader<'_>, transform_skip: bool, sps: &HevcSps) -> bool {
  if transform_skip {
    r.ue_31(); // log2_max_transform_skip_block_size_minus2
  }
  r.bit(); // cross_component_prediction_enabled_flag
  if r.bit() {
    // chroma_qp_offset_list_enabled_flag
    r.ue_31(); // diff_cu_chroma_qp_offset_depth
    let length = r.ue_31();
    if length > 5 {
      return false;
    }
    for _ in 0..=length {
      r.se(); // cb_qp_offset_list
      r.se(); // cr_qp_offset_list
    }
  }
  let luma = r.ue_31();
  let chroma = r.ue_31();
  luma <= (sps.bit_depth - 10).max(0) && chroma <= (sps.bit_depth_chroma - 10).max(0)
}

/// `pps_multilayer_extension` (hevc/ps.c:1880-1927): `false` where refused.
fn pps_multilayer_extension(r: &mut Reader<'_>, vps_max_layers: u8) -> bool {
  r.bit(); // poc_reset_info_present_flag
  if r.bit() {
    r.bits(6); // pps_scaling_list_ref_layer_id
  }
  let offsets = r.ue() as u8;
  if i32::from(offsets) > i32::from(vps_max_layers) - 1 {
    return false;
  }
  for _ in 0..offsets {
    r.bits(6); // ref_loc_offset_layer_id
    for _ in 0..2 {
      // scaled_ref_layer_offset_present_flag, ref_region_offset_present_flag
      if r.bit() {
        for _ in 0..4 {
          r.se_long();
        }
      }
    }
    if r.bit() {
      // resample_phase_set_present_flag
      r.ue_31();
      r.ue_31();
      r.ue();
      r.ue();
    }
  }
  // colour_mapping_enabled_flag
  !r.bit() || colour_mapping_table(r)
}

/// `colour_mapping_table` (hevc/ps.c:1844-1878): `false` where refused.
fn colour_mapping_table(r: &mut Reader<'_>) -> bool {
  let layers = r.ue().wrapping_add(1) as u8;
  if layers > 62 {
    return false;
  }
  for _ in 0..layers {
    r.bits(6); // cm_ref_layer_id
  }
  let depth = r.bits(2);
  let part_num_y = 1u32 << r.bits(2);
  let mut bit_depths = [0u8; 4];
  for depth in &mut bit_depths {
    *depth = r.ue().wrapping_add(8) as u8;
  }
  let [luma_in, chroma_in, luma_out, chroma_out] = bit_depths;
  if luma_out < luma_in || chroma_out < chroma_in {
    return false;
  }
  let quant = r.bits(2) as i32;
  let flc = r.bits(2) as i32 + 1;
  if depth == 1 {
    r.se_long(); // cm_adapt_threshold_u_delta
    r.se_long(); // cm_adapt_threshold_v_delta
  }
  let residual = (10 + i32::from(luma_in) - i32::from(luma_out) - quant - flc).max(0) as u32;
  colour_mapping_octants(r, 0, depth, part_num_y, residual);
  true
}

/// `colour_mapping_octants` (hevc/ps.c:1807-1842).
fn colour_mapping_octants(r: &mut Reader<'_>, at: u32, depth: u32, part_num_y: u32, residual: u32) {
  if at < depth && r.bit() {
    // split_octant_flag
    for _ in 0..8 {
      colour_mapping_octants(r, at + 1, depth, part_num_y, residual);
    }
    return;
  }
  for _ in 0..part_num_y * 4 {
    if r.bit() {
      // coded_res_flag
      for _ in 0..3 {
        let quotient = r.ue_long();
        let remainder = if residual != 0 { r.bits(residual) } else { 0 };
        if quotient != 0 || remainder != 0 {
          r.bit(); // res_coeff_s
        }
      }
    }
  }
}

/// `pps_3d_extension` (hevc/ps.c:1951-1973), which refuses nothing: a flag
/// for each depth value, read one at a time there, skipped at once here.
fn pps_3d_extension(r: &mut Reader<'_>) {
  if !r.bit() {
    // dlts_present_flag
    return;
  }
  let layers = r.bits(6) + 1;
  let bits = r.bits(4) + 8;
  for _ in 0..layers {
    // dlt_flag set, dlt_pred_flag clear
    if r.bit() && !r.bit() {
      if r.bit() {
        r.skip(1 << bits); // dlt_value_flag
      } else {
        delta_dlt(r, bits);
      }
    }
  }
}

/// `delta_dlt` (hevc/ps.c:1929-1949).
fn delta_dlt(r: &mut Reader<'_>, bits: u32) {
  let values = r.bits(bits);
  if values == 0 {
    return;
  }
  let max_diff = if values > 1 { r.bits(bits) } else { 0 };
  let min_diff_minus1 = if values > 2 && max_diff != 0 {
    r.bits(log2(max_diff) + 1) as i32
  } else {
    -1
  };
  // `unsigned` against `int`.
  let floor = (min_diff_minus1 + 1) as u32;
  if max_diff > floor {
    let length = log2(max_diff - floor) + 1;
    r.skip(u64::from(values - 1) * u64::from(length));
  }
}

/// `pps_scc_extension` (hevc/ps.c:2015-2067): `false` where refused.
fn pps_scc_extension(r: &mut Reader<'_>, sps: &HevcSps) -> bool {
  r.bit(); // pps_curr_pic_ref_enabled_flag
  if r.bit() {
    // residual_adaptive_colour_transform_enabled_flag
    r.bit(); // pps_slice_act_qp_offsets_present_flag
    // Each an `int8_t` of a `get_se_golomb` less an `unsigned`.
    let mut offsets = [0i8; 3];
    for (offset, less) in offsets.iter_mut().zip([5u32, 5, 3]) {
      *offset = (r.se() as u32).wrapping_sub(less) as i8;
    }
    if offsets.iter().any(|&offset| offset <= -12 || offset >= 12) {
      return false;
    }
  }
  if r.bit() {
    // pps_palette_predictor_initializers_present_flag
    let count = r.ue() as u8;
    if count > 0 {
      if count > 128 {
        return false;
      }
      let monochrome = r.bit();
      let luma = r.ue_31().wrapping_add(8) as u8;
      if i32::from(luma) != sps.bit_depth {
        return false;
      }
      let mut chroma = 0u8;
      if !monochrome {
        chroma = r.ue_31().wrapping_add(8) as u8;
        if i32::from(chroma) != sps.bit_depth_chroma {
          return false;
        }
      }
      for depth in [luma, chroma, chroma]
        .into_iter()
        .take(if monochrome { 1 } else { 3 })
      {
        for _ in 0..count {
          r.skip_field(u32::from(depth));
        }
      }
    }
  }
  true
}

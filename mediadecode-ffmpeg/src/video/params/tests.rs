use super::*;

/// The xorshift generator the reader's vectors were drawn with.
struct Xorshift(u64);

impl Xorshift {
  fn next(&mut self) -> u64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    self.0
  }
}

/// What FFmpeg 9.0.1's own reader answered for the first 24 draws below —
/// its `get_bits.h` and `golomb.h` built from the release tarball, safe and
/// 64-bit, each answer an operation, its value and the index after it.
const FFMPEG_ANSWERS: [&str; 24] = [
  "T0 3:-1@23 3:-1@36 5:3@41 9:1064@41 0:112@48",
  "T1 5:0@1 8:-1@4 7:0@5 3:-1@22 0:1048815@44 7:0@45 4:0@46 2:32768@49 6:4294967295@49 8:-2147483648@49 9:4294967288@49 1:0@49 8:-2147483648@49",
  "T2 8:0@1 9:1083@1 9:1083@1 2:4832043@24 1:1@25 4:12@32 0:32720@50 8:1@53 8:-1@56 8:9@65 1:1@66 7:-1@69 5:1@72 4:2@75 7:-1@78 1:0@79 3:-1@92 3:-1@92 9:992@92 7:0@92 6:0@92 5:0@92 1:1@92",
  "T3 1:0@1 8:1@4 8:0@5 3:-1@26 3:-1@60 8:-2147483648@60 4:-1094995529@60 0:0@60 2:0@60 3:-1@60 7:0@60 7:0@60 7:0@60 8:-2147483648@60 3:-1@60 4:-1094995529@60",
  "T4 5:32@17",
  "T5 7:0@1 2:469@13 3:-1@14 7:-1@17 7:1@20 9:130@20 9:130@20 1:0@21 4:0@22 1:1@23 4:0@24 6:0@25 9:125@25 0:7839@39 1:0@40",
  "T6 7:0@1 2:39936@17 6:4294967295@17 3:-1@17 0:0@17 5:32@17 5:32@17 1:0@17 9:4294967288@17 8:-2147483648@17 1:0@17 9:4294967288@17 1:0@17 3:-1@17 1:0@17 6:4294967295@17 1:0@17 4:-1094995529@17 8:-2147483648@17 9:4294967288@17 7:0@17 0:0@17 9:4294967288@17 9:4294967288@17",
  "T7 5:5@5 6:0@6 6:3@11 1:1@12 7:3@17 0:82959@20 6:0@20 1:0@20 5:7@20",
  "T8 1:0@1 5:32@20 0:19040@37 4:107@47 6:0@47 2:1@47 6:0@47 7:0@47 0:2048@47 9:992@47 4:0@47 4:0@47 3:-1@47 7:0@47 0:16@47 8:0@47 3:-1@47 2:8388608@47 9:992@47 5:0@47",
  "T9 2:0@4",
  "T10 9:1026@0 0:4751360@23 2:12@29 8:0@30 5:1@33 7:0@34 2:0@34 2:0@34 3:-1@34",
  "T11 1:0@1 5:0@2 4:6@7 8:-1@10 2:197@18 5:1@21 6:10@28 0:7506@41 6:0@42 8:1@45 3:-1@71 7:0@72 4:1@75 5:0@76 3:-1@109",
  "T12 7:0@1 9:121@1 4:2@4 3:-1@5 7:0@6 0:1@11 4:0@12 6:0@13 2:6991872@36 8:2608@61 9:61@61",
  "T13 8:1@3 7:1@6 3:-1@31 2:280811@52 2:233962391@59 3:-1@59 6:0@59 9:992@59 6:0@59 6:0@59 0:71@59 2:1150@59 4:0@59 9:992@59",
  "T14 1:0@1 6:136566@36 3:-1@37 0:768@38 5:0@38 2:384@38 0:25165824@38",
  "T15 9:1006@0 1:1@1 6:0@2 9:4@2 1:0@3 8:0@4 0:8388608@14 8:-2147483648@14 6:4294967295@14 6:4294967295@14 6:4294967295@14 7:0@14 5:32@14 7:0@14 5:32@14 8:-2147483648@14 3:-1@14",
  "T16 3:-1@20 3:-1@20 4:5@25 4:2@28 7:6@35 0:3@39 8:0@40 1:1@41 0:38@48 5:0@49 0:11305816@73 3:-1@75 0:134@90 8:2@95 8:-1@98 6:6@103 2:8720@109 4:7@109 4:7@109 5:7@109",
  "T17 0:758@10 2:19@10 8:0@10 5:0@10 9:992@10 3:-1@10",
  "T18 8:-1840@11 9:992@11 9:992@11 6:0@11 8:0@11 0:7538688@11 2:7538688@11 1:1@11 4:0@11",
  "T19 0:0@8 4:-1094995529@8 7:0@8",
  "T20 1:0@1 4:-1094995529@64 5:32@83 2:0@93 8:-2147483648@138 6:4294967295@138 1:0@138 5:32@138 4:-1094995529@138 0:0@138 1:0@138 6:4294967295@138 8:-2147483648@138 1:0@138 0:0@138",
  "T21 2:9216@22 6:0@22 0:0@22 0:7@22 1:0@22 3:-1@22 5:32@22",
  "T22 4:0@1 0:28672@13 0:0@13 7:0@13 1:0@13 9:4294967288@13 8:-2147483648@13 6:4294967295@13 8:-2147483648@13 1:0@13 7:0@13 9:4294967288@13 7:0@13 1:0@13 6:4294967295@13 9:4294967288@13 1:0@13 9:4294967288@13 7:0@13 5:32@13 0:0@13",
  "T23 5:32@17 6:13303807@51 3:-1@51 5:32@51 9:4294967288@51 7:0@51 8:-2147483648@51 6:4294967295@51 6:4294967295@51 5:32@51 1:0@51 3:-1@51 4:-1094995529@51 7:0@51 1:0@51 4:-1094995529@51 2:0@51 9:4294967288@51 4:-1094995529@51 9:4294967288@51",
];

/// LAW (Codex R15, [high], the reader under rows 1 and 4): **the reader
/// reads as FFmpeg's does, past the end too.** 24 draws, each a buffer of 1
/// to 24 bytes, a size of up to its bits, and up to 24 operations —
/// `get_bits`, `get_bits1`, `get_bits_long`, `skip_bits_long`, the five
/// exp-Golomb readers, `show_bits1` with `get_bits_left` — answer what
/// FFmpeg 9.0.1's own reader answered: every value, every index, a long
/// code's `AVERROR_INVALIDDATA`, the index held eight bits past the end.
/// (Over 200 000 such draws, a scratch build of FFmpeg's reader and this one
/// answered alike.)
#[test]
fn the_reader_reads_as_ffmpegs_does() {
  let mut rng = Xorshift(0x9E37_79B9_7F4A_7C15);
  for (draw, expected) in FFMPEG_ANSWERS.iter().enumerate() {
    let len = 1 + (rng.next() % 24) as usize;
    let mut buf = [0u8; 24];
    let zero_bias = rng.next() % 4;
    for byte in buf.iter_mut().take(len) {
      let r = rng.next();
      *byte = if r % 4 < zero_bias { 0 } else { (r >> 8) as u8 };
    }
    let size = rng.next() % (len as u64 * 8 + 1);
    let mut reader = Reader::new(&buf[..len], size);
    let mut answers = format!("T{draw}");
    for _ in 0..1 + rng.next() % 24 {
      let op = rng.next() % 10;
      let value: i64 = match op {
        0 => i64::from(reader.bits(1 + (rng.next() % 25) as u32)),
        1 => i64::from(reader.bit()),
        2 => i64::from(reader.bits(1 + (rng.next() % 32) as u32)),
        3 => {
          reader.skip(rng.next() % 40);
          -1
        }
        4 => i64::from(reader.ue()),
        5 => i64::from(reader.ue_31()),
        6 => i64::from(reader.ue_long()),
        7 => i64::from(reader.se()),
        8 => i64::from(reader.se_long()),
        _ => {
          i64::from((u32::from(reader.show_bit()) * 1000).wrapping_add(reader.left() as i32 as u32))
        }
      };
      answers.push_str(&format!(" {op}:{value}@{}", reader.count()));
    }
    assert_eq!(answers, *expected, "draw {draw}");
  }
}

/// LAW: **the exp-Golomb tables are golomb.c's**: a code of up to nine bits
/// read whole, a longer one given twice its leading zeros plus one bits and
/// the values 32 and 17 — the prefix `000001000` 31 and 16.
#[test]
fn the_golomb_tables_are_ffmpegs() {
  for (prefix, len, ue, se) in [
    (0usize, 19u8, 32u8, 17i8),
    (1, 17, 32, 17),
    (2, 15, 32, 17),
    (4, 13, 32, 17),
    (8, 11, 31, 16),
    (9, 11, 32, 17),
    (16, 9, 15, 8),
    (17, 9, 16, -8),
    (31, 9, 30, -15),
    (32, 7, 7, 4),
    (64, 5, 3, 2),
    (128, 3, 1, 1),
    (192, 3, 2, -1),
    (256, 1, 0, 0),
    (511, 1, 0, 0),
  ] {
    assert_eq!(
      (GOLOMB.len[prefix], GOLOMB.ue[prefix], GOLOMB.se[prefix]),
      (len, ue, se),
      "prefix {prefix:09b}"
    );
  }
}

/// An RBSP written a field at a time, most significant bit first.
#[derive(Default)]
struct Bits {
  bytes: Vec<u8>,
  used: u32,
}

impl Bits {
  fn put(&mut self, value: u64, n: u32) -> &mut Self {
    for shift in (0..n).rev() {
      if self.used.is_multiple_of(8) {
        self.bytes.push(0);
      }
      let bit = u8::from((value >> shift) & 1 == 1);
      let last = self.bytes.last_mut().expect("a byte");
      *last |= bit << (7 - self.used % 8);
      self.used += 1;
    }
    self
  }

  fn ue(&mut self, value: u32) -> &mut Self {
    let code = u64::from(value) + 1;
    let len = 64 - code.leading_zeros();
    self.put(0, len - 1).put(code, len)
  }

  fn se(&mut self, value: i32) -> &mut Self {
    let mapped = if value > 0 {
      2 * value as u32 - 1
    } else {
      2 * value.unsigned_abs()
    };
    self.ue(mapped)
  }

  /// The NAL unit of header byte `header`: these bits, the stop bit, and
  /// emulation prevention put in.
  fn unit(&mut self, header: u8) -> Vec<u8> {
    self.put(1, 1);
    while !self.used.is_multiple_of(8) {
      self.put(0, 1);
    }
    let mut unit = vec![header];
    let mut zeros = 0;
    for &byte in &self.bytes {
      if zeros >= 2 && byte <= 3 {
        unit.push(3);
        zeros = 0;
      }
      unit.push(byte);
      zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    unit
  }
}

/// A Constrained Baseline sequence parameter set of id `id` for a 128x96
/// picture: one reference frame, picture order type 2, no VUI — or `vui`
/// bits after its flag where given.
fn baseline_sps(id: u32, vui: Option<&dyn Fn(&mut Bits)>) -> Vec<u8> {
  let mut bits = Bits::default();
  bits.put(66, 8).put(0xc0, 8).put(30, 8).ue(id);
  bits
    .ue(0)
    .ue(2)
    .ue(1)
    .put(0, 1)
    .ue(7)
    .ue(5)
    .put(1, 1)
    .put(1, 1)
    .put(0, 1);
  match vui {
    Some(write) => {
      bits.put(1, 1);
      write(&mut bits);
    }
    None => {
      bits.put(0, 1);
    }
  }
  bits.unit(0x67)
}

/// A picture parameter set of id `id` referring to the sequence parameter
/// set `sps`, CAVLC, one slice group, one reference each way.
fn pps(id: u32, sps: u32) -> Vec<u8> {
  let mut bits = Bits::default();
  bits.ue(id).ue(sps).put(0, 1).put(0, 1).ue(0).ue(0).ue(0);
  bits
    .put(0, 1)
    .put(0, 2)
    .se(0)
    .se(0)
    .se(0)
    .put(1, 1)
    .put(0, 1)
    .put(0, 1);
  bits.unit(0x68)
}

/// An `avcC` record carrying `sps` and `pps`, four-byte NAL length fields.
fn avcc(sps: &[&[u8]], pps: &[&[u8]]) -> Vec<u8> {
  let first = sps.first().copied().unwrap_or(&[0x67, 66, 0xc0, 30]);
  let mut record = vec![
    1,
    first[1],
    first[2],
    first[3],
    0xff,
    0xe0 | sps.len() as u8,
  ];
  for unit in sps {
    record.extend_from_slice(&(unit.len() as u16).to_be_bytes());
    record.extend_from_slice(unit);
  }
  record.push(pps.len() as u8);
  for unit in pps {
    record.extend_from_slice(&(unit.len() as u16).to_be_bytes());
    record.extend_from_slice(unit);
  }
  record
}

/// Start-coded extradata carrying `units`.
fn annexb(units: &[&[u8]]) -> Vec<u8> {
  units
    .iter()
    .flat_map(|unit| [&[0, 0, 0, 1][..], unit].concat())
    .collect()
}

/// Whether FFmpeg's H.264 decoder, as this build links it, opens on
/// `record` with `AV_EF_EXPLODE` set, which makes a failure
/// `ff_h264_decode_extradata` answers a failed open (`h264_decode_init`,
/// h264dec.c:399-412) — a failure it answers: for an `avcC` entry's sets it
/// answers success whatever they do (`decode_extradata_ps_mp4` returns 0,
/// h264_parse.c:463), and with `AV_EF_EXPLODE` it does not retry them
/// escaped (h264_parse.c:427).
fn ffmpeg_opens_strictly_on(record: &[u8]) -> bool {
  use ffmpeg_next::ffi;
  // SAFETY: a context allocated for FFmpeg's own `h264`, given a copy of
  // `record` padded with zeros as libavcodec reads extradata, opened and
  // freed, its extradata with it.
  unsafe {
    let codec = ffi::avcodec_find_decoder(ffi::AVCodecID::AV_CODEC_ID_H264);
    assert!(!codec.is_null(), "FFmpeg's h264 is built in");
    let mut ctx = ffi::avcodec_alloc_context3(codec);
    assert!(!ctx.is_null(), "a context");
    let padded = record.len() + ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
    let data = ffi::av_mallocz(padded).cast::<u8>();
    assert!(!data.is_null(), "extradata allocated");
    core::ptr::copy_nonoverlapping(record.as_ptr(), data, record.len());
    (*ctx).extradata = data;
    (*ctx).extradata_size = record.len() as i32;
    (*ctx).err_recognition = ffi::AV_EF_EXPLODE;
    (*ctx).thread_count = 1;
    let opened = ffi::avcodec_open2(ctx, codec, core::ptr::null_mut()) >= 0;
    ffi::avcodec_free_context(&mut ctx);
    opened
  }
}

/// The record `hex` spells.
fn unhex(hex: &str) -> Vec<u8> {
  (0..hex.len())
    .step_by(2)
    .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
    .collect()
}

/// LAW (Codex R15, [high], row 1): **an H.264 extradata FFmpeg would reject
/// or apply only in part is rejected, with the reason; one it applies whole
/// is taken — backed by the FFmpeg this build links.**
///
/// - **The `avcC` record's own shape**, read by the mirror and by FFmpeg
///   opened strictly (`AV_EF_EXPLODE`): a well-formed record is taken and
///   opened on; one of five or six bytes, and one whose sequence or picture
///   parameter set runs past it, are rejected by name and fail the strict
///   open.
/// - **Its parameter sets**, each case in both packings: a well-formed pair,
///   and a sequence parameter set whose VUI is cut short (FFmpeg's third
///   reading lets the truncation stand), are taken; a sequence parameter set
///   of id 40 or of picture order type 3, a picture parameter set of id 300
///   or with a chroma QP offset of 13, and one referring to a sequence
///   parameter set the record does not carry — or met before it — are
///   rejected by name. Start-coded, FFmpeg reports each failure (the strict
///   open fails); in an `avcC` record it says nothing of any — the strict
///   open succeeds, the set skipped — which is the silence the rejection
///   answers.
/// - **The escaping retry**: a record whose sequence parameter set FFmpeg
///   applies only once it puts emulation prevention bytes in
///   (`decode_extradata_ps_mp4`, found by a scratch differential against
///   FFmpeg's own `ff_h264_decode_extradata`) is taken.
#[test]
fn h264_extradata_is_read_as_ffmpeg_applies_it() {
  use crate::{ExtradataRejection as Rejected, ParameterSet::*};
  let sps0 = baseline_sps(0, None);
  let pps0 = pps(0, 0);
  let mut overrun_sps = avcc(&[&sps0], &[&pps0]);
  overrun_sps[7] += 40;
  let mut overrun_pps = avcc(&[&sps0], &[&pps0]);
  let pps_length = overrun_pps.len() - pps0.len() - 1;
  overrun_pps[pps_length] += 1;
  let shapes: [(&str, Vec<u8>, Result<(), Rejected>); 5] = [
    ("a well-formed avcC", avcc(&[&sps0], &[&pps0]), Ok(())),
    (
      "five bytes",
      vec![0x01, 0x42, 0x00, 0x1e, 0xfc],
      Err(Rejected::TooShort { size: 5 }),
    ),
    (
      "six bytes",
      vec![0x01, 0x42, 0xc0, 0x1e, 0xff, 0xe1],
      Err(Rejected::TooShort { size: 6 }),
    ),
    (
      "a sequence set past the record",
      overrun_sps,
      Err(Rejected::Overrun(Sequence)),
    ),
    (
      "a picture set past the record",
      overrun_pps,
      Err(Rejected::Overrun(Picture)),
    ),
  ];
  for (name, record, expected) in shapes {
    assert_eq!(h264_record(&record), expected, "{name}: the mirror");
    assert_eq!(
      ffmpeg_opens_strictly_on(&record),
      expected.is_ok(),
      "{name}: FFmpeg, strictly"
    );
  }

  let truncated_vui = baseline_sps(
    0,
    Some(&|bits: &mut Bits| {
      // No aspect ratio, overscan, signal or chroma location; timing
      // present, its fields cut.
      bits.put(0, 4).put(1, 1);
    }),
  );
  let poc_3 = {
    let mut bits = Bits::default();
    bits.put(66, 8).put(0xc0, 8).put(30, 8).ue(0).ue(0).ue(3);
    bits.unit(0x67)
  };
  let qp_13 = {
    let mut bits = Bits::default();
    bits.ue(0).ue(0).put(0, 1).put(0, 1).ue(0).ue(0).ue(0);
    bits
      .put(0, 1)
      .put(0, 2)
      .se(0)
      .se(0)
      .se(13)
      .put(1, 1)
      .put(0, 1)
      .put(0, 1);
    bits.unit(0x68)
  };
  let id_40 = baseline_sps(40, None);
  let pps_300 = pps(300, 0);
  let pps_of_1 = pps(0, 1);
  let sets: [(&str, Vec<&[u8]>, Vec<&[u8]>, Result<(), Rejected>); 8] = [
    ("a well-formed pair", vec![&sps0], vec![&pps0], Ok(())),
    ("a VUI cut short", vec![&truncated_vui], vec![&pps0], Ok(())),
    (
      "sequence set 40",
      vec![&id_40],
      vec![&pps0],
      Err(Rejected::Unparsed(Sequence)),
    ),
    (
      "picture order type 3",
      vec![&poc_3],
      vec![&pps0],
      Err(Rejected::Unparsed(Sequence)),
    ),
    (
      "picture set 300",
      vec![&sps0],
      vec![&pps_300],
      Err(Rejected::Unparsed(Picture)),
    ),
    (
      "a chroma QP offset of 13",
      vec![&sps0],
      vec![&qp_13],
      Err(Rejected::Unparsed(Picture)),
    ),
    (
      "a picture set of sequence set 1",
      vec![&sps0],
      vec![&pps_of_1],
      Err(Rejected::Unresolved),
    ),
    (
      "a picture set before its sequence set",
      vec![&pps0],
      vec![&sps0],
      Err(Rejected::Unresolved),
    ),
  ];
  for (name, first, second, expected) in sets {
    let packed = avcc(&first, &second);
    let start_coded = annexb(&[first, second].concat());
    assert_eq!(h264_record(&packed), expected, "{name}, avcC: the mirror");
    assert_eq!(
      h264_record(&start_coded),
      expected,
      "{name}, start-coded: the mirror"
    );
    assert_eq!(
      ffmpeg_opens_strictly_on(&start_coded),
      expected.is_ok(),
      "{name}, start-coded: FFmpeg reports what it does not store"
    );
    assert!(
      ffmpeg_opens_strictly_on(&packed),
      "{name}, avcC: FFmpeg says nothing of its sets, strictly or not"
    );
  }

  let escaped_only =
    unhex("0164000affe100176764000aaca2420db0110000000100000300740f12519601000668ebe3cb22c0");
  assert_eq!(
    h264_record(&escaped_only),
    Ok(()),
    "taken: FFmpeg applies it through its escaping retry"
  );
}

/// LAW: **an entry too large for the escaping retry rejects the record** —
/// a sequence parameter set FFmpeg cannot parse, padded to 21 802 bytes,
/// past what `decode_extradata_ps_mp4` escapes (h264_parse.c:436-437) — and
/// one just under it is skipped, as any set FFmpeg cannot parse.
#[test]
fn an_entry_too_large_to_retry_rejects_the_record() {
  use crate::{ExtradataRejection as Rejected, ParameterSet::*};
  let pps0 = pps(0, 0);
  for (size, expected) in [
    (21_800, Rejected::Oversized(Sequence)),
    (21_799, Rejected::Unparsed(Sequence)),
  ] {
    let mut unit = baseline_sps(40, None);
    unit.resize(size, 0xff);
    assert_eq!(
      h264_record(&avcc(&[&unit], &[&pps0])),
      Err(expected),
      "an entry of {} bytes",
      size + 2
    );
  }
}

/// The `data` FFmpeg keeps of `unit` read the `reading`-th way
/// ([`h264_identity`]): the unit cut out of a start-coded buffer as a
/// record's units are.
fn kept_of(unit: &[u8], reading: u8) -> Vec<u8> {
  let buffer = annexb(&[unit]);
  let mut walk = Walk::new(&buffer, buffer.len(), 0, Codec::H264, false, true);
  let first = walk.next().expect("a unit").expect("cut");
  let mut scratch = Vec::new();
  let memory = walk.memory(&first, &mut scratch);
  h264_identity(&first, memory, reading)
}

/// A unit of header byte `0x67`: `payload` bytes, then `extra` more payload
/// bits, the last of them 1, and the stop bit after them — in a byte of its
/// own where there are none.
fn unit_of(payload: &[u8], extra: Option<u8>) -> Vec<u8> {
  let mut bits = Bits::default();
  for &byte in payload {
    bits.put(u64::from(byte), 8);
  }
  if let Some(n) = extra {
    bits.put(1, u32::from(n));
  }
  bits.unit(0x67)
}

/// LAW (R19 row 2; Codex R18 [medium]): **the `data` FFmpeg's H.264 decoder
/// keeps of a parameter set is its first 4096 bytes, the stop bit put back
/// only where they leave it room** — `get_bits_bytesize` cut at
/// `sizeof(sps->data)`, then the stop bit re-added where the payload filled
/// its last byte and `data_size` is still under 4096 (h264_ps.c:297-306; the
/// picture parameter set's, 717-728). From its header (the first reading):
/// 4,094 payload bytes and a stop bit of its own keep 4096, the stop bit
/// last; 4,095 keep 4096, no stop bit; 4,095 bytes and three bits keep the
/// first 4096 bytes; units of 6,000 bytes alike in those and unlike after
/// keep the same. From its raw bytes after its header (the second): 4,094
/// payload bytes and the stop byte keep those and a stop bit; 4,095 and the
/// stop byte keep those alone. Every byte kept, the second, third and last
/// cases kept 4,097 bytes, and the fourth told two sets FFmpeg keeps alike
/// apart.
#[test]
fn h264_data_is_kept_as_ffmpeg_keeps_it() {
  let payload = |n: usize| vec![0xaa_u8; n];
  let kept = kept_of(&unit_of(&payload(4094), None), 1);
  assert_eq!(kept.len(), H264_DATA);
  assert_eq!(kept.last(), Some(&0x80), "the stop bit put back");
  let kept = kept_of(&unit_of(&payload(4095), None), 1);
  assert_eq!(kept.len(), H264_DATA, "no room for the stop bit");
  assert_eq!(kept.last(), Some(&0xaa));
  let kept = kept_of(&unit_of(&payload(4095), Some(3)), 1);
  assert_eq!(kept.len(), H264_DATA, "cut at 4096");
  assert!(
    kept[..] == unit_of(&payload(4095), Some(3))[..H264_DATA],
    "the unit's first 4096 bytes"
  );
  let mut other = payload(6000);
  other[4500..].fill(0x55);
  assert!(
    kept_of(&unit_of(&payload(6000), None), 1) == kept_of(&unit_of(&other, None), 1),
    "alike in the first 4096 bytes: the same data"
  );
  let raw = kept_of(&unit_of(&payload(4094), None), 2);
  assert_eq!(raw.len(), H264_DATA);
  assert_eq!(
    raw[H264_DATA - 2..],
    [0x80, 0x80],
    "the stop byte, then the stop bit"
  );
  let raw = kept_of(&unit_of(&payload(4095), None), 2);
  assert_eq!(raw.len(), H264_DATA);
  assert_eq!(
    raw[H264_DATA - 2..],
    [0xaa, 0x80],
    "the stop byte, no room for the stop bit"
  );
}

/// What a long sequence parameter set of [`long_sps`] carries past the 4096
/// bytes FFmpeg keeps of its `data`.
#[derive(Clone, Copy, Default)]
struct PastData {
  /// The VCL HRD's last `cbr_flag`, which FFmpeg stores in `cpr_flag`.
  last_cbr: bool,
  /// `low_delay_hrd_flag`, which it reads and drops.
  low_delay: bool,
  /// `pic_struct_present_flag`, which it stores.
  pic_struct: bool,
  /// `max_bytes_per_pic_denom`, 3 or 4, which it reads and drops.
  max_bytes_per_pic_denom: u32,
  /// `max_num_reorder_frames`, 3 or 4, which it stores.
  reorder: u32,
  /// Bytes after the last field, which it never reads.
  tail: &'static [u8],
}

/// A sequence parameter set of id 0 FFmpeg reads past the 4096 bytes it keeps
/// of its `data`: High 4:4:4 Predictive, every scaling list sent, picture
/// order type 1 with 255 offsets in a cycle, cropping, a VUI with every
/// optional part and both HRDs of 32 entries, each value at the longest code
/// FFmpeg stores — the end of the VCL HRD and the bitstream restriction past
/// them, as `past` says.
fn long_sps(past: PastData) -> Vec<u8> {
  let mut bits = Bits::default();
  bits.put(244, 8).put(0, 8).put(30, 8).ue(0);
  // 4:4:4, no separate colour planes, 8 bits, no bypass; every scaling list,
  // each of its deltas -128.
  bits.ue(3).put(0, 1).ue(0).ue(0).put(0, 1).put(1, 1);
  for size in [16; 6].into_iter().chain([64; 6]) {
    bits.put(1, 1);
    for _ in 0..size {
      bits.se(-128);
    }
  }
  // Picture order type 1: each offset a 63-bit code.
  let far = 1 << 30;
  bits.ue(0).ue(1).put(0, 1).se(far).se(far).ue(255);
  for _ in 0..255 {
    bits.se(far);
  }
  // One reference frame, 128x96 in frames, cropped.
  bits.ue(1).put(0, 1).ue(7).ue(5).put(1, 1).put(1, 1);
  bits.put(1, 1).ue(60).ue(60).ue(40).ue(40);
  // The VUI: an extended aspect ratio, overscan, the video signal and colour
  // description, the chroma location, timing.
  bits.put(1, 1).put(1, 1).put(255, 8).put(4, 16).put(3, 16);
  bits.put(1, 1).put(1, 1);
  bits
    .put(1, 1)
    .put(5, 3)
    .put(1, 1)
    .put(1, 1)
    .put(1, 8)
    .put(1, 8)
    .put(1, 8);
  bits.put(1, 1).ue(5).ue(5);
  bits.put(1, 1).put(1, 32).put(50, 32).put(1, 1);
  for vcl in [false, true] {
    bits.put(1, 1).ue(31).put(0, 4).put(0, 4);
    for entry in 0..32 {
      bits.ue(u32::MAX - 1).ue(u32::MAX - 1);
      if vcl && entry == 31 {
        assert!(
          8 + bits.used as usize >= 8 * H264_DATA,
          "the premise: the VCL HRD's last cbr_flag past the 4096 bytes kept"
        );
      }
      bits.put(u64::from(vcl && entry == 31 && past.last_cbr), 1);
    }
    bits.put(23, 5).put(23, 5).put(23, 5).put(24, 5);
  }
  bits.put(u64::from(past.low_delay), 1);
  bits.put(u64::from(past.pic_struct), 1);
  bits.put(1, 1).put(1, 1);
  bits.ue(past.max_bytes_per_pic_denom).ue(1).ue(16).ue(16);
  bits.ue(past.reorder).ue(16);
  for &byte in past.tail {
    bits.put(u64::from(byte), 8);
  }
  bits.unit(0x67)
}

/// LAW (R19 row 2; Codex R18 [medium]): **an H.264 sequence parameter set is
/// told from another as FFmpeg tells it — its first 4096 bytes and every
/// field its reading stores — so what FFmpeg reads past those bytes and drops
/// makes no new set, and what it reads past them and stores does.** FFmpeg
/// compares the whole `SPS`, `data` cut at 4096 bytes and every field
/// (`memcmp`, h264_ps.c:578-587): a set it reads past the 4096 bytes is one
/// whose stored fields run past them. A record of such a set — FFmpeg's own
/// decoder, opened on it strictly, stores it — and a picture parameter set
/// bound to it: the same set again with bytes after its last field, with
/// another `low_delay_hrd_flag` or `max_bytes_per_pic_denom`, changes nothing
/// held; with another last `cbr_flag`, `pic_struct_present_flag` or
/// `max_num_reorder_frames` it replaces the set, the picture parameter set
/// bound to the one it replaced — `Superseded`, as FFmpeg holds it. A set
/// whose bitstream restriction runs past its end, which FFmpeg's third
/// reading stores with the fields it read off the bytes after it, again with
/// the same bytes after it: the same set. Compared whole, the first three
/// replaced the set; compared by `data` alone, the next three did not; a set
/// read past its end taken as new whatever it read, the last replaced itself.
#[test]
fn an_h264_sequence_parameter_set_is_told_from_another_as_ffmpeg_tells_it() {
  use super::super::held::Held;
  let h264 = crate::CodecId::H264.raw();
  let base = PastData {
    max_bytes_per_pic_denom: 3,
    reorder: 3,
    ..PastData::default()
  };
  let record = annexb(&[&long_sps(base), &pps(0, 0)]);
  assert!(
    ffmpeg_opens_strictly_on(&record),
    "the premise: FFmpeg stores the long set and the picture parameter set"
  );
  let held = Held::opened_on(h264, &record);
  assert!(held.holds_pps(0), "the premise: both sets held");
  for (name, past) in [
    (
      "bytes after the last field",
      PastData {
        tail: &[0x55; 64],
        ..base
      },
    ),
    (
      "low_delay_hrd_flag",
      PastData {
        low_delay: true,
        ..base
      },
    ),
    (
      "max_bytes_per_pic_denom",
      PastData {
        max_bytes_per_pic_denom: 4,
        ..base
      },
    ),
  ] {
    let again = long_sps(past);
    assert!(
      held.after_packet(None, Some(&annexb(&[&again]))).is_none(),
      "{name}: the same set, nothing held changes"
    );
  }
  for (name, past) in [
    (
      "the last cbr_flag",
      PastData {
        last_cbr: true,
        ..base
      },
    ),
    (
      "pic_struct_present_flag",
      PastData {
        pic_struct: true,
        ..base
      },
    ),
    ("max_num_reorder_frames", PastData { reorder: 4, ..base }),
  ] {
    let other = long_sps(past);
    assert!(
      ffmpeg_opens_strictly_on(&annexb(&[&other, &pps(0, 0)])),
      "{name}: the premise, FFmpeg stores it"
    );
    let replaced = held
      .after_packet(None, Some(&annexb(&[&other])))
      .expect("a set that differs replaces SPS 0");
    assert_eq!(
      replaced.record(h264, &record, None),
      Err(crate::Unrecordable::Superseded),
      "{name}: PPS 0 bound to the set replaced"
    );
  }

  // A set read past its end: no timing, no HRD, the bitstream restriction's
  // flags and nothing after them.
  let cut = baseline_sps(
    0,
    Some(&|bits: &mut Bits| {
      bits.put(0, 8).put(1, 1).put(1, 1);
    }),
  );
  let record = annexb(&[&cut, &pps(0, 0)]);
  let reading = {
    let mut walk = Walk::new(&record, record.len(), 0, Codec::H264, false, true);
    let unit = walk.next().expect("a unit").expect("cut");
    let mut scratch = Vec::new();
    let memory = walk.memory(&unit, &mut scratch);
    h264_sps_readings(&unit, memory, &record).map(|(.., reading)| reading)
  };
  assert_eq!(reading, Some(3), "the premise: read past its end");
  let held = Held::opened_on(h264, &record);
  assert!(held.holds_pps(0), "the premise: both sets held");
  assert!(
    held.after_packet(Some(&record), None).is_none(),
    "a set read past its end, the same bytes after it: the same set"
  );
}

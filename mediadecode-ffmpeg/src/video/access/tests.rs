//! The keyframe reading, over hand-built access units.

use super::{Anchor, KeyframeRule, RecoveryPoint};
use crate::CodecId;

/// Start-codes each NAL unit.
fn annex_b(units: &[&[u8]]) -> Vec<u8> {
  units
    .iter()
    .flat_map(|unit| [0u8, 0, 0, 1].iter().chain(unit.iter()).copied())
    .collect()
}

/// Length-prefixes each NAL unit with a four-byte field.
fn length_prefixed(units: &[&[u8]]) -> Vec<u8> {
  units
    .iter()
    .flat_map(|unit| {
      (unit.len() as u32)
        .to_be_bytes()
        .into_iter()
        .chain(unit.iter().copied())
    })
    .collect()
}

/// An `avcC` record whose NAL length fields are four bytes wide.
const AVCC: [u8; 7] = [1, 0x64, 0, 0x1f, 0xff, 0xe1, 0];

/// An `hvcC` record (23 bytes before its arrays) with four-byte NAL
/// length fields.
fn hvcc() -> Vec<u8> {
  let mut record = vec![1u8; 23];
  record[21] = 0x0f;
  record[22] = 0;
  record
}

/// **H.264: an IDR is clean, a recovery point is not**, in both packings
/// — an access unit carrying SPS, PPS and an IDR slice (type 5) against
/// one carrying a recovery-point SEI (6) and a non-IDR slice (1).
#[test]
fn an_h264_idr_is_clean_and_a_recovery_point_is_not() {
  let idr: [&[u8]; 3] = [&[0x67, 1], &[0x68, 1], &[0x65, 0x88]];
  let recovery: [&[u8]; 2] = [&[0x06, 6, 1], &[0x41, 0x9a]];
  for (rule, pack) in [
    (
      KeyframeRule::of(CodecId::H264.raw(), &[]),
      annex_b as fn(&[&[u8]]) -> Vec<u8>,
    ),
    (
      KeyframeRule::of(CodecId::H264.raw(), &AVCC),
      length_prefixed,
    ),
  ] {
    assert!(rule.is_clean(&pack(&idr)), "{rule:?}: the IDR");
    assert!(
      !rule.is_clean(&pack(&recovery)),
      "{rule:?}: the recovery point"
    );
  }
}

/// LAW (Codex R4, [high]): **H.264: the first picture decides.** A packet
/// whose first slice is a non-IDR picture (type 1) is not clean though an
/// IDR slice (type 5) follows it: a decoder started there would begin cold
/// on a picture that references what it never saw. With the IDR first it
/// is clean. In both packings.
#[test]
fn an_h264_packet_with_a_picture_before_its_idr_is_not_clean() {
  let picture_first: [&[u8]; 4] = [&[0x67, 1], &[0x68, 1], &[0x41, 0x9a], &[0x65, 0x88]];
  let idr_first: [&[u8]; 4] = [&[0x67, 1], &[0x68, 1], &[0x65, 0x88], &[0x41, 0x9a]];
  for (rule, pack) in [
    (
      KeyframeRule::of(CodecId::H264.raw(), &[]),
      annex_b as fn(&[&[u8]]) -> Vec<u8>,
    ),
    (
      KeyframeRule::of(CodecId::H264.raw(), &AVCC),
      length_prefixed,
    ),
  ] {
    assert!(
      !rule.is_clean(&pack(&picture_first)),
      "{rule:?}: a picture before the IDR"
    );
    assert!(rule.is_clean(&pack(&idr_first)), "{rule:?}: the IDR first");
  }
}

/// **HEVC: an IDR or a BLA is clean, a CRA never is**, whatever precedes
/// the first picture's NAL unit (a VPS, an SEI).
#[test]
fn an_hevc_idr_or_bla_is_clean_and_a_cra_is_not() {
  let picture = |kind: u8| -> Vec<u8> {
    let units: [&[u8]; 3] = [&[32 << 1, 1], &[39 << 1, 1], &[kind << 1, 1, 0xaf]];
    annex_b(&units)
  };
  let rule = KeyframeRule::of(CodecId::HEVC.raw(), &[]);
  for kind in [16u8, 17, 18, 19, 20] {
    assert!(rule.is_clean(&picture(kind)), "NAL type {kind}");
  }
  assert!(!rule.is_clean(&picture(21)), "a CRA");

  let rule = KeyframeRule::of(CodecId::HEVC.raw(), &hvcc());
  assert_eq!(
    rule,
    KeyframeRule::Hevc {
      nal_length: Some(4)
    }
  );
  assert!(rule.is_clean(&length_prefixed(&[&[19 << 1, 1, 0xaf]])));
  assert!(!rule.is_clean(&length_prefixed(&[&[21 << 1, 1, 0xaf]])));
}

/// LAW (Codex R3): **a unit that is not whole is not clean.** Every header
/// byte is read and checked — H.264's forbidden bit; HEVC's forbidden bit,
/// its second header byte and a `nuh_temporal_id_plus1` that is not zero —
/// and a picture's unit must carry payload past its header. The review's
/// truncated `00 00 01 26`, which a one-byte read takes for an IDR, is
/// not clean; nor is anything else that does not parse whole.
#[test]
fn a_unit_that_is_not_whole_is_not_clean() {
  let hevc = KeyframeRule::of(CodecId::HEVC.raw(), &[]);
  for (data, why) in [
    (vec![0u8, 0, 1, 0x26], "the review's truncated IDR"),
    (annex_b(&[&[19 << 1, 1]]), "an IDR header with no slice"),
    (
      annex_b(&[&[0x80 | (19 << 1), 1, 0xaf]]),
      "the forbidden bit",
    ),
    (annex_b(&[&[19 << 1, 0, 0xaf]]), "a temporal id of zero"),
    (
      annex_b(&[&[32 << 1, 1], &[0x80, 1], &[19 << 1, 1, 0xaf]]),
      "a malformed unit before the picture",
    ),
    (
      vec![0, 0, 1, 19 << 1, 1, 0xaf, 0, 0, 1],
      "a start code ending the data",
    ),
  ] {
    assert!(!hevc.is_clean(&data), "HEVC: {why}");
  }
  assert!(
    hevc.is_clean(&annex_b(&[&[19 << 1, 1, 0xaf]])),
    "the whole IDR is clean"
  );

  let h264 = KeyframeRule::of(CodecId::H264.raw(), &[]);
  for (data, why) in [
    (annex_b(&[&[0x65]]), "an IDR header with no slice"),
    (annex_b(&[&[0x80 | 0x65, 0x88]]), "the forbidden bit"),
    (
      annex_b(&[&[0xe7, 1], &[0x65, 0x88]]),
      "a malformed SPS beside the IDR",
    ),
  ] {
    assert!(!h264.is_clean(&data), "H.264: {why}");
  }
  let avcc = KeyframeRule::of(CodecId::H264.raw(), &AVCC);
  assert!(!avcc.is_clean(&[0, 0, 0, 0]), "an empty unit");
}

/// **Bytes that do not parse prove nothing**: a length field running past
/// the end is not clean.
#[test]
fn a_truncated_unit_is_not_clean() {
  let rule = KeyframeRule::of(CodecId::H264.raw(), &AVCC);
  let mut idr = length_prefixed(&[&[0x65, 0x88, 0x84]]);
  idr.truncate(idr.len() - 1);
  assert!(!rule.is_clean(&idr));
}

/// **The other codecs** (Codex R6 row 2): VP8, VP9 and AV1 keyframes reset
/// every reference; any codec this crate reads no picture header of is
/// never clean mid-stream — its decoder's `has_b_frames` proves nothing,
/// since FFmpeg raises it only when it meets reordering — and returns to the
/// session's threads at a seek alone.
#[test]
fn the_other_codecs_are_read_by_their_references_alone() {
  for codec in [CodecId::VP8, CodecId::VP9, CodecId::AV1] {
    assert!(KeyframeRule::of(codec.raw(), &[]).is_clean(&[]));
  }
  let mpeg4 = KeyframeRule::of(CodecId::MPEG4.raw(), &[]);
  assert_eq!(mpeg4, KeyframeRule::Reordering);
  assert!(!mpeg4.is_clean(&[]), "never clean mid-stream");
}

/// LAW (Codex R6 row 2, [high]): **an MPEG-2 keyframe is clean only behind
/// a closed GOP header.** A packet whose group-of-pictures header before
/// its picture sets `closed_gop` is a clean random access point; one whose
/// header leaves it unset, one with no GOP header, one whose GOP header
/// comes after its picture, and one whose header is cut short are not.
#[test]
fn an_mpeg2_keyframe_is_clean_only_behind_a_closed_gop_header() {
  let rule = KeyframeRule::of(CodecId::MPEG2VIDEO.raw(), &[]);
  assert_eq!(rule, KeyframeRule::Mpeg12);
  let sequence: &[u8] = &[
    0, 0, 1, 0xb3, 0x08, 0x00, 0x60, 0x13, 0xff, 0xff, 0xe0, 0x18,
  ];
  // `time_code` zero but for its marker bit; then `closed_gop`, `broken_link`.
  let gop = |closed: bool| -> Vec<u8> {
    vec![
      0,
      0,
      1,
      0xb8,
      0x00,
      0x08,
      0x00,
      if closed { 0x40 } else { 0x00 },
    ]
  };
  let picture: &[u8] = &[0, 0, 1, 0x00, 0x00, 0x0f, 0xff, 0xf8];
  for (data, clean, why) in [
    (
      [sequence, &gop(true), picture].concat(),
      true,
      "a closed GOP",
    ),
    (
      [sequence, &gop(false), picture].concat(),
      false,
      "an open GOP",
    ),
    ([sequence, picture].concat(), false, "no GOP header"),
    (
      [picture, &gop(true)[..]].concat(),
      false,
      "the GOP header after the picture",
    ),
    (
      [sequence, &gop(true)[..6]].concat(),
      false,
      "a GOP header cut short",
    ),
  ] {
    assert_eq!(rule.is_clean(&data), clean, "{why}");
  }
}

/// LAW (Codex R4, [medium]): **a start code is read whole, a unit's trailing
/// zeros are not its own, and every unit is validated.** Codex's bytes: in
/// `00 00 00 01 26 01 | 00 00 00 01 02 01 80` a three-byte-only reader finds
/// the second prefix at its second byte and leaves its first zero on the IDR
/// unit, which then reads `26 01 00` — a header with a byte after it — and
/// is taken for a whole IDR. Read whole, the IDR unit is its header alone
/// and not clean; so is H.264's `65` in the same shape, and an IDR followed
/// by `trailing_zero_8bits` before a three-byte prefix. With a slice header
/// the same layouts are clean. A malformed unit AFTER the first picture
/// makes the access unit not clean too, and so do bytes before the first
/// start code that are not zeros, a short extended header, an IDR whose
/// `nal_ref_idc` is zero, and an HEVC IRAP picture with a temporal id.
#[test]
fn every_unit_is_read_whole_and_validated() {
  let hevc = KeyframeRule::of(CodecId::HEVC.raw(), &[]);
  let h264 = KeyframeRule::of(CodecId::H264.raw(), &[]);
  for (rule, data, why) in [
    (
      hevc,
      vec![0u8, 0, 0, 1, 0x26, 1, 0, 0, 0, 1, 2, 1, 0x80],
      "Codex's bytes: the IDR is its header alone",
    ),
    (
      h264,
      vec![0u8, 0, 0, 1, 0x65, 0, 0, 0, 1, 0x41, 0x9a],
      "H.264's 65 in the same shape",
    ),
    (
      hevc,
      vec![0u8, 0, 1, 0x26, 1, 0, 0, 0, 0, 1, 2, 1, 0x80],
      "a header-only IDR, then trailing zeros and a four-byte prefix",
    ),
    (
      hevc,
      vec![0u8, 0, 1, 0x26, 1, 0, 0, 0, 1, 2, 1, 0x80],
      "a header-only IDR, then a trailing zero and a three-byte prefix",
    ),
    (
      hevc,
      annex_b(&[&[19 << 1, 1, 0xaf], &[0x80 | (1 << 1), 1, 0x80]]),
      "a malformed unit after the first picture",
    ),
    (
      hevc,
      [&[7u8][..], &annex_b(&[&[19 << 1, 1, 0xaf]])].concat(),
      "a byte before the first start code",
    ),
    (
      hevc,
      annex_b(&[&[19 << 1, 2, 0xaf]]),
      "an IDR with a temporal id",
    ),
    (
      h264,
      annex_b(&[&[0x6e, 1], &[0x65, 0x88]]),
      "a prefix unit with a short extended header",
    ),
    (
      h264,
      annex_b(&[&[0x05, 0x88]]),
      "an IDR whose nal_ref_idc is zero",
    ),
  ] {
    assert!(!rule.is_clean(&data), "{rule:?}: {why}");
  }
  for (rule, data, why) in [
    (
      hevc,
      vec![0u8, 0, 0, 1, 0x26, 1, 0xaf, 0, 0, 0, 1, 2, 1, 0x80],
      "four-byte prefixes around a whole IDR",
    ),
    (
      hevc,
      vec![0u8, 0, 1, 0x26, 1, 0xaf, 0, 0, 0, 0, 1, 2, 1, 0x80],
      "a whole IDR, its trailing zeros stripped",
    ),
    (
      h264,
      vec![0u8, 0, 0, 1, 0x65, 0x88, 0, 0, 0, 1, 0x41, 0x9a],
      "four-byte prefixes around a whole H.264 IDR",
    ),
    (
      hevc,
      [&[0u8, 0][..], &annex_b(&[&[19 << 1, 1, 0xaf]])].concat(),
      "leading zeros before the first start code",
    ),
  ] {
    assert!(rule.is_clean(&data), "{rule:?}: {why}");
  }
}

/// LAW (Codex R5 row 4, [high]): **millions of tiny units are classified
/// in constant memory.** The units are walked one at a time, each header
/// validated as the walk reaches it and the first picture deciding, and
/// never collected: a hostile keyframe of one-byte length fields and
/// one-byte units used to cost a slice entry — 16 bytes of `Vec` — for
/// every two bytes of input. Four million one-byte H.264 filler units and
/// an IDR slice, length-prefixed by one-byte fields and start-coded, and
/// four million two-byte HEVC SEI units and an IDR: each access unit is
/// clean, and is classified without a single allocation.
#[test]
fn millions_of_tiny_units_are_classified_without_allocating() {
  const UNITS: usize = 4_000_000;
  // An `avcC` record whose `lengthSizeMinusOne` is 0: one-byte fields.
  let one_byte = KeyframeRule::of(CodecId::H264.raw(), &[1, 0x64, 0, 0x1f, 0xfc, 0xe1, 0]);
  assert_eq!(
    one_byte,
    KeyframeRule::H264 {
      nal_length: Some(1)
    }
  );
  let mut length_prefixed_au = Vec::with_capacity(UNITS * 2 + 3);
  let mut annex_b_au = Vec::with_capacity(UNITS * 4 + 5);
  let mut hevc_au = Vec::with_capacity(UNITS * 5 + 6);
  for _ in 0..UNITS {
    length_prefixed_au.extend_from_slice(&[1, 0x0c]);
    annex_b_au.extend_from_slice(&[0, 0, 1, 0x0c]);
    hevc_au.extend_from_slice(&[0, 0, 1, 39 << 1, 1]);
  }
  length_prefixed_au.extend_from_slice(&[2, 0x65, 0x88]);
  annex_b_au.extend_from_slice(&[0, 0, 1, 0x65, 0x88]);
  hevc_au.extend_from_slice(&[0, 0, 1, 19 << 1, 1, 0xaf]);
  for (rule, data, why) in [
    (
      one_byte,
      &length_prefixed_au,
      "H.264, one-byte length fields",
    ),
    (
      KeyframeRule::of(CodecId::H264.raw(), &[]),
      &annex_b_au,
      "H.264, start codes",
    ),
    (
      KeyframeRule::of(CodecId::HEVC.raw(), &[]),
      &hevc_au,
      "HEVC, start codes",
    ),
  ] {
    let (clean, allocations, bytes) = crate::test_alloc::measured(|| rule.is_clean(data));
    assert!(clean, "{why}: the IDR after the units is clean");
    assert_eq!(
      (allocations, bytes),
      (0, 0),
      "{why}: classified without allocating"
    );
  }
}

/// Packs `bits`, a string of `0`s and `1`s, into the bytes of an H.264
/// raw byte sequence payload — zero-padded — with an emulation prevention
/// byte (`03`) before every byte of `00` to `03` that two zero bytes lead.
fn rbsp(bits: &str) -> Vec<u8> {
  emulation_prevented(&packed(bits))
}

/// Packs `bits`, a string of `0`s and `1`s, into bytes, zero-padded.
fn packed(bits: &str) -> Vec<u8> {
  bits
    .as_bytes()
    .chunks(8)
    .map(|chunk| {
      chunk
        .iter()
        .enumerate()
        .filter(|&(_, &bit)| bit == b'1')
        .fold(0u8, |byte, (index, _)| byte | (0x80 >> index))
    })
    .collect()
}

/// `payload` as a NAL unit carries it: an emulation prevention byte (`03`)
/// before every byte of `00` to `03` that two zero bytes lead.
fn emulation_prevented(payload: &[u8]) -> Vec<u8> {
  let mut raw = Vec::new();
  let mut zeros = 0;
  for &byte in payload {
    if zeros >= 2 && byte <= 3 {
      raw.push(3);
      zeros = 0;
    }
    zeros = if byte == 0 { zeros + 1 } else { 0 };
    raw.push(byte);
  }
  raw
}

/// An H.264 slice NAL unit: `header`, then a slice header opening with
/// `first_mb_in_slice` and `slice_type` as exp-Golomb codes.
fn slice(header: u8, first_mb_in_slice: &str, slice_type: &str) -> Vec<u8> {
  [
    vec![header],
    rbsp(&format!("{first_mb_in_slice}{slice_type}1")),
  ]
  .concat()
}

/// An H.264 SEI NAL unit carrying `messages`, each a payload type and its
/// payload: the type and the size each a run of `FF` bytes and a last byte,
/// the payload, then the `rbsp_trailing_bits` — every byte of it emulation
/// prevented.
fn sei(messages: &[(u32, &[u8])]) -> Vec<u8> {
  let mut payload = Vec::new();
  let value = |out: &mut Vec<u8>, mut value: u32| {
    while value >= 255 {
      out.push(0xff);
      value -= 255;
    }
    out.push(value as u8);
  };
  for &(kind, body) in messages {
    value(&mut payload, kind);
    value(&mut payload, body.len() as u32);
    payload.extend_from_slice(body);
  }
  payload.push(0x80);
  [vec![0x06], emulation_prevented(&payload)].concat()
}

/// A recovery point message's payload: `recovery_frame_cnt` as the
/// exp-Golomb bits `frames`, `exact_match_flag` 1, `broken_link_flag` as
/// `broken`, `changing_slice_group_idc` 0, then the payload's alignment —
/// a one, then zeros.
fn recovery(frames: &str, broken: bool) -> Vec<u8> {
  let bits = format!("{frames}1{}001", if broken { "1" } else { "0" });
  let padded = format!("{bits}{}", "0".repeat((8 - bits.len() % 8) % 8));
  packed(&padded)
}

/// An anchor as the laws state it: its recovery point's count (0 where it
/// stands on none) and whether it is definitive.
fn read(anchor: Option<Anchor>) -> Option<(u32, bool)> {
  anchor.map(|anchor| {
    (
      anchor.recovery().map_or(0, RecoveryPoint::frames),
      anchor.definitive(),
    )
  })
}

/// LAW (Codex R6 row 1, [high]; restated by Codex R7 and R8): **a resync
/// anchor is a packet the bitstream proves a random-access point, and it is
/// definitive where it is a clean one.** H.264: an IDR slice anchors,
/// definitively; a non-IDR picture anchors only behind a recovery point SEI
/// message — an I slice, an SI slice, an I slice whose header holds an
/// emulation prevention byte: none of them alone — while a packet whose first
/// slice is P or B does not, though an IDR follows it in the packet. HEVC:
/// every IRAP picture anchors, a CRA among them, while a trailing or a RASL
/// picture first does not; an IDR or a BLA is definitive, a CRA not. Every
/// other codec takes the key flag as FFmpeg's parser set it, definitive
/// where its keyframes reset every reference.
#[test]
fn a_resync_anchor_is_a_packet_whose_first_picture_is_random_access() {
  let h264 = KeyframeRule::of(CodecId::H264.raw(), &[]);
  // ue(0) is `1`; ue(7), I for every slice, `0001000`; ue(5), P,
  // `00110`; ue(4), SI, `00101`; ue(6), B, `00111`.
  let idr = slice(0x65, "1", "0001000");
  let i_slice = slice(0x41, "1", "0001000");
  let si_slice = slice(0x41, "1", "00101");
  let p_slice = slice(0x41, "1", "00110");
  let b_slice = slice(0x01, "1", "00111");
  // `first_mb_in_slice` with 22 leading zeros: the payload runs `00 00 02`,
  // which the bitstream carries as `00 00 03 02`.
  let far_i_slice = slice(
    0x41,
    &format!("{}1{}", "0".repeat(22), "0".repeat(22)),
    "0001000",
  );
  assert!(
    far_i_slice.windows(3).any(|w| w == [0, 0, 3]),
    "the fixture carries an emulation prevention byte"
  );
  let recovers = sei(&[(6, &recovery("1", false))]);
  for (units, anchor, why) in [
    (vec![&idr[..]], Some((0, true)), "an IDR slice"),
    (
      vec![&recovers[..], &i_slice[..]],
      Some((0, false)),
      "a recovery point's I slice",
    ),
    (vec![&i_slice[..]], None, "an I slice alone"),
    (vec![&si_slice[..]], None, "an SI slice alone"),
    (
      vec![&recovers[..], &far_i_slice[..]],
      Some((0, false)),
      "a recovery point's I slice read through `00 00 03`",
    ),
    (vec![&p_slice[..]], None, "a P slice, a stale key flag"),
    (vec![&b_slice[..]], None, "a B slice"),
    (
      vec![&p_slice[..], &idr[..]],
      None,
      "a P slice before the IDR",
    ),
  ] {
    assert_eq!(read(h264.anchor(&annex_b(&units))), anchor, "H.264: {why}");
  }

  let hevc = KeyframeRule::of(CodecId::HEVC.raw(), &[]);
  let picture = |kind: u8| -> Vec<u8> { vec![kind << 1, 1, 0xaf] };
  for (kinds, anchor, why) in [
    (vec![21u8], Some((0, false)), "a CRA"),
    (vec![19], Some((0, true)), "an IDR"),
    (vec![16], Some((0, true)), "a BLA"),
    (vec![1], None, "a trailing picture"),
    (vec![8], None, "a RASL picture"),
    (vec![1, 21], None, "a trailing picture before the CRA"),
  ] {
    let units: Vec<Vec<u8>> = kinds.iter().map(|&kind| picture(kind)).collect();
    let units: Vec<&[u8]> = units.iter().map(Vec::as_slice).collect();
    assert_eq!(read(hevc.anchor(&annex_b(&units))), anchor, "HEVC: {why}");
  }

  for (codec, definitive) in [
    (CodecId::MPEG4, false),
    (CodecId::VP9, true),
    (CodecId::AV1, true),
  ] {
    assert_eq!(
      read(KeyframeRule::of(codec.raw(), &[]).anchor(&[])),
      Some((0, definitive)),
      "{codec:?}: the key flag stands"
    );
  }
}

/// LAW (Codex R7, [high]; restated by Codex R8): **an H.264 resync anchor is
/// an IDR picture, or an access unit whose recovery point SEI says so — and
/// the anchor carries its `recovery_frame_cnt`, reported.** A slice type describes its own
/// slice alone: an access unit whose I slice comes first and a P slice after
/// it, with no recovery point, anchors nothing. A recovery point anchors at
/// any slice type, its count the answer — 0, 2, its broken link changing
/// nothing; every SEI message before it is walked by its size, through the
/// emulation prevention bytes a payload's `00 00 01` takes. A slice type of
/// 12 is no slice type: the unit anchors nothing. A recovery point after the
/// first picture, or one whose message runs past its unit or past its own
/// payload, anchors nothing.
#[test]
fn an_h264_anchor_is_an_idr_or_a_recovery_point_sei() {
  let h264 = KeyframeRule::of(CodecId::H264.raw(), &[]);
  // ue(2), I: `011`; ue(0), P: `1`; the second slice from macroblock 1.
  let i_first = slice(0x41, "1", "011");
  let p_after = slice(0x41, "010", "1");
  let p_slice = slice(0x41, "1", "00110");
  // ue(12): `0001101`.
  let twelve = slice(0x41, "1", "0001101");
  let zero = sei(&[(6, &recovery("1", false))]);
  let two = sei(&[(6, &recovery("011", false))]);
  let broken = sei(&[(6, &recovery("011", true))]);
  // A user-data payload before the recovery point, holding `00 00 01`.
  let user_data: Vec<u8> = [&[0u8; 16][..], &[0, 0, 1, 0x42]].concat();
  let behind = sei(&[(5, &user_data), (6, &recovery("011", false))]);
  assert!(
    behind.windows(4).any(|w| w == [0, 0, 3, 1]),
    "the fixture carries an emulation prevention byte"
  );
  // A message whose size runs past the unit, and a recovery point whose
  // fields overrun the one byte of payload it says it has.
  let past_the_unit: Vec<u8> = vec![0x06, 6, 9, 0x80];
  let overrun = sei(&[(6, &[0x00][..])]);
  for (units, anchor, why) in [
    (
      vec![&i_first[..], &p_after[..]],
      None,
      "an I slice first, a P slice after, no recovery point",
    ),
    (
      vec![&zero[..], &p_slice[..]],
      Some(0),
      "a recovery point, 0",
    ),
    (vec![&two[..], &p_slice[..]], Some(2), "a recovery point, 2"),
    (
      vec![&broken[..], &p_slice[..]],
      Some(2),
      "a broken link changes nothing forward",
    ),
    (
      vec![&behind[..], &p_slice[..]],
      Some(2),
      "walked by size past another payload",
    ),
    (vec![&zero[..], &twelve[..]], None, "slice type 12"),
    (
      vec![&p_slice[..], &two[..]],
      None,
      "the recovery point after the picture",
    ),
    (
      vec![&past_the_unit[..], &p_slice[..]],
      None,
      "a message past its unit",
    ),
    (
      vec![&overrun[..], &p_slice[..]],
      None,
      "a recovery point past its payload",
    ),
  ] {
    assert_eq!(
      read(h264.anchor(&annex_b(&units))).map(|(frames, _)| frames),
      anchor,
      "{why}"
    );
  }
}

/// LAW (the coordinator's row 6): **a codec that codes every picture alone
/// is clean at every packet, and anchors at every one.** ProRes, DNxHD,
/// MJPEG and Ut Video carry `AV_CODEC_PROP_INTRA_ONLY` in their
/// descriptors; their rule takes every packet, whatever it holds.
#[test]
fn an_intra_only_codec_is_clean_and_anchors_at_every_packet() {
  for codec in [
    CodecId::PRORES.raw(),
    CodecId::DNXHD.raw(),
    CodecId::MJPEG.raw(),
    ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_UTVIDEO as i32,
  ] {
    let rule = KeyframeRule::of(codec, &[]);
    assert_eq!(rule, KeyframeRule::IntraOnly, "codec {codec}");
    assert!(rule.every_packet() && rule.is_clean(&[1, 2, 3]));
    assert_eq!(
      read(rule.anchor(&[1, 2, 3])),
      Some((0, true)),
      "codec {codec}: every packet anchors, definitively"
    );
  }
  assert_eq!(
    KeyframeRule::of(CodecId::MPEG4.raw(), &[]),
    KeyframeRule::Reordering,
    "MPEG-4 references other pictures"
  );
}

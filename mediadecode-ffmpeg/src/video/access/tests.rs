//! The keyframe reading, over hand-built access units.

use super::{Anchor, KeyframeRule, Proof, RecoveryPoint};
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
      nal_length: Some(4),
      alpha: false,
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
      nal_length: Some(1),
      aso: false,
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
/// exp-Golomb bits `frames`, `exact_match_flag` as `exact`,
/// `broken_link_flag` as `broken`, `changing_slice_group_idc` 0, then the
/// payload's alignment — a one, then zeros.
fn recovery(frames: &str, exact: bool, broken: bool) -> Vec<u8> {
  let bit = |flag: bool| if flag { "1" } else { "0" };
  let bits = format!("{frames}{}{}001", bit(exact), bit(broken));
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

/// LAW (Codex R6 row 1, [high]; restated by Codex R7, R8 and R12): **a
/// resync anchor is a packet the bitstream proves a random-access point, and
/// it is definitive where it is a clean one.** H.264: an IDR slice anchors,
/// definitively; a non-IDR picture anchors only behind a recovery point SEI
/// message — an I slice, an SI slice: neither alone — and a recovery point
/// read through an emulation prevention byte in its own payload anchors with
/// the count it carries, while a packet whose first slice is P or B does
/// not, though an IDR follows it in the packet, nor one whose first slice is
/// a later slice of its picture (R12): an I slice whose `first_mb_in_slice`
/// runs to 22 leading zeros, its header holding the emulation prevention
/// byte, behind a recovery point. HEVC:
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
  let recovers = sei(&[(6, &recovery("1", true, false))]);
  // `recovery_frame_cnt` with 22 leading zeros, 4 194 303: the message's
  // payload runs `00 00 02`, carried as `00 00 03 02`.
  let recovers_far = sei(&[(
    6,
    &recovery(
      &format!("{}1{}", "0".repeat(22), "0".repeat(22)),
      true,
      false,
    ),
  )]);
  assert!(
    recovers_far.windows(3).any(|w| w == [0, 0, 3]),
    "the message carries an emulation prevention byte"
  );
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
      vec![&recovers_far[..], &i_slice[..]],
      Some(((1 << 22) - 1, false)),
      "a recovery point read through `00 00 03`",
    ),
    (
      vec![&recovers[..], &far_i_slice[..]],
      None,
      "a later slice of its picture behind a recovery point",
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
  let zero = sei(&[(6, &recovery("1", true, false))]);
  let two = sei(&[(6, &recovery("011", true, false))]);
  let broken = sei(&[(6, &recovery("011", true, true))]);
  // A user-data payload before the recovery point, holding `00 00 01`.
  let user_data: Vec<u8> = [&[0u8; 16][..], &[0, 0, 1, 0x42]].concat();
  let behind = sei(&[(5, &user_data), (6, &recovery("011", true, false))]);
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

/// LAW (Codex R8 row 3, [medium]): **an approximate recovery point anchors,
/// and its flags are read.** H.264 D.2.8: `exact_match_flag` 0 says the
/// pictures from the recovery on need not match a decode that started
/// before it — an approximate recovery. What the resync proves is the
/// pictures a decoder started at the recovery point produces, which FFmpeg
/// does whatever the flag says, so the approximate point anchors as the
/// exact one does; the anchor carries both flags, read field by field, and
/// `broken_link_flag` beside them.
#[test]
fn an_approximate_recovery_point_anchors_and_its_flags_are_read() {
  let h264 = KeyframeRule::of(CodecId::H264.raw(), &[]);
  let p_slice = slice(0x41, "1", "00110");
  for (exact, broken) in [(true, false), (false, false), (false, true), (true, true)] {
    let message = sei(&[(6, &recovery("011", exact, broken))]);
    let anchor = h264
      .anchor(&annex_b(&[&message[..], &p_slice[..]]))
      .expect("a recovery point anchors, exact or approximate");
    let point = anchor.recovery().expect("the recovery point it stands on");
    assert_eq!(
      (point.frames(), point.exact_match(), point.broken_link()),
      (2, exact, broken),
      "the message's fields, read"
    );
    assert!(!anchor.definitive(), "a recovery point is not definitive");
  }
}

/// LAW (Codex R9, [high]): **a resync's proof is its codec rule's.** For
/// H.264, either packing, it is FFmpeg withholding every picture it has not
/// recovered, so the first picture out after the anchor closes the gap; for
/// HEVC and every other rule, the reorder bound.
#[test]
fn a_resync_is_proved_by_withheld_output_on_h264_and_by_the_reorder_bound_elsewhere() {
  for nal_length in [None, Some(4)] {
    assert_eq!(
      KeyframeRule::H264 {
        nal_length,
        aso: false
      }
      .proof(),
      Proof::Withheld
    );
  }
  for rule in [
    KeyframeRule::Hevc {
      nal_length: None,
      alpha: false,
    },
    KeyframeRule::Hevc {
      nal_length: Some(4),
      alpha: false,
    },
    KeyframeRule::Mpeg12,
    KeyframeRule::Resets,
    KeyframeRule::IntraOnly,
    KeyframeRule::Reordering,
  ] {
    assert_eq!(rule.proof(), Proof::ReorderBound, "{rule:?}");
  }
}

/// LAW (Codex R12, [high]): **a clean point and an anchor START a picture.**
/// An H.264 IDR slice whose `first_mb_in_slice` is 5 — a later slice of the
/// IDR picture, what opens a packet holding the rest of a picture split
/// across packets — is neither clean nor an anchor, in either packing, nor
/// is a recovery point whose first slice is a later one; an HEVC IDR or CRA
/// whose first slice segment has `first_slice_segment_in_pic_flag` 0 is
/// neither either; an H.264 IDR unit cut inside its slice header, before
/// its `slice_type`, is not clean. The same units starting their pictures —
/// `first_mb_in_slice` 0, the flag 1 — stay clean and anchor. Reading
/// `first_mb_in_slice` and discarding its value, the continuation slice was
/// a clean point and a definitive anchor: the switch drained the decoder
/// that had the picture's leading slices and handed the rest to one that
/// never saw them.
#[test]
fn a_clean_point_and_an_anchor_start_a_picture() {
  let starts = slice(0x65, "1", "0001000");
  let continues = slice(0x65, "00110", "0001000");
  for nal_length in [None, Some(4)] {
    let h264 = KeyframeRule::H264 {
      nal_length,
      aso: false,
    };
    let pack = |units: &[&[u8]]| match nal_length {
      None => annex_b(units),
      Some(_) => length_prefixed(units),
    };
    let start = pack(&[&[0x67, 1], &[0x68, 1], &starts[..]]);
    let continuation = pack(&[&continues[..]]);
    assert!(
      h264.is_clean(&start),
      "{h264:?}: an IDR starting its picture is clean"
    );
    assert!(
      h264.anchor(&start).is_some_and(Anchor::definitive),
      "{h264:?}: and a definitive anchor"
    );
    assert!(
      !h264.is_clean(&continuation),
      "{h264:?}: an IDR slice whose first_mb_in_slice is 5 is not clean"
    );
    assert_eq!(h264.anchor(&continuation), None, "{h264:?}: nor an anchor");
    let message = sei(&[(6, &recovery("1", true, false))]);
    let later_p = slice(0x41, "00110", "00110");
    assert_eq!(
      h264.anchor(&pack(&[&message[..], &later_p[..]])),
      None,
      "{h264:?}: a recovery point whose first slice is a later one anchors nothing"
    );
    let truncated = pack(&[&[0x65, 0x80]]);
    assert!(
      !h264.is_clean(&truncated) && h264.anchor(&truncated).is_none(),
      "{h264:?}: an IDR unit cut before its slice_type is neither"
    );
  }
  for nal_length in [None, Some(4)] {
    let hevc = KeyframeRule::Hevc {
      nal_length,
      alpha: false,
    };
    let pack = |units: &[&[u8]]| match nal_length {
      None => annex_b(units),
      Some(_) => length_prefixed(units),
    };
    for (kind, clean) in [(19u8, true), (21, false)] {
      let start = pack(&[&[kind << 1, 1, 0xaf]]);
      let continuation = pack(&[&[kind << 1, 1, 0x2f]]);
      assert_eq!(
        (hevc.is_clean(&start), hevc.anchor(&start).is_some()),
        (clean, true),
        "{hevc:?}: type {kind} starting its picture"
      );
      assert_eq!(
        (hevc.is_clean(&continuation), hevc.anchor(&continuation)),
        (false, None),
        "{hevc:?}: type {kind} whose first segment has first_slice_segment_in_pic_flag 0"
      );
    }
  }
}

/// LAW (Codex R13, [high]): **an H.264 stream whose sequence parameter set
/// permits arbitrary slice order has no clean point and no anchor.** The
/// Baseline (66) and Extended (88) profiles let a picture's slices come in
/// any order, so the slice holding macroblock 0 can follow another slice of
/// its picture: an IDR split across packets can put a later slice at the
/// head of one packet and macroblock 0's at the head of the next, which the
/// MB-0 proof alone took for a picture start. Read off an `avcC` record's
/// profile bytes, an IDR starting its picture is neither clean nor an
/// anchor for Baseline or Extended without `constraint_set1_flag`; read off
/// an Annex B packet's own SPS, the same. With the flag set — Constrained
/// Baseline, held to the Main profile's in-order slices — or the High
/// profile, it is clean and anchors as before.
#[test]
fn a_stream_permitting_arbitrary_slice_order_has_no_clean_point_and_no_anchor() {
  let idr = slice(0x65, "1", "0001000");
  let cases = [
    (66u8, 0x80u8, true, "Baseline"),
    (66, 0xc0, false, "Constrained Baseline"),
    (88, 0x00, true, "Extended"),
    (88, 0x40, false, "Extended held to Main"),
    (100, 0x00, false, "High"),
  ];
  for (profile, flags, permits, name) in cases {
    let avcc = KeyframeRule::of(
      CodecId::H264.raw(),
      &[1, profile, flags, 0x1e, 0xff, 0xe1, 0],
    );
    let au = length_prefixed(&[&idr[..]]);
    assert_eq!(
      (avcc.is_clean(&au), avcc.anchor(&au).is_some()),
      (!permits, !permits),
      "{name}, read off the avcC record: clean and anchoring {}",
      !permits
    );
    let annex_b_rule = KeyframeRule::of(CodecId::H264.raw(), &[]);
    let sps = [0x67, profile, flags, 0x1e, 0xac];
    let au = annex_b(&[&sps[..], &[0x68, 0xce], &idr[..]]);
    assert_eq!(
      (
        annex_b_rule.is_clean(&au),
        annex_b_rule.anchor(&au).is_some()
      ),
      (!permits, !permits),
      "{name}, read off the packet's own SPS: clean and anchoring {}",
      !permits
    );
  }
}

/// LAW (Codex R13, [high]): **an HEVC packet is read by its base layer, a
/// unit of layer 63 skipped.** FFmpeg's NAL splitter drops every unit of
/// layer 63, whatever the rest of its header says, and its decoder outputs
/// the base layer: a packet whose first unit is an IDR of layer 63, or of an
/// enhancement layer (1), before a base-layer trailing picture is neither
/// clean nor an anchor, in either packing. A base-layer IDR after a unit of
/// layer 63 — an IDR, or one with a temporal id of 0, which FFmpeg drops
/// unread — or before an enhancement layer's trailing picture is clean and a
/// definitive anchor; a base-layer CRA after an IDR of layer 63 anchors and
/// is not clean. Read whatever its layer, the IDR of layer 63 made the
/// packet a clean point and a definitive anchor though FFmpeg never decodes
/// it.
#[test]
fn an_hevc_packet_is_read_by_its_base_layer() {
  let at_layer = |kind: u8, layer: u8| -> Vec<u8> {
    vec![(kind << 1) | (layer >> 5), ((layer & 0x1f) << 3) | 1, 0xaf]
  };
  for nal_length in [None, Some(4)] {
    let hevc = KeyframeRule::Hevc {
      nal_length,
      alpha: false,
    };
    let read = |units: &[&[u8]]| {
      let au = match nal_length {
        None => annex_b(units),
        Some(_) => length_prefixed(units),
      };
      (hevc.is_clean(&au), hevc.anchor(&au).map(Anchor::definitive))
    };
    let (idr, trail, cra) = (at_layer(19, 0), at_layer(1, 0), at_layer(21, 0));
    for layer in [63, 1] {
      assert_eq!(
        read(&[&at_layer(19, layer)[..], &trail[..]]),
        (false, None),
        "{hevc:?}: an IDR of layer {layer} before a base-layer trailing picture"
      );
    }
    let mut unread = at_layer(19, 63);
    unread[1] &= !0x07;
    for before in [at_layer(19, 63), unread] {
      assert_eq!(
        read(&[&before[..], &idr[..]]),
        (true, Some(true)),
        "{hevc:?}: a base-layer IDR after the unit of layer 63 {before:02x?}"
      );
    }
    assert_eq!(
      read(&[&idr[..], &at_layer(1, 1)[..]]),
      (true, Some(true)),
      "{hevc:?}: a base-layer IDR before an enhancement layer's trailing picture"
    );
    assert_eq!(
      read(&[&at_layer(19, 63)[..], &cra[..]]),
      (false, Some(false)),
      "{hevc:?}: a base-layer CRA after an IDR of layer 63"
    );
  }
}

/// `value` as an unsigned exp-Golomb code, `ue(v)`, in `0`s and `1`s.
fn ue(value: u32) -> String {
  let coded = value + 1;
  let width = 32 - coded.leading_zeros();
  format!("{}{coded:b}", "0".repeat(width as usize - 1))
}

/// What [`vps`] writes: an HEVC video parameter set of one sub-layer, Main
/// profile at level 3.1, and the extension FFmpeg 9 reads whole for a set
/// of two layers (`decode_vps_ext`).
#[derive(Clone, Copy)]
struct Vps {
  /// `vps_max_layers_minus1`; as many layer sets as layers, at most two.
  layers_minus1: u8,
  /// Timing information and one set of HRD parameters (NAL parameters, one
  /// CPB).
  hrd: bool,
  /// The extension's `scalability_mask_flag`; no extension where `None`.
  mask: Option<u16>,
  /// The auxiliary type's `dimension_id`: 1 alpha, 2 depth.
  aux_id: u8,
  /// `layer_id_in_nuh[1]`, written where given; FFmpeg takes 1 where not.
  layer_id: Option<u8>,
  /// `vps_base_layer_internal_flag`, which FFmpeg requires.
  base_internal: bool,
  /// `chroma_and_bit_depth_vps_present_flag`, which FFmpeg requires.
  chroma_and_depth: bool,
  /// The extension cut short before its profile and mask.
  cut: bool,
}

impl Vps {
  /// One layer, no timing, no extension.
  const ONE: Self = Self {
    layers_minus1: 0,
    hrd: false,
    mask: None,
    aux_id: 1,
    layer_id: None,
    base_internal: true,
    chroma_and_depth: true,
    cut: false,
  };

  /// Two layers, the extension's mask `mask`.
  const fn two(mask: u16) -> Self {
    Self {
      layers_minus1: 1,
      mask: Some(mask),
      ..Self::ONE
    }
  }
}

/// An HEVC video parameter set NAL unit for `max_layers_minus1` + 1 layers
/// and one sub-layer, Main profile at level 3.1: where `hrd`, with timing
/// information and one set of HRD parameters (NAL parameters, one CPB);
/// where `mask` is given, with the VPS extension FFmpeg 9 reads whole, its
/// `scalability_mask_flag` `mask` and the auxiliary type alpha, cut short
/// where `cut` before the extension's profile and mask.
fn vps(max_layers_minus1: u8, hrd: bool, mask: Option<u16>, cut: bool) -> Vec<u8> {
  vps_of(Vps {
    layers_minus1: max_layers_minus1,
    hrd,
    mask,
    cut,
    ..Vps::ONE
  })
}

/// The video parameter set NAL unit `of` describes.
fn vps_of(of: Vps) -> Vec<u8> {
  const AUXILIARY: u16 = 1 << (15 - 3);
  let max_layers_minus1 = of.layers_minus1;
  let mut bits = String::from("0000"); // vps_video_parameter_set_id
  // vps_base_layer_internal_flag, vps_base_layer_available_flag
  bits += if of.base_internal { "11" } else { "01" };
  bits += &format!("{max_layers_minus1:06b}");
  bits += "000"; // vps_max_sub_layers_minus1
  bits += "1"; // vps_temporal_id_nesting_flag
  bits += &"1".repeat(16); // vps_reserved_0xffff_16bits
  // profile_tier_level(1, 0): space, tier, Main, compatible with Main and
  // Main 10, progressive frames, 43 reserved bits and one, level 93.
  bits += "00000001";
  bits += "01100000000000000000000000000000";
  bits += "1001";
  bits += &"0".repeat(44);
  bits += "01011101";
  bits += "1"; // vps_sub_layer_ordering_info_present_flag
  bits += &(ue(4) + &ue(2) + &ue(0));
  bits += &format!("{max_layers_minus1:06b}"); // vps_max_layer_id
  let layer_sets_minus1 = u32::from(max_layers_minus1 > 0);
  bits += &ue(layer_sets_minus1);
  for _ in 0..layer_sets_minus1 {
    bits += &"1".repeat(usize::from(max_layers_minus1) + 1); // layer_id_included_flag
  }
  if of.hrd {
    bits += "1"; // vps_timing_info_present_flag
    bits += &format!("{:032b}{:032b}", 1001u32, 30000u32);
    bits += "0"; // vps_poc_proportional_to_timing_flag
    bits += &ue(1); // vps_num_hrd_parameters
    bits += &ue(0); // hrd_layer_set_idx
    // hrd_parameters(1, 0): NAL parameters only, no sub-picture parameters,
    // scales, three lengths; then the one sub-layer's fixed rate, its
    // duration, one CPB and its NAL bit rate, size and CBR flag.
    bits += "100";
    bits += "00000000";
    bits += "101111011110111";
    bits += &("1".to_owned() + &ue(0) + &ue(0) + &ue(2) + &ue(2) + "0");
  } else {
    bits += "0";
  }
  match of.mask {
    Some(mask) => {
      bits += "1"; // vps_extension_flag
      while bits.len() % 8 != 0 {
        bits += "1"; // vps_extension_alignment_bit_equal_to_one
      }
      if !of.cut {
        bits += "01011101"; // profile_tier_level(0, 0)
        bits += "0"; // splitting_flag
        bits += &format!("{mask:016b}");
        let types = mask.count_ones() as usize;
        bits += &"001".repeat(types); // dimension_id_len_minus1: two bits each
        match of.layer_id {
          Some(id) => bits += &format!("1{id:06b}"), // vps_nuh_layer_id_present_flag
          None => bits += "0",
        }
        // dimension_id[1][j], in the mask's order from its first type: the
        // multiview type's view order index 1, the auxiliary type's AuxId.
        for index in 0..16 {
          if mask & (1 << (15 - index)) != 0 {
            let dimension = if 1 << (15 - index) == AUXILIARY {
              of.aux_id
            } else {
              1
            };
            bits += &format!("{dimension:02b}");
          }
        }
        bits += "0000"; // view_id_len
        bits += "1"; // direct_dependency_flag[1][0]
        bits += "0"; // vps_sub_layers_max_minus1_present_flag
        bits += "0"; // max_tid_ref_present_flag
        bits += "0"; // default_ref_layers_active_flag
        bits += &ue(0); // vps_num_profile_tier_level_minus1
        bits += &ue(0); // num_add_olss
        bits += "00"; // default_output_layer_idc
        bits += &ue(0); // vps_num_rep_formats_minus1
        bits += &format!("{:016b}{:016b}", 128u16, 96u16); // the picture's size
        if of.chroma_and_depth {
          // chroma_and_bit_depth_vps_present_flag, 4:2:0, eight bits each.
          bits += "1";
          bits += "01";
          bits += "00000000";
        } else {
          bits += "0";
        }
        bits += "0"; // conformance_window_vps_flag
        bits += "00"; // max_one_active_ref_layer_flag, vps_poc_lsb_aligned_flag
        bits += "0"; // sub_layer_flag_info_present_flag
        // max_vps_dec_pic_buffering_minus1 for both output layers,
        // max_vps_num_reorder_pics, max_vps_latency_increase_plus1.
        bits += &ue(0).repeat(4);
        bits += &ue(0); // direct_dep_type_len_minus2
        bits += "0"; // direct_dependency_all_layers_flag
        bits += &ue(0); // vps_non_vui_extension_length
        bits += "0"; // vps_vui_present_flag
      }
    }
    None => bits += "0", // vps_extension_flag
  }
  bits += "1"; // rbsp_stop_one_bit
  [vec![0x40, 0x01], rbsp(&bits)].concat()
}

/// An `hvcC` record with four-byte NAL length fields whose one array holds
/// `units`.
fn hvcc_holding(units: &[&[u8]]) -> Vec<u8> {
  let mut record = hvcc();
  record[22] = 1;
  record.push(0x20);
  record.extend_from_slice(&(units.len() as u16).to_be_bytes());
  for unit in units {
    record.extend_from_slice(&(unit.len() as u16).to_be_bytes());
    record.extend_from_slice(unit);
  }
  record
}

/// LAW (pre-R14 row 4; restated by Codex R14, [medium]): **an HEVC stream
/// whose video parameter set declares an auxiliary layer FFmpeg decodes as
/// alpha has no clean point and no anchor; one FFmpeg refuses, or whose
/// extension it ignores, declares none.** FFmpeg 9's HEVC decoder decodes
/// the auxiliary layer of a two-layer stream beside the base one, as the
/// alpha plane of every picture (`ff_hevc_is_alpha_video`), so a base-layer
/// IDR proves nothing of where that layer's pictures start. The set is read
/// as `ff_hevc_decode_nal_vps` and `decode_vps_ext` read it, off an `hvcC`
/// record, off start-coded extradata, and off a packet's own units, in
/// either packing. Declaring: two layers whose extension sets the auxiliary
/// type (3) — alone, with HRD parameters before the extension, beside the
/// multiview type as `x265` writes its alpha streams — and one whose
/// auxiliary type is depth (2), which FFmpeg's "broken VPS extension" road
/// keeps as alpha video; each leaves an IDR starting its picture neither
/// clean nor an anchor. Declaring none, the IDR clean and a definitive
/// anchor: one layer, with or without HRD parameters; two in multiview
/// (MV-HEVC); three layers with an auxiliary mask, an extension FFmpeg
/// ignores, decoding the base layer alone; two cut short before the
/// extension's mask, which FFmpeg reads past the unit; the second layer's
/// `nuh_layer_id` 0; a set without its base layer internal, which FFmpeg
/// refuses; an extension without its rep format's chroma and bit depth,
/// which FFmpeg refuses. Read without the VPS, every declaring one was a
/// clean point and an anchor; read by the reading of the mask alone, the
/// three-layer, the cut, the layer-0 and both refused sets declared alpha.
#[test]
fn an_hevc_stream_declaring_an_auxiliary_layer_has_no_clean_point_and_no_anchor() {
  const MULTIVIEW: u16 = 1 << (15 - 1);
  const AUXILIARY: u16 = 1 << (15 - 3);
  let idr = [0x26, 0x01, 0xaf];
  for (name, unit, declares) in [
    ("one layer", vps(0, false, None, false), false),
    (
      "one layer, HRD parameters",
      vps(0, true, None, false),
      false,
    ),
    ("multiview", vps(1, false, Some(MULTIVIEW), false), false),
    (
      "multiview, HRD parameters",
      vps(1, true, Some(MULTIVIEW), false),
      false,
    ),
    ("auxiliary", vps(1, false, Some(AUXILIARY), false), true),
    (
      "auxiliary, HRD parameters",
      vps(1, true, Some(AUXILIARY), false),
      true,
    ),
    (
      "multiview and auxiliary",
      vps(1, false, Some(MULTIVIEW | AUXILIARY), false),
      true,
    ),
    (
      "auxiliary, of depth",
      vps_of(Vps {
        aux_id: 2,
        ..Vps::two(AUXILIARY)
      }),
      true,
    ),
    (
      "three layers, an auxiliary mask",
      vps(2, false, Some(AUXILIARY), false),
      false,
    ),
    (
      "two layers, cut short",
      vps(1, false, Some(AUXILIARY), true),
      false,
    ),
    (
      "auxiliary, its second layer id 0",
      vps_of(Vps {
        layer_id: Some(0),
        ..Vps::two(AUXILIARY)
      }),
      false,
    ),
    (
      "auxiliary, its base layer not internal",
      vps_of(Vps {
        base_internal: false,
        ..Vps::two(AUXILIARY)
      }),
      false,
    ),
    (
      "auxiliary, its rep format without chroma and bit depth",
      vps_of(Vps {
        chroma_and_depth: false,
        ..Vps::two(AUXILIARY)
      }),
      false,
    ),
  ] {
    for (rule, pack) in [
      (
        KeyframeRule::of(CodecId::HEVC.raw(), &hvcc_holding(&[&unit[..]])),
        length_prefixed as fn(&[&[u8]]) -> Vec<u8>,
      ),
      (
        KeyframeRule::of(CodecId::HEVC.raw(), &annex_b(&[&unit[..]])),
        annex_b,
      ),
    ] {
      assert_eq!(
        rule.declares_alpha(),
        declares,
        "{name}: {rule:?}, off the extradata"
      );
      let au = pack(&[&idr[..]]);
      assert_eq!(
        (rule.is_clean(&au), rule.anchor(&au).map(Anchor::definitive)),
        if declares {
          (false, None)
        } else {
          (true, Some(true))
        },
        "{name}: {rule:?}, an IDR under the extradata's VPS"
      );
    }
    for nal_length in [None, Some(4)] {
      let rule = KeyframeRule::Hevc {
        nal_length,
        alpha: false,
      };
      let au = match nal_length {
        None => annex_b(&[&unit[..], &idr[..]]),
        Some(_) => length_prefixed(&[&unit[..], &idr[..]]),
      };
      assert_eq!(
        rule.units_declare_alpha(&au),
        declares,
        "{name}: {rule:?}, in band"
      );
      assert_eq!(
        (rule.is_clean(&au), rule.anchor(&au).map(Anchor::definitive)),
        if declares {
          (false, None)
        } else {
          (true, Some(true))
        },
        "{name}: {rule:?}, an IDR behind its own VPS"
      );
    }
  }
}

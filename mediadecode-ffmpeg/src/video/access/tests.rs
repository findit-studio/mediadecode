//! The keyframe reading, over hand-built access units.

use super::KeyframeRule;
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
    assert!(rule.is_clean(&pack(&idr), true), "{rule:?}: the IDR");
    assert!(
      !rule.is_clean(&pack(&recovery), true),
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
      !rule.is_clean(&pack(&picture_first), true),
      "{rule:?}: a picture before the IDR"
    );
    assert!(
      rule.is_clean(&pack(&idr_first), true),
      "{rule:?}: the IDR first"
    );
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
    assert!(rule.is_clean(&picture(kind), true), "NAL type {kind}");
  }
  assert!(!rule.is_clean(&picture(21), true), "a CRA");

  let rule = KeyframeRule::of(CodecId::HEVC.raw(), &hvcc());
  assert_eq!(
    rule,
    KeyframeRule::Hevc {
      nal_length: Some(4)
    }
  );
  assert!(rule.is_clean(&length_prefixed(&[&[19 << 1, 1, 0xaf]]), true));
  assert!(!rule.is_clean(&length_prefixed(&[&[21 << 1, 1, 0xaf]]), true));
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
    assert!(!hevc.is_clean(&data, true), "HEVC: {why}");
  }
  assert!(
    hevc.is_clean(&annex_b(&[&[19 << 1, 1, 0xaf]]), true),
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
    assert!(!h264.is_clean(&data, true), "H.264: {why}");
  }
  let avcc = KeyframeRule::of(CodecId::H264.raw(), &AVCC);
  assert!(!avcc.is_clean(&[0, 0, 0, 0], true), "an empty unit");
}

/// **Bytes that do not parse prove nothing**: a length field running past
/// the end is not clean.
#[test]
fn a_truncated_unit_is_not_clean() {
  let rule = KeyframeRule::of(CodecId::H264.raw(), &AVCC);
  let mut idr = length_prefixed(&[&[0x65, 0x88, 0x84]]);
  idr.truncate(idr.len() - 1);
  assert!(!rule.is_clean(&idr, false));
}

/// **The other codecs**: VP8, VP9 and AV1 keyframes reset every reference;
/// anything else is clean only while its decoder reorders nothing.
#[test]
fn the_other_codecs_are_read_by_their_references_and_their_reordering() {
  for codec in [CodecId::VP8, CodecId::VP9, CodecId::AV1] {
    assert!(KeyframeRule::of(codec.raw(), &[]).is_clean(&[], true));
  }
  let mpeg2 = KeyframeRule::of(CodecId::MPEG2VIDEO.raw(), &[]);
  assert_eq!(mpeg2, KeyframeRule::Reordering);
  assert!(mpeg2.is_clean(&[], false));
  assert!(!mpeg2.is_clean(&[], true));
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
    assert!(!rule.is_clean(&data, true), "{rule:?}: {why}");
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
    assert!(rule.is_clean(&data, true), "{rule:?}: {why}");
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
    let (clean, allocations, bytes) = crate::test_alloc::measured(|| rule.is_clean(data, true));
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
  let mut payload = Vec::new();
  for chunk in bits.as_bytes().chunks(8) {
    let mut byte = 0u8;
    for (index, &bit) in chunk.iter().enumerate() {
      if bit == b'1' {
        byte |= 0x80 >> index;
      }
    }
    payload.push(byte);
  }
  let mut raw = Vec::new();
  let mut zeros = 0;
  for byte in payload {
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

/// LAW (Codex R6 row 1, [high]): **a resync anchor is a packet whose first
/// picture the bitstream proves a random-access one.** H.264: an IDR slice
/// anchors, and so does a non-IDR slice whose header says I (the I picture
/// of an open GOP, a recovery point) or SI — read through an emulation
/// prevention byte where the header holds one — while a packet whose first
/// slice is P or B does not, though an IDR follows it in the packet. HEVC:
/// every IRAP picture anchors, a CRA among them, while a trailing or a RASL
/// picture first does not. Every other codec takes the key flag as FFmpeg's
/// parser set it.
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
  let sei: &[u8] = &[0x06, 6, 1, 0x80];
  for (units, anchors, why) in [
    (vec![&idr[..]], true, "an IDR slice"),
    (vec![sei, &i_slice[..]], true, "a recovery point's I slice"),
    (vec![&si_slice[..]], true, "an SI slice"),
    (
      vec![&far_i_slice[..]],
      true,
      "an I slice read through `00 00 03`",
    ),
    (vec![&p_slice[..]], false, "a P slice, a stale key flag"),
    (vec![&b_slice[..]], false, "a B slice"),
    (
      vec![&p_slice[..], &idr[..]],
      false,
      "a P slice before the IDR",
    ),
  ] {
    assert_eq!(h264.anchors(&annex_b(&units)), anchors, "H.264: {why}");
  }

  let hevc = KeyframeRule::of(CodecId::HEVC.raw(), &[]);
  let picture = |kind: u8| -> Vec<u8> { vec![kind << 1, 1, 0xaf] };
  for (kinds, anchors, why) in [
    (vec![21u8], true, "a CRA"),
    (vec![19], true, "an IDR"),
    (vec![16], true, "a BLA"),
    (vec![1], false, "a trailing picture"),
    (vec![8], false, "a RASL picture"),
    (vec![1, 21], false, "a trailing picture before the CRA"),
  ] {
    let units: Vec<Vec<u8>> = kinds.iter().map(|&kind| picture(kind)).collect();
    let units: Vec<&[u8]> = units.iter().map(Vec::as_slice).collect();
    assert_eq!(hevc.anchors(&annex_b(&units)), anchors, "HEVC: {why}");
  }

  for codec in [CodecId::MPEG4, CodecId::VP9, CodecId::AV1] {
    assert!(
      KeyframeRule::of(codec.raw(), &[]).anchors(&[]),
      "{codec:?}: the key flag stands"
    );
  }
}

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

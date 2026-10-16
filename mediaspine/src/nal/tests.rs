use std::{vec, vec::Vec};

use super::*;

/// An H.264 NAL unit: its header byte, then `payload`.
fn avc(nal_ref_idc: u8, nal_unit_type: u8, payload: &[u8]) -> Vec<u8> {
  let mut nal = vec![(nal_ref_idc << 5) | nal_unit_type];
  nal.extend_from_slice(payload);
  nal
}

/// A non-reference H.264 slice.
fn b_slice() -> Vec<u8> {
  avc(0, 1, &[0x88, 0x84, 0x21])
}

/// A reference H.264 slice.
fn p_slice() -> Vec<u8> {
  avc(2, 1, &[0x9A, 0x02, 0x10])
}

/// An HEVC NAL unit: its two header bytes, then `payload`.
fn hevc(nal_unit_type: u8, layer: u8, temporal_id: u8, payload: &[u8]) -> Vec<u8> {
  let mut nal = vec![
    (nal_unit_type << 1) | (layer >> 5),
    ((layer & 0x1F) << 3) | (temporal_id + 1),
  ];
  nal.extend_from_slice(payload);
  nal
}

/// An HEVC SPS declaring `max_sub_layers_minus1`.
fn hevc_sps(max_sub_layers_minus1: u8) -> Vec<u8> {
  hevc(33, 0, 0, &[(max_sub_layers_minus1 << 1) | 1, 0x01, 0x60])
}

/// An HEVC VPS declaring `max_sub_layers_minus1`.
fn hevc_vps(max_sub_layers_minus1: u8) -> Vec<u8> {
  hevc(32, 0, 0, &[0x0C, 0x01 | (max_sub_layers_minus1 << 1), 0xFF])
}

/// `nals` each after a big-endian length of `size` bytes.
fn lengths(size: usize, nals: &[Vec<u8>]) -> Vec<u8> {
  let mut out = Vec::new();
  for nal in nals {
    let length = nal.len().to_be_bytes();
    out.extend_from_slice(&length[length.len() - size..]);
    out.extend_from_slice(nal);
  }
  out
}

/// `nals` each after a start code: four bytes for the first, three after.
fn annex_b(nals: &[Vec<u8>]) -> Vec<u8> {
  let mut out = Vec::new();
  for (index, nal) in nals.iter().enumerate() {
    out.extend_from_slice(if index == 0 {
      &[0, 0, 0, 1]
    } else {
      &[0, 0, 1]
    });
    out.extend_from_slice(nal);
  }
  out
}

/// An `avcC` of one SPS and one PPS, its `lengthSizeMinusOne` given.
fn avcc(length_size_minus_one: u8) -> Vec<u8> {
  let sps = avc(3, 7, &[0x64, 0x00, 0x28]);
  let pps = avc(3, 8, &[0xEE, 0x3C, 0x80]);
  let mut record = vec![1, 0x64, 0x00, 0x28, 0xFC | length_size_minus_one, 0xE1];
  record.extend_from_slice(&(sps.len() as u16).to_be_bytes());
  record.extend_from_slice(&sps);
  record.push(1);
  record.extend_from_slice(&(pps.len() as u16).to_be_bytes());
  record.extend_from_slice(&pps);
  record
}

/// An `hvcC` of one array holding `nals`, its `numTemporalLayers` and
/// `lengthSizeMinusOne` given.
fn hvcc(num_temporal_layers: u8, length_size_minus_one: u8, nals: &[Vec<u8>]) -> Vec<u8> {
  let mut record = vec![0; 23];
  record[0] = 1;
  record[21] = (num_temporal_layers << 3) | length_size_minus_one;
  record[22] = 1;
  record.push(0x80 | 33);
  record.extend_from_slice(&(nals.len() as u16).to_be_bytes());
  for nal in nals {
    record.extend_from_slice(&(nal.len() as u16).to_be_bytes());
    record.extend_from_slice(nal);
  }
  record
}

fn kept(reason: KeptReason) -> Verdict {
  Verdict::Kept(reason)
}

#[test]
fn an_avc_record_frames_lengths_of_one_two_and_four_bytes_and_refuses_three() {
  for (minus_one, size) in [(0, 1), (1, 2), (3, 4)] {
    let mut stream = NalStream::new(NalCodec::H264, &avcc(minus_one));
    let framing = match size {
      1 => Framing::Length(LengthSize::One),
      2 => Framing::Length(LengthSize::Two),
      _ => Framing::Length(LengthSize::Four),
    };
    assert_eq!(stream.framing(), Some(framing), "{size}");
    assert_eq!(
      stream.read(&lengths(size, &[b_slice()])),
      Verdict::Disposable,
      "{size}"
    );
    assert_eq!(
      stream.read(&lengths(size, &[p_slice()])),
      kept(KeptReason::Referenced),
      "{size}"
    );
  }
  // `lengthSizeMinusOne` 2, a three-byte length, frames nothing a decoder
  // reads; and a record that ends before its parameter sets do.
  let mut three = NalStream::new(NalCodec::H264, &avcc(2));
  assert_eq!(three.framing(), None);
  assert_eq!(
    three.read(&[0, 0, 4, 1, 2, 3, 4]),
    kept(KeptReason::Unframed)
  );
  let record = avcc(3);
  let short = NalStream::new(NalCodec::H264, &record[..record.len() - 1]);
  assert_eq!(short.framing(), None);
  assert_eq!(NalStream::new(NalCodec::H264, &record[..6]).framing(), None);
}

#[test]
fn annex_b_reads_three_and_four_byte_start_codes_and_leading_and_trailing_zeros() {
  // No extradata, or extradata that is no `avcC`, is Annex B.
  for extradata in [&[][..], &annex_b(&[avc(3, 7, &[0x64])])[..]] {
    let mut stream = NalStream::new(NalCodec::H264, extradata);
    assert_eq!(stream.framing(), Some(Framing::AnnexB));
    let aud = avc(0, 9, &[0xF0]);
    let mut packet = vec![0, 0];
    packet.extend_from_slice(&annex_b(&[aud, b_slice(), b_slice()]));
    packet.extend_from_slice(&[0, 0, 0]);
    assert_eq!(stream.read(&packet), Verdict::Disposable);
    assert_eq!(
      stream.read(&annex_b(&[b_slice(), p_slice()])),
      kept(KeptReason::Referenced)
    );
  }
  let mut stream = NalStream::new(NalCodec::H264, &[]);
  // A byte other than zero before the first start code, no start code at
  // all, and a start code with nothing after it.
  assert_eq!(
    stream.read(&[7, 0, 0, 1, 0x01]),
    kept(KeptReason::Malformed)
  );
  assert_eq!(stream.read(&[0x01, 0x88]), kept(KeptReason::Malformed));
  assert_eq!(
    stream.read(&[0, 0, 1, 0x01, 0x88, 0, 0, 1]),
    kept(KeptReason::Malformed)
  );
}

#[test]
fn an_h264_packet_is_disposable_only_where_every_slice_is_non_reference_and_none_is_idr() {
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  // SEI, an access unit delimiter and filler data ride beside the slices.
  let sei = avc(0, 6, &[0x05, 0x01, 0x80]);
  let aud = avc(0, 9, &[0xF0]);
  let filler = avc(0, 12, &[0xFF, 0x80]);
  assert_eq!(
    read(&mut stream, &[aud.clone(), sei.clone(), b_slice(), filler]),
    Verdict::Disposable
  );
  // A field pair: both fields of `nal_ref_idc` 0, or one a reference.
  assert_eq!(
    read(&mut stream, &[b_slice(), b_slice()]),
    Verdict::Disposable
  );
  assert_eq!(
    read(&mut stream, &[b_slice(), p_slice()]),
    kept(KeptReason::Referenced)
  );
  // Slice data partitions are slices too.
  assert_eq!(
    read(&mut stream, &[avc(0, 2, &[0x88]), avc(0, 3, &[0x88])]),
    Verdict::Disposable
  );
  assert_eq!(
    read(&mut stream, &[avc(0, 2, &[0x88]), avc(1, 4, &[0x88])]),
    kept(KeptReason::Referenced)
  );
  // An IDR slice, whatever its `nal_ref_idc`.
  assert_eq!(
    read(&mut stream, &[avc(3, 5, &[0x88])]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(
    read(&mut stream, &[avc(0, 5, &[0x88])]),
    kept(KeptReason::RandomAccess)
  );
  // No slice at all.
  assert_eq!(read(&mut stream, &[aud, sei]), kept(KeptReason::NoPicture));
}

#[test]
fn a_parameter_set_or_an_end_beside_a_non_reference_slice_keeps_the_packet() {
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  for nal_unit_type in [7, 8, 10, 11, 13, 15, 16, 19, 0, 23, 31] {
    assert_eq!(
      stream.read(&lengths(4, &[avc(3, nal_unit_type, &[0x42]), b_slice()])),
      kept(KeptReason::Companion(nal_unit_type)),
      "{nal_unit_type}"
    );
  }
  // The scalable, multiview and 3-D extensions.
  for nal_unit_type in [14, 20, 21] {
    assert_eq!(
      stream.read(&lengths(4, &[avc(0, nal_unit_type, &[0x42]), b_slice()])),
      kept(KeptReason::Extension),
      "{nal_unit_type}"
    );
  }
}

#[test]
fn a_packet_that_does_not_parse_is_kept() {
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  // A length of 0, one past the end, and bytes too few for a length.
  assert_eq!(
    stream.read(&[0, 0, 0, 0, 0x01, 0x88]),
    kept(KeptReason::Malformed)
  );
  assert_eq!(
    stream.read(&[0, 0, 0, 9, 0x01, 0x88]),
    kept(KeptReason::Malformed)
  );
  let mut packet = lengths(4, &[b_slice()]);
  packet.extend_from_slice(&[0, 0]);
  assert_eq!(stream.read(&packet), kept(KeptReason::Malformed));
  // The forbidden bit.
  assert_eq!(
    stream.read(&lengths(4, &[vec![0x81, 0x88]])),
    kept(KeptReason::Malformed)
  );
  // A malformed unit after a keeping one is still malformed: the packet
  // does not parse.
  assert_eq!(
    stream.read(&lengths(4, &[p_slice(), vec![0x81, 0x88]])),
    kept(KeptReason::Malformed)
  );
  // An empty packet holds no picture.
  assert_eq!(stream.read(&[]), kept(KeptReason::NoPicture));
}

#[test]
fn a_four_byte_length_track_reads_an_annex_b_packet_as_ffmpeg_does() {
  // `h264dec.c` 602–607: a packet that begins `00 00 00 01` whose next four
  // bytes are no length that fits is Annex B, and so are the packets after
  // it until one begins with a length that fits.
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  let annex = annex_b(&[avc(0, 9, &[0xF0]), b_slice()]);
  assert_eq!(stream.read(&annex), Verdict::Disposable);
  assert_eq!(stream.framing(), Some(Framing::AnnexB));
  assert_eq!(stream.read(&lengths(4, &[b_slice()])), Verdict::Disposable);
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Four)));
  // Two-byte lengths are read as declared.
  let mut two = NalStream::new(NalCodec::H264, &avcc(1));
  assert_eq!(two.read(&annex), kept(KeptReason::Malformed));
}

#[test]
fn a_new_configuration_record_changes_the_framing() {
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  assert_eq!(stream.read(&lengths(4, &[b_slice()])), Verdict::Disposable);
  stream.reconfigure(&avcc(1));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  assert_eq!(stream.read(&lengths(2, &[b_slice()])), Verdict::Disposable);
  stream.reconfigure(&[]);
  assert_eq!(stream.framing(), Some(Framing::AnnexB));
  assert_eq!(stream.read(&annex_b(&[b_slice()])), Verdict::Disposable);
}

#[test]
fn an_hevc_slnr_picture_is_disposable_only_at_the_highest_sub_layer() {
  // Three sub-layers, from the SPS in the record.
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 3, &[hevc_sps(2)]));
  assert_eq!(stream.highest_sub_layer(), Some(2));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  let aud = hevc(35, 0, 2, &[0x50]);
  let sei = hevc(39, 0, 2, &[0x05, 0x01, 0x80]);
  for nal_unit_type in [0, 2, 4, 6, 8, 10, 12, 14] {
    assert_eq!(
      read(
        &mut stream,
        &[aud.clone(), sei.clone(), hevc(nal_unit_type, 0, 2, &[0xAF])]
      ),
      Verdict::Disposable,
      "{nal_unit_type}"
    );
    // Below it, a higher sub-layer may reference the picture.
    for below in [0, 1] {
      assert_eq!(
        read(&mut stream, &[hevc(nal_unit_type, 0, below, &[0xAF])]),
        kept(KeptReason::BelowHighestSubLayer),
        "{nal_unit_type} at {below}"
      );
    }
  }
  // The sub-layer reference types, and IRAP.
  for nal_unit_type in [1, 3, 5, 7, 9, 11, 13, 15] {
    assert_eq!(
      read(&mut stream, &[hevc(nal_unit_type, 0, 2, &[0xAF])]),
      kept(KeptReason::Referenced),
      "{nal_unit_type}"
    );
  }
  for nal_unit_type in 16..=23 {
    assert_eq!(
      read(&mut stream, &[hevc(nal_unit_type, 0, 0, &[0xAF])]),
      kept(KeptReason::RandomAccess),
      "{nal_unit_type}"
    );
  }
  // A TemporalId past every declared sub-layer, and one of -1.
  assert_eq!(
    read(&mut stream, &[hevc(0, 0, 3, &[0xAF])]),
    kept(KeptReason::Malformed)
  );
  assert_eq!(
    read(&mut stream, &[vec![0x00, 0x00, 0xAF]]),
    kept(KeptReason::Malformed)
  );
}

#[test]
fn a_single_sub_layer_stream_marks_its_slnr_pictures() {
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(1, 3, &[hevc_sps(0)]));
  assert_eq!(stream.highest_sub_layer(), Some(0));
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    Verdict::Disposable
  );
}

#[test]
fn the_highest_sub_layer_is_learned_from_every_parameter_set_and_only_rises() {
  // No SPS yet: nothing is disposable, whatever the record's
  // `numTemporalLayers` says.
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(1, 3, &[]));
  assert_eq!(stream.highest_sub_layer(), None);
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::SubLayersUnknown)
  );
  // An in-band SPS of one sub-layer: the packet carrying it is kept, the
  // next is read.
  assert_eq!(
    stream.read(&lengths(4, &[hevc_sps(0), hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::Companion(33))
  );
  assert_eq!(stream.highest_sub_layer(), Some(0));
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    Verdict::Disposable
  );
  // A VPS of three raises it; a later SPS of one does not lower it.
  let _ = stream.read(&lengths(4, &[hevc_vps(2)]));
  assert_eq!(stream.highest_sub_layer(), Some(2));
  let _ = stream.read(&lengths(4, &[hevc_sps(0)]));
  assert_eq!(stream.highest_sub_layer(), Some(2));
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::BelowHighestSubLayer)
  );
  // The record's `numTemporalLayers` raises it too, once an SPS is known.
  let mut record = NalStream::new(NalCodec::Hevc, &hvcc(4, 3, &[hevc_sps(0)]));
  assert_eq!(record.highest_sub_layer(), Some(3));
  assert_eq!(
    record.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::BelowHighestSubLayer)
  );
  // A field past 6 does not parse.
  assert_eq!(
    stream.read(&lengths(4, &[hevc_sps(7)])),
    kept(KeptReason::Malformed)
  );
}

#[test]
fn hevc_annex_b_extradata_and_in_band_sets_are_read() {
  let extradata = annex_b(&[hevc_vps(0), hevc_sps(1), hevc(34, 0, 0, &[0xC1])]);
  let mut stream = NalStream::new(NalCodec::Hevc, &extradata);
  assert_eq!(stream.framing(), Some(Framing::AnnexB));
  assert_eq!(stream.highest_sub_layer(), Some(1));
  assert_eq!(
    stream.read(&annex_b(&[hevc(35, 0, 1, &[0x50]), hevc(0, 0, 1, &[0xAF])])),
    Verdict::Disposable
  );
}

#[test]
fn an_hevc_layer_above_0_or_a_companion_unit_keeps_the_packet() {
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 3, &[hevc_sps(0)]));
  let slnr = hevc(0, 0, 0, &[0xAF]);
  assert_eq!(
    stream.read(&lengths(4, &[slnr.clone(), hevc(0, 1, 0, &[0xAF])])),
    kept(KeptReason::Extension)
  );
  for nal_unit_type in [32, 33, 34, 36, 37, 24, 31, 41, 47, 48, 62, 63] {
    let unit = match nal_unit_type {
      32 => hevc_vps(0),
      33 => hevc_sps(0),
      other => hevc(other, 0, 0, &[0x42, 0x42]),
    };
    assert_eq!(
      stream.read(&lengths(4, &[slnr.clone(), unit])),
      kept(KeptReason::Companion(nal_unit_type)),
      "{nal_unit_type}"
    );
  }
  for beside in [35, 38, 39, 40] {
    assert_eq!(
      stream.read(&lengths(4, &[hevc(beside, 0, 0, &[0x50]), slnr.clone()])),
      Verdict::Disposable,
      "{beside}"
    );
  }
}

#[test]
fn an_hevc_record_is_detected_as_ffmpeg_detects_it() {
  // At least 23 bytes, its first byte 1 — or 0, with a nonzero second byte
  // or a third above 1 — else Annex B.
  let sps = [hevc_sps(0)];
  assert_eq!(
    NalStream::new(NalCodec::Hevc, &hvcc(0, 1, &sps)).framing(),
    Some(Framing::Length(LengthSize::Two))
  );
  let mut legacy = hvcc(0, 0, &sps);
  legacy[0] = 0;
  legacy[1] = 1;
  assert_eq!(
    NalStream::new(NalCodec::Hevc, &legacy).framing(),
    Some(Framing::Length(LengthSize::One))
  );
  legacy[1] = 0;
  legacy[2] = 1;
  assert_eq!(
    NalStream::new(NalCodec::Hevc, &legacy).framing(),
    Some(Framing::AnnexB)
  );
  // A length of three bytes, and a record that ends inside an array.
  assert_eq!(
    NalStream::new(NalCodec::Hevc, &hvcc(0, 2, &sps)).framing(),
    None
  );
  let whole = hvcc(0, 3, &sps);
  assert_eq!(
    NalStream::new(NalCodec::Hevc, &whole[..whole.len() - 1]).framing(),
    None
  );
}

/// SplitMix64, for the property below: fixed seeds, the same draws on
/// every platform.
struct Seeded(u64);

impl Seeded {
  fn next(&mut self) -> u64 {
    self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = self.0;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
  }

  fn below(&mut self, bound: u64) -> u64 {
    self.next() % bound
  }
}

#[test]
fn a_packet_holding_anything_that_may_be_needed_is_never_marked() {
  // Random H.264 and HEVC packets, length-framed and Annex B: whenever one
  // holds a reference slice, a random-access slice, a parameter set or any
  // unit but a non-reference slice at the highest sub-layer, SEI, a
  // delimiter or filler — or no slice — it is kept; else it is disposable.
  let mut marked = 0;
  let mut kept_ = 0;
  for seed in 0..512 {
    let mut draw = Seeded(seed);
    let hevc_track = draw.below(2) == 1;
    let annex = draw.below(2) == 1;
    let mut nals = Vec::new();
    let mut needed = false;
    let mut pictures = 0;
    for _ in 0..1 + draw.below(5) {
      // Most units are drawn from those that may ride in a disposable
      // packet, so both sides are drawn often; the rest from every type.
      let friendly = draw.below(5) != 0;
      if hevc_track {
        const RIDE: [u8; 12] = [0, 2, 4, 6, 8, 10, 12, 14, 35, 38, 39, 40];
        let nal_unit_type = if friendly {
          RIDE[draw.below(RIDE.len() as u64) as usize]
        } else {
          draw.below(64) as u8
        };
        let temporal_id = if friendly { 2 } else { draw.below(3) as u8 };
        let layer = if !friendly && draw.below(4) == 0 {
          1
        } else {
          0
        };
        let slnr = matches!(nal_unit_type, 0 | 2 | 4 | 6 | 8 | 10 | 12 | 14);
        let beside = matches!(nal_unit_type, 35 | 38 | 39 | 40);
        if layer != 0 || !(slnr && temporal_id == 2 || beside) {
          needed = true;
        }
        if slnr {
          pictures += 1;
        }
        // The units read here never include a parameter set that lowers
        // or raises the stream's three sub-layers.
        let nal = match nal_unit_type {
          32 => hevc_vps(2),
          33 => hevc_sps(2),
          other => hevc(other, layer, temporal_id, &[0xAF, 0x13]),
        };
        nals.push(nal);
      } else {
        const RIDE: [u8; 7] = [1, 2, 3, 4, 6, 9, 12];
        let nal_unit_type = if friendly {
          RIDE[draw.below(RIDE.len() as u64) as usize]
        } else {
          draw.below(32) as u8
        };
        let nal_ref_idc = if friendly { 0 } else { draw.below(4) as u8 };
        let slice = matches!(nal_unit_type, 1..=4);
        let beside = matches!(nal_unit_type, 6 | 9 | 12);
        if !(slice && nal_ref_idc == 0 || beside) {
          needed = true;
        }
        if slice {
          pictures += 1;
        }
        nals.push(avc(nal_ref_idc, nal_unit_type, &[0x88, 0x13]));
      }
    }
    if pictures == 0 {
      needed = true;
    }
    let (extradata, packet) = match (hevc_track, annex) {
      (false, false) => (avcc(3), lengths(4, &nals)),
      (false, true) => (Vec::new(), annex_b(&nals)),
      (true, false) => (hvcc(0, 3, &[hevc_sps(2)]), lengths(4, &nals)),
      (true, true) => (annex_b(&[hevc_sps(2)]), annex_b(&nals)),
    };
    let codec = if hevc_track {
      NalCodec::Hevc
    } else {
      NalCodec::H264
    };
    let verdict = NalStream::new(codec, &extradata).read(&packet);
    assert_eq!(
      verdict.is_disposable(),
      !needed,
      "seed {seed}: {nals:?} read {verdict:?}"
    );
    if verdict.is_disposable() {
      marked += 1;
    } else {
      kept_ += 1;
    }
  }
  // Both sides drawn.
  assert!(marked > 16 && kept_ > 16, "{marked} marked, {kept_} kept");
}

#[test]
fn a_kept_reason_says_why() {
  let shown = |reason: KeptReason| std::string::ToString::to_string(&reason);
  assert_eq!(
    shown(KeptReason::Companion(8)),
    "a NAL unit of type 8 a later picture may need"
  );
  assert_eq!(
    shown(KeptReason::BelowHighestSubLayer),
    "a picture below the highest temporal sub-layer"
  );
}

use super::*;

use mediadecode::decoder::AudioStreamDecoder;

use crate::FfmpegOwnedAudioStreamDecoder;

/// An encoded audio clip: its codec parameters and its packets in decode
/// order.
struct Clip {
  parameters: Parameters,
  packets: Vec<ffmpeg_next::Packet>,
}

/// `frames` frames of two tones at `rate` on `layout`, encoded in-process
/// by FFmpeg's own encoder for `id`, its configuration in the codec
/// parameters' extradata alone (`AV_CODEC_FLAG_GLOBAL_HEADER`).
fn encode(
  id: ffmpeg_next::codec::Id,
  rate: i32,
  layout: ffmpeg_next::ChannelLayout,
  frames: usize,
) -> Clip {
  use ffmpeg_next as ff;
  ff::init().expect("ffmpeg init");
  let codec = ff::codec::encoder::find(id).expect("an encoder for the codec");
  let ctx = ff::codec::context::Context::new_with_codec(codec);
  let mut enc = ctx.encoder().audio().expect("an audio encoder context");
  let format = ff::format::Sample::F32(ff::format::sample::Type::Planar);
  enc.set_rate(rate);
  enc.set_channel_layout(layout);
  enc.set_format(format);
  enc.set_time_base(ff::Rational::new(1, rate));
  enc.set_bit_rate(96_000);
  enc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
  let mut opened = enc.open_as(codec).expect("open the encoder");
  let parameters = Parameters::from(&opened);
  let samples = opened.frame_size() as usize;
  let mut packets = Vec::new();
  let drain = |opened: &mut ff::codec::encoder::audio::Encoder, out: &mut Vec<ff::Packet>| loop {
    let mut packet = ff::Packet::empty();
    if opened.receive_packet(&mut packet).is_err() {
      break;
    }
    out.push(packet);
  };
  for index in 0..frames {
    let mut frame = ff::frame::Audio::new(format, samples, layout);
    frame.set_rate(rate as u32);
    for plane in 0..frame.planes() {
      for (at, sample) in frame.plane_mut::<f32>(plane).iter_mut().enumerate() {
        let t = (index * samples + at) as f32 / rate as f32;
        *sample = 0.4 * (core::f32::consts::TAU * (440.0 + 110.0 * plane as f32) * t).sin();
      }
    }
    frame.set_pts(Some((index * samples) as i64));
    opened.send_frame(&frame).expect("send_frame");
    drain(&mut opened, &mut packets);
  }
  opened.send_eof().expect("send_eof");
  drain(&mut opened, &mut packets);
  assert!(packets.len() >= 4, "the clip has packets");
  Clip {
    parameters,
    packets,
  }
}

/// The extradata `parameters` carry.
fn extradata_of(parameters: &Parameters) -> Vec<u8> {
  // SAFETY: `parameters` owns a live `AVCodecParameters`, whose extradata
  // is null or `extradata_size` bytes long.
  unsafe {
    let raw = parameters.as_ptr();
    let size = usize::try_from((*raw).extradata_size).unwrap_or(0);
    if (*raw).extradata.is_null() || size == 0 {
      Vec::new()
    } else {
      core::slice::from_raw_parts((*raw).extradata, size).to_vec()
    }
  }
}

/// `packet` carrying `extradata` as `AV_PKT_DATA_NEW_EXTRADATA`.
fn with_new_extradata(mut packet: ffmpeg_next::Packet, extradata: &[u8]) -> ffmpeg_next::Packet {
  use ffmpeg_next::packet::Mut;
  // SAFETY: `packet` is a live packet this function owns; FFmpeg allocates
  // the side data, padded, and frees it with the packet; `extradata` is
  // copied into exactly the bytes it allocated.
  unsafe {
    let slot = crate::ffi::packet_new_side_data(
      packet.as_mut_ptr(),
      ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA as i32,
      extradata.len(),
    )
    .expect("new extradata attached");
    core::ptr::copy_nonoverlapping(extradata.as_ptr(), slot, extradata.len());
  }
  packet
}

/// `a`, then `b`, `b`'s timestamps after `a`'s, on `a`'s codec parameters:
/// `b`'s configuration carried as `AV_PKT_DATA_NEW_EXTRADATA` by a packet
/// with no body between them (`bodiless`), or by `b`'s first packet.
fn a_then_b(a: &Clip, b: &Clip, bodiless: bool) -> Clip {
  let record = extradata_of(&b.parameters);
  assert!(
    !record.is_empty(),
    "b's configuration is in its codec parameters"
  );
  let shift = a
    .packets
    .iter()
    .filter_map(|packet| packet.pts().map(|pts| pts + packet.duration()))
    .max()
    .unwrap_or(0);
  let mut packets = a.packets.clone();
  if bodiless {
    packets.push(with_new_extradata(ffmpeg_next::Packet::empty(), &record));
  }
  for (index, packet) in b.packets.iter().enumerate() {
    let mut moved = packet.clone();
    moved.set_pts(packet.pts().map(|pts| pts + shift));
    moved.set_dts(packet.dts().map(|dts| dts + shift));
    if index == 0 && !bodiless {
      moved = with_new_extradata(moved, &record);
    }
    packets.push(moved);
  }
  Clip {
    parameters: a.parameters.clone(),
    packets,
  }
}

/// A frame as delivered: its rate, channels, samples and every plane's
/// bytes.
type Heard = (u32, u8, u32, Vec<Vec<u8>>);

/// What a session opened on `clip`'s parameters makes of its packets, every
/// packet sent until taken and every frame drained, an error answered
/// recorded and passed over; `before(index, dec)` runs ahead of each send.
fn session_hears(
  clip: &Clip,
  mut before: impl FnMut(usize, &FfmpegOwnedAudioStreamDecoder),
) -> (Vec<Heard>, Vec<String>) {
  let tb = Timebase::new(1, core::num::NonZeroI32::new(48_000).expect("nonzero"));
  let mut dec =
    FfmpegOwnedAudioStreamDecoder::open(clip.parameters.clone(), tb, DecoderLimits::default())
      .expect("the audio decoder opens");
  let mut frame = crate::empty_owned_audio_frame();
  let mut heard = Vec::new();
  let mut errors = Vec::new();
  let mut drain = |dec: &mut FfmpegOwnedAudioStreamDecoder,
                   heard: &mut Vec<Heard>,
                   errors: &mut Vec<String>| loop {
    match dec.receive_frame(&mut frame) {
      Ok(Received::Frame) => heard.push((
        frame.sample_rate(),
        frame.channel_count(),
        frame.nb_samples(),
        frame
          .planes()
          .iter()
          .map(|plane| plane.data_ref().as_ref().to_vec())
          .collect(),
      )),
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(error) => {
        errors.push(format!("receive: {error:?}"));
        break;
      }
    }
  };
  for (index, packet) in clip.packets.iter().enumerate() {
    before(index, &dec);
    let pushed = crate::boundary::audio_packet_from_ffmpeg(packet, Timebase::SECONDS)
      .expect("a wrappable payload")
      .expect("a packet");
    loop {
      match dec.send_packet(&pushed) {
        Ok(Sent::Accepted) => break,
        Ok(Sent::MustDrain) => drain(&mut dec, &mut heard, &mut errors),
        Err(error) => {
          errors.push(format!("send {index}: {error:?}"));
          break;
        }
      }
    }
    drain(&mut dec, &mut heard, &mut errors);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  drain(&mut dec, &mut heard, &mut errors);
  (heard, errors)
}

const NEW_EXTRADATA: i32 = ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA as i32;

/// LAW (R20 row 1; Codex R19 [high]): **an AAC configuration riding a packet
/// with no body applies to the next packet with one.** FFmpeg's AAC decoder
/// applies a packet's `AV_PKT_DATA_NEW_EXTRADATA` at the head of the packet
/// it decodes (`aac_decode_frame`, aac/aacdec.c:2572-2589), and never sees a
/// packet with no body: libavcodec refuses one whose empty body is not null
/// before it reads the side data (decode.c:742-743), and hands the decoder
/// one whose body is null as a packet of no bytes (745-748; `av_packet_ref`,
/// packet.c:452-460), which it fails. Two AAC streams of FFmpeg's own
/// encoder, 44.1 kHz mono then 22.05 kHz stereo, the second's
/// `AudioSpecificConfig` carried by a packet with no body between them: the
/// session takes it, defers the record onto the second stream's first
/// packet, and hears every frame as a straight decode of the record on that
/// packet — the second stream at 22.05 kHz in stereo. Rebuilt as R19
/// rebuilt it, the packet was refused `EINVAL` and the second stream decoded
/// under the first's configuration.
#[test]
fn an_aac_configuration_riding_a_packet_with_no_body_applies_to_the_next_packet() {
  use ffmpeg_next::{ChannelLayout, codec::Id};
  let a = encode(Id::AAC, 44_100, ChannelLayout::MONO, 12);
  let b = encode(Id::AAC, 22_050, ChannelLayout::STEREO, 12);
  let clip = a_then_b(&a, &b, true);
  let at = a.packets.len();
  assert!(clip.packets[at].data().is_none(), "{at} has no body");
  let (reference, errors) = session_hears(&a_then_b(&a, &b, false), |_, _| {});
  assert!(errors.is_empty(), "the straight decode: {errors:?}");
  assert!(
    reference
      .first()
      .is_some_and(|heard| (heard.0, heard.1) == (44_100, 1))
      && reference
        .last()
        .is_some_and(|heard| (heard.0, heard.1) == (22_050, 2)),
    "the premise: the record moves the decoder from 44.1 kHz mono to 22.05 kHz stereo"
  );
  let mut deferred = Vec::new();
  let (heard, errors) = session_hears(&clip, |index, dec| {
    if index == at + 1 || index == at + 2 {
      deferred.push(dec.deferred_kinds_for_test());
    }
  });
  assert!(
    errors.is_empty() && heard == reference,
    "every frame, as the straight decode of the record on the next packet: errors {errors:?}, \
     the second stream heard at {:?}",
    heard.last().map(|heard| (heard.0, heard.1))
  );
  assert_eq!(
    deferred,
    vec![vec![NEW_EXTRADATA], Vec::new()],
    "the record deferred onto the next packet, then taken with it"
  );
}

/// LAW (R20 row 1; Codex R19 [high]): **a decoder that reads an empty packet
/// as its last never sees one.** FFmpeg's WMA decoder takes a packet of no
/// bytes for the end of its stream — it outputs the samples its last
/// transform left and resets its superframe (`wma_decode_superframe`,
/// wmadec.c:845-862) — and libavcodec hands it one for a packet whose body
/// is null, a copy whose body is a buffer of size 0 (decode.c:745-748;
/// `av_packet_ref`, packet.c:452-460). A WMA v2 stream of FFmpeg's
/// own encoder with a packet with no body in its middle, carrying side data
/// the decoder does not read: the session hears every frame as a straight
/// decode of the stream without it. Handed over with no data, the decoder
/// heard an end in the middle of the stream.
#[test]
fn a_decoder_that_reads_an_empty_packet_as_its_last_never_sees_one() {
  use ffmpeg_next::{ChannelLayout, codec::Id};
  let whole = encode(Id::WMAV2, 44_100, ChannelLayout::STEREO, 12);
  let (reference, errors) = session_hears(&whole, |_, _| {});
  assert!(errors.is_empty(), "the straight decode: {errors:?}");
  let mut packets = whole.packets.clone();
  let middle = packets.len() / 2;
  packets.insert(
    middle,
    with_new_extradata(ffmpeg_next::Packet::empty(), &[1, 2, 3]),
  );
  let clip = Clip {
    parameters: whole.parameters.clone(),
    packets,
  };
  let (heard, errors) = session_hears(&clip, |_, _| {});
  assert!(
    errors.is_empty(),
    "the packet with no body taken: {errors:?}"
  );
  assert!(
    heard == reference,
    "every frame, as the straight decode of the stream without it"
  );
}

/// LAW (R20 row 1): **a packet with no body after the end of the stream is
/// refused as libavcodec refuses every packet after it** (`AVERROR_EOF`,
/// decode.c:739-740), never deferred; what waits at the end is dropped and
/// said so (`boundary::Deferred::abandon`).
#[test]
fn the_end_refuses_a_packet_with_no_body_and_drops_what_waits() {
  use ffmpeg_next::{ChannelLayout, codec::Id};
  let a = encode(Id::AAC, 44_100, ChannelLayout::MONO, 4);
  let tb = Timebase::new(1, core::num::NonZeroI32::new(48_000).expect("nonzero"));
  let mut dec =
    FfmpegOwnedAudioStreamDecoder::open(a.parameters.clone(), tb, DecoderLimits::default())
      .expect("the audio decoder opens");
  let record = crate::boundary::audio_packet_from_ffmpeg(
    &with_new_extradata(ffmpeg_next::Packet::empty(), &extradata_of(&a.parameters)),
    Timebase::SECONDS,
  )
  .expect("a wrappable payload")
  .expect("a packet");
  let _ = crate::boundary::abandoned::take();
  crate::accepted(dec.send_packet(&record), "the packet with no body");
  assert_eq!(dec.deferred_kinds_for_test(), vec![NEW_EXTRADATA]);
  crate::accepted(dec.send_eof(), "send_eof");
  assert_eq!(
    crate::boundary::abandoned::take(),
    vec![(crate::boundary::Abandoned::End, vec![NEW_EXTRADATA])],
    "the record reported dropped at the end"
  );
  let answer = dec.send_packet(&record);
  assert!(
    matches!(
      answer,
      Err(AudioDecodeError::Decode(Error::Ffmpeg(
        ffmpeg_next::Error::Eof
      )))
    ),
    "refused after the end: {answer:?}"
  );
  assert!(dec.deferred_kinds_for_test().is_empty(), "nothing deferred");
  dec.flush().expect("flush");
  crate::accepted(dec.send_packet(&record), "after a flush");
  dec.flush().expect("flush");
  assert_eq!(
    crate::boundary::abandoned::take(),
    vec![(crate::boundary::Abandoned::Flush, vec![NEW_EXTRADATA])],
    "the record reported dropped at the flush"
  );
}

/// LAW (R21 row 2; Codex R20 [high]): **of AAC records deferred, the later
/// whole one rides; a record of no bytes folds nothing.** FFmpeg's AAC
/// decoder reads a record as the whole of its configuration, the earlier
/// discarded (`aac_decode_frame`, aac/aacdec.c:2580-2589), and for one of no
/// bytes discards its configuration and fails the packet: an object type of
/// 0 is no type it decodes (aac/aacdec.c:1177-1182). Three AAC streams of
/// FFmpeg's own encoder: 44.1 kHz mono; a 32 kHz mono stream's record on a
/// packet with no body, then the 22.05 kHz stereo stream's record on
/// another, then a record of no bytes on a third; then the stereo stream.
/// The session hears every frame as the straight decode of the stereo
/// stream's record on its first packet. Coalesced by type, as R20 coalesced
/// them, the record of no bytes rode that packet, the decoder dropped its
/// configuration, and the stereo stream was not heard.
#[test]
fn of_aac_records_deferred_the_later_whole_one_rides_and_one_of_no_bytes_folds_nothing() {
  use ffmpeg_next::{ChannelLayout, codec::Id};
  let a = encode(Id::AAC, 44_100, ChannelLayout::MONO, 12);
  let c = encode(Id::AAC, 32_000, ChannelLayout::MONO, 4);
  let b = encode(Id::AAC, 22_050, ChannelLayout::STEREO, 12);
  let (reference, errors) = session_hears(&a_then_b(&a, &b, false), |_, _| {});
  assert!(errors.is_empty(), "the straight decode: {errors:?}");
  let mut clip = a_then_b(&a, &b, true);
  let at = a.packets.len();
  clip.packets.insert(
    at,
    with_new_extradata(ffmpeg_next::Packet::empty(), &extradata_of(&c.parameters)),
  );
  clip.packets.insert(
    at + 2,
    with_new_extradata(ffmpeg_next::Packet::empty(), &[]),
  );
  assert!(
    (at..at + 3).all(|index| clip.packets[index].data().is_none()),
    "three packets with no body"
  );
  let (heard, errors) = session_hears(&clip, |_, _| {});
  assert!(
    errors.is_empty() && heard == reference,
    "every frame, as the straight decode of the later record on the next packet: errors \
     {errors:?}, the second stream heard at {:?}",
    heard.last().map(|heard| (heard.0, heard.1))
  );
}

/// `packet` carrying `data` as side data of `kind`.
fn with_side_data(mut packet: ffmpeg_next::Packet, kind: i32, data: &[u8]) -> ffmpeg_next::Packet {
  use ffmpeg_next::packet::Mut;
  // SAFETY: `packet` is a live packet this function owns; FFmpeg allocates
  // the side data, padded, and frees it with the packet; `data` is copied
  // into exactly the bytes it allocated.
  unsafe {
    let slot = crate::ffi::packet_new_side_data(packet.as_mut_ptr(), kind, data.len())
      .expect("side data attached");
    core::ptr::copy_nonoverlapping(data.as_ptr(), slot, data.len());
  }
  packet
}

/// LAW (R21 row 4; Codex R20 [medium]): **a skip of samples a packet with no
/// body carries is dropped and said so, never carried onto the next packet.**
/// `AV_PKT_DATA_SKIP_SAMPLES` is the trim of the samples decoded from the
/// packet it rides: libavcodec copies it onto that packet's frame
/// (`ff_decode_frame_props_from_pkt`, decode.c:1553-1565) and trims that
/// frame by it (`discard_samples`, decode.c:322-420), and no frame comes of
/// a packet with no body (`boundary::carry_of`). An AAC stream of FFmpeg's
/// own encoder with a packet with no body in its middle carrying a skip of
/// 512 samples from its start: the session hears every frame as the
/// straight decode of the stream without it, and the notice names the skip.
/// Carried onto the next packet, as R20 carried every type, the 512 samples
/// were cut from that packet's frame.
#[test]
fn a_skip_of_samples_a_packet_with_no_body_carries_is_dropped_not_carried() {
  use ffmpeg_next::{ChannelLayout, codec::Id};
  const SKIP_SAMPLES: i32 = ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES as i32;
  let whole = encode(Id::AAC, 44_100, ChannelLayout::MONO, 12);
  let (reference, errors) = session_hears(&whole, |_, _| {});
  assert!(errors.is_empty(), "the straight decode: {errors:?}");
  let mut skip = 512u32.to_le_bytes().to_vec();
  skip.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
  let mut packets = whole.packets.clone();
  let middle = packets.len() / 2;
  packets.insert(
    middle,
    with_side_data(ffmpeg_next::Packet::empty(), SKIP_SAMPLES, &skip),
  );
  let clip = Clip {
    parameters: whole.parameters.clone(),
    packets,
  };
  let _ = crate::boundary::abandoned::take();
  let mut deferred = None;
  let (heard, errors) = session_hears(&clip, |index, dec| {
    if index == middle + 1 {
      deferred = Some(dec.deferred_kinds_for_test());
    }
  });
  assert!(errors.is_empty(), "nothing refused: {errors:?}");
  assert!(
    heard == reference,
    "every frame, as the straight decode of the stream without it: the fewest samples a frame \
     held, {}, where the straight decode's held {}",
    heard.iter().map(|frame| frame.2).min().unwrap_or_default(),
    reference
      .iter()
      .map(|frame| frame.2)
      .min()
      .unwrap_or_default(),
  );
  assert_eq!(deferred, Some(Vec::new()), "nothing waits");
  assert_eq!(
    crate::boundary::abandoned::take(),
    vec![(crate::boundary::Abandoned::Alone, vec![SKIP_SAMPLES])],
    "the skip dropped, and said so"
  );
}

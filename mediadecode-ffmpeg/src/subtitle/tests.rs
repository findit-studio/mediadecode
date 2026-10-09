use super::*;

use mediadecode::decoder::SubtitleDecoder;

use crate::{FfmpegBytes, FfmpegOwnedSubtitleStreamDecoder, extras::SideDataEntry};

const NEW_EXTRADATA: i32 = ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA as i32;

/// A text subtitle decoder, opened with no fixture file.
fn subrip() -> FfmpegOwnedSubtitleStreamDecoder {
  ffmpeg_next::init().expect("ffmpeg init");
  let mut parameters = Parameters::new();
  // SAFETY: `parameters` owns a live, zeroed `AVCodecParameters`; both
  // fields are plain scalars and `SUBRIP` is a text subtitle codec that
  // opens with no extradata.
  unsafe {
    let raw = parameters.as_mut_ptr();
    (*raw).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_SUBTITLE;
    (*raw).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_SUBRIP;
  }
  FfmpegOwnedSubtitleStreamDecoder::open(parameters, Timebase::default(), DecoderLimits::default())
    .expect("open a subrip decoder")
}

/// A subtitle packet of `text`, carrying `side_data`.
fn cue(text: &[u8], side_data: Vec<SideDataEntry>) -> crate::OwnedSubtitlePacket {
  mediadecode::packet::SubtitlePacket::new(
    FfmpegBytes::copy_from_slice(text),
    SubtitlePacketExtra::new(0).with_side_data(side_data),
  )
}

/// LAW (R20 row 1): **a subtitle packet with no body is decoded by no
/// decoder; its side data rides the next cue.** `avcodec_decode_subtitle2`
/// hands a packet of no bytes to a decoder with a delay as its flush
/// (decode.c:954). A packet with no body carrying a record, between two
/// cues: taken, no cue, the record deferred; the next cue decodes and takes
/// it; one left at the end is dropped and said so.
#[test]
fn a_subtitle_packet_with_no_body_is_decoded_by_no_decoder() {
  let mut decoder = subrip();
  let mut dst = crate::empty_owned_subtitle_frame();
  let record = || {
    cue(
      b"",
      vec![SideDataEntry::new(
        NEW_EXTRADATA,
        FfmpegBytes::copy_from_slice(&[1]),
      )],
    )
  };
  let _ = crate::boundary::abandoned::take();

  crate::accepted(
    decoder.send_packet(&cue(b"one", Vec::new())),
    "the first cue",
  );
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("receive"),
    Received::Frame
  );
  crate::accepted(decoder.send_packet(&record()), "the packet with no body");
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("receive"),
    Received::NeedsInput,
    "no cue"
  );
  assert_eq!(decoder.deferred_kinds_for_test(), vec![NEW_EXTRADATA]);
  crate::accepted(
    decoder.send_packet(&cue(b"two", Vec::new())),
    "the second cue",
  );
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("receive"),
    Received::Frame
  );
  assert!(
    decoder.deferred_kinds_for_test().is_empty(),
    "taken with the cue"
  );
  crate::accepted(decoder.send_packet(&record()), "the packet with no body");
  crate::accepted(decoder.send_eof(), "send_eof");
  assert_eq!(
    crate::boundary::abandoned::take(),
    vec![(crate::boundary::Abandoned::End, vec![NEW_EXTRADATA])],
    "the record reported dropped at the end"
  );
  assert_eq!(
    decoder.receive_frame(&mut dst).expect("receive"),
    Received::Ended
  );
}

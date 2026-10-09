//! The software decoder's threads, proved on the stream shape that
//! asked for them: H.264 High 4:2:2 at 10 bits, the profile VideoToolbox
//! does not take, so the software road is the only road
//! ([mediagraph#537](https://github.com/findit-studio/mediagraph/issues/537)).
//!
//! `src/video/tests.rs` pins the same two laws on an in-process MPEG-4
//! part 2 clip; this file pins them on the codec and profile the issue
//! measured, through the demuxer-fed packets a real consumer sends.

mod support;

use core::num::NonZeroU32;

use mediadecode::{Received, Sent, Timebase, Timestamp, decoder::VideoStreamDecoder};
use mediadecode_ffmpeg::{
  DecodePath, DecoderLimits, FfmpegOwnedVideoStreamDecoder, PacketLimits, Threads,
  empty_owned_video_frame, owned_video_packet_from_ffmpeg_in,
};

/// One picture: its timestamp and its planes' bytes.
type Picture = (Option<Timestamp>, Vec<Vec<u8>>);

/// Decodes `path`'s video stream on the software road under `threads`
/// and answers every picture in output order, with the thread count the
/// session settled on.
fn decode(path: &std::path::Path, threads: Threads) -> (Vec<Picture>, Option<NonZeroU32>) {
  let mut input = ffmpeg_next::format::input(path).expect("open the fixture");
  let (stream_index, time_base, parameters) = {
    let stream = input
      .streams()
      .best(ffmpeg_next::media::Type::Video)
      .expect("the fixture has a video stream");
    let tb = stream.time_base();
    (
      stream.index(),
      Timebase::new(
        tb.numerator(),
        core::num::NonZeroI32::new(tb.denominator().max(1)).expect("a non-zero denominator"),
      ),
      stream.parameters(),
    )
  };
  let mut decoder = FfmpegOwnedVideoStreamDecoder::open_as(
    parameters,
    time_base,
    DecoderLimits::default().with_threads(threads),
    DecodePath::Software,
  )
  .expect("the software road opens for H.264 High 4:2:2");
  let settled = decoder.active_threads();

  let mut frame = empty_owned_video_frame();
  let mut pictures = Vec::new();
  let mut drain = |decoder: &mut FfmpegOwnedVideoStreamDecoder, pictures: &mut Vec<Picture>| {
    while let Received::Frame = decoder.receive_frame(&mut frame).expect("receive frame") {
      let planes = frame
        .planes()
        .iter()
        .map(|plane| plane.data_ref().as_ref().to_vec())
        .collect();
      pictures.push((frame.pts(), planes));
    }
  };
  while let Some((stream, av_packet)) = input.packets().next() {
    if stream.index() != stream_index {
      continue;
    }
    let Some(packet) =
      owned_video_packet_from_ffmpeg_in(&av_packet, time_base, PacketLimits::default())
        .expect("a wrappable video payload")
    else {
      continue;
    };
    assert_eq!(
      decoder.send_packet(&packet).expect("send packet"),
      Sent::Accepted,
      "the session was drained after every send"
    );
    drain(&mut decoder, &mut pictures);
  }
  assert_eq!(decoder.send_eof().expect("send eof"), Sent::Accepted);
  drain(&mut decoder, &mut pictures);
  (pictures, settled)
}

/// **Auto threads the decode the issue measured on one core.** libavcodec
/// frame-threads H.264, so on a multi-core host the session settles on
/// more than one thread; `Single` stays on one.
#[test]
fn auto_decodes_high_422_h264_on_more_than_one_thread() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.h264_high422_10bit();
  let cores = std::thread::available_parallelism().map_or(1, |n| n.get());

  let (pictures, auto) = decode(&path, Threads::Auto);
  assert!(!pictures.is_empty(), "the fixture decoded no pictures");
  let auto = auto.expect("h264 runs on libavcodec's own threads").get();
  if cores > 1 {
    assert!(
      auto > 1,
      "Auto on a {cores}-core host resolved to {auto} thread(s) for h264"
    );
  }

  let (_, single) = decode(&path, Threads::Single);
  assert_eq!(single, Some(NonZeroU32::MIN));
}

/// **Frame threading does not change a byte.** The same High 4:2:2
/// 10-bit stream, B-frames and all, decodes to the same pictures under
/// `Auto` as under `Single`: the same count, the same timestamps in the
/// same order, byte-identical planes.
#[test]
fn auto_decodes_the_same_planes_as_one_thread() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.h264_high422_10bit();

  let (single, _) = decode(&path, Threads::Single);
  let (auto, _) = decode(&path, Threads::Auto);
  assert!(
    single.len() >= 40,
    "two seconds at 23.976 fps decoded to only {} pictures",
    single.len()
  );
  assert_eq!(auto.len(), single.len(), "picture count");
  for (index, (auto, single)) in auto.iter().zip(&single).enumerate() {
    assert_eq!(auto.0, single.0, "picture {index}'s timestamp");
    assert_eq!(auto.1.len(), 3, "a 4:2:2 picture has three planes");
    assert!(
      auto.1 == single.1,
      "picture {index}'s planes differ between Auto and Single"
    );
  }
}

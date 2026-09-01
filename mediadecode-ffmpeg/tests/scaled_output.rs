//! The `scaled_output` capability word, proved on real decode sessions
//! on this host.
//!
//! [`mediadecode::decoder::ScaledOutputCapability`]'s unit tests (in
//! `mediadecode`'s own `src/decoder.rs`) prove the *trait default*:
//! an implementor that overrides nothing answers `Unsupported` and
//! never errors. This file proves the *real backend's* answer — on an
//! actual hardware-probed session and an actual software-pinned one —
//! is the same honest refusal, and that a refused request changes
//! nothing about the pictures the session goes on to deliver.
//!
//! See [`mediadecode_ffmpeg::CarrierVideoStreamDecoder::
//! scaled_output_capability`]'s doc comment for the census this
//! answers to: every path this crate opens — hardware included —
//! negotiates through FFmpeg's generic hwaccel machinery, which does
//! not expose a caller-owned destination size, and the legacy FFmpeg
//! API that would (`AVVideotoolboxContext`) is not part of this
//! crate's bound FFI surface.

mod support;

use mediadecode::{Received, Sent, Timebase, decoder::ScaledOutputCapability};
use mediadecode_ffmpeg::{
  DecoderLimits, OwnedVideoFrame, PacketLimits, empty_owned_video_frame,
  owned_video_packet_from_ffmpeg_in,
};

/// Opens the auto-probed video decoder on `path`'s first video stream,
/// requests `request` before any packet is sent, and returns
/// `(capability_before, capability_after_request, first_frame)`.
fn probe_scaled_output(
  path: &std::path::Path,
  request: (u32, u32),
) -> (
  ScaledOutputCapability,
  ScaledOutputCapability,
  OwnedVideoFrame,
) {
  use mediadecode::decoder::VideoStreamDecoder;

  let mut input = ffmpeg_next::format::input(path).expect("open the fixture");
  let stream = input
    .streams()
    .best(ffmpeg_next::media::Type::Video)
    .expect("the fixture has a video stream");
  let stream_index = stream.index();
  let tb = stream.time_base();
  let time_base = Timebase::new(
    tb.numerator(),
    core::num::NonZeroI32::new(tb.denominator().max(1)).expect("a non-zero denominator"),
  );

  let mut decoder = mediadecode_ffmpeg::FfmpegOwnedVideoStreamDecoder::open(
    stream.parameters(),
    time_base,
    DecoderLimits::default(),
  )
  .expect("open the video decoder");

  let before = decoder.scaled_output_capability();
  let after = decoder.request_scaled_output(request);

  let mut frame: OwnedVideoFrame = empty_owned_video_frame();
  for (index, (s, av_packet)) in input.packets().enumerate() {
    if s.index() != stream_index {
      continue;
    }
    let Some(packet) =
      owned_video_packet_from_ffmpeg_in(&av_packet, time_base, PacketLimits::default())
        .expect("a wrappable video payload")
    else {
      continue;
    };
    if decoder.send_packet(&packet).expect("send packet") != Sent::Accepted {
      continue;
    }
    if let Received::Frame = decoder.receive_frame(&mut frame).expect("receive frame") {
      return (before, after, frame);
    }
    assert!(index < 64, "no frame within the first 64 packets");
  }
  panic!("the input ended before a frame was produced");
}

/// The hardware-probed path — VideoToolbox on macOS, the one backend
/// `Backend::probe_order` names on this host — refuses a scaled-output
/// request, and the frame that comes back is full coded size, not the
/// requested one.
#[test]
fn hardware_probed_session_refuses_scaled_output_and_delivers_full_size() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  // `multi_track_mkv`'s video track is a plain, uncropped 160x120 —
  // display and coded size agree, so there is only one "full size" to
  // compare the (refused) 32x32 request against.
  let path = corpus.multi_track_mkv();
  let (before, after, frame) = probe_scaled_output(&path, (32, 32));

  assert_eq!(before, ScaledOutputCapability::Unsupported);
  assert_eq!(after, ScaledOutputCapability::Unsupported);
  // 160x120, not the 32x32 requested — proving the refused request
  // changed nothing, rather than merely that *some* frame came back.
  assert_eq!(frame.width(), 160);
  assert_eq!(frame.height(), 120);
}

/// The software road (a codec no hardware backend here accelerates —
/// see `Corpus::software_only_video`'s own doc for the census):
/// refuses the same way, for its own, separately documented reasons
/// (`lowres`'s narrow, broken, detail-skipping coverage) rather than
/// by falling through to the hardware finding above.
#[test]
fn software_only_session_refuses_scaled_output_and_delivers_full_size() {
  let Some(corpus) = support::Corpus::new() else {
    return;
  };
  let path = corpus.software_only_video();
  let (before, after, frame) = probe_scaled_output(&path, (16, 16));

  assert_eq!(before, ScaledOutputCapability::Unsupported);
  assert_eq!(after, ScaledOutputCapability::Unsupported);
  assert_eq!(frame.width(), 1280);
  assert_eq!(frame.height(), 720);
}

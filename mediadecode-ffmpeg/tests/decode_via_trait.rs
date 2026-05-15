//! End-to-end integration test that exercises `FfmpegVideoStreamDecoder`
//! through `mediadecode`'s trait surface using the **safe** public API
//! of this crate. Catches regressions where the trait impl compiles
//! in isolation but the generic dispatch path doesn't actually work.
//!
//! Two pieces:
//! 1. **Compile-time check** (always run): a generic helper bounded on
//!    `VideoStreamDecoder<Adapter = Ffmpeg, Buffer = FfmpegBuffer>`
//!    accepts a `FfmpegVideoStreamDecoder`. If trait associated types
//!    or method signatures drift, this fails to build.
//! 2. **Runtime smoke** (`#[ignore]`-gated unless
//!    `MEDIADECODE_SAMPLE_VIDEO` is set): opens a real video file,
//!    drives `send_packet` / `receive_frame` through the trait, and
//!    asserts the delivered `VideoFrame` carries sane width/height/
//!    pix_fmt.

use ffmpeg::{format, media};
use ffmpeg_next as ffmpeg;
use mediadecode::{Timebase, decoder::VideoStreamDecoder};
use mediadecode_ffmpeg::{
  Ffmpeg, FfmpegBuffer, FfmpegVideoStreamDecoder, VideoFrame, VideoPacket, empty_video_frame,
  video_packet_from_ffmpeg,
};
use std::num::NonZeroU32;

/// Generic helper bounded purely on the `mediadecode` trait — proves
/// `FfmpegVideoStreamDecoder` is reachable through the abstraction.
fn decode_through_trait<D>(
  decoder: &mut D,
  packet: &VideoPacket,
  dst: &mut VideoFrame,
) -> Result<bool, D::Error>
where
  D: VideoStreamDecoder<Adapter = Ffmpeg, Buffer = FfmpegBuffer>,
{
  decoder.send_packet(packet)?;
  match decoder.receive_frame(dst) {
    Ok(()) => Ok(true),
    Err(_e) => Ok(false), // EAGAIN — caller should send more packets
  }
}

#[test]
fn ffmpeg_video_stream_decoder_implements_trait() {
  fn _accepts<D>(_: D)
  where
    D: VideoStreamDecoder<Adapter = Ffmpeg, Buffer = FfmpegBuffer>,
  {
  }

  fn _check_at_compile_time() {
    let opt: Option<FfmpegVideoStreamDecoder> = None;
    if let Some(d) = opt {
      _accepts(d);
    }
  }
}

const SAMPLE_ENV: &str = "MEDIADECODE_SAMPLE_VIDEO";

#[test]
#[ignore = "requires MEDIADECODE_SAMPLE_VIDEO env var pointing at a video file"]
fn decode_one_frame_through_trait() {
  let path = std::env::var_os(SAMPLE_ENV).unwrap_or_else(|| panic!("{SAMPLE_ENV} not set"));

  ffmpeg::init().expect("ffmpeg init");

  let mut input = format::input(&path).expect("open input");
  let stream = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream");
  let stream_index = stream.index();
  let stream_tb = stream.time_base();
  let time_base = Timebase::new(
    stream_tb.numerator() as u32,
    NonZeroU32::new(stream_tb.denominator().max(1) as u32).expect("non-zero den"),
  );

  let mut decoder =
    FfmpegVideoStreamDecoder::open(stream.parameters(), time_base).expect("open decoder");

  eprintln!(
    "decoder opened — initial path: {}",
    if decoder.is_hardware() {
      "hardware"
    } else {
      "software"
    },
  );

  let mut dst = empty_video_frame();
  let mut got_frame = false;

  for (s, av_packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    let pkt = match video_packet_from_ffmpeg(&av_packet) {
      Some(p) => p,
      None => continue,
    };
    match decode_through_trait(&mut decoder, &pkt, &mut dst) {
      Ok(true) => {
        eprintln!(
          "first frame: {}x{} pix_fmt={:?} (path = {})",
          dst.width(),
          dst.height(),
          dst.pixel_format(),
          if decoder.is_hardware() {
            "hardware"
          } else {
            "software"
          },
        );
        assert!(dst.width() > 0);
        assert!(dst.height() > 0);
        assert!(!matches!(
          *dst.pixel_format(),
          mediadecode::PixelFormat::Unknown(_)
        ));
        got_frame = true;
        break;
      }
      Ok(false) => continue,
      Err(e) => panic!("decode error: {e:?}"),
    }
  }

  assert!(got_frame, "no frame delivered through the trait surface");
}

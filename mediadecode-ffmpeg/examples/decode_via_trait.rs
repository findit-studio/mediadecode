//! Decode a video file through `mediadecode`'s trait surface using
//! this crate's **safe** public API.
//!
//! Demonstrates:
//! - **Backend-neutral consumer code** — `decode_one_video` is generic
//!   over `VideoStreamDecoder<Adapter = Ffmpeg, Buffer = FfmpegBuffer>`.
//!   Same shape would work for any future mediadecode adapter.
//! - **Transparent SW fallback** — `FfmpegVideoStreamDecoder::open`
//!   handles HW probe + SW fallback under the hood.
//! - **No `unsafe`** — wrappers like `video_packet_from_ffmpeg` and
//!   `empty_video_frame` mean the caller never reads or constructs
//!   raw FFmpeg buffer pointers.
//!
//! Compare with `examples/decode.rs`, which uses the lower-level
//! `VideoDecoder` (HW-probe wrapper) directly — no SW fallback,
//! more plumbing.
//!
//! ```sh
//! cargo run --release --example decode_via_trait -- /path/to/video.mp4
//! ```

use ffmpeg::{format, media};
use ffmpeg_next as ffmpeg;
use mediadecode::{Timebase, decoder::VideoStreamDecoder};
use mediadecode_ffmpeg::{
  Ffmpeg, FfmpegBuffer, FfmpegVideoStreamDecoder, VideoFrame, empty_video_frame,
  video_packet_from_ffmpeg,
};
use std::num::NonZeroU32;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
  let path = std::env::args()
    .nth(1)
    .ok_or("usage: decode_via_trait <video-file>")?;

  ffmpeg::init()?;

  let mut input = format::input(&path)?;
  let stream = input
    .streams()
    .best(media::Type::Video)
    .ok_or("no video stream")?;
  let stream_index = stream.index();
  let stream_tb = stream.time_base();
  let time_base = Timebase::new(
    stream_tb.numerator() as u32,
    NonZeroU32::new(stream_tb.denominator().max(1) as u32).ok_or("bad time base")?,
  );

  let mut decoder = FfmpegVideoStreamDecoder::open(stream.parameters(), time_base)?;
  println!(
    "decoder opened on the {} path",
    if decoder.is_hardware() {
      "hardware"
    } else {
      "software"
    },
  );

  let count = decode_one_video(&mut decoder, &mut input, stream_index)?;
  println!(
    "decoded {count} frame(s); final path: {}",
    if decoder.is_hardware() {
      "hardware"
    } else {
      "software"
    },
  );
  Ok(())
}

/// Generic helper bounded purely on the `mediadecode` trait. Any
/// decoder satisfying `VideoStreamDecoder<Adapter = Ffmpeg, Buffer =
/// FfmpegBuffer>` works here — `FfmpegVideoStreamDecoder` is just one
/// instance.
fn decode_one_video<D>(
  decoder: &mut D,
  input: &mut format::context::Input,
  stream_index: usize,
) -> std::result::Result<u64, Box<dyn std::error::Error>>
where
  D: VideoStreamDecoder<Adapter = Ffmpeg, Buffer = FfmpegBuffer>,
  D::Error: std::error::Error + Send + Sync + 'static,
{
  let mut dst = empty_video_frame();
  let mut count: u64 = 0;

  let drain = |decoder: &mut D, dst: &mut VideoFrame, count: &mut u64| {
    while decoder.receive_frame(dst).is_ok() {
      *count += 1;
      println!(
        "frame#{count} pts={:?} {}x{} pix_fmt={}",
        dst.pts().map(|t| t.pts()),
        dst.width(),
        dst.height(),
        dst.pixel_format(),
      );
    }
  };

  for (s, av_packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    let pkt = match video_packet_from_ffmpeg(&av_packet) {
      Some(p) => p,
      None => continue,
    };
    if let Err(e) = decoder.send_packet(&pkt) {
      // EAGAIN: drain and retry.
      drain(decoder, &mut dst, &mut count);
      decoder.send_packet(&pkt).map_err(|_| e)?;
    }
    drain(decoder, &mut dst, &mut count);
  }
  let _ = decoder.send_eof();
  drain(decoder, &mut dst, &mut count);
  Ok(count)
}

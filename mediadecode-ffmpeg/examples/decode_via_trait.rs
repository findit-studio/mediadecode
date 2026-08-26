//! Decode a video file through `mediadecode`'s trait surface using
//! this crate's **safe** public API.
//!
//! Demonstrates:
//! - **Backend-neutral consumer code** — `decode_one_video` is generic
//!   over `VideoStreamDecoder<Adapter = Ffmpeg, Buffer = FfmpegBuffer>`.
//!   Same shape would work for any future mediadecode adapter.
//! - **The ordinary lane** — the bare names are the view lane, so each
//!   frame here is a window onto the decoder's own buffer, read and
//!   dropped inside the loop. Swap `FfmpegBuffer` for `FfmpegBytes` and
//!   `empty_video_frame` for `empty_owned_video_frame` and the same
//!   loop produces frames that can be queued and sent elsewhere.
//! - **Transparent SW fallback** — `FfmpegVideoStreamDecoder::open`
//!   handles HW probe + SW fallback under the hood.
//! - **No `unsafe`** — wrappers like `video_packet_from_ffmpeg` and
//!   `empty_video_frame` mean the caller never reads or constructs
//!   raw FFmpeg buffer pointers.
//! - **No errno either** — `receive_frame` answers
//!   `mediadecode::Received`, so this generic loop distinguishes
//!   *needs input* from *ended* from *broken* without naming an FFmpeg
//!   type. Its non-generic sibling used to be the only one that could,
//!   and only by matching `Error::Ffmpeg(Other { errno })` in user
//!   code.
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
use mediadecode::{Received, Sent, Timebase, decoder::VideoStreamDecoder};
use mediadecode_ffmpeg::{
  DecoderLimits, Ffmpeg, FfmpegBuffer, FfmpegVideoStreamDecoder, PacketLimits, VideoFrame,
  empty_video_frame, video_packet_from_ffmpeg_in,
};
use std::num::NonZeroI32;

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
    stream_tb.numerator(),
    NonZeroI32::new(stream_tb.denominator().max(1)).ok_or("bad time base")?,
  );

  let mut decoder =
    FfmpegVideoStreamDecoder::open(stream.parameters(), time_base, DecoderLimits::default())?;
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

  /// Delivers every frame that is ready, and answers whether the
  /// stream is over. A receive-side failure surfaces instead of being
  /// swallowed — which the `while ….is_ok()` idiom this replaced could
  /// not do, because it could not tell a drained decoder from a broken
  /// one.
  fn drain<D>(
    decoder: &mut D,
    dst: &mut VideoFrame,
    count: &mut u64,
  ) -> std::result::Result<bool, D::Error>
  where
    D: VideoStreamDecoder<Adapter = Ffmpeg, Buffer = FfmpegBuffer>,
  {
    loop {
      match decoder.receive_frame(dst)? {
        Received::Frame => {
          *count += 1;
          println!(
            "frame#{count} pts={:?} {}x{} pix_fmt={}",
            dst.pts().map(|t| t.pts()),
            dst.width(),
            dst.height(),
            dst.pixel_format(),
          );
        }
        Received::NeedsInput => return Ok(false),
        Received::Ended => return Ok(true),
      }
    }
  }

  for (s, av_packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    // **By value**: the packet iterator hands the packet over, and so
    // do we. A view carrier is a window into its buffer, so a source
    // that survived the call would be a mutable alias of it — which is
    // why the borrowing door is the owned lane. See
    // `boundary::video_packet_from_ffmpeg_in`.
    let pkt =
      match video_packet_from_ffmpeg_in(av_packet, Timebase::default(), PacketLimits::default())? {
        Some(p) => p,
        None => continue,
      };
    // **The two-offer rule, retired.** It used to read: submit, and if
    // that fails drain and submit again, because "drain me first" and
    // "this packet is damaged" arrived as the same `Err` and only a
    // second attempt could tell them apart. The decoder says which it
    // is now, so this offers until it is taken — however many drains
    // that needs — and `?` gives up only on a real fault.
    loop {
      match decoder.send_packet(&pkt)? {
        Sent::Accepted => break,
        Sent::MustDrain => {
          if drain(decoder, &mut dst, &mut count)? {
            return Ok(count);
          }
        }
      }
    }
    if drain(decoder, &mut dst, &mut count)? {
      return Ok(count);
    }
  }
  // The end-of-stream is offered on the same terms.
  while decoder.send_eof()? == Sent::MustDrain {
    drain(decoder, &mut dst, &mut count)?;
  }
  // The tail, and its end is a state rather than a guess.
  drain(decoder, &mut dst, &mut count)?;
  Ok(count)
}

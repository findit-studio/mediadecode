//! Decode every video frame in `argv[1]`, printing one line per frame.
//!
//! ```sh
//! cargo run --release --example decode -- /path/to/video.mp4
//! ```

use ffmpeg::{format, media};
use ffmpeg_next as ffmpeg;
use mediadecode_ffmpeg::{Frame, VideoDecoder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let path = std::env::args()
    .nth(1)
    .ok_or("usage: decode <video-file>")?;

  ffmpeg::init()?;

  let mut input = format::input(&path)?;
  let stream = input
    .streams()
    .best(media::Type::Video)
    .ok_or("no video stream")?;
  let stream_index = stream.index();

  let mut decoder = match VideoDecoder::open(stream.parameters()) {
    Ok(d) => d,
    Err(mediadecode_ffmpeg::Error::AllBackendsFailed { attempts, .. }) => {
      eprintln!(
        "no hardware backend available; tried {} backend(s):",
        attempts.len()
      );
      for (b, e) in &attempts {
        eprintln!("  {b:?}: {e}");
      }
      eprintln!("(callers handle software fallback themselves — see ffmpeg::decoder::Video)");
      return Ok(());
    }
    Err(e) => return Err(e.into()),
  };
  println!(
    "open: backend={:?} {}x{}",
    decoder.backend(),
    decoder.width(),
    decoder.height(),
  );

  let mut frame = Frame::empty()?;
  let mut count: u64 = 0;

  let drain = |decoder: &mut VideoDecoder, frame: &mut Frame, count: &mut u64| loop {
    match decoder.receive_frame(frame) {
      Ok(()) => {
        *count += 1;
        println!(
          "frame#{count} pts={:?} {}x{} pix_fmt={:?}",
          frame.pts(),
          frame.width(),
          frame.height(),
          frame.pix_fmt(),
        );
      }
      Err(mediadecode_ffmpeg::Error::Ffmpeg(ffmpeg::Error::Other { errno }))
        if errno == ffmpeg::error::EAGAIN =>
      {
        break;
      }
      Err(mediadecode_ffmpeg::Error::Ffmpeg(ffmpeg::Error::Eof)) => break,
      Err(e) => {
        eprintln!("decode error: {e}");
        break;
      }
    }
  };

  for (s, packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    loop {
      match decoder.send_packet(&packet) {
        Ok(()) => break,
        Err(mediadecode_ffmpeg::Error::Ffmpeg(ffmpeg::Error::Other { errno }))
          if errno == ffmpeg::error::EAGAIN =>
        {
          drain(&mut decoder, &mut frame, &mut count);
        }
        Err(e) => return Err(e.into()),
      }
    }
    drain(&mut decoder, &mut frame, &mut count);
  }
  loop {
    match decoder.send_eof() {
      Ok(()) => break,
      Err(mediadecode_ffmpeg::Error::Ffmpeg(ffmpeg::Error::Other { errno }))
        if errno == ffmpeg::error::EAGAIN =>
      {
        drain(&mut decoder, &mut frame, &mut count);
      }
      Err(e) => return Err(e.into()),
    }
  }
  drain(&mut decoder, &mut frame, &mut count);

  println!(
    "decoded {count} frames; final backend={:?}",
    decoder.backend()
  );
  Ok(())
}

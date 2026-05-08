//! `#[ignore]`-gated smoke test that exercises end-to-end hardware decode
//! against a real video file: opens the auto-probed decoder, drives it
//! until the first frame is delivered, and asserts the active backend is
//! one of the supported HW variants. Run with:
//!
//! ```sh
//! HWDECODE_SAMPLE_VIDEO=/path/to/clip.mp4 cargo test --test hw_smoke -- --ignored
//! ```

use ffmpeg::{format, media};
use ffmpeg_next as ffmpeg;
use mediadecode_ffmpeg::{Backend, Frame, VideoDecoder};

const SAMPLE_ENV: &str = "HWDECODE_SAMPLE_VIDEO";

#[test]
#[ignore = "requires HWDECODE_SAMPLE_VIDEO and a working hardware backend"]
fn auto_probe_picks_hardware_backend() {
  let path = std::env::var_os(SAMPLE_ENV).unwrap_or_else(|| panic!("{SAMPLE_ENV} not set"));

  ffmpeg::init().expect("ffmpeg init");

  let mut input = format::input(&path).expect("open input");
  let stream = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream");
  let stream_index = stream.index();

  let mut decoder = VideoDecoder::open(stream.parameters()).expect("open decoder");
  eprintln!("auto-probe optimistic backend = {:?}", decoder.backend());

  // Decode at least one frame so the probe collapses, then check the
  // backend that actually produced it. Checking `decoder.backend()` before
  // any frame has been received would observe the optimistic pre-probe
  // value and could false-pass when a HW backend silently degrades.
  //
  // FFmpeg's send/receive contract: `send_packet` may return EAGAIN if
  // the decoder's internal queue is full; the caller must drain via
  // `receive_frame` before retrying. We handle EAGAIN on both sides so a
  // codec with a deeper buffer (or a candidate that's already produced
  // output during probe replay) doesn't crash this smoke test.
  let mut frame = Frame::empty().expect("alloc frame");
  let mut got_frame = false;
  let log_first = |frame: &Frame, decoder: &VideoDecoder| {
    eprintln!(
      "first frame: backend={:?} {}x{} pix_fmt={:?}",
      decoder.backend(),
      frame.width(),
      frame.height(),
      frame.pix_fmt()
    );
  };
  'outer: for (s, packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    // Loop until `send_packet` accepts. On EAGAIN, drain one frame
    // (which either completes the smoke test or frees queue space),
    // then retry send_packet.
    loop {
      match decoder.send_packet(&packet) {
        Ok(()) => break,
        Err(mediadecode_ffmpeg::Error::Ffmpeg(ffmpeg::Error::Other { errno }))
          if errno == ffmpeg::error::EAGAIN =>
        {
          match decoder.receive_frame(&mut frame) {
            Ok(()) => {
              got_frame = true;
              log_first(&frame, &decoder);
              break 'outer;
            }
            Err(mediadecode_ffmpeg::Error::Ffmpeg(ffmpeg::Error::Other { errno }))
              if errno == ffmpeg::error::EAGAIN =>
            {
              // Defensive: per FFmpeg's send/receive contract, if
              // send_packet returns EAGAIN then receive_frame should
              // not. Retry send_packet anyway rather than spinning.
            }
            Err(e) => panic!("receive_frame (drain): {e}"),
          }
        }
        Err(e) => panic!("send_packet: {e}"),
      }
    }
    match decoder.receive_frame(&mut frame) {
      Ok(()) => {
        got_frame = true;
        log_first(&frame, &decoder);
        break;
      }
      Err(mediadecode_ffmpeg::Error::Ffmpeg(ffmpeg::Error::Other { errno }))
        if errno == ffmpeg::error::EAGAIN =>
      {
        continue;
      }
      Err(e) => panic!("receive_frame: {e}"),
    }
  }
  assert!(got_frame, "no frames decoded");
  // After the probe collapses, `backend()` reports the backend that
  // actually produced the first frame. Make the doc-comment claim
  // explicit: it must be one of the HW variants. Today the enum is
  // exhaustively HW-only, so `matches!` here is tautological — but it
  // documents intent and would catch a future regression that
  // reintroduces a non-HW variant or leaves the active state
  // mis-classified.
  let backend = decoder.backend();
  assert!(
    matches!(
      backend,
      Backend::VideoToolbox | Backend::Vaapi | Backend::Cuda | Backend::D3d11va
    ),
    "expected HW backend, got {backend:?}"
  );
}

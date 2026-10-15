//! With `ffmpeg`, the umbrella links libavcodec and the encoder gate
//! answers through it — and still keeps GPL encoders closed, because the
//! umbrella does not turn on `mediaencode-ffmpeg`'s `gpl` feature.

#![cfg(feature = "ffmpeg")]

use mediaio::ffmpeg::encode::{EncoderRefusal, encoder_by_name};

#[test]
fn the_linked_libavcodec_answers_through_the_umbrella() {
  let admitted = encoder_by_name("aac").expect("every libavcodec carries aac");
  assert_eq!(admitted.name(), "aac");
}

#[test]
fn a_gpl_encoder_stays_refused_by_name_through_the_umbrella() {
  assert_eq!(
    encoder_by_name("libx265"),
    Err(EncoderRefusal::Gpl { name: "libx265" })
  );
}

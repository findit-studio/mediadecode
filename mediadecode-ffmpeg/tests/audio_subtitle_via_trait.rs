//! Compile-time + runtime checks that `FfmpegAudioStreamDecoder` and
//! `FfmpegSubtitleStreamDecoder` reach through `mediadecode`'s trait
//! surface using this crate's **safe** public API. Same shape as
//! `tests/decode_via_trait.rs`, for audio + subtitle.

use ffmpeg_next as ffmpeg;
use mediadecode::{
  Timebase,
  decoder::{AudioStreamDecoder, SubtitleDecoder},
  subtitle::SubtitlePayload,
};
// The owned family under the names this suite was written with — the
// bare aliases mean the view lane now. Import block only; the
// assertions below are unchanged.
use mediadecode_ffmpeg::{
  DecoderLimits, Ffmpeg, FfmpegBytes, FfmpegOwnedAudioStreamDecoder as FfmpegAudioStreamDecoder,
  FfmpegOwnedSubtitleStreamDecoder as FfmpegSubtitleStreamDecoder, OwnedAudioFrame as AudioFrame,
  empty_owned_audio_frame as empty_audio_frame, empty_owned_subtitle_frame as empty_subtitle_frame,
};
use std::num::NonZeroI32;

#[test]
fn ffmpeg_audio_decoder_implements_trait() {
  fn _accepts_audio<D>(_: D)
  where
    D: AudioStreamDecoder<Adapter = Ffmpeg, Buffer = FfmpegBytes>,
  {
  }
  fn _check_audio_at_compile_time() {
    let opt: Option<FfmpegAudioStreamDecoder> = None;
    if let Some(d) = opt {
      _accepts_audio(d);
    }
  }
}

#[test]
fn ffmpeg_subtitle_decoder_implements_trait() {
  fn _accepts_subtitle<D>(_: D)
  where
    D: SubtitleDecoder<Adapter = Ffmpeg, Buffer = FfmpegBytes>,
  {
  }
  fn _check_subtitle_at_compile_time() {
    let opt: Option<FfmpegSubtitleStreamDecoder> = None;
    if let Some(d) = opt {
      _accepts_subtitle(d);
    }
  }
}

const AUDIO_SAMPLE_ENV: &str = "MEDIADECODE_SAMPLE_AUDIO";
const SUBTITLE_SAMPLE_ENV: &str = "MEDIADECODE_SAMPLE_SUBTITLE";

#[test]
#[ignore = "requires MEDIADECODE_SAMPLE_AUDIO env var pointing at an audio file (or container with an audio track)"]
fn decode_one_audio_frame_through_trait() {
  let path =
    std::env::var_os(AUDIO_SAMPLE_ENV).unwrap_or_else(|| panic!("{AUDIO_SAMPLE_ENV} not set"));

  ffmpeg::init().expect("ffmpeg init");

  let mut input = ffmpeg::format::input(&path).expect("open input");
  let stream = input
    .streams()
    .best(ffmpeg::media::Type::Audio)
    .expect("audio stream");
  let stream_index = stream.index();
  let stream_tb = stream.time_base();
  let time_base = Timebase::new(
    stream_tb.numerator(),
    NonZeroI32::new(stream_tb.denominator().max(1)).expect("non-zero den"),
  );

  let mut decoder =
    FfmpegAudioStreamDecoder::open(stream.parameters(), time_base, DecoderLimits::default())
      .expect("open audio decoder");

  let mut dst: AudioFrame = empty_audio_frame();
  let mut got_frame = false;

  for (s, av_packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    let pkt = match mediadecode_ffmpeg::boundary::owned_audio_packet_from_ffmpeg_in(
      &av_packet,
      mediadecode::Timebase::default(),
      mediadecode_ffmpeg::PacketLimits::default(),
    )
    .expect("a wrappable payload")
    {
      Some(p) => p,
      None => continue,
    };
    decoder.send_packet(&pkt).expect("audio send_packet");
    match decoder.receive_frame(&mut dst) {
      Ok(()) => {
        eprintln!(
          "audio frame: rate={}Hz samples={} channels={} format={:?}",
          dst.sample_rate(),
          dst.nb_samples(),
          dst.channel_count(),
          dst.sample_format(),
        );
        assert!(dst.sample_rate() > 0);
        assert!(dst.nb_samples() > 0);
        assert!(dst.channel_count() > 0);
        got_frame = true;
        break;
      }
      Err(_) => continue,
    }
  }

  assert!(
    got_frame,
    "no audio frame delivered through the trait surface"
  );
}

#[test]
#[ignore = "requires MEDIADECODE_SAMPLE_SUBTITLE env var pointing at a container with a subtitle track"]
fn decode_one_subtitle_through_trait() {
  let path = std::env::var_os(SUBTITLE_SAMPLE_ENV)
    .unwrap_or_else(|| panic!("{SUBTITLE_SAMPLE_ENV} not set"));

  ffmpeg::init().expect("ffmpeg init");

  let mut input = ffmpeg::format::input(&path).expect("open input");
  let stream = input
    .streams()
    .best(ffmpeg::media::Type::Subtitle)
    .expect("subtitle stream");
  let stream_index = stream.index();
  let stream_tb = stream.time_base();
  let time_base = Timebase::new(
    stream_tb.numerator(),
    NonZeroI32::new(stream_tb.denominator().max(1)).expect("non-zero den"),
  );

  let mut decoder =
    FfmpegSubtitleStreamDecoder::open(stream.parameters(), time_base, DecoderLimits::default())
      .expect("open subtitle decoder");

  let mut dst = empty_subtitle_frame();
  let mut got_frame = false;

  for (s, av_packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    let pkt = match mediadecode_ffmpeg::boundary::owned_subtitle_packet_from_ffmpeg_in(
      &av_packet,
      mediadecode::Timebase::default(),
      mediadecode_ffmpeg::PacketLimits::default(),
    )
    .expect("a wrappable payload")
    {
      Some(p) => p,
      None => continue,
    };
    decoder.send_packet(&pkt).expect("subtitle send_packet");
    match decoder.receive_frame(&mut dst) {
      Ok(()) => {
        match dst.payload() {
          SubtitlePayload::Text(p) => {
            let bytes = p.text().as_ref().to_vec();
            let s = std::string::String::from_utf8_lossy(&bytes);
            eprintln!("subtitle text: {s:?}");
            assert!(!s.is_empty(), "decoded subtitle text was empty");
          }
          SubtitlePayload::Bitmap(p) => {
            eprintln!("subtitle bitmap regions: {}", p.regions().len());
            assert!(!p.regions().is_empty());
          }
        }
        got_frame = true;
        break;
      }
      Err(_) => continue,
    }
  }

  assert!(got_frame, "no subtitle delivered through the trait surface");
}

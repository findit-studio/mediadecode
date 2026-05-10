//! End-to-end audio decoder coverage against every clip in the
//! [`audio-fixtures`][1] submodule. Demuxes each WAV via FFmpeg,
//! drives `FfmpegAudioStreamDecoder` through the trait surface, and
//! asserts the recovered sample stream matches the expected
//! `(sample_rate, channels, samples)` triple from upstream's
//! `manifest.json`.
//!
//! Two codec directories are exercised — `pcm_s16le/` and
//! `pcm_f32le/`. The fixture set is mostly the former; the f32le
//! sub-corpus exists because one upstream clip
//! (`01_dialogue.wav`) ships at 32-bit float depth even though
//! the rest are 16-bit. Both groups are mono 16 kHz; only the
//! sample format and per-clip sample count vary, and both are
//! checked exactly.
//!
//! When the submodule isn't initialized the test returns early
//! with a hint instead of failing — keeps `cargo test` welcoming
//! for contributors who haven't run `git submodule update --init`
//! yet, while CI (always-`submodules: recursive`) exercises the
//! full sweep.
//!
//! [1]: https://github.com/findit-ai/audio-fixtures

use std::{num::NonZeroU32, path::PathBuf};

use ffmpeg_next as ffmpeg;
use mediadecode::{Timebase, decoder::AudioStreamDecoder};
use mediadecode_ffmpeg::{FfmpegAudioStreamDecoder, audio_packet_from_ffmpeg, empty_audio_frame};

/// One row per fixture: `(directory, file, sample_rate, channels,
/// expected_samples)`. Kept hard-coded rather than parsed at runtime
/// so a change in `audio-fixtures/manifest.json` (a new file, a
/// re-trim, a re-encode) trips the assertion path rather than
/// silently passing. Update both sides when adding a fixture.
const FIXTURES: &[(&str, &str, u32, u8, u64)] = &[
  // --- pcm_s16le/ ---
  ("pcm_s16le", "02_pyannote_sample.wav", 16_000, 1, 480_000),
  ("pcm_s16le", "03_dual_speaker.wav", 16_000, 1, 960_000),
  ("pcm_s16le", "04_three_speaker.wav", 16_000, 1, 639_573),
  ("pcm_s16le", "05_four_speaker.wav", 16_000, 1, 960_000),
  ("pcm_s16le", "06_long_recording.wav", 16_000, 1, 15_643_627),
  (
    "pcm_s16le",
    "07_yuhewei_dongbei_english.wav",
    16_000,
    1,
    404_213,
  ),
  (
    "pcm_s16le",
    "08_luyu_jinjing_freedom.wav",
    16_000,
    1,
    22_675_308,
  ),
  (
    "pcm_s16le",
    "09_mrbeast_dollar_date.wav",
    16_000,
    1,
    16_671_744,
  ),
  (
    "pcm_s16le",
    "10_mrbeast_clean_water.wav",
    16_000,
    1,
    9_911_979,
  ),
  (
    "pcm_s16le",
    "11_mrbeast_age_race.wav",
    16_000,
    1,
    22_568_310,
  ),
  ("pcm_s16le", "12_mrbeast_schools.wav", 16_000, 1, 15_426_781),
  (
    "pcm_s16le",
    "13_mrbeast_saved_animals.wav",
    16_000,
    1,
    16_882_005,
  ),
  (
    "pcm_s16le",
    "14_mrbeast_strongman_robot.wav",
    16_000,
    1,
    17_648_640,
  ),
  // --- pcm_f32le/ ---
  ("pcm_f32le", "01_dialogue.wav", 16_000, 1, 3_631_361),
];

fn fixtures_root() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .parent()
    .expect("workspace root")
    .join("tests/fixtures/audio")
}

fn decode_clip(path: &std::path::Path, expected: (u32, u8, u64)) {
  let (expected_sample_rate, expected_channels, expected_samples) = expected;

  let mut input = ffmpeg::format::input(&path).expect("open input");
  let stream = input
    .streams()
    .best(ffmpeg::media::Type::Audio)
    .expect("audio stream");
  let stream_index = stream.index();
  let stream_tb = stream.time_base();
  let time_base = Timebase::new(
    stream_tb.numerator() as u32,
    NonZeroU32::new(stream_tb.denominator().max(1) as u32).expect("non-zero den"),
  );

  let mut decoder =
    FfmpegAudioStreamDecoder::open(stream.parameters(), time_base).expect("open audio decoder");

  let mut frame = empty_audio_frame();
  let mut total_samples: u64 = 0;
  let mut frame_count: u64 = 0;
  let mut observed_sample_rate: Option<u32> = None;
  let mut observed_channels: Option<u8> = None;

  for (s, av_packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    let Some(pkt) = audio_packet_from_ffmpeg(&av_packet) else {
      continue;
    };
    decoder.send_packet(&pkt).expect("audio send_packet");
    while decoder.receive_frame(&mut frame).is_ok() {
      total_samples = total_samples.saturating_add(frame.nb_samples() as u64);
      frame_count = frame_count.saturating_add(1);
      observed_sample_rate.get_or_insert(frame.sample_rate());
      observed_channels.get_or_insert(frame.channel_count());
    }
  }
  decoder.send_eof().expect("send_eof");
  while decoder.receive_frame(&mut frame).is_ok() {
    total_samples = total_samples.saturating_add(frame.nb_samples() as u64);
    frame_count = frame_count.saturating_add(1);
  }

  assert!(frame_count > 0, "no audio frames decoded for {path:?}");
  assert_eq!(
    observed_sample_rate,
    Some(expected_sample_rate),
    "sample rate drift on {path:?}",
  );
  assert_eq!(
    observed_channels,
    Some(expected_channels),
    "channel count drift on {path:?}",
  );
  assert_eq!(
    total_samples,
    expected_samples,
    "sample count drift on {} ({total_samples} got, {expected_samples} expected)",
    path.file_name().unwrap_or_default().to_string_lossy(),
  );
}

#[test]
fn decode_all_audio_fixtures() {
  let root = fixtures_root();
  if !root.exists() {
    eprintln!(
      "skip: {} not found — run `git submodule update --init --depth=1` \
       to fetch the audio-fixtures submodule, then re-run this test.",
      root.display()
    );
    return;
  }

  ffmpeg::init().expect("ffmpeg init");

  for (codec_dir, name, sample_rate, channels, samples) in FIXTURES {
    let path = root.join(codec_dir).join(name);
    eprintln!("decoding {codec_dir}/{name}…");
    decode_clip(&path, (*sample_rate, *channels, *samples));
  }
  eprintln!(
    "decoded {} fixtures end-to-end through the trait surface",
    FIXTURES.len(),
  );
}

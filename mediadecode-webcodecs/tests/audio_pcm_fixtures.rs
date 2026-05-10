//! End-to-end audio decoder coverage against every clip in the
//! [`audio-fixtures`][1] submodule, run in headless Chromium via
//! `wasm-bindgen-test`. Mirror of the FFmpeg-side
//! `tests/audio_pcm_fixtures.rs`.
//!
//! Two codec directories — `pcm_s16le/` and `pcm_f32le/` — are
//! exercised with the matching WebCodecs codec strings (`"pcm-s16"`
//! and `"pcm-f32"`). Format invariants (16 kHz, mono) are shared;
//! per-clip sample counts come from the fixture manifest and are
//! checked exactly on the decoded side.
//!
//! Why `include_bytes!` rather than `fetch()`:
//! - the wasm test harness has no host filesystem;
//! - `wasm-bindgen-test-runner`'s built-in HTTP server only serves
//!   the wasm bundle + JS shim, not arbitrary repo files, so a
//!   relative URL would 404 without extra workflow plumbing;
//! - `include_bytes!` resolves at compile time, which means the
//!   submodule must be present **at build time** — exactly what
//!   CI's `submodules: recursive` checkout already guarantees.
//!
//! Embedding all 14 fixtures bakes ~283 MiB into the test wasm
//! binary; rustc handles that fine (the data segment is just
//! verbatim bytes), the wasm itself loads in a few seconds under
//! headless Chrome, and decode is sample-shuffling so each clip
//! finishes in tens of milliseconds.
//!
//! [1]: https://github.com/findit-ai/audio-fixtures
#![cfg(target_arch = "wasm32")]

use mediadecode::{
  Timebase,
  future::local::AudioStreamDecoder,
  packet::{AudioPacket, PacketFlags},
};
use mediadecode_webcodecs::{
  AudioDecodeError, AudioPacketExtra, WebCodecsAudioStreamDecoder, WebCodecsBuffer,
  empty_audio_frame,
};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

/// One row per fixture. `bytes` is a `&'static [u8]` baked in by
/// `include_bytes!`. `codec_string` is the WebCodecs codec
/// identifier (registry-defined: `"pcm-s16"`, `"pcm-f32"`, …).
struct Fixture {
  name: &'static str,
  bytes: &'static [u8],
  codec_string: &'static str,
  expected_samples: u64,
}

const FIXTURES: &[Fixture] = &[
  // --- pcm_s16le/ → "pcm-s16" ---
  Fixture {
    name: "pcm_s16le/02_pyannote_sample.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/02_pyannote_sample.wav"),
    codec_string: "pcm-s16",
    expected_samples: 480_000,
  },
  Fixture {
    name: "pcm_s16le/03_dual_speaker.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/03_dual_speaker.wav"),
    codec_string: "pcm-s16",
    expected_samples: 960_000,
  },
  Fixture {
    name: "pcm_s16le/04_three_speaker.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/04_three_speaker.wav"),
    codec_string: "pcm-s16",
    expected_samples: 639_573,
  },
  Fixture {
    name: "pcm_s16le/05_four_speaker.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/05_four_speaker.wav"),
    codec_string: "pcm-s16",
    expected_samples: 960_000,
  },
  Fixture {
    name: "pcm_s16le/06_long_recording.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/06_long_recording.wav"),
    codec_string: "pcm-s16",
    expected_samples: 15_643_627,
  },
  Fixture {
    name: "pcm_s16le/07_yuhewei_dongbei_english.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/07_yuhewei_dongbei_english.wav"),
    codec_string: "pcm-s16",
    expected_samples: 404_213,
  },
  Fixture {
    name: "pcm_s16le/08_luyu_jinjing_freedom.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/08_luyu_jinjing_freedom.wav"),
    codec_string: "pcm-s16",
    expected_samples: 22_675_308,
  },
  Fixture {
    name: "pcm_s16le/09_mrbeast_dollar_date.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/09_mrbeast_dollar_date.wav"),
    codec_string: "pcm-s16",
    expected_samples: 16_671_744,
  },
  Fixture {
    name: "pcm_s16le/10_mrbeast_clean_water.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/10_mrbeast_clean_water.wav"),
    codec_string: "pcm-s16",
    expected_samples: 9_911_979,
  },
  Fixture {
    name: "pcm_s16le/11_mrbeast_age_race.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/11_mrbeast_age_race.wav"),
    codec_string: "pcm-s16",
    expected_samples: 22_568_310,
  },
  Fixture {
    name: "pcm_s16le/12_mrbeast_schools.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/12_mrbeast_schools.wav"),
    codec_string: "pcm-s16",
    expected_samples: 15_426_781,
  },
  Fixture {
    name: "pcm_s16le/13_mrbeast_saved_animals.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/13_mrbeast_saved_animals.wav"),
    codec_string: "pcm-s16",
    expected_samples: 16_882_005,
  },
  Fixture {
    name: "pcm_s16le/14_mrbeast_strongman_robot.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_s16le/14_mrbeast_strongman_robot.wav"),
    codec_string: "pcm-s16",
    expected_samples: 17_648_640,
  },
  // --- pcm_f32le/ → "pcm-f32" ---
  Fixture {
    name: "pcm_f32le/01_dialogue.wav",
    bytes: include_bytes!("../../tests/fixtures/audio/pcm_f32le/01_dialogue.wav"),
    codec_string: "pcm-f32",
    expected_samples: 3_631_361,
  },
];

const EXPECTED_SAMPLE_RATE: u32 = 16_000;
const EXPECTED_CHANNELS: u8 = 1;

/// Send 1024 frames per chunk. Big enough to keep the loop short on
/// long clips, small enough that even the longest fixture
/// (~22 M samples) produces ~22 k chunks — well within any reasonable
/// bookkeeping budget.
const SAMPLES_PER_CHUNK: usize = 1024;

/// Minimal RIFF/WAVE parser — locates `fmt ` and `data` chunks and
/// returns `(sample_rate, channels, bits_per_sample, pcm_bytes)`.
fn parse_wav(data: &[u8]) -> (u32, u16, u16, &[u8]) {
  assert!(data.len() > 44, "WAV smaller than minimum header");
  assert_eq!(&data[0..4], b"RIFF", "missing RIFF magic");
  assert_eq!(&data[8..12], b"WAVE", "missing WAVE magic");

  let mut pos = 12;
  let mut sample_rate: u32 = 0;
  let mut channels: u16 = 0;
  let mut bits_per_sample: u16 = 0;
  while pos + 8 <= data.len() {
    let id = &data[pos..pos + 4];
    let size =
      u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]) as usize;
    let body_start = pos + 8;
    if id == b"fmt " {
      assert!(size >= 16, "short fmt chunk");
      channels = u16::from_le_bytes([data[body_start + 2], data[body_start + 3]]);
      sample_rate = u32::from_le_bytes([
        data[body_start + 4],
        data[body_start + 5],
        data[body_start + 6],
        data[body_start + 7],
      ]);
      bits_per_sample = u16::from_le_bytes([data[body_start + 14], data[body_start + 15]]);
    } else if id == b"data" {
      let end = body_start + size.min(data.len() - body_start);
      return (
        sample_rate,
        channels,
        bits_per_sample,
        &data[body_start..end],
      );
    }
    pos = body_start + size + (size & 1);
  }
  panic!("data chunk not found");
}

async fn decode_fixture(fix: &Fixture) {
  let (sample_rate, channels, bits_per_sample, pcm) = parse_wav(fix.bytes);
  assert_eq!(
    sample_rate, EXPECTED_SAMPLE_RATE,
    "{}: sample rate",
    fix.name
  );
  assert_eq!(
    channels, EXPECTED_CHANNELS as u16,
    "{}: channel count",
    fix.name
  );
  // Cross-check codec_string ↔ bits_per_sample: catches a fixture
  // moved into the wrong codec dir during a future re-org.
  let expected_bits = match fix.codec_string {
    "pcm-s16" => 16,
    "pcm-f32" => 32,
    other => panic!("{}: unsupported codec_string {other}", fix.name),
  };
  assert_eq!(
    bits_per_sample, expected_bits,
    "{}: bits-per-sample mismatch with codec_string",
    fix.name
  );

  let time_base = Timebase::new(
    1,
    core::num::NonZeroU32::new(sample_rate).expect("non-zero rate"),
  );
  let mut decoder = WebCodecsAudioStreamDecoder::open_with_codec_string(
    fix.codec_string,
    None,
    sample_rate,
    EXPECTED_CHANNELS,
    time_base,
  )
  .unwrap_or_else(|e| panic!("{}: open: {e:?}", fix.name));

  let bytes_per_chunk = SAMPLES_PER_CHUNK * (bits_per_sample / 8) as usize * channels as usize;
  let mut total_samples_in: u64 = 0;
  let mut total_samples_out: u64 = 0;
  let mut frame = empty_audio_frame();
  // PTS preservation guards the codex-flagged regression: when
  // Chrome's PCM decoder rebases output `AudioData.timestamp`
  // onto microseconds, the side-map lookup misses on the
  // submission ID. The output callback now FIFO-pops the
  // oldest pending record on miss, which preserves the user's
  // PTS — `AudioFrame.pts` must be `Some` for every frame, and
  // values must be monotonically non-decreasing across the
  // stream. A regression to the synthesise-`None` fallback
  // would trip the first `expect`; a swap of pending-record
  // matching order would trip the monotonic check.
  let mut last_pts: Option<i64> = None;
  let mut frames_with_pts: u64 = 0;
  let mut frames_total: u64 = 0;

  for (i, chunk_bytes) in pcm.chunks(bytes_per_chunk).enumerate() {
    let chunk_samples =
      (chunk_bytes.len() / (bits_per_sample / 8) as usize / channels as usize) as u64;
    let pts = mediadecode::Timestamp::new((i as u64 * SAMPLES_PER_CHUNK as u64) as i64, time_base);
    let packet = AudioPacket::new(
      WebCodecsBuffer::from_bytes(chunk_bytes.to_vec()),
      AudioPacketExtra::new(true),
    )
    .with_flags(PacketFlags::KEY)
    .with_pts(Some(pts));

    decoder
      .send_packet(&packet)
      .await
      .unwrap_or_else(|e| panic!("{}: send_packet: {e:?}", fix.name));
    total_samples_in += chunk_samples;

    loop {
      match decoder.receive_frame(&mut frame).await {
        Ok(()) => {
          total_samples_out = total_samples_out.saturating_add(frame.nb_samples() as u64);
          frames_total = frames_total.saturating_add(1);
          assert_eq!(frame.sample_rate(), sample_rate, "{}", fix.name);
          assert_eq!(frame.channel_count(), EXPECTED_CHANNELS, "{}", fix.name);
          let pts = frame
            .pts()
            .unwrap_or_else(|| panic!("{}: AudioFrame.pts is None", fix.name));
          frames_with_pts = frames_with_pts.saturating_add(1);
          let pts_value = pts.pts();
          if let Some(prev) = last_pts {
            assert!(
              pts_value >= prev,
              "{}: AudioFrame.pts went backwards ({pts_value} < {prev})",
              fix.name,
            );
          }
          last_pts = Some(pts_value);
        }
        Err(AudioDecodeError::NoFrameReady) => break,
        Err(e) => panic!("{}: receive_frame: {e:?}", fix.name),
      }
    }
  }

  decoder
    .send_eof()
    .await
    .unwrap_or_else(|e| panic!("{}: send_eof: {e:?}", fix.name));
  loop {
    match decoder.receive_frame(&mut frame).await {
      Ok(()) => {
        total_samples_out = total_samples_out.saturating_add(frame.nb_samples() as u64);
        frames_total = frames_total.saturating_add(1);
        let pts = frame
          .pts()
          .unwrap_or_else(|| panic!("{}: post-EOF AudioFrame.pts is None", fix.name));
        frames_with_pts = frames_with_pts.saturating_add(1);
        let pts_value = pts.pts();
        if let Some(prev) = last_pts {
          assert!(
            pts_value >= prev,
            "{}: post-EOF AudioFrame.pts went backwards ({pts_value} < {prev})",
            fix.name,
          );
        }
        last_pts = Some(pts_value);
      }
      Err(AudioDecodeError::Eof) => break,
      Err(AudioDecodeError::NoFrameReady) => continue,
      Err(e) => panic!("{}: post-EOF receive_frame: {e:?}", fix.name),
    }
  }
  assert_eq!(
    frames_with_pts, frames_total,
    "{}: every decoded frame must carry a PTS",
    fix.name,
  );

  assert_eq!(
    total_samples_in, fix.expected_samples,
    "{}: input sample count",
    fix.name,
  );
  assert_eq!(
    total_samples_out, fix.expected_samples,
    "{}: decoded sample count drifted ({total_samples_out} got, {} expected)",
    fix.name, fix.expected_samples,
  );
}

#[wasm_bindgen_test]
async fn decode_all_audio_fixtures() {
  for fix in FIXTURES {
    decode_fixture(fix).await;
  }
}

//! Test media for the demux and resample lanes, generated at run time.
//!
//! # Why generated and not committed
//!
//! This repository already carries a media corpus: the `audio-fixtures`
//! submodule under `tests/fixtures/audio`, which
//! `tests/audio_pcm_fixtures.rs` sweeps end to end. It is 16 kHz mono
//! WAV throughout. Nothing in it has a second track, a subtitle, a
//! font, cover art, a timecode stream, or a sample rate worth
//! converting *from* — so it can prove a decoder and cannot prove
//! either of the contracts this file's consumers are about.
//!
//! Those shapes are therefore made here, with the `ffmpeg` CLI, into a
//! temp directory that goes away with the run. Nothing binary is
//! committed, and the recipe is readable beside the assertions it
//! feeds.
//!
//! # When the CLI is absent
//!
//! [`Corpus::new`] returns `None` after printing why, and every lane
//! that needs generated media returns early — the same welcome the
//! submodule sweep extends to a contributor who has not run
//! `git submodule update --init`. Lanes that need no media (the seek
//! arithmetic, the mid-stream refusal) run regardless.

#![allow(dead_code)]

use std::{
  path::{Path, PathBuf},
  process::Command,
  sync::Once,
};

/// Initialises libavformat exactly once per test binary.
pub fn init_ffmpeg() {
  static ONCE: Once = Once::new();
  ONCE.call_once(|| {
    ffmpeg_next::init().expect("ffmpeg init");
  });
}

/// A temp directory holding generated media. Dropped with the test.
pub struct Corpus {
  dir: tempfile::TempDir,
}

impl Corpus {
  /// Creates the corpus directory, or returns `None` when the `ffmpeg`
  /// CLI is not on `PATH`.
  pub fn new() -> Option<Self> {
    if !ffmpeg_cli_available() {
      eprintln!(
        "skip: the `ffmpeg` CLI is not on PATH — this lane generates its own test media \
         (multi-track / cover-art / timecode containers and sine WAVs) because the committed \
         corpus has none of those shapes."
      );
      return None;
    }
    init_ffmpeg();
    Some(Self {
      dir: tempfile::tempdir().expect("temp dir"),
    })
  }

  fn path(&self, name: &str) -> PathBuf {
    self.dir.path().join(name)
  }

  /// A Matroska file with one track of each of four kinds: H.264
  /// video, 48 kHz stereo AAC, a SubRip subtitle track, and an attached
  /// "font" whose payload lives in codec extradata and never appears in
  /// the packet stream.
  #[rustfmt::skip]
  pub fn multi_track_mkv(&self) -> PathBuf {
    let out = self.path("multi.mkv");
    if out.exists() {
      return out;
    }
    let font = self.path("font.ttf");
    std::fs::write(&font, FONT_PAYLOAD).expect("write attachment payload");
    let subs = self.path("subs.srt");
    std::fs::write(&subs, SUBRIP).expect("write subtitles");

    run_ffmpeg(&[
      "-f", "lavfi", "-i", "testsrc2=size=160x120:rate=25:duration=2",
      "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=2",
      "-i", subs.to_str().expect("utf-8 path"),
      "-map", "0:v", "-map", "1:a", "-map", "2:s",
      "-c:v", "libx264", "-preset", "ultrafast", "-g", "25", "-pix_fmt", "yuv420p",
      "-c:a", "aac", "-ac", "2", "-ar", "48000",
      "-c:s", "srt",
      "-attach", font.to_str().expect("utf-8 path"),
      "-metadata:s:t", "mimetype=application/x-truetype-font",
      "-metadata:s:t", "filename=font.ttf",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }

  /// An MP3 with a PNG cover image attached — libavformat presents it
  /// as a video stream carrying `AV_DISPOSITION_ATTACHED_PIC`, parks
  /// the real packet in `AVStream.attached_pic`, *and* emits that same
  /// packet in the stream. The exactly-once contract has to survive
  /// all three facts at once.
  #[rustfmt::skip]
  pub fn cover_art_mp3(&self) -> PathBuf {
    let out = self.path("cover.mp3");
    if out.exists() {
      return out;
    }
    let cover = self.path("cover.png");
    run_ffmpeg(&[
      "-f", "lavfi", "-i", "color=c=red:size=32x32:duration=0.04:rate=25",
      "-frames:v", "1",
      cover.to_str().expect("utf-8 path"),
    ]);
    run_ffmpeg(&[
      "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=2",
      "-i", cover.to_str().expect("utf-8 path"),
      "-map", "0:a", "-map", "1:v",
      "-c:a", "libmp3lame", "-c:v", "copy",
      "-id3v2_version", "3",
      "-disposition:v", "attached_pic",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }

  /// A QuickTime file whose `-timecode` flag adds a `tmcd` track —
  /// `AVMEDIA_TYPE_DATA`, one packet for the whole file. The only
  /// commonly-generated container shape that exercises the `Data` arm.
  #[rustfmt::skip]
  pub fn timecode_mov(&self) -> PathBuf {
    let out = self.path("timecode.mov");
    if out.exists() {
      return out;
    }
    run_ffmpeg(&[
      "-f", "lavfi", "-i", "testsrc2=size=160x120:rate=25:duration=2",
      "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=2",
      "-map", "0:v", "-map", "1:a",
      "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
      "-c:a", "aac",
      "-timecode", "01:00:00:00",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }

  /// A signed-16-bit PCM WAV holding `seconds` of a `hz` sine at
  /// `rate` Hz across `channels` channels. PCM so the decode step adds
  /// no error of its own to whatever the resample lane measures.
  #[rustfmt::skip]
  pub fn sine_wav(&self, name: &str, rate: u32, channels: u8, hz: u32, seconds: f32) -> PathBuf {
    let out = self.path(name);
    if out.exists() {
      return out;
    }
    let source = format!("sine=frequency={hz}:sample_rate={rate}:duration={seconds}");
    run_ffmpeg(&[
      "-f", "lavfi", "-i", &source,
      "-ac", &channels.to_string(),
      "-c:a", "pcm_s16le",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }
}

/// Bytes standing in for a font. The Matroska muxer attaches whatever
/// file it is handed and takes the MIME type from metadata, so the
/// payload only has to be recognisable on the way back out — which is
/// exactly what the extradata-synthesis lane asserts. A real TTF would
/// add a licence to the repository and prove nothing further.
pub const FONT_PAYLOAD: &[u8] = b"FAKE-TTF-PAYLOAD-0123456789";

const SUBRIP: &str = "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n\
                      2\n00:00:01,000 --> 00:00:02,000\nworld\n\n";

fn ffmpeg_cli_available() -> bool {
  Command::new("ffmpeg")
    .arg("-version")
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false)
}

fn run_ffmpeg(args: &[&str]) {
  let out = Command::new("ffmpeg")
    .args(["-y", "-loglevel", "error"])
    .args(args)
    .output()
    .expect("run ffmpeg");
  assert!(
    out.status.success(),
    "ffmpeg {args:?} failed:\n{}",
    String::from_utf8_lossy(&out.stderr),
  );
}

/// The `(stream_index, pts)` sequence a bare `av_read_frame` loop sees,
/// which is the container's own interleaved order by definition. The
/// demux lane compares against this rather than against a hand-written
/// expectation, so the assertion stays true for any file.
pub fn raw_packet_order(path: &Path) -> Vec<(usize, Option<i64>)> {
  let mut input = ffmpeg_next::format::input(path).expect("open input");
  let mut out = Vec::new();
  loop {
    let mut packet = ffmpeg_next::Packet::empty();
    match packet.read(&mut input) {
      Ok(()) => out.push((packet.stream(), packet.pts())),
      Err(ffmpeg_next::Error::Eof) => break,
      Err(ffmpeg_next::Error::InvalidData) => continue,
      Err(e) => panic!("read: {e}"),
    }
  }
  out
}

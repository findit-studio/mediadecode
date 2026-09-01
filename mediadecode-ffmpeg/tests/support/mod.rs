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

  /// The corpus directory itself, for a lane that mints its own
  /// fixture and wants it to live and die with the run.
  ///
  /// `tests/codec_ticket_parity.rs` is that lane: stream-level
  /// `coded_side_data` has no `ffmpeg` CLI recipe, so it assembles a
  /// MOV carrying a `colr`/`prof` ICC atom byte by byte and drops it in
  /// here.
  pub fn dir(&self) -> &Path {
    self.dir.path()
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

  /// A Matroska file whose audio and subtitle tracks carry **language
  /// tags** and whose video track carries none.
  ///
  /// The three shapes issue #44 is about, in one file:
  ///
  /// - `jpn` on the audio — an ordinary ISO 639-2 declaration;
  /// - `ger` on the subtitles — 639-2/**B**, which is what an MKV
  ///   writes where an MP4 writes the /T spelling `deu`. The pair is
  ///   the reason a demux tier must not fold: they are one language
  ///   under two codes, and the table that knows so lives downstream;
  /// - **nothing** on the video, because Matroska omits the element
  ///   rather than writing a placeholder — which is what makes `None`
  ///   a real answer here.
  ///
  /// See [`Self::language_tagged_mp4`] for the fourth shape, which
  /// Matroska cannot produce.
  #[rustfmt::skip]
  pub fn language_tagged_mkv(&self) -> PathBuf {
    let out = self.path("language.mkv");
    if out.exists() {
      return out;
    }
    let subs = self.subrip();

    run_ffmpeg(&[
      "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25:duration=1",
      "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1",
      "-i", subs.to_str().expect("utf-8 path"),
      "-map", "0:v", "-map", "1:a", "-map", "2:s",
      "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
      "-c:a", "aac", "-c:s", "srt",
      "-metadata:s:a:0", "language=jpn",
      "-metadata:s:s:0", "language=ger",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }

  /// The same declaration in an **MP4**, where the untagged track says
  /// `und` instead of saying nothing.
  ///
  /// This is the fourth shape, and the one that makes the seat's
  /// `Option` mean something: an ISOBMFF `mdhd` has a language field it
  /// must fill, so a track nobody tagged is written `und` —
  /// *undetermined*, which the file really does say. Matroska simply
  /// omits the element. A door that folded `und` into `None`, or `None`
  /// into `und`, would erase the difference between a file that
  /// declined to say and one that said it did not know.
  #[rustfmt::skip]
  pub fn language_tagged_mp4(&self) -> PathBuf {
    let out = self.path("language.mp4");
    if out.exists() {
      return out;
    }
    run_ffmpeg(&[
      "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25:duration=1",
      "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1",
      "-map", "0:v", "-map", "1:a",
      "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
      "-c:a", "aac",
      "-metadata:s:a:0", "language=ger",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }

  /// A SubRip file, written by hand.
  ///
  /// **The queue-backed demuxer family.** `srtdec` — like SubViewer,
  /// MicroDVD, WebVTT and the rest built on `FFDemuxSubtitlesQueue` —
  /// parses every cue at open, keeps the packets in its own queue, and
  /// answers each `av_read_frame` with an `av_packet_ref` of one. Every
  /// packet it delivers therefore arrives with **two** references
  /// through nobody's fault, which is the shape that has to keep
  /// working on both lanes.
  ///
  /// Written rather than transcoded because SubRip is a text format:
  /// the cheapest possible fixture for the shape, and one that needs no
  /// encoder to exist.
  pub fn subrip(&self) -> PathBuf {
    let out = self.path("cues.srt");
    if out.exists() {
      return out;
    }
    std::fs::write(
      &out,
      "1\n00:00:01,000 --> 00:00:02,000\nfirst cue\n\n\
       2\n00:00:03,000 --> 00:00:04,500\nsecond cue\n\n\
       3\n00:00:06,000 --> 00:00:07,000\nthird cue\n\n",
    )
    .expect("writing the subrip fixture");
    out
  }

  /// [`Self::subrip`] with cues too big to fit a capped allocator.
  ///
  /// The queue-backed road delivers by `av_packet_ref`, which allocates
  /// only a reference struct however large the cue is — so a cap set
  /// between "a reference" and "a cue" lets `av_read_frame` through and
  /// stops the copy this crate makes afterwards. That separation is
  /// what the middle-row fault lane needs, and small cues cannot
  /// provide it.
  pub fn subrip_bulky(&self) -> PathBuf {
    let out = self.path("bulky.srt");
    if out.exists() {
      return out;
    }
    let mut text = String::new();
    for (index, marker) in ["alpha", "beta", "gamma"].iter().enumerate() {
      let start = index * 3;
      text.push_str(&format!(
        "{}\n00:00:{:02},000 --> 00:00:{:02},000\n{}{}\n\n",
        index + 1,
        start + 1,
        start + 2,
        marker,
        "x".repeat(8192),
      ));
    }
    std::fs::write(&out, text).expect("writing the bulky subrip fixture");
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

  /// A 32×24 JPEG carrying EXIF orientation `tag` in its IFD0.
  ///
  /// # Why the tag is spliced in rather than muxed
  ///
  /// The `ffmpeg` CLI cannot write one. Its mjpeg encoder emits no
  /// EXIF APP1 at all, and `-metadata Orientation=6` lands in the
  /// container's metadata rather than in the file's EXIF IFD — which
  /// is the road the *decoder* reads. So the CLI makes the pixels and
  /// this function makes the tag: a 32-byte APP1 segment, built here,
  /// spliced in directly after the SOI marker where a JPEG's first
  /// application segment belongs.
  ///
  /// That keeps the fixture road's own rule — nothing binary
  /// committed, the recipe readable beside the assertions it feeds —
  /// and it is the only way to mint the shape at all.
  ///
  /// Tags outside 1..=8 are accepted and written verbatim: an
  /// out-of-range tag is a real shape a file can carry, and what
  /// libavcodec does with it (emit no display matrix) is worth
  /// pinning.
  pub fn exif_oriented_jpeg(&self, tag: u16) -> PathBuf {
    let out = self.path(&format!("orient{tag}.jpg"));
    if out.exists() {
      return out;
    }
    let plain = self.path("orient-plain.jpg");
    if !plain.exists() {
      run_ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red:size=32x24:duration=0.04:rate=25",
        "-frames:v",
        "1",
        plain.to_str().expect("utf-8 path"),
      ]);
    }
    let jpeg = std::fs::read(&plain).expect("read the plain jpeg");
    assert_eq!(
      &jpeg[..2],
      b"\xff\xd8",
      "an ffmpeg-written JPEG starts with SOI"
    );

    let mut spliced = Vec::with_capacity(jpeg.len() + 34);
    spliced.extend_from_slice(&jpeg[..2]);
    spliced.extend_from_slice(&exif_orientation_app1(tag));
    spliced.extend_from_slice(&jpeg[2..]);
    std::fs::write(&out, &spliced).expect("write the oriented jpeg");
    out
  }

  /// An **indexed** (paletted) PNG — `pal8` on the way back out.
  ///
  /// The `png` encoder emits `pal8` when handed `-pix_fmt pal8`, which
  /// is what a real indexed cover image is. Its data plane is palette
  /// indices and its palette rides `data[1]` at a fixed 1024 bytes.
  pub fn indexed_png(&self) -> PathBuf {
    let out = self.path("indexed.png");
    if out.exists() {
      return out;
    }
    #[rustfmt::skip]
    run_ffmpeg(&[
      "-f", "lavfi", "-i", "testsrc2=size=32x24:rate=1:duration=1",
      "-frames:v", "1", "-pix_fmt", "pal8",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }

  /// A clip in a codec no hardware backend on this platform
  /// accelerates, so `VideoDecoder` opens software directly.
  ///
  /// Censused on this build: VideoToolbox advertises a hardware config
  /// for h264, hevc, vp9, mpeg4, mpeg2video and av1 — but **not** for
  /// `vp8`, `ffv1`, `theora` or `mjpeg`. So a VP8 clip reaches the
  /// software road without a probe, which is what makes the direct
  /// software funnel testable at all: on any accelerated codec the
  /// hardware pool judge answers first and this funnel never runs.
  pub fn software_only_video(&self) -> PathBuf {
    let out = self.path("swonly.webm");
    if out.exists() {
      return out;
    }
    #[rustfmt::skip]
    run_ffmpeg(&[
      "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=5:d=1",
      "-c:v", "libvpx", "-pix_fmt", "yuv420p",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }

  /// An h264 clip whose **display** extent is 32x32 and whose **coded**
  /// surface is 1920x1088 — a 2040x divergence, written into the SPS as
  /// real cropping by x264's `crop-rect`.
  ///
  /// This is the shape that separates the two dimension vocabularies.
  /// `max_pixels` is applied by `ff_set_dimensions` to the *display*
  /// dims, so 1024 pixels is all it ever sees; what the decoder
  /// allocates is the coded surface, which `get_buffer2` really does
  /// receive at 1920x1088 (measured: aligned to 1920x1090, a 2 MiB
  /// buffer).
  pub fn cropped_h264(&self) -> PathBuf {
    let out = self.path("cropped.mp4");
    if out.exists() {
      return out;
    }
    #[rustfmt::skip]
    run_ffmpeg(&[
      "-f", "lavfi", "-i", "testsrc2=size=1920x1088:rate=5:d=1",
      "-c:v", "libx264",
      "-x264-params", "crop-rect=0,0,1888,1056",
      "-pix_fmt", "yuv420p",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }

  /// A 6-channel FLAC, whose blocks are the shape the over-divided
  /// sample ruler refused: 65,535 samples x 6 channels is 393,210
  /// channel-samples of ordinary surround media.
  pub fn surround_flac(&self) -> PathBuf {
    let out = self.path("surround.flac");
    if out.exists() {
      return out;
    }
    let src = self.sine_wav("surround-src.wav", 48_000, 6, 440, 1.0);
    #[rustfmt::skip]
    run_ffmpeg(&[
      "-i", src.to_str().expect("utf-8 path"),
      "-c:a", "flac", "-sample_fmt", "s32",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }

  /// A grayscale PNG of exactly `width x height`.
  ///
  /// Used for the degenerate-aspect-ratio lanes: a 65536x1 picture is
  /// 64 KiB by `width * height` and 2 MiB once libavcodec rounds its
  /// single row up to the 32 its allocator wants. Flat colour, so even
  /// the very wide ones encode to a few hundred bytes.
  pub fn gray_png(&self, width: u32, height: u32) -> PathBuf {
    let out = self.path(&format!("gray-{width}x{height}.png"));
    if out.exists() {
      return out;
    }
    let size = format!("color=c=gray:s={width}x{height}:r=1:d=1");
    #[rustfmt::skip]
    run_ffmpeg(&[
      "-f", "lavfi", "-i", &size,
      "-frames:v", "1", "-pix_fmt", "gray",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }

  /// A **1-bit** PNG — `monob` on the way back out, whose rows are
  /// `ceil(width / 8)` bytes of packed bits.
  pub fn monochrome_png(&self) -> PathBuf {
    let out = self.path("mono.png");
    if out.exists() {
      return out;
    }
    #[rustfmt::skip]
    run_ffmpeg(&[
      "-f", "lavfi", "-i", "testsrc2=size=32x24:rate=1:duration=1",
      "-frames:v", "1", "-pix_fmt", "monob",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }

  /// A tiny HEVC clip carrying real HDR10 static metadata: PQ transfer
  /// (`smpte2084`), BT.2020 primaries, and both mastering-display and
  /// content-light-level SEI messages — `libx265`'s own `hdr10=1`
  /// bitstream writer, not a hand-rolled approximation.
  ///
  /// `ffmpeg`'s CLI has no flag to attach `AV_FRAME_DATA_MASTERING_
  /// DISPLAY_METADATA` / `AV_FRAME_DATA_CONTENT_LIGHT_LEVEL` directly
  /// (`hevc_metadata` only reaches the VUI colour tags); `libx265`'s
  /// `-x265-params master-display=…:max-cll=…` is the one encoder path
  /// available here that writes the SEI, and it is a real HEVC decode
  /// FFmpeg's own parser reads back — cross-checked with
  /// `ffprobe -show_frames` before these exact numbers went into
  /// `convert::tests`' unit fixtures. Like every other codec this
  /// module's recipes name (`libx264`, `aac`, `flac`, …), `libx265` is
  /// assumed present rather than separately probed — the same stance
  /// [`run_ffmpeg`] takes everywhere else; it ships in the Homebrew
  /// `ffmpeg` formula this crate's own CI installs.
  #[rustfmt::skip]
  pub fn hdr10_hevc(&self) -> PathBuf {
    let out = self.path("hdr10.mp4");
    if out.exists() {
      return out;
    }
    run_ffmpeg(&[
      "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=5:duration=1",
      "-c:v", "libx265", "-pix_fmt", "yuv420p10le",
      "-x265-params",
      "hdr10=1:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:\
       master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):\
       max-cll=1000,400",
      out.to_str().expect("utf-8 path"),
    ]);
    out
  }

  /// A tiny HEVC clip tagged HLG (`arib-std-b67`) / BT.2020, carrying
  /// **no** mastering-display or content-light-level side data — the
  /// paired "absent metadata answers absent" fixture to
  /// [`Self::hdr10_hevc`]: a different, real transfer characteristic,
  /// decoded through the same path, with the HDR10-only seats reading
  /// `None` rather than a leftover default.
  #[rustfmt::skip]
  pub fn hlg_hevc(&self) -> PathBuf {
    let out = self.path("hlg.mp4");
    if out.exists() {
      return out;
    }
    run_ffmpeg(&[
      "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=5:duration=1",
      "-c:v", "libx265", "-pix_fmt", "yuv420p10le",
      "-x265-params",
      "repeat-headers=1:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc",
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

/// A JPEG APP1 segment whose EXIF IFD0 carries exactly one entry:
/// Orientation (tag `0x0112`), type SHORT, value `orientation`.
///
/// Little-endian (`II`) throughout, which is what the `4949` magic
/// declares and what every consumer of this fixture reads.
fn exif_orientation_app1(orientation: u16) -> Vec<u8> {
  let mut ifd = Vec::new();
  ifd.extend_from_slice(&1u16.to_le_bytes()); // one entry
  ifd.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
  ifd.extend_from_slice(&3u16.to_le_bytes()); // SHORT
  ifd.extend_from_slice(&1u32.to_le_bytes()); // count
  ifd.extend_from_slice(&orientation.to_le_bytes()); // value, in the
  ifd.extend_from_slice(&0u16.to_le_bytes()); // 4-byte value field
  ifd.extend_from_slice(&0u32.to_le_bytes()); // no next IFD

  let mut payload = Vec::new();
  payload.extend_from_slice(b"Exif\0\0");
  payload.extend_from_slice(b"II"); // little-endian
  payload.extend_from_slice(&42u16.to_le_bytes()); // TIFF magic
  payload.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at offset 8
  payload.extend_from_slice(&ifd);

  let mut segment = vec![0xFF, 0xE1];
  // The length field counts itself and the payload, and is the one
  // big-endian number in a little-endian file: it belongs to JPEG's
  // framing, not to TIFF's.
  let len = u16::try_from(payload.len() + 2).expect("the segment is 34 bytes");
  segment.extend_from_slice(&len.to_be_bytes());
  segment.extend_from_slice(&payload);
  segment
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

/// Asserts a submission was taken, and answers nothing.
///
/// The `#[must_use]` on [`mediadecode::Sent`] is deliberate teeth: a
/// test that submits and ignores the answer would not notice a decoder
/// quietly asking to be drained. These lanes submit into a session they
/// have just emptied, where
/// [`Sent::MustDrain`](mediadecode::Sent::MustDrain) is a real
/// surprise — so they say so here rather than dropping it.
#[track_caller]
pub fn accepted<E: std::fmt::Debug>(status: Result<mediadecode::Sent, E>, what: &str) {
  assert_eq!(
    status.unwrap_or_else(|e| panic!("{what}: {e:?}")),
    mediadecode::Sent::Accepted,
    "{what}: the session asked to be drained where the test expected room",
  );
}

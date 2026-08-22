//! The resample seam's contracts, pinned against real audio.
//!
//! The pipeline under test is the whole substrate: `FfmpegDemuxer` →
//! `FfmpegAudioStreamDecoder` → `FfmpegResampler`, driven through the
//! `mediadecode` traits. The media is PCM, generated at run time (see
//! `support/mod.rs`), so the decode step contributes no error of its
//! own to what the measurements below see.
//!
//! What is pinned:
//!
//! - a 48 kHz → 16 kHz conversion keeps the tone and lands within a
//!   few samples of the exact 3:1 length;
//! - a 44.1 kHz → 16 kHz conversion — a fractional ratio, 441:160 —
//!   does too, and leaves a tail inside the filter that only
//!   `send_eof` gets out;
//! - stereo folds to mono, layout and plane count following;
//! - output timestamps are continuous across every frame including the
//!   drained tail: `pts(n+1) == pts(n) + nb_samples(n)`;
//! - a mid-stream format change is refused by name, with no file
//!   involved.

mod support;

use ffmpeg_next::{
  ChannelLayout,
  format::{Sample, sample::Type},
};
use mediadecode::{
  decoder::AudioStreamDecoder,
  demuxer::{DemuxedPacket, Demuxer, TrackKind},
  resampler::AudioResampler,
};
use mediadecode_ffmpeg::{
  FfmpegAudioStreamDecoder, FfmpegDemuxer, FfmpegResampler, ResampleError, ResampleSpec,
  empty_audio_frame,
};
use support::Corpus;

/// Everything one run of the pipeline produced.
struct Converted {
  /// Interleaved samples, in the target format's native width.
  samples: Vec<i16>,
  /// `(pts, nb_samples)` per delivered frame, in target-rate ticks.
  frames: Vec<(i64, u32)>,
  /// How many frames came out only after `send_eof` — the conversion
  /// tail.
  tail_frames: usize,
  /// The channel count the produced frames advertised.
  channels: u8,
  /// The plane count the produced frames advertised.
  planes: u8,
}

/// Demuxes, decodes and resamples one file end to end through the
/// trait surface.
fn run(path: &std::path::Path, target: ResampleSpec) -> Converted {
  let mut demuxer = FfmpegDemuxer::open(path).expect("open");
  let track = demuxer
    .tracks()
    .iter()
    .position(|t| t.kind() == TrackKind::Audio)
    .expect("an audio track");
  let info = &demuxer.tracks()[track];
  let mut decoder =
    FfmpegAudioStreamDecoder::open(info.extra().parameters().clone(), info.timebase())
      .expect("open decoder");

  // The source spec comes off the opened decoder: it is exactly what
  // the frames will carry. The track's declared parameters agree here
  // (PCM), and the lane below checks that they do.
  let source = ResampleSpec::from_decoder(decoder.inner()).expect("the decoder names its shape");
  assert_eq!(
    ResampleSpec::from_parameters(info.extra().parameters()),
    Some(source),
    "for PCM the declared spec and the decoder's own spec are the same",
  );

  let mut resampler = FfmpegResampler::new(source, target).expect("open resampler");
  let mut decoded = empty_audio_frame();
  let mut out = empty_audio_frame();
  let mut converted = Converted {
    samples: Vec::new(),
    frames: Vec::new(),
    tail_frames: 0,
    channels: 0,
    planes: 0,
  };

  let collect = |converted: &mut Converted, frame: &mediadecode_ffmpeg::AudioFrame| {
    converted.channels = frame.channel_count();
    converted.planes = frame.plane_count();
    converted.frames.push((
      frame.pts().expect("every output frame is stamped").pts(),
      frame.nb_samples(),
    ));
    // The target format below is packed s16, so plane 0 interleaves
    // every channel. The plane's buffer is padded to FFmpeg's
    // alignment; the valid range is what the header claims.
    let valid = frame.nb_samples() as usize * frame.channel_count() as usize * 2;
    let bytes = &frame.planes()[0].data_ref().as_ref()[..valid];
    converted.samples.extend(
      bytes
        .as_chunks::<2>()
        .0
        .iter()
        .copied()
        .map(i16::from_le_bytes),
    );
  };

  while let Some(packet) = demuxer.next_packet().expect("pull") {
    let DemuxedPacket::Audio { track: t, packet } = packet else {
      continue;
    };
    if t.get() != track {
      continue;
    }
    decoder.send_packet(&packet).expect("send_packet");
    while decoder.receive_frame(&mut decoded).is_ok() {
      resampler.send_frame(&decoded).expect("send_frame");
      while resampler.receive_frame(&mut out).is_ok() {
        collect(&mut converted, &out);
      }
    }
  }
  decoder.send_eof().expect("decoder eof");
  while decoder.receive_frame(&mut decoded).is_ok() {
    resampler.send_frame(&decoded).expect("send_frame");
    while resampler.receive_frame(&mut out).is_ok() {
      collect(&mut converted, &out);
    }
  }

  // The tail. Everything after this point is what would be lost.
  resampler.send_eof().expect("resampler eof");
  while resampler.receive_frame(&mut out).is_ok() {
    converted.tail_frames += 1;
    collect(&mut converted, &out);
  }
  converted
}

/// Sign changes across the signal — for a pure tone, twice its
/// frequency times its duration. Cheap, exact enough to tell 440 Hz
/// from the 1320 Hz a skipped resample or the 147 Hz a doubled one
/// would produce, and readable without a transform.
fn zero_crossings(samples: &[i16]) -> usize {
  samples
    .windows(2)
    .filter(|w| (w[0] >= 0) != (w[1] >= 0))
    .count()
}

fn mono_16k() -> ResampleSpec {
  ResampleSpec::new(16_000, Sample::I16(Type::Packed), ChannelLayout::MONO)
}

#[test]
fn forty_eight_to_sixteen_keeps_the_tone_and_the_length() {
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.sine_wav("sine48.wav", 48_000, 1, 440, 1.0);
  let out = run(&path, mono_16k());

  // 3:1 exactly. The window is ±0.25% rather than exact so a different
  // `swr` engine may round its edges differently; FFmpeg 9's default
  // lands on 16000 on the nose, and a conversion that forgot to drain
  // or forgot to filter misses by a whole factor.
  assert!(
    (15_960..=16_040).contains(&out.samples.len()),
    "16000 samples expected, got {}",
    out.samples.len(),
  );
  // 440 Hz over one second: 880 sign changes. A conversion that
  // dropped samples instead of filtering would land near 2640.
  let crossings = zero_crossings(&out.samples);
  assert!(
    (860..=900).contains(&crossings),
    "440 Hz means ~880 zero crossings, got {crossings}",
  );
  assert_eq!(out.channels, 1);
}

#[test]
fn a_fractional_ratio_still_lands_on_length_and_leaves_a_tail() {
  let Some(corpus) = Corpus::new() else { return };
  // 44100 -> 16000 is 441:160: no whole-number relationship, so the
  // filter never empties on a frame boundary and the tail is real.
  let path = corpus.sine_wav("sine44.wav", 44_100, 1, 440, 1.0);
  let out = run(&path, mono_16k());

  assert!(
    (15_960..=16_040).contains(&out.samples.len()),
    "16000 samples expected, got {}",
    out.samples.len(),
  );
  let crossings = zero_crossings(&out.samples);
  assert!(
    (860..=900).contains(&crossings),
    "440 Hz means ~880 zero crossings, got {crossings}",
  );
  assert!(
    out.tail_frames > 0,
    "send_eof drained nothing — every file would lose its last tens of milliseconds",
  );
}

#[test]
fn stereo_folds_to_mono() {
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.sine_wav("sine48-stereo.wav", 48_000, 2, 440, 1.0);

  let source = {
    let demuxer = FfmpegDemuxer::open(&path).expect("open");
    ResampleSpec::from_parameters(demuxer.tracks()[0].extra().parameters()).expect("audio spec")
  };
  assert_eq!(source.channels(), 2, "the file really is stereo");

  let out = run(&path, mono_16k());
  assert_eq!(out.channels, 1, "the target layout is what comes out");
  assert_eq!(out.planes, 1, "packed s16 mono is one plane");
  assert!(
    (15_960..=16_040).contains(&out.samples.len()),
    "one sample per output frame per channel, and there is one channel now: got {}",
    out.samples.len(),
  );
}

#[test]
fn output_timestamps_are_continuous_through_the_tail() {
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.sine_wav("sine44.wav", 44_100, 1, 440, 1.0);
  let out = run(&path, mono_16k());

  assert!(out.frames.len() > 1, "more than one frame to compare");
  assert_eq!(
    out.frames[0].0, 0,
    "the source starts at zero, so does the output"
  );
  for pair in out.frames.windows(2) {
    let (pts, samples) = pair[0];
    let (next, _) = pair[1];
    assert_eq!(
      next,
      pts + i64::from(samples),
      "a gap or an overlap in the output timeline at pts {pts}",
    );
  }
  let (last_pts, last_len) = *out.frames.last().expect("at least one frame");
  assert_eq!(
    last_pts + i64::from(last_len),
    out.samples.len() as i64,
    "the timeline ends where the samples do",
  );
}

#[test]
fn a_mid_stream_format_change_is_refused_by_name() {
  // No file: the refusal is a property of the face, and a decoded
  // frame with the wrong header is enough to provoke it.
  support::init_ffmpeg();

  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  let mut resampler = FfmpegResampler::new(source, mono_16k()).expect("open resampler");

  let good = mediadecode_ffmpeg::AudioFrame::new(
    48_000,
    0,
    2,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&ChannelLayout::STEREO),
    std::array::from_fn(|_| {
      mediadecode::frame::Plane::new(mediadecode_ffmpeg::FfmpegBuffer::empty(), 0)
    }),
    1,
    Default::default(),
  );
  resampler
    .send_frame(&good)
    .expect("the declared source spec");

  // Same everything but the rate.
  let changed = mediadecode_ffmpeg::AudioFrame::new(
    44_100,
    0,
    2,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&ChannelLayout::STEREO),
    std::array::from_fn(|_| {
      mediadecode::frame::Plane::new(mediadecode_ffmpeg::FfmpegBuffer::empty(), 0)
    }),
    1,
    Default::default(),
  );
  let err = resampler
    .send_frame(&changed)
    .expect_err("the face never silently reconfigures");
  match err {
    ResampleError::SourceChanged {
      expected_rate,
      found_rate,
      ..
    } => {
      assert_eq!((expected_rate, found_rate), (48_000, 44_100));
    }
    other => panic!("expected a named refusal, got {other:?}"),
  }

  // And the same for a layout change at the same rate.
  let mono = mediadecode_ffmpeg::AudioFrame::new(
    48_000,
    0,
    1,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&ChannelLayout::MONO),
    std::array::from_fn(|_| {
      mediadecode::frame::Plane::new(mediadecode_ffmpeg::FfmpegBuffer::empty(), 0)
    }),
    1,
    Default::default(),
  );
  assert!(matches!(
    resampler.send_frame(&mono),
    Err(ResampleError::SourceChanged { .. }),
  ));
}

#[test]
fn the_needs_more_signal_is_an_error_variant() {
  support::init_ffmpeg();
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  let mut resampler = FfmpegResampler::new(source, mono_16k()).expect("open resampler");
  let mut dst = empty_audio_frame();

  let err = resampler
    .receive_frame(&mut dst)
    .expect_err("nothing has been sent");
  assert!(err.is_again(), "got {err:?}");

  // After EOF with nothing inside, the drain is empty and says so the
  // same way.
  resampler.send_eof().expect("eof");
  assert!(
    resampler
      .receive_frame(&mut dst)
      .expect_err("an empty tail")
      .is_again()
  );

  // `send_frame` after EOF is refused rather than silently accepted.
  let frame = mediadecode_ffmpeg::AudioFrame::new(
    48_000,
    0,
    2,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&ChannelLayout::STEREO),
    std::array::from_fn(|_| {
      mediadecode::frame::Plane::new(mediadecode_ffmpeg::FfmpegBuffer::empty(), 0)
    }),
    1,
    Default::default(),
  );
  assert!(matches!(
    resampler.send_frame(&frame),
    Err(ResampleError::AfterEof),
  ));

  // Flush is the way back: the resampler is reusable for another
  // stream on the same two specs.
  resampler.flush().expect("flush");
  resampler.send_frame(&frame).expect("reusable after flush");
}

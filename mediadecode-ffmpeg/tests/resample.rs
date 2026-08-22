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

// ---------------------------------------------------------------------------
//  Adversarial lanes. Every one of these is a shape a container, a
//  decoder or a caller can produce and no honest file does.
// ---------------------------------------------------------------------------

/// A source frame in the 48 kHz stereo packed-s16 spec, whose header
/// claims `samples` samples while its single plane really holds
/// `plane_len` bytes. The two agree for an honest frame
/// (`samples * 2 channels * 2 bytes`) and disagree for a forged one.
fn stereo_frame(
  samples: u32,
  plane_len: usize,
  pts: Option<i64>,
) -> mediadecode_ffmpeg::AudioFrame {
  filled_frame(
    48_000,
    samples,
    2,
    ChannelLayout::STEREO,
    &vec![0u8; plane_len],
    pts,
  )
}

/// A mono packed-s16 frame at `rate` holding `samples` samples of
/// constant amplitude — DC, which a resampler's low-pass keeps rather
/// than smooths away, so a tail left inside the filter is visible in
/// the next stream's output.
fn mono_frame(
  rate: u32,
  samples: u32,
  amplitude: i16,
  pts: Option<i64>,
) -> mediadecode_ffmpeg::AudioFrame {
  let bytes: Vec<u8> = std::iter::repeat_n(amplitude.to_le_bytes(), samples as usize)
    .flatten()
    .collect();
  filled_frame(rate, samples, 1, ChannelLayout::MONO, &bytes, pts)
}

fn filled_frame(
  rate: u32,
  samples: u32,
  channels: u8,
  layout: ChannelLayout,
  bytes: &[u8],
  pts: Option<i64>,
) -> mediadecode_ffmpeg::AudioFrame {
  let plane = mediadecode_ffmpeg::FfmpegBuffer::copy_from_slice(bytes).expect("plane allocation");
  let planes = std::array::from_fn(|index| {
    mediadecode::frame::Plane::new(
      if index == 0 {
        plane.clone()
      } else {
        mediadecode_ffmpeg::FfmpegBuffer::empty()
      },
      0,
    )
  });
  mediadecode_ffmpeg::AudioFrame::new(
    rate,
    samples,
    channels,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&layout),
    planes,
    1,
    Default::default(),
  )
  .with_pts(pts.map(|pts| {
    mediadecode::Timestamp::new(
      pts,
      mediadecode::Timebase::new(
        1,
        std::num::NonZeroI32::new(rate as i32).expect("a real rate"),
      ),
    )
  }))
}

#[test]
fn a_custom_layout_is_refused_by_name() {
  support::init_ffmpeg();

  // `ResampleSpec::new` is `const` and total, so this is the route that
  // walks straight past `from_parameters` / `from_decoder` and their
  // refusals. A CUSTOM `AVChannelLayout` owns a heap channel map that
  // FFmpeg frees with `av_channel_layout_uninit` — which every owning
  // `AVFrame` runs on drop — while `ffmpeg_next::ChannelLayout` copies
  // it by assignment and has no destructor. One staged frame dropped
  // would free the map this test still holds.
  let mut raw: ffmpeg_next::ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
  let rc = unsafe { ffmpeg_next::ffi::av_channel_layout_custom_init(&mut raw, 2) };
  assert_eq!(rc, 0, "av_channel_layout_custom_init");
  assert_eq!(
    raw.order,
    ffmpeg_next::ffi::AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM,
    "the layout under test really is a custom one",
  );
  let custom = ChannelLayout(raw);

  let hazardous = ResampleSpec::new(48_000, Sample::I16(Type::Packed), custom);
  match FfmpegResampler::new(hazardous, mono_16k()) {
    Err(ResampleError::UnsupportedLayout { end, channels, .. }) => {
      assert_eq!(end.to_string(), "source");
      assert_eq!(channels, 2);
    }
    Err(other) => panic!("expected UnsupportedLayout, got {other:?}"),
    Ok(_) => panic!("a custom layout must not reach swr or a staged frame"),
  }

  // The same refusal from the other end.
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  assert!(matches!(
    FfmpegResampler::new(
      source,
      ResampleSpec::new(16_000, Sample::I16(Type::Packed), custom)
    ),
    Err(ResampleError::UnsupportedLayout {
      end: mediadecode_ffmpeg::SpecEnd::Target,
      ..
    }),
  ));

  // The map is still ours to free: nothing took a copy of it.
  unsafe { ffmpeg_next::ffi::av_channel_layout_uninit(&mut raw) };

  // The rest of the roster the choke point owns, since the `const`
  // constructor cannot.
  assert!(matches!(
    FfmpegResampler::new(
      ResampleSpec::new(0, Sample::I16(Type::Packed), ChannelLayout::STEREO),
      mono_16k(),
    ),
    Err(ResampleError::UnsupportedRate { rate: 0, .. }),
  ));
  assert!(matches!(
    FfmpegResampler::new(
      ResampleSpec::new(48_000, Sample::None, ChannelLayout::STEREO),
      mono_16k(),
    ),
    Err(ResampleError::UnsupportedFormat { .. }),
  ));
  let mut empty: ffmpeg_next::ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
  empty.nb_channels = 0;
  assert!(matches!(
    FfmpegResampler::new(
      ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout(empty)),
      mono_16k(),
    ),
    Err(ResampleError::UnsupportedLayout { channels: 0, .. }),
  ));
}

#[test]
fn a_planar_layout_past_eight_channels_is_refused_at_construction() {
  support::init_ffmpeg();
  // 22.2 is twenty-four channels. Planar, that is twenty-four planes,
  // and a `mediadecode` audio frame has eight slots. As a source no
  // frame could ever arrive; as a target `swr` would produce one this
  // crate cannot hand back — and only after consuming the input, so the
  // failure would land on a session the caller cannot retry. Both are
  // refused where nothing has happened yet.
  let planar_22_2 = ResampleSpec::new(48_000, Sample::F32(Type::Planar), ChannelLayout::_22POINT2);
  assert_eq!(planar_22_2.channels(), 24, "22.2 really is 24 channels");

  match FfmpegResampler::new(planar_22_2, mono_16k()) {
    Err(ResampleError::TooManyPlanes {
      end,
      channels,
      limit,
    }) => {
      assert_eq!(end.to_string(), "source");
      assert_eq!((channels, limit), (24, 8));
    }
    Err(other) => panic!("expected TooManyPlanes for the source, got {other:?}"),
    Ok(_) => panic!("a source no frame can represent must not open"),
  }

  let stereo = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  assert!(
    matches!(
      FfmpegResampler::new(stereo, planar_22_2).map(|_| ()),
      Err(ResampleError::TooManyPlanes {
        end: mediadecode_ffmpeg::SpecEnd::Target,
        channels: 24,
        ..
      }),
    ),
    "the target end is the one that used to fail mid-stream",
  );

  // Packed 22.2 is one plane and stays welcome, and planar right up to
  // the limit does too: the refusal is about plane slots, not channels.
  FfmpegResampler::new(
    ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::_22POINT2),
    mono_16k(),
  )
  .expect("packed 22.2 is one plane");
  FfmpegResampler::new(
    ResampleSpec::new(48_000, Sample::F32(Type::Planar), ChannelLayout::_7POINT1),
    mono_16k(),
  )
  .expect("eight planar channels is exactly the limit");
}

#[test]
fn a_forged_frame_geometry_is_refused_before_it_can_allocate() {
  support::init_ffmpeg();
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  let mut resampler = FfmpegResampler::new(source, mono_16k()).expect("open resampler");

  // A header claiming more samples than a `c_int` can hold, over a
  // plane holding sixteen bytes. Sizing the staging allocation off the
  // header first does not merely waste memory: `av_frame_get_buffer`
  // refuses the truncated count, the failure is not checked, and the
  // unbacked frame goes on to `swr_convert_frame` — with a negative
  // sample count. The geometry has to be settled before any of that.
  let forged = stereo_frame(u32::MAX, 16, None);
  match resampler.send_frame(&forged) {
    Err(ResampleError::PlaneCount { expected, found }) => {
      assert_eq!(found, 16, "the plane's real length is what was compared");
      assert!(expected > found);
    }
    other => panic!("expected a geometry refusal, got {other:?}"),
  }

  // A packed frame with no planes at all.
  let planeless = mediadecode_ffmpeg::AudioFrame::new(
    48_000,
    128,
    2,
    mediadecode_ffmpeg::SampleFormat::S16,
    mediadecode_ffmpeg::channel_layout_description_from_ffmpeg(&ChannelLayout::STEREO),
    std::array::from_fn(|_| {
      mediadecode::frame::Plane::new(mediadecode_ffmpeg::FfmpegBuffer::empty(), 0)
    }),
    0,
    Default::default(),
  );
  assert!(matches!(
    resampler.send_frame(&planeless),
    Err(ResampleError::PlaneCount {
      expected: 1,
      found: 0
    }),
  ));

  // And an honest frame still goes through afterwards: the refusals
  // above left nothing broken behind them.
  resampler
    .send_frame(&stereo_frame(480, 480 * 2 * 2, Some(0)))
    .expect("an honest frame");
}

#[test]
fn a_refused_frame_does_not_stamp_the_next_good_one() {
  support::init_ffmpeg();
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  let mut resampler = FfmpegResampler::new(source, mono_16k()).expect("open resampler");

  // The first frame the session ever sees is a forged one carrying a
  // timestamp far down the timeline. Anchoring before staging took its
  // word for where the stream starts.
  let forged = stereo_frame(4_800, 16, Some(48_000 * 60));
  assert!(matches!(
    resampler.send_frame(&forged),
    Err(ResampleError::PlaneCount { .. }),
  ));

  // Then the real first frame, at zero.
  resampler
    .send_frame(&stereo_frame(4_800, 4_800 * 2 * 2, Some(0)))
    .expect("an honest frame");

  let mut out = empty_audio_frame();
  resampler.receive_frame(&mut out).expect("converted output");
  assert_eq!(
    out.pts().expect("stamped").pts(),
    0,
    "the rejected frame's timestamp must not have anchored the timeline",
  );
}

#[test]
fn a_timestamp_that_cannot_be_rescaled_is_refused_before_anything_moves() {
  support::init_ffmpeg();
  // 48 kHz in, 192 kHz out: the anchor is multiplied by four on its way
  // to the output timeline, so a timestamp near either end of `i64`
  // leaves it. Saturating hid both: the positive end reached the
  // counted timeline only after `swr` had eaten the input, and the
  // negative end landed on `i64::MIN`, which is `AV_NOPTS_VALUE` — the
  // timestamp was not clamped, it was erased.
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  let target = ResampleSpec::new(192_000, Sample::I16(Type::Packed), ChannelLayout::MONO);
  let mut resampler = FfmpegResampler::new(source, target).expect("open resampler");

  let samples = 480;
  let bytes = samples as usize * 2 * 2;
  for pts in [i64::MAX - 1, i64::MIN + 1, i64::MIN] {
    match resampler.send_frame(&stereo_frame(samples, bytes, Some(pts))) {
      Err(ResampleError::TimestampOutOfRange { pts: reported }) => {
        assert_eq!(reported, pts, "the refusal names the timestamp it read");
      }
      other => panic!("expected TimestampOutOfRange for {pts}, got {other:?}"),
    }
    // Nothing was staged, nothing was converted, nothing is owed: the
    // refusal happened before `swr` saw a sample.
    assert_eq!(
      resampler.delay(),
      0,
      "a refused timestamp left input inside the filter",
    );
    let mut dst = empty_audio_frame();
    assert!(
      resampler
        .receive_frame(&mut dst)
        .expect_err("nothing was converted")
        .is_again(),
    );
  }

  // And the anchor never moved: the first frame the session accepts is
  // still the one that fixes where the stream starts.
  resampler
    .send_frame(&stereo_frame(samples, bytes, Some(0)))
    .expect("an honest frame");
  let mut out = empty_audio_frame();
  resampler.receive_frame(&mut out).expect("converted output");
  assert_eq!(
    out.pts().expect("stamped").pts(),
    0,
    "a refused timestamp anchored the timeline anyway",
  );
}

#[test]
fn the_output_timeline_refuses_to_overflow() {
  support::init_ffmpeg();
  // Equal rates, so the input timestamp reaches the output timeline
  // unrescaled and the arithmetic under test is the only thing that
  // can move it.
  let source = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::STEREO);
  let target = ResampleSpec::new(48_000, Sample::I16(Type::Packed), ChannelLayout::MONO);
  let mut resampler = FfmpegResampler::new(source, target).expect("open resampler");

  let samples = 4_800;
  let frame = stereo_frame(samples, samples as usize * 2 * 2, Some(i64::MAX - 8));
  match resampler.send_frame(&frame) {
    Err(ResampleError::TimestampOverflow { pts, samples }) => {
      assert_eq!(pts, i64::MAX - 8);
      assert!(samples > 8, "the count that would not fit");
    }
    other => panic!("expected TimestampOverflow, got {other:?}"),
  }
}

#[test]
fn flush_leaves_nothing_of_the_previous_stream_behind() {
  support::init_ffmpeg();
  // A fractional ratio, so the filter really holds a tail between
  // calls: 44100 -> 16000 is 441:160.
  let source = ResampleSpec::new(44_100, Sample::I16(Type::Packed), ChannelLayout::MONO);
  let mut resampler = FfmpegResampler::new(source, mono_16k()).expect("open resampler");

  let mut out = empty_audio_frame();
  for index in 0..4 {
    let pts = index * 4_410;
    resampler
      .send_frame(&mono_frame(44_100, 4_410, 20_000, Some(pts)))
      .expect("send_frame");
    while resampler.receive_frame(&mut out).is_ok() {}
  }
  assert!(
    resampler.delay() > 0,
    "the filter has to be holding something for the reset to matter",
  );

  resampler.flush().expect("flush");
  assert_eq!(
    resampler.delay(),
    0,
    "a flush that reports success cannot leave the old delay line inside swr",
  );

  // A second stream, silent, starting at zero. Anything the first one
  // left behind arrives here as the loud DC it was.
  let mut loudest = 0i16;
  let mut first_pts = None;
  for index in 0..4 {
    resampler
      .send_frame(&mono_frame(44_100, 4_410, 0, Some(index * 4_410)))
      .expect("send_frame");
    while resampler.receive_frame(&mut out).is_ok() {
      first_pts.get_or_insert(out.pts().expect("stamped").pts());
      let valid = out.nb_samples() as usize * 2;
      for chunk in out.planes()[0].data_ref().as_ref()[..valid]
        .as_chunks::<2>()
        .0
      {
        loudest = loudest.max(i16::from_le_bytes(*chunk).abs());
      }
    }
  }
  assert_eq!(first_pts, Some(0), "the new stream owns the new timeline");
  assert!(
    loudest < 100,
    "silence came out at amplitude {loudest}: the previous stream's tail survived the flush",
  );
}

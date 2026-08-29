//! The demux tier's contracts, pinned against real containers.
//!
//! Every lane here reads a file. The media is generated at run time —
//! see `support/mod.rs` for why the committed corpus cannot serve these
//! shapes — and each lane returns early with a printed reason when the
//! `ffmpeg` CLI that generates it is absent.
//!
//! What is pinned:
//!
//! - the five track kinds map to the five delivery arms, cover art
//!   landing on `Attachment` rather than `Video`;
//! - packets arrive in interleaved file order, compared against a bare
//!   `av_read_frame` loop over the same file rather than a hand-written
//!   expectation;
//! - an attachment track delivers exactly one packet, before any timed
//!   packet, whether its payload is synthesized from codec extradata (a
//!   font) or hoisted out of `AVStream.attached_pic` (cover art) — and
//!   in the cover-art case the duplicate the container *also* emits is
//!   dropped;
//! - a seek lands on a keyframe at or before the target, and replays no
//!   attachment;
//! - `None` means EOF and stays meaning it;
//! - timestamps carry their track's timebase, not a placeholder.

mod support;

use std::{
  fs::File,
  io::{Read, Seek},
};

use mediadecode::{
  Received, Timebase, Timestamp,
  demuxer::{DemuxedPacket, Demuxer, TrackKind},
  packet::PacketFlags,
};
// The owned family under the names this suite was written with — the
// bare aliases mean the view lane now. Import block only; the
// assertions below are unchanged.
use mediadecode_ffmpeg::{DemuxError, FfmpegOwnedDemuxer as FfmpegDemuxer};
use support::Corpus;

/// Drains a session, returning `(track, kind, pts)` for every delivered
/// packet.
fn drain(demuxer: &mut FfmpegDemuxer) -> Vec<(usize, TrackKind, Option<Timestamp>)> {
  let mut out = Vec::new();
  while let Some(packet) = demuxer.next_packet().expect("pull") {
    let pts = match &packet {
      DemuxedPacket::Video(p) => p.packet().pts(),
      DemuxedPacket::Audio(p) => p.packet().pts(),
      DemuxedPacket::Subtitle(p) => p.packet().pts(),
      DemuxedPacket::Data(p) => p.packet().pts(),
      DemuxedPacket::Attachment(_) => None,
    };
    out.push((packet.track().get(), packet.kind(), pts));
  }
  out
}

#[test]
fn the_five_kinds_map_to_the_five_arms() {
  let Some(corpus) = Corpus::new() else { return };

  let multi = FfmpegDemuxer::open(&corpus.multi_track_mkv()).expect("open mkv");
  let kinds: Vec<_> = multi.tracks().iter().map(|t| t.kind()).collect();
  assert_eq!(
    kinds,
    vec![
      TrackKind::Video,
      TrackKind::Audio,
      TrackKind::Subtitle,
      TrackKind::Attachment,
    ],
    "the Matroska file's four tracks",
  );

  // The normalization that matters: a still image in a video-shaped
  // slot is an attachment. If this ever reads `Video`, a thumbnailer
  // downstream starts treating a single JPEG as a motion track.
  let cover = FfmpegDemuxer::open(&corpus.cover_art_mp3()).expect("open mp3");
  let kinds: Vec<_> = cover.tracks().iter().map(|t| t.kind()).collect();
  assert_eq!(kinds, vec![TrackKind::Audio, TrackKind::Attachment]);

  let mov = FfmpegDemuxer::open(&corpus.timecode_mov()).expect("open mov");
  let kinds: Vec<_> = mov.tracks().iter().map(|t| t.kind()).collect();
  assert_eq!(
    kinds,
    vec![TrackKind::Video, TrackKind::Audio, TrackKind::Data],
    "-timecode adds a tmcd data track",
  );
}

#[test]
fn a_track_row_carries_its_codec_parameters_and_attachment_identity() {
  let Some(corpus) = Corpus::new() else { return };
  let mut demuxer = FfmpegDemuxer::open(&corpus.multi_track_mkv()).expect("open mkv");

  let video = &demuxer.tracks()[0];
  match video.params() {
    mediadecode::demuxer::TrackParams::Video(p) => {
      assert_eq!((p.width(), p.height()), (160, 120));
    }
    other => panic!("expected video params, got {:?}", other.kind()),
  }

  let audio = &demuxer.tracks()[1];
  match audio.params() {
    mediadecode::demuxer::TrackParams::Audio(p) => {
      assert_eq!(p.sample_rate(), 48_000);
      assert_eq!(p.channel_count(), 2);
      assert_eq!(p.channel_layout().channels(), 2);
    }
    other => panic!("expected audio params, got {:?}", other.kind()),
  }

  // Identity lives on the row, not on the packet.
  let font = &demuxer.tracks()[3];
  assert_eq!(font.filename().map(|s| s.as_str()), Some("font.ttf"));
  assert_eq!(
    font.mime_type().map(|s| s.as_str()),
    Some("application/x-truetype-font"),
  );

  // The ticket seat is what opens a decoder for the track. Its medium
  // is read as the raw `AVMediaType` it is on the wire — no bindgen
  // enum is materialised out of a value a container controls.
  assert_eq!(
    audio.extra().ticket().codec_type(),
    ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO as i32,
  );
  assert_eq!(audio.extra().stream_index(), 1);

  // The owned-tracks door: the first call moves every row out, and
  // `tracks()` answers empty afterward — the shape a fan-out consumer
  // relies on to wrap each row in `Arc` once, right after open.
  let expected_kinds: Vec<_> = demuxer.tracks().iter().map(|t| t.kind()).collect();
  let taken = demuxer.take_tracks();
  assert_eq!(
    taken.iter().map(|t| t.kind()).collect::<Vec<_>>(),
    expected_kinds,
    "take_tracks hands out every row, in table order",
  );
  assert!(
    demuxer.tracks().is_empty(),
    "the table is empty after the first take",
  );
}

#[test]
fn packets_arrive_in_interleaved_file_order() {
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.multi_track_mkv();

  let expected = support::raw_packet_order(&path);
  let mut demuxer = FfmpegDemuxer::open(&path).expect("open mkv");
  let delivered = drain(&mut demuxer);

  // The Matroska attachment produces no packet of its own, so the one
  // this layer synthesizes is the only difference from the raw order —
  // and it comes first.
  let (head, timed) = delivered.split_first().expect("at least one packet");
  assert_eq!(head.1, TrackKind::Attachment);

  let observed: Vec<(usize, Option<i64>)> = timed
    .iter()
    .map(|(track, _, pts)| (*track, pts.map(|t| t.pts())))
    .collect();
  assert_eq!(
    observed, expected,
    "the delivered order is the container's own order, packet for packet",
  );
}

#[test]
fn an_attachment_is_delivered_exactly_once_and_before_any_timed_packet() {
  let Some(corpus) = Corpus::new() else { return };

  // A font: no packet exists in the stream at all; the payload is
  // synthesized out of codec extradata.
  let mut demuxer = FfmpegDemuxer::open(&corpus.multi_track_mkv()).expect("open mkv");
  let first = demuxer.next_packet().expect("pull").expect("a packet");
  match first {
    DemuxedPacket::Attachment(p) => {
      assert_eq!(p.track().get(), 3);
      assert!(
        p.packet().extra().synthesized(),
        "a font's bytes never appear in the packet stream",
      );
      assert_eq!(p.packet().data().as_ref(), support::FONT_PAYLOAD);
    }
    other => panic!(
      "the attachment must precede every timed packet, got {:?}",
      other.kind()
    ),
  }
  let rest = drain(&mut demuxer);
  assert_eq!(
    rest
      .iter()
      .filter(|(_, k, _)| *k == TrackKind::Attachment)
      .count(),
    0,
    "exactly one, and it was the first",
  );

  // Cover art: the container *does* store a packet for it, and MP3
  // emits that packet in the stream as well. Exactly one must come out.
  let mut demuxer = FfmpegDemuxer::open(&corpus.cover_art_mp3()).expect("open mp3");
  let first = demuxer.next_packet().expect("pull").expect("a packet");
  let cover_len = match first {
    DemuxedPacket::Attachment(p) => {
      assert_eq!(p.track().get(), 1);
      assert!(
        !p.packet().extra().synthesized(),
        "cover art is hoisted from AVStream.attached_pic, not synthesized",
      );
      assert!(!p.packet().data().as_ref().is_empty());
      p.packet().data().as_ref().len()
    }
    other => panic!("expected the cover first, got {:?}", other.kind()),
  };
  assert!(cover_len > 8, "a PNG payload, not a marker");
  let rest = drain(&mut demuxer);
  assert_eq!(
    rest
      .iter()
      .filter(|(_, k, _)| *k == TrackKind::Attachment)
      .count(),
    0,
    "the duplicate the MP3 demuxer emits is dropped",
  );
  assert!(
    rest.iter().all(|(_, k, _)| *k == TrackKind::Audio),
    "everything after the cover is audio",
  );
}

#[test]
fn every_attachment_track_is_delivered_before_the_first_timed_packet() {
  let Some(corpus) = Corpus::new() else { return };

  // The contract, counted rather than sampled: one packet per
  // attachment track, all of them ahead of every timed packet, on every
  // shape the corpus can make. Nothing is left owed at EOF, because
  // nothing about the delivery depends on a packet arriving.
  for path in [
    corpus.multi_track_mkv(),
    corpus.cover_art_mp3(),
    corpus.timecode_mov(),
  ] {
    let mut demuxer = FfmpegDemuxer::open(&path).expect("open");
    let expected = demuxer
      .tracks()
      .iter()
      .filter(|t| t.kind() == TrackKind::Attachment)
      .count();
    let delivered = drain(&mut demuxer);

    let attachments = delivered
      .iter()
      .take_while(|(_, kind, _)| *kind == TrackKind::Attachment)
      .count();
    assert_eq!(
      attachments, expected,
      "{path:?}: {expected} attachment tracks, {attachments} packets ahead of the timeline",
    );
    assert_eq!(
      delivered
        .iter()
        .filter(|(_, kind, _)| *kind == TrackKind::Attachment)
        .count(),
      expected,
      "{path:?}: and none of them arrived later",
    );
  }
}

#[test]
fn a_packets_side_data_arrives_with_it() {
  let Some(corpus) = Corpus::new() else { return };
  // Measured on this corpus: every generated container carries at
  // least one packet with real side data — `AV_PKT_DATA_SKIP_SAMPLES`
  // (kind 11), the encoder-delay trim an MP3 or AAC stream needs to be
  // cut correctly. The extras have always documented a `side_data`
  // seat; nothing ever filled it, so that trim was dropped at the
  // boundary on every packet of every file.
  const SKIP_SAMPLES: i32 = 11;

  let mut demuxer = FfmpegDemuxer::open(&corpus.cover_art_mp3()).expect("open mp3");
  let mut seen = 0;
  while let Some(packet) = demuxer.next_packet().expect("pull") {
    if let DemuxedPacket::Audio(p) = packet {
      seen += p
        .packet()
        .extra()
        .side_data()
        .iter()
        .filter(|entry| entry.kind() == SKIP_SAMPLES)
        .count();
    }
  }
  assert!(
    seen > 0,
    "no packet arrived carrying the side data the container really holds",
  );
}

/// Decodes every audio packet of `path` through the trait decoder,
/// counting samples. With `strip_side_data`, each packet is rebuilt
/// carrying its body and timestamps but none of its side data — which
/// is exactly what the boundary used to hand the codec.
fn decoded_samples(path: &std::path::Path, strip_side_data: bool) -> u64 {
  use mediadecode::decoder::AudioStreamDecoder;

  let mut demuxer = FfmpegDemuxer::open(path).expect("open");
  let track = demuxer
    .tracks()
    .iter()
    .position(|t| t.kind() == TrackKind::Audio)
    .expect("an audio track");
  let info = &demuxer.tracks()[track];
  let mut decoder = mediadecode_ffmpeg::FfmpegOwnedAudioStreamDecoder::open(
    info
      .extra()
      .clone_parameters()
      .expect("the checked handoff"),
    info.timebase(),
    mediadecode_ffmpeg::DecoderLimits::default(),
  )
  .expect("open decoder");

  let mut frame = mediadecode_ffmpeg::empty_owned_audio_frame();
  let mut total = 0u64;
  while let Some(packet) = demuxer.next_packet().expect("pull") {
    let DemuxedPacket::Audio(p) = packet else {
      continue;
    };
    let packet = p.into_packet();
    let packet = if strip_side_data {
      mediadecode_ffmpeg::OwnedAudioPacket::new(
        packet.data().clone(),
        mediadecode_ffmpeg::extras::AudioPacketExtra::new(packet.extra().stream_index()),
      )
      .with_pts(packet.pts())
      .with_dts(packet.dts())
      .with_duration(packet.duration())
      .with_flags(packet.flags())
    } else {
      packet
    };
    support::accepted(decoder.send_packet(&packet), "send_packet");
    while matches!(
      decoder.receive_frame(&mut frame).expect("receive_frame"),
      Received::Frame
    ) {
      total += u64::from(frame.nb_samples());
    }
  }
  support::accepted(decoder.send_eof(), "eof");
  loop {
    match decoder.receive_frame(&mut frame).expect("receive_frame") {
      Received::Frame => total += u64::from(frame.nb_samples()),
      Received::NeedsInput => panic!("a decoder at EOF asked for input"),
      Received::Ended => break,
    }
  }
  total
}

#[test]
fn side_data_survives_from_the_container_to_the_codec() {
  let Some(corpus) = Corpus::new() else { return };
  // The whole path, end to end: the demuxer captures the packet's side
  // data, the reverse conversion reattaches it, and the codec acts on
  // it. `AV_PKT_DATA_SKIP_SAMPLES` is the encoder-delay trim an MP3
  // carries, and acting on it is *observable* — the decoder returns
  // fewer samples, because the padding LAME added is dropped.
  let path = corpus.cover_art_mp3();
  let carried = decoded_samples(&path, false);
  let stripped = decoded_samples(&path, true);

  assert!(carried > 0 && stripped > 0, "both runs must decode");
  assert!(
    carried < stripped,
    "the trim changed nothing: {carried} samples with side data, {stripped} without — \
     the codec never saw it",
  );
  // And the trimmed length is the *true* one: the corpus generates two
  // seconds of 44.1 kHz tone, so 88 200 samples is the answer the file
  // is owed. Whatever padding LAME chose is what the other run keeps.
  assert_eq!(
    carried, 88_200,
    "the trim is applied but lands wrong ({stripped} untrimmed)",
  );
}

/// Re-runs one of this file's tests in a child process, alone, and
/// asserts the child exited cleanly having really run it.
///
/// `av_max_alloc` is process-global, so a lane that makes every FFmpeg
/// allocation fail cannot share a process with anything else; and a
/// lane whose point is that the process *survives* needs a process that
/// could have died.
fn in_subprocess(test_name: &str, body: impl FnOnce()) {
  const CHILD: &str = "MEDIADECODE_FFMPEG_DEMUX_FAULT_CHILD";
  if std::env::var(CHILD).as_deref() == Ok(test_name) {
    body();
    return;
  }
  let exe = std::env::current_exe().expect("the test binary");
  let output = std::process::Command::new(exe)
    .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
    .env(CHILD, test_name)
    .output()
    .expect("spawning the child");
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert_eq!(
    output.status.code(),
    Some(0),
    "the child running `{test_name}` did not exit cleanly ({:?})\n{stdout}\n{}",
    output.status,
    String::from_utf8_lossy(&output.stderr),
  );
  assert!(
    stdout.contains("1 passed"),
    "the child ran no test — is `{test_name}` still the name?\n{stdout}",
  );
}

#[test]
fn the_demux_to_decoder_handoff_survives_an_allocation_fault() {
  let Some(corpus) = Corpus::new() else { return };
  // The public path, from a real container: a caller takes a track row
  // and asks it for the parameters that open a decoder. That handoff
  // used to be `parameters().clone()` — `ffmpeg_next`'s unchecked
  // clone, which dereferences a failed allocation. Under a capped
  // allocator this lane would then have died; now it is an error with a
  // name, and the process is still here to say so.
  let path = corpus.cover_art_mp3();
  in_subprocess(
    "the_demux_to_decoder_handoff_survives_an_allocation_fault",
    move || {
      let demuxer = FfmpegDemuxer::open(&path).expect("open mp3");
      let track = demuxer
        .tracks()
        .iter()
        .find(|t| t.kind() == TrackKind::Audio)
        .expect("an audio track");

      // SAFETY: `av_max_alloc` stores an atomic; this child runs alone.
      unsafe { ffmpeg_next::ffi::av_max_alloc(1) };
      let refused = track.extra().clone_parameters().map(|_| ());
      unsafe { ffmpeg_next::ffi::av_max_alloc(i32::MAX as usize) };

      assert!(
        matches!(refused, Err(DemuxError::ParametersAlloc(_))),
        "expected a named refusal, got {refused:?}",
      );

      // And the handoff really is the one that opens a decoder.
      let parameters = track.extra().clone_parameters().expect("uncapped");
      mediadecode_ffmpeg::FfmpegOwnedAudioStreamDecoder::open(
        parameters,
        track.timebase(),
        mediadecode_ffmpeg::DecoderLimits::default(),
      )
      .expect("the handoff opens a decoder");
    },
  );
}

#[test]
fn a_hoisted_cover_art_packet_keeps_its_flags() {
  let Some(corpus) = Corpus::new() else { return };
  // The hoisted attachment is built by hand from `AVStream.attached_pic`
  // rather than through the boundary conversion, and it used to be
  // built with no flags at all. FFmpeg marks an attached picture
  // `AV_PKT_FLAG_KEY` — a still image is a keyframe if anything is —
  // so "no flags" was visibly wrong for every cover art this crate has
  // ever delivered.
  let mut demuxer = FfmpegDemuxer::open(&corpus.cover_art_mp3()).expect("open mp3");
  let first = demuxer.next_packet().expect("pull").expect("a packet");
  let DemuxedPacket::Attachment(p) = first else {
    panic!("the cover comes first");
  };
  assert!(
    !p.packet().extra().synthesized(),
    "this is the hoisted packet, not one this layer invented",
  );
  assert!(
    p.packet().flags().contains(PacketFlags::KEY),
    "the hoisted packet lost the flags it really carried: {:?}",
    p.packet().flags(),
  );

  // The synthesized one is a different case and says so: nothing was
  // parked, so there are no flags to carry.
  let mut demuxer = FfmpegDemuxer::open(&corpus.multi_track_mkv()).expect("open mkv");
  let first = demuxer.next_packet().expect("pull").expect("a packet");
  let DemuxedPacket::Attachment(p) = first else {
    panic!("the font comes first");
  };
  assert!(p.packet().extra().synthesized());
  assert_eq!(p.packet().flags(), PacketFlags::empty());
}

#[test]
fn the_data_arm_delivers_the_timecode_track() {
  let Some(corpus) = Corpus::new() else { return };
  let mut demuxer = FfmpegDemuxer::open(&corpus.timecode_mov()).expect("open mov");
  let delivered = drain(&mut demuxer);

  let data: Vec<_> = delivered
    .iter()
    .filter(|(_, kind, _)| *kind == TrackKind::Data)
    .collect();
  assert_eq!(data.len(), 1, "a tmcd track carries one sample");
  assert_eq!(data[0].0, 2, "on the third track");
}

#[test]
fn timestamps_carry_their_own_track_timebase() {
  let Some(corpus) = Corpus::new() else { return };
  let mut demuxer = FfmpegDemuxer::open(&corpus.multi_track_mkv()).expect("open mkv");
  let expected: Vec<Timebase> = demuxer.tracks().iter().map(|t| t.timebase()).collect();

  let mut seen = [false; 4];
  while let Some(packet) = demuxer.next_packet().expect("pull") {
    let track = packet.track().get();
    let pts = match &packet {
      DemuxedPacket::Video(p) => p.packet().pts(),
      DemuxedPacket::Audio(p) => p.packet().pts(),
      DemuxedPacket::Subtitle(p) => p.packet().pts(),
      DemuxedPacket::Data(p) => p.packet().pts(),
      DemuxedPacket::Attachment(_) => continue,
    };
    if let Some(pts) = pts {
      assert_eq!(
        pts.timebase(),
        expected[track],
        "a timestamp whose timebase is a placeholder is not a timestamp",
      );
      seen[track] = true;
    }
  }
  assert!(seen[0] && seen[1] && seen[2], "all three timed tracks seen");
}

#[test]
fn a_seek_lands_on_a_keyframe_at_or_before_the_target() {
  let Some(corpus) = Corpus::new() else { return };
  let mut demuxer = FfmpegDemuxer::open(&corpus.multi_track_mkv()).expect("open mkv");
  let video_tb = demuxer.tracks()[0].timebase();

  // One second into a two-second clip whose keyframe interval is 25
  // frames at 25 fps: the landing point is the keyframe at 0 s or the
  // one at 1 s, and either way not a frame after the target.
  let target = Timestamp::new(1, Timebase::SECONDS);
  demuxer.seek(target).expect("seek");

  let mut first_video = None;
  while let Some(packet) = demuxer.next_packet().expect("pull") {
    if let DemuxedPacket::Video(p) = packet {
      first_video = Some(p.into_packet());
      break;
    }
  }
  let packet = first_video.expect("a video packet after the seek");
  let pts = packet.pts().expect("a timestamp");
  assert!(
    packet.flags().contains(PacketFlags::KEY),
    "the landing point must be a keyframe, or the decoder has no reference",
  );
  assert!(
    pts.pts() <= target.rescale_to(video_tb).pts(),
    "landed at {} in {video_tb:?}, past the target",
    pts.pts(),
  );
}

#[test]
fn attachments_are_not_replayed_after_a_seek() {
  let Some(corpus) = Corpus::new() else { return };

  for path in [corpus.multi_track_mkv(), corpus.cover_art_mp3()] {
    let mut demuxer = FfmpegDemuxer::open(&path).expect("open");
    let first = demuxer.next_packet().expect("pull").expect("a packet");
    assert_eq!(first.kind(), TrackKind::Attachment);

    // Three seeks, including one back to the very start — the position
    // where a naive implementation would re-synthesize the payload.
    for secs in [1, 0, 1] {
      demuxer
        .seek(Timestamp::new(secs, Timebase::SECONDS))
        .expect("seek");
      let after = drain(&mut demuxer);
      assert_eq!(
        after
          .iter()
          .filter(|(_, k, _)| *k == TrackKind::Attachment)
          .count(),
        0,
        "{path:?}: a seek moves the timeline, and attachments are not on it",
      );
    }
  }
}

#[test]
fn an_attachment_owed_at_seek_time_is_still_owed_after_it() {
  let Some(corpus) = Corpus::new() else { return };
  // Seeking before the queue has drained must not silently swallow the
  // payload: "not replayed" means never delivered twice, not never
  // delivered.
  let mut demuxer = FfmpegDemuxer::open(&corpus.multi_track_mkv()).expect("open mkv");
  demuxer
    .seek(Timestamp::new(1, Timebase::SECONDS))
    .expect("seek");
  let delivered = drain(&mut demuxer);
  assert_eq!(
    delivered
      .iter()
      .filter(|(_, k, _)| *k == TrackKind::Attachment)
      .count(),
    1,
    "exactly one is still exactly one when the seek comes first",
  );
  assert_eq!(delivered[0].1, TrackKind::Attachment, "and still first");
}

#[test]
fn none_means_eof_and_keeps_meaning_it() {
  let Some(corpus) = Corpus::new() else { return };
  let mut demuxer = FfmpegDemuxer::open(&corpus.timecode_mov()).expect("open mov");
  let count = drain(&mut demuxer).len();
  assert!(count > 0);
  assert!(demuxer.next_packet().expect("pull").is_none());
  assert!(demuxer.next_packet().expect("pull").is_none());

  // And a seek after EOF resumes: the latch this layer set is the one
  // it clears.
  demuxer
    .seek(Timestamp::new(0, Timebase::SECONDS))
    .expect("seek after EOF");
  assert!(
    !drain(&mut demuxer).is_empty(),
    "an EOF latch left in place would make every later read fail",
  );
}

/// A byte source that serves `path` faithfully for `budget` bytes and
/// then panics — the shape that used to abort the process inside
/// libavformat's `extern "C"` read callback.
struct PanicAfter {
  file: File,
  budget: u64,
  served: u64,
}

impl PanicAfter {
  fn new(path: &std::path::Path, budget: u64) -> Self {
    Self {
      file: File::open(path).expect("open file"),
      budget,
      served: 0,
    }
  }
}

impl Read for PanicAfter {
  fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
    assert!(
      self.served < self.budget,
      "the reader is out of patience at byte {}",
      self.served,
    );
    let n = self.file.read(buf)?;
    self.served += n as u64;
    Ok(n)
  }
}

impl Seek for PanicAfter {
  fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
    self.file.seek(pos)
  }
}

/// Counts the bytes a reader is asked for, so the lane below can put
/// its panic *past* whatever opening the container consumes.
struct Counting {
  file: File,
  served: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Read for Counting {
  fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
    let n = self.file.read(buf)?;
    self
      .served
      .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
    Ok(n)
  }
}

impl Seek for Counting {
  fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
    self.file.seek(pos)
  }
}

#[test]
fn a_panicking_reader_is_an_error_not_an_abort() {
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.multi_track_mkv();

  // Deliberate panics print through the default hook; the noise below
  // is the test working, not the test failing.

  // 1. The panic lands during `avformat_open_input` — the very first
  //    read. Without the guard this call terminates the process.
  let Err(err) = FfmpegDemuxer::open_reader(PanicAfter::new(&path, 0), Some("multi.mkv")) else {
    panic!("a panicking reader cannot open a container");
  };
  match err {
    DemuxError::ReaderPanic(ref p) => {
      let message = p.message();
      assert!(
        message.contains("out of patience"),
        "the panic's own words are carried: {message}",
      );
    }
    other => panic!("expected ReaderPanic, got {other:?}"),
  }

  // 2. The panic lands mid-demux, after the session is open. How many
  //    bytes opening consumes is libavformat's business, so it is
  //    measured rather than guessed.
  let served = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
  let counting = Counting {
    file: File::open(&path).expect("open file"),
    served: std::sync::Arc::clone(&served),
  };
  let opened = FfmpegDemuxer::open_reader(counting, Some("multi.mkv")).expect("open");
  let at_open = served.load(std::sync::atomic::Ordering::Relaxed);
  drop(opened);

  let mut demuxer =
    FfmpegDemuxer::open_reader(PanicAfter::new(&path, at_open + 1), Some("multi.mkv"))
      .expect("opening reads fewer bytes than the budget");
  let mut failure = None;
  loop {
    match demuxer.next_packet() {
      Ok(Some(_)) => continue,
      Ok(None) => break,
      Err(e) => {
        failure = Some(e);
        break;
      }
    }
  }
  let failure = failure.expect("the pull loop must fail, not end");
  assert!(
    matches!(failure, DemuxError::ReaderPanic(_)),
    "a panic mid-demux is named, not mistaken for EOF: {failure:?}",
  );

  // 3. And the session stays terminal: the same cause, every time.
  assert!(matches!(
    demuxer.next_packet(),
    Err(DemuxError::ReaderPanic(_))
  ));
  assert!(matches!(
    demuxer.seek(Timestamp::new(0, Timebase::SECONDS)),
    Err(DemuxError::ReaderPanic(_))
  ));
}

/// A byte source that reads faithfully and panics on `seek`, but only
/// once the test arms it — so the panic lands after the session is
/// open, with its attachment still queued.
struct PanicOnArmedSeek {
  file: File,
  armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Read for PanicOnArmedSeek {
  fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
    self.file.read(buf)
  }
}

impl Seek for PanicOnArmedSeek {
  fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
    assert!(
      !self.armed.load(std::sync::atomic::Ordering::Relaxed),
      "the reader gave up on seeking",
    );
    self.file.seek(pos)
  }
}

#[test]
fn a_latched_panic_outranks_a_queued_attachment() {
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.multi_track_mkv();

  let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
  let reader = PanicOnArmedSeek {
    file: File::open(&path).expect("open file"),
    armed: std::sync::Arc::clone(&armed),
  };
  let mut demuxer = FfmpegDemuxer::open_reader(reader, Some("multi.mkv")).expect("open");
  // Nothing has been pulled: the font attachment is still in the queue.
  armed.store(true, std::sync::atomic::Ordering::Relaxed);

  assert!(
    matches!(
      demuxer.seek(Timestamp::new(1, Timebase::SECONDS)),
      Err(DemuxError::ReaderPanic(_))
    ),
    "the seek must report the panic it caused",
  );

  // The queue is filled at open and owes nothing to the reader — which
  // is exactly why draining it here would tell the caller the session
  // is still alive after it has been told otherwise.
  match demuxer.next_packet() {
    Err(DemuxError::ReaderPanic(_)) => {}
    Err(other) => panic!("expected ReaderPanic, got {other:?}"),
    Ok(Some(packet)) => panic!("a terminal session delivered a {:?} packet", packet.kind()),
    Ok(None) => panic!("a terminal session answered EOF"),
  }
}

#[test]
fn a_reader_opens_the_same_container_as_a_path() {
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.multi_track_mkv();

  let from_path = FfmpegDemuxer::open(&path).expect("open by path");
  let expected: Vec<_> = from_path.tracks().iter().map(|t| t.kind()).collect();
  drop(from_path);

  let file = File::open(&path).expect("open file");
  let mut from_reader =
    FfmpegDemuxer::open_reader(file, Some("multi.mkv")).expect("open by reader");
  let observed: Vec<_> = from_reader.tracks().iter().map(|t| t.kind()).collect();
  assert_eq!(observed, expected, "custom AVIO sees the same track table");

  // And it demuxes, not just probes.
  let delivered = drain(&mut from_reader);
  assert!(delivered.len() > 100, "got {} packets", delivered.len());
  assert_eq!(delivered[0].1, TrackKind::Attachment);
}

#[test]
fn the_probe_budget_refuses_an_open_before_libavformat_builds_the_container() {
  use mediadecode_ffmpeg::{DemuxError, DemuxLimits};
  let Some(corpus) = Corpus::new() else { return };

  // **The one seat that reaches behind libavformat.** Every other
  // budget here measures a copy *this crate* makes, and on the demux
  // road that is always after `avformat_open_input` and
  // `avformat_find_stream_info` have already built the attached
  // picture, the extradata and the coded side data out of the file. A
  // parser cannot allocate from bytes it was never handed, so the read
  // is what gets bounded.
  let path = corpus.cover_art_mp3();
  let bytes = std::fs::read(&path).expect("read the fixture");
  assert!(
    bytes.len() > 4096,
    "the fixture must exceed the tight budget"
  );

  // Far too little to probe with: the open is refused by name, and the
  // name says what happened rather than "invalid data" — which is what
  // libavformat folds the reader's I/O error into, and would report a
  // refusal this crate made as a malformed file.
  // 4 KiB: enough for libavformat to accept the `probesize` option —
  // it refuses absurdly small values outright, which is a refusal too,
  // just spelled `EINVAL` — and far less than this file needs, so the
  // meter is the instrument that speaks.
  let starved = DemuxLimits::new().with_max_probe_bytes(4096);
  match FfmpegDemuxer::open_reader_with(std::io::Cursor::new(bytes.clone()), Some("x.mp3"), starved)
  {
    Err(DemuxError::ProbeBudgetExhausted(p)) => {
      assert_eq!(p.budget(), 4096);
      // **Exactly the budget, never more.** The meter used to read the
      // caller's whole buffer and then discover it had overspent — so
      // it consumed bytes libavformat never received and reported
      // having read them. Requests are capped now, so the count is what
      // was actually handed over.
      assert_eq!(
        p.read(),
        p.budget(),
        "the refusal must report bytes libavformat really received",
      );
    }
    Err(other) => panic!("expected ProbeBudgetExhausted, got {other:?}"),
    Ok(_) => panic!("a 1 KiB probe budget opened a whole container"),
  }

  // And the defaults open the same file, so the seat is a seat and not
  // a wall. The budget is released once the container is analysed:
  // reading the media afterwards is the caller's own business, packet
  // by packet, already bounded by the packet seats.
  let mut demuxer = FfmpegDemuxer::open_reader_with(
    std::io::Cursor::new(bytes),
    Some("x.mp3"),
    DemuxLimits::new(),
  )
  .expect("the defaults must open an ordinary file");
  assert!(!demuxer.tracks().is_empty());
  while demuxer
    .next_packet()
    .expect("read past the probe budget")
    .is_some()
  {}
}

#[test]
fn the_path_entrypoint_carries_the_probe_knobs_too() {
  use mediadecode_ffmpeg::DemuxLimits;
  let Some(corpus) = Corpus::new() else { return };
  let path = corpus.cover_art_mp3();

  // The path road cannot carry the hard byte meter — that needs an
  // `AVIOContext` this crate owns, and a path is opened by
  // libavformat's own protocol layer — but `probesize` /
  // `formatprobesize` / `max_streams` reach it through the options
  // dictionary, and that is the residual stated in the accounting.
  //
  // What this lane pins is that the knobs are actually set: a
  // `max_streams` of zero is a value libavformat itself refuses to
  // open under, so reaching it proves the dictionary arrived.
  assert!(
    FfmpegDemuxer::open_with(&path, DemuxLimits::new().with_max_streams(0)).is_err(),
    "the probe options did not reach the path entrypoint",
  );

  // And the defaults still open it.
  assert!(FfmpegDemuxer::open_with(&path, DemuxLimits::new()).is_ok());
}

#[test]
fn the_probe_meter_caps_the_read_rather_than_refusing_the_answer() {
  use mediadecode_ffmpeg::{DemuxError, DemuxLimits};
  let Some(corpus) = Corpus::new() else { return };
  let bytes = std::fs::read(corpus.cover_art_mp3()).expect("read the fixture");

  /// The smallest budget this container opens under, found by bisection
  /// rather than assumed — it is a property of the fixture and of
  /// libavformat's appetite, not a number worth hard-coding.
  fn opens_under(bytes: &[u8], probe: u64) -> bool {
    FfmpegDemuxer::open_reader_with(
      std::io::Cursor::new(bytes.to_vec()),
      Some("x.mp3"),
      DemuxLimits::new().with_max_probe_bytes(probe),
    )
    .is_ok()
  }

  // libavformat refuses `probesize` below its own floor outright, so
  // start the search above it.
  let (mut lo, mut hi) = (8_192u64, 1_048_576u64);
  assert!(opens_under(&bytes, hi), "the fixture must open somewhere");
  while lo + 1 < hi {
    let mid = lo + (hi - lo) / 2;
    if opens_under(&bytes, mid) {
      hi = mid;
    } else {
      lo = mid;
    }
  }
  let exact = hi;

  // **The exact tail.** At precisely the allowance the container needs,
  // the open succeeds: a read landing exactly on zero remaining is
  // served in full and does not trip, because a budget is a ceiling on
  // what is spent rather than on asking.
  //
  // Before the cap this was not reliable — whether a container opened
  // depended on the *shape* of libavformat's requests, since a 32 KiB
  // ask against a 1 KiB allowance consumed everything and returned
  // nothing. A container that fitted its budget could still be refused.
  assert!(
    opens_under(&bytes, exact),
    "a container finishing inside its allowance must open",
  );

  // And one byte short of it, the next read is the one refused — with
  // an accurate count.
  match FfmpegDemuxer::open_reader_with(
    std::io::Cursor::new(bytes.clone()),
    Some("x.mp3"),
    DemuxLimits::new().with_max_probe_bytes(exact - 1),
  ) {
    Err(DemuxError::ProbeBudgetExhausted(p)) => {
      assert_eq!(p.budget(), exact - 1);
      assert_eq!(p.read(), p.budget(), "the count must be what was delivered");
    }
    Err(other) => panic!("expected ProbeBudgetExhausted, got {other:?}"),
    Ok(_) => panic!("the bisection is wrong: {exact} is not the boundary"),
  }
}

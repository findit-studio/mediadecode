//! The FFmpeg muxer over real containers: what it writes, read back by
//! the FFmpeg demuxer.
//!
//! The media is the corpus `mediadecode-ffmpeg`'s lanes generate with the
//! `ffmpeg` CLI, shared by path, and so is the generic remux — written
//! against the `Demuxer` and `Muxer` faces alone — which the
//! `generic_remux` lane there runs over a muxer that records and this
//! lane runs over the FFmpeg muxer.

#[path = "../../mediadecode-ffmpeg/tests/support/mod.rs"]
mod support;

use std::{
  fs::File,
  io::{self, Seek, SeekFrom, Write},
  num::NonZeroI32,
  path::{Path, PathBuf},
  sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
  },
};

use mediadecode::{
  Timebase, Timestamp,
  demuxer::{
    AttachmentTrackPacket, AudioTrackPacket, DemuxedPacket, Demuxer, TrackDisposition, TrackIndex,
    TrackInfo, TrackKind, VideoTrackPacket,
  },
};
use mediadecode_ffmpeg::{CodecId, Ffmpeg, FfmpegDemuxer, FfmpegOwnedDemuxer};
use mediaencode::{
  MuxError, MuxRefusal, Muxer,
  mediaspine::{
    demuxer::{AttachmentPacket, AttachmentTrackParams, Chapter},
    packet::PacketFlags,
    track::{AttachedFile, TrackDescription},
  },
};
use mediaencode_ffmpeg::{
  ContainerName, FfmpegMuxRefusal, FfmpegMuxer, FfmpegOwnedMuxer, MuxOpenError, MuxStage,
  OutputError, OutputText, Uncarried,
};
use mediaio_ffmpeg::{CodecTicket, FfmpegBytes, PacketLimits, extras::AttachmentPacketExtra};
use support::{Corpus, remux};

/// The codec parameters a row carries — the side-data carrier the FFmpeg
/// muxer's table takes.
fn ticket(row: &TrackInfo<Ffmpeg>) -> CodecTicket {
  row.extra().ticket().clone()
}

fn millis() -> Timebase {
  Timebase::new(1, NonZeroI32::new(1000).expect("non-zero"))
}

fn ms(value: i64) -> Timestamp {
  Timestamp::new(value, millis())
}

/// What a row declares, owned, for comparing two files' tables.
#[derive(Debug, PartialEq)]
struct Row {
  kind: TrackKind,
  codec: CodecId,
  language: Option<String>,
  disposition: TrackDisposition,
  filename: Option<String>,
  mime_type: Option<String>,
}

fn rows(path: &Path) -> Vec<Row> {
  let demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  demuxer
    .tracks()
    .iter()
    .map(|row| Row {
      kind: row.kind(),
      codec: row.params().codec(),
      language: row.language().map(|text| text.as_str().to_owned()),
      disposition: row.disposition(),
      filename: row.filename().map(|text| text.as_str().to_owned()),
      mime_type: row.mime_type().map(|text| text.as_str().to_owned()),
    })
    .collect()
}

/// One packet as a file holds it.
#[derive(Debug, PartialEq)]
struct Seen {
  bytes: Vec<u8>,
  pts: Option<Timestamp>,
  dts: Option<Timestamp>,
  /// Compared only where the container records it per packet: a cue's.
  duration: Option<Timestamp>,
  key: bool,
}

/// Every packet of `path`, per track, in the order the demuxer delivers
/// them.
fn packets(path: &Path) -> Vec<Vec<Seen>> {
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  let mut out: Vec<Vec<Seen>> = demuxer.tracks().iter().map(|_| Vec::new()).collect();
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    let track = packet.track().get();
    let seen = match &packet {
      DemuxedPacket::Video(p) => {
        let p = p.packet();
        Seen {
          bytes: p.data().as_ref().to_vec(),
          pts: p.pts(),
          dts: p.dts(),
          duration: None,
          key: p.flags().contains(PacketFlags::KEY),
        }
      }
      DemuxedPacket::Audio(p) => {
        let p = p.packet();
        Seen {
          bytes: p.data().as_ref().to_vec(),
          pts: p.pts(),
          dts: p.dts(),
          duration: None,
          key: p.flags().contains(PacketFlags::KEY),
        }
      }
      DemuxedPacket::Subtitle(p) => {
        let p = p.packet();
        Seen {
          bytes: p.data().as_ref().to_vec(),
          pts: p.pts(),
          dts: p.pts(),
          duration: p.duration(),
          key: p.flags().contains(PacketFlags::KEY),
        }
      }
      DemuxedPacket::Data(p) => {
        let p = p.packet();
        Seen {
          bytes: p.data().as_ref().to_vec(),
          pts: p.pts(),
          dts: p.pts(),
          duration: p.duration(),
          key: p.flags().contains(PacketFlags::KEY),
        }
      }
      DemuxedPacket::Attachment(p) => Seen {
        bytes: p.packet().data().as_ref().to_vec(),
        pts: None,
        dts: None,
        duration: None,
        key: false,
      },
    };
    out[track].push(seen);
  }
  out
}

fn chapters(path: &Path) -> Vec<Chapter<Ffmpeg>> {
  FfmpegOwnedDemuxer::open(path)
    .expect("open")
    .chapters()
    .to_vec()
}

/// Remuxes `source` into `container` at `out` through the generic remux
/// over the FFmpeg pair, answering what the muxer reported.
fn remux_through_ffmpeg(
  source: &Path,
  container: ContainerName,
  out: &Path,
) -> mediaencode_ffmpeg::MuxReport {
  let mut demuxer = FfmpegDemuxer::open(source).expect("open the source");
  let sink = File::create(out).expect("create the output");
  let mut report = None;
  remux(&mut demuxer, ticket, |table, chapters| {
    let (muxer, opened) =
      FfmpegMuxer::open(sink, container, table, chapters, PacketLimits::default())
        .expect("the muxer opens");
    report = Some(opened);
    muxer
  });
  report.expect("the muxer was opened")
}

fn output(corpus: &Corpus, name: &str) -> PathBuf {
  corpus.dir().join(name)
}

/// **L1 — a Matroska remux into Matroska keeps everything both rows
/// carry.** Through the generic remux over the FFmpeg pair, the copy
/// reads back with the source's table — each track's kind, codec,
/// language and disposition, the font's filename and MIME type — its
/// chapters, and every packet's bytes, timestamps and key flag (and a
/// cue's duration), with nothing reported.
#[test]
fn l1_a_matroska_remux_into_matroska_keeps_what_both_rows_carry() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.remux_source_mkv();
  let copy = output(&corpus, "l1.mkv");
  let report = remux_through_ffmpeg(&source, ContainerName::MATROSKA, &copy);
  assert!(report.is_empty(), "Matroska carries it all: {report:?}");

  let (before, after) = (rows(&source), rows(&copy));
  assert_eq!(after, before, "the table");
  assert!(
    before
      .iter()
      .any(|row| row.language.as_deref() == Some("jpn"))
      && before
        .iter()
        .any(|row| row.disposition.contains(TrackDisposition::FORCED)),
    "the premise: the source declares languages and a forced track",
  );
  assert_eq!(chapters(&copy), chapters(&source), "the chapters");
  assert_eq!(chapters(&source).len(), 2, "the premise: two chapters");

  let (before, after) = (packets(&source), packets(&copy));
  assert_eq!(after.len(), before.len());
  for (track, (after, before)) in after.iter().zip(&before).enumerate() {
    assert!(!before.is_empty(), "the premise: track {track} has packets");
    assert_eq!(after, before, "track {track}'s packets");
  }
  assert_eq!(
    before[3],
    vec![Seen {
      bytes: support::FONT_PAYLOAD.to_vec(),
      pts: None,
      dts: None,
      duration: None,
      key: false,
    }],
    "the font, whole",
  );
}

/// **L2 — into MP4, SubRip is refused by name and the font is
/// reported.** MP4 stores no SubRip, so a table carrying it is refused at
/// open — nothing written — naming the track and the codec. Without it,
/// the muxer opens and reports the font MP4 has no place for; video and
/// audio come back packet for packet, and no attachment does.
#[test]
fn l2_into_mp4_subrip_is_refused_by_name_and_the_font_is_reported() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let target = output(&corpus, "l2.mp4");

  let table = |keep: &dyn Fn(TrackKind) -> bool| {
    let mut demuxer = FfmpegDemuxer::open(&source).expect("open the source");
    let mut files: Vec<Option<_>> = demuxer.tracks().iter().map(|_| None).collect();
    let mut timed = Vec::new();
    while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
      match packet {
        DemuxedPacket::Attachment(attachment) => {
          let (track, payload) = attachment.into_parts();
          files[track.get()] = Some(payload);
        }
        packet => timed.push(packet),
      }
    }
    let mut kept = Vec::new();
    let mut table = Vec::new();
    for (index, (row, file)) in demuxer.tracks().iter().zip(files).enumerate() {
      if keep(row.kind()) {
        kept.push(index);
        table.push(
          TrackDescription::from_track_info(&**row, file, ticket(row)).expect("a description"),
        );
      }
    }
    (table, kept, timed)
  };

  let (everything, _, _) = table(&|_| true);
  let refused = FfmpegMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    everything,
    &[],
    PacketLimits::default(),
  );
  match refused {
    Err(MuxOpenError::CodecNotCarried { track, codec }) => {
      assert_eq!((track.get(), codec), (2, CodecId::SUBRIP));
    }
    other => panic!("expected the SubRip track refused by name, got {other:?}"),
  }
  assert_eq!(
    std::fs::metadata(&target).expect("the file").len(),
    0,
    "nothing was written"
  );

  let (without_cues, kept, timed) = table(&|kind| kind != TrackKind::Subtitle);
  let (mut muxer, report) = FfmpegMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    without_cues,
    &[],
    PacketLimits::default(),
  )
  .expect("the muxer opens without the cues");
  assert!(
    report.uncarried().contains(&Uncarried::Attachment {
      track: TrackIndex::new(2)
    }),
    "the font is reported: {report:?}",
  );
  for packet in timed {
    let Some(index) = kept.iter().position(|&kept| kept == packet.track().get()) else {
      continue;
    };
    let track = TrackIndex::new(index);
    let routed = match packet {
      DemuxedPacket::Video(p) => {
        DemuxedPacket::Video(VideoTrackPacket::new(track, p.into_parts().1))
      }
      DemuxedPacket::Audio(p) => {
        DemuxedPacket::Audio(AudioTrackPacket::new(track, p.into_parts().1))
      }
      other => panic!("only video and audio are kept: {other:?}"),
    };
    muxer
      .write_packet(routed)
      .map_err(|error| error.map_packet(drop))
      .expect("a packet");
  }
  muxer.finish().expect("the container is finished");

  let written = rows(&target);
  assert_eq!(
    written.iter().map(|row| row.kind).collect::<Vec<_>>(),
    vec![TrackKind::Video, TrackKind::Audio],
    "video and audio, and no attachment",
  );
  let (before, after) = (packets(&source), packets(&target));
  for (track, source_track) in [(0, 0), (1, 1)] {
    let bytes = |seen: &[Seen]| seen.iter().map(|p| p.bytes.clone()).collect::<Vec<_>>();
    assert_eq!(
      bytes(&after[track]),
      bytes(&before[source_track]),
      "track {track}'s bytes"
    );
  }
}

/// **L3 — an MP3's cover art reaches MP4 as its cover.** The cover
/// track goes to MP4 as a picture of the cover kind, written once after
/// the header, and reads back as the attachment it was, byte for byte;
/// the audio comes with it.
#[test]
fn l3_an_mp3s_cover_art_reaches_mp4_as_its_cover() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.cover_art_mp3();
  let target = output(&corpus, "l3.mp4");
  let report = remux_through_ffmpeg(&source, ContainerName::MP4, &target);
  assert!(
    !report
      .uncarried()
      .iter()
      .any(|lost| matches!(lost, Uncarried::Attachment { .. })),
    "MP4 carries cover art: {report:?}",
  );

  let (before, after) = (packets(&source), packets(&target));
  let cover = |rows: &[Row], packets: &[Vec<Seen>]| {
    let index = rows
      .iter()
      .position(|row| row.kind == TrackKind::Attachment)
      .expect("a cover track");
    (rows[index].codec, packets[index][0].bytes.clone())
  };
  let (source_rows, target_rows) = (rows(&source), rows(&target));
  let (codec, bytes) = cover(&source_rows, &before);
  assert_eq!(codec, CodecId::PNG, "the premise: a PNG cover");
  assert_eq!(
    cover(&target_rows, &after),
    (codec, bytes),
    "the cover, whole"
  );
  let audio = |rows: &[Row], packets: &[Vec<Seen>]| {
    let index = rows
      .iter()
      .position(|row| row.kind == TrackKind::Audio)
      .expect("an audio track");
    packets[index]
      .iter()
      .map(|p| p.bytes.clone())
      .collect::<Vec<_>>()
  };
  assert_eq!(
    audio(&target_rows, &after),
    audio(&source_rows, &before),
    "the audio"
  );
}

/// The first `count` video packets of `path`, owned, and its first
/// attachment packet.
fn owned_video_and_font(
  path: &Path,
  count: usize,
) -> (
  Vec<VideoTrackPacket<Ffmpeg, FfmpegBytes>>,
  Option<AttachmentTrackPacket<Ffmpeg, FfmpegBytes>>,
  Option<AudioTrackPacket<Ffmpeg, FfmpegBytes>>,
) {
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  let (mut video, mut font, mut audio) = (Vec::new(), None, None);
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    match packet {
      DemuxedPacket::Video(p) if video.len() < count => video.push(p),
      DemuxedPacket::Attachment(p) if font.is_none() => font = Some(p),
      DemuxedPacket::Audio(p) if audio.is_none() => audio = Some(p),
      _ => {}
    }
    if video.len() == count && font.is_some() && audio.is_some() {
      break;
    }
  }
  (video, font, audio)
}

/// A one-track table: `path`'s video track.
fn video_table(path: &Path) -> Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> {
  let demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  let row = demuxer
    .tracks()
    .iter()
    .find(|row| row.kind() == TrackKind::Video)
    .expect("a video track");
  vec![TrackDescription::from_track_info(&**row, None, ticket(row)).expect("a description")]
}

/// `packet` on track `track`, decoded at `dts` and presented at `pts`.
fn restamped(
  packet: &VideoTrackPacket<Ffmpeg, FfmpegBytes>,
  track: usize,
  pts: Option<i64>,
  dts: Option<i64>,
) -> VideoTrackPacket<Ffmpeg, FfmpegBytes> {
  let (_, packet) = packet.clone().into_parts();
  VideoTrackPacket::new(
    TrackIndex::new(track),
    packet.with_pts(pts.map(ms)).with_dts(dts.map(ms)),
  )
}

/// The portable refusal and the packet handed back, or a panic naming
/// what came instead.
fn refused<P: core::fmt::Debug>(
  result: Result<(), MuxError<mediaencode_ffmpeg::MuxFault, FfmpegMuxRefusal, P>>,
) -> (MuxRefusal, P) {
  match result {
    Err(MuxError::Refused { refusal, packet }) => (refusal, packet),
    other => panic!("expected a portable refusal, got {other:?}"),
  }
}

/// **L4 — every refusal hands the packet back.** On an open FFmpeg
/// muxer: a track it does not have, a packet of another kind, a missing
/// decode time, an earlier one, a presentation before its decode and an
/// attachment offered as a packet are refused by the face's names; an
/// equal decode time is refused by MP4's own name and admitted by
/// Matroska. Every refused packet comes back holding the very bytes it
/// was offered with, and nothing refused reaches the file.
#[test]
fn l4_every_refusal_hands_the_packet_back() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (video, font, audio) = owned_video_and_font(&source, 3);
  let (font, audio) = (font.expect("the font"), audio.expect("an audio packet"));
  assert_eq!(video.len(), 3, "the premise: three video packets");

  let target = output(&corpus, "l4.mp4");
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    video_table(&source),
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  muxer
    .write_video(restamped(&video[0], 0, Some(0), Some(0)))
    .expect("the first packet");
  muxer
    .write_video(restamped(&video[1], 0, Some(40), Some(40)))
    .expect("the second packet");

  let check = |offered: &VideoTrackPacket<Ffmpeg, FfmpegBytes>,
               back: &VideoTrackPacket<Ffmpeg, FfmpegBytes>| {
    assert_eq!(back.track(), offered.track());
    assert!(
      back.packet().data().ptr_eq(offered.packet().data()),
      "the very bytes it was offered with",
    );
    assert_eq!(back.packet().pts(), offered.packet().pts());
    assert_eq!(back.packet().dts(), offered.packet().dts());
  };

  let offered = restamped(&video[2], 5, Some(80), Some(80));
  let (refusal, back) = refused(muxer.write_video(offered.clone()));
  assert!(matches!(refusal, MuxRefusal::UnknownTrack { track } if track.get() == 5));
  check(&offered, &back);

  let (_, audio_packet) = audio.into_parts();
  let offered_audio = AudioTrackPacket::new(TrackIndex::new(0), audio_packet);
  let (refusal, back) = refused(muxer.write_audio(offered_audio.clone()));
  assert!(matches!(
    refusal,
    MuxRefusal::KindMismatch {
      expected: TrackKind::Video,
      offered: TrackKind::Audio,
      ..
    }
  ));
  assert!(back.packet().data().ptr_eq(offered_audio.packet().data()));

  let offered = restamped(&video[2], 0, Some(80), None);
  let (refusal, back) = refused(muxer.write_video(offered.clone()));
  assert!(matches!(refusal, MuxRefusal::MissingDts { .. }));
  check(&offered, &back);

  let offered = restamped(&video[2], 0, Some(80), Some(20));
  let (refusal, back) = refused(muxer.write_video(offered.clone()));
  assert!(matches!(refusal, MuxRefusal::NonMonotonicDts { .. }));
  check(&offered, &back);

  let offered = restamped(&video[2], 0, Some(60), Some(80));
  let (refusal, back) = refused(muxer.write_video(offered.clone()));
  assert!(matches!(refusal, MuxRefusal::PtsBeforeDts { .. }));
  check(&offered, &back);

  let offered = restamped(&video[2], 0, Some(80), Some(40));
  match muxer.write_video(offered.clone()) {
    Err(MuxError::Backend {
      refusal: FfmpegMuxRefusal::RepeatedDts { track, dts },
      packet: back,
    }) => {
      assert_eq!((track.get(), dts), (0, ms(40)));
      check(&offered, &back);
    }
    other => panic!("expected MP4's own refusal of a repeated decode time, got {other:?}"),
  }

  let offered_font = DemuxedPacket::Attachment(font.clone());
  let (refusal, back) = refused(muxer.write_packet(offered_font));
  assert!(matches!(refusal, MuxRefusal::Attachment { .. }));
  match back {
    DemuxedPacket::Attachment(back) => {
      assert!(back.packet().data().ptr_eq(font.packet().data()));
    }
    other => panic!("expected the attachment back, got {other:?}"),
  }

  muxer
    .write_video(restamped(&video[2], 0, Some(80), Some(80)))
    .expect("the third packet, advancing");
  muxer.finish().expect("the container is finished");
  assert_eq!(
    packets(&target)[0].len(),
    3,
    "only what was admitted was written"
  );

  // Matroska is not strict: the repeat is written.
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(output(&corpus, "l4.mkv")).expect("create"),
    ContainerName::MATROSKA,
    video_table(&source),
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  muxer
    .write_video(restamped(&video[0], 0, Some(0), Some(0)))
    .expect("the first packet");
  muxer
    .write_video(restamped(&video[1], 0, Some(40), Some(40)))
    .expect("the second packet");
  muxer
    .write_video(restamped(&video[2], 0, Some(80), Some(40)))
    .expect("a repeated decode time, admitted by Matroska");
  muxer.finish().expect("the container is finished");
}

/// **L5 — an open refusal names what the container cannot take, and
/// writes nothing.** An unknown container; Matroska's attachment with no
/// filename, and one whose MIME type nobody knows; a codec WebM cannot
/// store; a chapter that runs backwards; a language holding a NUL.
#[test]
fn l5_an_open_refusal_names_what_the_container_cannot_take() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let target = output(&corpus, "l5.mkv");
  let open = |container: ContainerName,
              table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
              chapters: &[Chapter<Ffmpeg>]| {
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      container,
      table,
      chapters,
      PacketLimits::default(),
    );
    assert_eq!(
      std::fs::metadata(&target).expect("the file").len(),
      0,
      "nothing was written"
    );
    result.map(drop)
  };

  assert!(matches!(
    open(ContainerName::new("no-such-muxer"), video_table(&source), &[]),
    Err(MuxOpenError::UnknownContainer { name }) if name.as_str() == "no-such-muxer"
  ));

  // An attachment with no filename, and one with no MIME type Matroska
  // could name.
  let attachment = |codec: CodecId, filename: Option<&str>, ticket: CodecTicket| {
    let payload = AttachmentPacket::new(
      FfmpegBytes::try_copy_from_slice(b"a file").expect("a payload"),
      AttachmentPacketExtra::new(0),
    );
    TrackDescription::attachment(
      millis(),
      AttachmentTrackParams::new(codec),
      AttachedFile::new(payload).with_filename(filename.map(Into::into)),
      ticket,
    )
  };
  let font_ticket = {
    let demuxer = FfmpegOwnedDemuxer::open(&source).expect("open");
    ticket(
      demuxer
        .tracks()
        .iter()
        .find(|row| row.kind() == TrackKind::Attachment)
        .expect("the font"),
    )
  };
  let none_ticket = CodecTicket::mirror(&ffmpeg_next::codec::Parameters::new(), 0, 1 << 20)
    .expect("an empty ticket");
  assert!(matches!(
    open(
      ContainerName::MATROSKA,
      vec![attachment(CodecId::TTF, None, font_ticket.clone())],
      &[]
    ),
    Err(MuxOpenError::AttachmentWithoutFilename { track }) if track.get() == 0
  ));
  assert!(matches!(
    open(
      ContainerName::MATROSKA,
      vec![attachment(CodecId::NONE, Some("file.bin"), none_ticket)],
      &[]
    ),
    Err(MuxOpenError::AttachmentWithoutMimeType { track }) if track.get() == 0
  ));

  assert!(matches!(
    open(ContainerName::WEBM, video_table(&source), &[]),
    Err(MuxOpenError::CodecNotCarried { track, codec }) if track.get() == 0 && codec == CodecId::H264
  ));

  let backwards = Chapter::<Ffmpeg>::new(1, millis(), ms(2000), ms(1000));
  assert!(matches!(
    open(ContainerName::MATROSKA, video_table(&source), &[backwards]),
    Err(MuxOpenError::ChapterSpan { chapter: 0 })
  ));

  let mut table = video_table(&source);
  table[0].set_language(Some("e\0ng".into()));
  assert!(matches!(
    open(ContainerName::MATROSKA, table, &[]),
    Err(MuxOpenError::TextWithNul {
      track,
      text: OutputText::Language
    }) if track.get() == 0
  ));
}

/// A sink that counts its calls and panics in every one.
#[derive(Default)]
struct Exploding {
  calls: Arc<AtomicU32>,
}

impl Write for Exploding {
  fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
    self.calls.fetch_add(1, Ordering::Relaxed);
    panic!("the sink exploded");
  }
  fn flush(&mut self) -> io::Result<()> {
    self.calls.fetch_add(1, Ordering::Relaxed);
    panic!("the sink exploded");
  }
}

impl Seek for Exploding {
  fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
    self.calls.fetch_add(1, Ordering::Relaxed);
    panic!("the sink exploded");
  }
}

/// **L6 — a sink that panics is named in the fault, and never called
/// again.** libavformat drives the sink from C; the panic inside it is
/// caught at the boundary, the session's fault names it, and no later
/// call reaches the sink — the process does not abort.
#[test]
fn l6_a_sink_that_panics_is_named_in_the_fault() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (video, _, _) = owned_video_and_font(&source, 3);
  let sink = Exploding::default();
  let calls = Arc::clone(&sink.calls);

  let previous = std::panic::take_hook();
  std::panic::set_hook(Box::new(|_| {}));
  let fault = (|| {
    let (mut muxer, _) = match FfmpegOwnedMuxer::open(
      sink,
      ContainerName::MATROSKA,
      video_table(&source),
      &[],
      PacketLimits::default(),
    ) {
      Ok(opened) => opened,
      Err(MuxOpenError::Fault(fault)) => return fault,
      Err(other) => panic!("expected the muxer to open or fault, got {other:?}"),
    };
    for (index, packet) in video.iter().enumerate() {
      let at = 40 * index as i64;
      if let Err(error) = muxer.write_video(restamped(packet, 0, Some(at), Some(at))) {
        match error {
          MuxError::Fault(fault) => return fault,
          other => panic!("expected a fault, got {other:?}"),
        }
      }
    }
    muxer.finish().expect_err("the sink panicked")
  })();
  std::panic::set_hook(previous);

  assert_eq!(
    fault.sink_panic().map(|message| message.as_str()),
    Some("the sink exploded"),
    "the fault names the panic: {fault:?}",
  );
  assert!(matches!(fault.error(), OutputError::SinkPanic { .. }));
  assert!(matches!(
    fault.stage(),
    MuxStage::Header | MuxStage::Write { .. } | MuxStage::Trailer
  ));
  assert_eq!(
    calls.load(Ordering::Relaxed),
    1,
    "only the first call reached the sink"
  );
}

/// **The generic remux runs over the FFmpeg pair, cues at one instant
/// included.** Matroska is not strict and a subtitle stream never is, so
/// the two cues sharing a start time are both written, and read back at
/// that one instant.
#[test]
fn the_generic_remux_writes_cues_that_share_an_instant_through_the_ffmpeg_muxer() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.simultaneous_cues_mkv();
  let copy = output(&corpus, "cues.mkv");
  let report = remux_through_ffmpeg(&source, ContainerName::MATROSKA, &copy);
  assert!(report.is_empty(), "{report:?}");
  let cues = &packets(&copy)[1];
  assert_eq!(cues.len(), 2, "both cues were written");
  assert_eq!(cues[0].pts, cues[1].pts, "at one instant");
  assert_eq!(packets(&copy), packets(&source), "and everything else");
}

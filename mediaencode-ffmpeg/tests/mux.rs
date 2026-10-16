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
    AttachmentTrackPacket, AudioTrackPacket, DemuxedPacket, Demuxer, SubtitleTrackPacket,
    TrackDisposition, TrackIndex, TrackInfo, TrackKind, VideoTrackPacket,
  },
};
use mediadecode_ffmpeg::{CodecId, Ffmpeg, FfmpegDemuxer, FfmpegOwnedDemuxer};
use mediaencode::{
  MuxError, MuxRefusal, Muxer,
  mediaspine::{
    demuxer::{AttachmentPacket, AttachmentTrackParams, Chapter, TrackParams},
    mediatime::Rounding,
    packet::PacketFlags,
    track::{AttachedFile, TrackDescription},
  },
};
use mediaencode_ffmpeg::{
  ContainerName, FfmpegMuxRefusal, FfmpegMuxer, FfmpegOwnedMuxer, MuxOpenError, MuxStage,
  OutputError, OutputText, PayloadRule, Rewrite, Uncarried,
};
use mediaio_ffmpeg::{
  CodecTicket, FfmpegBytes, PacketLimits,
  extras::{AttachmentPacketExtra, SideDataEntry},
};
use smol_bytes::Utf8Bytes;
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

/// Owned packets taken from a file for the refusal laws.
struct Sample {
  video: Vec<VideoTrackPacket<Ffmpeg, FfmpegBytes>>,
  font: Option<AttachmentTrackPacket<Ffmpeg, FfmpegBytes>>,
  audio: Option<AudioTrackPacket<Ffmpeg, FfmpegBytes>>,
}

/// The first `count` video packets of `path`, owned, and its first
/// attachment and audio packets.
fn owned_video_and_font(path: &Path, count: usize) -> Sample {
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  let mut sample = Sample {
    video: Vec::new(),
    font: None,
    audio: None,
  };
  while let Some(packet) = demuxer.next_packet().expect("a packet or the end") {
    match packet {
      DemuxedPacket::Video(p) if sample.video.len() < count => sample.video.push(p),
      DemuxedPacket::Attachment(p) if sample.font.is_none() => sample.font = Some(p),
      DemuxedPacket::Audio(p) if sample.audio.is_none() => sample.audio = Some(p),
      _ => {}
    }
    if sample.video.len() == count && sample.font.is_some() && sample.audio.is_some() {
      break;
    }
  }
  sample
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
  let Sample { video, font, audio } = owned_video_and_font(&source, 3);
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
  let video = owned_video_and_font(&source, 3).video;
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

/// **L7 — a presentation or decode time that lands on `AV_NOPTS_VALUE`
/// is refused by name, the packet untouched.** `i64::MIN` is the count
/// libavformat reads as no timestamp at all. Into Matroska's 1/1000
/// stream a decode time of `i64::MIN` milliseconds counts to it, and so
/// does a presentation time; each is refused as `Unrepresentable`,
/// naming the time offered, and handed back, and only the packet with
/// real times reaches the file.
#[test]
fn l7_a_time_that_lands_on_no_timestamp_is_refused_by_name() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let video = owned_video_and_font(&source, 1).video;
  let target = output(&corpus, "l7.mkv");
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MATROSKA,
    video_table(&source),
    &[],
    PacketLimits::default(),
  )
  .expect("open");

  for (pts, dts, named) in [
    (Some(0), Some(i64::MIN), ms(i64::MIN)),
    (Some(i64::MIN), Some(0), ms(i64::MIN)),
  ] {
    let offered = restamped(&video[0], 0, pts, dts);
    match muxer.write_video(offered.clone()) {
      Err(MuxError::Backend {
        refusal: FfmpegMuxRefusal::Unrepresentable { track, timestamp },
        packet: back,
      }) => {
        assert_eq!(track.get(), 0);
        assert_eq!(
          (timestamp.pts(), timestamp.timebase()),
          (named.pts(), named.timebase()),
          "the time offered is named",
        );
        assert!(back.packet().data().ptr_eq(offered.packet().data()));
        assert_eq!(back.packet().pts(), offered.packet().pts());
        assert_eq!(back.packet().dts(), offered.packet().dts());
      }
      other => panic!("expected the time refused as unrepresentable, got {other:?}"),
    }
  }

  muxer
    .write_video(restamped(&video[0], 0, Some(0), Some(0)))
    .expect("real times are written");
  muxer.finish().expect("the container is finished");
  assert_eq!(
    packets(&target)[0].len(),
    1,
    "only what was admitted was written"
  );
}

/// **L8 — presentation order is judged on the counts libavformat is
/// handed.** Into Matroska's 1/1000 stream, a packet presented at
/// 3/10 000 s and decoded at 4/10 000 s counts to one tick, 0, for both —
/// which libavformat writes, as a stream copy of a file with sub-tick
/// jitter does — so it is admitted. One presented at 30 ms and decoded at
/// 40 ms is an inversion of the counts: refused by the face's name with
/// the times it was offered with, not their counts, and handed back.
#[test]
fn l8_presentation_order_is_judged_on_the_counts_libavformat_is_handed() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let video = owned_video_and_font(&source, 2).video;
  let target = output(&corpus, "l8.mkv");
  let (mut muxer, _) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MATROSKA,
    video_table(&source),
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  let tenth_ms = Timebase::new(1, NonZeroI32::new(10_000).expect("non-zero"));
  let at = |packet: &VideoTrackPacket<Ffmpeg, FfmpegBytes>, pts: i64, dts: i64| {
    let (_, packet) = packet.clone().into_parts();
    VideoTrackPacket::new(
      TrackIndex::new(0),
      packet
        .with_pts(Some(Timestamp::new(pts, tenth_ms)))
        .with_dts(Some(Timestamp::new(dts, tenth_ms))),
    )
  };

  muxer
    .write_video(at(&video[0], 3, 4))
    .expect("presented and decoded within one tick: admitted");

  let offered = at(&video[1], 300, 400);
  let (refusal, back) = refused(muxer.write_video(offered.clone()));
  match refusal {
    MuxRefusal::PtsBeforeDts { track, pts, dts } => {
      assert_eq!(track.get(), 0);
      assert_eq!(
        (pts.pts(), pts.timebase(), dts.pts(), dts.timebase()),
        (300, tenth_ms, 400, tenth_ms),
        "the times the packet carries, not their counts",
      );
    }
    other => panic!("expected the presentation-order refusal, got {other:?}"),
  }
  assert!(back.packet().data().ptr_eq(offered.packet().data()));
  assert_eq!(back.packet().pts(), offered.packet().pts());
  assert_eq!(back.packet().dts(), offered.packet().dts());

  muxer.finish().expect("the container is finished");
  let written = &packets(&target)[0];
  assert_eq!(written.len(), 1, "only the admitted packet was written");
  assert_eq!(written[0].pts, Some(ms(0)), "presented at its one tick");
}

/// What a description offers, as a row: what a read-back row is judged
/// against.
fn offered(description: &TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>) -> Row {
  let text = |text: Option<&Utf8Bytes>| text.map(|text| text.as_str().to_owned());
  let file = description.file();
  Row {
    kind: description.kind(),
    codec: description.codec(),
    language: text(description.language()),
    disposition: description.disposition(),
    filename: text(file.and_then(|file| file.filename())),
    mime_type: text(file.and_then(|file| file.mime_type())),
  }
}

/// What `written` lost of `offered`, as the report names it, in its
/// order: each field the track declared that the file does not hold — a
/// declared text read back otherwise, or not at all, and each declared
/// flag read back unset.
fn lost(track: usize, offered: &Row, written: &Row) -> Vec<Uncarried> {
  let track = TrackIndex::new(track);
  let dropped = |declared: &Option<String>, held: &Option<String>| {
    declared
      .as_ref()
      .filter(|declared| held.as_ref() != Some(*declared))
      .map(|declared| Utf8Bytes::from(declared.as_str()))
  };
  let mut lost = Vec::new();
  if let Some(filename) = dropped(&offered.filename, &written.filename) {
    lost.push(Uncarried::Filename { track, filename });
  }
  if let Some(mime_type) = dropped(&offered.mime_type, &written.mime_type) {
    lost.push(Uncarried::MimeType { track, mime_type });
  }
  if let Some(tag) = dropped(&offered.language, &written.language) {
    lost.push(Uncarried::Language { track, tag });
  }
  let flags = offered.disposition.difference(written.disposition);
  if !flags.is_empty() {
    lost.push(Uncarried::Disposition { track, flags });
  }
  lost
}

/// **L9 — the report names every field a Matroska attachment drops, and
/// nothing else.** A font declaring its filename, MIME type, a language
/// and two flags reaches Matroska with its file, filename and MIME type,
/// and its language among the tags written for it — but an attachment
/// there has no track entry for its flags. Read back, the report and the
/// file agree track by track: the report names exactly what each track
/// declared and the file does not hold.
#[test]
fn l9_the_report_names_every_field_a_matroska_attachment_drops() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let video = owned_video_and_font(&source, 3).video;
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
  let payload = AttachmentPacket::new(
    FfmpegBytes::try_copy_from_slice(support::FONT_PAYLOAD).expect("a payload"),
    AttachmentPacketExtra::new(0),
  );
  let font = TrackDescription::attachment(
    millis(),
    AttachmentTrackParams::new(CodecId::TTF),
    AttachedFile::new(payload)
      .with_filename(Some("font.ttf".into()))
      .with_mime_type(Some("application/x-truetype-font".into())),
    font_ticket,
  )
  .with_language(Some("jpn".into()))
  .with_disposition(TrackDisposition::DEFAULT | TrackDisposition::FORCED);
  let mut table = video_table(&source);
  table.push(font);
  let offered: Vec<Row> = table.iter().map(offered).collect();

  let target = output(&corpus, "l9.mkv");
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MATROSKA,
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  for (index, packet) in video.iter().enumerate() {
    let at = 40 * index as i64;
    muxer
      .write_video(restamped(packet, 0, Some(at), Some(at)))
      .map_err(|error| error.map_packet(drop))
      .expect("a packet");
  }
  muxer.finish().expect("the container is finished");

  let written = rows(&target);
  assert_eq!(written.len(), offered.len(), "the video and the font");
  let expected: Vec<Uncarried> = offered
    .iter()
    .zip(&written)
    .enumerate()
    .flat_map(|(track, (offered, written))| lost(track, offered, written))
    .collect();
  assert_eq!(
    report.uncarried(),
    expected.as_slice(),
    "the report names exactly what the file lost",
  );
  assert_eq!(
    expected,
    vec![Uncarried::Disposition {
      track: TrackIndex::new(1),
      flags: TrackDisposition::DEFAULT | TrackDisposition::FORCED,
    }],
    "the premise: the font kept its filename, MIME type and language, and lost its flags",
  );
}

/// **L10 — the report names every field MP4's cover drops, and nothing
/// else.** An MP3's cover art, declared with a filename, a MIME type, a
/// language and two flags beside its own, reaches MP4 as its cover: the
/// picture alone, read back as an attached picture. The report and the
/// file agree track by track — the cover's filename, MIME type, language
/// and flags each named, and the audio's losses, if any, with them.
#[test]
fn l10_the_report_names_every_field_mp4s_cover_drops() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.cover_art_mp3();
  let mut demuxer = FfmpegOwnedDemuxer::open(&source).expect("open the source");
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
  let mut table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> = demuxer
    .tracks()
    .iter()
    .zip(files)
    .map(|(row, file)| {
      TrackDescription::from_track_info(&**row, file, ticket(row)).expect("a description")
    })
    .collect();
  let cover = table
    .iter()
    .position(|description| description.kind() == TrackKind::Attachment)
    .expect("the premise: a cover track");
  let (timebase, params, _, cover_ticket, _, disposition, file) = table.remove(cover).into_parts();
  let TrackParams::Attachment(params) = params else {
    panic!("the premise: the cover is an attachment");
  };
  let (payload, _, _) = file.expect("the cover's picture").into_parts();
  table.insert(
    cover,
    TrackDescription::attachment(
      timebase,
      params,
      AttachedFile::new(payload)
        .with_filename(Some("cover.png".into()))
        .with_mime_type(Some("image/png".into())),
      cover_ticket,
    )
    .with_language(Some("eng".into()))
    .with_disposition(disposition | TrackDisposition::DEFAULT | TrackDisposition::COMMENT),
  );
  let offered: Vec<Row> = table.iter().map(offered).collect();

  let target = output(&corpus, "l10.mp4");
  let (mut muxer, report) = FfmpegOwnedMuxer::open(
    File::create(&target).expect("create"),
    ContainerName::MP4,
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open");
  for packet in timed {
    muxer
      .write_packet(packet)
      .map_err(|error| error.map_packet(drop))
      .expect("a packet");
  }
  muxer.finish().expect("the container is finished");

  // MP4 reads its tracks back in an order of its own: each offered track
  // is judged against the written track of its kind.
  let written = rows(&target);
  assert_eq!(written.len(), offered.len(), "the audio and the cover");
  let expected: Vec<Uncarried> = offered
    .iter()
    .enumerate()
    .flat_map(|(track, offered)| {
      let written = written
        .iter()
        .find(|row| row.kind == offered.kind)
        .expect("a written track of each kind");
      lost(track, offered, written)
    })
    .collect();
  assert_eq!(
    report.uncarried(),
    expected.as_slice(),
    "the report names exactly what the file lost",
  );
  let track = TrackIndex::new(cover);
  let on_the_cover = |lost: &&Uncarried| match lost {
    Uncarried::Filename { track: at, .. }
    | Uncarried::MimeType { track: at, .. }
    | Uncarried::Language { track: at, .. }
    | Uncarried::Disposition { track: at, .. } => *at == track,
    _ => false,
  };
  assert_eq!(
    expected.iter().filter(on_the_cover).count(),
    4,
    "the premise: the cover lost its filename, MIME type, language and flags: {expected:?}",
  );
  for named in [
    Uncarried::Filename {
      track,
      filename: "cover.png".into(),
    },
    Uncarried::MimeType {
      track,
      mime_type: "image/png".into(),
    },
    Uncarried::Language {
      track,
      tag: "eng".into(),
    },
  ] {
    assert!(expected.contains(&named), "{named:?} in {expected:?}");
  }
}

// ---------------------------------------------------------------------------
//  The packet-rule table: one law per refused row.
// ---------------------------------------------------------------------------

/// A file the `ffmpeg` CLI makes in the corpus from `args`, once.
fn generated(corpus: &Corpus, name: &str, args: &[&str]) -> PathBuf {
  let out = output(corpus, name);
  if !out.exists() {
    let status = std::process::Command::new("ffmpeg")
      .args(["-y", "-loglevel", "error"])
      .args(args)
      .arg(&out)
      .status()
      .expect("the ffmpeg CLI runs");
    assert!(status.success(), "ffmpeg made {name}");
  }
  out
}

/// `bytes`, copied into the carrier's buffer.
fn bytes_of(bytes: &[u8]) -> FfmpegBytes {
  FfmpegBytes::try_copy_from_slice(bytes).expect("a copy")
}

/// The one-track table of `path`'s first track of `kind`, afresh: a
/// description is handed to the muxer, not cloned.
fn table_of(
  path: &Path,
  kind: TrackKind,
) -> Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>> {
  first_track(path, kind, 0).0
}

/// A text file written into the corpus, once.
fn written_text(corpus: &Corpus, name: &str, text: &str) -> PathBuf {
  let out = output(corpus, name);
  if !out.exists() {
    std::fs::write(&out, text).expect("write");
  }
  out
}

/// AV1 video, made by SVT-AV1, in Matroska.
#[rustfmt::skip]
fn av1_mkv(corpus: &Corpus) -> PathBuf {
  generated(corpus, "av1.mkv", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x64:rate=25:duration=0.4",
    "-c:v", "libsvtav1", "-preset", "12", "-pix_fmt", "yuv420p",
  ])
}

/// H.264 with B-frames — a stream that reorders — in Matroska.
#[rustfmt::skip]
fn reordering_mkv(corpus: &Corpus) -> PathBuf {
  generated(corpus, "bframes.mkv", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25:duration=1",
    "-c:v", "libx264", "-preset", "medium", "-bf", "2", "-g", "25", "-pix_fmt", "yuv420p",
  ])
}

/// An audio file of `codec` at 48 kHz, stereo, in `name`'s container.
#[rustfmt::skip]
fn audio_file(corpus: &Corpus, name: &str, codec: &str) -> PathBuf {
  generated(corpus, name, &[
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.5",
    "-ac", "2", "-c:a", codec,
  ])
}

/// Stereo PCM in QuickTime, its channel layout stated: MP4's `ipcm`
/// writes one (`mov_write_chnl_tag`, `movenc.c` 1299–1323), which
/// Matroska does not keep.
#[rustfmt::skip]
fn pcm_mov(corpus: &Corpus) -> PathBuf {
  generated(corpus, "pcm.mov", &[
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.5",
    "-ch_layout", "stereo", "-c:a", "pcm_s16le",
  ])
}

/// ProRes video in QuickTime.
#[rustfmt::skip]
fn prores_mov(corpus: &Corpus) -> PathBuf {
  generated(corpus, "prores.mov", &[
    "-f", "lavfi", "-i", "testsrc2=size=64x64:rate=25:duration=0.2",
    "-c:v", "prores_ks", "-profile:v", "0", "-pix_fmt", "yuv422p10le",
  ])
}

/// Two `mov_text` cues in MP4.
#[rustfmt::skip]
fn mov_text_mp4(corpus: &Corpus) -> PathBuf {
  let srt = written_text(
    corpus,
    "two.srt",
    "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nworld\n\n",
  );
  generated(corpus, "movtext.mp4", &[
    "-i", srt.to_str().expect("utf-8 path"), "-c:s", "mov_text",
  ])
}

/// Two WebVTT cues in Matroska.
#[rustfmt::skip]
fn webvtt_mkv(corpus: &Corpus) -> PathBuf {
  let vtt = written_text(
    corpus,
    "two.vtt",
    "WEBVTT\n\n00:00.000 --> 00:01.000\nhello\n\n00:01.000 --> 00:02.000\nworld\n",
  );
  generated(corpus, "webvtt.mkv", &[
    "-i", vtt.to_str().expect("utf-8 path"), "-c:s", "webvtt",
  ])
}

/// A one-track table, and packets of that track.
type Track = (
  Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
  Vec<DemuxedPacket<Ffmpeg, FfmpegBytes>>,
);

/// The first track of `kind` in `path` as a one-track table, and that
/// track's first `count` packets, owned.
fn first_track(path: &Path, kind: TrackKind, count: usize) -> Track {
  let mut demuxer = FfmpegOwnedDemuxer::open(path).expect("open");
  let (index, description) = demuxer
    .tracks()
    .iter()
    .enumerate()
    .find(|(_, row)| row.kind() == kind)
    .map(|(index, row)| {
      (
        index,
        TrackDescription::from_track_info(&**row, None, ticket(row)).expect("a description"),
      )
    })
    .expect("a track of the kind");
  let mut packets = Vec::new();
  while packets.len() < count {
    let Some(packet) = demuxer.next_packet().expect("a packet or the end") else {
      break;
    };
    if packet.track().get() == index {
      packets.push(packet);
    }
  }
  assert_eq!(packets.len(), count, "the premise: {count} packets");
  (vec![description], packets)
}

type Video = mediaencode::mediaspine::demuxer::DemuxVideoPacket<Ffmpeg, FfmpegBytes>;
type Audio = mediaencode::mediaspine::demuxer::DemuxAudioPacket<Ffmpeg, FfmpegBytes>;
type Subtitle = mediaencode::mediaspine::demuxer::DemuxSubtitlePacket<Ffmpeg, FfmpegBytes>;

fn video_of(packet: &DemuxedPacket<Ffmpeg, FfmpegBytes>) -> Video {
  match packet {
    DemuxedPacket::Video(p) => p.packet().clone(),
    other => panic!("expected video, got {other:?}"),
  }
}

fn audio_of(packet: &DemuxedPacket<Ffmpeg, FfmpegBytes>) -> Audio {
  match packet {
    DemuxedPacket::Audio(p) => p.packet().clone(),
    other => panic!("expected audio, got {other:?}"),
  }
}

fn subtitle_of(packet: &DemuxedPacket<Ffmpeg, FfmpegBytes>) -> Subtitle {
  match packet {
    DemuxedPacket::Subtitle(p) => p.packet().clone(),
    other => panic!("expected a subtitle, got {other:?}"),
  }
}

/// `packet` on track 0, presented at `pts` and decoded at `dts`
/// milliseconds.
fn video_at(packet: &Video, pts: Option<i64>, dts: i64) -> VideoTrackPacket<Ffmpeg, FfmpegBytes> {
  VideoTrackPacket::new(
    TrackIndex::new(0),
    packet.clone().with_pts(pts.map(ms)).with_dts(Some(ms(dts))),
  )
}

fn audio_at(packet: &Audio, at: i64) -> AudioTrackPacket<Ffmpeg, FfmpegBytes> {
  AudioTrackPacket::new(
    TrackIndex::new(0),
    packet.clone().with_pts(Some(ms(at))).with_dts(Some(ms(at))),
  )
}

fn subtitle_at(
  packet: &Subtitle,
  at: i64,
  duration: i64,
) -> SubtitleTrackPacket<Ffmpeg, FfmpegBytes> {
  SubtitleTrackPacket::new(
    TrackIndex::new(0),
    packet
      .clone()
      .with_pts(Some(ms(at)))
      .with_duration(Some(ms(duration))),
  )
}

/// `packet` holding `bytes` instead, everything else as it was.
fn video_holding(packet: &Video, bytes: &[u8]) -> Video {
  let (_, extra) = packet.clone().into_parts();
  mediaencode::mediaspine::packet::VideoPacket::new(bytes_of(bytes), extra)
    .with_pts(packet.pts())
    .with_dts(packet.dts())
    .with_duration(packet.duration())
    .with_flags(packet.flags())
}

fn audio_holding(packet: &Audio, bytes: &[u8]) -> Audio {
  let (_, extra) = packet.clone().into_parts();
  mediaencode::mediaspine::packet::AudioPacket::new(bytes_of(bytes), extra)
    .with_pts(packet.pts())
    .with_dts(packet.dts())
    .with_duration(packet.duration())
    .with_flags(packet.flags())
}

fn subtitle_holding(packet: &Subtitle, bytes: &[u8]) -> Subtitle {
  let (_, extra) = packet.clone().into_parts();
  mediaencode::mediaspine::packet::SubtitlePacket::new(bytes_of(bytes), extra)
    .with_pts(packet.pts())
    .with_duration(packet.duration())
    .with_flags(packet.flags())
}

/// One side data entry of `kind`.
fn side(kind: ffmpeg_next::ffi::AVPacketSideDataType, bytes: &[u8]) -> SideDataEntry {
  SideDataEntry::new(kind as i32, bytes_of(bytes))
}

/// A muxer for `container` into `target`, over `table`.
fn muxer_into(
  target: &Path,
  container: ContainerName,
  table: Vec<TrackDescription<Ffmpeg, FfmpegBytes, CodecTicket>>,
) -> FfmpegOwnedMuxer<File> {
  FfmpegOwnedMuxer::open(
    File::create(target).expect("create"),
    container,
    table,
    &[],
    PacketLimits::default(),
  )
  .expect("open")
  .0
}

/// The container's own refusal and the packet handed back, or a panic
/// naming what came instead.
fn by_container<P: core::fmt::Debug>(
  result: Result<(), MuxError<mediaencode_ffmpeg::MuxFault, FfmpegMuxRefusal, P>>,
) -> (FfmpegMuxRefusal, P) {
  match result {
    Err(MuxError::Backend { refusal, packet }) => (refusal, packet),
    other => panic!("expected the container's refusal, got {other:?}"),
  }
}

/// A payload rule's refusal on track 0.
fn payload(rule: PayloadRule) -> FfmpegMuxRefusal {
  FfmpegMuxRefusal::Payload {
    track: TrackIndex::new(0),
    rule,
  }
}

/// The packet handed back is the one offered: its track, its very bytes,
/// its times and flags.
macro_rules! untouched {
  ($offered:expr, $back:expr) => {{
    let (offered, back) = (&$offered, &$back);
    assert_eq!(offered.track(), back.track());
    assert!(
      back.packet().data().ptr_eq(offered.packet().data()),
      "the very bytes it was offered with"
    );
    assert_eq!(offered.packet().pts(), back.packet().pts());
    assert_eq!(offered.packet().duration(), back.packet().duration());
    assert_eq!(offered.packet().flags(), back.packet().flags());
  }};
}

/// Every packet `path` holds, across its tracks.
fn packet_count(path: &Path) -> usize {
  packets(path).iter().map(Vec::len).sum()
}

/// A time of 2^31 seconds: past `i32::MAX` ticks in any time base of a
/// tick a second or finer.
const PAST_I32_SECONDS: i64 = 1 << 31;

/// **L11 — a negative duration is refused by name** in MP4, which refuses
/// a subtitle's after taking it, and in Matroska, which writes zero; the
/// packet is handed back and the file holds only what was admitted.
#[test]
fn l11_a_negative_duration_is_refused_by_name() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (_, packets) = first_track(&source, TrackKind::Video, 2);
  for (container, name) in [
    (ContainerName::MP4, "l11.mp4"),
    (ContainerName::MATROSKA, "l11.mkv"),
  ] {
    let target = output(&corpus, name);
    let mut muxer = muxer_into(&target, container, table_of(&source, TrackKind::Video));
    muxer
      .write_video(video_at(&video_of(&packets[0]), Some(0), 0))
      .expect("the first packet");
    let offered = video_at(
      &video_of(&packets[1]).with_duration(Some(ms(-40))),
      Some(40),
      40,
    );
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(
      refusal,
      FfmpegMuxRefusal::NegativeDuration {
        track: TrackIndex::new(0),
        duration: ms(-40),
      },
      "{name}"
    );
    untouched!(offered, back);
    muxer.finish().expect("finished");
    assert_eq!(packet_count(&target), 1, "{name}: only what was admitted");
  }
}

/// **L12 — MP4 refuses by name a duration past `i32::MAX` ticks, a decode
/// time that far after the last, and a presentation that far after its
/// decode.** It would refuse the first after taking the packet, rewrite
/// the second to one tick after the last with no presentation time, and
/// overflow its 32-bit composition offset on the third. The gap is judged
/// in `i128`: from a decode time far below zero to one far above, past
/// what an `i64` holds, it is refused, not overflowed.
#[test]
fn l12_mp4_refuses_times_past_its_32_bit_fields() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (table, packets) = first_track(&source, TrackKind::Video, 2);
  let track = TrackIndex::new(0);
  let past = ms(PAST_I32_SECONDS * 1000);
  let target = output(&corpus, "l12.mp4");
  let mut muxer = muxer_into(
    &target,
    ContainerName::MP4,
    table_of(&source, TrackKind::Video),
  );
  muxer
    .write_video(video_at(&video_of(&packets[0]), Some(0), 0))
    .expect("the first packet");

  let long = video_at(
    &video_of(&packets[1]).with_duration(Some(past)),
    Some(40),
    40,
  );
  let (refusal, back) = by_container(muxer.write_video(long.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::DurationTooLong {
      track,
      duration: past
    }
  );
  untouched!(long, back);

  let gap = video_at(&video_of(&packets[1]), Some(past.pts()), past.pts());
  let (refusal, back) = by_container(muxer.write_video(gap.clone()));
  assert_eq!(refusal, FfmpegMuxRefusal::DtsGap { track, dts: past });
  untouched!(gap, back);

  let late = video_at(&video_of(&packets[1]), Some(past.pts()), 40);
  let (refusal, back) = by_container(muxer.write_video(late.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::PresentationOffset {
      track,
      pts: past,
      dts: ms(40),
    }
  );
  untouched!(late, back);
  muxer.finish().expect("finished");
  assert_eq!(packet_count(&target), 1, "only what was admitted");

  // MP4 takes negative times as they are; a gap from far below zero to far
  // above it is past an `i64` of ticks, and judged without overflow.
  let target = output(&corpus, "l12-wide.mp4");
  let mut muxer = muxer_into(&target, ContainerName::MP4, table);
  let far = 300_000_000_000_000_000;
  muxer
    .write_video(video_at(&video_of(&packets[0]), Some(-far), -far))
    .expect("a time far below zero");
  let across = video_at(&video_of(&packets[1]), Some(far), far);
  let (refusal, _) = by_container(muxer.write_video(across));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::DtsGap {
      track,
      dts: ms(far)
    }
  );
}

/// **L13 — a stream that reorders has no presentation time made up for
/// it: a packet without one is refused by name** in Matroska, which
/// refuses it after taking it, and in MP4, which presents it at its
/// decode time. A stream that does not reorder is presented at its decode
/// time by libavformat, and admitted.
#[test]
fn l13_a_reordering_stream_needs_a_presentation_time() {
  let Some(corpus) = Corpus::new() else { return };
  let reordering = reordering_mkv(&corpus);
  let (_, packets) = first_track(&reordering, TrackKind::Video, 2);
  for (container, name) in [
    (ContainerName::MATROSKA, "l13.mkv"),
    (ContainerName::MP4, "l13.mp4"),
  ] {
    let target = output(&corpus, name);
    let mut muxer = muxer_into(&target, container, table_of(&reordering, TrackKind::Video));
    muxer
      .write_video(video_at(&video_of(&packets[0]), Some(80), 0))
      .expect("the first packet");
    let offered = video_at(&video_of(&packets[1]), None, 40);
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(
      refusal,
      FfmpegMuxRefusal::MissingPts {
        track: TrackIndex::new(0)
      },
      "{name}"
    );
    untouched!(offered, back);
    muxer.finish().expect("finished");
  }
  let (table, packets) = first_track(&corpus.multi_track_mkv(), TrackKind::Video, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l13-plain.mkv"),
    ContainerName::MATROSKA,
    table,
  );
  muxer
    .write_video(video_at(&video_of(&packets[0]), None, 0))
    .expect("a stream that does not reorder is presented at its decode time");
  muxer.finish().expect("finished");
}

/// **L14 — a time before the earliest the container writes is refused by
/// name**: in Matroska a presentation time below zero, or below an audio
/// track's codec delay before zero; in another container that shifts
/// early times, a decode time below zero. Either would move every
/// stream's times.
#[test]
fn l14_a_time_before_the_earliest_written_is_refused_by_name() {
  let Some(corpus) = Corpus::new() else { return };
  let track = TrackIndex::new(0);
  let source = corpus.multi_track_mkv();
  let (table, packets) = first_track(&source, TrackKind::Video, 1);
  let mut muxer = muxer_into(&output(&corpus, "l14.mkv"), ContainerName::MATROSKA, table);
  let early = video_at(&video_of(&packets[0]), Some(-40), -40);
  let (refusal, back) = by_container(muxer.write_video(early.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::BeforeZero {
      track,
      timestamp: ms(-40)
    }
  );
  untouched!(early, back);

  // AAC's encoder delay of 1024 samples at 48 kHz: 21 ms before zero.
  let (audio_table, audio) = first_track(&source, TrackKind::Audio, 2);
  let mut muxer = muxer_into(
    &output(&corpus, "l14-audio.mkv"),
    ContainerName::MATROSKA,
    audio_table,
  );
  let below = audio_at(&audio_of(&audio[0]), -22);
  let (refusal, back) = by_container(muxer.write_audio(below.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::BeforeZero {
      track,
      timestamp: ms(-22)
    }
  );
  untouched!(below, back);
  muxer
    .write_audio(audio_at(&audio_of(&audio[0]), -21))
    .expect("within the codec delay");
  muxer.finish().expect("finished");

  // FLV has no `AVFMT_TS_NEGATIVE`: libavformat would shift every stream
  // by the first decode time below zero.
  let mut muxer = muxer_into(
    &output(&corpus, "l14.flv"),
    ContainerName::new("flv"),
    table_of(&source, TrackKind::Video),
  );
  let early = video_at(&video_of(&packets[0]), Some(-40), -40);
  let (refusal, back) = by_container(muxer.write_video(early.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::BeforeZero {
      track,
      timestamp: ms(-40)
    }
  );
  untouched!(early, back);
}

/// **L15 — MP4 refuses by name a `mov_text` cue before zero, or before
/// the last cue ends**, which it would rewrite after the empty sample it
/// marks the last cue's end with. A cue after the last one ends is
/// written.
#[test]
fn l15_mp4_refuses_a_cue_out_of_order() {
  let Some(corpus) = Corpus::new() else { return };
  let source = mov_text_mp4(&corpus);
  let (table, cues) = first_track(&source, TrackKind::Subtitle, 1);
  let cue = subtitle_of(&cues[0]);
  let track = TrackIndex::new(0);
  let mut muxer = muxer_into(&output(&corpus, "l15.mp4"), ContainerName::MP4, table);
  muxer
    .write_subtitle(subtitle_at(&cue, 0, 1000))
    .expect("the first cue");
  let early = subtitle_at(&cue, 500, 100);
  let (refusal, back) = by_container(muxer.write_subtitle(early.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::CueOrder {
      track,
      pts: ms(500)
    }
  );
  untouched!(early, back);
  muxer
    .write_subtitle(subtitle_at(&cue, 1000, 500))
    .expect("a cue where the last one ends");
  muxer.finish().expect("finished");

  let mut muxer = muxer_into(
    &output(&corpus, "l15-early.mp4"),
    ContainerName::MP4,
    table_of(&source, TrackKind::Subtitle),
  );
  let before = subtitle_at(&cue, -10, 100);
  let (refusal, back) = by_container(muxer.write_subtitle(before.clone()));
  assert_eq!(
    refusal,
    FfmpegMuxRefusal::CueOrder {
      track,
      pts: ms(-10)
    }
  );
  untouched!(before, back);
}

/// **L16 — a packet of a codec of key frames alone, not marked one, is
/// refused by name**, which libavformat would mark: PCM into MP4 and into
/// Matroska. Matroska writes every subtitle block as a key frame, so
/// there a subtitle's flag is not kept either way, and the cue is
/// admitted.
#[test]
fn l16_an_unmarked_key_frame_is_refused_by_name() {
  let Some(corpus) = Corpus::new() else { return };
  let pcm = pcm_mov(&corpus);
  let (_, packets) = first_track(&pcm, TrackKind::Audio, 2);
  for (container, name) in [
    (ContainerName::MP4, "l16.mp4"),
    (ContainerName::MATROSKA, "l16.mkv"),
  ] {
    let mut muxer = muxer_into(
      &output(&corpus, name),
      container,
      table_of(&pcm, TrackKind::Audio),
    );
    muxer
      .write_audio(audio_at(&audio_of(&packets[0]), 0))
      .expect("a key frame");
    let unmarked = AudioTrackPacket::new(
      TrackIndex::new(0),
      audio_of(&packets[1])
        .with_pts(Some(ms(10)))
        .with_dts(Some(ms(10)))
        .with_flags(PacketFlags::empty()),
    );
    let (refusal, back) = by_container(muxer.write_audio(unmarked.clone()));
    assert_eq!(
      refusal,
      FfmpegMuxRefusal::KeyFrameUnmarked {
        track: TrackIndex::new(0)
      },
      "{name}"
    );
    untouched!(unmarked, back);
    muxer.finish().expect("finished");
  }
  let (table, cues) = first_track(&corpus.multi_track_mkv(), TrackKind::Subtitle, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l16-cue.mkv"),
    ContainerName::MATROSKA,
    table,
  );
  muxer
    .write_subtitle(SubtitleTrackPacket::new(
      TrackIndex::new(0),
      subtitle_of(&cues[0])
        .with_pts(Some(ms(0)))
        .with_flags(PacketFlags::empty()),
    ))
    .expect("Matroska writes a subtitle as a key frame regardless");
  muxer.finish().expect("finished");
}

/// **L17 — an empty packet a container drops is refused by name**: any in
/// MP4, an audio packet in Matroska. Matroska writes an empty video
/// packet as a block of its own.
#[test]
fn l17_an_empty_packet_a_container_drops_is_refused() {
  let Some(corpus) = Corpus::new() else { return };
  let source = corpus.multi_track_mkv();
  let (video_table, video) = first_track(&source, TrackKind::Video, 2);
  let mut muxer = muxer_into(&output(&corpus, "l17.mp4"), ContainerName::MP4, video_table);
  muxer
    .write_video(video_at(&video_of(&video[0]), Some(0), 0))
    .expect("the first packet");
  let empty = video_at(&video_holding(&video_of(&video[1]), &[]), Some(40), 40);
  let (refusal, back) = by_container(muxer.write_video(empty.clone()));
  assert_eq!(refusal, payload(PayloadRule::Empty));
  untouched!(empty, back);
  muxer.finish().expect("finished");

  let (audio_table, audio) = first_track(&source, TrackKind::Audio, 1);
  let mut muxer = muxer_into(
    &output(&corpus, "l17.mkv"),
    ContainerName::MATROSKA,
    audio_table,
  );
  let empty = audio_at(&audio_holding(&audio_of(&audio[0]), &[]), 0);
  let (refusal, back) = by_container(muxer.write_audio(empty.clone()));
  assert_eq!(refusal, payload(PayloadRule::Empty));
  untouched!(empty, back);
  muxer.finish().expect("finished");

  let target = output(&corpus, "l17-video.mkv");
  let mut muxer = muxer_into(
    &target,
    ContainerName::MATROSKA,
    table_of(&source, TrackKind::Video),
  );
  muxer
    .write_video(video_at(&video_of(&video[0]), Some(0), 0))
    .expect("the first packet");
  muxer
    .write_video(video_at(
      &video_holding(&video_of(&video[1]), &[]),
      Some(40),
      40,
    ))
    .expect("Matroska writes an empty video packet");
  muxer.finish().expect("finished");
}

/// **L18 — AAC in ADTS headers is refused by name where the container
/// takes raw AAC**: a transport stream's AAC into MP4, which would refuse
/// the first such packet after taking it, and an ADTS packet on a raw AAC
/// track into Matroska, which would write it as it came.
#[test]
fn l18_aac_in_adts_headers_is_refused_by_name() {
  let Some(corpus) = Corpus::new() else { return };
  let (table, packets) = first_track(&corpus.transport_stream(), TrackKind::Audio, 1);
  let mut muxer = muxer_into(&output(&corpus, "l18.mp4"), ContainerName::MP4, table);
  let adts = audio_at(&audio_of(&packets[0]), 0);
  assert!(
    adts.packet().data().as_ref().starts_with(&[0xFF]),
    "the premise: a transport stream's AAC is framed in ADTS"
  );
  let (refusal, back) = by_container(muxer.write_audio(adts.clone()));
  assert_eq!(refusal, payload(PayloadRule::AdtsHeader));
  untouched!(adts, back);

  let (table, packets) = first_track(&corpus.multi_track_mkv(), TrackKind::Audio, 1);
  let mut muxer = muxer_into(&output(&corpus, "l18.mkv"), ContainerName::MATROSKA, table);
  let raw = audio_of(&packets[0]);
  let mut framed = vec![0xFF, 0xF1, 0x4C, 0x80, 0x00, 0x00, 0xFC];
  framed.extend_from_slice(raw.data().as_ref());
  let offered = audio_at(&audio_holding(&raw, &framed), 0);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::AdtsHeader));
  untouched!(offered, back);
}

/// **L19 — MP4 refuses PCM that is not a whole number of frames**, whose
/// rest it would not count.
#[test]
fn l19_mp4_refuses_a_partial_pcm_frame() {
  let Some(corpus) = Corpus::new() else { return };
  let (table, packets) = first_track(&pcm_mov(&corpus), TrackKind::Audio, 1);
  let mut muxer = muxer_into(&output(&corpus, "l19.mp4"), ContainerName::MP4, table);
  let whole = audio_of(&packets[0]);
  let mut bytes = whole.data().as_ref().to_vec();
  bytes.push(0);
  let offered = audio_at(&audio_holding(&whole, &bytes), 0);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::PartialSample));
  untouched!(offered, back);
  muxer
    .write_audio(audio_at(&whole, 0))
    .expect("whole frames");
  muxer.finish().expect("finished");
}

/// **L20 — AV1 whose OBUs do not parse is refused by name** in MP4,
/// which would write a sample of a negative size, and in Matroska, which
/// refuses it after taking it.
#[test]
fn l20_av1_whose_obus_do_not_parse_is_refused() {
  let Some(corpus) = Corpus::new() else { return };
  let av1 = av1_mkv(&corpus);
  let (_, packets) = first_track(&av1, TrackKind::Video, 1);
  for (container, name) in [
    (ContainerName::MP4, "l20.mp4"),
    (ContainerName::MATROSKA, "l20.mkv"),
  ] {
    let mut muxer = muxer_into(
      &output(&corpus, name),
      container,
      table_of(&av1, TrackKind::Video),
    );
    let packet = video_of(&packets[0]);
    let mut bytes = packet.data().as_ref().to_vec();
    bytes[0] |= 0x80;
    let offered = video_at(&video_holding(&packet, &bytes), Some(0), 0);
    let (refusal, back) = by_container(muxer.write_video(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::Obus), "{name}");
    untouched!(offered, back);
    muxer
      .write_video(video_at(&packet, Some(0), 0))
      .expect("OBUs that parse");
    muxer.finish().expect("finished");
  }
}

/// **L21 — Matroska refuses WavPack blocks that do not parse whole**: one
/// cut short, which it refuses after taking it, and one with bytes left
/// over, which it would drop.
#[test]
fn l21_matroska_refuses_wavpack_that_does_not_parse_whole() {
  let Some(corpus) = Corpus::new() else { return };
  let (table, packets) = first_track(
    &audio_file(&corpus, "wavpack.mkv", "wavpack"),
    TrackKind::Audio,
    1,
  );
  let mut muxer = muxer_into(&output(&corpus, "l21.mkv"), ContainerName::MATROSKA, table);
  let packet = audio_of(&packets[0]);
  let whole = packet.data().as_ref().to_vec();
  let mut over = whole.clone();
  over.extend([1, 2, 3]);
  for bytes in [&whole[..whole.len() - 1], &over[..]] {
    let offered = audio_at(&audio_holding(&packet, bytes), 0);
    let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::WavPackBlocks));
    untouched!(offered, back);
  }
  muxer
    .write_audio(audio_at(&packet, 0))
    .expect("whole blocks");
  muxer.finish().expect("finished");
}

/// **L22 — side data a container would refuse is refused by name**: MP4's
/// skip-samples that shorten a packet below nothing, and Matroska's new
/// FLAC STREAMINFO of another length than the track's. A STREAMINFO of
/// the track's length is written.
#[test]
fn l22_side_data_a_container_refuses_is_refused_by_name() {
  let Some(corpus) = Corpus::new() else { return };
  let (table, packets) = first_track(&corpus.multi_track_mkv(), TrackKind::Audio, 1);
  let mut muxer = muxer_into(&output(&corpus, "l22.mp4"), ContainerName::MP4, table);
  let mut skip = 0u32.to_le_bytes().to_vec();
  skip.extend(4096u32.to_le_bytes());
  skip.extend([0, 0]);
  let mut skipping = audio_of(&packets[0])
    .with_pts(Some(ms(0)))
    .with_dts(Some(ms(0)));
  skipping.extra_mut().set_side_data(vec![side(
    ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES,
    &skip,
  )]);
  let offered = AudioTrackPacket::new(TrackIndex::new(0), skipping);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::SkipSamples));
  untouched!(offered, back);

  let (table, packets) = first_track(
    &audio_file(&corpus, "flac.mkv", "flac"),
    TrackKind::Audio,
    2,
  );
  let streaminfo = table[0]
    .extradata()
    .map(|bytes| bytes.as_ref().to_vec())
    .unwrap_or_else(|| table[0].side_data().extradata().to_vec());
  let mut muxer = muxer_into(&output(&corpus, "l22.mkv"), ContainerName::MATROSKA, table);
  let with_extradata = |packet: &Audio, at: i64, bytes: &[u8]| {
    let mut packet = packet.clone().with_pts(Some(ms(at))).with_dts(Some(ms(at)));
    packet.extra_mut().set_side_data(vec![side(
      ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
      bytes,
    )]);
    AudioTrackPacket::new(TrackIndex::new(0), packet)
  };
  let offered = with_extradata(&audio_of(&packets[0]), 0, &[1; 10]);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::FlacStreamInfo));
  untouched!(offered, back);
  muxer
    .write_audio(with_extradata(&audio_of(&packets[0]), 0, &streaminfo))
    .expect("a STREAMINFO of the track's length");
  muxer.finish().expect("finished");
}

/// **L23 — Matroska refuses by name a ProRes packet that does not open on
/// its own frame atom**, which it would drop and its reader put back
/// otherwise. One that does is written and reads back as itself.
#[test]
fn l23_matroska_refuses_prores_off_its_atom() {
  let Some(corpus) = Corpus::new() else { return };
  let (table, frames) = first_track(&prores_mov(&corpus), TrackKind::Video, 2);
  let target = output(&corpus, "l23.mkv");
  let mut muxer = muxer_into(&target, ContainerName::MATROSKA, table);
  let packet = video_of(&frames[0]);
  let mut bytes = packet.data().as_ref().to_vec();
  assert_eq!(&bytes[4..8], b"icpf", "the premise: a ProRes frame's atom");
  bytes[4] = b'x';
  let offered = video_at(&video_holding(&packet, &bytes), Some(0), 0);
  let (refusal, back) = by_container(muxer.write_video(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::ProResAtom));
  untouched!(offered, back);
  for (index, packet) in frames.iter().enumerate() {
    let at = 40 * index as i64;
    muxer
      .write_video(video_at(&video_of(packet), Some(at), at))
      .expect("a frame on its atom");
  }
  muxer.finish().expect("finished");
  let written = &packets(&target)[0];
  for (index, packet) in frames.iter().enumerate() {
    assert_eq!(
      written[index].bytes,
      video_of(packet).data().as_ref(),
      "read back as itself"
    );
  }
}

/// **L24 — Matroska and WebM refuse by name a WebVTT cue that would not
/// read back from its lines**, here one whose text ends in a line break.
/// A cue that would is written and reads back as itself.
#[test]
fn l24_a_webvtt_cue_that_would_not_read_back_is_refused() {
  let Some(corpus) = Corpus::new() else { return };
  let source = webvtt_mkv(&corpus);
  let (_, cues) = first_track(&source, TrackKind::Subtitle, 1);
  for (container, name) in [
    (ContainerName::MATROSKA, "l24.mkv"),
    (ContainerName::WEBM, "l24.webm"),
  ] {
    let target = output(&corpus, name);
    let mut muxer = muxer_into(&target, container, table_of(&source, TrackKind::Subtitle));
    let cue = subtitle_of(&cues[0]);
    let mut text = cue.data().as_ref().to_vec();
    assert!(!text.ends_with(b"\n"), "the premise: a cue's text");
    text.push(b'\n');
    let offered = subtitle_at(&subtitle_holding(&cue, &text), 0, 1000);
    let (refusal, back) = by_container(muxer.write_subtitle(offered.clone()));
    assert_eq!(refusal, payload(PayloadRule::WebVttCue), "{name}");
    untouched!(offered, back);
    muxer
      .write_subtitle(subtitle_at(&cue, 0, 1000))
      .expect("a cue that reads back");
    muxer.finish().expect("finished");
    let written = &packets(&target)[0];
    assert_eq!(written.len(), 1);
    assert_eq!(
      written[0].bytes,
      cue.data().as_ref(),
      "{name}: read back as itself"
    );
  }
}

/// **L25 — MP4 refuses by name AC-3 and E-AC-3 packets its handling would
/// drop or refuse**: an AC-3 packet whose header does not parse; an E-AC-3
/// track's first packet with bytes after its frame that are no dependent
/// frame; and an E-AC-3 frame whose six blocks would follow three held
/// back — the first packet's own, recoded as three — past a sample of six.
#[test]
fn l25_mp4_refuses_ac3_it_would_drop_or_refuse() {
  let Some(corpus) = Corpus::new() else { return };
  let (table, packets) = first_track(&audio_file(&corpus, "ac3.mkv", "ac3"), TrackKind::Audio, 1);
  let mut muxer = muxer_into(&output(&corpus, "l25.mp4"), ContainerName::MP4, table);
  let packet = audio_of(&packets[0]);
  let mut bytes = packet.data().as_ref().to_vec();
  bytes[0] = 0;
  let offered = audio_at(&audio_holding(&packet, &bytes), 0);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::Ac3Header));
  untouched!(offered, back);
  muxer
    .write_audio(audio_at(&packet, 0))
    .expect("an AC-3 frame");
  muxer.finish().expect("finished");

  let eac3 = audio_file(&corpus, "eac3.mkv", "eac3");
  let (table, packets) = first_track(&eac3, TrackKind::Audio, 2);
  let first = audio_of(&packets[0]);
  let mut tail = first.data().as_ref().to_vec();
  tail.extend([1, 2, 3]);
  let mut muxer = muxer_into(&output(&corpus, "l25-eac3.mp4"), ContainerName::MP4, table);
  let offered = audio_at(&audio_holding(&first, &tail), 0);
  let (refusal, back) = by_container(muxer.write_audio(offered.clone()));
  assert_eq!(refusal, payload(PayloadRule::Ac3Substreams));
  untouched!(offered, back);

  // The first frame recoded as three blocks: `numblkscod`, bits 4–5 of
  // its fifth byte, at 48 kHz.
  let mut three = first.data().as_ref().to_vec();
  assert_eq!(three[4] >> 6, 0, "the premise: 48 kHz");
  assert_eq!((three[4] >> 4) & 3, 3, "the premise: six blocks");
  three[4] = (three[4] & !0x30) | 0x20;
  let mut muxer = muxer_into(
    &output(&corpus, "l25-blocks.mp4"),
    ContainerName::MP4,
    table_of(&eac3, TrackKind::Audio),
  );
  muxer
    .write_audio(audio_at(&audio_holding(&first, &three), 0))
    .expect("three blocks, held back");
  let six = audio_at(&audio_of(&packets[1]), 32);
  let (refusal, back) = by_container(muxer.write_audio(six.clone()));
  assert_eq!(refusal, payload(PayloadRule::Ac3Blocks));
  untouched!(six, back);
}

/// **L26 — a configuration the container needs and the track does not
/// carry is refused at open, by name, and nothing is written**: a
/// transport stream's AAC into Matroska, H.264 without one into MP4, AV1
/// without one into WebM.
#[test]
fn l26_a_missing_configuration_is_refused_at_open() {
  let Some(corpus) = Corpus::new() else { return };
  let open = |name: &str, container: ContainerName, table| {
    let target = output(&corpus, name);
    let result = FfmpegOwnedMuxer::open(
      File::create(&target).expect("create"),
      container,
      table,
      &[],
      PacketLimits::default(),
    );
    assert_eq!(
      std::fs::metadata(&target).expect("the file").len(),
      0,
      "nothing was written"
    );
    result.map(drop)
  };
  let (aac, _) = first_track(&corpus.transport_stream(), TrackKind::Audio, 1);
  assert!(matches!(
    open("l26.mkv", ContainerName::MATROSKA, aac),
    Err(MuxOpenError::ConfigurationMissing { track, codec }) if track.get() == 0 && codec == CodecId::AAC
  ));
  let none = || Some(bytes_of(&[]));
  let (h264, _) = first_track(&corpus.multi_track_mkv(), TrackKind::Video, 1);
  let h264 = h264
    .into_iter()
    .map(|row| row.with_extradata(none()))
    .collect();
  assert!(matches!(
    open("l26.mp4", ContainerName::MP4, h264),
    Err(MuxOpenError::ConfigurationMissing { codec, .. }) if codec == CodecId::H264
  ));
  let (av1, _) = first_track(&av1_mkv(&corpus), TrackKind::Video, 1);
  let av1 = av1
    .into_iter()
    .map(|row| row.with_extradata(none()))
    .collect();
  assert!(matches!(
    open("l26.webm", ContainerName::WEBM, av1),
    Err(MuxOpenError::ConfigurationMissing { codec, .. }) if codec == CodecId::AV1
  ));
}

/// **L28 — a packet the container writes in its own form is reported at
/// open, and reads back so**: MP4 marks each `mov_text` cue's end with an
/// empty sample of its own; MP4 writes a transport stream's Annex B H.264
/// with length prefixes; Matroska strips a WavPack block's header to the
/// fields it keeps.
#[test]
fn l28_a_rewrite_is_reported_and_reads_back_so() {
  let Some(corpus) = Corpus::new() else { return };
  let track = TrackIndex::new(0);
  let open = |name: &str, container: ContainerName, table| {
    FfmpegOwnedMuxer::open(
      File::create(output(&corpus, name)).expect("create"),
      container,
      table,
      &[],
      PacketLimits::default(),
    )
    .expect("open")
  };

  let (table, cues) = first_track(&mov_text_mp4(&corpus), TrackKind::Subtitle, 1);
  let (mut muxer, report) = open("l28.mp4", ContainerName::MP4, table);
  assert!(report.uncarried().contains(&Uncarried::Rewritten {
    track,
    rewrite: Rewrite::CueEnds
  }));
  let cue = subtitle_of(&cues[0]);
  muxer
    .write_subtitle(subtitle_at(&cue, 0, 500))
    .expect("a cue");
  muxer
    .write_subtitle(subtitle_at(&cue, 1000, 500))
    .expect("another, after a gap");
  muxer.finish().expect("finished");
  let ends: Vec<(Option<i64>, bool)> = packets(&output(&corpus, "l28.mp4"))[0]
    .iter()
    .map(|seen| {
      (
        seen
          .pts
          .and_then(|pts| pts.checked_rescale_with(millis(), Rounding::Nearest))
          .map(|pts| pts.pts()),
        seen.bytes == [0, 0],
      )
    })
    .collect();
  // The last cue's end, written at the trailer (`movenc.c` 9024–9031), is
  // not read back.
  assert_eq!(
    ends,
    vec![(Some(0), false), (Some(500), true), (Some(1000), false)],
    "an empty sample at the end of a cue a later one passes, read back as a cue of its own",
  );

  let (table, video) = first_track(&corpus.transport_stream(), TrackKind::Video, 1);
  let (mut muxer, report) = open("l28-annexb.mp4", ContainerName::MP4, table);
  assert!(report.uncarried().contains(&Uncarried::Rewritten {
    track,
    rewrite: Rewrite::LengthPrefixed
  }));
  let offered = video_of(&video[0]);
  assert!(
    offered.data().as_ref().starts_with(&[0, 0, 0, 1])
      || offered.data().as_ref().starts_with(&[0, 0, 1]),
    "the premise: Annex B"
  );
  muxer
    .write_video(video_at(&offered, Some(0), 0))
    .expect("written");
  muxer.finish().expect("finished");
  assert_ne!(
    packets(&output(&corpus, "l28-annexb.mp4"))[0][0].bytes,
    offered.data().as_ref(),
    "read back with length prefixes",
  );

  let (table, blocks) = first_track(
    &audio_file(&corpus, "wavpack.wv", "wavpack"),
    TrackKind::Audio,
    1,
  );
  let (mut muxer, report) = open("l28-wavpack.mkv", ContainerName::MATROSKA, table);
  assert!(report.uncarried().contains(&Uncarried::Rewritten {
    track,
    rewrite: Rewrite::WavPackHeaders
  }));
  let block = audio_of(&blocks[0]);
  muxer.write_audio(audio_at(&block, 0)).expect("written");
  muxer.finish().expect("finished");
  assert_ne!(
    packets(&output(&corpus, "l28-wavpack.mkv"))[0][0].bytes,
    block.data().as_ref(),
    "read back with the header Matroska rebuilds",
  );
}

//! The FFmpeg muxer: `mediaencode`'s [`Muxer`] face over libavformat.
//!
//! [`FfmpegMuxer`] writes a container — any libavformat names, by its
//! short name ([`ContainerName`]) — into a caller's `Write + Seek`, from
//! the track table a remux collects: one
//! [`TrackDescription`] per track, each carrying the [`CodecTicket`] its
//! codec parameters are rebuilt from, an attachment track carrying its
//! one file. Packets are copied into libavformat, never lent
//! ([`mediaio_ffmpeg::output`]).
//!
//! # Opening: refusals by name, losses in a report
//!
//! [`CarrierMuxer::open`] judges the whole table before it writes a byte.
//! What the container cannot take at all is refused by name, and nothing
//! is written ([`MuxOpenError`]): a codec the muxer says it cannot store,
//! an attachment Matroska could not label, a chapter whose span runs
//! backwards. What the container can take only by dropping something is
//! written, and what it drops is **reported** ([`MuxReport`]): an
//! attachment MP4 has no place for, a language MP4 cannot spell, a
//! disposition flag the container has no field for.
//!
//! The report covers what this release knows of each container — the
//! constants on [`ContainerName`]: Matroska, WebM and MP4. Any other
//! muxer opens the same way and libavformat decides what it writes; an
//! attachment is not handed to it (and is reported), and nothing else is
//! claimed about what it keeps.
//!
//! # Writing: the face's refusals, then the container's
//!
//! Every write keeps the face's rules first — the track exists and is of
//! the packet's kind, it carries the decode time its track is ordered
//! by, which does not go back ([`check_decode_order`]), and it is not
//! presented before it is decoded ([`check_presentation_order`]) — each
//! refused as a [`MuxRefusal`] with the packet handed back. Then the
//! container's own: a container without `AVFMT_TS_NONSTRICT` needs a
//! video or audio stream's decode times strictly increasing, which
//! libavformat would refuse only after taking the packet; this muxer
//! refuses the repeat first, by its own name
//! ([`FfmpegMuxRefusal::RepeatedDts`], in [`MuxError::Backend`]), and
//! hands the packet back.

use core::{fmt, marker::PhantomData};
use std::io::{Seek, Write};

use ffmpeg_next::ffi::AV_NOPTS_VALUE;
use mediaencode::{
  MuxError, MuxRefusal, Muxer, check_decode_order, check_presentation_order,
  mediaspine::{
    Timebase, Timestamp,
    demuxer::{
      AudioTrackPacket, Chapter, DataTrackPacket, SubtitleTrackPacket, TrackDisposition,
      TrackIndex, TrackKind, TrackParams, VideoTrackPacket,
    },
    mediatime::Rounding,
    track::TrackDescription,
  },
};
use mediaio_ffmpeg::{
  CarrierOps, CodecId, CodecTicket, Ffmpeg, FfmpegCarrier, Owned, PacketLimits, View,
  output::{
    CodecQuery, Output, OutputError, OutputPacket, OutputText, PacketTimes, StreamMedia, StreamSpec,
  },
};
use smol_bytes::Utf8Bytes;

/// A libavformat muxer's short name — the container a muxer writes.
///
/// An open set, as libavformat's own is: any name `ffmpeg -f` takes
/// opens. The constants are the containers whose losses this release
/// reports (see [the module](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerName(Utf8Bytes);

impl ContainerName {
  /// Matroska (`.mkv`): every codec Matroska names, attachments with
  /// their filename and MIME type, chapters.
  pub const MATROSKA: Self = Self(Utf8Bytes::from_static("matroska"));
  /// WebM: the Matroska subset for VP8, VP9, AV1, Vorbis, Opus and
  /// WebVTT; no attachments.
  pub const WEBM: Self = Self(Utf8Bytes::from_static("webm"));
  /// MP4 (ISO base media): cover art, chapters; no other attachment.
  pub const MP4: Self = Self(Utf8Bytes::from_static("mp4"));

  /// The muxer libavformat names `name`.
  pub fn new(name: impl Into<Utf8Bytes>) -> Self {
    Self(name.into())
  }

  /// The short name.
  pub fn as_str(&self) -> &str {
    self.0.as_str()
  }

  /// What this release knows of the container.
  fn family(&self) -> Family {
    match self.as_str() {
      "matroska" => Family::Matroska,
      "webm" => Family::WebM,
      "mp4" => Family::Mp4,
      _ => Family::Other,
    }
  }
}

impl fmt::Display for ContainerName {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// The containers this release knows the losses of, from the muxers'
/// source in FFmpeg 9.0.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
  Matroska,
  WebM,
  Mp4,
  Other,
}

/// Something the container could not carry, which the muxer was opened
/// without.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Uncarried {
  /// An attachment the container has no place for — a font in MP4, any
  /// attachment in WebM. Its track is not written.
  Attachment {
    /// The track, in the table the muxer was opened with.
    track: TrackIndex,
  },
  /// A language the container cannot spell: MP4 stores exactly three
  /// letters in `a`–`z` and its neighbours (`movenc.c` 8455–8458,
  /// `isom.c` 233–258 in FFmpeg 9.0.1); anything else is written as
  /// undetermined, or cut to three letters.
  Language {
    /// The track.
    track: TrackIndex,
    /// The tag the track declared.
    tag: Utf8Bytes,
  },
  /// Disposition flags the container has no field for.
  Disposition {
    /// The track.
    track: TrackIndex,
    /// The flags not written.
    flags: TrackDisposition,
  },
}

/// What the container could not carry, reported at open.
///
/// Empty when the container keeps everything the table states, as far
/// as this release knows the container (see [the module](self)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MuxReport {
  uncarried: Vec<Uncarried>,
}

impl MuxReport {
  /// Each thing not carried, in table order.
  pub fn uncarried(&self) -> &[Uncarried] {
    &self.uncarried
  }

  /// Whether everything was carried.
  pub fn is_empty(&self) -> bool {
    self.uncarried.is_empty()
  }
}

/// Where a muxer's fault happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MuxStage {
  /// Opening the muxing context.
  Open,
  /// Adding a track's stream.
  Stream {
    /// The track.
    track: TrackIndex,
  },
  /// Adding a chapter.
  Chapter {
    /// The chapter's position in the table it was opened with.
    chapter: usize,
  },
  /// Writing the header.
  Header,
  /// Writing a track's cover picture, just after the header.
  Cover {
    /// The track.
    track: TrackIndex,
  },
  /// Writing a packet.
  Write {
    /// The track.
    track: TrackIndex,
  },
  /// Writing the trailer.
  Trailer,
  /// Freeing the context and handing the sink back.
  Finish,
}

/// The muxer's own fault: where, and libavformat's — or the sink's —
/// account of it. A fault may come after libavformat took the packet,
/// so a write's fault does not hand it back.
#[derive(Debug, thiserror::Error)]
#[error("{stage:?}: {error}")]
pub struct MuxFault {
  stage: MuxStage,
  #[source]
  error: OutputError,
}

impl MuxFault {
  const fn new(stage: MuxStage, error: OutputError) -> Self {
    Self { stage, error }
  }

  /// Where it happened.
  pub const fn stage(&self) -> MuxStage {
    self.stage
  }

  /// What happened.
  pub const fn error(&self) -> &OutputError {
    &self.error
  }

  /// The panic the caller's sink raised, when that is what this was.
  pub fn sink_panic(&self) -> Option<&Utf8Bytes> {
    match &self.error {
      OutputError::SinkPanic { message } => Some(message),
      _ => None,
    }
  }
}

/// A refusal of the container's own, beyond the face's — the packet
/// handed back in [`MuxError::Backend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FfmpegMuxRefusal {
  /// A decode time equal to the last one written on its stream, in a
  /// container that needs them strictly increasing: libavformat's rule
  /// for every muxer without `AVFMT_TS_NONSTRICT`, on every stream but a
  /// subtitle or data stream, counted in the stream's own time base
  /// (`compute_muxer_pkt_fields`, `libavformat/mux.c` 557–566 in FFmpeg
  /// 9.0.1). The face admits the repeat; this container does not.
  #[error(
    "track {} already holds a packet at decode time {}: this container needs them increasing",
    .track.get(), .dts.pts()
  )]
  RepeatedDts {
    /// The track.
    track: TrackIndex,
    /// The decode time offered.
    dts: Timestamp,
  },
  /// A timestamp the stream's time base cannot count: past `i64` once
  /// rescaled, or — a presentation or decode time — landing on
  /// `i64::MIN`, which is `AV_NOPTS_VALUE`: libavformat reads that count
  /// as no timestamp at all, and would write the packet with timing of
  /// its own making, or refuse it after taking it (`mux.c` 531–572 in
  /// FFmpeg 9.0.1). Refused before libavformat is handed anything, the
  /// packet untouched.
  #[error("track {}: {} cannot be counted in the stream's time base", .track.get(), .timestamp.pts())]
  Unrepresentable {
    /// The track.
    track: TrackIndex,
    /// The timestamp.
    timestamp: Timestamp,
  },
}

/// Why a muxer did not open. Nothing was written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MuxOpenError {
  /// libavformat has no muxer by that name.
  #[error("libavformat has no muxer named {name:?}")]
  UnknownContainer {
    /// The name.
    name: ContainerName,
  },
  /// The muxer writes no byte stream of its own, so it cannot write into
  /// a sink.
  #[error("the {name:?} muxer writes no byte stream")]
  NoByteStream {
    /// The name.
    name: ContainerName,
  },
  /// A track the demuxer could not classify; there is no stream kind to
  /// write it as.
  #[error("track {} is of no kind a muxer can write", .track.get())]
  UnknownTrackKind {
    /// The track.
    track: TrackIndex,
  },
  /// The track's ticket names another codec than its description.
  #[error("track {}'s codec parameters are for another codec", .track.get())]
  TicketMismatch {
    /// The track.
    track: TrackIndex,
  },
  /// A codec the muxer says it cannot store (`avformat_query_codec`).
  #[error("the container cannot store track {}'s codec {codec:?}", .track.get())]
  CodecNotCarried {
    /// The track.
    track: TrackIndex,
    /// The codec.
    codec: CodecId,
  },
  /// An attachment with no filename, which Matroska requires
  /// (`matroskaenc.c` 2538–2543 in FFmpeg 9.0.1).
  #[error("attachment track {} has no filename, which Matroska requires", .track.get())]
  AttachmentWithoutFilename {
    /// The track.
    track: TrackIndex,
  },
  /// An attachment whose MIME type is neither stated nor known for its
  /// codec, which Matroska requires (`matroskaenc.c` 3606–3613).
  #[error("attachment track {} has no MIME type, which Matroska requires", .track.get())]
  AttachmentWithoutMimeType {
    /// The track.
    track: TrackIndex,
  },
  /// A track's text holding a NUL, which a C string cannot carry.
  #[error("track {}'s {text:?} holds a NUL byte", .track.get())]
  TextWithNul {
    /// The track.
    track: TrackIndex,
    /// Which text.
    text: OutputText,
  },
  /// A chapter whose title holds a NUL.
  #[error("chapter {chapter}'s title holds a NUL byte")]
  ChapterTitleWithNul {
    /// The chapter's position in the table.
    chapter: usize,
  },
  /// A chapter that starts before zero or ends before it starts — a
  /// span no container writes (Matroska fails its header on one,
  /// `matroskaenc.c` 2445–2451) — or one its own time base cannot count.
  #[error("chapter {chapter}'s span runs backwards or before zero")]
  ChapterSpan {
    /// The chapter's position in the table.
    chapter: usize,
  },
  /// The muxer's own fault.
  #[error(transparent)]
  Fault(#[from] MuxFault),
}

/// What the muxer keeps of a track while it writes.
#[derive(Debug, Clone, Copy)]
struct TrackState {
  kind: TrackKind,
  /// The stream the track writes to, and libavformat's time base for it
  /// — `None` for a track that takes no packet.
  stream: Option<(usize, Timebase)>,
  /// Whether libavformat's equal-decode-time rule applies.
  strict: bool,
  /// The last decode time written, as an instant.
  last: Option<Timestamp>,
  /// The same, counted in the stream's time base.
  last_ticks: Option<i64>,
}

/// How a track reaches the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Road {
  Timed,
  Attachment,
  Cover,
  NotWritten,
}

/// The write side's two vocabularies of refusal.
enum Refusal {
  Portable(MuxRefusal),
  Container(FfmpegMuxRefusal),
}

impl Refusal {
  fn with_packet<P>(self, packet: P) -> MuxError<MuxFault, FfmpegMuxRefusal, P> {
    match self {
      Self::Portable(refusal) => MuxError::Refused { refusal, packet },
      Self::Container(refusal) => MuxError::Backend { refusal, packet },
    }
  }
}

/// A packet admitted: where it goes and what libavformat is told.
struct Plan {
  track: TrackIndex,
  stream: usize,
  dts: Timestamp,
  times: PacketTimes,
  dts_ticks: i64,
}

/// The FFmpeg muxer, on carrier `C` — see [the module](self).
pub struct CarrierMuxer<W, C> {
  output: Output<W>,
  container: ContainerName,
  tracks: Vec<TrackState>,
  limits: PacketLimits,
  _carrier: PhantomData<fn() -> C>,
}

/// The FFmpeg muxer over the **view** lane's packets — what
/// `mediadecode-ffmpeg`'s `FfmpegDemuxer` delivers.
pub type FfmpegMuxer<W> = CarrierMuxer<W, View>;

/// The FFmpeg muxer over the **owned** lane's packets.
pub type FfmpegOwnedMuxer<W> = CarrierMuxer<W, Owned>;

impl<W, C> CarrierMuxer<W, C>
where
  W: Write + Seek + Send + 'static,
  C: FfmpegCarrier + CarrierOps,
{
  /// Opens a muxer for `container` writing into `sink`, with one track
  /// per description in `tracks` — the track at `TrackIndex::new(i)` is
  /// the `i`-th — and `chapters`.
  ///
  /// Judges the whole table first and writes nothing on a refusal
  /// ([`MuxOpenError`]); then writes the header, an attachment Matroska
  /// keeps with it and any cover picture, and answers what the container
  /// could not carry ([`MuxReport`]). `sink` is dropped with a refusal or
  /// a fault.
  pub fn open(
    sink: W,
    container: ContainerName,
    tracks: Vec<TrackDescription<Ffmpeg, C::Buffer, CodecTicket>>,
    chapters: &[Chapter<Ffmpeg>],
    limits: PacketLimits,
  ) -> Result<(Self, MuxReport), MuxOpenError> {
    let mut output = Output::open(sink, container.as_str()).map_err(|error| match error {
      OutputError::UnknownContainer { .. } | OutputError::Nul { .. } => {
        MuxOpenError::UnknownContainer {
          name: container.clone(),
        }
      }
      OutputError::NoByteStream { .. } => MuxOpenError::NoByteStream {
        name: container.clone(),
      },
      error => MuxOpenError::Fault(MuxFault::new(MuxStage::Open, error)),
    })?;
    let family = container.family();
    let mut report = MuxReport::default();

    // **The whole table is judged before anything is added**, so a
    // refusal leaves nothing written.
    let mut roads = Vec::with_capacity(tracks.len());
    for (index, description) in tracks.iter().enumerate() {
      let track = TrackIndex::new(index);
      roads.push(judge(&output, family, track, description, &mut report)?);
    }
    let mut spans = Vec::with_capacity(chapters.len());
    for (index, chapter) in chapters.iter().enumerate() {
      spans.push(chapter_span(index, chapter)?);
    }

    let strict = output.strict_decode_times();
    let mut states = Vec::with_capacity(tracks.len());
    for (index, (description, road)) in tracks.iter().zip(&roads).enumerate() {
      let track = TrackIndex::new(index);
      let kind = description.kind();
      let stream = match road {
        Road::NotWritten => None,
        road => {
          let spec = stream_spec(description, *road);
          let stream = output
            .add_stream(&spec)
            .map_err(|error| MuxFault::new(MuxStage::Stream { track }, error))?;
          Some(stream)
        }
      };
      states.push((kind, stream, *road));
    }
    for (index, (chapter, (start, end))) in chapters.iter().zip(spans).enumerate() {
      output
        .add_chapter(
          chapter.id(),
          chapter.timebase(),
          start,
          end,
          chapter.title().map(Utf8Bytes::as_str),
        )
        .map_err(|error| MuxFault::new(MuxStage::Chapter { chapter: index }, error))?;
    }
    output
      .write_header()
      .map_err(|error| MuxFault::new(MuxStage::Header, error))?;

    let mut track_states = Vec::with_capacity(states.len());
    for (index, (kind, stream, road)) in states.into_iter().enumerate() {
      let track = TrackIndex::new(index);
      let stream = match (stream, road) {
        (Some(stream), Road::Timed) => {
          let time_base = output
            .stream_time_base(stream)
            .map_err(|error| MuxFault::new(MuxStage::Header, error))?;
          Some((stream, time_base))
        }
        // An attachment stream takes no packet; a cover takes its one,
        // here, and nothing after it. (A description of an attachment
        // always carries its file; the guard is the type's, not ours.)
        (Some(stream), Road::Cover) => {
          if let Some(file) = tracks[index].file() {
            output
              .write_packet::<C>(
                stream,
                OutputPacket::Cover(file.payload()),
                PacketTimes::new(Some(0), Some(0), 0),
                limits,
              )
              .map_err(|error| MuxFault::new(MuxStage::Cover { track }, error))?;
          }
          None
        }
        _ => None,
      };
      track_states.push(TrackState {
        kind,
        stream,
        strict: strict && !matches!(kind, TrackKind::Subtitle | TrackKind::Data),
        last: None,
        last_ticks: None,
      });
    }

    Ok((
      Self {
        output,
        container,
        tracks: track_states,
        limits,
        _carrier: PhantomData,
      },
      report,
    ))
  }

  /// The container this muxer writes.
  pub const fn container(&self) -> &ContainerName {
    &self.container
  }

  /// Completes the container and hands the sink back — [`Muxer::finish`],
  /// keeping the sink.
  pub fn finish_into_sink(mut self) -> Result<W, MuxFault> {
    self
      .output
      .write_trailer()
      .map_err(|error| MuxFault::new(MuxStage::Trailer, error))?;
    self
      .output
      .into_sink()
      .map_err(|error| MuxFault::new(MuxStage::Finish, error))
  }

  /// Judges a timed packet: the face's rules, then the container's.
  fn admit(
    &self,
    track: TrackIndex,
    offered: TrackKind,
    pts: Option<Timestamp>,
    dts: Option<Timestamp>,
    duration: Option<Timestamp>,
  ) -> Result<Plan, Refusal> {
    let state = self
      .tracks
      .get(track.get())
      .ok_or(Refusal::Portable(MuxRefusal::UnknownTrack { track }))?;
    if state.kind != offered {
      return Err(Refusal::Portable(MuxRefusal::KindMismatch {
        track,
        expected: state.kind,
        offered,
      }));
    }
    let dts = dts.ok_or(Refusal::Portable(MuxRefusal::MissingDts { track }))?;
    check_decode_order(track, state.last, dts).map_err(Refusal::Portable)?;
    check_presentation_order(track, pts, dts).map_err(Refusal::Portable)?;
    // A timed track of a known kind always has its stream: only an
    // attachment track takes none, and its kind is refused above.
    let Some((stream, time_base)) = state.stream else {
      return Err(Refusal::Portable(MuxRefusal::KindMismatch {
        track,
        expected: state.kind,
        offered,
      }));
    };
    // Each time counted in the stream's time base, as libavformat is
    // handed it — or refused by name, the packet untouched.
    let unrepresentable =
      |timestamp: Timestamp| Refusal::Container(FfmpegMuxRefusal::Unrepresentable { track, timestamp });
    let count = |timestamp: Timestamp| {
      timestamp
        .checked_rescale_with(time_base, Rounding::Nearest)
        .map(|rescaled| rescaled.pts())
        .ok_or_else(|| unrepresentable(timestamp))
    };
    // A presentation or decode time may not land on `AV_NOPTS_VALUE`, the
    // count libavformat reads as no timestamp at all. A duration has no
    // such count: libavformat zeroes a negative one (`mux.c` 607–611).
    let stamp = |timestamp: Timestamp| -> Result<i64, Refusal> {
      match count(timestamp)? {
        ticks if ticks == AV_NOPTS_VALUE => Err(unrepresentable(timestamp)),
        ticks => Ok(ticks),
      }
    };
    let dts_ticks = stamp(dts)?;
    let pts_ticks = pts.map(stamp).transpose()?;
    let duration_ticks = duration.map(count).transpose()?.unwrap_or(0);
    // libavformat's rule, exactly: nothing is checked while the last
    // decode time is zero — its counter starts there — and a repeat is
    // refused after any other.
    if state.strict && matches!(state.last_ticks, Some(last) if last != 0 && last >= dts_ticks) {
      return Err(Refusal::Container(FfmpegMuxRefusal::RepeatedDts {
        track,
        dts,
      }));
    }
    Ok(Plan {
      track,
      stream,
      dts,
      times: PacketTimes::new(pts_ticks, Some(dts_ticks), duration_ticks),
      dts_ticks,
    })
  }

  /// Records a written packet, or names the fault.
  fn settle<P>(
    &mut self,
    plan: Plan,
    written: Result<(), OutputError>,
  ) -> Result<(), MuxError<MuxFault, FfmpegMuxRefusal, P>> {
    written.map_err(|error| {
      MuxError::Fault(MuxFault::new(MuxStage::Write { track: plan.track }, error))
    })?;
    let state = &mut self.tracks[plan.track.get()];
    state.last = Some(plan.dts);
    state.last_ticks = Some(plan.dts_ticks);
    Ok(())
  }
}

impl<W, C> Muxer for CarrierMuxer<W, C>
where
  W: Write + Seek + Send + 'static,
  C: FfmpegCarrier + CarrierOps,
{
  type Adapter = Ffmpeg;
  type Buffer = C::Buffer;
  type Refusal = FfmpegMuxRefusal;
  type Error = MuxFault;

  fn write_video(
    &mut self,
    packet: VideoTrackPacket<Ffmpeg, C::Buffer>,
  ) -> Result<(), MuxError<MuxFault, FfmpegMuxRefusal, VideoTrackPacket<Ffmpeg, C::Buffer>>> {
    let p = packet.packet();
    let plan = match self.admit(
      packet.track(),
      TrackKind::Video,
      p.pts(),
      p.dts(),
      p.duration(),
    ) {
      Ok(plan) => plan,
      Err(refusal) => return Err(refusal.with_packet(packet)),
    };
    let written =
      self
        .output
        .write_packet::<C>(plan.stream, OutputPacket::Video(p), plan.times, self.limits);
    self.settle(plan, written)
  }

  fn write_audio(
    &mut self,
    packet: AudioTrackPacket<Ffmpeg, C::Buffer>,
  ) -> Result<(), MuxError<MuxFault, FfmpegMuxRefusal, AudioTrackPacket<Ffmpeg, C::Buffer>>> {
    let p = packet.packet();
    let plan = match self.admit(
      packet.track(),
      TrackKind::Audio,
      p.pts(),
      p.dts(),
      p.duration(),
    ) {
      Ok(plan) => plan,
      Err(refusal) => return Err(refusal.with_packet(packet)),
    };
    let written =
      self
        .output
        .write_packet::<C>(plan.stream, OutputPacket::Audio(p), plan.times, self.limits);
    self.settle(plan, written)
  }

  // A subtitle or data packet is decoded when it is presented.
  fn write_subtitle(
    &mut self,
    packet: SubtitleTrackPacket<Ffmpeg, C::Buffer>,
  ) -> Result<(), MuxError<MuxFault, FfmpegMuxRefusal, SubtitleTrackPacket<Ffmpeg, C::Buffer>>> {
    let p = packet.packet();
    let plan = match self.admit(
      packet.track(),
      TrackKind::Subtitle,
      p.pts(),
      p.pts(),
      p.duration(),
    ) {
      Ok(plan) => plan,
      Err(refusal) => return Err(refusal.with_packet(packet)),
    };
    let written = self.output.write_packet::<C>(
      plan.stream,
      OutputPacket::Subtitle(p),
      plan.times,
      self.limits,
    );
    self.settle(plan, written)
  }

  fn write_data(
    &mut self,
    packet: DataTrackPacket<Ffmpeg, C::Buffer>,
  ) -> Result<(), MuxError<MuxFault, FfmpegMuxRefusal, DataTrackPacket<Ffmpeg, C::Buffer>>> {
    let p = packet.packet();
    let plan = match self.admit(
      packet.track(),
      TrackKind::Data,
      p.pts(),
      p.pts(),
      p.duration(),
    ) {
      Ok(plan) => plan,
      Err(refusal) => return Err(refusal.with_packet(packet)),
    };
    let written =
      self
        .output
        .write_packet::<C>(plan.stream, OutputPacket::Data(p), plan.times, self.limits);
    self.settle(plan, written)
  }

  fn finish(self) -> Result<(), MuxFault> {
    self.finish_into_sink().map(drop)
  }
}

impl<W, C> fmt::Debug for CarrierMuxer<W, C> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CarrierMuxer")
      .field("container", &self.container)
      .field("tracks", &self.tracks.len())
      .field("output", &self.output)
      .finish_non_exhaustive()
  }
}

/// Judges one track for `family`: refused by name, or the road it takes
/// and what it loses, into `report`.
fn judge<W, D: AsRef<[u8]>>(
  output: &Output<W>,
  family: Family,
  track: TrackIndex,
  description: &TrackDescription<Ffmpeg, D, CodecTicket>,
  report: &mut MuxReport,
) -> Result<Road, MuxOpenError> {
  let codec = description.codec();
  if codec.raw() != description.side_data().codec_id() {
    return Err(MuxOpenError::TicketMismatch { track });
  }
  let nul = |text: Option<&Utf8Bytes>, which: OutputText| match text {
    Some(text) if text.as_str().contains('\0') => {
      Err(MuxOpenError::TextWithNul { track, text: which })
    }
    _ => Ok(()),
  };
  nul(description.language(), OutputText::Language)?;

  let road = match description.kind() {
    TrackKind::Unknown => return Err(MuxOpenError::UnknownTrackKind { track }),
    TrackKind::Attachment => match family {
      Family::Matroska => {
        let file = description.file();
        let filename = file.and_then(|file| file.filename());
        if filename.is_none() {
          return Err(MuxOpenError::AttachmentWithoutFilename { track });
        }
        nul(filename, OutputText::Filename)?;
        let stated = file.and_then(|file| file.mime_type());
        nul(stated, OutputText::MimeType)?;
        if stated.is_none() && matroska_mime_type(codec).is_none() {
          return Err(MuxOpenError::AttachmentWithoutMimeType { track });
        }
        Road::Attachment
      }
      Family::Mp4 if is_cover_codec(codec) => Road::Cover,
      _ => {
        report.uncarried.push(Uncarried::Attachment { track });
        Road::NotWritten
      }
    },
    _ => {
      if output.query_codec(codec) == CodecQuery::NotCarried {
        return Err(MuxOpenError::CodecNotCarried { track, codec });
      }
      let dropped = disposition_dropped(family, codec, description.disposition());
      if !dropped.is_empty() {
        report.uncarried.push(Uncarried::Disposition {
          track,
          flags: dropped,
        });
      }
      Road::Timed
    }
  };
  if let Some(tag) = description.language()
    && road != Road::NotWritten
    && !language_carried(family, tag.as_str())
  {
    report.uncarried.push(Uncarried::Language {
      track,
      tag: tag.clone(),
    });
  }
  Ok(road)
}

/// The stream a track becomes, on its road.
fn stream_spec<'a, D: AsRef<[u8]>>(
  description: &'a TrackDescription<Ffmpeg, D, CodecTicket>,
  road: Road,
) -> StreamSpec<'a> {
  let ticket = description.side_data();
  let spec = StreamSpec::new(ticket, description.timebase())
    .with_disposition(description.disposition().bits() as i32)
    .with_language(description.language().map(Utf8Bytes::as_str))
    .with_extradata(description.extradata().map(|bytes| bytes.as_ref()));
  let spec = match description.params() {
    TrackParams::Video(params) => spec.with_frame_rate(params.frame_rate()),
    _ => spec,
  };
  match road {
    Road::Attachment => {
      let file = description.file();
      let mime_type = file
        .and_then(|file| file.mime_type())
        .map(Utf8Bytes::as_str)
        .or_else(|| matroska_mime_type(description.codec()));
      spec
        .with_media(StreamMedia::Attachment)
        .with_extradata(file.map(|file| file.payload().data().as_ref()))
        .with_filename(file.and_then(|file| file.filename()).map(Utf8Bytes::as_str))
        .with_mime_type(mime_type)
    }
    Road::Cover => spec.with_media(StreamMedia::Cover),
    Road::Timed | Road::NotWritten => spec,
  }
}

/// The MIME type Matroska writes for an attachment that states none: its
/// codec's, or `text/plain` for plain text (`get_mimetype`,
/// `matroskaenc.c` 2496–2511 in FFmpeg 9.0.1).
fn matroska_mime_type(codec: CodecId) -> Option<&'static str> {
  match codec.mime_type() {
    Some(mime) => Some(mime),
    None if codec == CodecId::TEXT => Some("text/plain"),
    None => None,
  }
}

/// The still-image codecs MP4 keeps as cover art (`movenc.c` 2167–2172
/// in FFmpeg 9.0.1).
fn is_cover_codec(codec: CodecId) -> bool {
  codec == CodecId::MJPEG || codec == CodecId::PNG || codec == CodecId::BMP
}

/// Whether `family` writes the language `tag` as it is.
fn language_carried(family: Family, tag: &str) -> bool {
  match family {
    // MP4 packs three five-bit letters: exactly three bytes, each in
    // 0x60..=0x7F (`ff_mov_iso639_to_lang`, `isom.c` 233–258). Anything
    // else is written as undetermined, or cut to its first three.
    Family::Mp4 => {
      let bytes = tag.as_bytes();
      bytes.len() == 3 && bytes.iter().all(|byte| (0x60..=0x7F).contains(byte))
    }
    // Matroska and WebM write the tag as given (`matroskaenc.c`
    // 1967–1969); for any other container nothing is claimed.
    Family::Matroska | Family::WebM | Family::Other => true,
  }
}

/// The disposition flags `family` has no field for.
fn disposition_dropped(
  family: Family,
  codec: CodecId,
  flags: TrackDisposition,
) -> TrackDisposition {
  use TrackDisposition as D;
  let kept = match family {
    // `matroskaenc.c` 1971–2023, 2157: the default and forced flags, the
    // commentary, impairment and text-description flags, and original or
    // dub — one of them; both at once write neither.
    Family::Matroska => {
      let mut kept = D::DEFAULT
        | D::FORCED
        | D::COMMENT
        | D::HEARING_IMPAIRED
        | D::VISUAL_IMPAIRED
        | D::DESCRIPTIONS;
      if !flags.contains(D::ORIGINAL | D::DUB) {
        kept |= D::ORIGINAL | D::DUB;
      }
      kept
    }
    // WebM writes the default and forced flags, and a WebVTT track's
    // kind through its codec id — the first of captions, descriptions or
    // metadata it declares (`matroskaenc.c` 1979–2002).
    Family::WebM => {
      let mut kept = D::DEFAULT | D::FORCED;
      if codec == CodecId::WEBVTT
        && let Some(kind) = [D::CAPTIONS, D::DESCRIPTIONS, D::METADATA]
          .into_iter()
          .find(|kind| flags.contains(*kind))
      {
        kept |= kind;
      }
      kept
    }
    // The track's enabled flag and the DASH role `kind` boxes
    // (`movenc.c` 4338–4385, `isom.c` 450–462).
    Family::Mp4 => {
      D::DEFAULT
        | D::HEARING_IMPAIRED
        | D::CAPTIONS
        | D::COMMENT
        | D::VISUAL_IMPAIRED
        | D::DESCRIPTIONS
        | D::DUB
        | D::FORCED
    }
    Family::Other => return D::empty(),
  };
  flags.difference(kept)
}

/// A chapter's span in its own time base, or a refusal naming it.
fn chapter_span(index: usize, chapter: &Chapter<Ffmpeg>) -> Result<(i64, i64), MuxOpenError> {
  if chapter
    .title()
    .is_some_and(|title| title.as_str().contains('\0'))
  {
    return Err(MuxOpenError::ChapterTitleWithNul { chapter: index });
  }
  let count = |timestamp: Timestamp| {
    timestamp
      .checked_rescale_with(chapter.timebase(), Rounding::Nearest)
      .map(|rescaled| rescaled.pts())
  };
  match (count(chapter.start()), count(chapter.end())) {
    (Some(start), Some(end)) if start >= 0 && end >= start => Ok((start, end)),
    _ => Err(MuxOpenError::ChapterSpan { chapter: index }),
  }
}

#[cfg(test)]
mod tests;

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
//! The report follows each track's **road**, as the roads keep different
//! fields: a timed stream keeps its language and the flags its container
//! has fields for; an attachment Matroska writes keeps its file, filename,
//! MIME type and language, but no flag; MP4's cover keeps its picture
//! alone, and reads back as an attached picture — its filename, MIME
//! type, language and other flags are each reported
//! ([`Uncarried::Filename`], [`Uncarried::MimeType`]).
//!
//! The report covers what this release knows of each container — the
//! constants on [`ContainerName`]: Matroska, WebM and MP4. Any other
//! muxer opens the same way and libavformat decides what it writes; an
//! attachment is not handed to it (and is reported), and nothing else is
//! claimed about what it keeps.
//!
//! # Writing: the face's refusals, then the container's
//!
//! Every write keeps the face's rules — the track exists and is of the
//! packet's kind, it carries the decode time its track is ordered by,
//! which does not go back ([`check_decode_order`]), and it is not
//! presented before it is decoded — each refused as a [`MuxRefusal`]
//! with the packet handed back. The presentation rule is judged as
//! libavformat judges it, on the counts it is handed in the stream's own
//! time base ([`check_presentation_order`] over those counts), so two
//! times a hair apart that count to one tick are written, as a stream
//! copy writes them; its refusal names the times the packet carries.
//! The counts come first: a time the stream cannot count — past `i64`,
//! or landing on libavformat's no-timestamp value — is refused by the
//! muxer's own name ([`FfmpegMuxRefusal::Unrepresentable`], in
//! [`MuxError::Backend`]). Then the container's own rule: a container
//! without `AVFMT_TS_NONSTRICT` needs a video or audio stream's decode
//! times strictly increasing, which libavformat would refuse only after
//! taking the packet; this muxer refuses the repeat first, by its own
//! name ([`FfmpegMuxRefusal::RepeatedDts`]), and hands the packet back.
//!
//! # The packet-rule table
//!
//! libavformat interleaves the packets it is handed before its muxer sees
//! them, so a muxer that refuses a packet does so after taking it, and
//! often while writing another. This muxer accounts for every rule
//! libavformat applies on its packet path in the containers it knows —
//! the generic checks and timestamp computation (`libavformat/mux.c`),
//! MP4's (`movenc.c`) and Matroska's and WebM's (`matroskaenc.c`), at
//! FFmpeg 9.0.1 — and maps each to exactly one of three:
//!
//! - **refused by name**, before libavformat is handed anything, the
//!   packet untouched: a timing rule as a [`FfmpegMuxRefusal`] variant,
//!   a rule over the packet's bytes or side data as
//!   [`FfmpegMuxRefusal::Payload`] and its [`PayloadRule`], and a track
//!   the container cannot write at all as a [`MuxOpenError`] at open;
//! - **reported** at open, where the container writes a track's packets
//!   in its own form ([`Uncarried::Rewritten`] and its [`Rewrite`]);
//! - **passed through**, written and read back as handed over.
//!
//! The automatic bitstream filters are off in these containers, so no
//! filter rewrites a packet before its container sees it
//! ([`Output::set_automatic_filters`]). What a packet's side data becomes
//! is the container's: each writes the kinds it has a place for and no
//! other. Another container is opened the same way and keeps the generic
//! rules — duration, key frame and decode order, and libavformat's shift
//! of early times — and nothing else is claimed of it.

use core::{fmt, marker::PhantomData};
use std::io::{Seek, Write};

use ffmpeg_next::ffi::{AV_NOPTS_VALUE, AVPacketSideDataType};
use mediaencode::{
  MuxError, MuxRefusal, Muxer, check_decode_order, check_presentation_order,
  mediaspine::{
    Timebase, Timestamp,
    demuxer::{
      AudioTrackPacket, Chapter, DataTrackPacket, SubtitleTrackPacket, TrackDisposition,
      TrackIndex, TrackKind, TrackParams, VideoTrackPacket,
    },
    mediatime::Rounding,
    packet::PacketFlags,
    track::TrackDescription,
  },
};
use mediaio_ffmpeg::{
  CarrierOps, CodecId, CodecTicket, Ffmpeg, FfmpegCarrier, Owned, PacketLimits, View,
  extras::SideDataEntry,
  output::{
    CodecQuery, Output, OutputError, OutputPacket, OutputText, PacketTimes, StreamCodec,
    StreamMedia, StreamSpec,
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
  /// An attachment's filename its road does not keep: MP4 keeps a
  /// cover's picture and the type of its image, and nothing else
  /// (`mov_write_covr`, `movenc.c` 4815–4839 in FFmpeg 9.0.1).
  Filename {
    /// The track.
    track: TrackIndex,
    /// The filename the attachment declared.
    filename: Utf8Bytes,
  },
  /// An attachment's MIME type its road does not keep — MP4's cover, as
  /// for its filename.
  MimeType {
    /// The track.
    track: TrackIndex,
    /// The MIME type the attachment declared.
    mime_type: Utf8Bytes,
  },
  /// A language the track's road does not write so that it reads back as
  /// itself — the report table's language rule. Matroska and WebM write
  /// an empty tag as `und` (`matroskaenc.c` 1967–1969 in FFmpeg 9.0.1) and
  /// read `und` back as none (`matroskadec.c` 3280–3281). MP4 keeps only a
  /// code that packs and unpacks to itself: exactly three bytes in
  /// `0x60..=0x7F`, the first not `0x60`, which packs below `0x400` and
  /// unpacks through the Macintosh table, and not all three `0x7F`, which
  /// packs to "unspecified" (`isom.c` 233–280; `movenc.c` 8455–8458).
  /// MP4's cover, which no track carries, keeps none.
  Language {
    /// The track.
    track: TrackIndex,
    /// The tag the track declared.
    tag: Utf8Bytes,
  },
  /// Disposition flags the container has no field for on the track's
  /// road. An attachment Matroska writes has no track entry, and so no
  /// flag (`matroskaenc.c` 1957–1958); MP4's cover reads back as an
  /// attached picture and nothing more.
  Disposition {
    /// The track.
    track: TrackIndex,
    /// The flags not written.
    flags: TrackDisposition,
  },
  /// Packets the container rewrites on their way into the file: what it
  /// writes is the container's form of a packet, not the bytes offered —
  /// the packet-rule table's reported rows (see [the module](self)).
  Rewritten {
    /// The track.
    track: TrackIndex,
    /// What the container does to the track's packets.
    rewrite: Rewrite,
  },
}

/// What a container does to a track's packets on their way into the
/// file, reported at open ([`Uncarried::Rewritten`]) — each from the
/// muxer's source in FFmpeg 9.0.1.
///
/// Marked `#[non_exhaustive]`: a rewrite a later release learns of joins
/// as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Rewrite {
  /// H.264, HEVC, VVC or LCEVC whose configuration is Annex B: each
  /// packet's start codes are rewritten as length prefixes, and MP4 drops
  /// an HEVC packet's parameter sets under `hvc1` (`movenc.c` 7087–7158;
  /// `matroskaenc.c` 3574–3582).
  LengthPrefixed,
  /// AV1: each packet's temporal delimiters, redundant frame headers,
  /// tile lists and padding are dropped (`av1.c` 33–81; `movenc.c`
  /// 7159–7174; `matroskaenc.c` 3589–3590).
  ObusFiltered,
  /// MP4 AC-3 and E-AC-3: E-AC-3 frames of fewer than six blocks are
  /// joined into samples of six, and the blocks of a set the stream leaves
  /// incomplete at its end are dropped (`handle_eac3`, `movenc.c` 577–606;
  /// `mov_free`, 7951–7955). A packet MP4 would refuse or drop is refused
  /// by name ([`PayloadRule::Ac3Header`] and its neighbours).
  Ac3Parsed,
  /// MP4 `mov_text`: an empty sample is written at the end of each cue a
  /// later packet passes, and at zero before the first, each read back as
  /// a cue of its own (`movenc.c` 7625–7656).
  CueEnds,
  /// MP4 TTML from libavcodec's TTML encoder: the track's paragraphs are
  /// joined into one sample (`movenc.c` 7658–7684, 8596–8600).
  ParagraphsJoined,
  /// MP4 VC-1 and TrueHD: a sample's sync flag is read from its bitstream,
  /// not from the packet's key flag (`movenc.c` 7344–7348).
  SyncFromBitstream,
  /// A packet's duration is not kept as given. MP4 keeps a subtitle's or
  /// data packet's other than `mov_text`'s only as the distance to the
  /// next one's decode time, the last one's from its packet (`movenc.c`
  /// 7321; `stts`, 3241). Matroska and WebM keep a video or audio packet's only
  /// where it differs from the track's default duration
  /// (`mkv_write_block`, `matroskaenc.c` 2895–2911), and a video track
  /// without a frame rate, or an audio track whose codec fixes no frame's
  /// samples, has none (1933–1940, 2058–2065, 2119–2124).
  Durations,
  /// Matroska WavPack: each block's header is stripped to the fields
  /// Matroska keeps (`matroskaenc.c` 2775–2816, 3571–3572).
  WavPackHeaders,
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
  /// Each thing not carried, in table order — and within a track, field
  /// by field: an attachment's filename, its MIME type, the language, the
  /// disposition.
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
  /// A negative duration: libavformat writes zero in its place
  /// (`mux.c` 607–611 in FFmpeg 9.0.1), Matroska does for a subtitle too
  /// (`matroskaenc.c` 3134), and MP4 refuses a subtitle's after taking the
  /// packet (`movenc.c` 6925–6928).
  #[error("track {}: the duration {} is negative", .track.get(), .duration.pts())]
  NegativeDuration {
    /// The track.
    track: TrackIndex,
    /// The duration offered.
    duration: Timestamp,
  },
  /// MP4: a duration of more than `i32::MAX` ticks of the stream's time
  /// base, which MP4 refuses after taking the packet (`movenc.c`
  /// 6925–6928).
  #[error("track {}: the duration {} is longer than MP4 stores", .track.get(), .duration.pts())]
  DurationTooLong {
    /// The track.
    track: TrackIndex,
    /// The duration offered.
    duration: Timestamp,
  },
  /// MP4: a decode time `i32::MAX` ticks or more after the track's last:
  /// MP4 would rewrite it to one tick after the last and drop the
  /// presentation time, and write the packet so (`check_pkt`, `movenc.c`
  /// 6902–6923).
  #[error("track {}: the decode time {} lies too far after the last for MP4", .track.get(), .dts.pts())]
  DtsGap {
    /// The track.
    track: TrackIndex,
    /// The decode time offered.
    dts: Timestamp,
  },
  /// MP4: a presentation more than `i32::MAX` ticks after its decode,
  /// beyond MP4's 32-bit composition offset (`movenc.c` 6955–6959; the
  /// offset is an `int`, `movenc.h` 58).
  #[error(
    "track {}: presented at {}, decoded at {}: too far apart for MP4",
    .track.get(), .pts.pts(), .dts.pts()
  )]
  PresentationOffset {
    /// The track.
    track: TrackIndex,
    /// The presentation time offered.
    pts: Timestamp,
    /// The decode time offered.
    dts: Timestamp,
  },
  /// No presentation time on a stream that reorders — its parameters'
  /// `video_delay` above zero: libavformat makes none up there
  /// (`mux.c` 531–532), and MP4 then presents the packet at its decode
  /// time (`movenc.c` 7324–7327) while Matroska and WebM refuse it after
  /// taking it (`matroskaenc.c` 3137–3142).
  #[error("track {} reorders, and the packet has no presentation time", .track.get())]
  MissingPts {
    /// The track.
    track: TrackIndex,
  },
  /// A time before the earliest the container writes without moving
  /// every stream: libavformat shifts every stream's times by the first
  /// packet's distance below its stream's earliest (`mux.c` 636–717). In
  /// Matroska and WebM that is a presentation time below zero, or below an
  /// audio track's codec delay before zero (`matroskaenc.c` 2089–2104,
  /// 3538–3541); in another container without `AVFMT_TS_NEGATIVE` a decode
  /// time below zero. MP4 takes negative times as they are.
  #[error("track {}: {} lies before the earliest time the container writes", .track.get(), .timestamp.pts())]
  BeforeZero {
    /// The track.
    track: TrackIndex,
    /// The time offered — a presentation time, or a decode time.
    timestamp: Timestamp,
  },
  /// MP4 `mov_text`: a cue before zero, or before the previous cue ends:
  /// MP4 marks each cue's end with an empty sample, counted from the
  /// track's first, and rewrites a cue before the mark — its decode time
  /// moved past it, its presentation time dropped (`movenc.c` 7625–7656,
  /// 6902–6923).
  #[error("track {}: a cue at {} starts before zero or before the last one ends", .track.get(), .pts.pts())]
  CueOrder {
    /// The track.
    track: TrackIndex,
    /// The cue's time.
    pts: Timestamp,
  },
  /// A packet of a codec every frame of which is a key frame, not marked
  /// one: libavformat would mark it (`mux.c` 820–821;
  /// [`CodecId::intra_only`]). Not in Matroska or WebM for a subtitle
  /// track, whose every block they write as a key frame regardless
  /// (`matroskaenc.c` 3132–3133).
  #[error("track {}: a packet of a codec of key frames alone is not marked one", .track.get())]
  KeyFrameUnmarked {
    /// The track.
    track: TrackIndex,
  },
  /// A packet whose bytes the container would refuse after taking it, or
  /// write otherwise than they are ([`PayloadRule`]).
  #[error("track {}: {rule}", .track.get())]
  Payload {
    /// The track.
    track: TrackIndex,
    /// What about its bytes.
    rule: PayloadRule,
  },
}

/// What about a packet's bytes [`FfmpegMuxRefusal::Payload`] refuses — each
/// a rule of the muxer's in FFmpeg 9.0.1.
///
/// Marked `#[non_exhaustive]`: a rule a later release learns of joins as a
/// variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PayloadRule {
  /// No bytes: MP4 drops an empty packet (`movenc.c` 7431–7441), and
  /// Matroska and WebM an empty audio packet (`matroskaenc.c` 3262–3268).
  #[error("the packet is empty, and the container drops it")]
  Empty,
  /// AAC framed in ADTS headers, into MP4 or Matroska, which take raw AAC:
  /// MP4 refuses the first such packet after taking it and writes the
  /// rest malformed (`movenc.c` 7076–7086); Matroska writes them as they
  /// come into a track that declares raw AAC. The automatic filter that
  /// would strip them is off, so that every packet is written as offered.
  #[error("the packet is AAC in an ADTS header, and the container takes raw AAC")]
  AdtsHeader,
  /// MP4 PCM: not a whole number of frames — channels times bytes per
  /// sample. MP4 refuses a packet holding none after taking it, and counts
  /// only the whole frames of another, losing the rest (`movenc.c`
  /// 6999–7009, 8538–8560).
  #[error("the packet is not a whole number of the codec's frames")]
  PartialSample,
  /// AV1 whose OBUs do not parse: MP4 writes a sample of a negative size,
  /// and Matroska refuses the packet after taking it (`av1.c` 33–81, over
  /// libavcodec's `parse_obu_header`, `av1_parse.h` 92–134).
  #[error("the packet's OBUs do not parse")]
  Obus,
  /// Matroska WavPack whose blocks do not parse whole: Matroska refuses
  /// the packet after taking it, and drops bytes left over after the last
  /// whole block (`matroskaenc.c` 2775–2816; `wv.c` 30–53).
  #[error("the packet's WavPack blocks do not parse whole")]
  WavPackBlocks,
  /// MP4: skip-samples side data that would shorten the packet below
  /// nothing, which MP4 refuses after taking it (`movenc.c` 7311–7320).
  #[error("the packet's skip-samples side data leaves less than nothing")]
  SkipSamples,
  /// Matroska FLAC: new STREAMINFO side data of another length than the
  /// track's, which Matroska refuses after taking the packet
  /// (`matroskaenc.c` 3085–3093).
  #[error("the packet's new FLAC STREAMINFO is not the length of the track's")]
  FlacStreamInfo,
  /// Matroska ProRes not opening on its frame's atom — its size, the
  /// packet's, and `icpf` — or holding a second one where the first ends:
  /// Matroska drops a packet's first 8 bytes (`matroskaenc.c` 3584–3587,
  /// 695–696), and its reader puts an atom back only where none follows
  /// (`matroskadec.c` 3870–3889, 4176–4178), so such a packet reads back
  /// otherwise than it was written.
  #[error("the ProRes packet does not open on its own frame atom")]
  ProResAtom,
  /// Matroska and WebM WebVTT: a cue whose text ends in a line break, or
  /// whose identifier or settings hold one or are given empty. They are
  /// written on lines of their own (`webm_reformat_vtt`, `matroskaenc.c`
  /// 2828–2855) and read back by line, the text's last line breaks dropped
  /// and an empty identifier or settings not given (`matroska_parse_webvtt`,
  /// `matroskadec.c` 3890–3990).
  #[error("the WebVTT cue does not read back from its lines as itself")]
  WebVttCue,
  /// MP4 AC-3 and E-AC-3: a packet whose first frame's header libavcodec's
  /// parser refuses (`ff_ac3_parse_header`, `ac3_parser.c` 288–395 in
  /// FFmpeg 9.0.1). MP4 drops it at the stream's start and refuses it,
  /// after taking it, later (`handle_eac3`, `movenc.c` 481–493).
  #[error("the packet's AC-3 frame header does not parse")]
  Ac3Header,
  /// MP4 E-AC-3: a track's first packet whose frames after its first are
  /// not dependent frames that tile it, which MP4 refuses after taking it
  /// (`movenc.c` 551–575).
  #[error("the packet's E-AC-3 frames after the first are not dependent frames that tile it")]
  Ac3Substreams,
  /// MP4 E-AC-3: a frame whose blocks would take the sample MP4 is making
  /// of frames of fewer than six blocks past six, which MP4 refuses after
  /// taking it (`movenc.c` 577–585).
  #[error("the packet's E-AC-3 blocks run past a sample of six")]
  Ac3Blocks,
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
  /// A codec whose configuration — its extradata — the container needs and
  /// the track does not carry: MP4 would take an H.264, HEVC, VVC or LCEVC
  /// track's first packet as it and read every packet by it (`movenc.c`
  /// 7027–7041, 7087–7158); Matroska and WebM refuse every AV1 packet of a
  /// track with none (`matroskaenc.c` 3098–3109), and Matroska every AAC
  /// packet of one (3080–3083).
  #[error("track {} carries no configuration for {codec:?}, which this container needs", .track.get())]
  ConfigurationMissing {
    /// The track.
    track: TrackIndex,
    /// The codec.
    codec: CodecId,
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
  /// The rules the container's packet path holds this track's packets
  /// to (see [the module](self)).
  rules: PacketRules,
  /// MP4 `mov_text`: where the last cue written ends, in ticks.
  cue_end: Option<i128>,
  /// MP4 AC-3 and E-AC-3: what MP4 has learned of the track's frames.
  ac3: Ac3State,
}

/// The rows of the packet-rule table one track's packets meet in its
/// container, settled at open from the container and the stream's own
/// parameters ([`PacketRules::settle`]); each is refused by name, the
/// packet untouched, before libavformat is handed anything.
#[derive(Debug, Clone, Copy, Default)]
struct PacketRules {
  /// MP4's `check_pkt` and composition-offset rules: a decode time at
  /// least `i32::MAX` ticks after the last, a duration past `i32::MAX`,
  /// a presentation more than `i32::MAX` after its decode.
  mp4: bool,
  /// The earliest time the container writes without moving every stream.
  earliest: Option<Earliest>,
  /// An empty packet is dropped.
  no_empty: bool,
  /// The stream reorders, and the container makes up or refuses a
  /// missing presentation time.
  needs_pts: bool,
  /// AAC must be raw: no ADTS header.
  raw_aac: bool,
  /// Every packet of the codec is a key frame.
  intra_only: bool,
  /// MP4 PCM: the bytes of one frame.
  frame_bytes: Option<u32>,
  /// AV1: the OBUs are parsed.
  obus: bool,
  /// Matroska WavPack: the blocks are parsed.
  wavpack: bool,
  /// MP4 `mov_text`: cues in order, from zero.
  cues_in_order: bool,
  /// MP4: skip-samples side data is applied, with the codec's frame size
  /// and sample rate.
  skip_samples: Option<(i32, i32)>,
  /// Matroska FLAC: the STREAMINFO length new extradata must keep.
  streaminfo_len: Option<usize>,
  /// Matroska ProRes: each packet opens on its frame's atom.
  prores: bool,
  /// Matroska and WebM WebVTT: each cue reads back from its lines.
  webvtt: bool,
  /// MP4 AC-3 (`Some(false)`) and E-AC-3 (`Some(true)`): each packet is one
  /// MP4's AC-3 handling writes.
  ac3: Option<bool>,
}

/// The earliest time a container writes without moving every stream, in
/// the stream's ticks, and which of a packet's times it judges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Earliest {
  /// Matroska and WebM judge the presentation time (`matroskaenc.c`
  /// 3538–3541) — the decode time where a packet states none, as
  /// libavformat fills it (`mux.c` 531–532).
  Presentation(i64),
  /// Another container judges the decode time (`mux.c` 647).
  Decode(i64),
}

impl PacketRules {
  /// The rules `family`'s packet path applies to a timed track of `kind`
  /// whose stream holds `codec`, counted in `time_base`; `shifts` says
  /// whether libavformat moves early times in this container
  /// ([`Output::shifts_early_times`]).
  fn settle(
    family: Family,
    kind: TrackKind,
    codec: &StreamCodec,
    time_base: Timebase,
    shifts: bool,
  ) -> Self {
    let id = codec.codec();
    let mp4 = family == Family::Mp4;
    let matroska = family == Family::Matroska;
    let either_matroska = matroska || family == Family::WebM;
    let earliest = if either_matroska {
      Some(Earliest::Presentation(-matroska_codec_delay(
        kind, codec, time_base,
      )))
    } else if family == Family::Other && shifts {
      Some(Earliest::Decode(0))
    } else {
      None
    };
    Self {
      mp4,
      earliest,
      no_empty: mp4 || (either_matroska && kind == TrackKind::Audio),
      needs_pts: family != Family::Other && codec.video_delay() > 0,
      raw_aac: (mp4 || matroska) && id == CodecId::AAC,
      // Matroska and WebM write every subtitle block as a key frame
      // (`matroskaenc.c` 3132–3133): the flag a subtitle packet offers is
      // not kept there either way.
      intra_only: id.intra_only() && !(either_matroska && kind == TrackKind::Subtitle),
      frame_bytes: if mp4 {
        mp4_frame_bytes(kind, codec)
      } else {
        None
      },
      obus: (mp4 || either_matroska) && id == CodecId::AV1,
      wavpack: matroska && id == CodecId::WAVPACK,
      cues_in_order: mp4 && id == CodecId::MOV_TEXT,
      skip_samples: (mp4 && codec.frame_size() != 0 && codec.sample_rate() > 0)
        .then(|| (codec.frame_size(), codec.sample_rate())),
      streaminfo_len: (matroska && id == CodecId::FLAC).then(|| codec.extradata().len()),
      prores: matroska && id == CodecId::PRORES,
      webvtt: either_matroska && id == CodecId::WEBVTT,
      ac3: (mp4 && (id == CodecId::AC3 || id == CodecId::EAC3)).then_some(id == CodecId::EAC3),
    }
  }
}

/// An audio track's codec delay in Matroska, in its stream's ticks: its
/// initial padding counted at its sample rate and rounded to the nearest
/// tick, as `av_rescale_q` rounds (`matroskaenc.c` 2089–2104) — the
/// distance before zero its earliest presentation may lie. Zero for any
/// other track.
fn matroska_codec_delay(kind: TrackKind, codec: &StreamCodec, time_base: Timebase) -> i64 {
  let padding = codec.initial_padding();
  if kind != TrackKind::Audio || padding <= 0 || codec.sample_rate() <= 0 {
    return 0;
  }
  // `padding / sample_rate` seconds in ticks of `num / den` seconds.
  rescale_nearest(
    i128::from(padding),
    i128::from(time_base.den().get()),
    i128::from(codec.sample_rate()) * i128::from(time_base.num()),
  )
}

/// `a × b / c` rounded to the nearest, half away from zero — `av_rescale`'s
/// `AV_ROUND_NEAR_INF` — for `a ≥ 0` and `b, c > 0`; saturated at `i64`.
fn rescale_nearest(a: i128, b: i128, c: i128) -> i64 {
  if c <= 0 {
    return 0;
  }
  let product = a.saturating_mul(b);
  i64::try_from((product.saturating_add(c / 2)) / c).unwrap_or(i64::MAX)
}

/// MP4's bytes per frame for a PCM-like audio codec — `bits per sample /
/// 8 × channels`, which `size / sample_size` counts a packet's samples by
/// — or `None` where MP4 counts samples otherwise (`movenc.c` 8538–8560):
/// a codec with frames of more than one sample, or of no fixed size.
fn mp4_frame_bytes(kind: TrackKind, codec: &StreamCodec) -> Option<u32> {
  if kind != TrackKind::Audio {
    return None;
  }
  let bits = codec.codec().bits_per_sample();
  let frame_size = codec.frame_size();
  if (frame_size == 0 && bits == 0) || frame_size > 1 {
    return None;
  }
  let bytes = (bits >> 3).checked_mul(u32::try_from(codec.channels()).ok()?)?;
  (bytes > 0).then_some(bytes)
}

/// What the packet rules read of a packet beside its times.
#[derive(Clone, Copy)]
struct Payload<'a> {
  bytes: &'a [u8],
  key: bool,
  side_data: &'a [SideDataEntry],
}

/// Whether `bytes` opens with an ADTS header — its first twelve bits set —
/// as MP4's and Matroska's checks read one (`movenc.c` 7076–7077;
/// `matroskaenc.c` 3636–3637).
fn adts_header(bytes: &[u8]) -> bool {
  bytes.len() > 2 && (u16::from_be_bytes([bytes[0], bytes[1]]) & 0xfff0) == 0xfff0
}

/// Whether `bytes` is a run of OBUs libavformat's AV1 filter parses whole
/// (`av1_filter_obus`, `av1.c` 33–81).
fn obus_parse(bytes: &[u8]) -> bool {
  let mut rest = bytes;
  while !rest.is_empty() {
    match obu_len(rest) {
      Some(len) => rest = &rest[len..],
      None => return false,
    }
  }
  true
}

/// The length of the OBU `buf` starts with — its header and its payload —
/// as libavcodec's `parse_obu_header` reads it (`av1_parse.h` 92–134 in
/// FFmpeg 9.0.1), or `None` where it refuses one: the forbidden bit set, a
/// header running past the ten bytes it reads or past the packet, or a
/// payload running past the packet. A header is a byte, an extension byte
/// where flagged, and a LEB128 size of up to eight bytes where flagged
/// (`leb.h` 57–68); without one the OBU runs to the packet's end.
fn obu_len(buf: &[u8]) -> Option<usize> {
  let window = &buf[..buf.len().min(10)];
  let first = *window.first()?;
  if first & 0x80 != 0 {
    return None;
  }
  let extension = first & 0x04 != 0;
  let has_size = first & 0x02 != 0;
  let mut header = 1;
  if extension {
    window.get(header)?;
    header += 1;
  }
  let payload = if has_size {
    let mut size: u64 = 0;
    for shift in 0..8 {
      let byte = *window.get(header)?;
      header += 1;
      size |= u64::from(byte & 0x7f) << (7 * shift);
      if byte & 0x80 == 0 {
        break;
      }
    }
    size
  } else {
    // The rest of the packet, as `buf_size - 1 - extension_flag` counts it.
    (buf.len() - header) as u64
  };
  let len = payload.checked_add(header as u64)?;
  if len > buf.len() as u64 {
    return None;
  }
  usize::try_from(len).ok()
}

/// Whether `bytes` is a run of WavPack blocks Matroska's reformatter
/// parses whole (`mkv_reformat_wavpack`, `matroskaenc.c` 2775–2816, over
/// `ff_wv_parse_header`, `wv.c` 30–53): each a 32-byte header tagged
/// `wvpk` whose block size lies in 24..=2^20, then the rest of its block,
/// and nothing left over.
fn wavpack_parse(bytes: &[u8]) -> bool {
  const HEADER: usize = 32;
  const BLOCK_LIMIT: u32 = 1 << 20;
  let mut rest = bytes;
  while !rest.is_empty() {
    if rest.len() < HEADER || &rest[..4] != b"wvpk" {
      return false;
    }
    let size = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
    if !(24..=BLOCK_LIMIT).contains(&size) {
      return false;
    }
    let block = (size - 24) as usize;
    match rest[HEADER..].get(block..) {
      Some(after) => rest = after,
      None => return false,
    }
  }
  true
}

/// The payload of `side_data`'s entry of `kind`, if the packet carries one.
fn side_data(side_data: &[SideDataEntry], kind: AVPacketSideDataType) -> Option<&[u8]> {
  side_data
    .iter()
    .find(|entry| entry.kind() == kind as i32)
    .map(SideDataEntry::data)
}

/// Whether MP4 would shorten the packet below nothing by its
/// skip-samples side data (`movenc.c` 7311–7320): its duration, at least
/// a frame of `frame_size` samples, less the samples to drop from its end,
/// each counted in `time_base` from `sample_rate` as `av_rescale_q`
/// rounds.
fn skips_below_nothing(
  side: &[SideDataEntry],
  duration: i64,
  (frame_size, sample_rate): (i32, i32),
  time_base: Timebase,
) -> bool {
  let Some(skip) = side_data(side, AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES) else {
    return false;
  };
  if skip.len() < 10 {
    return false;
  }
  let ticks = |samples: i128| {
    rescale_nearest(
      samples,
      i128::from(time_base.den().get()),
      i128::from(sample_rate) * i128::from(time_base.num()),
    )
  };
  let end = u32::from_le_bytes([skip[4], skip[5], skip[6], skip[7]]);
  let longest = ticks(i128::from(frame_size)).max(duration);
  i128::from(longest) - i128::from(ticks(i128::from(end))) < 0
}

/// Whether a ProRes packet reads back from Matroska as itself: it opens on
/// its frame's atom — its size, the packet's, and `icpf` — which Matroska
/// drops (`matroskaenc.c` 3584–3587, 695–696) and its reader puts back,
/// unless the bytes after the atom open on one of their own
/// (`matroskadec.c` 3870–3889, 4176–4178).
fn prores_atom(bytes: &[u8]) -> bool {
  bytes.len() >= 8
    && usize::try_from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
      .is_ok_and(|size| size == bytes.len())
    && &bytes[4..8] == b"icpf"
    && bytes.get(12..16) != Some(b"icpf".as_slice())
}

/// Whether a WebVTT cue reads back from Matroska's and WebM's lines as
/// itself (`webm_reformat_vtt`, `matroskaenc.c` 2828–2855;
/// `matroska_parse_webvtt`, `matroskadec.c` 3890–3990): its identifier
/// and settings each absent or a line of their own, not empty, and its
/// text not ending in a line break.
fn webvtt_cue(bytes: &[u8], side: &[SideDataEntry]) -> bool {
  let line = |kind| match side_data(side, kind) {
    None => true,
    Some(text) => !text.is_empty() && !text.iter().any(|byte| matches!(byte, b'\r' | b'\n')),
  };
  line(AVPacketSideDataType::AV_PKT_DATA_WEBVTT_IDENTIFIER)
    && line(AVPacketSideDataType::AV_PKT_DATA_WEBVTT_SETTINGS)
    && !matches!(bytes.last(), Some(b'\r' | b'\n'))
}

/// The bytes of an AC-3 frame, by its frame size code and sample rate
/// code, in 16-bit words (`ff_ac3_frame_size_tab`, `ac3tab.c` 36–75 in
/// FFmpeg 9.0.1).
const AC3_FRAME_WORDS: [[u16; 3]; 38] = [
  [64, 69, 96],
  [64, 70, 96],
  [80, 87, 120],
  [80, 88, 120],
  [96, 104, 144],
  [96, 105, 144],
  [112, 121, 168],
  [112, 122, 168],
  [128, 139, 192],
  [128, 140, 192],
  [160, 174, 240],
  [160, 175, 240],
  [192, 208, 288],
  [192, 209, 288],
  [224, 243, 336],
  [224, 244, 336],
  [256, 278, 384],
  [256, 279, 384],
  [320, 348, 480],
  [320, 349, 480],
  [384, 417, 576],
  [384, 418, 576],
  [448, 487, 672],
  [448, 488, 672],
  [512, 557, 768],
  [512, 558, 768],
  [640, 696, 960],
  [640, 697, 960],
  [768, 835, 1152],
  [768, 836, 1152],
  [896, 975, 1344],
  [896, 976, 1344],
  [1024, 1114, 1536],
  [1024, 1115, 1536],
  [1152, 1253, 1728],
  [1152, 1254, 1728],
  [1280, 1393, 1920],
  [1280, 1394, 1920],
];

/// The channels each bit of an E-AC-3 dependent frame's channel map adds,
/// from its most significant (`ff_eac3_custom_channel_map_locations`,
/// `ac3tab.c` 164–181): a pair where the location is one.
const EAC3_MAP_CHANNELS: [u32; 16] = [1, 1, 1, 1, 1, 2, 2, 1, 1, 2, 2, 2, 1, 2, 1, 1];

/// An AC-3 or E-AC-3 frame's header, as much of it as MP4's handling
/// reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ac3Header {
  /// The bit stream id.
  bsid: u8,
  /// `EAC3_FRAME_TYPE_*`: independent `0`, dependent `1`, an AC-3 frame
  /// `2`.
  frame_type: u8,
  /// The frame's bytes, as its header counts them.
  size: usize,
  /// The audio blocks it holds.
  blocks: u8,
}

/// Bits read from the front, as libavcodec's checked reader reads them:
/// past the end, zeros.
struct Bits<'a> {
  bytes: &'a [u8],
  at: usize,
}

impl Bits<'_> {
  fn read(&mut self, count: u32) -> u32 {
    let mut value = 0;
    for _ in 0..count {
      let byte = self.bytes.get(self.at / 8).copied().unwrap_or(0);
      value = (value << 1) | u32::from((byte >> (7 - self.at % 8)) & 1);
      self.at += 1;
    }
    value
  }

  fn skip(&mut self, count: usize) {
    self.at += count;
  }
}

/// The header of the AC-3 or E-AC-3 frame `bytes` opens with, or `None`
/// where libavcodec's parser refuses it (`ff_ac3_parse_header`,
/// `ac3_parser.c` 288–395, and `eac3_parse_header`, 130–288, in FFmpeg
/// 9.0.1): no sync word; a bit stream id past 16; for AC-3 a reserved
/// sample rate or a frame size code past 37; for E-AC-3 a reserved frame
/// type, a frame of fewer than 7 bytes, a reserved sample rate, a
/// substream other than the first, or a dependent frame's channel map of
/// more than 16 channels.
fn ac3_header(bytes: &[u8]) -> Option<Ac3Header> {
  let mut bits = Bits { bytes, at: 0 };
  if bits.read(16) != 0x0B77 {
    return None;
  }
  // The bit stream id sits at bits 40–44 in both.
  let bsid = u8::try_from(Bits { bytes, at: 40 }.read(5)).ok()?;
  if bsid > 16 {
    return None;
  }
  if bsid <= 10 {
    bits.skip(16);
    let sample_rate = bits.read(2);
    if sample_rate == 3 {
      return None;
    }
    let frame_size = usize::try_from(bits.read(6)).ok()?;
    let words = AC3_FRAME_WORDS.get(frame_size)?[usize::try_from(sample_rate).ok()?];
    return Some(Ac3Header {
      bsid,
      frame_type: 2,
      size: usize::from(words) * 2,
      blocks: 6,
    });
  }
  let frame_type = u8::try_from(bits.read(2)).ok()?;
  if frame_type == 3 {
    return None;
  }
  let substream = bits.read(3);
  let size = (usize::try_from(bits.read(11)).ok()? + 1) << 1;
  if size < 7 {
    return None;
  }
  let blocks = if bits.read(2) == 3 {
    if bits.read(2) == 3 {
      return None;
    }
    6
  } else {
    [1, 2, 3, 6][usize::try_from(bits.read(2)).ok()?]
  };
  let channel_mode = bits.read(3);
  bits.skip(1);
  if substream != 0 {
    return None;
  }
  // The bit stream id, then the volume fields — twice for dual mono —
  // before a dependent frame's channel map.
  bits.skip(5);
  for _ in 0..if channel_mode == 0 { 2 } else { 1 } {
    bits.skip(5);
    if bits.read(1) == 1 {
      bits.skip(8);
    }
  }
  if frame_type == 1 && bits.read(1) == 1 {
    let map = bits.read(16);
    let channels: u32 = (0..16)
      .filter(|bit| map & (1 << (15 - bit)) != 0)
      .map(|bit| EAC3_MAP_CHANNELS[bit])
      .sum();
    if channels > 16 {
      return None;
    }
  }
  Some(Ac3Header {
    bsid,
    frame_type,
    size,
    blocks,
  })
}

/// What MP4's AC-3 handling has learned of a track (`handle_eac3`,
/// `movenc.c` 464–610): whether it has read the stream's first frame set,
/// the bit stream id of its first frame, and the blocks of E-AC-3 frames
/// it holds back to make up a sample of six.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Ac3State {
  done: bool,
  bsid: u8,
  pending: u8,
}

/// Holds an MP4 AC-3 (`eac3` false) or E-AC-3 packet to MP4's handling of
/// it, from what MP4 has learned of the track (`handle_eac3`, `movenc.c`
/// 464–610): answers what MP4 learns of this packet, or the rule it
/// breaks.
fn ac3_packet(state: Ac3State, eac3: bool, bytes: &[u8]) -> Result<Ac3State, PayloadRule> {
  let first = ac3_header(bytes).ok_or(PayloadRule::Ac3Header)?;
  let mut next = state;
  // 509–575: until the first set is read, a second independent frame ends
  // it; else the frame is the set's first, and an E-AC-3 track's first
  // packet must be that frame and dependent frames tiling the rest.
  let independent = first.frame_type != 1;
  if !state.done {
    if independent && state.bsid != 0 {
      next.done = true;
    } else {
      next.bsid = first.bsid;
      if !eac3 {
        next.done = true;
      } else {
        let mut at = first.size;
        while at != bytes.len() {
          let dependent = bytes
            .get(at..)
            .and_then(ac3_header)
            .ok_or(PayloadRule::Ac3Substreams)?;
          if dependent.frame_type != 1 {
            return Err(PayloadRule::Ac3Substreams);
          }
          at += dependent.size;
        }
      }
    }
  }
  // 577–606: a frame of six blocks is a sample of its own; fewer are held
  // back until six.
  next.pending = match (state.pending, first.blocks) {
    (0, 6) => 0,
    (pending, blocks) if pending + blocks > 6 => return Err(PayloadRule::Ac3Blocks),
    (pending, blocks) if pending + blocks == 6 => 0,
    (pending, blocks) => pending + blocks,
  };
  Ok(next)
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
  /// What the container learns of the track from the packet.
  learned: Learned,
}

/// What a container learns of a track from a packet it writes, for the
/// next one's judgment.
#[derive(Debug, Clone, Copy, Default)]
struct Learned {
  /// MP4 `mov_text`: where the cue ends, in ticks.
  cue_end: Option<i128>,
  /// MP4 AC-3 and E-AC-3: what MP4 has learned of the frames.
  ac3: Option<Ac3State>,
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
    // Every packet is written as it was handed over, or refused by name:
    // in a container whose packet rules this release accounts for, no
    // automatic filter rewrites one first (see the module). Another
    // container keeps libavformat's own choice.
    if family != Family::Other {
      output
        .set_automatic_filters(false)
        .map_err(|error| MuxFault::new(MuxStage::Open, error))?;
    }

    // **The whole table is judged before anything is added**, so a
    // refusal leaves nothing written.
    let mut roads = Vec::with_capacity(tracks.len());
    let mut lost = Vec::with_capacity(tracks.len());
    for (index, description) in tracks.iter().enumerate() {
      let track = TrackIndex::new(index);
      let (road, losses) = judge(&output, family, track, description)?;
      roads.push(road);
      lost.push(losses);
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
          let spec = stream_spec(family, description, *road);
          let stream = output
            .add_stream(&spec)
            .map_err(|error| MuxFault::new(MuxStage::Stream { track }, error))?;
          let codec = output
            .stream_codec(stream)
            .map_err(|error| MuxFault::new(MuxStage::Stream { track }, error))?;
          Some((stream, codec))
        }
      };
      // The report, track by track: what the road drops of the track's
      // fields, then what the container does to its packets, judged from
      // the stream as libavformat holds it.
      report.uncarried.append(&mut lost[index]);
      if let (Road::Timed, Some((_, codec))) = (road, &stream) {
        let frame_rate = match description.params() {
          TrackParams::Video(params) => params.frame_rate(),
          _ => None,
        };
        report.uncarried.extend(
          rewrites(
            family,
            kind,
            codec.codec(),
            codec.extradata(),
            default_duration(kind, codec, frame_rate),
          )
          .map(|rewrite| Uncarried::Rewritten { track, rewrite }),
        );
      }
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

    let shifts = output.shifts_early_times();
    let mut track_states = Vec::with_capacity(states.len());
    for (index, (kind, stream, road)) in states.into_iter().enumerate() {
      let track = TrackIndex::new(index);
      let mut rules = PacketRules::default();
      let stream = match (stream, road) {
        (Some((stream, codec)), Road::Timed) => {
          let time_base = output
            .stream_time_base(stream)
            .map_err(|error| MuxFault::new(MuxStage::Header, error))?;
          rules = PacketRules::settle(family, kind, &codec, time_base, shifts);
          Some((stream, time_base))
        }
        // An attachment stream takes no packet; a cover takes its one,
        // here, and nothing after it. (A description of an attachment
        // always carries its file; the guard is the type's, not ours.)
        (Some((stream, _)), Road::Cover) => {
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
        rules,
        cue_end: None,
        ac3: Ac3State::default(),
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

  /// Judges a timed packet: the face's rules — the presentation rule on
  /// the counts libavformat is handed — then the container's, in the
  /// packet-rule table's order (see [the module](self)).
  fn admit(
    &self,
    track: TrackIndex,
    offered: TrackKind,
    pts: Option<Timestamp>,
    dts: Option<Timestamp>,
    duration: Option<Timestamp>,
    payload: Payload<'_>,
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
    let unrepresentable = |timestamp: Timestamp| {
      Refusal::Container(FfmpegMuxRefusal::Unrepresentable { track, timestamp })
    };
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
    // The face's presentation rule, applied as the container applies it:
    // to the counts libavformat is handed, in the stream's time base
    // (`mux.c` 567–572). Two instants a hair apart may count to one tick,
    // and a packet whose presentation and decode times do is one
    // libavformat writes — as a stream copy of a file with sub-tick
    // jitter does — so it is admitted. The refusal names the times the
    // packet carries, not their counts.
    if let (Some(pts), Some(pts_ticks)) = (pts, pts_ticks) {
      let stored = |ticks: i64| Timestamp::new(ticks, time_base);
      if check_presentation_order(track, Some(stored(pts_ticks)), stored(dts_ticks)).is_err() {
        return Err(Refusal::Portable(MuxRefusal::PtsBeforeDts {
          track,
          pts,
          dts,
        }));
      }
    }
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
    let learned = check_packet_rules(
      state,
      time_base,
      Timed {
        track,
        pts,
        dts,
        duration,
        pts_ticks,
        dts_ticks,
        duration_ticks,
      },
      payload,
    )
    .map_err(Refusal::Container)?;
    Ok(Plan {
      track,
      stream,
      dts,
      times: PacketTimes::new(pts_ticks, Some(dts_ticks), duration_ticks),
      dts_ticks,
      learned,
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
    if let Some(cue_end) = plan.learned.cue_end {
      state.cue_end = Some(cue_end);
    }
    if let Some(ac3) = plan.learned.ac3 {
      state.ac3 = ac3;
    }
    Ok(())
  }
}

/// A timed packet's times, as offered and as counted in its stream's time
/// base.
#[derive(Clone, Copy)]
struct Timed {
  track: TrackIndex,
  pts: Option<Timestamp>,
  dts: Timestamp,
  duration: Option<Timestamp>,
  pts_ticks: Option<i64>,
  dts_ticks: i64,
  duration_ticks: i64,
}

/// Holds a packet the face admitted to the rows of the packet-rule table
/// its track meets ([`PacketRules`]), in the table's order; answers what
/// the container learns of the track from it, for the next one's judgment.
fn check_packet_rules(
  state: &TrackState,
  time_base: Timebase,
  timed: Timed,
  payload: Payload<'_>,
) -> Result<Learned, FfmpegMuxRefusal> {
  let rules = &state.rules;
  let Timed {
    track,
    pts,
    dts,
    duration,
    pts_ticks,
    dts_ticks,
    duration_ticks,
  } = timed;
  let limit = i128::from(i32::MAX);
  if let Some(duration) = duration
    && duration_ticks < 0
  {
    return Err(FfmpegMuxRefusal::NegativeDuration { track, duration });
  }
  if rules.mp4 {
    if let Some(duration) = duration
      && i128::from(duration_ticks) > limit
    {
      return Err(FfmpegMuxRefusal::DurationTooLong { track, duration });
    }
    if let Some(last) = state.last_ticks
      && i128::from(dts_ticks) - i128::from(last) >= limit
    {
      return Err(FfmpegMuxRefusal::DtsGap { track, dts });
    }
    if let (Some(pts), Some(pts_ticks)) = (pts, pts_ticks)
      && i128::from(pts_ticks) - i128::from(dts_ticks) > limit
    {
      return Err(FfmpegMuxRefusal::PresentationOffset { track, pts, dts });
    }
  }
  if rules.needs_pts && pts.is_none() {
    return Err(FfmpegMuxRefusal::MissingPts { track });
  }
  match rules.earliest {
    Some(Earliest::Presentation(earliest)) if pts_ticks.unwrap_or(dts_ticks) < earliest => {
      return Err(FfmpegMuxRefusal::BeforeZero {
        track,
        timestamp: pts.unwrap_or(dts),
      });
    }
    Some(Earliest::Decode(earliest)) if dts_ticks < earliest => {
      return Err(FfmpegMuxRefusal::BeforeZero {
        track,
        timestamp: dts,
      });
    }
    _ => {}
  }
  let cue_end = if rules.cues_in_order {
    if dts_ticks < 0 || state.cue_end.is_some_and(|end| i128::from(dts_ticks) < end) {
      return Err(FfmpegMuxRefusal::CueOrder { track, pts: dts });
    }
    Some(i128::from(dts_ticks) + i128::from(duration_ticks))
  } else {
    None
  };
  if rules.intra_only && !payload.key {
    return Err(FfmpegMuxRefusal::KeyFrameUnmarked { track });
  }
  let refused = |rule| Err(FfmpegMuxRefusal::Payload { track, rule });
  if rules.no_empty && payload.bytes.is_empty() {
    return refused(PayloadRule::Empty);
  }
  if rules.raw_aac && adts_header(payload.bytes) {
    return refused(PayloadRule::AdtsHeader);
  }
  if let Some(frame) = rules.frame_bytes
    && !payload.bytes.len().is_multiple_of(frame as usize)
  {
    return refused(PayloadRule::PartialSample);
  }
  if rules.obus && !obus_parse(payload.bytes) {
    return refused(PayloadRule::Obus);
  }
  if rules.wavpack && !wavpack_parse(payload.bytes) {
    return refused(PayloadRule::WavPackBlocks);
  }
  if let Some(skip) = rules.skip_samples
    && skips_below_nothing(payload.side_data, duration_ticks, skip, time_base)
  {
    return refused(PayloadRule::SkipSamples);
  }
  if let Some(len) = rules.streaminfo_len
    && side_data(
      payload.side_data,
      AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
    )
    .is_some_and(|new| !new.is_empty() && new.len() != len)
  {
    return refused(PayloadRule::FlacStreamInfo);
  }
  if rules.prores && !prores_atom(payload.bytes) {
    return refused(PayloadRule::ProResAtom);
  }
  if rules.webvtt && !webvtt_cue(payload.bytes, payload.side_data) {
    return refused(PayloadRule::WebVttCue);
  }
  let ac3 = match rules.ac3 {
    Some(eac3) => match ac3_packet(state.ac3, eac3, payload.bytes) {
      Ok(learned) => Some(learned),
      Err(rule) => return refused(rule),
    },
    None => None,
  };
  Ok(Learned { cue_end, ac3 })
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
    let payload = Payload {
      bytes: p.data().as_ref(),
      key: p.flags().contains(PacketFlags::KEY),
      side_data: p.extra().side_data(),
    };
    let plan = match self.admit(
      packet.track(),
      TrackKind::Video,
      p.pts(),
      p.dts(),
      p.duration(),
      payload,
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
    let payload = Payload {
      bytes: p.data().as_ref(),
      key: p.flags().contains(PacketFlags::KEY),
      side_data: p.extra().side_data(),
    };
    let plan = match self.admit(
      packet.track(),
      TrackKind::Audio,
      p.pts(),
      p.dts(),
      p.duration(),
      payload,
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
    let payload = Payload {
      bytes: p.data().as_ref(),
      key: p.flags().contains(PacketFlags::KEY),
      side_data: p.extra().side_data(),
    };
    let plan = match self.admit(
      packet.track(),
      TrackKind::Subtitle,
      p.pts(),
      p.pts(),
      p.duration(),
      payload,
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
    let payload = Payload {
      bytes: p.data().as_ref(),
      key: p.flags().contains(PacketFlags::KEY),
      side_data: p.extra().side_data(),
    };
    let plan = match self.admit(
      packet.track(),
      TrackKind::Data,
      p.pts(),
      p.pts(),
      p.duration(),
      payload,
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
/// and what that road drops of its fields.
fn judge<W, D: AsRef<[u8]>>(
  output: &Output<W>,
  family: Family,
  track: TrackIndex,
  description: &TrackDescription<Ffmpeg, D, CodecTicket>,
) -> Result<(Road, Vec<Uncarried>), MuxOpenError> {
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
      _ => Road::NotWritten,
    },
    _ => {
      if output.query_codec(codec) == CodecQuery::NotCarried {
        return Err(MuxOpenError::CodecNotCarried { track, codec });
      }
      if configuration_missing(family, codec, stream_extradata(description)) {
        return Err(MuxOpenError::ConfigurationMissing { track, codec });
      }
      Road::Timed
    }
  };
  Ok((road, losses(family, road, track, description)))
}

/// The extradata a timed track's stream will hold: the description's
/// own, else its ticket's ([`stream_spec`]).
fn stream_extradata<D: AsRef<[u8]>>(
  description: &TrackDescription<Ffmpeg, D, CodecTicket>,
) -> &[u8] {
  match description.extradata() {
    Some(bytes) => bytes.as_ref(),
    None => description.side_data().extradata(),
  }
}

/// Whether `family` needs a configuration `codec`'s track does not carry
/// ([`MuxOpenError::ConfigurationMissing`]).
fn configuration_missing(family: Family, codec: CodecId, extradata: &[u8]) -> bool {
  if !extradata.is_empty() {
    return false;
  }
  match family {
    Family::Mp4 => [
      CodecId::H264,
      CodecId::HEVC,
      CodecId::VVC,
      CodecId::LCEVC,
      CodecId::EVC,
    ]
    .contains(&codec),
    Family::Matroska => codec == CodecId::AV1 || codec == CodecId::AAC,
    Family::WebM => codec == CodecId::AV1,
    Family::Other => false,
  }
}

/// Whether `extradata` is Annex B — starts with a 3- or 4-byte start code
/// — as MP4 and Matroska judge an HEVC or VVC configuration
/// (`movenc.c` 7108–7140; `matroskaenc.c` 3575–3580).
fn annex_b(extradata: &[u8]) -> bool {
  extradata.starts_with(&[0, 0, 1]) || extradata.starts_with(&[0, 0, 0, 1])
}

/// Whether Matroska writes a default duration for a timed track of `kind`
/// whose stream libavformat holds as `stream`, at `frame_rate`: a video
/// track's from its frame rate, an audio track's from the samples its
/// codec fixes in each frame (`matroskaenc.c` 2058–2065, 2119–2124). A
/// subtitle's duration is written with every block (2895–2898), as if it
/// had one.
fn default_duration(kind: TrackKind, stream: &StreamCodec, frame_rate: Option<Timebase>) -> bool {
  match kind {
    TrackKind::Video => frame_rate.is_some_and(|rate| rate.num() > 0),
    TrackKind::Audio => {
      stream.sample_rate() > 0 && (stream.frame_duration() != 0 || stream.frame_size() != 0)
    }
    _ => true,
  }
}

/// What `family` does to the packets of a timed track of `kind` and
/// `codec` whose stream holds `extradata`, with or without a
/// [`default_duration`] — the packet-rule table's reported rows, in the
/// table's order.
fn rewrites(
  family: Family,
  kind: TrackKind,
  codec: CodecId,
  extradata: &[u8],
  default_duration: bool,
) -> impl Iterator<Item = Rewrite> {
  let mp4 = family == Family::Mp4;
  let matroska = family == Family::Matroska;
  let either_matroska = matroska || family == Family::WebM;
  // MP4 reads an H.264 or LCEVC configuration whose first byte is not 1
  // — an `avcC`'s version — as Annex B (`movenc.c` 7087–7089, 7141–7142),
  // and an HEVC or VVC one longer than 6 bytes opening on a start code
  // (7108–7109, 7129–7130); Matroska an H.264 one opening on a start code,
  // an HEVC one so longer than 6 bytes, a VVC one 6 or more
  // (`matroskaenc.c` 3574–3582).
  let annex = if mp4 {
    match codec {
      c if c == CodecId::H264 || c == CodecId::LCEVC => {
        extradata.first().is_some_and(|&first| first != 1)
      }
      c if c == CodecId::HEVC || c == CodecId::VVC => extradata.len() > 6 && annex_b(extradata),
      _ => false,
    }
  } else if either_matroska {
    match codec {
      c if c == CodecId::H264 => annex_b(extradata),
      c if c == CodecId::HEVC => extradata.len() > 6 && annex_b(extradata),
      c if c == CodecId::VVC => extradata.len() >= 6 && annex_b(extradata),
      _ => false,
    }
  } else {
    false
  };
  let ttml_encoder = extradata.starts_with(b"lavc-ttmlenc");
  [
    (annex, Rewrite::LengthPrefixed),
    (
      (mp4 || either_matroska) && codec == CodecId::AV1,
      Rewrite::ObusFiltered,
    ),
    (
      mp4 && (codec == CodecId::AC3 || codec == CodecId::EAC3),
      Rewrite::Ac3Parsed,
    ),
    (mp4 && codec == CodecId::MOV_TEXT, Rewrite::CueEnds),
    (
      mp4 && codec == CodecId::TTML && ttml_encoder,
      Rewrite::ParagraphsJoined,
    ),
    (
      mp4 && (codec == CodecId::VC1 || codec == CodecId::TRUEHD),
      Rewrite::SyncFromBitstream,
    ),
    (
      (mp4 && matches!(kind, TrackKind::Subtitle | TrackKind::Data) && codec != CodecId::MOV_TEXT)
        || (either_matroska && !default_duration),
      Rewrite::Durations,
    ),
    (
      matroska && codec == CodecId::WAVPACK,
      Rewrite::WavPackHeaders,
    ),
  ]
  .into_iter()
  .filter_map(|(applies, rewrite)| applies.then_some(rewrite))
}

/// What `road` drops of `description` in `family`, in the report's
/// order: the whole attachment where it is not written; else, by the
/// report table ([`kept`]), an attachment's filename and MIME type, the
/// language and the disposition flags the road keeps no field for.
fn losses<D>(
  family: Family,
  road: Road,
  track: TrackIndex,
  description: &TrackDescription<Ffmpeg, D, CodecTicket>,
) -> Vec<Uncarried> {
  if road == Road::NotWritten {
    return vec![Uncarried::Attachment { track }];
  }
  let kept = kept(
    family,
    road,
    description.kind(),
    description.codec(),
    description.disposition(),
  );
  let mut lost = Vec::new();
  if let Some(file) = description.file() {
    if let Some(filename) = file.filename()
      && !kept.filename
    {
      lost.push(Uncarried::Filename {
        track,
        filename: filename.clone(),
      });
    }
    if let Some(mime_type) = file.mime_type()
      && !kept.mime_type
    {
      lost.push(Uncarried::MimeType {
        track,
        mime_type: mime_type.clone(),
      });
    }
  }
  if let Some(tag) = description.language()
    && !kept.language.keeps(tag.as_str())
  {
    lost.push(Uncarried::Language {
      track,
      tag: tag.clone(),
    });
  }
  let flags = description.disposition().difference(kept.flags);
  if !flags.is_empty() {
    lost.push(Uncarried::Disposition { track, flags });
  }
  lost
}

/// What a road keeps of a track's own fields in a container: one row of
/// the report table (see [the module](self)), which the report and the
/// stream's disposition both read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Kept {
  filename: bool,
  mime_type: bool,
  language: LanguageRule,
  flags: TrackDisposition,
}

/// Which languages a road writes so that they read back as themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LanguageRule {
  /// Any, as given.
  AsGiven,
  /// A Matroska or WebM track entry's: as given, but an empty tag is
  /// written as `und` (`matroskaenc.c` 1967–1969) and `und` reads back as
  /// none (`matroskadec.c` 3280–3281).
  Determined,
  /// MP4's packed code: exactly three bytes in `0x60..=0x7F` that pack
  /// and unpack to themselves — not a first byte of `0x60`, which packs
  /// below `0x400` and unpacks through the Macintosh table, and not
  /// `0x7F7F7F`, which packs to "unspecified" (`isom.c` 233–280;
  /// `movenc.c` 8455–8458). An empty tag is written as `und`.
  Packed,
  /// None.
  Dropped,
}

impl LanguageRule {
  /// Whether the road writes `tag` so that it reads back as `tag`.
  fn keeps(self, tag: &str) -> bool {
    match self {
      Self::AsGiven => true,
      Self::Determined => !tag.is_empty() && tag != "und",
      Self::Packed => {
        let bytes = tag.as_bytes();
        bytes.len() == 3
          && bytes.iter().all(|byte| (0x60..=0x7F).contains(byte))
          && bytes[0] != 0x60
          && bytes != [0x7F; 3]
      }
      Self::Dropped => false,
    }
  }
}

/// The report table: what `road` keeps in `family` of a track of `kind`
/// and `codec` declaring `flags` — from each muxer's source in FFmpeg
/// 9.0.1, and its demuxer's for what reads back. A container this release
/// does not know keeps everything, as far as anything is claimed.
fn kept(
  family: Family,
  road: Road,
  kind: TrackKind,
  codec: CodecId,
  flags: TrackDisposition,
) -> Kept {
  use TrackDisposition as D;
  // Every bit, an unnamed one among them.
  let everything = Kept {
    filename: true,
    mime_type: true,
    language: LanguageRule::AsGiven,
    flags: D::from_bits_retain(u32::MAX),
  };
  match (family, road) {
    (Family::Other, _) | (_, Road::NotWritten) => everything,
    // An attachment Matroska writes keeps its file, filename and MIME
    // type (`mkv_write_attachments`, `matroskaenc.c` 2513–2561) and its
    // language, among the tags written for it (`mkv_write_tags`,
    // 2377–2390), but no flag: it has no track entry (1957–1958).
    (_, Road::Attachment) => Kept {
      flags: D::empty(),
      ..everything
    },
    // MP4's cover keeps its picture and the type of its image
    // (`mov_write_covr`, `movenc.c` 4815–4839), and reads back as an
    // attached picture and nothing more.
    (_, Road::Cover) => Kept {
      filename: false,
      mime_type: false,
      language: LanguageRule::Dropped,
      flags: D::ATTACHED_PIC,
    },
    // `matroskaenc.c` 1967–2003, 2149–2150: the default and forced flags,
    // the commentary and impairment flags, original or dub — one of them;
    // both at once write neither — and text descriptions on a subtitle
    // track alone.
    (Family::Matroska, Road::Timed) => {
      let mut kept = D::DEFAULT | D::FORCED | D::COMMENT | D::HEARING_IMPAIRED | D::VISUAL_IMPAIRED;
      if !flags.contains(D::ORIGINAL | D::DUB) {
        kept |= D::ORIGINAL | D::DUB;
      }
      if kind == TrackKind::Subtitle {
        kept |= D::DESCRIPTIONS;
      }
      Kept {
        language: LanguageRule::Determined,
        flags: kept,
        ..everything
      }
    }
    // WebM: the default and forced flags, and a WebVTT track's kind
    // through its codec id — the first of captions, descriptions or
    // metadata it declares (`matroskaenc.c` 1974–2002).
    (Family::WebM, Road::Timed) => {
      let mut kept = D::DEFAULT | D::FORCED;
      if codec == CodecId::WEBVTT
        && let Some(first) = [D::CAPTIONS, D::DESCRIPTIONS, D::METADATA]
          .into_iter()
          .find(|kind| flags.contains(*kind))
      {
        kept |= first;
      }
      Kept {
        language: LanguageRule::Determined,
        flags: kept,
        ..everything
      }
    }
    // The track's enabled flag (`enable_tracks`, `movenc.c` 7883–7923)
    // and the DASH role `kind` boxes (4338–4356, `isom.c` 450–462).
    (Family::Mp4, Road::Timed) => Kept {
      language: LanguageRule::Packed,
      flags: D::DEFAULT
        | D::HEARING_IMPAIRED
        | D::CAPTIONS
        | D::COMMENT
        | D::VISUAL_IMPAIRED
        | D::DESCRIPTIONS
        | D::DUB
        | D::FORCED,
      ..everything
    },
  }
}

/// The stream a track becomes, on its road. The disposition handed to
/// libavformat is what the report table keeps of it ([`kept`]), so that no
/// flag the report names as dropped acts on the stream — MP4 would make a
/// timed video stream marked as an attached picture into a cover
/// (`is_cover_image`, `movenc.c` 7606–7620).
fn stream_spec<'a, D: AsRef<[u8]>>(
  family: Family,
  description: &'a TrackDescription<Ffmpeg, D, CodecTicket>,
  road: Road,
) -> StreamSpec<'a> {
  let ticket = description.side_data();
  let declared = description.disposition();
  let written = declared.intersection(
    kept(
      family,
      road,
      description.kind(),
      description.codec(),
      declared,
    )
    .flags,
  );
  let spec = StreamSpec::new(ticket, description.timebase())
    .with_disposition(written.bits() as i32)
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

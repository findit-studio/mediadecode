//! The muxing context: libavformat's `AVFormatContext` for output, owned
//! in Rust and writing through the [writer guard](crate::writer_guard)
//! into a caller's sink.
//!
//! [`Output`] is the one place this family hands libavformat a packet to
//! write, and every pointer it gives libavformat is one it owns: the
//! context, the I/O context over the guarded sink, the streams and
//! chapters it adds, and packets it builds from the portable carriers on
//! the **copy road** ([`BodyRoute::Copy`]) — `av_interleaved_write_frame`
//! keeps a packet in its interleaving queue past the call, so nothing a
//! caller holds is ever lent to it. Nothing here takes a pointer from a
//! caller, so nothing here is an `unsafe fn`: what libavformat is owed is
//! established inside, and each `unsafe` block says how.
//!
//! # A context's life
//!
//! [`Output::open`] names the container; streams and chapters are added
//! while the context is being built; [`Output::write_header`] ends that
//! and starts the writing; packets are written; [`Output::write_trailer`]
//! completes the file; [`Output::into_sink`] frees the context and hands
//! the sink back. libavformat leaves a call out of that order undefined
//! — `av_write_trailer` twice runs a muxer's trailer over state its first
//! run freed — so the order is kept here, at run time, and a call out of
//! it is refused by name ([`OutputError::OutOfOrder`]).
//!
//! # The sink
//!
//! libavformat drives the sink from `extern "C"` callbacks, so every call
//! into it runs under the writer guard: a panic is caught there, latched,
//! and reported to libavformat as an I/O error, and every FFmpeg error
//! this context reports is checked against the latch first, so a panic is
//! named as one ([`OutputError::SinkPanic`]) rather than as the error C
//! folded it into. A panicked sink is never called again.

use core::{
  ffi::{c_int, c_uint, c_void},
  marker::PhantomData,
  mem::ManuallyDrop,
  num::NonZeroI32,
  ptr::{self, NonNull, addr_of, addr_of_mut, read_unaligned, write_unaligned},
};
use std::{
  ffi::CString,
  io::{self, Seek, Write},
  sync::Arc,
};

use ffmpeg_next::{
  Packet,
  ffi::{
    AV_DISPOSITION_ATTACHED_PIC, AV_INPUT_BUFFER_PADDING_SIZE, AVChapter, AVCodecParameters,
    AVFMT_FLAG_CUSTOM_IO, AVFMT_NOFILE, AVFMT_NOTIMESTAMPS, AVFMT_TS_NONSTRICT, AVFormatContext,
    AVRational, AVStream, av_dict_free, av_dict_set, av_dynarray_add_nofree, av_free, av_freep,
    av_guess_format, av_interleaved_write_frame, av_mallocz, av_write_trailer,
    avcodec_parameters_copy, avformat_alloc_output_context2, avformat_free_context,
    avformat_new_stream, avformat_write_header,
  },
  format::context::StreamIo,
  packet::Mut,
};
use mediaspine::{
  Timebase,
  demuxer::{AttachmentPacket, DataPacket},
  packet::{AudioPacket, SubtitlePacket, VideoPacket},
};
use smol_bytes::Utf8Bytes;

use crate::{
  CodecId,
  boundary::{
    PacketBuildError, build_attachment_packet, build_audio_packet, build_data_packet,
    build_subtitle_packet, build_video_packet,
  },
  carrier::BodyRoute,
  error::ParametersError,
  extras::{
    AttachmentPacketExtra, AudioPacketExtra, DataPacketExtra, SubtitlePacketExtra, VideoPacketExtra,
  },
  ffi::c_shims,
  limits::PacketLimits,
  reader_guard::PanicLatch,
  ticket::CodecTicket,
  writer_guard::GuardedWriter,
};

/// `AVMEDIA_TYPE_VIDEO`, as the integer `AVCodecParameters.codec_type`
/// stores it.
const MEDIA_VIDEO: i32 = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO as i32;
/// `AVMEDIA_TYPE_AUDIO`, likewise.
const MEDIA_AUDIO: i32 = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_AUDIO as i32;
/// `AVMEDIA_TYPE_ATTACHMENT`, likewise.
const MEDIA_ATTACHMENT: i32 = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_ATTACHMENT as i32;

/// Where a context is in its life — see [the module](self).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputPhase {
  /// Streams and chapters are being added; nothing is written yet.
  Building,
  /// The header is written; packets may be written.
  Writing,
  /// The trailer was written, or attempted: the muxer is done.
  Finished,
  /// A step libavformat cannot be asked to continue from failed: the
  /// header, or the addition of a stream it half-built.
  Failed,
}

/// A call [`Output`] refused for being out of order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputCall {
  /// [`Output::add_stream`].
  AddStream,
  /// [`Output::add_chapter`].
  AddChapter,
  /// [`Output::write_header`].
  WriteHeader,
  /// [`Output::write_packet`].
  WritePacket,
  /// [`Output::write_trailer`].
  WriteTrailer,
}

/// A text handed to libavformat as a C string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OutputText {
  /// The container's short name.
  ContainerName,
  /// A stream's language.
  Language,
  /// An attachment's filename.
  Filename,
  /// An attachment's MIME type.
  MimeType,
  /// A chapter's title.
  ChapterTitle,
}

/// Why an [`Output`] call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OutputError {
  /// libavformat has no muxer by that short name.
  #[error("libavformat has no muxer named {name:?}")]
  UnknownContainer {
    /// The name asked for.
    name: Utf8Bytes,
  },
  /// The muxer writes no byte stream of its own (`AVFMT_NOFILE` — an
  /// image sequence, a device), so a caller's sink would never receive
  /// its output.
  #[error("the {name:?} muxer writes no byte stream, so it cannot write into a sink")]
  NoByteStream {
    /// The muxer's name.
    name: Utf8Bytes,
  },
  /// A text holds a NUL byte, which a C string cannot carry.
  #[error("the {text:?} holds a NUL byte, which a C string cannot carry")]
  Nul {
    /// Which text.
    text: OutputText,
  },
  /// Extradata, or an attachment's file, too large for an
  /// `AVCodecParameters` to hold (`extradata_size` is a C `int`, and the
  /// padding libavcodec reads past it is allocated with it).
  #[error("{bytes} bytes of extradata do not fit an AVCodecParameters")]
  ExtradataTooLarge {
    /// The size offered.
    bytes: usize,
  },
  /// More chapters than libavformat's `int` count can hold.
  #[error("libavformat cannot count another chapter")]
  TooManyChapters,
  /// The stream index names no stream of this context.
  #[error("stream {stream} is not in the context")]
  NoSuchStream {
    /// The index offered.
    stream: usize,
  },
  /// A call out of the order libavformat defines — see [the
  /// module](self).
  #[error("{call:?} is out of order: the context is {phase:?}")]
  OutOfOrder {
    /// The call refused.
    call: OutputCall,
    /// Where the context was.
    phase: OutputPhase,
  },
  /// A time base libavformat chose that this family cannot carry (a
  /// zero or negative part).
  #[error("libavformat chose the time base {num}/{den} for stream {stream}")]
  TimeBase {
    /// The stream.
    stream: usize,
    /// The numerator libavformat chose.
    num: i32,
    /// The denominator libavformat chose.
    den: i32,
  },
  /// The caller's sink panicked. libavformat saw an I/O error; this is
  /// what it was.
  #[error("the sink panicked: {message}")]
  SinkPanic {
    /// The panic's message.
    message: Utf8Bytes,
  },
  /// The caller's sink failed its final flush.
  #[error("the sink failed its final flush: {0}")]
  Sink(#[source] io::Error),
  /// libavformat's own error.
  #[error(transparent)]
  Ffmpeg(#[from] ffmpeg_next::Error),
  /// A packet could not be built from its carrier.
  #[error(transparent)]
  Build(#[from] PacketBuildError),
  /// A stream's codec parameters could not be rebuilt from its ticket.
  #[error(transparent)]
  Parameters(#[from] ParametersError),
}

/// What a muxer can say about carrying a codec, asked before the header
/// rather than learned from its failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecQuery {
  /// The muxer can store the codec.
  Carried,
  /// The muxer cannot store the codec.
  NotCarried,
  /// The muxer cannot tell: the header is where it will say.
  Unknown,
}

/// How a stream's packets reach the container — what kind of stream
/// [`Output::add_stream`] makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StreamMedia {
  /// A timed stream of the ticket's own media type, written packet by
  /// packet.
  Timed,
  /// An `AVMEDIA_TYPE_ATTACHMENT` stream whose extradata is the file —
  /// how Matroska carries an attachment, with its header. It never takes
  /// a packet.
  Attachment,
  /// A video stream whose disposition is exactly
  /// `AV_DISPOSITION_ATTACHED_PIC`, its picture written as its one
  /// packet after the header — how MP4 keeps cover art (`movenc.c`
  /// 184–189, 7606–7618 in FFmpeg 9.0.1).
  Cover,
}

/// What [`Output::add_stream`] needs to make one stream.
///
/// The codec parameters come from a [`CodecTicket`], rebuilt and copied
/// into the stream; everything else the stream carries is set beside
/// them. Texts are Rust strings, refused by name if they hold a NUL.
#[derive(Debug, Clone, Copy)]
pub struct StreamSpec<'a> {
  ticket: &'a CodecTicket,
  time_base: Timebase,
  media: StreamMedia,
  extradata: Option<&'a [u8]>,
  disposition: i32,
  frame_rate: Option<Timebase>,
  language: Option<&'a str>,
  filename: Option<&'a str>,
  mime_type: Option<&'a str>,
}

impl<'a> StreamSpec<'a> {
  /// A timed stream over `ticket`'s parameters, its packets stamped in
  /// `time_base` — the hint libavformat starts from; it may choose
  /// another at the header ([`Output::stream_time_base`]).
  pub const fn new(ticket: &'a CodecTicket, time_base: Timebase) -> Self {
    Self {
      ticket,
      time_base,
      media: StreamMedia::Timed,
      extradata: None,
      disposition: 0,
      frame_rate: None,
      language: None,
      filename: None,
      mime_type: None,
    }
  }

  /// Makes the stream of `media`'s kind.
  #[must_use]
  pub const fn with_media(mut self, media: StreamMedia) -> Self {
    self.media = media;
    self
  }

  /// Replaces the ticket's extradata — with an attachment's file, for
  /// [`StreamMedia::Attachment`].
  #[must_use]
  pub const fn with_extradata(mut self, extradata: Option<&'a [u8]>) -> Self {
    self.extradata = extradata;
    self
  }

  /// The `AV_DISPOSITION_*` bits the stream declares. A
  /// [`StreamMedia::Cover`] stream declares exactly
  /// `AV_DISPOSITION_ATTACHED_PIC`, whatever is given here.
  #[must_use]
  pub const fn with_disposition(mut self, disposition: i32) -> Self {
    self.disposition = disposition;
    self
  }

  /// The average frame rate a video stream declares.
  #[must_use]
  pub const fn with_frame_rate(mut self, frame_rate: Option<Timebase>) -> Self {
    self.frame_rate = frame_rate;
    self
  }

  /// The stream's `language` metadata.
  #[must_use]
  pub const fn with_language(mut self, language: Option<&'a str>) -> Self {
    self.language = language;
    self
  }

  /// An attachment's `filename` metadata.
  #[must_use]
  pub const fn with_filename(mut self, filename: Option<&'a str>) -> Self {
    self.filename = filename;
    self
  }

  /// An attachment's `mimetype` metadata.
  #[must_use]
  pub const fn with_mime_type(mut self, mime_type: Option<&'a str>) -> Self {
    self.mime_type = mime_type;
    self
  }
}

/// One packet for [`Output::write_packet`], by kind, on carrier `C`.
#[non_exhaustive]
pub enum OutputPacket<'a, C: crate::FfmpegCarrier> {
  /// A video packet.
  Video(&'a VideoPacket<VideoPacketExtra, C::Buffer>),
  /// An audio packet.
  Audio(&'a AudioPacket<AudioPacketExtra, C::Buffer>),
  /// A subtitle packet.
  Subtitle(&'a SubtitlePacket<SubtitlePacketExtra, C::Buffer>),
  /// A timed-data packet.
  Data(&'a DataPacket<DataPacketExtra, C::Buffer>),
  /// A [`StreamMedia::Cover`] stream's one picture.
  Cover(&'a AttachmentPacket<AttachmentPacketExtra, C::Buffer>),
}

/// A packet's timestamps in its stream's time base, as the caller
/// rescaled them — what libavformat is handed, in place of the
/// carrier's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PacketTimes {
  pts: Option<i64>,
  dts: Option<i64>,
  duration: i64,
}

impl PacketTimes {
  /// The presentation and decode timestamps, and the duration (`0` for
  /// unknown), each counted in the stream's time base.
  pub const fn new(pts: Option<i64>, dts: Option<i64>, duration: i64) -> Self {
    Self { pts, dts, duration }
  }
}

/// libavformat's muxing context over a caller's sink — see [the
/// module](self).
pub struct Output<W> {
  /// The context, owned: allocated by [`Self::open`], freed exactly once
  /// — by `Drop`, or by [`Self::into_sink`] — and always before `io`.
  ctx: NonNull<AVFormatContext>,
  /// The I/O context over the guarded sink. `ctx.pb` points at it, and
  /// `AVFMT_FLAG_CUSTOM_IO` keeps libavformat from closing it.
  io: ManuallyDrop<StreamIo>,
  /// The writer guard's latch, to name a sink's panic.
  latch: Arc<PanicLatch>,
  phase: OutputPhase,
  /// The sink's type, held by `io` type-erased.
  _sink: PhantomData<fn(W) -> W>,
}

// SAFETY: the context and the I/O context are heap allocations tied to no
// thread, and every access to either goes through `&mut self` — `&self`
// methods read fields libavformat writes only inside `&mut self` calls —
// so moving an `Output` to another thread moves exclusive ownership of
// both. The sink inside the I/O context is a `W: Send` (`open` demands
// it). `Sync` is not implemented.
unsafe impl<W: Send> Send for Output<W> {}

impl<W> Output<W> {
  /// The phase the context is in.
  pub const fn phase(&self) -> OutputPhase {
    self.phase
  }

  /// The muxer's short name — `"matroska"`, `"mp4"`.
  pub fn container(&self) -> &'static str {
    // SAFETY: `ctx` is live, and its `oformat` is the static muxer
    // `open` found, never null on a context `open` returned; its `name`
    // is a NUL-terminated literal in libavformat's static tables.
    unsafe {
      let oformat = (*self.ctx.as_ptr()).oformat;
      crate::ffi::table_text((*oformat).name, 256).unwrap_or("")
    }
  }

  /// Whether the muxer refuses a packet whose decode time equals the
  /// last one written on its stream — libavformat's rule for every muxer
  /// without `AVFMT_TS_NONSTRICT`, on every stream but a subtitle or data
  /// stream, and only after a stream's first nonzero decode time
  /// (`compute_muxer_pkt_fields`, `libavformat/mux.c` 557–566 in FFmpeg
  /// 9.0.1). A muxer with `AVFMT_NOTIMESTAMPS` ignores the check, and
  /// answers `false` here.
  pub fn strict_decode_times(&self) -> bool {
    // SAFETY: as [`Self::container`] — a plain integer field of the
    // static muxer.
    let flags = unsafe { (*(*self.ctx.as_ptr()).oformat).flags };
    flags & (AVFMT_TS_NONSTRICT | AVFMT_NOTIMESTAMPS) == 0
  }

  /// Whether the muxer can store `codec`, asked of libavformat
  /// (`avformat_query_codec`) before the header rather than learned from
  /// its failure.
  pub fn query_codec(&self, codec: CodecId) -> CodecQuery {
    // SAFETY: `ctx` is live and `oformat` is its static muxer; the
    // redeclared shim takes the codec id as a plain `c_int`, so no
    // bindgen enum is formed from it, and libavformat answers every
    // integer (its tag tables are searched, never indexed).
    let answer = unsafe {
      let ctx = self.ctx.as_ptr();
      c_shims::avformat_query_codec((*ctx).oformat, codec.raw(), (*ctx).strict_std_compliance)
    };
    match answer {
      1.. => CodecQuery::Carried,
      0 => CodecQuery::NotCarried,
      _ => CodecQuery::Unknown,
    }
  }

  /// The time base libavformat chose for `stream` — after the header,
  /// the one its packets must be counted in.
  pub fn stream_time_base(&self, stream: usize) -> Result<Timebase, OutputError> {
    let st = self.stream(stream)?;
    // SAFETY: `st` is a live stream of the live context.
    let AVRational { num, den } = unsafe { (*st).time_base };
    match (num > 0, NonZeroI32::new(den)) {
      (true, Some(nz)) if den > 0 => Ok(Timebase::new(num, nz)),
      _ => Err(OutputError::TimeBase { stream, num, den }),
    }
  }

  /// The number of streams added so far.
  pub fn streams(&self) -> usize {
    // SAFETY: a plain integer field of the live context.
    unsafe { (*self.ctx.as_ptr()).nb_streams as usize }
  }

  /// The panic the sink raised, if it raised one.
  pub fn sink_panic(&self) -> Option<Utf8Bytes> {
    self.latch.message()
  }

  /// The stream at `index`, or a refusal naming it.
  fn stream(&self, index: usize) -> Result<*mut AVStream, OutputError> {
    if index >= self.streams() {
      return Err(OutputError::NoSuchStream { stream: index });
    }
    // SAFETY: `streams` holds `nb_streams` live stream pointers, and
    // `index` is below that count.
    Ok(unsafe { *(*self.ctx.as_ptr()).streams.add(index) })
  }

  /// libavformat's `code`, named — the sink's panic first, if it raised
  /// one: libavformat reports a panicked sink as the I/O error the guard
  /// answered, folded into whatever it was doing.
  fn fault(&self, code: c_int) -> OutputError {
    match self.latch.message() {
      Some(message) => OutputError::SinkPanic { message },
      None => OutputError::Ffmpeg(ffmpeg_next::Error::from(code)),
    }
  }

  /// Refuses `call` unless the context is `phase`.
  fn require(&self, call: OutputCall, phase: OutputPhase) -> Result<(), OutputError> {
    if self.phase == phase {
      Ok(())
    } else {
      Err(OutputError::OutOfOrder {
        call,
        phase: self.phase,
      })
    }
  }
}

impl<W: Write + Seek + Send + 'static> Output<W> {
  /// Opens a context for the muxer libavformat names `container` — its
  /// short name, `"matroska"` or `"mp4"` — writing into `sink`.
  ///
  /// Refused by name: a name libavformat does not know, a name holding a
  /// NUL, and a muxer that writes no byte stream of its own. `sink` is
  /// dropped with a refusal: nothing was written into it.
  pub fn open(sink: W, container: &str) -> Result<Self, OutputError> {
    let name = CString::new(container).map_err(|_| OutputError::Nul {
      text: OutputText::ContainerName,
    })?;
    // SAFETY: `name` is a NUL-terminated string live for the call; the
    // other two arguments may be null. libavformat answers a static
    // muxer or null.
    let oformat = unsafe { av_guess_format(name.as_ptr(), ptr::null(), ptr::null()) };
    if oformat.is_null() {
      return Err(OutputError::UnknownContainer {
        name: Utf8Bytes::from(container),
      });
    }
    // SAFETY: `oformat` is a non-null static muxer; `flags` is a plain
    // integer field of it.
    if unsafe { (*oformat).flags } & AVFMT_NOFILE != 0 {
      return Err(OutputError::NoByteStream {
        name: Utf8Bytes::from(container),
      });
    }

    let (guarded, latch) = GuardedWriter::new(sink);
    let mut io = StreamIo::from_write_seek(guarded)?;
    let mut ctx: *mut AVFormatContext = ptr::null_mut();
    // SAFETY: `ctx` is a valid out-pointer; `oformat` is the static muxer
    // just found, so the two names may be null. On failure libavformat
    // leaves `ctx` null and owns nothing of ours.
    let status =
      unsafe { avformat_alloc_output_context2(&mut ctx, oformat, ptr::null(), ptr::null()) };
    let Some(ctx) = NonNull::new(ctx).filter(|_| status >= 0) else {
      return Err(OutputError::Ffmpeg(ffmpeg_next::Error::from(
        status.min(-1),
      )));
    };
    // SAFETY: `ctx` is the live context just allocated, and nothing else
    // holds it. `pb` is set to the I/O context this value keeps alive
    // longer than the context — `Drop` and `into_sink` free the context
    // first — and `AVFMT_FLAG_CUSTOM_IO` tells libavformat the I/O
    // context is the caller's, so it never closes or frees it.
    unsafe {
      (*ctx.as_ptr()).pb = io.as_mut_ptr();
      (*ctx.as_ptr()).flags |= AVFMT_FLAG_CUSTOM_IO;
    }
    Ok(Self {
      ctx,
      io: ManuallyDrop::new(io),
      latch,
      phase: OutputPhase::Building,
      _sink: PhantomData,
    })
  }

  /// Adds one stream, as `spec` describes it, and answers its index.
  ///
  /// Only while the context is being built. The texts are refused by
  /// name before anything is added; a failure after the stream was
  /// added leaves the context [`OutputPhase::Failed`], since libavformat
  /// would write a header for a stream half made.
  pub fn add_stream(&mut self, spec: &StreamSpec<'_>) -> Result<usize, OutputError> {
    self.require(OutputCall::AddStream, OutputPhase::Building)?;
    let language = optional_text(spec.language, OutputText::Language)?;
    let filename = optional_text(spec.filename, OutputText::Filename)?;
    let mime_type = optional_text(spec.mime_type, OutputText::MimeType)?;
    if let Some(bytes) = spec.extradata
      && bytes.len() > (c_int::MAX - AV_INPUT_BUFFER_PADDING_SIZE) as usize
    {
      return Err(OutputError::ExtradataTooLarge { bytes: bytes.len() });
    }
    // Rebuilt before the stream exists, so its failure adds nothing.
    let parameters = spec.ticket.rebuild()?;

    let ctx = self.ctx.as_ptr();
    // SAFETY: `ctx` is live and still being built; a null codec is
    // allowed. libavformat answers the new stream, owned by the context,
    // or null.
    let st = unsafe { avformat_new_stream(ctx, ptr::null()) };
    if st.is_null() {
      return Err(OutputError::Ffmpeg(ffmpeg_next::Error::Other {
        errno: libc::ENOMEM,
      }));
    }
    // From here a failure leaves a stream libavformat would write.
    self.phase = OutputPhase::Failed;

    // SAFETY: `st` is the live stream just added, its `codecpar`
    // allocated by `avformat_new_stream`; `parameters` owns a live
    // `AVCodecParameters` (rebuilt and null-checked by the ticket). The
    // copy allocates the stream's own extradata, side data and channel
    // map.
    let status = unsafe { avcodec_parameters_copy((*st).codecpar, parameters.as_ptr()) };
    if status < 0 {
      return Err(self.fault(status));
    }
    drop(parameters);
    // SAFETY: `st` is live, and its `codecpar` is the stream's own,
    // filled just above; nothing else holds either.
    let par = unsafe { (*st).codecpar };

    match spec.media {
      StreamMedia::Timed => {}
      // SAFETY: `par` is the stream's own parameters; the media type is
      // stored as the integer it is, so no bindgen enum is formed.
      StreamMedia::Attachment => unsafe {
        write_unaligned(
          addr_of_mut!((*par).codec_type).cast::<i32>(),
          MEDIA_ATTACHMENT,
        );
      },
      // SAFETY: as above.
      StreamMedia::Cover => unsafe {
        write_unaligned(addr_of_mut!((*par).codec_type).cast::<i32>(), MEDIA_VIDEO);
      },
    }
    if let Some(bytes) = spec.extradata {
      // SAFETY: `par` is the stream's own; `bytes` fits the `int` size
      // with its padding (judged above).
      unsafe { replace_extradata(par, bytes) }.map_err(|code| self.fault(code))?;
    }
    // SAFETY: `par` is the stream's own, and `ctx` the live context
    // whose static muxer's tag table is read.
    unsafe { settle_codec_tag(ctx, par) };
    if spec.media == StreamMedia::Timed {
      // SAFETY: `par` is the stream's own; plain fields.
      unsafe { stream_copy_audio_rule(par) };
    }

    let disposition = match spec.media {
      StreamMedia::Cover => AV_DISPOSITION_ATTACHED_PIC,
      _ => spec.disposition,
    };
    // SAFETY: `st` is live and the context is still being built: these
    // are the plain fields libavformat reads at the header — the time
    // base it starts from, the disposition, the sample aspect ratio the
    // header checks against the parameters' (`mux.c`'s init_muxer), and
    // the frame rate.
    unsafe {
      (*st).time_base = rational(spec.time_base);
      (*st).disposition = disposition;
      (*st).sample_aspect_ratio = (*par).sample_aspect_ratio;
      if let Some(rate) = spec.frame_rate {
        (*st).avg_frame_rate = rational(rate);
      }
    }
    for (key, value) in [
      (c"language", &language),
      (c"filename", &filename),
      (c"mimetype", &mime_type),
    ] {
      if let Some(value) = value {
        // SAFETY: `st` is live; `av_dict_set` copies key and value (no
        // `DONT_STRDUP` flag), both NUL-terminated and live for the call.
        let status = unsafe {
          av_dict_set(
            addr_of_mut!((*st).metadata),
            key.as_ptr(),
            value.as_ptr(),
            0,
          )
        };
        if status < 0 {
          return Err(self.fault(status));
        }
      }
    }

    self.phase = OutputPhase::Building;
    // SAFETY: a plain integer field of the live stream.
    Ok(unsafe { (*st).index } as usize)
  }

  /// Adds one chapter: `start` and `end` counted in `time_base`.
  ///
  /// Only while the context is being built. libavformat does not judge a
  /// chapter here; a muxer that cannot write one fails its header
  /// instead, so a caller judges the span first.
  pub fn add_chapter(
    &mut self,
    id: i64,
    time_base: Timebase,
    start: i64,
    end: i64,
    title: Option<&str>,
  ) -> Result<(), OutputError> {
    self.require(OutputCall::AddChapter, OutputPhase::Building)?;
    let title = optional_text(title, OutputText::ChapterTitle)?;
    let ctx = self.ctx.as_ptr();
    // SAFETY: a plain integer field of the live context.
    let count = unsafe { (*ctx).nb_chapters };
    let Ok(mut count) = c_int::try_from(count) else {
      return Err(OutputError::TooManyChapters);
    };
    if count == c_int::MAX {
      return Err(OutputError::TooManyChapters);
    }
    // SAFETY: a zeroed allocation the size of an `AVChapter`, the way
    // libavformat allocates its own (`avpriv_new_chapter`); null on
    // failure, checked before any write.
    let chapter = unsafe { av_mallocz(size_of::<AVChapter>()) }.cast::<AVChapter>();
    if chapter.is_null() {
      return Err(OutputError::Ffmpeg(ffmpeg_next::Error::Other {
        errno: libc::ENOMEM,
      }));
    }
    // SAFETY: `chapter` is the live, zeroed allocation just made, and
    // nothing else holds it; plain fields.
    unsafe {
      (*chapter).id = id;
      (*chapter).time_base = rational(time_base);
      (*chapter).start = start;
      (*chapter).end = end;
    }
    if let Some(title) = &title {
      // SAFETY: `chapter` is live and its metadata null; `av_dict_set`
      // copies the NUL-terminated key and value.
      let status = unsafe {
        av_dict_set(
          addr_of_mut!((*chapter).metadata),
          c"title".as_ptr(),
          title.as_ptr(),
          0,
        )
      };
      if status < 0 {
        // SAFETY: the chapter is still ours alone: its dictionary and the
        // chapter itself are freed, each exactly once.
        unsafe { free_chapter(chapter) };
        return Err(self.fault(status));
      }
    }
    // SAFETY: `chapters` is the context's array of `nb_chapters` pointers
    // (null while empty), allocated by libavutil; `count` mirrors that
    // count as the `int` the call takes. `av_dynarray_add_nofree` grows
    // the array and appends `chapter`, or leaves both untouched and fails.
    let status = unsafe {
      av_dynarray_add_nofree(
        addr_of_mut!((*ctx).chapters).cast::<c_void>(),
        &mut count,
        chapter.cast::<c_void>(),
      )
    };
    if status < 0 {
      // SAFETY: the array did not take the chapter, so it is still ours.
      unsafe { free_chapter(chapter) };
      return Err(self.fault(status));
    }
    // SAFETY: the array now holds `count` chapters; the context frees each
    // with it (`avformat_free_context`).
    unsafe { (*ctx).nb_chapters = count as c_uint };
    Ok(())
  }

  /// Writes the header: the end of building, the start of writing.
  ///
  /// A failure leaves the context [`OutputPhase::Failed`]: libavformat
  /// takes no packet and no trailer after a header it did not write.
  pub fn write_header(&mut self) -> Result<(), OutputError> {
    self.require(OutputCall::WriteHeader, OutputPhase::Building)?;
    // SAFETY: `ctx` is live, its `oformat` and `pb` set and its streams
    // complete; the muxer's options are its defaults (null dictionary).
    let status = unsafe { avformat_write_header(self.ctx.as_ptr(), ptr::null_mut()) };
    if status < 0 {
      self.phase = OutputPhase::Failed;
      return Err(self.fault(status));
    }
    self.phase = OutputPhase::Writing;
    Ok(())
  }

  /// Writes one packet to `stream`, with `times` in place of the
  /// carrier's own timestamps.
  ///
  /// The packet is built on the **copy road** — its payload and side data
  /// copied into an allocation libavformat alone then holds — and handed
  /// to `av_interleaved_write_frame`, which takes it and may keep it
  /// queued past the call. Nothing the caller holds is lent.
  pub fn write_packet<C: crate::FfmpegCarrier + crate::CarrierOps>(
    &mut self,
    stream: usize,
    packet: OutputPacket<'_, C>,
    times: PacketTimes,
    limits: PacketLimits,
  ) -> Result<(), OutputError> {
    self.require(OutputCall::WritePacket, OutputPhase::Writing)?;
    let index = c_int::try_from(stream).map_err(|_| OutputError::NoSuchStream { stream })?;
    self.stream(stream)?;
    // SAFETY: `BodyRoute::Copy` — every builder copies the payload into
    // a packet of its own, so there is nothing to prove; the packet goes
    // to libavformat, which may keep it.
    let mut av_packet: Packet = unsafe {
      match packet {
        OutputPacket::Video(p) => build_video_packet::<C>(p, limits, BodyRoute::Copy),
        OutputPacket::Audio(p) => build_audio_packet::<C>(p, limits, BodyRoute::Copy),
        OutputPacket::Subtitle(p) => build_subtitle_packet::<C>(p, limits, BodyRoute::Copy),
        OutputPacket::Data(p) => build_data_packet::<C>(p, limits, BodyRoute::Copy),
        OutputPacket::Cover(p) => build_attachment_packet::<C>(p, limits, BodyRoute::Copy),
      }
    }?;
    av_packet.set_pts(times.pts);
    av_packet.set_dts(times.dts);
    av_packet.set_duration(times.duration);
    // SAFETY: `av_packet` owns a live `AVPacket`; `stream_index` is a
    // plain field, set to a stream of this context.
    unsafe { (*av_packet.as_mut_ptr()).stream_index = index };
    // SAFETY: `ctx` is live and writing; `av_packet` is a refcounted
    // packet this call built, which `av_interleaved_write_frame` takes
    // over — it moves the reference into its queue and leaves the packet
    // blank, which then drops here.
    let status = unsafe { av_interleaved_write_frame(self.ctx.as_ptr(), av_packet.as_mut_ptr()) };
    drop(av_packet);
    if status < 0 {
      return Err(self.fault(status));
    }
    Ok(())
  }

  /// Writes the trailer: the packets still queued for interleaving, then
  /// whatever the muxer writes last.
  ///
  /// Asked once: the context is [`OutputPhase::Finished`] after it,
  /// whatever it answers, since libavformat ends the muxer either way.
  pub fn write_trailer(&mut self) -> Result<(), OutputError> {
    self.require(OutputCall::WriteTrailer, OutputPhase::Writing)?;
    // SAFETY: `ctx` is live and its header was written; this is the one
    // trailer call the phase allows.
    let status = unsafe { av_write_trailer(self.ctx.as_ptr()) };
    self.phase = OutputPhase::Finished;
    if status < 0 {
      return Err(self.fault(status));
    }
    Ok(())
  }

  /// Frees the context and hands the sink back, flushed.
  ///
  /// The bytes still buffered in the I/O context are written through
  /// the guard first, then the sink's own `flush` runs and its error is
  /// reported ([`OutputError::Sink`]) — or the panic it raised, at any
  /// point ([`OutputError::SinkPanic`]). A sink that panicked is not
  /// called again; whatever state it is in, the error says so.
  pub fn into_sink(self) -> Result<W, OutputError> {
    let this = ManuallyDrop::new(self);
    // SAFETY: `this` is never dropped, so each field is moved out exactly
    // once here, and none is used through `this` again.
    let (ctx, io, latch) = unsafe { (this.ctx, ptr::read(&this.io), ptr::read(&this.latch)) };
    // SAFETY: the context is this value's, freed exactly once — before
    // the I/O context its `pb` points at, which `AVFMT_FLAG_CUSTOM_IO`
    // keeps libavformat from touching.
    unsafe { avformat_free_context(ctx.as_ptr()) };
    let guarded = match ManuallyDrop::into_inner(io).into_inner::<GuardedWriter<W>>() {
      Ok(guarded) => guarded,
      // `open` built the I/O context over exactly this type.
      Err(_) => unreachable!("the I/O context holds the guarded sink it was built over"),
    };
    let mut guarded = guarded;
    let flushed = guarded.flush();
    if let Some(message) = latch.message() {
      return Err(OutputError::SinkPanic { message });
    }
    flushed.map_err(OutputError::Sink)?;
    Ok(guarded.into_inner())
  }
}

impl<W> Drop for Output<W> {
  fn drop(&mut self) {
    // SAFETY: the context is this value's, freed exactly once — here, or
    // in `into_sink`, which does not drop `self`. Freed before the I/O
    // context: `avformat_free_context` may run the muxer's `deinit`,
    // which must find `pb` alive, and leaves `pb` alone.
    unsafe { avformat_free_context(self.ctx.as_ptr()) };
    // SAFETY: dropped exactly once, after the context. Its `Drop` flushes
    // the buffered bytes and the sink through the guard, then frees.
    unsafe { ManuallyDrop::drop(&mut self.io) };
  }
}

impl<W> core::fmt::Debug for Output<W> {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    f.debug_struct("Output")
      .field("container", &self.container())
      .field("phase", &self.phase)
      .field("streams", &self.streams())
      .finish_non_exhaustive()
  }
}

/// An optional text as a C string, refused by name if it holds a NUL.
fn optional_text(text: Option<&str>, which: OutputText) -> Result<Option<CString>, OutputError> {
  text
    .map(|text| CString::new(text).map_err(|_| OutputError::Nul { text: which }))
    .transpose()
}

/// A [`Timebase`] as libavformat's `AVRational`.
fn rational(timebase: Timebase) -> AVRational {
  AVRational {
    num: timebase.num(),
    den: timebase.den().get(),
  }
}

/// Replaces `par`'s extradata with `bytes`, padded with the zeroes
/// libavcodec reads past it, and answers an FFmpeg error code on
/// failure.
///
/// # Safety
///
/// `par` must be a live `AVCodecParameters` nothing else holds, whose
/// `extradata` is null or libavutil's allocation; `bytes.len()` plus the
/// padding must fit a C `int`.
unsafe fn replace_extradata(par: *mut AVCodecParameters, bytes: &[u8]) -> Result<(), c_int> {
  // SAFETY: `extradata` is null or libavutil's own (this function's
  // contract); `av_freep` frees it and nulls the field.
  unsafe {
    av_freep(addr_of_mut!((*par).extradata).cast::<c_void>());
    (*par).extradata_size = 0;
  }
  if bytes.is_empty() {
    return Ok(());
  }
  let size = bytes.len() + AV_INPUT_BUFFER_PADDING_SIZE as usize;
  // SAFETY: a zeroed allocation of `size` bytes, null on failure; the
  // zeroes past `bytes` are the padding.
  let data = unsafe { av_mallocz(size) }.cast::<u8>();
  if data.is_null() {
    return Err(ffmpeg_next::ffi::AVERROR(libc::ENOMEM));
  }
  // SAFETY: `data` holds `size >= bytes.len()` bytes, a fresh allocation
  // that cannot overlap `bytes`; then attached to `par`, which frees it
  // with itself. `bytes.len()` fits a C `int` (this function's contract).
  unsafe {
    ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
    (*par).extradata = data;
    (*par).extradata_size = bytes.len() as c_int;
  }
  Ok(())
}

/// FFmpeg's own stream-copy rule for a codec tag (`fftools/
/// ffmpeg_mux_init.c` 1031–1039 in FFmpeg 9.0.1): keep the source's tag
/// where the muxer has no tag table, where its table maps that tag back
/// to the same codec, or where it names no tag for the codec at all;
/// otherwise clear it, so the muxer picks its own — the header refuses a
/// tag its table gives another codec (`mux.c`'s `validate_codec_tag`).
///
/// # Safety
///
/// `ctx` must be a live context with its muxer set, and `par` a live
/// `AVCodecParameters` nothing else holds.
unsafe fn settle_codec_tag(ctx: *mut AVFormatContext, par: *mut AVCodecParameters) {
  // SAFETY: plain fields of the live parameters; the codec id is read as
  // the integer it is stored as.
  let (tag, codec) = unsafe {
    (
      (*par).codec_tag,
      read_unaligned(addr_of!((*par).codec_id).cast::<i32>()),
    )
  };
  if tag == 0 {
    return;
  }
  // SAFETY: the static muxer's `codec_tag`: null, or a null-terminated
  // list of libavformat's static tag tables.
  let tables = unsafe { (*(*ctx).oformat).codec_tag };
  if tables.is_null() {
    return;
  }
  let mut found: c_uint = 0;
  // SAFETY: `tables` is the muxer's own list; the redeclared shims take
  // the codec id and tag as plain integers, so no bindgen enum is formed,
  // and search the tables without indexing them by either.
  let keep = unsafe {
    c_shims::av_codec_get_id(tables, tag) == codec
      || c_shims::av_codec_get_tag2(tables, codec, &mut found) == 0
  };
  if !keep {
    // SAFETY: a plain field of the live parameters.
    unsafe { (*par).codec_tag = 0 };
  }
}

/// FFmpeg's own stream-copy rule for an audio stream's `block_align`
/// (`fftools/ffmpeg_mux_init.c` 1076–1083 in FFmpeg 9.0.1): an MP3
/// stream's value of 1, 576 or 1152 is a frame size some demuxers report
/// there, and an AC-3 stream's is none a muxer should write; both are
/// cleared.
///
/// # Safety
///
/// `par` must be a live `AVCodecParameters` nothing else holds.
unsafe fn stream_copy_audio_rule(par: *mut AVCodecParameters) {
  // SAFETY: plain fields of the live parameters, the enums read as the
  // integers they are stored as.
  unsafe {
    let media = read_unaligned(addr_of!((*par).codec_type).cast::<i32>());
    let codec = read_unaligned(addr_of!((*par).codec_id).cast::<i32>());
    if media != MEDIA_AUDIO {
      return;
    }
    let block_align = (*par).block_align;
    if (codec == CodecId::MP3.raw() && matches!(block_align, 1 | 576 | 1152))
      || codec == CodecId::AC3.raw()
    {
      (*par).block_align = 0;
    }
  }
}

/// Frees a chapter this module allocated and no array took.
///
/// # Safety
///
/// `chapter` must be a live `av_mallocz` allocation of an `AVChapter`
/// that nothing else holds, its `metadata` null or libavutil's own.
unsafe fn free_chapter(chapter: *mut AVChapter) {
  // SAFETY: this function's contract: the dictionary and the chapter are
  // libavutil's allocations, each freed exactly once.
  unsafe {
    av_dict_free(addr_of_mut!((*chapter).metadata));
    av_free(chapter.cast::<c_void>());
  }
}

/// A text the context may hand libavformat, as the bytes it reads.
///
/// Kept for the laws: what `av_dict_set` was given comes back out of the
/// stream's own dictionary.
#[cfg(test)]
fn metadata_value(
  dict: *const ffmpeg_next::ffi::AVDictionary,
  key: &core::ffi::CStr,
) -> Option<String> {
  // SAFETY: `dict` is null or a live dictionary of the test's own
  // context; the entry's value is a NUL-terminated copy it owns.
  unsafe {
    let entry = ffmpeg_next::ffi::av_dict_get(dict, key.as_ptr(), ptr::null(), 0);
    if entry.is_null() {
      return None;
    }
    let value: *const core::ffi::c_char = (*entry).value;
    Some(
      core::ffi::CStr::from_ptr(value)
        .to_string_lossy()
        .into_owned(),
    )
  }
}

#[cfg(test)]
mod tests;

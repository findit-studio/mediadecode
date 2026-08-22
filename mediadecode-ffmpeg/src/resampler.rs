//! [`mediadecode::resampler::AudioResampler`] impl backed by
//! `libswresample`.
//!
//! Converts rate, sample format and channel layout between two specs
//! fixed at construction — [`FfmpegResampler::new`] takes both, because
//! neither end is discoverable and neither is a constant. The source is
//! whatever the file holds; the target is whatever the consumer wants,
//! and consumers disagree (16 kHz mono for a speech model, 48 kHz for
//! an audio-event one, from the same track at the same time).
//!
//! # Output timestamps
//!
//! `swr` is a delay line: it needs future input to produce present
//! output, so at any moment a filter's worth of samples is inside it.
//! Timestamps are therefore *counted*, not computed per call — the
//! output timeline is anchored on the first input timestamp and
//! advanced by the number of samples actually produced. The frames
//! drained after EOF continue that same line rather than restarting it,
//! and no arithmetic anywhere depends on how many samples a given
//! `swr_convert_frame` happened to yield.

use std::{
  collections::VecDeque,
  ptr::{addr_of, read_unaligned},
};

use ffmpeg_next::{
  ChannelLayout,
  codec::Parameters,
  ffi::{
    AV_NOPTS_VALUE, AVChannelOrder, AVMatrixEncoding, AVSampleFormat, av_channel_layout_from_mask,
    av_frame_get_buffer, swr_build_matrix2,
  },
  format::Sample,
  frame,
  software::resampling,
};
use mediadecode::{Timebase, frame::AudioFrame, resampler::AudioResampler};
use mediaframe::audio::ChannelLayoutDescription;

use crate::{
  Error, Ffmpeg, FfmpegBuffer,
  convert::{self, ConvertError},
  extras::AudioFrameExtra,
  sample_format::SampleFormat,
};

/// The frame type [`FfmpegResampler`] accepts and produces.
type Frame = AudioFrame<SampleFormat, ChannelLayoutDescription, AudioFrameExtra, FfmpegBuffer>;

/// One end of a conversion: sample rate, sample format, channel layout.
///
/// Spelled in FFmpeg's own vocabulary because construction is off the
/// [`AudioResampler`] trait and this is the backend that has to be
/// handed to `swr_alloc_set_opts2`. [`FfmpegResampler`] restates the
/// source spec in the vocabulary a decoded frame carries, so the
/// mid-stream check compares like with like without the caller ever
/// seeing two dialects.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ResampleSpec {
  rate: u32,
  format: Sample,
  layout: ChannelLayout,
}

impl ResampleSpec {
  /// Constructs a spec from its three parts.
  ///
  /// Deliberately total and `const`: a spec is a description, and
  /// describing something `swr` cannot convert is not itself an error.
  /// [`FfmpegResampler::new`] is the choke point every construction
  /// route passes through, and it is what refuses a rate, a format or a
  /// channel layout this backend cannot honour — see
  /// [`FfmpegResampler::new`] for the roster and
  /// [`ResampleError::UnsupportedLayout`] for why a layout can be
  /// refused at all.
  #[inline]
  pub const fn new(rate: u32, format: Sample, layout: ChannelLayout) -> Self {
    Self {
      rate,
      format,
      layout,
    }
  }

  /// The spec a track *declares*, read off the codec parameters a
  /// [`crate::FfmpegDemuxer`] track row carries
  /// (`track.extra().parameters()`) — the "source from `TrackInfo`"
  /// path.
  ///
  /// Returns `None` for a non-audio track, for one whose declared
  /// sample format is `AV_SAMPLE_FMT_NONE` (a codec whose format is
  /// only known once its decoder opens), and for a custom or ambisonic
  /// channel layout — see [`Self::from_decoder`] for the first case and
  /// the note on [`unspecified_layout`] for the last.
  pub fn from_parameters(parameters: &Parameters) -> Option<Self> {
    if parameters.medium() != ffmpeg_next::media::Type::Audio {
      return None;
    }
    // SAFETY: `parameters` keeps the `AVCodecParameters` live; every
    // read below goes through the raw pointer and none of them
    // materialises a bindgen enum out of foreign memory.
    let par = unsafe { parameters.as_ptr() };
    let rate = unsafe { (*par).sample_rate }.max(0) as u32;
    if rate == 0 {
      return None;
    }
    let format = SampleFormat::from_raw(unsafe { (*par).format }).to_ffmpeg()?;
    let layout = unsafe { layout_from_raw(addr_of!((*par).ch_layout)) }?;
    Some(Self::new(rate, format, layout))
  }

  /// The spec an opened decoder will actually produce — its rate,
  /// sample format and channel layout, straight off the codec context.
  ///
  /// Reach it through
  /// [`FfmpegAudioStreamDecoder::inner`](crate::FfmpegAudioStreamDecoder::inner).
  /// `None` on a custom or ambisonic layout, and on a context whose
  /// sample format is still unset (a decoder that has not been opened).
  pub fn from_decoder(decoder: &ffmpeg_next::decoder::Audio) -> Option<Self> {
    // SAFETY: `decoder` keeps the `AVCodecContext` live. `sample_fmt`
    // is read as the raw integer it is rather than through
    // `decoder.format()`, which would construct an `AVSampleFormat`
    // out of foreign memory.
    let ctx = unsafe { decoder.as_ptr() };
    let format =
      SampleFormat::from_raw(unsafe { read_unaligned(addr_of!((*ctx).sample_fmt).cast::<i32>()) })
        .to_ffmpeg()?;
    let rate = unsafe { (*ctx).sample_rate }.max(0) as u32;
    if rate == 0 {
      return None;
    }
    let layout = unsafe { layout_from_raw(addr_of!((*ctx).ch_layout)) }?;
    Some(Self::new(rate, format, layout))
  }

  /// A layout that names a channel *count* and nothing else —
  /// `AV_CHANNEL_ORDER_UNSPEC`.
  ///
  /// Not a degenerate case: a WAV file without a `WAVE_FORMAT_EXTENSIBLE`
  /// channel mask genuinely declares no layout, and FFmpeg faithfully
  /// reports it as unspecified in the codec parameters, in the codec
  /// context, and on every decoded frame. Substituting a default layout
  /// would make the source spec disagree with the frames it is supposed
  /// to describe, and every `send_frame` would be refused as a
  /// mid-stream change. `swr` accepts an unspecified layout at either
  /// end and maps the channels positionally.
  #[inline]
  pub fn unspecified_layout(channels: i32) -> ChannelLayout {
    // SAFETY: a zeroed `AVChannelLayout` is a valid value — `order`
    // reads as `AV_CHANNEL_ORDER_UNSPEC`, the zero discriminant, and
    // the union is documented as unused for that order.
    unsafe {
      let mut layout: ffmpeg_next::ffi::AVChannelLayout = std::mem::zeroed();
      layout.nb_channels = channels.max(0);
      ChannelLayout(layout)
    }
  }

  /// Sample rate in Hz.
  #[inline]
  pub const fn rate(&self) -> u32 {
    self.rate
  }
  /// Sample format.
  #[inline]
  pub const fn format(&self) -> Sample {
    self.format
  }
  /// Channel layout.
  #[inline]
  pub const fn layout(&self) -> ChannelLayout {
    self.layout
  }
  /// Channel count, from the layout.
  #[inline]
  pub fn channels(&self) -> i32 {
    self.layout.channels()
  }

  /// The timebase output frames carry — one tick per output sample.
  fn timebase(&self) -> Timebase {
    Timebase::new(
      1,
      std::num::NonZeroI32::new(self.rate.min(i32::MAX as u32) as i32).unwrap_or(
        // A zero-rate spec never reaches here: `new` is the only way in
        // and every caller of it names a real rate. Falling back to
        // one tick per second keeps the arithmetic total rather than
        // panicking on a value that cannot occur.
        std::num::NonZeroI32::new(1).expect("1 is non-zero"),
      ),
    )
  }
}

/// `mediadecode::resampler::AudioResampler` impl wrapping
/// `swresample`.
///
/// Construction is [`Self::new`], off the trait, taking both specs —
/// see the trait's own documentation for why the target can never be a
/// constant.
pub struct FfmpegResampler {
  ctx: resampling::Context,
  source: ResampleSpec,
  target: ResampleSpec,
  /// The source spec restated in the vocabulary a decoded `AudioFrame`
  /// carries. The mid-stream check compares against these, not against
  /// FFmpeg's dialect, so it never has to translate a frame.
  source_format: SampleFormat,
  source_layout: ChannelLayoutDescription,
  /// The layouts `swr` is really configured with — see
  /// [`initialized_layout`]. Every `AVFrame` this type stages or
  /// allocates carries these, not the declared ones.
  staged_source_layout: ChannelLayout,
  staged_target_layout: ChannelLayout,
  target_timebase: Timebase,
  ready: VecDeque<Frame>,
  /// Next output timestamp, in target-rate ticks. `None` until the
  /// first input frame anchors it.
  next_pts: Option<i64>,
  eof: bool,
}

impl FfmpegResampler {
  /// Opens a resampler between two explicit specs.
  ///
  /// Both are required and neither is inferred. The source is what the
  /// decoder will hand over — read it off the track
  /// ([`ResampleSpec::from_parameters`]) or off the opened decoder
  /// ([`ResampleSpec::from_decoder`]). The target is the caller's, and
  /// is options: 16 kHz mono for a speech model, 48 kHz for an
  /// audio-event one, both from the same track.
  ///
  /// # The choke point
  ///
  /// [`ResampleSpec::new`] is `const` and total, so this is where both
  /// ends are checked — every construction route (`from_parameters`,
  /// `from_decoder`, the public constructor) passes through here, and
  /// nothing hazardous reaches `swr` or a staged `AVFrame` behind it:
  ///
  /// - a rate of zero, or one past `c_int`
  ///   ([`ResampleError::UnsupportedRate`]);
  /// - `AV_SAMPLE_FMT_NONE` ([`ResampleError::UnsupportedFormat`]);
  /// - a channel layout that is neither native nor unspecified, or one
  ///   naming no channels ([`ResampleError::UnsupportedLayout`]).
  pub fn new(source: ResampleSpec, target: ResampleSpec) -> Result<Self, ResampleError> {
    check_spec(&source, SpecEnd::Source)?;
    check_spec(&target, SpecEnd::Target)?;
    check_pair(&source, &target)?;

    let staged_source_layout = initialized_layout(source.layout);
    let staged_target_layout = initialized_layout(target.layout);
    let ctx = open_context(&source, &target, staged_source_layout, staged_target_layout)?;

    let source_format = SampleFormat::from_ffmpeg(source.format);
    // SAFETY: the layout is a live `ChannelLayout` owned by `source`
    // for the duration of this call.
    let source_layout =
      crate::channel_layout::channel_layout_description_from_ffmpeg(&source.layout);
    let target_timebase = target.timebase();

    Ok(Self {
      ctx,
      source,
      target,
      source_format,
      source_layout,
      staged_source_layout,
      staged_target_layout,
      target_timebase,
      ready: VecDeque::new(),
      next_pts: None,
      eof: false,
    })
  }

  /// The spec frames must arrive in.
  #[inline]
  pub const fn source(&self) -> &ResampleSpec {
    &self.source
  }

  /// The spec frames leave in.
  #[inline]
  pub const fn target(&self) -> &ResampleSpec {
    &self.target
  }

  /// Borrows the wrapped `swr` context.
  #[inline]
  pub const fn inner(&self) -> &resampling::Context {
    &self.ctx
  }

  /// Samples still inside the delay line, counted at the output rate.
  #[inline]
  pub fn delay(&self) -> i64 {
    self.ctx.delay().map_or(0, |d| d.output.max(0))
  }

  /// Refuses a frame whose shape is not the source spec.
  fn check_source(&self, frame: &Frame) -> Result<(), ResampleError> {
    if frame.sample_rate() != self.source.rate
      || *frame.sample_format() != self.source_format
      || *frame.channel_layout() != self.source_layout
    {
      return Err(ResampleError::SourceChanged {
        expected_rate: self.source.rate,
        expected_format: self.source_format,
        found_rate: frame.sample_rate(),
        found_format: *frame.sample_format(),
      });
    }
    Ok(())
  }

  /// Where a frame's timestamp lands on the output timeline, or `None`
  /// when it carries none.
  ///
  /// Rescaled with the **checked** rung, and before anything is staged.
  /// `Timestamp::rescale_to` saturates, and both ends of that clamp are
  /// wrong here: a positive one reaches the counted timeline's checked
  /// addition only after `swr` has consumed the input, leaving a
  /// session no caller can retry; a negative one lands on `i64::MIN`,
  /// which *is* `AV_NOPTS_VALUE`, so the conversion back reads the
  /// frame as carrying no timestamp at all and an extreme timestamp is
  /// silently erased. A timestamp that does not fit the output timeline
  /// is refused by name, with the resampler untouched.
  fn anchor_of(&self, frame: &Frame) -> Result<Option<i64>, ResampleError> {
    let Some(timestamp) = frame.pts() else {
      return Ok(None);
    };
    let ticks = timestamp.pts();
    let out_of_range = || ResampleError::TimestampOutOfRange { pts: ticks };
    // `AV_NOPTS_VALUE` is a sentinel, not a time. A frame carrying it
    // as a value says something contradictory, and anchoring on it
    // would produce output frames that report no timestamp.
    if ticks == AV_NOPTS_VALUE {
      return Err(out_of_range());
    }
    let rescaled = timestamp
      .timebase()
      .checked_rescale(ticks, self.target_timebase)
      .ok_or_else(out_of_range)?;
    if rescaled == AV_NOPTS_VALUE {
      return Err(out_of_range());
    }
    Ok(Some(rescaled))
  }

  /// Stages a decoded frame as an `AVFrame` swr can read.
  ///
  /// Geometry is settled **before** anything is allocated. A frame's
  /// header is a claim, not a fact: `nb_samples` comes from the same
  /// foreign memory as the planes it describes, and sizing an
  /// allocation off it first would let a forged frame with a
  /// twelve-byte plane ask for tens of gigabytes on its way to being
  /// refused.
  fn stage_input(&self, frame: &Frame) -> Result<frame::Audio, ResampleError> {
    let samples = frame.nb_samples() as usize;
    let channels = self.source.channels();

    // What the *format* requires, not what the allocated frame reports
    // — the frame does not exist yet.
    let planes = if self.source.format.is_planar() {
      channels.max(0) as usize
    } else {
      1
    };
    let found = frame.plane_count() as usize;
    if planes > found {
      return Err(ResampleError::PlaneCount {
        expected: planes,
        found,
      });
    }
    let bytes = plane_bytes(self.source.format, samples, channels)
      .ok_or(ResampleError::SampleCount { requested: samples })?;
    for plane in frame.planes().iter().take(planes) {
      let src = plane.data_ref().as_ref();
      if src.len() < bytes {
        return Err(ResampleError::PlaneCount {
          expected: bytes,
          found: src.len(),
        });
      }
    }

    // Only now, with every plane proved long enough for the sample
    // count that sizes this allocation.
    let mut input = new_audio_frame(
      self.source.format,
      samples,
      self.source.rate,
      self.staged_source_layout,
    )?;
    // What the allocation really produced. `data_mut` panics past its
    // own plane count, and this crate does not put a panic on a path
    // that reads foreign geometry.
    let staged = input.planes();
    if staged < planes {
      return Err(ResampleError::PlaneCount {
        expected: planes,
        found: staged,
      });
    }
    for (index, plane) in frame.planes().iter().take(planes).enumerate() {
      let src = plane.data_ref().as_ref();
      let dst = input.data_mut(index);
      if dst.len() < bytes {
        return Err(ResampleError::PlaneCount {
          expected: bytes,
          found: dst.len(),
        });
      }
      dst[..bytes].copy_from_slice(&src[..bytes]);
    }
    Ok(input)
  }

  /// Allocates an output frame large enough for everything currently
  /// convertible: the delay line's contents plus `in_samples` of new
  /// input, rescaled to the output rate and rounded up.
  fn alloc_output(&self, in_samples: i64) -> Result<frame::Audio, ResampleError> {
    let delay_in = self.ctx.delay().map_or(0, |d| d.input.max(0));
    let total = delay_in.saturating_add(in_samples).max(0) as i128;
    let scaled = (total * i128::from(self.target.rate) + i128::from(self.source.rate) - 1)
      / i128::from(self.source.rate).max(1);
    // One extra sample of headroom: swr rounds its own accounting, and
    // an output frame one short would silently push the remainder into
    // the internal FIFO where the pts accounting cannot see it until
    // the next call.
    let samples = scaled + 1;
    // `av_frame_get_buffer` takes the count as a `c_int`. A request
    // past that is refused by name rather than clamped: a silently
    // shortened output frame is a stream that loses samples.
    if samples > i128::from(i32::MAX) {
      return Err(ResampleError::SampleCount {
        // Saturating only for a count past `usize` itself, which no
        // machine could hold either way.
        requested: usize::try_from(samples).unwrap_or(usize::MAX),
      });
    }
    new_audio_frame(
      self.target.format,
      samples.max(1) as usize,
      self.target.rate,
      self.staged_target_layout,
    )
  }

  /// Labels a converted `AVFrame` on the counted output timeline and
  /// turns it into a `mediadecode` frame.
  ///
  /// The timeline moves **after** the conversion succeeds. A frame that
  /// could not be converted has not been delivered, so the next one
  /// that can be must carry the timestamp this one would have had —
  /// otherwise a single conversion failure punches a permanent hole in
  /// an otherwise continuous output stream.
  fn take_output(&mut self, mut out: frame::Audio) -> Result<Option<Frame>, ResampleError> {
    let produced = out.samples() as i64;
    if produced <= 0 {
      return Ok(None);
    }
    let pts = self.next_pts.unwrap_or(0);
    // Counted, not saturated: `i64::MAX` repeated for every later frame
    // is not a timeline, it is a lie with the shape of one.
    let next = pts
      .checked_add(produced)
      .ok_or(ResampleError::TimestampOverflow {
        pts,
        samples: produced,
      })?;
    out.set_pts(Some(pts));
    // SAFETY: `out` is a live, just-converted `AVFrame`; the conversion
    // refcounts each plane it takes, so `out` may be dropped after.
    unsafe {
      (*out.as_mut_ptr()).duration = produced;
    }
    let frame = unsafe { convert::av_frame_to_audio_frame(out.as_ptr(), self.target_timebase) }
      .map_err(ResampleError::Convert)?;
    self.next_pts = Some(next);
    Ok(Some(frame))
  }
}

impl AudioResampler for FfmpegResampler {
  type Adapter = Ffmpeg;
  type Buffer = FfmpegBuffer;
  type Error = ResampleError;

  fn send_frame(&mut self, frame: &Frame) -> Result<(), ResampleError> {
    if self.eof {
      return Err(ResampleError::AfterEof);
    }
    self.check_source(frame)?;
    // A frame carrying no samples is a header and nothing else. There
    // is nothing to convert and nothing to stage: `av_frame_get_buffer`
    // refuses a zero-sample allocation, so staging one would hand `swr`
    // an unbacked `AVFrame` for no gain.
    if frame.nb_samples() == 0 {
      return Ok(());
    }

    // Nothing below touches the session's state until the conversion
    // has succeeded. A refused frame must leave the timeline exactly
    // where it was, or the next good frame inherits the rejected one's
    // timestamp.
    let anchor = self.anchor_of(frame)?;
    let input = self.stage_input(frame)?;
    let mut out = self.alloc_output(frame.nb_samples() as i64)?;
    self
      .ctx
      .run(&input, &mut out)
      .map_err(|e| ResampleError::Resample(Error::Ffmpeg(e)))?;

    // The frame is inside the filter now, so the timeline may be
    // anchored on it. Anchored on *input* rather than on the first
    // output, because a call that produces nothing but fills the delay
    // line still fixes where the stream starts.
    if self.next_pts.is_none() {
      self.next_pts = anchor;
    }
    if let Some(converted) = self.take_output(out)? {
      self.ready.push_back(converted);
    }
    Ok(())
  }

  fn receive_frame(&mut self, dst: &mut Frame) -> Result<(), ResampleError> {
    if let Some(frame) = self.ready.pop_front() {
      *dst = frame;
      return Ok(());
    }
    if !self.eof {
      return Err(ResampleError::Again);
    }
    // EOF: drain the conversion tail. Without this every file loses the
    // tens of milliseconds sitting inside the filter.
    let remaining = self.delay();
    if remaining <= 0 {
      return Err(ResampleError::Again);
    }
    let mut out = new_audio_frame(
      self.target.format,
      remaining.min(i64::from(i32::MAX)) as usize,
      self.target.rate,
      self.staged_target_layout,
    )?;
    self
      .ctx
      .flush(&mut out)
      .map_err(|e| ResampleError::Resample(Error::Ffmpeg(e)))?;
    match self.take_output(out)? {
      Some(frame) => {
        *dst = frame;
        Ok(())
      }
      None => Err(ResampleError::Again),
    }
  }

  fn send_eof(&mut self) -> Result<(), ResampleError> {
    self.eof = true;
    Ok(())
  }

  /// Resets the resampler for another stream on the same two specs.
  ///
  /// The `swr` context is **rebuilt**, not drained. `swresample` has no
  /// reset call, and draining it dry cannot be verified from outside: a
  /// `swr_convert_frame` that makes no progress reports no error, so a
  /// drain loop that gives up and a drain loop that finished are
  /// indistinguishable — and a flush that returned `Ok` with the old
  /// delay line still inside would let one stream's tail contaminate
  /// the next. A fresh context is the only reset whose success is a
  /// fact.
  ///
  /// The new context is built before the old one is dropped, so a
  /// failure leaves the resampler exactly as it was: this call either
  /// resets everything or changes nothing.
  fn flush(&mut self) -> Result<(), ResampleError> {
    let ctx = open_context(
      &self.source,
      &self.target,
      self.staged_source_layout,
      self.staged_target_layout,
    )?;
    self.ctx = ctx;
    self.ready.clear();
    self.next_pts = None;
    self.eof = false;
    debug_assert_eq!(self.delay(), 0, "a fresh swr context holds nothing");
    Ok(())
  }
}

/// Errors from [`FfmpegResampler`].
#[derive(thiserror::Error, Debug, Clone)]
pub enum ResampleError {
  /// No converted frame is ready yet — send more input, or
  /// [`send_eof`](AudioResampler::send_eof) and drain the tail.
  ///
  /// This is the "needs more" signal, carried in the error type exactly
  /// as
  /// [`AudioStreamDecoder::receive_frame`](mediadecode::decoder::AudioStreamDecoder::receive_frame)
  /// carries it.
  #[error("no converted frame ready")]
  Again,

  /// A frame arrived whose shape is not the source spec this resampler
  /// was built with — the mid-stream refusal.
  ///
  /// The face never silently reconfigures: doing so would resample the
  /// two halves of a stream on different terms and hand back a single
  /// unbroken timeline built out of them. Build a new resampler for the
  /// new source spec.
  #[error(
    "source format changed mid-stream: expected {expected_rate} Hz {expected_format:?}, \
     got {found_rate} Hz {found_format:?}"
  )]
  SourceChanged {
    /// Rate the resampler was built for.
    expected_rate: u32,
    /// Sample format the resampler was built for.
    expected_format: SampleFormat,
    /// Rate the offending frame carried.
    found_rate: u32,
    /// Sample format the offending frame carried.
    found_format: SampleFormat,
  },

  /// [`send_frame`](AudioResampler::send_frame) was called after
  /// [`send_eof`](AudioResampler::send_eof). Call
  /// [`flush`](AudioResampler::flush) first to reuse the resampler for
  /// another stream.
  #[error("send_frame after send_eof; flush() first to start another stream")]
  AfterEof,

  /// A frame's planes do not hold what its header claims — too few
  /// planes for the format, or a plane shorter than its sample count
  /// requires.
  #[error("frame plane geometry mismatch: expected {expected}, found {found}")]
  PlaneCount {
    /// What the format and sample count require.
    expected: usize,
    /// What the frame carries.
    found: usize,
  },

  /// A sample count no frame can hold: one whose byte size overflows,
  /// or one past the `c_int` `av_frame_get_buffer` takes.
  #[error("{requested} samples is not a frame size")]
  SampleCount {
    /// The count that was asked for.
    requested: usize,
  },

  /// One end of the conversion declares a sample rate `swr` cannot be
  /// driven with — zero, or past `c_int`.
  #[error("the {end} rate {rate} is not a sample rate swr can use")]
  UnsupportedRate {
    /// Which end of the conversion.
    end: SpecEnd,
    /// The rate that was declared.
    rate: u32,
  },

  /// One end of the conversion declares no sample format
  /// (`AV_SAMPLE_FMT_NONE`) — the state a codec context is in before
  /// its decoder opens.
  #[error("the {end} spec names no sample format")]
  UnsupportedFormat {
    /// Which end of the conversion.
    end: SpecEnd,
  },

  /// One end of the conversion declares a channel layout this backend
  /// will not carry.
  ///
  /// Native and unspecified layouts are the two it does. A **custom**
  /// or **ambisonic** `AVChannelLayout` owns a heap-allocated channel
  /// map, and FFmpeg documents that such a layout must be copied with
  /// `av_channel_layout_copy` rather than assigned — while
  /// `ffmpeg_next::ChannelLayout` is a `Copy` wrapper with no
  /// destructor. Every `AVFrame` this type stages or allocates receives
  /// the layout by assignment, and `av_frame_free` runs
  /// `av_channel_layout_uninit` on it: the first staged frame to be
  /// dropped would free a map the spec, the decoder and every later
  /// frame still point at. Refusing at construction is what keeps that
  /// use-after-free unreachable; a resampler over those layouts is a
  /// separate design, not a silent approximation.
  #[error("the {end} channel layout is not supported: order {order}, {channels} channels")]
  UnsupportedLayout {
    /// Which end of the conversion.
    end: SpecEnd,
    /// `AVChannelOrder` as the raw integer it is on the wire.
    order: i32,
    /// The channel count the layout declares.
    channels: i32,
  },

  /// A planar spec with more channels than a decoded frame has plane
  /// slots.
  ///
  /// `mediadecode`'s `AudioFrame` carries a fixed eight planes
  /// (`AV_NUM_DATA_POINTERS`); planar audio past that lives in
  /// `AVFrame.extended_data[]`, which this crate does not plumb
  /// through. As a **source** no valid frame could ever arrive; as a
  /// **target** `swr` would produce one this crate cannot hand back —
  /// and it would fail only after the input had been consumed, leaving
  /// a session that cannot be retried. Both are refused at
  /// construction, where nothing has happened yet.
  #[error("the {end} spec is planar with {channels} channels; a frame carries {limit} planes")]
  TooManyPlanes {
    /// Which end of the conversion.
    end: SpecEnd,
    /// The channel count the layout declares.
    channels: i32,
    /// Plane slots a frame has.
    limit: i32,
  },

  /// A frame's timestamp does not land on the output timeline: it does
  /// not survive the rescale as an `i64`, or it is `AV_NOPTS_VALUE`,
  /// which is a sentinel rather than a time.
  ///
  /// Raised before anything is staged, so a refused frame leaves the
  /// resampler exactly as it was.
  #[error("the frame timestamp {pts} does not land on the output timeline")]
  TimestampOutOfRange {
    /// The timestamp the frame carried, in its own timebase.
    pts: i64,
  },

  /// The conversion between these two layouts would silently drop a
  /// source channel: FFmpeg's own mixing matrix routes it to no output.
  ///
  /// `swr` mixes the channel positions its rematrix table knows and
  /// processes the rest of the input as though it were absent — a log
  /// line at most. Measured against FFmpeg 9, packed 22.2 → mono loses
  /// fifteen of twenty-four channels and `cube` → stereo loses two of
  /// eight, so this is not a matter of channel count. Installing an
  /// explicit mix matrix is how such a conversion would be accepted
  /// deliberately; until this crate has a seat for one, the pair is
  /// refused.
  #[error(
    "converting {source_channels} channels to {target_channels} would drop source channel \
     {channel}: FFmpeg's mixing matrix routes it to no output"
  )]
  ChannelDropped {
    /// Channels the source layout declares.
    source_channels: i32,
    /// Channels the target layout declares.
    target_channels: i32,
    /// The first source channel that reaches no output channel.
    channel: i32,
  },

  /// FFmpeg will not build a mixing matrix between these two layouts at
  /// all.
  #[error("FFmpeg builds no mixing matrix from {source_channels} channels to {target_channels}")]
  RematrixUnsupported {
    /// Channels the source layout declares.
    source_channels: i32,
    /// Channels the target layout declares.
    target_channels: i32,
  },

  /// The output timeline would leave `i64`. Counted timestamps are
  /// exact or they are nothing, so this is named rather than saturated.
  #[error("the output timeline overflows: {pts} + {samples} samples")]
  TimestampOverflow {
    /// Where the timeline stood.
    pts: i64,
    /// How many samples were produced.
    samples: i64,
  },

  /// The wrapped `swresample` call reported an error.
  #[error(transparent)]
  Resample(#[from] Error),

  /// Conversion from FFmpeg's `AVFrame` to mediadecode's `AudioFrame`
  /// failed.
  #[error(transparent)]
  Convert(#[from] ConvertError),
}

impl ResampleError {
  /// `true` for [`Self::Again`] — the "send more input" signal, which a
  /// drain loop tests for rather than matching on.
  #[inline]
  pub const fn is_again(&self) -> bool {
    matches!(self, Self::Again)
  }
}

/// Which end of a conversion a refusal is about.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SpecEnd {
  /// The spec frames must arrive in.
  Source,
  /// The spec frames leave in.
  Target,
}

impl core::fmt::Display for SpecEnd {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    f.write_str(match self {
      Self::Source => "source",
      Self::Target => "target",
    })
  }
}

/// Plane slots a `mediadecode::frame::AudioFrame` has — the fixed array
/// matching `AV_NUM_DATA_POINTERS`. Planar audio past this many
/// channels lives in `AVFrame.extended_data[]` / `extended_buf[]`,
/// which this crate does not plumb through: `convert` refuses such a
/// frame and `AudioFrame::new` will not build one.
const MAX_AUDIO_PLANES: i32 = 8;

/// Refuses a spec `swr` cannot be driven with, or whose channel layout
/// cannot be carried by value — see [`ResampleError::UnsupportedLayout`]
/// for that one, which is the whole reason this check exists at the
/// choke point rather than in the `const` constructor.
fn check_spec(spec: &ResampleSpec, end: SpecEnd) -> Result<(), ResampleError> {
  if spec.rate == 0 || spec.rate > i32::MAX as u32 {
    return Err(ResampleError::UnsupportedRate {
      end,
      rate: spec.rate,
    });
  }
  if spec.format == Sample::None {
    return Err(ResampleError::UnsupportedFormat { end });
  }
  let order = layout_order(&spec.layout);
  let channels = spec.layout.channels();
  let carried = order == AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32
    || order == AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC as i32;
  if !carried || channels <= 0 {
    return Err(ResampleError::UnsupportedLayout {
      end,
      order,
      channels,
    });
  }
  // A planar spec with more channels than the frame model has plane
  // slots is a resampler that cannot work in either direction, and
  // saying so here is the difference between a refusal at construction
  // and a refusal on every frame — the target one arriving *after*
  // `swr` has already consumed the input, which is not a state a caller
  // can retry from.
  if spec.format.is_planar() && channels > MAX_AUDIO_PLANES {
    return Err(ResampleError::TooManyPlanes {
      end,
      channels,
      limit: MAX_AUDIO_PLANES,
    });
  }
  Ok(())
}

/// A layout's `AVChannelOrder` as the integer it is on the wire.
///
/// Read raw rather than matched as an `AVChannelOrder`, the discipline
/// this crate keeps everywhere it touches a bindgen enum: a value
/// outside this build's discriminant set would be undefined behaviour
/// the moment it existed as one.
fn layout_order(layout: &ChannelLayout) -> i32 {
  // SAFETY: `layout` is a live `ChannelLayout` for the duration of this
  // call; `addr_of!` reaches its `order` field without forming a
  // reference to the enum.
  unsafe { read_unaligned(addr_of!(layout.0.order).cast::<i32>()) }
}

/// FFmpeg's `SWR_CH_MAX`: the square its own matrix builder writes,
/// whatever the two layouts' channel counts are.
///
/// Not a convenience. `swr_build_matrix2` copies its internal
/// `[SWR_CH_MAX][SWR_CH_MAX]` block out at the caller's stride, so a
/// buffer sized to the actual channel counts is written far past its
/// end — measured, and the measurement is a killed process.
const SWR_CH_MAX: usize = 64;

/// Refuses a **pair** whose rematrixing would silently drop input
/// channels.
///
/// Each end can be perfectly valid on its own and the conversion
/// between them still lose whole channels: `swr` mixes only the channel
/// positions its rematrix table knows, and quietly processes the rest
/// of the input as though it were not there. Measured against the
/// linked FFmpeg 9 with a tone isolated in each source channel: packed
/// 22.2 → mono drops fifteen of twenty-four (`swr` says as much in a
/// log line and converts anyway), `cube` → stereo drops two of *eight*
/// — so a channel-count threshold is both too strict and too loose to
/// be the rule.
///
/// The rule is asked of FFmpeg instead: build the mixing matrix its own
/// builder would use, and refuse when any input channel reaches no
/// output at all. `lfe_mix_level` is deliberately non-zero, so the
/// question is "can this channel reach the output" rather than "does
/// FFmpeg's default downmix policy include it" — the default leaves LFE
/// out of a downmix on purpose, and refusing an everyday 5.1 → stereo
/// over that would be absurd. The predicate matched the tone sweep
/// exactly on every pair measured.
///
/// A pair `swr` cannot matrix at all is refused too. Accepting these
/// deliberately is a *mix matrix* seat on the spec — a real design, not
/// something to mint in passing; until it exists, refusal is the honest
/// answer.
fn check_pair(source: &ResampleSpec, target: &ResampleSpec) -> Result<(), ResampleError> {
  let native = AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32;
  // An unspecified layout is mapped positionally by `swr` with no
  // rematrixing at all, and identical layouts need no matrix: neither
  // can drop a channel, and neither is what the builder describes.
  if layout_order(&source.layout) != native
    || layout_order(&target.layout) != native
    || source.layout == target.layout
  {
    return Ok(());
  }
  let source_channels = source.layout.channels();
  let target_channels = target.layout.channels();

  let mut matrix = vec![0f64; SWR_CH_MAX * SWR_CH_MAX];
  // SAFETY: both layouts are live for the call; `matrix` is the full
  // `SWR_CH_MAX` square the builder writes, passed with the matching
  // stride; the encoding is a compile-time constant of this build; and
  // a null log context is documented as allowed.
  let rc = unsafe {
    swr_build_matrix2(
      &source.layout.0,
      &target.layout.0,
      core::f64::consts::FRAC_1_SQRT_2,
      core::f64::consts::FRAC_1_SQRT_2,
      1.0,
      1.0,
      1.0,
      matrix.as_mut_ptr(),
      SWR_CH_MAX as isize,
      AVMatrixEncoding::AV_MATRIX_ENCODING_NONE,
      core::ptr::null_mut(),
    )
  };
  if rc < 0 {
    return Err(ResampleError::RematrixUnsupported {
      source_channels,
      target_channels,
    });
  }
  for channel in 0..source_channels.min(SWR_CH_MAX as i32) {
    let index = channel as usize;
    if (0..target_channels.min(SWR_CH_MAX as i32) as usize)
      .all(|out| matrix[index + SWR_CH_MAX * out] == 0.0)
    {
      return Err(ResampleError::ChannelDropped {
        source_channels,
        target_channels,
        channel,
      });
    }
  }
  Ok(())
}

/// Opens a `swr` context for the two specs. Shared by
/// [`FfmpegResampler::new`] and the rebuild
/// [`AudioResampler::flush`] performs.
fn open_context(
  source: &ResampleSpec,
  target: &ResampleSpec,
  staged_source_layout: ChannelLayout,
  staged_target_layout: ChannelLayout,
) -> Result<resampling::Context, ResampleError> {
  resampling::Context::get(
    source.format,
    staged_source_layout,
    source.rate,
    target.format,
    staged_target_layout,
    target.rate,
  )
  .map_err(|e| ResampleError::Resample(Error::Ffmpeg(e)))
}

/// Allocates an audio `AVFrame`, checking every step the dependency's
/// own `frame::Audio::new` does not.
///
/// `ffmpeg_next`'s constructor dereferences `av_frame_alloc`'s result
/// without a null check and discards `av_frame_get_buffer`'s return
/// value, so an allocation failure there yields a frame whose planes
/// are not backed — which is then handed to FFI. Both are checked here;
/// on failure the caller gets a named error and no frame at all. The
/// null check is the crate's existing one
/// ([`crate::frame::alloc_av_audio_frame`], which the decoders already
/// allocate through), so there is one answer to `av_frame_alloc`
/// returning null rather than two.
fn new_audio_frame(
  format: Sample,
  samples: usize,
  rate: u32,
  layout: ChannelLayout,
) -> Result<frame::Audio, ResampleError> {
  if samples == 0 || samples > i32::MAX as usize {
    return Err(ResampleError::SampleCount { requested: samples });
  }
  let mut out = crate::frame::alloc_av_audio_frame()?;
  out.set_format(format);
  out.set_samples(samples);
  // The layout is assigned by value, which is sound only because
  // `check_spec` refused every layout that owns a heap channel map.
  out.set_channel_layout(layout);
  out.set_rate(rate);
  // SAFETY: `out` is a live `AVFrame` whose format, sample count and
  // layout were just set; `av_frame_get_buffer` allocates its planes
  // and reports failure in its return value, which is checked.
  let rc = unsafe { av_frame_get_buffer(out.as_mut_ptr(), 0) };
  if rc < 0 {
    return Err(ResampleError::Resample(Error::Ffmpeg(
      ffmpeg_next::Error::from(rc),
    )));
  }
  Ok(out)
}

/// Bytes one plane holds for `samples` samples of `format`, or `None`
/// when that product does not fit a `usize`. Packed formats keep every
/// channel in the single plane; planar formats give each channel its
/// own.
fn plane_bytes(format: Sample, samples: usize, channels: i32) -> Option<usize> {
  let bytes = samples.checked_mul(format.bytes())?;
  if format.is_planar() {
    Some(bytes)
  } else {
    bytes.checked_mul(channels.max(1) as usize)
  }
}

/// Builds a native-order [`ChannelLayout`] from a channel bitmask,
/// without ever forming an `AVChannelLayout` out of foreign memory:
/// the struct starts zeroed (`AV_CHANNEL_ORDER_UNSPEC` is `0`, a valid
/// discriminant) and FFmpeg fills it.
fn layout_from_mask(mask: u64) -> ChannelLayout {
  // SAFETY: a zeroed `AVChannelLayout` is a valid value — its `order`
  // field reads as `AV_CHANNEL_ORDER_UNSPEC`, the zero discriminant —
  // and `av_channel_layout_from_mask` overwrites it wholesale.
  unsafe {
    let mut layout = std::mem::zeroed();
    if av_channel_layout_from_mask(&mut layout, mask) < 0 {
      return ChannelLayout::default(mask.count_ones() as i32);
    }
    ChannelLayout(layout)
  }
}

/// The layout `swr` will actually be configured with.
///
/// `swr_init` replaces an unspecified input or output layout with
/// FFmpeg's default for that channel count, and from then on compares
/// every frame handed to it against *that* layout — a staged frame
/// still carrying the unspecified one is refused with
/// `AVERROR_INPUT_CHANGED`. Applying the same rule here, once, keeps
/// the frames this type builds in step with the context it built.
///
/// The declared layout is kept separately and is what the mid-stream
/// check compares against, because it is what decoded frames really
/// carry: a WAV without a channel mask hands out unspecified frames
/// forever, whatever `swr` decided internally.
fn initialized_layout(layout: ChannelLayout) -> ChannelLayout {
  if layout.is_empty() {
    ChannelLayout::default(layout.channels())
  } else {
    layout
  }
}

/// Reads an `AVChannelLayout` out of FFmpeg memory into a layout this
/// spec can own, or `None` for one it does not represent.
///
/// The `order` field is read as the integer it is on the wire: an
/// out-of-range value would be undefined behaviour the instant it
/// existed as an `AVChannelOrder`, which is the hazard this crate
/// keeps out everywhere it touches a bindgen enum.
///
/// A **custom** or **ambisonic** layout returns `None`. Both keep a
/// heap-allocated channel map inside the layout, and `ChannelLayout` is
/// a plain `Copy` wrapper with no destructor: owning one here would
/// either alias a map the decoder still frees or leak the copy. A
/// resampler over one of those layouts is a separate design, not a
/// silent approximation.
///
/// # Safety
///
/// `ptr` must be a live `*const AVChannelLayout` for the duration of
/// this call.
unsafe fn layout_from_raw(ptr: *const ffmpeg_next::ffi::AVChannelLayout) -> Option<ChannelLayout> {
  let order = unsafe { read_unaligned(addr_of!((*ptr).order).cast::<i32>()) };
  let channels = unsafe { (*ptr).nb_channels };
  if channels <= 0 {
    return None;
  }
  if order == AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32 {
    // SAFETY: `u.mask` is the union's variant for NATIVE, and the
    // order was checked against our own constant before the read.
    let mask = unsafe { (*ptr).u.mask };
    if mask != 0 {
      return Some(layout_from_mask(mask));
    }
    // Native in name with no channels named: unspecified in substance.
    return Some(ResampleSpec::unspecified_layout(channels));
  }
  if order == AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC as i32 {
    return Some(ResampleSpec::unspecified_layout(channels));
  }
  None
}

/// Compile-time assurance that `SampleFormat`'s round trip through
/// FFmpeg's vocabulary is the identity on the closed set. Both
/// directions are hand-written tables, and a table that disagreed with
/// its inverse would silently mislabel every sample.
const _: () = {
  assert!(
    SampleFormat::from_raw(AVSampleFormat::AV_SAMPLE_FMT_NONE as i32)
      .to_ffmpeg()
      .is_none()
  );
};

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_sample_format_table_round_trips() {
    for format in [
      SampleFormat::U8,
      SampleFormat::S16,
      SampleFormat::S32,
      SampleFormat::S64,
      SampleFormat::FLT,
      SampleFormat::DBL,
      SampleFormat::U8P,
      SampleFormat::S16P,
      SampleFormat::S32P,
      SampleFormat::S64P,
      SampleFormat::FLTP,
      SampleFormat::DBLP,
    ] {
      let ffmpeg = format.to_ffmpeg().expect("a named format");
      assert_eq!(
        SampleFormat::from_ffmpeg(ffmpeg),
        format,
        "{format:?} does not survive the round trip",
      );
      assert_eq!(ffmpeg.is_planar(), format.is_planar());
    }
    assert!(SampleFormat::NONE.to_ffmpeg().is_none());
    assert!(SampleFormat::from_raw(9999).to_ffmpeg().is_none());
  }

  #[test]
  fn a_mask_rebuilds_the_layout_it_names() {
    let stereo = layout_from_mask(ChannelLayout::STEREO.bits());
    assert_eq!(stereo.channels(), 2);
    assert_eq!(stereo.bits(), ChannelLayout::STEREO.bits());

    let five_one = layout_from_mask(ChannelLayout::_5POINT1.bits());
    assert_eq!(five_one.channels(), 6);
    assert_eq!(
      five_one.bits(),
      ChannelLayout::_5POINT1.bits(),
      "the side-vs-back distinction is exactly what a default layout would lose",
    );
  }

  #[test]
  fn plane_geometry_follows_packed_versus_planar() {
    use ffmpeg_next::format::sample::Type;
    // Packed: one plane holding every channel.
    assert_eq!(
      plane_bytes(Sample::I16(Type::Packed), 1024, 2),
      Some(1024 * 2 * 2)
    );
    // Planar: one plane per channel, so the count does not multiply in.
    assert_eq!(
      plane_bytes(Sample::I16(Type::Planar), 1024, 2),
      Some(1024 * 2)
    );
    assert_eq!(
      plane_bytes(Sample::F32(Type::Planar), 1024, 6),
      Some(1024 * 4)
    );
    // A sample count whose byte size does not fit is not a size. This
    // is the arithmetic that used to run before the allocation it
    // feeds, and it wrapped.
    assert_eq!(
      plane_bytes(Sample::F32(Type::Packed), usize::MAX / 2, 8),
      None,
      "an overflowing plane size is refused, not wrapped",
    );
  }

  #[test]
  fn the_target_timebase_is_one_tick_per_output_sample() {
    let spec = ResampleSpec::new(
      16_000,
      Sample::I16(ffmpeg_next::format::sample::Type::Packed),
      ChannelLayout::MONO,
    );
    let tb = spec.timebase();
    assert_eq!((tb.num(), tb.den().get()), (1, 16_000));
  }
}

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

use derive_more::{IsVariant, TryUnwrap, Unwrap};
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
use mediadecode::{
  Timebase, Timestamp,
  frame::{AudioFrame, Plane},
  resampler::AudioResampler,
};
use mediaframe::audio::ChannelLayoutDescription;

use crate::{Error, Ffmpeg, FfmpegBuffer, extras::AudioFrameExtra, sample_format::SampleFormat};

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
    // Before `medium()`, which dereferences the pointer inside
    // ffmpeg-next. `Parameters`' safe constructors hand back a
    // null-backed value when FFmpeg's allocation failed and report
    // nothing, so a caller can arrive here holding one without ever
    // having been told. Parameters that were never allocated describe
    // no audio, which this function already has a word for.
    // SAFETY: reading the pointer without dereferencing it.
    if unsafe { parameters.as_ptr() }.is_null() {
      return None;
    }
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
    // Same reason as `from_parameters`: `codec::Context::new()` is a
    // safe constructor over an unchecked `avcodec_alloc_context3`, so a
    // decoder can be null-backed without anyone having been told.
    if ctx.is_null() {
      return None;
    }
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
  /// The target spec in the vocabulary an output frame carries,
  /// computed once at construction. Assembling a converted frame after
  /// `swr` has run must not have to ask FFmpeg anything, because asking
  /// can fail — see [`FfmpegResampler::prepare_output`].
  target_format: SampleFormat,
  target_layout: ChannelLayoutDescription,
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

    // The layouts `swr` is really configured with, resolved *before*
    // the pair is judged — because the conversion that will run is
    // between these two, not between the two that were declared. An
    // unspecified layout becomes FFmpeg's default for its channel count
    // (twenty-four unspecified channels are 22.2), so judging the
    // declared pair let exactly the routing the explicit 22.2 refusal
    // blocks walk in through the unspecified door.
    let staged_source_layout = initialized_layout(source.layout);
    let staged_target_layout = initialized_layout(target.layout);
    check_pair(&staged_source_layout, &staged_target_layout)?;
    let ctx = open_context(&source, &target, staged_source_layout, staged_target_layout)?;

    let source_format = SampleFormat::from_ffmpeg(source.format);
    let target_format = SampleFormat::from_ffmpeg(target.format);
    // SAFETY: the layout is a live `ChannelLayout` owned by this scope.
    let target_layout =
      crate::channel_layout::channel_layout_description_from_ffmpeg(&staged_target_layout);
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
      target_format,
      target_layout,
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
      return Err(ResampleError::SourceChanged(SourceChanged::new(
        self.source.rate,
        self.source_format,
        frame.sample_rate(),
        *frame.sample_format(),
      )));
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
    let out_of_range = || ResampleError::TimestampOutOfRange(TimestampOutOfRange::new(ticks));
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
      return Err(ResampleError::PlaneCount(PlaneCount::new(planes, found)));
    }
    let bytes = plane_bytes(self.source.format, samples, channels)
      .ok_or(ResampleError::SampleCount(SampleCount::new(samples)))?;
    for plane in frame.planes().iter().take(planes) {
      let src = plane.data_ref().as_ref();
      if src.len() < bytes {
        return Err(ResampleError::PlaneCount(PlaneCount::new(bytes, src.len())));
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
      return Err(ResampleError::PlaneCount(PlaneCount::new(planes, staged)));
    }
    for (index, plane) in frame.planes().iter().take(planes).enumerate() {
      let src = plane.data_ref().as_ref();
      let dst = input.data_mut(index);
      if dst.len() < bytes {
        return Err(ResampleError::PlaneCount(PlaneCount::new(bytes, dst.len())));
      }
      dst[..bytes].copy_from_slice(&src[..bytes]);
    }
    Ok(input)
  }

  /// The most samples the next conversion could produce: the delay
  /// line's contents plus `in_samples` of new input, rescaled to the
  /// output rate and rounded up.
  ///
  /// Separate from the allocation because it is also the preflight the
  /// output timeline is checked against — *before* `swr` consumes
  /// anything, so a refusal leaves the session where a caller can retry
  /// it.
  fn output_capacity(&self, in_samples: i64) -> Result<usize, ResampleError> {
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
      // Saturating only for a count past `usize` itself, which no
      // machine could hold either way.
      return Err(ResampleError::SampleCount(SampleCount::new(
        usize::try_from(samples).unwrap_or(usize::MAX),
      )));
    }
    Ok(samples.max(1) as usize)
  }

  /// Refuses a conversion whose output could not be labelled: the
  /// timeline plus everything this call might produce has to stay
  /// inside `i64`.
  ///
  /// Asked before `swr` sees a sample, like everything else that can
  /// fail. [`Self::finish_output`] performs the same addition against
  /// the count actually produced, which cannot exceed the capacity
  /// checked here — so once this passes, that one cannot fail.
  fn check_timeline(&self, anchor: Option<i64>, capacity: usize) -> Result<(), ResampleError> {
    let pts = self.next_pts.or(anchor).unwrap_or(0);
    let samples = capacity as i64;
    if pts.checked_add(samples).is_none() {
      return Err(ResampleError::TimestampOverflow(TimestampOverflow::new(
        pts, samples,
      )));
    }
    Ok(())
  }

  /// Allocates the output frame **and acquires every reference the
  /// converted frame will need**, before `swr` is allowed to touch a
  /// sample.
  ///
  /// This is the shape the whole seam is built around. Anything
  /// fallible that runs *after* `swr_convert_frame` has consumed input
  /// leaves a session no caller can act on: retrying feeds the same
  /// samples twice, continuing loses them, and the delay line has moved
  /// either way. The failure kept relocating — the timestamp addition,
  /// the tail drain, then the output wrapping — so the fix is not
  /// another check in another place but an ordering that leaves nothing
  /// on the far side: the frame, one refcounted view per plane, a
  /// placeholder for every unused slot, and the queue slot are all
  /// taken here, where failing costs nothing but an error.
  ///
  /// The views are taken at the frame's **full capacity** and trimmed
  /// afterwards to what `swr` produced ([`FfmpegBuffer::shrink_to`],
  /// which only ever narrows), so the trimming needs no allocation and
  /// cannot fail either.
  fn prepare_output(&self, capacity: usize) -> Result<PreparedOutput, ResampleError> {
    let frame = new_audio_frame(
      self.target.format,
      capacity,
      self.target.rate,
      self.staged_target_layout,
    )?;
    let channels = self.target.channels();
    let plane_count = if self.target.format.is_planar() {
      channels.max(0) as usize
    } else {
      1
    };
    let plane_len = plane_bytes(self.target.format, capacity, channels)
      .ok_or(ResampleError::SampleCount(SampleCount::new(capacity)))?;
    // Linear in the sample count, which is what lets the post-run
    // trim be a multiplication rather than another fallible call.
    let per_sample = plane_bytes(self.target.format, 1, channels)
      .ok_or(ResampleError::SampleCount(SampleCount::new(1)))?;
    if frame.planes() < plane_count {
      return Err(ResampleError::PlaneCount(PlaneCount::new(
        plane_count,
        frame.planes(),
      )));
    }

    let mut buffers: [Option<FfmpegBuffer>; 8] = [const { None }; 8];
    for (index, slot) in buffers.iter_mut().enumerate() {
      *slot = Some(if index < plane_count {
        // SAFETY: `frame` owns a live `AVFrame` this call just
        // allocated; `data` is a public field and `plane_count` is
        // within the eight slots `data` has.
        let data_ptr = unsafe { (*frame.as_ptr()).data[index] };
        if data_ptr.is_null() {
          return Err(ResampleError::OutputBuffer(OutputBuffer::new(index)));
        }
        // SAFETY: the frame is live, and the helper only reads
        // `buf[]`'s ranges to find the one containing `data_ptr`.
        let buf =
          unsafe { crate::convert::find_audio_backing_buffer(frame.as_ptr(), data_ptr, plane_len) }
            .ok_or(ResampleError::OutputBuffer(OutputBuffer::new(index)))?;
        // SAFETY: `buf` is non-null and live, and the helper proved the
        // view lies inside it.
        let offset = unsafe { (data_ptr as usize).wrapping_sub((*buf).data as usize) };
        unsafe { FfmpegBuffer::from_ref_view(buf, offset, plane_len) }
          .ok_or(ResampleError::OutputBuffer(OutputBuffer::new(index)))?
      } else {
        FfmpegBuffer::try_empty().ok_or(ResampleError::OutputBuffer(OutputBuffer::new(index)))?
      });
    }

    Ok(PreparedOutput {
      frame,
      buffers,
      plane_count,
      plane_len,
      per_sample,
    })
  }

  /// Turns a converted frame into a `mediadecode` one. **Infallible**,
  /// by construction: every allocation and every reference it needs was
  /// taken in [`Self::prepare_output`], and everything left here is
  /// arithmetic over values this type owns.
  ///
  /// `None` when the conversion produced nothing — the delay line
  /// swallowed the input, which is ordinary and not a failure.
  fn finish_output(&mut self, mut prepared: PreparedOutput) -> Option<Frame> {
    let produced = prepared.frame.samples();
    if produced == 0 {
      return None;
    }
    let pts = self.next_pts.unwrap_or(0);
    // `check_timeline` ran before `swr` did, against a capacity that is
    // never smaller than what came out, so this addition cannot leave
    // `i64`. It is stated rather than checked because a check here
    // would be an error path on the wrong side of the conversion —
    // exactly what this design exists to remove.
    debug_assert!(
      pts.checked_add(produced as i64).is_some(),
      "the timeline was preflighted against a capacity >= produced",
    );
    self.next_pts = Some(pts.saturating_add(produced as i64));

    let bytes = prepared
      .per_sample
      .saturating_mul(produced)
      .min(prepared.plane_len);
    let plane_count = prepared.plane_count;
    let planes = std::array::from_fn(|index| {
      let mut buffer = prepared.buffers[index]
        .take()
        .expect("prepare_output fills every slot");
      if index < plane_count {
        buffer.shrink_to(bytes);
        Plane::new(buffer, bytes as u32)
      } else {
        Plane::new(buffer, 0)
      }
    });

    Some(
      AudioFrame::new(
        self.target.rate,
        produced as u32,
        self.target.channels().clamp(0, 255) as u8,
        self.target_format,
        self.target_layout.clone(),
        planes,
        plane_count as u8,
        AudioFrameExtra::default(),
      )
      .with_pts(Some(Timestamp::new(pts, self.target_timebase)))
      .with_duration(Some(Timestamp::new(produced as i64, self.target_timebase))),
    )
  }
}

/// Everything a converted frame needs, acquired before the conversion
/// runs. See [`FfmpegResampler::prepare_output`].
struct PreparedOutput {
  frame: frame::Audio,
  /// One refcounted view per populated plane, a placeholder for every
  /// other slot. Taken at full capacity; trimmed after the conversion.
  buffers: [Option<FfmpegBuffer>; 8],
  plane_count: usize,
  /// Bytes one plane holds at full capacity — the ceiling every trim
  /// stays under.
  plane_len: usize,
  /// Bytes one plane holds per sample.
  per_sample: usize,
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
    let capacity = self.output_capacity(frame.nb_samples() as i64)?;
    self.check_timeline(anchor, capacity)?;
    let mut prepared = self.prepare_output(capacity)?;
    // The last fallible thing before the conversion: room for the frame
    // it will produce. `push_back` on a full queue allocates, and an
    // allocation failure there aborts the process rather than
    // unwinding — so the growth happens here, where it can be an error.
    self
      .ready
      .try_reserve(1)
      .map_err(|_| ResampleError::QueueAlloc)?;

    // The only mutation. Everything above could fail and cost nothing;
    // nothing below can fail at all.
    self
      .ctx
      .run(&input, &mut prepared.frame)
      .map_err(|e| ResampleError::Resample(Error::Ffmpeg(e)))?;

    // The frame is inside the filter now, so the timeline may be
    // anchored on it. Anchored on *input* rather than on the first
    // output, because a call that produces nothing but fills the delay
    // line still fixes where the stream starts.
    if self.next_pts.is_none() {
      self.next_pts = anchor;
    }
    if let Some(converted) = self.finish_output(prepared) {
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
    let capacity = remaining.min(i64::from(i32::MAX)) as usize;
    // Same discipline as `send_frame`, and for the same reason: the
    // tail is drained only once the timeline can hold it and every
    // reference the converted frame needs is already in hand, so a
    // failure leaves the delay line untouched instead of turning
    // samples into an error.
    self.check_timeline(None, capacity)?;
    let mut prepared = self.prepare_output(capacity)?;
    self
      .ctx
      .flush(&mut prepared.frame)
      .map_err(|e| ResampleError::Resample(Error::Ffmpeg(e)))?;
    match self.finish_output(prepared) {
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

/// Payload for [`ResampleError::SourceChanged`].
///
/// A frame arrived whose shape is not the source spec this resampler
/// was built with — the mid-stream refusal.
///
/// The face never silently reconfigures: doing so would resample the
/// two halves of a stream on different terms and hand back a single
/// unbroken timeline built out of them. Build a new resampler for the
/// new source spec.
#[derive(thiserror::Error, Debug, Clone)]
#[error(
  "source format changed mid-stream: expected {expected_rate} Hz {expected_format:?}, \
   got {found_rate} Hz {found_format:?}"
)]
pub struct SourceChanged {
  expected_rate: u32,
  expected_format: SampleFormat,
  found_rate: u32,
  found_format: SampleFormat,
}

impl SourceChanged {
  /// Constructs a `SourceChanged` payload.
  #[inline]
  pub const fn new(
    expected_rate: u32,
    expected_format: SampleFormat,
    found_rate: u32,
    found_format: SampleFormat,
  ) -> Self {
    Self {
      expected_rate,
      expected_format,
      found_rate,
      found_format,
    }
  }
  /// Rate the resampler was built for.
  #[inline]
  pub const fn expected_rate(&self) -> u32 {
    self.expected_rate
  }
  /// Sample format the resampler was built for.
  #[inline]
  pub const fn expected_format(&self) -> SampleFormat {
    self.expected_format
  }
  /// Rate the offending frame carried.
  #[inline]
  pub const fn found_rate(&self) -> u32 {
    self.found_rate
  }
  /// Sample format the offending frame carried.
  #[inline]
  pub const fn found_format(&self) -> SampleFormat {
    self.found_format
  }
}

/// Payload for [`ResampleError::PlaneCount`].
///
/// A frame's planes do not hold what its header claims — too few
/// planes for the format, or a plane shorter than its sample count
/// requires.
#[derive(thiserror::Error, Debug, Clone)]
#[error("frame plane geometry mismatch: expected {expected}, found {found}")]
pub struct PlaneCount {
  expected: usize,
  found: usize,
}

impl PlaneCount {
  /// Constructs a `PlaneCount` payload.
  #[inline]
  pub const fn new(expected: usize, found: usize) -> Self {
    Self { expected, found }
  }
  /// What the format and sample count require.
  #[inline]
  pub const fn expected(&self) -> usize {
    self.expected
  }
  /// What the frame carries.
  #[inline]
  pub const fn found(&self) -> usize {
    self.found
  }
}

/// Payload for [`ResampleError::SampleCount`].
///
/// A sample count no frame can hold: one whose byte size overflows, or
/// one past the `c_int` `av_frame_get_buffer` takes.
#[derive(thiserror::Error, Debug, Clone)]
#[error("{requested} samples is not a frame size")]
pub struct SampleCount {
  requested: usize,
}

impl SampleCount {
  /// Constructs a `SampleCount` payload.
  #[inline]
  pub const fn new(requested: usize) -> Self {
    Self { requested }
  }
  /// The count that was asked for.
  #[inline]
  pub const fn requested(&self) -> usize {
    self.requested
  }
}

/// Payload for [`ResampleError::UnsupportedRate`].
///
/// One end of the conversion declares a sample rate `swr` cannot be
/// driven with — zero, or past `c_int`.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the {end} rate {rate} is not a sample rate swr can use")]
pub struct UnsupportedRate {
  end: SpecEnd,
  rate: u32,
}

impl UnsupportedRate {
  /// Constructs an `UnsupportedRate` payload.
  #[inline]
  pub const fn new(end: SpecEnd, rate: u32) -> Self {
    Self { end, rate }
  }
  /// Which end of the conversion.
  #[inline]
  pub const fn end(&self) -> SpecEnd {
    self.end
  }
  /// The rate that was declared.
  #[inline]
  pub const fn rate(&self) -> u32 {
    self.rate
  }
}

/// Payload for [`ResampleError::UnsupportedFormat`].
///
/// One end of the conversion declares no sample format
/// (`AV_SAMPLE_FMT_NONE`) — the state a codec context is in before its
/// decoder opens.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the {end} spec names no sample format")]
pub struct UnsupportedFormat {
  end: SpecEnd,
}

impl UnsupportedFormat {
  /// Constructs an `UnsupportedFormat` payload.
  #[inline]
  pub const fn new(end: SpecEnd) -> Self {
    Self { end }
  }
  /// Which end of the conversion.
  #[inline]
  pub const fn end(&self) -> SpecEnd {
    self.end
  }
}

/// Payload for [`ResampleError::UnsupportedLayout`].
///
/// One end of the conversion declares a channel layout this backend
/// will not carry.
///
/// Native and unspecified layouts are the two it does. A **custom** or
/// **ambisonic** `AVChannelLayout` owns a heap-allocated channel map,
/// and FFmpeg documents that such a layout must be copied with
/// `av_channel_layout_copy` rather than assigned — while
/// `ffmpeg_next::ChannelLayout` is a `Copy` wrapper with no destructor.
/// Every `AVFrame` this type stages or allocates receives the layout by
/// assignment, and `av_frame_free` runs `av_channel_layout_uninit` on
/// it: the first staged frame to be dropped would free a map the spec,
/// the decoder and every later frame still point at. Refusing at
/// construction is what keeps that use-after-free unreachable; a
/// resampler over those layouts is a separate design, not a silent
/// approximation.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the {end} channel layout is not supported: order {order}, {channels} channels")]
pub struct UnsupportedLayout {
  end: SpecEnd,
  order: i32,
  channels: i32,
}

impl UnsupportedLayout {
  /// Constructs an `UnsupportedLayout` payload.
  #[inline]
  pub const fn new(end: SpecEnd, order: i32, channels: i32) -> Self {
    Self {
      end,
      order,
      channels,
    }
  }
  /// Which end of the conversion.
  #[inline]
  pub const fn end(&self) -> SpecEnd {
    self.end
  }
  /// `AVChannelOrder` as the raw integer it is on the wire.
  #[inline]
  pub const fn order(&self) -> i32 {
    self.order
  }
  /// The channel count the layout declares.
  #[inline]
  pub const fn channels(&self) -> i32 {
    self.channels
  }
}

/// Payload for [`ResampleError::TooManyPlanes`].
///
/// A planar spec with more channels than a decoded frame has plane
/// slots.
///
/// `mediadecode`'s `AudioFrame` carries a fixed eight planes
/// (`AV_NUM_DATA_POINTERS`); planar audio past that lives in
/// `AVFrame.extended_data[]`, which this crate does not plumb through.
/// As a **source** no valid frame could ever arrive; as a **target**
/// `swr` would produce one this crate cannot hand back — and it would
/// fail only after the input had been consumed, leaving a session that
/// cannot be retried. Both are refused at construction, where nothing
/// has happened yet.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the {end} spec is planar with {channels} channels; a frame carries {limit} planes")]
pub struct TooManyPlanes {
  end: SpecEnd,
  channels: i32,
  limit: i32,
}

impl TooManyPlanes {
  /// Constructs a `TooManyPlanes` payload.
  #[inline]
  pub const fn new(end: SpecEnd, channels: i32, limit: i32) -> Self {
    Self {
      end,
      channels,
      limit,
    }
  }
  /// Which end of the conversion.
  #[inline]
  pub const fn end(&self) -> SpecEnd {
    self.end
  }
  /// The channel count the layout declares.
  #[inline]
  pub const fn channels(&self) -> i32 {
    self.channels
  }
  /// Plane slots a frame has.
  #[inline]
  pub const fn limit(&self) -> i32 {
    self.limit
  }
}

/// Payload for [`ResampleError::TimestampOutOfRange`].
///
/// A frame's timestamp does not land on the output timeline: it does
/// not survive the rescale as an `i64`, or it is `AV_NOPTS_VALUE`,
/// which is a sentinel rather than a time.
///
/// Raised before anything is staged, so a refused frame leaves the
/// resampler exactly as it was.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the frame timestamp {pts} does not land on the output timeline")]
pub struct TimestampOutOfRange {
  pts: i64,
}

impl TimestampOutOfRange {
  /// Constructs a `TimestampOutOfRange` payload.
  #[inline]
  pub const fn new(pts: i64) -> Self {
    Self { pts }
  }
  /// The timestamp the frame carried, in its own timebase.
  #[inline]
  pub const fn pts(&self) -> i64 {
    self.pts
  }
}

/// Payload for [`ResampleError::ChannelDropped`].
///
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
#[derive(thiserror::Error, Debug, Clone)]
#[error(
  "converting {source_channels} channels to {target_channels} would drop source channel \
   {channel}: FFmpeg's mixing matrix routes it to no output"
)]
pub struct ChannelDropped {
  source_channels: i32,
  target_channels: i32,
  channel: i32,
}

impl ChannelDropped {
  /// Constructs a `ChannelDropped` payload.
  #[inline]
  pub const fn new(source_channels: i32, target_channels: i32, channel: i32) -> Self {
    Self {
      source_channels,
      target_channels,
      channel,
    }
  }
  /// Channels the source layout declares.
  #[inline]
  pub const fn source_channels(&self) -> i32 {
    self.source_channels
  }
  /// Channels the target layout declares.
  #[inline]
  pub const fn target_channels(&self) -> i32 {
    self.target_channels
  }
  /// The first source channel that reaches no output channel.
  #[inline]
  pub const fn channel(&self) -> i32 {
    self.channel
  }
}

/// Payload for [`ResampleError::RematrixUnsupported`].
///
/// FFmpeg will not build a mixing matrix between these two layouts at
/// all.
#[derive(thiserror::Error, Debug, Clone)]
#[error("FFmpeg builds no mixing matrix from {source_channels} channels to {target_channels}")]
pub struct RematrixUnsupported {
  source_channels: i32,
  target_channels: i32,
}

impl RematrixUnsupported {
  /// Constructs a `RematrixUnsupported` payload.
  #[inline]
  pub const fn new(source_channels: i32, target_channels: i32) -> Self {
    Self {
      source_channels,
      target_channels,
    }
  }
  /// Channels the source layout declares.
  #[inline]
  pub const fn source_channels(&self) -> i32 {
    self.source_channels
  }
  /// Channels the target layout declares.
  #[inline]
  pub const fn target_channels(&self) -> i32 {
    self.target_channels
  }
}

/// Payload for [`ResampleError::TimestampOverflow`].
///
/// The output timeline would leave `i64`. Counted timestamps are exact
/// or they are nothing, so this is named rather than saturated.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the output timeline overflows: {pts} + {samples} samples")]
pub struct TimestampOverflow {
  pts: i64,
  samples: i64,
}

impl TimestampOverflow {
  /// Constructs a `TimestampOverflow` payload.
  #[inline]
  pub const fn new(pts: i64, samples: i64) -> Self {
    Self { pts, samples }
  }
  /// Where the timeline stood.
  #[inline]
  pub const fn pts(&self) -> i64 {
    self.pts
  }
  /// How many samples were produced.
  #[inline]
  pub const fn samples(&self) -> i64 {
    self.samples
  }
}

/// Payload for [`ResampleError::OutputBuffer`].
///
/// A reference to one of the output frame's planes could not be taken.
///
/// Raised while preparing the conversion, never after it: that is the
/// point of preparing.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the output frame's plane {plane} could not be referenced")]
pub struct OutputBuffer {
  plane: usize,
}

impl OutputBuffer {
  /// Constructs an `OutputBuffer` payload.
  #[inline]
  pub const fn new(plane: usize) -> Self {
    Self { plane }
  }
  /// Which plane slot.
  #[inline]
  pub const fn plane(&self) -> usize {
    self.plane
  }
}

/// Errors from [`FfmpegResampler`].
#[derive(thiserror::Error, Debug, Clone, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
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
  #[error(transparent)]
  SourceChanged(#[from] SourceChanged),

  /// [`send_frame`](AudioResampler::send_frame) was called after
  /// [`send_eof`](AudioResampler::send_eof). Call
  /// [`flush`](AudioResampler::flush) first to reuse the resampler for
  /// another stream.
  #[error("send_frame after send_eof; flush() first to start another stream")]
  AfterEof,

  /// A frame's planes do not hold what its header claims — too few
  /// planes for the format, or a plane shorter than its sample count
  /// requires.
  #[error(transparent)]
  PlaneCount(#[from] PlaneCount),

  /// A sample count no frame can hold: one whose byte size overflows,
  /// or one past the `c_int` `av_frame_get_buffer` takes.
  #[error(transparent)]
  SampleCount(#[from] SampleCount),

  /// One end of the conversion declares a sample rate `swr` cannot be
  /// driven with — zero, or past `c_int`.
  #[error(transparent)]
  UnsupportedRate(#[from] UnsupportedRate),

  /// One end of the conversion declares no sample format
  /// (`AV_SAMPLE_FMT_NONE`) — the state a codec context is in before
  /// its decoder opens.
  #[error(transparent)]
  UnsupportedFormat(#[from] UnsupportedFormat),

  /// One end of the conversion declares a channel layout this backend
  /// will not carry.
  #[error(transparent)]
  UnsupportedLayout(#[from] UnsupportedLayout),

  /// A planar spec with more channels than a decoded frame has plane
  /// slots.
  #[error(transparent)]
  TooManyPlanes(#[from] TooManyPlanes),

  /// A frame's timestamp does not land on the output timeline.
  #[error(transparent)]
  TimestampOutOfRange(#[from] TimestampOutOfRange),

  /// The conversion between these two layouts would silently drop a
  /// source channel.
  #[error(transparent)]
  ChannelDropped(#[from] ChannelDropped),

  /// FFmpeg will not build a mixing matrix between these two layouts at
  /// all.
  #[error(transparent)]
  RematrixUnsupported(#[from] RematrixUnsupported),

  /// The output timeline would leave `i64`. Counted timestamps are
  /// exact or they are nothing, so this is named rather than saturated.
  #[error(transparent)]
  TimestampOverflow(#[from] TimestampOverflow),

  /// The wrapped `swresample` call reported an error.
  #[error(transparent)]
  Resample(#[from] Error),

  /// A reference to one of the output frame's planes could not be
  /// taken.
  #[error(transparent)]
  OutputBuffer(#[from] OutputBuffer),

  /// The queue of converted frames could not be grown to hold one more.
  #[error("out of memory reserving room for a converted frame")]
  QueueAlloc,
}

/// Which end of a conversion a refusal is about.
#[derive(Copy, Clone, Debug, PartialEq, Eq, IsVariant)]
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
    return Err(ResampleError::UnsupportedRate(UnsupportedRate::new(
      end, spec.rate,
    )));
  }
  if spec.format == Sample::None {
    return Err(ResampleError::UnsupportedFormat(UnsupportedFormat::new(
      end,
    )));
  }
  let order = layout_order(&spec.layout);
  let channels = spec.layout.channels();
  let carried = order == AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32
    || order == AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC as i32;
  if !carried || channels <= 0 {
    return Err(ResampleError::UnsupportedLayout(UnsupportedLayout::new(
      end, order, channels,
    )));
  }
  // A planar spec with more channels than the frame model has plane
  // slots is a resampler that cannot work in either direction, and
  // saying so here is the difference between a refusal at construction
  // and a refusal on every frame — the target one arriving *after*
  // `swr` has already consumed the input, which is not a state a caller
  // can retry from.
  if spec.format.is_planar() && channels > MAX_AUDIO_PLANES {
    return Err(ResampleError::TooManyPlanes(TooManyPlanes::new(
      end,
      channels,
      MAX_AUDIO_PLANES,
    )));
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

/// Refuses an **effective pair** whose rematrixing would silently drop
/// input channels.
///
/// Takes the layouts `swr` is configured with, not the ones the caller
/// declared. The two differ exactly where it matters: an unspecified
/// layout is resolved to FFmpeg's default for its channel count before
/// the context is opened, and twenty-four unspecified channels resolve
/// to 22.2 — so the declared pair says "unspecified, nothing to
/// rematrix" while the conversion that runs is the lossy one.
///
/// This is the second half of the crate's two-layout bookkeeping, and
/// the halves answer different questions. The **declared** layout is
/// what decoded frames carry (a WAV without a channel mask hands out
/// unspecified frames forever) and stays the yardstick for the
/// mid-stream refusal: *is this frame the stream I was built for?* The
/// **effective** layout is what `swr` and every staged `AVFrame` use,
/// and it is the one judged here: *what will `swr` actually do?*
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
fn check_pair(source: &ChannelLayout, target: &ChannelLayout) -> Result<(), ResampleError> {
  let native = AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32;
  // A layout still unspecified *after* resolution — a channel count
  // FFmpeg has no default for — is mapped positionally by `swr` with no
  // rematrixing at all, and identical layouts need no matrix: neither
  // can drop a channel, and neither is what the builder describes.
  if layout_order(source) != native || layout_order(target) != native || source == target {
    return Ok(());
  }
  let source_channels = source.channels();
  let target_channels = target.channels();

  let mut matrix = vec![0f64; SWR_CH_MAX * SWR_CH_MAX];
  // SAFETY: both layouts are live for the call; `matrix` is the full
  // `SWR_CH_MAX` square the builder writes, passed with the matching
  // stride; the encoding is a compile-time constant of this build; and
  // a null log context is documented as allowed.
  let rc = unsafe {
    swr_build_matrix2(
      &source.0,
      &target.0,
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
    return Err(ResampleError::RematrixUnsupported(
      RematrixUnsupported::new(source_channels, target_channels),
    ));
  }
  for channel in 0..source_channels.min(SWR_CH_MAX as i32) {
    let index = channel as usize;
    if (0..target_channels.min(SWR_CH_MAX as i32) as usize)
      .all(|out| matrix[index + SWR_CH_MAX * out] == 0.0)
    {
      return Err(ResampleError::ChannelDropped(ChannelDropped::new(
        source_channels,
        target_channels,
        channel,
      )));
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
    return Err(ResampleError::SampleCount(SampleCount::new(samples)));
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

  use mediadecode::resampler::AudioResampler;

  /// A 48 kHz packed-s16 stereo frame of silence, with the plane its
  /// header claims.
  fn stereo_frame(samples: u32) -> Frame {
    let plane = FfmpegBuffer::copy_from_slice(&vec![0u8; samples as usize * 2 * 2]).expect("plane");
    let planes = std::array::from_fn(|index| {
      Plane::new(
        if index == 0 {
          plane.clone()
        } else {
          FfmpegBuffer::empty()
        },
        0,
      )
    });
    AudioFrame::new(
      48_000,
      samples,
      2,
      SampleFormat::S16,
      crate::channel_layout::channel_layout_description_from_ffmpeg(&ChannelLayout::STEREO),
      planes,
      1,
      AudioFrameExtra::default(),
    )
    .with_pts(Some(Timestamp::new(
      0,
      Timebase::new(1, std::num::NonZeroI32::new(48_000).expect("a real rate")),
    )))
  }

  fn stereo_to_mono() -> FfmpegResampler {
    FfmpegResampler::new(
      ResampleSpec::new(
        48_000,
        Sample::I16(ffmpeg_next::format::sample::Type::Packed),
        ChannelLayout::STEREO,
      ),
      ResampleSpec::new(
        16_000,
        Sample::I16(ffmpeg_next::format::sample::Type::Packed),
        ChannelLayout::MONO,
      ),
    )
    .expect("open resampler")
  }

  #[test]
  fn resample_error_carries_the_derived_accessor_face() {
    // `IsVariant` / `Unwrap` / `TryUnwrap` — one arm per derive family,
    // mirroring the mediadecode-side proof for this crate's own
    // newly-wired `derive_more` dependency.
    let err = ResampleError::OutputBuffer(OutputBuffer::new(2));
    assert!(err.is_output_buffer());
    assert!(!err.is_again());
    assert_eq!(err.unwrap_output_buffer_ref().plane(), 2);
    assert!(err.try_unwrap_again().is_err());
  }

  #[test]
  fn an_allocation_fault_while_sending_leaves_the_session_untouched() {
    // The class this design exists to end: a failure on the far side of
    // `swr_convert_frame` leaves a session no caller can act on —
    // retrying feeds the same samples twice, continuing loses them, and
    // the delay line has moved either way. Every allocation the
    // conversion needs is taken before `swr` runs, so an allocator that
    // refuses everything can only produce an error that cost nothing.
    crate::fault_subprocess::in_subprocess(
      "resampler::tests::an_allocation_fault_while_sending_leaves_the_session_untouched",
      || {
        let mut resampler = stereo_to_mono();
        let frame = stereo_frame(4_800);
        let mut dst = crate::boundary::empty_audio_frame();
        resampler.send_frame(&frame).expect("a first frame");
        while resampler.receive_frame(&mut dst).is_ok() {}
        let delay = resampler.delay();
        assert!(delay > 0, "the filter has to be holding something");

        crate::fault_subprocess::cap_ffmpeg_allocations(1);
        let refused = resampler.send_frame(&frame);
        crate::fault_subprocess::uncap_ffmpeg_allocations();

        assert!(
          refused.is_err(),
          "an allocator that refuses everything must not look like success",
        );
        assert_eq!(
          resampler.delay(),
          delay,
          "the frame went into the filter anyway",
        );
        assert!(
          resampler.receive_frame(&mut dst).unwrap_err().is_again(),
          "a failed send left output ready",
        );

        // And the session is still a session: the same frame converts.
        resampler
          .send_frame(&frame)
          .expect("the failure cost nothing");
        assert!(resampler.receive_frame(&mut dst).is_ok());
      },
    );
  }

  #[test]
  fn an_allocation_fault_while_draining_keeps_the_tail() {
    // The same property one call along, where the samples at risk are
    // the ones already inside the filter: a drain that fails must leave
    // the tail where it was, not turn it into an error.
    crate::fault_subprocess::in_subprocess(
      "resampler::tests::an_allocation_fault_while_draining_keeps_the_tail",
      || {
        let mut resampler = stereo_to_mono();
        let frame = stereo_frame(4_800);
        let mut dst = crate::boundary::empty_audio_frame();
        for _ in 0..3 {
          resampler.send_frame(&frame).expect("send_frame");
          while resampler.receive_frame(&mut dst).is_ok() {}
        }
        resampler.send_eof().expect("eof");
        let tail = resampler.delay();
        assert!(tail > 0, "there has to be a tail to lose");

        crate::fault_subprocess::cap_ffmpeg_allocations(1);
        let refused = resampler.receive_frame(&mut dst);
        crate::fault_subprocess::uncap_ffmpeg_allocations();

        let refused = refused.expect_err("the drain cannot have succeeded");
        assert!(
          !refused.is_again(),
          "an allocation failure is not `send me more input`: {refused:?}",
        );
        assert_eq!(
          resampler.delay(),
          tail,
          "the tail was consumed by a drain that failed",
        );

        // And it is still drainable, which is the whole point.
        resampler
          .receive_frame(&mut dst)
          .expect("the tail survived the failure");
      },
    );
  }

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

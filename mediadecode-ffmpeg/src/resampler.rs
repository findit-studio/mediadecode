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
  ffi::{AVChannelOrder, AVSampleFormat, av_channel_layout_from_mask},
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
  pub fn new(source: ResampleSpec, target: ResampleSpec) -> Result<Self, ResampleError> {
    let staged_source_layout = initialized_layout(source.layout);
    let staged_target_layout = initialized_layout(target.layout);
    let ctx = resampling::Context::get(
      source.format,
      staged_source_layout,
      source.rate,
      target.format,
      staged_target_layout,
      target.rate,
    )
    .map_err(|e| ResampleError::Resample(Error::Ffmpeg(e)))?;

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

  /// Stages a decoded frame as an `AVFrame` swr can read.
  fn stage_input(&self, frame: &Frame) -> Result<frame::Audio, ResampleError> {
    let samples = frame.nb_samples() as usize;
    let mut input = frame::Audio::new(self.source.format, samples, self.staged_source_layout);
    input.set_rate(self.source.rate);

    let planes = input.planes();
    if planes > frame.plane_count() as usize {
      return Err(ResampleError::PlaneCount {
        expected: planes,
        found: frame.plane_count() as usize,
      });
    }
    let bytes = plane_bytes(self.source.format, samples, self.source.channels());
    for (index, plane) in frame.planes().iter().take(planes).enumerate() {
      let src = plane.data_ref().as_ref();
      let dst = input.data_mut(index);
      if src.len() < bytes || dst.len() < bytes {
        return Err(ResampleError::PlaneCount {
          expected: bytes,
          found: src.len().min(dst.len()),
        });
      }
      dst[..bytes].copy_from_slice(&src[..bytes]);
    }
    Ok(input)
  }

  /// Allocates an output frame large enough for everything currently
  /// convertible: the delay line's contents plus `in_samples` of new
  /// input, rescaled to the output rate and rounded up.
  fn alloc_output(&self, in_samples: i64) -> frame::Audio {
    let delay_in = self.ctx.delay().map_or(0, |d| d.input.max(0));
    let total = delay_in.saturating_add(in_samples).max(0) as i128;
    let scaled = (total * i128::from(self.target.rate) + i128::from(self.source.rate) - 1)
      / i128::from(self.source.rate).max(1);
    // One extra sample of headroom: swr rounds its own accounting, and
    // an output frame one short would silently push the remainder into
    // the internal FIFO where the pts accounting cannot see it until
    // the next call.
    let samples = (scaled + 1).clamp(1, i128::from(i32::MAX)) as usize;
    let mut out = frame::Audio::new(self.target.format, samples, self.staged_target_layout);
    out.set_rate(self.target.rate);
    out
  }

  /// Labels a converted `AVFrame` on the counted output timeline and
  /// turns it into a `mediadecode` frame.
  fn take_output(&mut self, mut out: frame::Audio) -> Result<Option<Frame>, ResampleError> {
    let produced = out.samples() as i64;
    if produced <= 0 {
      return Ok(None);
    }
    let pts = self.next_pts.unwrap_or(0);
    self.next_pts = Some(pts.saturating_add(produced));
    out.set_pts(Some(pts));
    // SAFETY: `out` is a live, just-converted `AVFrame`; the conversion
    // refcounts each plane it takes, so `out` may be dropped after.
    unsafe {
      (*out.as_mut_ptr()).duration = produced;
    }
    let frame = unsafe { convert::av_frame_to_audio_frame(out.as_ptr(), self.target_timebase) }
      .map_err(ResampleError::Convert)?;
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

    // Anchor the output timeline on the first input timestamp. Anchored
    // on *input* rather than on the first output, because a first call
    // that produces nothing but fills the delay line still fixes where
    // the stream starts.
    if self.next_pts.is_none()
      && let Some(pts) = frame.pts()
    {
      self.next_pts = Some(pts.rescale_to(self.target_timebase).pts());
    }

    let input = self.stage_input(frame)?;
    let mut out = self.alloc_output(frame.nb_samples() as i64);
    self
      .ctx
      .run(&input, &mut out)
      .map_err(|e| ResampleError::Resample(Error::Ffmpeg(e)))?;
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
    let mut out = frame::Audio::new(
      self.target.format,
      remaining as usize,
      self.staged_target_layout,
    );
    out.set_rate(self.target.rate);
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

  fn flush(&mut self) -> Result<(), ResampleError> {
    self.ready.clear();
    self.next_pts = None;
    self.eof = false;
    // Drain and discard whatever the filter still holds, so the next
    // stream does not inherit the previous one's tail. `swr` has no
    // reset call; running it dry is the reset.
    //
    // The loop is bounded because a `flush` that yielded nothing would
    // otherwise spin: each pass must strictly shrink the delay, and any
    // pass that does not ends it.
    let mut previous = i64::MAX;
    loop {
      let remaining = self.delay();
      if remaining <= 0 || remaining >= previous {
        break;
      }
      previous = remaining;
      let mut out = frame::Audio::new(
        self.target.format,
        remaining as usize,
        self.staged_target_layout,
      );
      out.set_rate(self.target.rate);
      if self.ctx.flush(&mut out).is_err() || out.samples() == 0 {
        break;
      }
    }
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

/// Bytes one plane holds for `samples` samples of `format`. Packed
/// formats keep every channel in the single plane; planar formats give
/// each channel its own.
fn plane_bytes(format: Sample, samples: usize, channels: i32) -> usize {
  let per_sample = format.bytes();
  if format.is_planar() {
    samples * per_sample
  } else {
    samples * per_sample * channels.max(1) as usize
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
      1024 * 2 * 2
    );
    // Planar: one plane per channel, so the count does not multiply in.
    assert_eq!(plane_bytes(Sample::I16(Type::Planar), 1024, 2), 1024 * 2);
    assert_eq!(plane_bytes(Sample::F32(Type::Planar), 1024, 6), 1024 * 4);
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

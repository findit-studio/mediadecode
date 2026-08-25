//! Conversion helpers from FFmpeg `AVFrame` / `AVPacket` to the
//! `mediadecode` types parameterized by [`crate::Ffmpeg`] and
//! `FfmpegBytes`.
//!
//! Every plane is **copied once**, here, out of FFmpeg's
//! `AVBufferRef` and into Rust-owned memory — the
//! [D-seat amputation contract][law]. Through 0.8 the video path
//! exported a refcounted *view* into libavcodec's own allocation
//! whenever the stride happened to be tight, and copied only when it
//! was padded; a consumer therefore inherited an FFmpeg lifetime it
//! could not see, on some frames and not others. 0.9 copies both
//! branches. What is unchanged is the *shape* each branch produces —
//! a tight plane keeps the decoder's `linesize` as its stride, a
//! padded one is compacted to `row_bytes` — because that geometry is
//! what consumers read, and the amputation is about ownership, not
//! about relaying out the picture.
//!
//! # Header fields: the validation-order census
//!
//! Every number in this module comes out of an `AVFrame` a file chose
//! the contents of, and each one is answerable to two questions —
//! *what judges it*, and *what reads it first*. When the second
//! precedes the first, the judgement is being made against a value its
//! own consumer has already laundered, which is not a judgement. That
//! is not hypothetical: it is how a declared `-1` channel count reached
//! a ceiling as a legitimate-looking `0`, having been floored by the
//! very helper the ceiling was supposed to run before.
//!
//! So the order is censused rather than assumed. Every raw header field
//! these three paths read, with its validator and its first consumer:
//!
//! | path | field | validator | first consumer | order |
//! |---|---|---|---|---|
//! | audio | `nb_samples` | `< 0` → [`InvalidSampleCount`] | the byte product | validator first |
//! | audio | `ch_layout.nb_channels` | `< 0`, `> 255`, `== 0` with samples → [`UnsupportedChannelCount`] | `channel_layout_description_from_raw_ptr` | **was inverted — hoisted** |
//! | audio | `format` | `bytes_per_sample()` → [`UnsupportedSampleFormat`] | `is_planar()`, for the plane count | validator first |
//! | audio | `linesize[0]` | `< 0`, and `== 0` with samples → [`InvalidPlaneLayout`] | `allocated_per_plane` | validator first |
//! | audio | `sample_rate` | none — censused metadata | `AudioFrame::new` | no geometry rides it |
//! | audio | `data[i]` | null check, then the backing-buffer proof | the copy | validator first |
//! | picture | `width` / `height` | `< 0` → [`InvalidDimensions`] | `copy_out_planes`' pixel ceiling | **was inverted — hoisted** |
//! | picture | `format` | `is_deliverable` → unsupported-format | `plane_geometry` | validator first |
//! | picture | `linesize[i]` | `<= 0` **and** `< row_bytes[i]` → [`InvalidPlaneLayout`] | its own pass, after the budget and before any copy | validator first |
//! | picture | `crop_*` | `checked_add` per pair, then `sum < extent` | the rect | validator first |
//! | picture | `nb_side_data`, entry `size` (still road) | the entry cap and [`FrameLimits::max_image_side_data_bytes`](crate::FrameLimits::max_image_side_data_bytes) | the plane copy, then the side-data copy | **was inverted — hoisted ahead of `copy_out_planes`** |
//! | picture | colour enums, `pict_type` | the raw `i32` fold, which is total | the fold's own output | the fold *is* the validator |
//! | packet | `flags` (`AV_PKT_FLAG_TRUSTED`) | [`crate::buffer::TrustedPayload`], both legs | the payload copy | validator first |
//!
//! # The open-C-enum sweep, including this crate's own code
//!
//! The same discipline, applied to *entry points* rather than fields: a
//! value read out of FFmpeg memory as a closed Rust enum is undefined
//! behaviour before any comparison on it can run, and FFmpeg extends
//! these enums in ABI-compatible releases.
//!
//! | caller | entry point | enum | closed by |
//! |---|---|---|---|
//! | image / video / audio / subtitle open | `Decoder::{video,audio,subtitle}()` | `AVCodecID`, `AVMediaType` | `find_decoder` (raw `u32`) + `ensure_codec_type` (raw `i32`) |
//! | track build, attachment classify, resampler spec, `Debug` | `Parameters::medium()` | `AVMediaType` | `boundary::media_kind_of`, a total fold |
//! | **the pixel-format census** | `av_pix_fmt_desc_get_id` | `AVPixelFormat` | local `c_int` shim |
//! | **the pixel-format census** | `av_image_get_buffer_size` | `AVPixelFormat` | local `c_int` shim |
//! | **the sample-format census** | `av_get_bytes_per_sample` | `AVSampleFormat` | local `c_int` shim |
//! | HW format negotiation | `get_format` callback list | `AVPixelFormat` | walked as `*const i32` |
//!
//! # The dimension-vocabulary sweep
//!
//! A frame has more than one extent, and a judge that reads the wrong
//! one is not a judge. `AVFrame.width`/`.height` are the **display**
//! dims; what gets *allocated* is the **coded** extent on the software
//! road and the **frames-context pool** on the hardware one. On a
//! cropped stream they diverge without limit — measured on this build,
//! an h264 clip carrying SPS cropping shows 32x32 display over a
//! 1920x1088 coded surface, a 2040x gap.
//!
//! Every site that reads a dimension, and which vocabulary it needs:
//!
//! | site | reads | sizes what | verdict |
//! |---|---|---|---|
//! | `judge_buffer` | `AVFrame.width/height` at `get_buffer2` | the software allocation's **cost** | **correct**: measured, libavcodec hands this hook the frame at *coded* extent (1920x1088, aligned 1920x1090, 2,092,831 bytes), and the footprint prices those aligned dims against `max_frame_bytes`. Logical extent is not this seat's question — `max_pixels` is enforced by `ff_set_dimensions` against the **raw** dims, which is the semantics it has |
//! | `get_hw_format` | `AVCodecContext.coded_width/height` | the hardware pool | **correct, and new**: the display dims `max_pixels` was checked against are blind to it |
//! | `judge_hw_transfer` | the frames-context pool dims | the transfer's CPU destination | **was display — repriced** |
//! | `estimate_transfer_bytes` | the frames-context pool dims | the probe's pending budget | correct already, and its doc named this trap first |
//! | `drain_into_pending` (two sites) | `AVFrame.width/height` | **nothing** — log fields only | benign |
//! | `VideoDecoder::width/height` | the decoder's display dims | nothing; a public accessor | correct — display is what a caller is asking for |
//! | `copy_out_planes` | the converted frame's own extent | the plane copy | correct — a decoded CPU frame's extent *is* its allocation |
//!
//! The pattern worth keeping: **the extent to judge is the one the
//! allocator will use, and it is never assumed — it is read from
//! whatever structure the allocation is sized from.** Where that
//! structure cannot be read, the judge fails closed, because an
//! unprovable extent is not a small one.
//!
//! And the capstone the whole series arrives at, which generalises both
//! tables above:
//!
//! > **A judge must dominate the allocator's arithmetic, not the
//! > payload's.**
//!
//! Every ceiling here answers "may this be allocated?", so the number
//! it compares has to be what the *allocator* will take — not what the
//! bytes nominally weigh, not what a tight layout would cost, and not
//! what the header displays. The two differ by under one percent on
//! ordinary frames, which is precisely why every under-pricing defect
//! in this release hid behind a shape big enough for the slack not to
//! show: `nv12` 16x16 is 384 bytes of pixels and a 1,792-byte
//! allocation, a one-sample eight-channel planar frame is 16 bytes of
//! samples and 768 allocated, and `yuv420p` 1920x1080 is 3,110,400
//! against 3,133,696. See [`crate::footprint`], where the pricing lives
//! and where the estimates are verified against real allocations rather
//! than argued.
//!
//! The last three rows of the enum table above are the class **inside
//! this crate's own new code**, and the census rows are its sharpest instance: that code
//! exists precisely to price formats this build's bindings may not
//! name, and the binding it called handed those ids back as a closed
//! `AVPixelFormat`. Every future format would have become an invalid
//! enum value on the way into the pricing meant to handle it — the
//! census would have been undefined behaviour on exactly its reason for
//! existing. Writing the discipline down was not enough; it had to be
//! re-applied to the code that enforces it.
//!
//! The still road's side-data judgement is the same lesson one level
//! up, about passes rather than fields: it was correct, and it ran
//! after `copy_out_planes`, so an over-budget still had already bought
//! up to `max_frame_bytes` of plane copies before its annotations were
//! totalled. It reads only header fields and allocates nothing, so it
//! now runs with the other free judgements. **Everything a conversion
//! can refuse is refused before anything it can allocate is
//! allocated.**
//!
//! The picture road's byte ceiling is now judged from the **geometry
//! alone** — the format's row width times its row count, which no
//! number the frame chose can influence — so it runs before any stride
//! is so much as read. Then every stride is judged, in its own pass,
//! before a single plane is bought: a layout fault is a property of the
//! frame, knowable before any of it is paid for, and discovering it
//! three plane allocations in was how a refused frame still cost three
//! allocations.
//!
//! The colour row is the shape to copy: a fold that cannot fail and
//! maps everything unknown onto a named "not stated" leaves nothing for
//! an order to get wrong.
//!
//! [law]: mediadecode::adapter#the-d-seat-amputation-contract
use core::ptr::{addr_of, read_unaligned};

use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::ffi::{
  AV_NOPTS_VALUE, AVChromaLocation, AVColorPrimaries, AVColorRange, AVColorSpace,
  AVColorTransferCharacteristic, AVFrame, AVFrameSideDataType, AVPictureType, AVSubtitleType,
};
use mediadecode::{
  PixelFormat, Timebase, Timestamp,
  color::{ChromaLocation, ColorInfo, ColorMatrix, ColorPrimaries, ColorRange, ColorTransfer},
  frame::{AudioFrame, Dimensions, ImageFrame, Plane, Rect, SubtitleFrame, VideoFrame},
  subtitle::{Bitmap as SubtitleBitmap, SubtitlePayload, Text as SubtitleText},
};
use mediaframe::audio::ChannelLayoutDescription;
use smol_str::SmolStr;

use crate::{
  boundary,
  buffer::FfmpegBytes,
  extras::{
    AudioFrameExtra, ImageFrameExtra, ImageOrientation, PictureType, SideDataEntry,
    SubtitleFrameExtra, VideoFrameExtra,
  },
  limits::FrameLimits,
  pixdesc,
  sample_format::SampleFormat,
};

/// Payload for [`ConvertError::UnsupportedPixelFormat`].
///
/// The frame's pixel format isn't in the closed CPU-format set this
/// crate supports for safe per-plane access.
#[derive(Debug, Clone)]
pub struct UnsupportedPixelFormat {
  format: PixelFormat,
  raw: i32,
  name: Option<SmolStr>,
}

impl UnsupportedPixelFormat {
  /// Constructs an `UnsupportedPixelFormat` payload.
  #[inline]
  pub const fn new(format: PixelFormat, raw: i32, name: Option<SmolStr>) -> Self {
    Self { format, raw, name }
  }

  /// The unified vocabulary's answer for [`Self::raw`].
  ///
  /// [`PixelFormat::None`] whenever the raw integer has no mapping — a
  /// hardware surface, a Bayer mosaic, a format FFmpeg gained after
  /// this build. That is a *value*, not a failed lookup, and it is
  /// deliberately not made to carry the integer: [`Self::raw`] and
  /// [`Self::name`] are where the identity survives.
  #[inline]
  pub const fn format(&self) -> &PixelFormat {
    &self.format
  }
  /// The raw `AVFrame.format` integer, exactly as FFmpeg wrote it.
  ///
  /// Present at every tier — it costs one `i32` — because it is the
  /// only field that is always available and always precise. Without
  /// it the message for the fall-through case says `None` and names
  /// nothing at all.
  #[inline]
  pub const fn raw(&self) -> i32 {
    self.raw
  }
  /// FFmpeg's own name for [`Self::raw`] (`av_get_pix_fmt_name`), when
  /// libavutil has one.
  ///
  /// `None` for an integer libavutil does not describe — a corrupt
  /// read, or a format from a newer library than the one linked.
  #[inline]
  pub fn name(&self) -> Option<&str> {
    self.name.as_deref()
  }
}

impl core::fmt::Display for UnsupportedPixelFormat {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match &self.name {
      Some(name) => write!(
        f,
        "convert: unsupported pixel format {:?} (AVPixelFormat {} = {name:?})",
        self.format, self.raw
      ),
      None => write!(
        f,
        "convert: unsupported pixel format {:?} (AVPixelFormat {}, unnamed by libavutil)",
        self.format, self.raw
      ),
    }
  }
}

/// Payload for [`ConvertError::InvalidPlaneLayout`].
///
/// A plane reported `linesize <= 0` or otherwise inconsistent layout.
#[derive(Debug, Clone, Copy)]
pub struct InvalidPlaneLayout {
  plane: usize,
}

impl InvalidPlaneLayout {
  /// Constructs an `InvalidPlaneLayout` payload.
  #[inline]
  pub const fn new(plane: usize) -> Self {
    Self { plane }
  }
  /// Plane index.
  #[inline]
  pub const fn plane(&self) -> usize {
    self.plane
  }
}

impl core::fmt::Display for InvalidPlaneLayout {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(f, "convert: invalid layout on plane {}", self.plane)
  }
}

/// Payload for [`ConvertError::BufferAcquireFailed`].
///
/// A plane's `data[i]` does not lie inside any of the frame's own
/// `buf[]` allocations, so its extent cannot be proved and nothing may
/// be read from it.
///
/// **A fact about the frame, not about the moment.** An exhausted
/// allocator is [`CarrierAllocFailed`] — the two were one arm once, and
/// telling them apart is what lets a decoder park a frame worth
/// re-attempting without parking one that will never convert.
#[derive(Debug, Clone, Copy)]
pub struct BufferAcquireFailed {
  plane: usize,
}

impl BufferAcquireFailed {
  /// Constructs a `BufferAcquireFailed` payload.
  #[inline]
  pub const fn new(plane: usize) -> Self {
    Self { plane }
  }
  /// Plane index whose buffer couldn't be acquired.
  #[inline]
  pub const fn plane(&self) -> usize {
    self.plane
  }
}

/// Payload for [`ConvertError::CarrierAllocFailed`].
///
/// The plane's extent was proved and the carrier still could not be
/// made: a refcount the view lane could not take, a gather or copy the
/// allocator refused.
///
/// **A fact about the moment, not about the frame.** The same frame may
/// convert perfectly a moment later, which is why the decode roads park
/// it and re-attempt rather than letting it go.
#[derive(Debug, Clone, Copy)]
pub struct CarrierAllocFailed {
  plane: usize,
}

impl CarrierAllocFailed {
  /// Constructs a `CarrierAllocFailed` payload.
  #[inline]
  #[must_use]
  pub const fn new(plane: usize) -> Self {
    Self { plane }
  }

  /// Plane index whose carrier could not be allocated.
  #[inline]
  #[must_use]
  pub const fn plane(&self) -> usize {
    self.plane
  }
}

impl core::fmt::Display for CarrierAllocFailed {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "convert: could not allocate a carrier for plane {}",
      self.plane
    )
  }
}

impl std::error::Error for CarrierAllocFailed {}

impl core::fmt::Display for BufferAcquireFailed {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "convert: could not acquire buffer ref for plane {}",
      self.plane
    )
  }
}

/// Payload for [`ConvertError::TooManyPixels`].
///
/// A frame declares more pixels than the session's
/// [`FrameLimits::max_pixels`] allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooManyPixels {
  pixels: u64,
  limit: u64,
}

impl TooManyPixels {
  /// Constructs a `TooManyPixels` payload.
  #[inline]
  pub const fn new(pixels: u64, limit: u64) -> Self {
    Self { pixels, limit }
  }
  /// The pixel count the frame declared.
  #[inline]
  pub const fn pixels(&self) -> u64 {
    self.pixels
  }
  /// The ceiling in force.
  #[inline]
  pub const fn limit(&self) -> u64 {
    self.limit
  }
}

impl core::fmt::Display for TooManyPixels {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "convert: a {}-pixel frame exceeds the {}-pixel ceiling",
      self.pixels, self.limit
    )
  }
}

/// Payload for [`ConvertError::FrameTooLarge`].
///
/// A frame's planes would export more bytes than the session's
/// [`FrameLimits::max_frame_bytes`] allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTooLarge {
  bytes: usize,
  limit: usize,
}

impl FrameTooLarge {
  /// Constructs a `FrameTooLarge` payload.
  #[inline]
  pub const fn new(bytes: usize, limit: usize) -> Self {
    Self { bytes, limit }
  }
  /// The bytes the frame's planes would have exported.
  #[inline]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The ceiling in force.
  #[inline]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

impl core::fmt::Display for FrameTooLarge {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "convert: a frame exporting {} bytes exceeds the {}-byte ceiling",
      self.bytes, self.limit
    )
  }
}

/// Payload for [`ConvertError::InvalidSampleCount`].
///
/// An audio frame declares a negative `nb_samples`.
///
/// Refused rather than floored to zero. A negative count is not an
/// empty frame — it is a header that cannot be read — and clamping it
/// turned a malformed frame into a well-formed empty one that a
/// consumer would have gone on decoding past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidSampleCount {
  count: i32,
}

impl InvalidSampleCount {
  /// Constructs an `InvalidSampleCount` payload.
  #[inline]
  pub const fn new(count: i32) -> Self {
    Self { count }
  }
  /// The count the frame declared.
  #[inline]
  pub const fn count(&self) -> i32 {
    self.count
  }
}

impl core::fmt::Display for InvalidSampleCount {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(f, "convert: {} is not a sample count", self.count)
  }
}

/// Payload for [`ConvertError::UnsupportedSampleFormat`].
///
/// The frame's sample format has no byte width — `AV_SAMPLE_FMT_NONE`,
/// or a format newer than this build names.
///
/// Checked **before** the zero-sample shortcut, because a frame with no
/// readable format is malformed whether or not it carries samples.
/// Letting an empty one through returned an `AudioFrame` advertising a
/// format nothing can interpret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedSampleFormat {
  raw: i32,
}

impl UnsupportedSampleFormat {
  /// Constructs an `UnsupportedSampleFormat` payload.
  #[inline]
  pub const fn new(raw: i32) -> Self {
    Self { raw }
  }
  /// The raw `AVFrame.format` integer, exactly as FFmpeg wrote it.
  #[inline]
  pub const fn raw(&self) -> i32 {
    self.raw
  }
}

impl core::fmt::Display for UnsupportedSampleFormat {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "convert: AVSampleFormat {} has no byte width this build can use",
      self.raw
    )
  }
}

/// Payload for [`ConvertError::UnsupportedChannelCount`].
///
/// A channel count this crate will not carry: more than
/// [`u8::MAX`], which the portable `AudioFrame` seat cannot hold, or
/// none at all on a frame that claims samples.
///
/// **Refused, never clamped.** Clamping to 255 was silent truncation of
/// exactly the kind this boundary exists to refuse: a 256-channel
/// packed frame then computed its byte product from the clipped count
/// and copied 510 of its 512 bytes, delivering a short buffer that
/// advertised 255 channels. A short read is not a smaller frame; it is
/// a wrong one.
///
/// The count is carried **signed**, as `AVChannelLayout.nb_channels`
/// declares it. It has to be: a negative count is one of the things
/// this arm refuses, and the first version of this refusal read the
/// count off a materialised layout description that had already
/// floored it to zero — so `nb_channels == -1` arrived looking like a
/// legitimate zero-channel frame and was never seen by the guard meant
/// to catch it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedChannelCount {
  channels: i32,
}

impl UnsupportedChannelCount {
  /// Constructs an `UnsupportedChannelCount` payload.
  #[inline]
  pub const fn new(channels: i32) -> Self {
    Self { channels }
  }
  /// The count the frame's layout declared, exactly as it read.
  #[inline]
  pub const fn channels(&self) -> i32 {
    self.channels
  }
}

impl core::fmt::Display for UnsupportedChannelCount {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "convert: {} channels cannot be carried (1..={} on a frame with samples)",
      self.channels,
      u8::MAX,
    )
  }
}

/// Payload for [`ConvertError::InvalidDimensions`].
///
/// A picture frame declaring a negative width or height.
///
/// The sibling of [`InvalidSampleCount`] on the picture road, and found
/// by auditing for it: `width` and `height` were floored with `.max(0)`
/// before anything judged them, so a declared `-1` became `0` and then
/// sailed through the pixel ceiling (zero pixels is under every
/// ceiling) to produce a real `VideoFrame` of zero extent. A refusal
/// delivered as a successful decode, which is the one outcome worse
/// than an error.
///
/// Zero itself is **not** refused here: it is what an unset dimension
/// reads as, the ceilings and the plane geometry both handle it, and
/// inventing a refusal for it would be policy this audit has no
/// evidence for. Only the negative — which cannot be a dimension under
/// any reading — is named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidDimensions {
  width: i32,
  height: i32,
}

impl InvalidDimensions {
  /// Constructs an `InvalidDimensions` payload.
  #[inline]
  pub const fn new(width: i32, height: i32) -> Self {
    Self { width, height }
  }
  /// The width the frame declared, exactly as it read.
  #[inline]
  pub const fn width(&self) -> i32 {
    self.width
  }
  /// The height the frame declared, exactly as it read.
  #[inline]
  pub const fn height(&self) -> i32 {
    self.height
  }
}

impl core::fmt::Display for InvalidDimensions {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "convert: frame declares dimensions {}x{}, which are not a picture",
      self.width, self.height,
    )
  }
}

/// Payload for [`ConvertError::ImageSideDataTooLarge`].
///
/// A decoded still whose side data exceeds
/// [`FrameLimits::max_image_side_data_bytes`](crate::FrameLimits::max_image_side_data_bytes).
///
/// Refused rather than truncated. The shared stream collector drops
/// what does not fit and logs it, which on a still is the wrong answer
/// twice: an ICC profile is the entry most likely to be large and the
/// one whose loss silently changes the colours, and the drop is
/// positional, so a big profile pushes the display matrix off the end
/// and the picture comes back rotated wrong with nothing to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSideDataTooLarge {
  bytes: usize,
  limit: usize,
}

impl ImageSideDataTooLarge {
  /// Constructs an `ImageSideDataTooLarge` payload.
  #[inline]
  pub const fn new(bytes: usize, limit: usize) -> Self {
    Self { bytes, limit }
  }
  /// Bytes the still's side data reached.
  #[inline]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The ceiling in force.
  #[inline]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

impl core::fmt::Display for ImageSideDataTooLarge {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "convert: still side data reaches {} bytes over a ceiling of {}",
      self.bytes, self.limit,
    )
  }
}

/// Payload for [`ConvertError::ImageSideDataEntries`].
///
/// A decoded still declaring more side-data entries than this crate
/// will walk. The count sibling of [`ImageSideDataTooLarge`], and
/// refused for the same reason: truncating the list is how the
/// orientation goes missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSideDataEntries {
  count: usize,
  limit: usize,
}

impl ImageSideDataEntries {
  /// Constructs an `ImageSideDataEntries` payload.
  #[inline]
  pub const fn new(count: usize, limit: usize) -> Self {
    Self { count, limit }
  }
  /// Entries the still declared.
  #[inline]
  pub const fn count(&self) -> usize {
    self.count
  }
  /// The cap in force.
  #[inline]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

impl core::fmt::Display for ImageSideDataEntries {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "convert: still declares {} side-data entries over a cap of {}",
      self.count, self.limit,
    )
  }
}

/// Errors from [`av_frame_to_video_frame`].
#[derive(Debug, Clone, IsVariant, Unwrap, TryUnwrap)]
#[non_exhaustive]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
pub enum ConvertError {
  /// `av_frame` was null.
  NullFrame,
  /// The frame declares more pixels than the ceiling allows. Refused
  /// **before** any plane is allocated.
  TooManyPixels(TooManyPixels),
  /// The frame's planes would export more bytes than the ceiling
  /// allows. Refused **before** any plane is allocated.
  FrameTooLarge(FrameTooLarge),
  /// An audio frame declares a negative sample count.
  InvalidSampleCount(InvalidSampleCount),
  /// A picture frame declares a negative width or height.
  InvalidDimensions(InvalidDimensions),
  /// A decoded still's side data is larger than the ceiling allows.
  ImageSideDataTooLarge(ImageSideDataTooLarge),
  /// A decoded still declares more side-data entries than this crate
  /// will walk.
  ImageSideDataEntries(ImageSideDataEntries),
  /// An audio frame's sample format has no byte width.
  UnsupportedSampleFormat(UnsupportedSampleFormat),
  /// An audio frame's channel count is one this crate will not carry.
  UnsupportedChannelCount(UnsupportedChannelCount),
  /// The frame's pixel format isn't in the closed CPU-format set this
  /// crate supports for safe per-plane access.
  UnsupportedPixelFormat(UnsupportedPixelFormat),
  /// A plane reported `linesize <= 0` or otherwise inconsistent layout.
  InvalidPlaneLayout(InvalidPlaneLayout),
  /// A plane's `data[i]` does not lie inside any of the frame's own
  /// `buf[]` allocations, so its extent cannot be proved.
  BufferAcquireFailed(BufferAcquireFailed),
  /// The plane's extent was proved and the carrier still could not be
  /// made. See [`CarrierAllocFailed`].
  CarrierAllocFailed(CarrierAllocFailed),
}

impl ConvertError {
  /// Whether a decode session should **park** the frame this refusal
  /// came from and re-attempt it before receiving another.
  ///
  /// The same shape as the demux seat, for the same reason: a decoder's
  /// `receive_frame` advances libavcodec, so a conversion that then
  /// fails on an allocation would lose a frame nothing can ask for
  /// again. Only an allocation qualifies — every other arm here is a
  /// fact about the frame (a format nothing can carry, a layout that
  /// does not add up, a plane outside its own buffers), and re-offering
  /// one of those would answer every later receive with the same error.
  #[inline]
  pub(crate) const fn parks_in_decode(&self) -> bool {
    matches!(self, Self::CarrierAllocFailed(_))
  }
}

impl core::fmt::Display for ConvertError {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::NullFrame => write!(f, "convert: AVFrame pointer was null"),
      Self::TooManyPixels(p) => core::fmt::Display::fmt(p, f),
      Self::FrameTooLarge(p) => core::fmt::Display::fmt(p, f),
      Self::InvalidSampleCount(p) => core::fmt::Display::fmt(p, f),
      Self::InvalidDimensions(p) => core::fmt::Display::fmt(p, f),
      Self::ImageSideDataTooLarge(p) => core::fmt::Display::fmt(p, f),
      Self::ImageSideDataEntries(p) => core::fmt::Display::fmt(p, f),
      Self::UnsupportedSampleFormat(p) => core::fmt::Display::fmt(p, f),
      Self::UnsupportedChannelCount(p) => core::fmt::Display::fmt(p, f),
      Self::UnsupportedPixelFormat(p) => core::fmt::Display::fmt(p, f),
      Self::InvalidPlaneLayout(p) => core::fmt::Display::fmt(p, f),
      Self::BufferAcquireFailed(p) => core::fmt::Display::fmt(p, f),
      Self::CarrierAllocFailed(p) => core::fmt::Display::fmt(p, f),
    }
  }
}

impl core::error::Error for ConvertError {}

/// Builds [`ConvertError::UnsupportedPixelFormat`] for a frame whose raw
/// format integer this crate will not deliver.
///
/// Both refusal sites go through here so the raw id and the name are
/// never gathered at one of them and forgotten at the other.
fn unsupported_pixel_format(format: PixelFormat, raw: i32) -> ConvertError {
  ConvertError::UnsupportedPixelFormat(UnsupportedPixelFormat::new(
    format,
    raw,
    crate::ffi::pix_fmt_name(raw),
  ))
}

/// Safe wrapper around [`av_frame_to_video_frame`] taking a borrowed
/// [`ffmpeg::Frame`](ffmpeg_next::Frame). Recommended entry point for
/// most callers — equivalent to passing `frame.as_ptr()` to the
/// unsafe variant, but the FFmpeg side keeps the frame alive for the
/// duration of the call so the safety contract is satisfied
/// internally.
///
/// **Borrowed source, owned lane.** This road copies, on purpose and
/// without a lane to choose. `ffmpeg_next`'s frame wrappers lend
/// `&mut [u8]` through `data_mut` and share their buffers by refcount
/// with no copy-on-write, so a caller who still holds the frame holds a
/// mutable alias of every byte a view would read — and both sides are
/// `Send`, so the two halves need not even be on one thread. No safe
/// signature that borrows a frame can hand out a window onto it.
///
/// The view lane reaches frames the way it is meant to: through a
/// decoder, which owns the `AVFrame` it decoded into and never lends it
/// out. A caller holding an `AVFrame` of their own can use the `unsafe`
/// entry point below, whose contract names the obligation this
/// signature cannot express.
/// The lane is not a parameter here, and asking for one does not
/// compile:
///
/// ```compile_fail,E0107
/// use mediadecode_ffmpeg::{FrameLimits, View, convert::video_frame_from};
/// let frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::GRAY8, 64, 4);
/// let _ = video_frame_from::<View>(&frame, mediadecode::Timebase::default(), FrameLimits::default());
/// ```
pub fn video_frame_from(
  frame: &ffmpeg_next::Frame,
  time_base: Timebase,
  limits: FrameLimits,
) -> Result<VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, FfmpegBytes>, ConvertError> {
  // SAFETY: `&frame` keeps the AVFrame alive for the duration of this
  // call; the unsafe convert just reads through the pointer, and the
  // owned lane copies every byte it reads, so nothing outlives the
  // borrow.
  unsafe { av_frame_to_video_frame_as::<crate::Owned>(frame.as_ptr(), time_base, limits) }
}

/// Safe wrapper around [`av_frame_to_audio_frame`] taking a borrowed
/// [`ffmpeg::frame::Audio`](ffmpeg_next::frame::Audio).
///
/// **Borrowed source, owned lane.** This road copies, on purpose and
/// without a lane to choose. `ffmpeg_next`'s frame wrappers lend
/// `&mut [u8]` through `data_mut` and share their buffers by refcount
/// with no copy-on-write, so a caller who still holds the frame holds a
/// mutable alias of every byte a view would read — and both sides are
/// `Send`, so the two halves need not even be on one thread. No safe
/// signature that borrows a frame can hand out a window onto it.
///
/// The view lane reaches frames the way it is meant to: through a
/// decoder, which owns the `AVFrame` it decoded into and never lends it
/// out. A caller holding an `AVFrame` of their own can use the `unsafe`
/// entry point below, whose contract names the obligation this
/// signature cannot express.
pub fn audio_frame_from(
  frame: &ffmpeg_next::frame::Audio,
  time_base: Timebase,
  limits: FrameLimits,
) -> Result<
  AudioFrame<SampleFormat, ChannelLayoutDescription, AudioFrameExtra, FfmpegBytes>,
  ConvertError,
> {
  // SAFETY: `&frame` keeps the AVFrame alive for the duration of this
  // call, and the owned lane copies what it reads.
  unsafe { av_frame_to_audio_frame_as::<crate::Owned>(frame.as_ptr(), time_base, limits) }
}

/// Safe wrapper around [`av_subtitle_to_subtitle_frame`] taking a
/// borrowed [`ffmpeg::Subtitle`](ffmpeg_next::Subtitle).
///
/// Owned-lane, like its siblings — though a subtitle rect is copied on
/// both lanes anyway (`AVSubtitleRect` has no refcounted buffer), so
/// here the restriction costs a caller nothing at all.
pub fn subtitle_frame_from(
  subtitle: &ffmpeg_next::Subtitle,
  time_base: Timebase,
) -> Result<SubtitleFrame<SubtitleFrameExtra, FfmpegBytes>, ConvertError> {
  // SAFETY: `&subtitle` keeps the AVSubtitle alive for the duration
  // of this call.
  unsafe { av_subtitle_to_subtitle_frame_as::<crate::Owned>(subtitle.as_ptr(), time_base) }
}

/// Converts an FFmpeg `AVFrame` (CPU-side, post-`av_hwframe_transfer_data`
/// or from a software decoder) into a `mediadecode::VideoFrame`
/// parameterized by [`crate::Ffmpeg`] / `FfmpegBytes`.
///
/// `time_base` is the source stream's time base, used to label
/// `pts`/`duration` as mediatime [`Timestamp`]s.
///
/// # Safety
///
/// `av_frame` must be a live `*const AVFrame` for the duration of this
/// call. The frame's buffers are neither consumed nor referenced —
/// every byte the produced `VideoFrame` carries is a copy, so the
/// source frame may be unreffed, reused or dropped the moment this
/// returns.
/// * no handle capable of **mutating** the frame's buffers may
///   outlive this call while the returned carriers do. On the view
///   lane a plane is a window into `frame`'s own allocation, and
///   `ffmpeg_next`'s wrappers lend `&mut [u8]` by refcount with no
///   copy-on-write — so keeping the source frame and writing through
///   it would race a carrier a consumer is reading. Consume the
///   frame, or use the owned lane, or use the safe borrowed wrapper
///   (which is the owned lane for exactly this reason).
pub(crate) unsafe fn av_frame_to_video_frame_as<C: crate::FfmpegCarrier + crate::CarrierOps>(
  av_frame: *const AVFrame,
  time_base: Timebase,
  limits: FrameLimits,
) -> Result<VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, C::Buffer>, ConvertError> {
  if av_frame.is_null() {
    return Err(ConvertError::NullFrame);
  }
  // We deliberately never form `&*av_frame` — `AVFrame` contains
  // bindgen-enum fields (`pict_type`, `color_primaries`, `colorspace`,
  // `color_trc`, `color_range`, `chroma_location`, and an embedded
  // `AVChannelLayout` whose `order` is also enum-typed). If FFmpeg
  // (or a hostile decoder) writes a value outside our bindgen's
  // discriminant set, the `&AVFrame` reference itself would be
  // immediate UB before any field access. Working through the raw
  // pointer with field-by-field reads (and `addr_of!` for the
  // enum-typed fields) sidesteps this whole class.

  // Non-enum primitives are safe to read via `(*av_frame).field`
  // because validity for `i32`/`i64`/pointer types is just
  // "initialized bytes"; the surrounding struct's enum fields don't
  // contaminate this read.
  let format_raw = unsafe { (*av_frame).format };
  let width_raw = unsafe { (*av_frame).width };
  let height_raw = unsafe { (*av_frame).height };
  let pts_raw = unsafe { (*av_frame).pts };
  let duration_raw = unsafe { (*av_frame).duration };
  // **Judged before anything consumes them.** These were floored with
  // `.max(0)`, which turned a declared `-1` into `0` — and zero pixels
  // is under every ceiling, so the frame was built rather than refused.
  // The same order bug the audio road had with its channel count: the
  // field's first consumer ran ahead of the field's validator.
  if width_raw < 0 || height_raw < 0 {
    return Err(ConvertError::InvalidDimensions(InvalidDimensions::new(
      width_raw, height_raw,
    )));
  }
  let width = width_raw as u32;
  let height = height_raw as u32;
  let pix_fmt = boundary::from_av_pixel_format(format_raw);

  // SAFETY: caller upholds `av_frame`'s liveness for the whole call.
  let (planes_out, plane_count) = unsafe {
    copy_out_planes::<C>(
      av_frame,
      &pix_fmt,
      format_raw,
      width,
      height,
      limits,
      PlaneRoad::Video,
    )
  }?;

  // pts / duration / time_base
  let pts = if pts_raw != AV_NOPTS_VALUE {
    Some(Timestamp::new(pts_raw, time_base))
  } else {
    None
  };
  let duration = if duration_raw > 0 {
    Some(Timestamp::new(duration_raw, time_base))
  } else {
    None
  };

  // Visible rect (FFmpeg crop).
  let visible_rect = unsafe { build_visible_rect(av_frame, width, height) };

  // Color metadata (the universal cross-backend bits). We read each
  // bindgen enum-typed field through a raw `i32` window — even
  // referencing an out-of-range enum value is UB before any cast can
  // run, so we never let Rust assume the field actually inhabits the
  // enum's discriminant set. FFmpeg version skew or a buggy decoder
  // can put unknown values into these fields.

  // SAFETY: `av_frame` points at a live AVFrame; `addr_of!` computes
  // the address without forming a reference, and `read_unaligned::<i32>`
  // is sound because each of these enum types has the layout of
  // `c_int` (i32) per FFmpeg's bindgen output.
  let color_primaries_raw =
    unsafe { read_unaligned(addr_of!((*av_frame).color_primaries) as *const i32) };
  let color_trc_raw = unsafe { read_unaligned(addr_of!((*av_frame).color_trc) as *const i32) };
  let colorspace_raw = unsafe { read_unaligned(addr_of!((*av_frame).colorspace) as *const i32) };
  let color_range_raw = unsafe { read_unaligned(addr_of!((*av_frame).color_range) as *const i32) };
  let chroma_location_raw =
    unsafe { read_unaligned(addr_of!((*av_frame).chroma_location) as *const i32) };
  let color = ColorInfo::UNSPECIFIED
    .with_primaries(map_primaries(color_primaries_raw))
    .with_transfer(map_transfer(color_trc_raw))
    .with_matrix(map_matrix(colorspace_raw))
    .with_range(map_range_for(&pix_fmt, color_range_raw))
    .with_chroma_location(map_chroma_loc(chroma_location_raw));

  // Backend-specific extras.
  let extra = unsafe { build_video_frame_extra(av_frame) };

  // pix_fmt is already mediadecode::PixelFormat thanks to the boundary
  // function above, so we just pass it through.
  let mut out = VideoFrame::new(
    Dimensions::new(width, height),
    pix_fmt,
    planes_out,
    plane_count,
    extra,
  )
  .with_pts(pts)
  .with_duration(duration)
  .with_color(color);
  if let Some(r) = visible_rect {
    out = out.with_visible_rect(Some(r));
  }
  Ok(out)
}

/// Safe wrapper around [`av_frame_to_image_frame`] taking a borrowed
/// [`ffmpeg::Frame`](ffmpeg_next::Frame).
///
/// Owned-lane, for the reason [`video_frame_from`] states: a borrowed
/// frame cannot be safely viewed.
pub fn image_frame_from(
  frame: &ffmpeg_next::Frame,
  limits: FrameLimits,
) -> Result<ImageFrame<mediadecode::PixelFormat, ImageFrameExtra, FfmpegBytes>, ConvertError> {
  // SAFETY: `&frame` keeps the AVFrame alive for the duration of this
  // call, and the owned lane copies what it reads.
  unsafe { av_frame_to_image_frame_as::<crate::Owned>(frame.as_ptr(), limits) }
}

/// Converts an FFmpeg `AVFrame` holding a decoded **still** into a
/// [`mediadecode::frame::ImageFrame`].
///
/// The same picture geometry as [`av_frame_to_video_frame`] — one
/// plane-extraction rule, shared — and none of its timeline. There is
/// no `time_base` parameter because there is nothing to label with it:
/// a still is not on the timeline, so `ImageFrame` has no `pts` and no
/// `duration` seats. Whatever `AVFrame.pts` a one-shot image decoder
/// happens to leave behind is an artefact of the packet it was fed,
/// not a fact about the picture, and it is deliberately dropped rather
/// than carried into a field that would invite a consumer to sort by
/// it.
///
/// `visible_rect` is FFmpeg's crop, exactly as on the video side, and
/// it earns its place here: a JPEG's coded dimensions are rounded up
/// to its MCU grid, so the crop is what distinguishes the picture from
/// the padding the encoder added to reach a multiple of 8 or 16.
///
/// # Safety
///
/// `av_frame` must be a live `*const AVFrame` for the duration of this
/// call. The frame's buffers are not consumed — every byte the
/// produced [`ImageFrame`] carries is a copy.
/// * no handle capable of **mutating** the frame's buffers may
///   outlive this call while the returned carriers do. On the view
///   lane a plane is a window into `frame`'s own allocation, and
///   `ffmpeg_next`'s wrappers lend `&mut [u8]` by refcount with no
///   copy-on-write — so keeping the source frame and writing through
///   it would race a carrier a consumer is reading. Consume the
///   frame, or use the owned lane, or use the safe borrowed wrapper
///   (which is the owned lane for exactly this reason).
pub(crate) unsafe fn av_frame_to_image_frame_as<C: crate::FfmpegCarrier + crate::CarrierOps>(
  av_frame: *const AVFrame,
  limits: FrameLimits,
) -> Result<ImageFrame<mediadecode::PixelFormat, ImageFrameExtra, C::Buffer>, ConvertError> {
  if av_frame.is_null() {
    return Err(ConvertError::NullFrame);
  }
  // Same stance as `av_frame_to_video_frame`: never form `&AVFrame`.
  // See its comments for why every read here goes through the raw
  // pointer, and why the enum-typed fields go through `addr_of!` +
  // `read_unaligned::<i32>`.
  let format_raw = unsafe { (*av_frame).format };
  let width_raw = unsafe { (*av_frame).width };
  let height_raw = unsafe { (*av_frame).height };
  // **Judged before anything consumes them.** These were floored with
  // `.max(0)`, which turned a declared `-1` into `0` — and zero pixels
  // is under every ceiling, so the frame was built rather than refused.
  // The same order bug the audio road had with its channel count: the
  // field's first consumer ran ahead of the field's validator.
  if width_raw < 0 || height_raw < 0 {
    return Err(ConvertError::InvalidDimensions(InvalidDimensions::new(
      width_raw, height_raw,
    )));
  }
  let width = width_raw as u32;
  let height = height_raw as u32;
  let pix_fmt = boundary::from_av_pixel_format(format_raw);

  // **The still's side data is judged here, before a plane is bought.**
  // It reads only header fields and allocates nothing, so it is one of
  // the free judgements and belongs with them. After the copy it meant
  // an over-budget still had already paid for up to `max_frame_bytes`
  // of plane copies before its annotations were so much as totalled —
  // a correct refusal delivered after the expensive half of the work.
  //
  // Everything this conversion can refuse is now refused before
  // anything it can allocate is allocated.
  //
  // SAFETY: caller upholds `av_frame`'s liveness for the whole call.
  unsafe { measure_image_side_data(av_frame, limits) }?;

  // SAFETY: caller upholds `av_frame`'s liveness for the whole call.
  let (planes_out, plane_count) = unsafe {
    copy_out_planes::<C>(
      av_frame,
      &pix_fmt,
      format_raw,
      width,
      height,
      limits,
      PlaneRoad::Still,
    )
  }?;

  // SAFETY: `av_frame` is live; the crop fields are plain integers.
  let visible_rect = unsafe { build_visible_rect(av_frame, width, height) };

  // SAFETY: `av_frame` points at a live AVFrame; each enum-typed field
  // is read through a raw `i32` window rather than as its bindgen enum.
  let color_primaries_raw =
    unsafe { read_unaligned(addr_of!((*av_frame).color_primaries) as *const i32) };
  let color_trc_raw = unsafe { read_unaligned(addr_of!((*av_frame).color_trc) as *const i32) };
  let colorspace_raw = unsafe { read_unaligned(addr_of!((*av_frame).colorspace) as *const i32) };
  let color_range_raw = unsafe { read_unaligned(addr_of!((*av_frame).color_range) as *const i32) };
  let chroma_location_raw =
    unsafe { read_unaligned(addr_of!((*av_frame).chroma_location) as *const i32) };
  let color = ColorInfo::UNSPECIFIED
    .with_primaries(map_primaries(color_primaries_raw))
    .with_transfer(map_transfer(color_trc_raw))
    .with_matrix(map_matrix(colorspace_raw))
    // The `yuvj*` override matters more here than anywhere: cover art
    // is overwhelmingly MJPEG, and MJPEG is where a frame's
    // `color_range` is routinely left unspecified on a signal that is
    // full-range by definition.
    .with_range(map_range_for(&pix_fmt, color_range_raw))
    .with_chroma_location(map_chroma_loc(chroma_location_raw));

  // SAFETY: caller upholds liveness; the collector reads the enum-typed
  // `type_` raw and bounds-checks each entry's data slice.
  let side_data = unsafe { collect_image_side_data(av_frame, limits) }?;
  let extra = ImageFrameExtra::default()
    .with_orientation(orientation_of(&side_data))
    .with_side_data(side_data);

  Ok(
    ImageFrame::new(
      Dimensions::new(width, height),
      pix_fmt,
      planes_out,
      plane_count,
      extra,
    )
    .with_visible_rect(visible_rect)
    .with_color(color),
  )
}

/// The orientation a still's display matrix names, if it carries one.
///
/// Read out of the side data this crate already collects rather than
/// off the `AVFrame` a second time: the entry is there, whole and
/// unparsed, and one read is one place for the fact to come from.
///
/// `None` when the frame carries no display matrix — the ordinary case
/// — and also when it carries one this vocabulary cannot read, in
/// which case the raw entry stays in the side-data list rather than
/// being lost.
fn orientation_of(side_data: &[SideDataEntry]) -> Option<ImageOrientation> {
  const DISPLAY_MATRIX: i32 = AVFrameSideDataType::AV_FRAME_DATA_DISPLAYMATRIX as i32;
  side_data
    .iter()
    .find(|entry| entry.kind() == DISPLAY_MATRIX)
    .and_then(|entry| ImageOrientation::from_display_matrix(entry.data()))
}

/// Whether the **video** road can deliver `pix_fmt`.
///
/// Exposed so a consumer — and this crate's own tests — can ask the
/// question the still road answers differently. See [`PlaneRoad`].
pub fn is_video_deliverable(pix_fmt: &PixelFormat) -> bool {
  pixdesc::is_deliverable(pix_fmt)
}

/// Which plane vocabulary a conversion is working in.
///
/// The two roads differ by exactly two layouts. A still may be
/// paletted (`pal8`, an indexed PNG or BMP — indices in `data[0]`, a
/// fixed 1024-byte palette in `data[1]`) or sub-byte packed (`monob` /
/// `monow`, a 1-bit PNG — rows of `ceil(width / 8)`); motion video
/// keeps refusing both.
///
/// **The still road was widened, not the shared one, and that was a
/// measured choice.** Widening the shared road would have changed what
/// every existing video consumer can be handed — `is_supported_cpu_pix_fmt`,
/// the HW transfer validation and the video suites all key off the same
/// deliverability answer — to serve formats motion video does not
/// occur in. The still road is where indexed and 1-bit pictures
/// actually arrive, and it is one enum away.
///
/// Nothing is converted on either road. mediadecode delivers what
/// FFmpeg decoded; turning `pal8` into RGB is colconv's job, one tier
/// along, and doing it here would be this crate deciding what a
/// consumer's pixels should look like.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PlaneRoad {
  /// Motion video: the shared vocabulary.
  Video,
  /// A still: the shared vocabulary plus paletted and sub-byte
  /// layouts.
  Still,
}

impl PlaneRoad {
  fn is_deliverable(self, pix_fmt: &PixelFormat) -> bool {
    match self {
      Self::Video => pixdesc::is_deliverable(pix_fmt),
      Self::Still => pixdesc::is_still_deliverable(pix_fmt),
    }
  }

  fn plane_geometry(
    self,
    pix_fmt: &PixelFormat,
    width: usize,
    height: usize,
  ) -> Option<pixdesc::PlaneGeometry> {
    match self {
      Self::Video => pixdesc::plane_geometry(pix_fmt, width, height),
      Self::Still => pixdesc::still_plane_geometry(pix_fmt, width, height),
    }
  }
}

/// The planes of a CPU-side picture `AVFrame`, copied out.
///
/// Shared by the video and image households: the geometry of a still
/// is the geometry of a picture, and there is one plane-extraction
/// rule here rather than two that could drift apart.
///
/// Returns the four-slot array and how many of its entries are
/// populated. Unused slots hold the shared empty carrier.
///
/// # Safety
///
/// `av_frame` must be a live `*const AVFrame` for the duration of this
/// call, and `format_raw` / `pix_fmt` / `width` / `height` must be the
/// values read from it.
unsafe fn copy_out_planes<C: crate::FfmpegCarrier + crate::CarrierOps>(
  av_frame: *const AVFrame,
  pix_fmt: &PixelFormat,
  format_raw: i32,
  width: u32,
  height: u32,
  limits: FrameLimits,
  road: PlaneRoad,
) -> Result<([Plane<C::Buffer>; 4], u8), ConvertError> {
  // The pixel ceiling, first of all — before the format is even looked
  // up, because a forged `width` / `height` costs nothing to write and
  // everything to honour. libavcodec has normally refused such a frame
  // already (the same number reaches `AVCodecContext.max_pixels` when a
  // decoder is opened from these limits), but this path also converts
  // frames the caller produced by other means, so the ceiling is
  // enforced on both sides of that door.
  let pixels = u64::from(width) * u64::from(height);
  if pixels > limits.max_pixels() {
    return Err(ConvertError::TooManyPixels(TooManyPixels::new(
      pixels,
      limits.max_pixels(),
    )));
  }
  // Reject any format whose planes we can't safely extract — HWACCEL
  // surfaces, Bayer mosaics, paletted, and sub-byte bitstream
  // packings — before touching plane memory. Without a deliverable
  // layout we'd be reading garbage `linesize * height` bytes.
  if !road.is_deliverable(pix_fmt) {
    return Err(unsupported_pixel_format(pix_fmt.clone(), format_raw));
  }
  // The per-plane row count and visible (tight) byte width come from
  // `pixdesc::plane_geometry`, which derives them from libavutil's own
  // `av_image_fill_linesizes` / `av_image_fill_plane_sizes` for this
  // exact `(format, width, height)` — correct by construction for every
  // deliverable CPU format. For a deliverable format `plane_geometry`
  // only returns `None` on out-of-range dimensions; treat that as an
  // unsupported frame rather than guessing a layout.
  let geom = match road.plane_geometry(pix_fmt, width as usize, height as usize) {
    Some(g) => g,
    None => return Err(unsupported_pixel_format(pix_fmt.clone(), format_raw)),
  };

  // The byte ceiling, before a single plane is allocated. Totalled over
  // what the planes will *actually* export — which needs the stride
  // decision, so it is this crate's real allocation figure rather than
  // an estimate of it. A first pass to judge, a second to pay: the
  // alternative is discovering the frame was too big three plane
  // allocations in, which is the shape that OOMs.
  // **Judged from the geometry alone — no per-plane frame read at
  // all.** Every plane exports the format's own row width times its own
  // row count: a tight stride equals that width and a padded one is
  // compacted back to it, so the total does not depend on any number
  // the frame chose. That makes this the cheapest judgement available,
  // which is why it runs before the strides are so much as looked at.
  let mut exported: usize = 0;
  for plane_idx in 0..geom.count {
    let plane_bytes = geom.row_bytes[plane_idx]
      .checked_mul(geom.height[plane_idx])
      .ok_or(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(
        plane_idx,
      )))?;
    exported = exported
      .checked_add(plane_bytes)
      .ok_or(ConvertError::FrameTooLarge(FrameTooLarge::new(
        usize::MAX,
        limits.max_frame_bytes(),
      )))?;
  }
  if exported > limits.max_frame_bytes() {
    return Err(ConvertError::FrameTooLarge(FrameTooLarge::new(
      exported,
      limits.max_frame_bytes(),
    )));
  }

  // **Then every stride, before a single plane is copied.** Splitting
  // this out of the copy loop is the point: the loop allocates as it
  // goes, so a frame refused on plane 2 had already paid for planes 0
  // and 1 and thrown them away. A layout fault is a property of the
  // frame, knowable before any of it is bought.
  //
  // An undersized stride used to be treated as a *padded* one here —
  // the branch for a stride that is larger — which meant the frame was
  // sized from a row width the plane did not have and the real refusal
  // was left to the copy. The copy loop keeps its own form of this
  // check: one comparison guarding a `from_raw_parts`, and defence in
  // depth at a pointer boundary is not duplication.
  for plane_idx in 0..geom.count {
    // The palette is flat: its size is the format's, and FFmpeg leaves
    // its `linesize` at zero deliberately, so there is no stride here
    // to judge.
    if geom.palette_plane == Some(plane_idx) {
      continue;
    }
    // SAFETY: `av_frame` is live per the contract and `plane_idx` is
    // below the descriptor's plane count, so within `linesize`'s eight
    // slots.
    let linesize = unsafe { (*av_frame).linesize[plane_idx] };
    // A zero stride means the decoder left a plane this format
    // populates unset; a negative one is FFmpeg's vertical-flip
    // convention, which this crate's safe accessors refuse; and one
    // below the row width is a plane that does not hold what the format
    // says it holds. All three are the same answer.
    if linesize <= 0 || (linesize as usize) < geom.row_bytes[plane_idx] {
      return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(
        plane_idx,
      )));
    }
  }

  let mut planes_out: [Plane<C::Buffer>; 4] = std::array::from_fn(|_| plane_placeholder::<C>());
  let mut plane_count: u8 = 0;

  // The loop body indexes `planes_out`, the AVFrame's `linesize`, and
  // its `data` array all by `plane_idx`. None of these are slices we
  // can iterate via `iter_mut().enumerate()` — `linesize` / `data` are
  // raw `[T; 8]` fields read through `(*av_frame).field[plane_idx]`,
  // and `planes_out` is also indexed by the same key for symmetry —
  // so the index-based loop is the natural shape. The descriptor's
  // `count` (`1..=4`) bounds the loop to exactly the planes this format
  // populates.
  #[allow(clippy::needless_range_loop)]
  for plane_idx in 0..geom.count {
    // Read per-plane fields through the raw pointer (no `&AVFrame`
    // formed). `linesize` is `[c_int; 8]` and `data` is `[*mut u8; 8]`.
    // The palette first: a flat `AVPALETTE_SIZE` run at `data[i]` with
    // no linesize of its own. Bounded by the format, so there is
    // nothing here for a budget to judge.
    if geom.palette_plane == Some(plane_idx) {
      let data_ptr = unsafe { (*av_frame).data[plane_idx] };
      if data_ptr.is_null() {
        return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(
          plane_idx,
        )));
      }
      let bytes = geom.row_bytes[plane_idx];
      // SAFETY: `find_backing_buffer` proves the run lies inside one of
      // the frame's own live buffers before it is read.
      // The palette is a flat `AVPALETTE_SIZE` run whose length is the
      // format's, not the file's — fully written, so shareable whole.
      //
      // SAFETY: non-null and addressing this plane.
      let carried =
        unsafe { capture_from_backing::<C>(av_frame, data_ptr as *const u8, bytes, plane_idx) }?;
      planes_out[plane_idx] = Plane::new(carried, bytes as u32);
      plane_count = (plane_idx + 1) as u8;
      continue;
    }

    let linesize = unsafe { (*av_frame).linesize[plane_idx] };
    if linesize <= 0 {
      // `plane_idx < geom.count`, so this plane must be populated; a
      // zero linesize means the decoder left an expected plane unset,
      // and a negative linesize is FFmpeg's vertical-flip convention
      // (which our safe accessors refuse). Either way the layout is
      // unusable.
      return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(
        plane_idx,
      )));
    }
    let data_ptr = unsafe { (*av_frame).data[plane_idx] };
    if data_ptr.is_null() {
      return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(
        plane_idx,
      )));
    }
    let plane_h = geom.height[plane_idx];
    let row_bytes = geom.row_bytes[plane_idx];
    if row_bytes > linesize as usize {
      return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(
        plane_idx,
      )));
    }
    // What the copy may read, and what shape it leaves behind:
    //
    // Each row in the AVBufferRef is `linesize` bytes wide but only the
    // first `row_bytes` of them are guaranteed-initialized (the
    // codec's actual output). The remaining `linesize - row_bytes`
    // bytes per row are FFmpeg-allocator scratch — `av_malloc`'d, not
    // necessarily written by the decoder. Forming an `&[u8]` over those
    // bytes is UB even if no consumer reads them, which is why the
    // padded branch never touches them.
    //
    // - When `linesize == row_bytes` (no padding), the plane is one
    //   contiguous run and is copied whole; `stride` stays `linesize`.
    // - When `linesize > row_bytes`, each row is copied tightly and
    //   `stride` becomes `row_bytes`.
    //
    // Both branches copy in 0.9 — the amputation. The *geometry* is
    // untouched: a consumer of a tight plane still reads the decoder's
    // own stride, and a padded plane still arrives compacted.
    let (data, exported_stride) = if (linesize as usize) == row_bytes {
      let plane_bytes =
        (plane_h)
          .checked_mul(linesize as usize)
          .ok_or(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(
            plane_idx,
          )))?;
      // The bounds proof: the AVBufferRef in `(*av_frame).buf[]` that
      // contains `data_ptr` covers at least `plane_bytes` from it. The
      // returned pointer is not needed — 0.8 used it to compute a view
      // offset; 0.9 only needs the guarantee that the read is in range.
      // **The tight plane is the shareable one.** `linesize ==
      // row_bytes` means the whole `plane_bytes` run is the decoder's
      // own output with nothing between the rows, so a view over it
      // exposes no byte that was not written. The owned lane copies it;
      // the view lane takes a reference to exactly this range.
      //
      // SAFETY: `data_ptr` is non-null and addresses this plane.
      let carried = unsafe {
        capture_from_backing::<C>(av_frame, data_ptr as *const u8, plane_bytes, plane_idx)
      }?;
      (carried, linesize as u32)
    } else {
      let total_bytes = row_bytes
        .checked_mul(plane_h)
        .ok_or(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(
          plane_idx,
        )))?;
      // Bound-check the readable extent in the source AVBufferRef
      // BEFORE we start dereferencing per-row offsets. The contiguous
      // branch above does this by passing `plane_bytes` to
      // `find_backing_buffer`; the row-wise branch must do the same — a
      // buggy or hostile decoder/filter could hand us a `data_ptr`
      // backed by a buffer too small for `(plane_h - 1) * linesize +
      // row_bytes`, in which case `from_raw_parts` on the last few
      // rows would form a slice over invalid memory (immediate UB,
      // before any read).
      let last_row_offset = (plane_h.saturating_sub(1))
        .checked_mul(linesize as usize)
        .ok_or(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(
          plane_idx,
        )))?;
      let readable_extent =
        last_row_offset
          .checked_add(row_bytes)
          .ok_or(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(
            plane_idx,
          )))?;
      unsafe { find_backing_buffer(av_frame, data_ptr, readable_extent) }.ok_or(
        ConvertError::BufferAcquireFailed(BufferAcquireFailed::new(plane_idx)),
      )?;
      // **A padded plane is copied on both lanes**, and the view lane
      // does not get an exception here. Only the first `row_bytes` of
      // each `linesize`-wide row are the decoder's output; the rest is
      // allocator scratch nothing wrote. A carrier is an `AsRef<[u8]>`,
      // so sharing the padded span would form a slice over
      // uninitialised memory — undefined before a consumer reads a byte
      // of it, and the same leak the owned lane refused when it stopped
      // exporting `linesize`. Stopping at the last row's `row_bytes`
      // does not help either: the gaps *between* rows are in the span
      // too.
      //
      // So this is the conditional-sharing rule again, in its second
      // place: share where the extent is provably all output, copy
      // where it is not.
      //
      // Written straight into the carrier's allocation, one row at a
      // time — **not** staged through a `Vec` first. The staged
      // spelling allocated the whole plane twice and copied it twice,
      // so a 250 MiB frame peaked at 750 MiB counting FFmpeg's own;
      // this leaves the unavoidable 2×. The size was checked against
      // the frame ceiling above, before any of this was allocated,
      // which is what took the place of the staging `Vec`'s
      // `try_reserve_exact`.
      debug_assert_eq!(total_bytes, row_bytes * plane_h);
      let packed = C::from_rows(plane_h, row_bytes, |row_idx| {
        // `row_offset` cannot overflow: `readable_extent` above already
        // added `(plane_h - 1) * linesize` to `row_bytes` without
        // overflowing, and `row_idx < plane_h`.
        let row_offset = row_idx * linesize as usize;
        // SAFETY: bounds-checked above via `find_backing_buffer`;
        // `row_offset + row_bytes <= readable_extent <= buf.size`.
        // Each per-row slice is the part the decoder writes
        // (initialized).
        unsafe { core::slice::from_raw_parts(data_ptr.add(row_offset) as *const u8, row_bytes) }
      })
      .ok_or(ConvertError::CarrierAllocFailed(CarrierAllocFailed::new(
        plane_idx,
      )))?;
      (packed, row_bytes as u32)
    };

    planes_out[plane_idx] = Plane::new(data, exported_stride);
    plane_count = (plane_idx + 1) as u8;
  }

  Ok((planes_out, plane_count))
}

/// A placeholder for an unused plane slot.
///
/// `[Plane<D>; 4]` requires four populated entries; only
/// `plane_count` of them are exposed through `planes()`. 0.8 gave each
/// slot its own one-byte `AVBufferRef` and could fail doing it; the
/// shared empty carrier costs one allocation for the process.
fn plane_placeholder<C: crate::FfmpegCarrier + crate::CarrierOps>() -> Plane<C::Buffer> {
  Plane::new(C::empty(), 0)
}

/// # Safety
/// `av_frame` must be a live `*const AVFrame` for the duration of this
/// call. The function reads only `crop_*` fields through the raw
/// pointer — it never forms `&AVFrame`, so unrelated invalid enum
/// fields elsewhere in the struct don't matter.
unsafe fn build_visible_rect(av_frame: *const AVFrame, width: u32, height: u32) -> Option<Rect> {
  // The crops are `size_t`. Read as `u64` and kept there: `as u32`
  // truncated them, so a crop of `2^32 + 5` arrived as a perfectly
  // plausible `5` and the rect that came out was wrong in a way nothing
  // could see. The same law as the dimensions above — a number a file
  // chooses is judged, not clipped — applied to the one field on this
  // road that is pure annotation.
  let crop_left = unsafe { (*av_frame).crop_left } as u64;
  let crop_top = unsafe { (*av_frame).crop_top } as u64;
  let crop_right = unsafe { (*av_frame).crop_right } as u64;
  let crop_bottom = unsafe { (*av_frame).crop_bottom } as u64;
  if crop_left == 0 && crop_top == 0 && crop_right == 0 && crop_bottom == 0 {
    return None;
  }
  // A crop that does not fit inside the picture is not a crop. FFmpeg's
  // own `av_frame_apply_cropping` maintains `left + right < width`, so
  // a frame breaking that is malformed — and `saturating_sub` used to
  // answer it with a zero-extent rect, which is a claim rather than an
  // absence.
  //
  // The frame is not refused over it: the pixels are still whatever the
  // decoder produced, and this field only annotates them. What is
  // withheld is the annotation. That is the same stance the colour
  // fields take toward a value this build cannot name — say nothing
  // rather than say something invented.
  // Checked, per pair. These are `size_t` straight off the frame, so
  // each one alone can be near `u64::MAX` and `left + right` is a real
  // overflow — which panics in debug and *wraps* in release, and a
  // wrapped sum passes the extent test and then narrows into a rect
  // pointing outside the picture. The refusal has to come before the
  // arithmetic can lie, not after it.
  let (Some(horizontal), Some(vertical)) = (
    crop_left.checked_add(crop_right),
    crop_top.checked_add(crop_bottom),
  ) else {
    return None;
  };
  // `>=`, not `>`. FFmpeg's own `av_frame_apply_cropping` requires the
  // crops to leave something behind, and a sum *equal* to the extent
  // leaves a zero-width or zero-height rect — which is not a smaller
  // picture, it is the absence of one, asserted as a fact. Withheld
  // like any other uninterpretable annotation.
  if horizontal >= u64::from(width) || vertical >= u64::from(height) {
    return None;
  }
  // Narrowed only now. Each subtraction is proved non-negative by the
  // test above, and all four values are proved strictly below the
  // frame's own `u32` extent, so no cast here can truncate.
  Some(Rect::new(
    crop_left as u32,
    crop_top as u32,
    (u64::from(width) - horizontal) as u32,
    (u64::from(height) - vertical) as u32,
  ))
}

/// # Safety
/// `av_frame` must be a live `*const AVFrame` for the duration of this
/// call. Reads each individual field through the raw pointer; never
/// forms a `&AVFrame` reference.
unsafe fn build_video_frame_extra(av_frame: *const AVFrame) -> VideoFrameExtra {
  let mut out = VideoFrameExtra::default();
  // SAR.
  let sar_num = unsafe { (*av_frame).sample_aspect_ratio.num };
  let sar_den = unsafe { (*av_frame).sample_aspect_ratio.den };
  if sar_num > 0 && sar_den > 0 && (sar_num != 1 || sar_den != 1) {
    out.set_sample_aspect_ratio(Some((sar_num as u32, sar_den as u32)));
  }
  // Picture type — read raw to avoid bindgen-enum UB if FFmpeg writes
  // an out-of-range value (version skew / hostile decoder).

  // SAFETY: `av_frame` is live; reading `pict_type` as `i32` matches
  // the bindgen enum's underlying `c_int` storage.
  let pict_type_raw = unsafe { read_unaligned(addr_of!((*av_frame).pict_type) as *const i32) };
  out.set_picture_type(map_picture_type_raw(pict_type_raw));
  // Key frame and interlace flags. AVFrame.flags has dedicated bits
  // for these in recent FFmpeg; the deprecated fields (key_frame,
  // interlaced_frame, top_field_first) still mirror them.
  let flags = unsafe { (*av_frame).flags };
  out.set_key_frame(flags & ffmpeg_next::ffi::AV_FRAME_FLAG_KEY != 0);
  out.set_interlaced(flags & ffmpeg_next::ffi::AV_FRAME_FLAG_INTERLACED != 0);
  out.set_top_field_first(flags & ffmpeg_next::ffi::AV_FRAME_FLAG_TOP_FIELD_FIRST != 0);
  // Best-effort timestamp.
  let bet = unsafe { (*av_frame).best_effort_timestamp };
  if bet != AV_NOPTS_VALUE {
    out.set_best_effort_timestamp(Some(bet));
  }
  // Side data — passthrough as raw bytes.
  out.set_side_data(unsafe { collect_side_data(av_frame) });
  out
}

/// Maximum number of `AVFrameSideData` entries we will copy out of
/// a single AVFrame. Realistic streams attach a handful (mastering
/// display, content light level, dynamic HDR metadata, S12M
/// timecodes, A53 captions, …) — usually < 8. The cap exists so a
/// crafted stream can't drive the safe converter into a long
/// per-frame entry-allocation loop.
pub(crate) const SIDE_DATA_MAX_ENTRIES: usize = 64;
/// Per-AVFrame total side-data byte cap. HDR / dynamic-metadata
/// payloads are typically a few hundred bytes; A53 captions can run
/// to a few kilobytes; SEI dumps in pathological streams have been
/// observed in the tens of kilobytes. 256 KiB is two orders of
/// magnitude over the realistic upper bound while still bounded
/// enough that an attacker-driven OOM via metadata is impossible.
pub(crate) const SIDE_DATA_MAX_TOTAL_BYTES: usize = 256 * 1024;

/// Maximum number of `AVSubtitleRect` entries we copy from a single
/// AVSubtitle. Realistic subtitles attach 1–4 rects per cue; 64
/// gives two orders of magnitude of headroom.
const SUBTITLE_MAX_RECTS: usize = 64;
/// Per-rect text/ASS payload byte cap. ASS lines exceeding this
/// are unrealistic; the cap exists to defeat a malicious decoder
/// attaching a multi-megabyte "subtitle" string.
const SUBTITLE_MAX_TEXT_BYTES_PER_RECT: usize = 64 * 1024;
/// Total text/ASS payload byte cap across all rects of a single
/// AVSubtitle, including newline separators.
const SUBTITLE_MAX_TEXT_TOTAL_BYTES: usize = 256 * 1024;
/// Per-rect bitmap (`linesize * height`) byte cap. DVB / PGS
/// subtitles realistically run to ~256 KiB on full-HD overlays;
/// 16 MiB is two orders of magnitude over.
const SUBTITLE_MAX_BITMAP_BYTES_PER_RECT: usize = 16 * 1024 * 1024;
/// Total bitmap byte cap across all rects of a single AVSubtitle.
const SUBTITLE_MAX_BITMAP_TOTAL_BYTES: usize = 32 * 1024 * 1024;

/// Bounded counterpart to `CStr::from_ptr(p).to_bytes()`. Reads at
/// most `cap + 1` bytes from `ptr` looking for a NUL terminator;
/// returns `Some(slice)` of the bytes preceding the NUL on success,
/// or `None` if no NUL was found within the window (the input was
/// either too long or missing its required terminator entirely).
///
/// `CStr::from_ptr` walks until it hits a NUL — a valid-but-
/// pathological string makes that scan unbounded, and a missing
/// NUL is an outright UB precondition violation. This helper bounds
/// both at `cap + 1` bytes.
///
/// # Safety
/// `ptr` must be non-null and valid for reads of at least
/// `min(cap + 1, length-until-NUL)` bytes. FFmpeg subtitle/text
/// pointers satisfy this when `(*rect).text` / `.ass` is non-null
/// (per FFmpeg's contract — though the contract itself doesn't
/// bound the length).
unsafe fn bounded_cstr_bytes<'a>(ptr: *const core::ffi::c_char, cap: usize) -> Option<&'a [u8]> {
  // Read up to `cap + 1` bytes; the +1 lets a string exactly `cap`
  // bytes long (with a NUL at index `cap`) succeed.
  let max = cap.saturating_add(1);
  for i in 0..max {
    // SAFETY: Caller guarantees `ptr` is valid for reads of bytes
    // until the NUL or `max`. We stop at the first NUL within the
    // window.
    let byte = unsafe { *(ptr.add(i) as *const u8) };
    if byte == 0 {
      // SAFETY: `ptr` is valid for `i` byte reads (we just walked
      // them above). The slice doesn't include the NUL.
      return Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, i) });
    }
  }
  // No NUL found within `cap + 1` bytes — input is too long or
  // missing its terminator. Reject.
  None
}

/// # Safety
/// `av_frame` must be a live `*const AVFrame`. The function reads
/// `nb_side_data` and `side_data[]` through the raw pointer; each
/// `AVFrameSideData.type_` is read raw (it's a bindgen enum), and
/// each `data` payload is bounds-checked before slicing.
///
/// Memory-safety stance: this function is called on every decoded
/// frame, on data the decoder controls. Side-data is bounded by
/// [`SIDE_DATA_MAX_ENTRIES`] entries and [`SIDE_DATA_MAX_TOTAL_BYTES`]
/// total bytes; once either cap is reached we stop copying further
/// entries and a `tracing::warn!` is emitted at most once per call.
/// Allocations use `try_reserve_exact` so OOM surfaces as a dropped
/// entry rather than a process abort.
unsafe fn collect_side_data(av_frame: *const AVFrame) -> std::vec::Vec<SideDataEntry> {
  // Read `nb_side_data` as the bindgen `c_int` and clamp non-
  // positive values BEFORE casting to `usize`. A negative value
  // (corrupt / version-skew decoder output) cast directly to
  // `usize` becomes a huge positive count and would walk OOB
  // memory below; treat it as "no side data".
  let nb_side_data_raw = unsafe { (*av_frame).nb_side_data };
  let side_data = unsafe { (*av_frame).side_data };
  if nb_side_data_raw <= 0 || side_data.is_null() {
    return Vec::new();
  }
  let count_raw = nb_side_data_raw as usize;
  let count = count_raw.min(SIDE_DATA_MAX_ENTRIES);
  if count_raw > SIDE_DATA_MAX_ENTRIES {
    tracing::warn!(
      cap = SIDE_DATA_MAX_ENTRIES,
      requested = count_raw,
      "mediadecode-ffmpeg: AVFrame.nb_side_data exceeds entry cap; truncating",
    );
  }
  let mut out: Vec<SideDataEntry> = Vec::new();
  if out.try_reserve_exact(count).is_err() {
    return Vec::new();
  }
  let mut total_bytes: usize = 0;
  for i in 0..count {
    let sd = unsafe { *side_data.add(i) };
    if sd.is_null() {
      continue;
    }
    // `AVFrameSideData.type_` is `AVFrameSideDataType` — bindgen
    // enum. Read raw to avoid forming an invalid value if FFmpeg
    // writes an unknown discriminant (version skew).
    let kind = unsafe { read_unaligned(addr_of!((*sd).type_) as *const i32) };
    let size = unsafe { (*sd).size };
    let data_ptr = unsafe { (*sd).data };
    let data_slice = if size == 0 || data_ptr.is_null() {
      FfmpegBytes::empty()
    } else {
      // Byte-budget check: stop copying further side-data entries
      // once we've reached the per-frame cap. Earlier entries
      // already in `out` stay; later entries are dropped.
      let projected = total_bytes.saturating_add(size);
      if projected > SIDE_DATA_MAX_TOTAL_BYTES {
        tracing::warn!(
          cap = SIDE_DATA_MAX_TOTAL_BYTES,
          projected,
          "mediadecode-ffmpeg: AVFrame side-data byte cap reached; dropping remaining entries",
        );
        break;
      }
      total_bytes = projected;
      // Staged through a `Vec` first, so `try_reserve_exact` keeps
      // *one* of the two payload-sized allocations a dropped entry
      // rather than a process abort. The carrier copy that follows is a
      // second full allocation of the same size — not a header — and it
      // is infallible; what the staging buys is that the first and
      // larger risk is reportable and the second is asked for a size
      // the allocator has just proved it has. Affordable only because
      // side data is capped at `SIDE_DATA_MAX_TOTAL_BYTES`; the plane
      // path next door is not small and uses the one-allocation road.
      let mut buf: Vec<u8> = Vec::new();
      if buf.try_reserve_exact(size).is_err() {
        continue;
      }
      // SAFETY: `data_ptr` is documented as valid for `size` bytes
      // per FFmpeg's AVFrameSideData contract.
      let src = unsafe { core::slice::from_raw_parts(data_ptr, size) };
      buf.extend_from_slice(src);
      FfmpegBytes::copy_from_slice(&buf)
    };
    out.push(SideDataEntry::new(kind, data_slice));
  }
  out
}

/// Totals a still's declared side data and judges it, **allocating
/// nothing and reading no payload**.
///
/// Split out of [`collect_image_side_data`] so it can run before the
/// planes are copied. It was not enough for the budget to be checked
/// before the side data was copied: `av_frame_to_image_frame` buys the
/// planes first, so an over-budget still had already paid for up to
/// `max_frame_bytes` of plane copies by the time its annotations were
/// judged. The refusal was correct and arrived after the expensive half
/// of the work.
///
/// Judging is free here. Every number this pass reads is a header
/// field — the entry count, and each entry's declared `size` — and no
/// payload is dereferenced. So it belongs at the front, with the other
/// free judgements.
///
/// # Safety
///
/// `av_frame` must be a live `*const AVFrame`.
unsafe fn measure_image_side_data(
  av_frame: *const AVFrame,
  limits: FrameLimits,
) -> Result<usize, ConvertError> {
  let nb_side_data_raw = unsafe { (*av_frame).nb_side_data };
  let side_data = unsafe { (*av_frame).side_data };
  if nb_side_data_raw <= 0 || side_data.is_null() {
    return Ok(0);
  }
  let count = nb_side_data_raw as usize;
  if count > SIDE_DATA_MAX_ENTRIES {
    return Err(ConvertError::ImageSideDataEntries(
      ImageSideDataEntries::new(count, SIDE_DATA_MAX_ENTRIES),
    ));
  }
  let budget = limits.max_image_side_data_bytes();
  let mut total: usize = 0;
  for i in 0..count {
    // The entry *pointer* comes out of the array; the entry itself is
    // read only for its declared size. A null slot is skipped exactly
    // as the copying pass skips it, so the two totals agree.
    let sd = unsafe { *side_data.add(i) };
    if sd.is_null() {
      continue;
    }
    let size = unsafe { (*sd).size };
    total = total.saturating_add(size);
    if total > budget {
      return Err(ConvertError::ImageSideDataTooLarge(
        ImageSideDataTooLarge::new(total, budget),
      ));
    }
  }
  Ok(total)
}

/// [`collect_side_data`] for the **still** road: budgeted, and it
/// refuses rather than truncating.
///
/// The two roads want different answers to the same overflow. A video
/// stream's frame side data is small, repeated, and per-frame, so the
/// shared collector's fixed caps and silent drop are a reasonable trade
/// — losing one frame's annotation is recoverable, and refusing a
/// frame mid-stream is not. A still is decoded once and *is* its
/// annotations: the ICC profile that decides its colours and the
/// display matrix that decides its orientation both live here, both are
/// carried by exactly one frame, and dropping either is not degradation
/// but a wrong picture returned as a right one.
///
/// So this collector takes a budget from [`FrameLimits`] and names its
/// refusals. See
/// [`DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES`](crate::DEFAULT_MAX_IMAGE_SIDE_DATA_BYTES)
/// for why the default is what the parameter road already admits.
///
/// # Safety
///
/// `av_frame` must be a live `*const AVFrame`.
unsafe fn collect_image_side_data(
  av_frame: *const AVFrame,
  limits: FrameLimits,
) -> Result<std::vec::Vec<SideDataEntry>, ConvertError> {
  // Same raw reads as the shared collector: a negative count is
  // malformed rather than empty, and the entry `type_` is an open C
  // enum read as the integer it is.
  let nb_side_data_raw = unsafe { (*av_frame).nb_side_data };
  let side_data = unsafe { (*av_frame).side_data };
  if nb_side_data_raw <= 0 || side_data.is_null() {
    return Ok(Vec::new());
  }
  let count = nb_side_data_raw as usize;
  let budget = limits.max_image_side_data_bytes();
  // **The measuring pass, re-run.** It runs earlier too — before the
  // planes are bought — and this is the copying pass. Repeating a pair
  // of comparisons that guard an allocation is defence in depth, not
  // duplication: it keeps this function correct on its own terms rather
  // than only in the order it happens to be called in.
  let total = unsafe { measure_image_side_data(av_frame, limits) }?;

  let mut out: Vec<SideDataEntry> = Vec::new();
  if out.try_reserve_exact(count).is_err() {
    return Err(ConvertError::ImageSideDataTooLarge(
      ImageSideDataTooLarge::new(total, budget),
    ));
  }
  for i in 0..count {
    let sd = unsafe { *side_data.add(i) };
    if sd.is_null() {
      continue;
    }
    let kind = unsafe { read_unaligned(addr_of!((*sd).type_) as *const i32) };
    let size = unsafe { (*sd).size };
    let data_ptr = unsafe { (*sd).data };
    let payload = if size == 0 || data_ptr.is_null() {
      FfmpegBytes::empty()
    } else {
      // SAFETY: `data_ptr` is documented as valid for `size` bytes per
      // FFmpeg's `AVFrameSideData` contract, and the total was proved
      // to fit the budget above.
      let src = unsafe { core::slice::from_raw_parts(data_ptr, size) };
      FfmpegBytes::copy_from_slice(src)
    };
    out.push(SideDataEntry::new(kind, payload));
  }
  Ok(out)
}

/// Locate the `AVBufferRef` in `(*av_frame).buf[]` that backs
/// `data_ptr`, confirming the requested `bytes` fit inside the buffer.
/// Returns `None` on no match, null/empty `buf` entries, or any
/// arithmetic that would overflow `usize`.
///
/// # Safety
/// `av_frame` must be a live `*const AVFrame`. Reads `buf[]` (an
/// array of pointers — no bindgen-enum validity hazards).
/// Captures `len` bytes at `data_ptr` out of whichever of the frame's
/// own buffers backs it.
///
/// **The proof runs before the capture, on both lanes.**
/// [`find_backing_buffer`] establishes that `data_ptr .. +len` lies
/// inside one of `(*av_frame).buf[]`; only then is the seam asked for a
/// carrier. The owned lane copies those bytes out; the view lane takes
/// a reference to the same range. Neither gets to skip the proof,
/// because it is written once, here.
///
/// The `len` a caller passes is therefore a claim about **what is
/// initialised**, and each medium computes it differently — see the
/// call sites for the per-medium rules.
///
/// # Safety
///
/// `av_frame` must be a live `*const AVFrame` and `data_ptr` must point
/// into one of its planes.
unsafe fn capture_from_backing<C: crate::FfmpegCarrier + crate::CarrierOps>(
  av_frame: *const AVFrame,
  data_ptr: *const u8,
  len: usize,
  plane_idx: usize,
) -> Result<C::Buffer, ConvertError> {
  // SAFETY: the caller upholds `av_frame`'s liveness and `data_ptr`'s
  // provenance.
  let backing = unsafe { find_backing_buffer(av_frame, data_ptr, len) }.ok_or(
    ConvertError::BufferAcquireFailed(BufferAcquireFailed::new(plane_idx)),
  )?;
  // SAFETY: `backing` is one of the frame's live buffers and was just
  // proved to cover `len` bytes from `data_ptr`.
  let offset = unsafe { (data_ptr as usize).wrapping_sub((*backing).data as usize) };
  // SAFETY: the offset and length were proved to lie inside `backing`.
  unsafe { C::capture(backing, offset, len) }.ok_or(ConvertError::CarrierAllocFailed(
    CarrierAllocFailed::new(plane_idx),
  ))
}

unsafe fn find_backing_buffer(
  av_frame: *const AVFrame,
  data_ptr: *const u8,
  bytes: usize,
) -> Option<*mut ffmpeg_next::ffi::AVBufferRef> {
  let buf_array_len = unsafe { (*av_frame).buf.len() };
  for i in 0..buf_array_len {
    let buf = unsafe { (*av_frame).buf[i] };
    if buf.is_null() {
      continue;
    }
    let buf_data = unsafe { (*buf).data as *const u8 };
    let buf_size = unsafe { (*buf).size };
    if buf_data.is_null() {
      continue;
    }
    let start = buf_data as usize;
    let Some(end) = start.checked_add(buf_size) else {
      continue;
    };
    let dp = data_ptr as usize;
    let Some(dp_end) = dp.checked_add(bytes) else {
      continue;
    };
    if dp >= start && dp_end <= end {
      return Some(buf);
    }
  }
  None
}

fn map_primaries(raw: i32) -> ColorPrimaries {
  match raw {
    x if x == AVColorPrimaries::AVCOL_PRI_BT709 as i32 => ColorPrimaries::Bt709,
    x if x == AVColorPrimaries::AVCOL_PRI_UNSPECIFIED as i32 => ColorPrimaries::Unspecified,
    x if x == AVColorPrimaries::AVCOL_PRI_BT470M as i32 => ColorPrimaries::Bt470M,
    x if x == AVColorPrimaries::AVCOL_PRI_BT470BG as i32 => ColorPrimaries::Bt470Bg,
    x if x == AVColorPrimaries::AVCOL_PRI_SMPTE170M as i32 => ColorPrimaries::Smpte170M,
    x if x == AVColorPrimaries::AVCOL_PRI_SMPTE240M as i32 => ColorPrimaries::Smpte240M,
    x if x == AVColorPrimaries::AVCOL_PRI_FILM as i32 => ColorPrimaries::Film,
    x if x == AVColorPrimaries::AVCOL_PRI_BT2020 as i32 => ColorPrimaries::Bt2020,
    x if x == AVColorPrimaries::AVCOL_PRI_SMPTE428 as i32 => ColorPrimaries::SmpteSt428,
    x if x == AVColorPrimaries::AVCOL_PRI_SMPTE431 as i32 => ColorPrimaries::SmpteRp431,
    x if x == AVColorPrimaries::AVCOL_PRI_SMPTE432 as i32 => ColorPrimaries::SmpteEg432,
    x if x == AVColorPrimaries::AVCOL_PRI_EBU3213 as i32 => ColorPrimaries::Ebu3213E,
    _ => ColorPrimaries::Unspecified,
  }
}

fn map_transfer(raw: i32) -> ColorTransfer {
  match raw {
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_BT709 as i32 => ColorTransfer::Bt709,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_UNSPECIFIED as i32 => {
      ColorTransfer::Unspecified
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_GAMMA22 as i32 => ColorTransfer::Gamma22,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_GAMMA28 as i32 => ColorTransfer::Gamma28,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_SMPTE170M as i32 => ColorTransfer::Smpte170M,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_SMPTE240M as i32 => ColorTransfer::Smpte240M,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_LINEAR as i32 => ColorTransfer::Linear,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_LOG as i32 => ColorTransfer::Log100,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_LOG_SQRT as i32 => ColorTransfer::Log316,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_IEC61966_2_4 as i32 => {
      ColorTransfer::Iec6196624
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_BT1361_ECG as i32 => {
      ColorTransfer::Bt1361Ecg
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_IEC61966_2_1 as i32 => {
      ColorTransfer::Iec6196621
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_BT2020_10 as i32 => {
      ColorTransfer::Bt2020_10Bit
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_BT2020_12 as i32 => {
      ColorTransfer::Bt2020_12Bit
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_SMPTE2084 as i32 => {
      ColorTransfer::SmpteSt2084Pq
    }
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_SMPTE428 as i32 => ColorTransfer::SmpteSt428,
    x if x == AVColorTransferCharacteristic::AVCOL_TRC_ARIB_STD_B67 as i32 => {
      ColorTransfer::AribStdB67Hlg
    }
    _ => ColorTransfer::Unspecified,
  }
}

fn map_matrix(raw: i32) -> ColorMatrix {
  match raw {
    x if x == AVColorSpace::AVCOL_SPC_BT709 as i32 => ColorMatrix::Bt709,
    x if x == AVColorSpace::AVCOL_SPC_BT2020_NCL as i32 => ColorMatrix::Bt2020Ncl,
    x if x == AVColorSpace::AVCOL_SPC_SMPTE170M as i32 => ColorMatrix::Bt601,
    x if x == AVColorSpace::AVCOL_SPC_BT470BG as i32 => ColorMatrix::Bt601,
    x if x == AVColorSpace::AVCOL_SPC_SMPTE240M as i32 => ColorMatrix::Smpte240m,
    x if x == AVColorSpace::AVCOL_SPC_FCC as i32 => ColorMatrix::Fcc,
    x if x == AVColorSpace::AVCOL_SPC_YCGCO as i32 => ColorMatrix::YCgCo,
    _ => ColorMatrix::Bt709, // ColorMatrix has no Unspecified; Bt709 is FFmpeg's height>=720 default
  }
}

fn map_range(raw: i32) -> ColorRange {
  match raw {
    x if x == AVColorRange::AVCOL_RANGE_JPEG as i32 => ColorRange::Full,
    x if x == AVColorRange::AVCOL_RANGE_MPEG as i32 => ColorRange::Limited,
    _ => ColorRange::Unspecified,
  }
}

/// `true` for the JPEG-range planar YUV (`yuvj*`) formats. These are
/// **full-range by definition** — the `j` is FFmpeg's marker for an
/// MJPEG/JPEG-family full-swing signal — so their color range is a
/// property of the format itself, not something the frame's
/// `color_range` field needs to (or reliably does) carry.
fn is_yuvj(pix_fmt: &PixelFormat) -> bool {
  matches!(
    pix_fmt,
    PixelFormat::Yuvj411p
      | PixelFormat::Yuvj420p
      | PixelFormat::Yuvj422p
      | PixelFormat::Yuvj440p
      | PixelFormat::Yuvj444p
  )
}

/// Derives the delivered [`ColorRange`] from the frame's `color_range`
/// field, honoring the range a pixel format *implies*.
///
/// A `yuvj*` frame is JPEG full-range by definition, but its
/// `AVFrame.color_range` is frequently `AVCOL_RANGE_UNSPECIFIED` (the
/// MJPEG/JPEG decode paths don't always stamp it). Deriving the range
/// purely from that field would mislabel a full-range frame as
/// `Unspecified` (which downstream YUV→RGB conversion reads as the
/// Limited-swing default) — a silent decode-correctness regression. So
/// for the `yuvj*` family we force [`ColorRange::Full`] regardless of
/// the field. Every other format defers entirely to `color_range`.
fn map_range_for(pix_fmt: &PixelFormat, color_range_raw: i32) -> ColorRange {
  if is_yuvj(pix_fmt) {
    return ColorRange::Full;
  }
  map_range(color_range_raw)
}

fn map_chroma_loc(raw: i32) -> ChromaLocation {
  match raw {
    x if x == AVChromaLocation::AVCHROMA_LOC_LEFT as i32 => ChromaLocation::Left,
    x if x == AVChromaLocation::AVCHROMA_LOC_CENTER as i32 => ChromaLocation::Center,
    x if x == AVChromaLocation::AVCHROMA_LOC_TOPLEFT as i32 => ChromaLocation::TopLeft,
    x if x == AVChromaLocation::AVCHROMA_LOC_TOP as i32 => ChromaLocation::Top,
    x if x == AVChromaLocation::AVCHROMA_LOC_BOTTOMLEFT as i32 => ChromaLocation::BottomLeft,
    x if x == AVChromaLocation::AVCHROMA_LOC_BOTTOM as i32 => ChromaLocation::Bottom,
    _ => ChromaLocation::Unspecified,
  }
}

/// Converts an FFmpeg audio `AVFrame` into a `mediadecode::AudioFrame`.
///
/// Each plane is copied out of the source frame's `AVBufferRef`
/// entries into an `FfmpegBytes` (the corresponding `data[i]` is always
/// covered by exactly one of `buf[i]` per FFmpeg's contract, which is
/// what bounds the read). Channel counts above 8 (which would spill
/// into `extended_buf`) are refused rather than clamped — see the
/// plane-count check below.
///
/// # Safety
///
/// `av_frame` must be a live `*const AVFrame` for the duration of this
/// call and must describe an audio frame (`format` is an
/// `AVSampleFormat`, `nb_samples > 0`, and `data[]` / `buf[]`
/// populated). The frame's buffers are neither consumed nor
/// referenced; every byte the produced `AudioFrame` carries is a copy.
/// * no handle capable of **mutating** the frame's buffers may
///   outlive this call while the returned carriers do. On the view
///   lane a plane is a window into `frame`'s own allocation, and
///   `ffmpeg_next`'s wrappers lend `&mut [u8]` by refcount with no
///   copy-on-write — so keeping the source frame and writing through
///   it would race a carrier a consumer is reading. Consume the
///   frame, or use the owned lane, or use the safe borrowed wrapper
///   (which is the owned lane for exactly this reason).
pub(crate) unsafe fn av_frame_to_audio_frame_as<C: crate::FfmpegCarrier + crate::CarrierOps>(
  av_frame: *const AVFrame,
  time_base: Timebase,
  limits: FrameLimits,
) -> Result<
  AudioFrame<SampleFormat, ChannelLayoutDescription, AudioFrameExtra, C::Buffer>,
  ConvertError,
> {
  if av_frame.is_null() {
    return Err(ConvertError::NullFrame);
  }
  // Same stance as `av_frame_to_video_frame`: never form `&AVFrame`.
  // Read every field through the raw pointer; for `ch_layout` (which
  // contains an `order: AVChannelOrder` enum) we hand the raw pointer
  // straight into
  // `channel_layout::channel_layout_description_from_raw_ptr`,
  // which validates `order` as `i32` before constructing any
  // `AVChannelOrder` value.
  let format_raw = unsafe { (*av_frame).format };
  let sample_rate_raw = unsafe { (*av_frame).sample_rate };
  let nb_samples_raw = unsafe { (*av_frame).nb_samples };
  let pts_raw = unsafe { (*av_frame).pts };
  let duration_raw = unsafe { (*av_frame).duration };
  let bet_raw = unsafe { (*av_frame).best_effort_timestamp };

  let sample_format = SampleFormat::from_raw(format_raw);
  let sample_rate = sample_rate_raw.max(0) as u32;

  // **Every header field is judged here, before a byte of geometry is
  // computed — and none of them is clamped.**
  //
  // A clamp on this road is silent truncation of an attacker-supplied
  // number, which is the exact sin this boundary exists to refuse. The
  // three that mattered each produced a *well-formed-looking* frame out
  // of a malformed one, which is worse than an error: a floored
  // negative count became an empty frame a consumer went on decoding
  // past, and a clipped channel count made a packed frame compute its
  // byte product from 255 when the file said 256 — copying 510 of 512
  // bytes and advertising the wrong shape.
  //
  // The one survivor is `sample_rate`, floored above. Censused and
  // kept: it feeds no geometry, no allocation and no copy length — it
  // is metadata — and zero is already this crate's "rate unspecified".
  // Nothing downstream sizes anything from it.
  if nb_samples_raw < 0 {
    return Err(ConvertError::InvalidSampleCount(InvalidSampleCount::new(
      nb_samples_raw,
    )));
  }
  let nb_samples = nb_samples_raw as u32;

  // SAFETY: `av_frame` is a live `*const AVFrame`; passing the
  // address of the embedded ch_layout as `*const AVChannelLayout`
  // is sound because `addr_of!` doesn't form a reference.
  let ch_layout_ptr = unsafe { addr_of!((*av_frame).ch_layout) };

  // **The channel count is judged off the raw field, before the layout
  // is materialised.** The first version of this guard read it back off
  // the `ChannelLayoutDescription`, which was two bugs at once:
  //
  // * the description stores `nb_channels.max(0) as u32`, so a declared
  //   `-1` reached the guard as a legitimate-looking zero and produced
  //   a zero-channel frame instead of a refusal — the validator was
  //   reading a number its own consumer had already laundered; and
  // * materialising runs first. For an `AV_CHANNEL_ORDER_CUSTOM`
  //   layout that means rendering the layout's name and walking
  //   `nb_channels` map entries into a `Vec` — work proportional to a
  //   number this very guard exists to bound, done *before* the bound
  //   is applied.
  //
  // A validator downstream of its own field's first consumer is not a
  // validator. The raw signed read comes first, every refusal is stated
  // against it, and only a count already proved to be in `0..=255` is
  // allowed to drive the description.
  //
  // SAFETY: `ch_layout_ptr` addresses the frame's live embedded layout.
  // `nb_channels` is a plain `c_int`, so a direct field read through the
  // raw pointer is sound — the enum-typed `order` beside it is what
  // needs `addr_of!` + a raw `i32` read, and that read happens inside
  // the description helper below, not here.
  let channel_count_raw = unsafe { (*ch_layout_ptr).nb_channels };
  if channel_count_raw < 0 {
    return Err(ConvertError::UnsupportedChannelCount(
      UnsupportedChannelCount::new(channel_count_raw),
    ));
  }
  // Refused before any plane geometry, and refused for packed layouts
  // too — which the old `> 8` plane check never reached, because packed
  // audio declares one plane whatever its channel count is.
  if channel_count_raw > i32::from(u8::MAX) {
    return Err(ConvertError::UnsupportedChannelCount(
      UnsupportedChannelCount::new(channel_count_raw),
    ));
  }
  // A frame carrying samples across no channels is not an empty frame;
  // it is an incoherent one. The packed byte product used to substitute
  // 1 here, which invented a channel the file never declared.
  if channel_count_raw == 0 && nb_samples > 0 {
    return Err(ConvertError::UnsupportedChannelCount(
      UnsupportedChannelCount::new(channel_count_raw),
    ));
  }
  let channel_count_full = channel_count_raw as u32;
  let channel_count = channel_count_raw as u8;

  // Materialised only now, with the count it will report already
  // proved to be one this crate can carry. Because the raw field is in
  // `0..=255`, the description's own `nb_channels.max(0)` is the
  // identity here and its `channels()` equals `channel_count_full`.
  let channel_layout =
    unsafe { crate::channel_layout::channel_layout_description_from_raw_ptr(ch_layout_ptr) };
  debug_assert_eq!(
    channel_layout.channels(),
    channel_count_full,
    "the description must report the count that was judged",
  );

  // The sample format, **before** the zero-sample shortcut: a frame
  // whose format has no byte width is malformed whether or not it
  // carries samples, and letting an empty one through returned an
  // `AudioFrame` advertising a format nothing can interpret.
  let bytes_per_sample =
    sample_format
      .bytes_per_sample()
      .ok_or(ConvertError::UnsupportedSampleFormat(
        UnsupportedSampleFormat::new(format_raw),
      ))? as usize;

  // Plane count: 1 for packed, channel_count for planar.
  let is_planar = sample_format.is_planar();
  let plane_count_full = if is_planar { channel_count as usize } else { 1 };
  // mediadecode's `AudioFrame` carries up to 8 plane slots
  // (matching `AV_NUM_DATA_POINTERS`). Planar audio with more than
  // 8 channels uses `AVFrame.extended_data[]` / `extended_buf[]`,
  // which we don't yet plumb through. Refuse the frame rather than
  // silently truncating to the first 8 channels and returning an
  // `AudioFrame` whose advertised `channel_count` exceeds its
  // populated plane count.
  if plane_count_full > 8 {
    return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(8)));
  }
  let plane_count = plane_count_full as u8;

  // **Two different numbers, and conflating them was a bug.**
  //
  // `linesize[0]` is what FFmpeg *allocated* per plane, which
  // `av_samples_get_buffer_size` rounds up for alignment — routinely
  // 32 or 64 bytes past the samples. The bytes that are *valid* are
  // `nb_samples * bytes_per_sample`, per plane when planar and times
  // the channel count when packed. Nothing initialises the difference.
  //
  // Exporting `linesize` therefore did two wrong things at once: it
  // formed a `&[u8]` over maybe-uninitialised padding, which is
  // undefined behaviour before anything reads it, and it handed that
  // padding to a consumer inside a safe `FfmpegBytes` — stale heap,
  // leaked through an owned carrier.
  //
  // So `linesize` is used for exactly one thing below: proving the
  // source allocation really is as large as it claims. What is copied
  // is the valid product. This is what the resampler's own output path
  // has always done (`per_sample * produced`); the decode path now
  // agrees with it.
  let linesize0 = unsafe { (*av_frame).linesize[0] };
  // A negative allocation is incoherent at any sample count, so it is
  // refused before the count is consulted rather than floored to zero.
  // Zero itself is only refused when the frame claims samples — it is
  // the canonical shape of an empty audio frame.
  if linesize0 < 0 || (nb_samples > 0 && linesize0 == 0) {
    return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)));
  }
  let allocated_per_plane = linesize0 as usize;
  let valid_per_plane = if nb_samples_raw == 0 {
    // A header frame: real, and carrying no samples. There is nothing
    // valid to export, whatever the allocation says. Reached only for a
    // count that is *exactly* zero — a negative one was refused by name
    // above rather than floored into this branch.
    0
  } else {
    let valid = if is_planar {
      // Planar: each plane carries `nb_samples * bytes_per_sample`.
      (nb_samples as usize)
        .checked_mul(bytes_per_sample)
        .ok_or(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)))?
    } else {
      // Packed: the single plane interleaves all channels.
      // The **declared** channel count, never a substituted one: it was
      // proved above to be in `1..=u8::MAX` on a frame with samples.
      (nb_samples as usize)
        .checked_mul(bytes_per_sample)
        .and_then(|x| x.checked_mul(channel_count_full as usize))
        .ok_or(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)))?
    };
    // The allocation must cover the samples the header claims —
    // otherwise a shrunk `linesize` would let a consumer that trusts
    // `nb_samples` read past what is there.
    if allocated_per_plane < valid {
      return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)));
    }
    valid
  };

  // The byte ceiling, before a single plane is allocated. An audio
  // frame has no pixels to bound, so this is the whole ceiling here —
  // and it is needed: `linesize[0]` is a number from the decoder, and
  // the check above only proves it is not *smaller* than the format
  // requires. Nothing above bounds it from the other side.
  let exported =
    valid_per_plane
      .checked_mul(plane_count as usize)
      .ok_or(ConvertError::FrameTooLarge(FrameTooLarge::new(
        usize::MAX,
        limits.max_frame_bytes(),
      )))?;
  if exported > limits.max_frame_bytes() {
    return Err(ConvertError::FrameTooLarge(FrameTooLarge::new(
      exported,
      limits.max_frame_bytes(),
    )));
  }

  // Every slot starts as the shared empty carrier at stride zero, which
  // is already exactly what a zero-sample frame's planes should be.
  let mut planes_out: [Plane<C::Buffer>; 8] = std::array::from_fn(|_| plane_placeholder::<C>());

  // **A zero-sample frame has no planes to validate.** FFmpeg's
  // canonical empty audio frame carries a format, a layout and a rate
  // with `data[i] == NULL`, `linesize == 0` and no `AVBufferRef` at
  // all — there is nothing allocated because there is nothing to hold.
  // Running the loop below over it refused the frame on the first null
  // pointer, so a header frame mid-stream came back as
  // `InvalidPlaneLayout` and interrupted a decode that was going fine.
  //
  // The declared layout is still reported: `plane_count` stays packed's
  // 1 or planar's channel count, and those slots hold the empty carrier
  // at stride 0 — a consumer sees the shape it expects, carrying no
  // samples, which is what the frame says. No allocation happens; the
  // empty carrier is one refcount bump.
  //
  // Nothing below changes for a frame that does carry samples: the loop
  // body is untouched, and this only decides whether it runs at all.
  let populated = if valid_per_plane == 0 {
    0
  } else {
    plane_count as usize
  };

  // Same rationale as in the video path — index-by-key over three
  // unrelated raw arrays (`planes_out`, `(*av_frame).data`, and the
  // implicit per-plane bookkeeping); no slice iteration applies.
  #[allow(clippy::needless_range_loop)]
  for plane_idx in 0..populated {
    let data_ptr = unsafe { (*av_frame).data[plane_idx] };
    if data_ptr.is_null() {
      // A null plane in a planar layout (or the sole plane in a
      // packed layout) means the decoder produced an incomplete
      // frame — surface as an error rather than returning a frame
      // whose `planes()` exposes empty placeholder channels for
      // the missing data.
      return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(
        plane_idx,
      )));
    }
    // The bounds proof, against the **allocation**: the plane really is
    // as large as its `linesize` claims, and lies inside one of the
    // frame's own buffers. This is the only thing `linesize` is used
    // for.
    let backing = unsafe { find_audio_backing_buffer(av_frame, data_ptr, allocated_per_plane) }
      .ok_or(ConvertError::BufferAcquireFailed(BufferAcquireFailed::new(
        plane_idx,
      )))?;
    // Lossless, and provably so rather than by ceiling: this branch runs
    // only when `valid_per_plane <= allocated_per_plane`, which is an
    // `i32` read from `linesize[0]` proved non-negative above. No plane
    // can exceed `i32::MAX` bytes, so nothing here is truncated even if
    // a caller raises `max_frame_bytes` past `u32::MAX`.
    // **Audio stops at exactly the valid bytes, on both lanes.**
    // `linesize[0]` is what `av_samples_get_buffer_size` *allocated*,
    // rounded up for alignment; what the decoder wrote is
    // `nb_samples * bytes_per_sample` (times the channels when packed).
    // The difference is untouched allocator memory — the R5 finding —
    // and it is no more exportable through a view than it was through a
    // copy: a carrier is an `AsRef<[u8]>`, so the span it names is the
    // span a consumer may read, and padding in that span is the same
    // information leak whoever formed it.
    //
    // So the view lane shares the **prefix**, not the plane. Which is
    // also why `linesize` is used for exactly one thing here: proving
    // the allocation really is as large as it claims.
    //
    // SAFETY: `backing` is one of the frame's live buffers, proved above
    // to cover `allocated_per_plane` bytes from `data_ptr`, and
    // `valid_per_plane <= allocated_per_plane`.
    let offset = unsafe { (data_ptr as usize).wrapping_sub((*backing).data as usize) };
    // SAFETY: the offset and length lie inside `backing` by the proof
    // above.
    let carried = unsafe { C::capture(backing, offset, valid_per_plane) }.ok_or(
      ConvertError::CarrierAllocFailed(CarrierAllocFailed::new(plane_idx)),
    )?;
    planes_out[plane_idx] = Plane::new(carried, valid_per_plane as u32);
  }

  let pts = if pts_raw != AV_NOPTS_VALUE {
    Some(Timestamp::new(pts_raw, time_base))
  } else {
    None
  };
  let duration = if duration_raw > 0 {
    Some(Timestamp::new(duration_raw, time_base))
  } else {
    None
  };

  let mut extra = AudioFrameExtra::default();
  if bet_raw != AV_NOPTS_VALUE {
    extra.set_best_effort_timestamp(Some(bet_raw));
  }
  // SAFETY: caller upholds liveness for the duration of the call;
  // collect_side_data reads enum-typed `type_` raw and bounds-checks
  // each entry's data slice.
  extra.set_side_data(unsafe { collect_side_data(av_frame) });

  Ok(
    AudioFrame::new(
      sample_rate,
      nb_samples,
      channel_count,
      sample_format,
      channel_layout,
      planes_out,
      plane_count,
      extra,
    )
    .with_pts(pts)
    .with_duration(duration),
  )
}

/// The `AVBufferRef` in `(*av_frame).buf[]` that backs `data_ptr` for
/// `bytes` bytes, or `None` when none of them does.
///
/// # Safety
/// `av_frame` must be a live `*const AVFrame`.
pub(crate) unsafe fn find_audio_backing_buffer(
  av_frame: *const AVFrame,
  data_ptr: *const u8,
  bytes: usize,
) -> Option<*mut ffmpeg_next::ffi::AVBufferRef> {
  // Audio frames pack each plane into a separate AVBufferRef in buf[].
  // Same scan as the video path — finds whichever buffer's data range
  // contains data_ptr. Overflow-safe arithmetic per
  // `find_backing_buffer`'s rationale.
  let buf_array_len = unsafe { (*av_frame).buf.len() };
  for i in 0..buf_array_len {
    let buf = unsafe { (*av_frame).buf[i] };
    if buf.is_null() {
      continue;
    }
    let buf_data = unsafe { (*buf).data as *const u8 };
    let buf_size = unsafe { (*buf).size };
    if buf_data.is_null() {
      continue;
    }
    let start = buf_data as usize;
    let Some(end) = start.checked_add(buf_size) else {
      continue;
    };
    let dp = data_ptr as usize;
    let Some(dp_end) = dp.checked_add(bytes) else {
      continue;
    };
    if dp >= start && dp_end <= end {
      return Some(buf);
    }
  }
  None
}

/// Converts an FFmpeg `AVSubtitle` into a `mediadecode::SubtitleFrame`.
///
/// Strategy:
/// - If the subtitle contains any text/ASS rects, produce a
///   [`SubtitlePayload::Text`] whose buffer is the concatenation of
///   their UTF-8 contents (newline-separated).
/// - Otherwise, if the subtitle contains bitmap rects, produce a
///   [`SubtitlePayload::Bitmap`] with one [`mediadecode::subtitle::BitmapRegion`]
///   per rect (paletted indices and RGBA palette copied into fresh
///   owned `FfmpegBytes` carriers, since `AVSubtitleRect` data is not
///   refcounted and does not outlive the `AVSubtitle`).
/// - An empty subtitle (no rects) becomes an empty `Text` payload.
///
/// `time_base` is the source stream's time base, used to label
/// `pts` / `duration`. The duration is computed as
/// `(end_display_time - start_display_time)` in milliseconds, then
/// rescaled into `time_base`.
///
/// # Safety
///
/// `av_subtitle` must be a live `*const AVSubtitle` for the duration
/// of this call; the rect array (`av_subtitle.rects`) must be valid
/// for `av_subtitle.num_rects` entries.
/// * no handle capable of **mutating** the frame's buffers may
///   outlive this call while the returned carriers do. On the view
///   lane a plane is a window into `frame`'s own allocation, and
///   `ffmpeg_next`'s wrappers lend `&mut [u8]` by refcount with no
///   copy-on-write — so keeping the source frame and writing through
///   it would race a carrier a consumer is reading. Consume the
///   frame, or use the owned lane, or use the safe borrowed wrapper
///   (which is the owned lane for exactly this reason).
pub(crate) unsafe fn av_subtitle_to_subtitle_frame_as<
  C: crate::FfmpegCarrier + crate::CarrierOps,
>(
  av_subtitle: *const ffmpeg_next::ffi::AVSubtitle,
  time_base: Timebase,
) -> Result<SubtitleFrame<SubtitleFrameExtra, C::Buffer>, ConvertError> {
  if av_subtitle.is_null() {
    return Err(ConvertError::NullFrame);
  }
  // Same stance as `av_frame_to_video_frame`: never form `&AVSubtitle`
  // or `&AVSubtitleRect` (both contain `type_: AVSubtitleType` enum
  // fields). Read every field through the raw pointer.

  let mut text_chunks: std::vec::Vec<u8> = std::vec::Vec::new();
  let mut bitmap_regions: std::vec::Vec<mediadecode::subtitle::BitmapRegion<C::Buffer>> =
    std::vec::Vec::new();

  let count_raw = unsafe { (*av_subtitle).num_rects } as usize;
  let rects_ptr = unsafe { (*av_subtitle).rects };
  // Defensive: `num_rects > 0` with `rects == null` would be a malformed
  // AVSubtitle, but a hostile decoder could produce one — bail rather
  // than dereferencing.
  if count_raw > 0 && rects_ptr.is_null() {
    return Err(ConvertError::NullFrame);
  }
  // Cap rect count, total text bytes, and total bitmap bytes
  // against decoder-controlled metadata. Realistic subtitles carry
  // a handful of rects (typically 1–4 per displayed cue), text
  // payloads in the low kilobytes (ASS lines), and bitmap
  // payloads in the low hundreds of KiB (DVB / PGS). These caps
  // are two orders of magnitude over realistic ceilings; their
  // job is to bound a malicious / corrupt stream's allocation
  // budget, not to limit legitimate use.
  let count = count_raw.min(SUBTITLE_MAX_RECTS);
  if count_raw > SUBTITLE_MAX_RECTS {
    tracing::warn!(
      cap = SUBTITLE_MAX_RECTS,
      requested = count_raw,
      "mediadecode-ffmpeg: AVSubtitle.num_rects exceeds rect cap; truncating",
    );
  }
  let mut text_total_bytes: usize = 0;
  let mut bitmap_total_bytes: usize = 0;

  let text_kind = AVSubtitleType::SUBTITLE_TEXT as i32;
  let ass_kind = AVSubtitleType::SUBTITLE_ASS as i32;
  let bitmap_kind = AVSubtitleType::SUBTITLE_BITMAP as i32;
  for i in 0..count {
    // SAFETY: rects_ptr is non-null (checked above) and points to
    // num_rects valid `*mut AVSubtitleRect` entries per FFmpeg's
    // contract; `i < count == num_rects`, so the offset is in-bounds.
    let rect_ptr = unsafe { *rects_ptr.add(i) };
    if rect_ptr.is_null() {
      continue;
    }
    // Read `type_` raw — avoid forming `&AVSubtitleRect` (which
    // would require type_ to be a valid AVSubtitleType variant).
    // SAFETY: `rect_ptr` is a live `*mut AVSubtitleRect`; `addr_of!`
    // computes the field address without forming a reference;
    // reading as `i32` matches the bindgen enum's `c_int` storage.
    let rect_type_raw = unsafe { read_unaligned(addr_of!((*rect_ptr).type_) as *const i32) };
    // Pre-read primitive fields we'll use later (no `&AVSubtitleRect`
    // ever formed).
    let rect_text_ptr = unsafe { (*rect_ptr).text };
    let rect_ass_ptr = unsafe { (*rect_ptr).ass };
    let rect_data0_ptr = unsafe { (*rect_ptr).data[0] };
    let rect_data1_ptr = unsafe { (*rect_ptr).data[1] };
    let rect_linesize0 = unsafe { (*rect_ptr).linesize[0] };
    let rect_w = unsafe { (*rect_ptr).w };
    let rect_h = unsafe { (*rect_ptr).h };
    let rect_x = unsafe { (*rect_ptr).x };
    let rect_y = unsafe { (*rect_ptr).y };

    match rect_type_raw {
      x if x == text_kind && !rect_text_ptr.is_null() => {
        // SAFETY: `text` is documented as a 0-terminated UTF-8
        // string, owned by FFmpeg for the lifetime of the AVSubtitle.
        // We use a *bounded* NUL search instead of `CStr::from_ptr`
        // — the latter walks until it finds a NUL, which a valid-
        // but-pathological string makes unbounded, and a missing
        // NUL violates the `CStr::from_ptr` precondition outright.
        // `bounded_cstr_bytes` searches at most
        // `SUBTITLE_MAX_TEXT_BYTES_PER_RECT + 1` bytes; if no NUL
        // is found inside that window the rect is rejected.
        let bytes = unsafe { bounded_cstr_bytes(rect_text_ptr, SUBTITLE_MAX_TEXT_BYTES_PER_RECT) }
          .ok_or(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)))?;
        // The cap is now enforced inside `bounded_cstr_bytes` (no
        // NUL within `cap + 1` ⇒ rejection); a redundant length
        // check is unnecessary but kept as documentation.
        if bytes.len() > SUBTITLE_MAX_TEXT_BYTES_PER_RECT {
          return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)));
        }
        let separator = if text_chunks.is_empty() { 0 } else { 1 };
        let projected = text_total_bytes
          .saturating_add(bytes.len())
          .saturating_add(separator);
        if projected > SUBTITLE_MAX_TEXT_TOTAL_BYTES {
          return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)));
        }
        if separator == 1 {
          text_chunks.push(b'\n');
        }
        text_chunks.extend_from_slice(bytes);
        text_total_bytes = projected;
      }
      x if x == ass_kind && !rect_ass_ptr.is_null() => {
        // SAFETY: `ass` is documented as 0-terminated UTF-8.
        // Same bounded-scan rationale as the TEXT branch above.
        let bytes = unsafe { bounded_cstr_bytes(rect_ass_ptr, SUBTITLE_MAX_TEXT_BYTES_PER_RECT) }
          .ok_or(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)))?;
        if bytes.len() > SUBTITLE_MAX_TEXT_BYTES_PER_RECT {
          return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)));
        }
        let separator = if text_chunks.is_empty() { 0 } else { 1 };
        let projected = text_total_bytes
          .saturating_add(bytes.len())
          .saturating_add(separator);
        if projected > SUBTITLE_MAX_TEXT_TOTAL_BYTES {
          return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)));
        }
        if separator == 1 {
          text_chunks.push(b'\n');
        }
        text_chunks.extend_from_slice(bytes);
        text_total_bytes = projected;
      }
      x if x == bitmap_kind => {
        // Bitmap region. data[0] = paletted indices, data[1] = RGBA
        // palette (256 entries × 4 bytes = 1024 bytes). Both are
        // owned by FFmpeg and not refcounted; copy into fresh buffers.
        let w = rect_w.max(0) as u32;
        let h = rect_h.max(0) as u32;
        let stride = rect_linesize0.max(0) as u32;
        if rect_data0_ptr.is_null() || stride == 0 || h == 0 {
          continue;
        }
        // `checked_mul` so a corrupt rect can't drive
        // `from_raw_parts` to an address-space-spanning length (UB
        // even before any deref).
        let data_len = (stride as usize)
          .checked_mul(h as usize)
          .ok_or(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)))?;
        // Per-rect bitmap byte cap (defends against a single
        // attacker rect larger than realistic DVB / PGS subtitles
        // by a wide margin).
        if data_len > SUBTITLE_MAX_BITMAP_BYTES_PER_RECT {
          return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)));
        }
        let projected_total = bitmap_total_bytes.saturating_add(data_len);
        if projected_total > SUBTITLE_MAX_BITMAP_TOTAL_BYTES {
          return Err(ConvertError::InvalidPlaneLayout(InvalidPlaneLayout::new(0)));
        }
        // SAFETY: data[0] is valid for `linesize[0] * h` bytes per
        // FFmpeg's contract; the multiplication is checked above.
        let data_slice = unsafe { core::slice::from_raw_parts(rect_data0_ptr, data_len) };
        // **A rect is copied on both lanes.** `AVSubtitleRect` has no
        // `buf[]`: its `data[]` are plain `av_malloc` allocations owned
        // by the `AVSubtitle`, which `avsubtitle_free` releases when
        // this call returns. There is no refcount to take, so the view
        // lane has nothing to view and says so.
        let data_buf = C::from_bytes(data_slice)
          .ok_or(ConvertError::CarrierAllocFailed(CarrierAllocFailed::new(0)))?;
        let palette_len = 256 * 4;
        let palette_buf = if rect_data1_ptr.is_null() {
          C::empty()
        } else {
          // SAFETY: palette buffer is 256*4 bytes per FFmpeg's contract.
          let p = unsafe { core::slice::from_raw_parts(rect_data1_ptr, palette_len) };
          C::from_bytes(p).ok_or(ConvertError::CarrierAllocFailed(CarrierAllocFailed::new(1)))?
        };
        bitmap_regions.push(mediadecode::subtitle::BitmapRegion::new(
          rect_x.max(0) as u32,
          rect_y.max(0) as u32,
          w,
          h,
          stride,
          data_buf,
          palette_buf,
        ));
        bitmap_total_bytes = projected_total;
      }
      _ => {}
    }
  }

  let payload = if !text_chunks.is_empty() {
    SubtitlePayload::Text(SubtitleText::new(
      C::from_bytes(&text_chunks)
        .ok_or(ConvertError::CarrierAllocFailed(CarrierAllocFailed::new(0)))?,
      None,
    ))
  } else if !bitmap_regions.is_empty() {
    SubtitlePayload::Bitmap(SubtitleBitmap::new(bitmap_regions))
  } else {
    // No rects (or only `None`-typed) — empty text payload.
    SubtitlePayload::Text(SubtitleText::new(C::empty(), None))
  };

  let sub_pts = unsafe { (*av_subtitle).pts };
  let pts = if sub_pts != AV_NOPTS_VALUE {
    Some(Timestamp::new(sub_pts, time_base))
  } else {
    None
  };

  let extra = SubtitleFrameExtra::new(unsafe { (*av_subtitle).start_display_time }, unsafe {
    (*av_subtitle).end_display_time
  });

  Ok(SubtitleFrame::new(payload, extra).with_pts(pts))
}

fn map_picture_type_raw(raw: i32) -> PictureType {
  match raw {
    x if x == AVPictureType::AV_PICTURE_TYPE_I as i32 => PictureType::I,
    x if x == AVPictureType::AV_PICTURE_TYPE_P as i32 => PictureType::P,
    x if x == AVPictureType::AV_PICTURE_TYPE_B as i32 => PictureType::B,
    x if x == AVPictureType::AV_PICTURE_TYPE_S as i32 => PictureType::S,
    x if x == AVPictureType::AV_PICTURE_TYPE_SI as i32 => PictureType::Si,
    x if x == AVPictureType::AV_PICTURE_TYPE_SP as i32 => PictureType::Sp,
    x if x == AVPictureType::AV_PICTURE_TYPE_BI as i32 => PictureType::Bi,
    _ => PictureType::Unspecified,
  }
}

#[cfg(test)]
mod tests;

/// [`av_frame_to_video_frame_as`] on the **view** lane.
///
/// # Safety
///
/// As the crate-private worker: a live source for the duration of the
/// call, and — on this lane — no handle capable of mutating its buffers
/// may outlive the returned carriers.
pub unsafe fn av_frame_to_video_frame(
  av_frame: *const AVFrame,
  time_base: Timebase,
  limits: FrameLimits,
) -> Result<VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, crate::FfmpegBuffer>, ConvertError>
{
  // SAFETY: forwarded verbatim; the caller's obligations are the
  // worker's.
  unsafe { av_frame_to_video_frame_as::<crate::View>(av_frame, time_base, limits) }
}

/// [`av_frame_to_video_frame`] on the **owned** lane, which copies every byte it
/// reads and therefore has no aliasing obligation.
///
/// # Safety
///
/// The source must be live for the duration of the call.
pub unsafe fn av_frame_to_owned_video_frame(
  av_frame: *const AVFrame,
  time_base: Timebase,
  limits: FrameLimits,
) -> Result<VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, FfmpegBytes>, ConvertError> {
  // SAFETY: forwarded verbatim.
  unsafe { av_frame_to_video_frame_as::<crate::Owned>(av_frame, time_base, limits) }
}

/// [`av_frame_to_image_frame_as`] on the **view** lane.
///
/// # Safety
///
/// As the crate-private worker: a live source for the duration of the
/// call, and — on this lane — no handle capable of mutating its buffers
/// may outlive the returned carriers.
pub unsafe fn av_frame_to_image_frame(
  av_frame: *const AVFrame,
  limits: FrameLimits,
) -> Result<ImageFrame<mediadecode::PixelFormat, ImageFrameExtra, crate::FfmpegBuffer>, ConvertError>
{
  // SAFETY: forwarded verbatim; the caller's obligations are the
  // worker's.
  unsafe { av_frame_to_image_frame_as::<crate::View>(av_frame, limits) }
}

/// [`av_frame_to_image_frame`] on the **owned** lane, which copies every byte it
/// reads and therefore has no aliasing obligation.
///
/// # Safety
///
/// The source must be live for the duration of the call.
pub unsafe fn av_frame_to_owned_image_frame(
  av_frame: *const AVFrame,
  limits: FrameLimits,
) -> Result<ImageFrame<mediadecode::PixelFormat, ImageFrameExtra, FfmpegBytes>, ConvertError> {
  // SAFETY: forwarded verbatim.
  unsafe { av_frame_to_image_frame_as::<crate::Owned>(av_frame, limits) }
}

/// [`av_frame_to_audio_frame_as`] on the **view** lane.
///
/// # Safety
///
/// As the crate-private worker: a live source for the duration of the
/// call, and — on this lane — no handle capable of mutating its buffers
/// may outlive the returned carriers.
pub unsafe fn av_frame_to_audio_frame(
  av_frame: *const AVFrame,
  time_base: Timebase,
  limits: FrameLimits,
) -> Result<
  AudioFrame<SampleFormat, ChannelLayoutDescription, AudioFrameExtra, crate::FfmpegBuffer>,
  ConvertError,
> {
  // SAFETY: forwarded verbatim; the caller's obligations are the
  // worker's.
  unsafe { av_frame_to_audio_frame_as::<crate::View>(av_frame, time_base, limits) }
}

/// [`av_frame_to_audio_frame`] on the **owned** lane, which copies every byte it
/// reads and therefore has no aliasing obligation.
///
/// # Safety
///
/// The source must be live for the duration of the call.
pub unsafe fn av_frame_to_owned_audio_frame(
  av_frame: *const AVFrame,
  time_base: Timebase,
  limits: FrameLimits,
) -> Result<
  AudioFrame<SampleFormat, ChannelLayoutDescription, AudioFrameExtra, FfmpegBytes>,
  ConvertError,
> {
  // SAFETY: forwarded verbatim.
  unsafe { av_frame_to_audio_frame_as::<crate::Owned>(av_frame, time_base, limits) }
}

/// [`av_subtitle_to_subtitle_frame_as`] on the **view** lane.
///
/// # Safety
///
/// As the crate-private worker: a live source for the duration of the
/// call, and — on this lane — no handle capable of mutating its buffers
/// may outlive the returned carriers.
pub unsafe fn av_subtitle_to_subtitle_frame(
  av_subtitle: *const ffmpeg_next::ffi::AVSubtitle,
  time_base: Timebase,
) -> Result<SubtitleFrame<SubtitleFrameExtra, crate::FfmpegBuffer>, ConvertError> {
  // SAFETY: forwarded verbatim; the caller's obligations are the
  // worker's.
  unsafe { av_subtitle_to_subtitle_frame_as::<crate::View>(av_subtitle, time_base) }
}

/// [`av_subtitle_to_subtitle_frame`] on the **owned** lane, which copies every byte it
/// reads and therefore has no aliasing obligation.
///
/// # Safety
///
/// The source must be live for the duration of the call.
pub unsafe fn av_subtitle_to_owned_subtitle_frame(
  av_subtitle: *const ffmpeg_next::ffi::AVSubtitle,
  time_base: Timebase,
) -> Result<SubtitleFrame<SubtitleFrameExtra, FfmpegBytes>, ConvertError> {
  // SAFETY: forwarded verbatim.
  unsafe { av_subtitle_to_subtitle_frame_as::<crate::Owned>(av_subtitle, time_base) }
}

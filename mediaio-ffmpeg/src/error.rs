//! The crossing's own errors.
//!
//! Two families, both reported by this crate and carried unchanged by
//! the machines built on it:
//!
//! * [`ParametersError`] — what the parameter copier, the codec ticket
//!   and the track row refuse about a stream's `AVCodecParameters`. The
//!   demuxer in `mediadecode-ffmpeg` reports each arm under its own
//!   `DemuxError` arm with the same payload, and its decoder error
//!   carries the three a decoder can meet.
//! * [`CrossingError`] — an FFmpeg call at the crossing that answered
//!   with an error, most often an allocation it refused.

use derive_more::{IsVariant, TryUnwrap, Unwrap};

use crate::channel_layout::ChannelLayoutFault;

/// Why a stream's codec parameters could not be mirrored, copied or
/// rebuilt.
///
/// What [`CodecTicket::mirror`](crate::ticket::CodecTicket::mirror),
/// [`CodecTicket::rebuild`](crate::ticket::CodecTicket::rebuild) and
/// [`TrackExtra::clone_parameters`](crate::extras::TrackExtra::clone_parameters)
/// answer when they refuse.
///
/// **Closed, so it is exhaustive.** These arms are the whole account of
/// a parameter copy at the crossing, and the demuxer's error names every
/// one of them under the same payload — `mediadecode-ffmpeg`'s
/// `From<ParametersError> for DemuxError` matches them exhaustively — so
/// an arm cannot be added here without the demuxer naming it too.
#[derive(thiserror::Error, Debug, Clone, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
pub enum ParametersError {
  /// One stream's codec parameters hold more heap bytes than the budget
  /// allows. See [`ParametersTooLarge`].
  #[error(transparent)]
  TooLarge(#[from] ParametersTooLarge),
  /// The parameters arrived never allocated. See [`ParametersMissing`].
  #[error(transparent)]
  Missing(#[from] ParametersMissing),
  /// The parameters could not be allocated. See [`ParametersAlloc`].
  #[error(transparent)]
  Alloc(#[from] ParametersAlloc),
  /// Copying the parameters failed part way. See [`ParametersCopy`].
  #[error(transparent)]
  Copy(#[from] ParametersCopy),
  /// A channel layout carried user-private data an owned ticket cannot
  /// mirror. See [`ParametersOpaque`].
  #[error(transparent)]
  Opaque(#[from] ParametersOpaque),
  /// A custom channel layout declared channels it has no map for. See
  /// [`ParametersChannelMap`].
  #[error(transparent)]
  ChannelMap(#[from] ParametersChannelMap),
  /// A channel layout declared a shape FFmpeg's own helpers cannot be
  /// given. See [`ParametersLayoutShape`].
  #[error(transparent)]
  LayoutShape(#[from] ParametersLayoutShape),
}

impl ParametersError {
  /// Names a channel-layout fault for `stream_index` — **one mapper, so
  /// the roads that share the preflight also share its report**.
  ///
  /// `Alloc` is the only arm that is not a statement about the container:
  /// it is the allocator declining a rendering buffer, and it keeps the
  /// name every other allocator refusal on this road has.
  pub fn from_layout_fault(stream_index: usize, fault: ChannelLayoutFault) -> Self {
    use ChannelLayoutFault as Fault;
    match fault {
      // The second cannot arrive from a pointer road — it is how the
      // *safe* conversion refuses a custom layout whose extent it cannot
      // establish — but both say the same thing about this stream.
      Fault::MalformedCustomMap { channels } | Fault::UnverifiableCustomMap { channels } => {
        Self::ChannelMap(ParametersChannelMap::new(stream_index, channels))
      }
      Fault::MalformedLayout { order, channels } => {
        Self::LayoutShape(ParametersLayoutShape::new(stream_index, order, channels))
      }
      Fault::Alloc => Self::Alloc(ParametersAlloc::new(stream_index)),
    }
  }
}

/// Payload for [`ParametersError::TooLarge`] — and for the demuxer's
/// `DemuxError::ParametersTooLarge`, which carries the same value.
///
/// One stream's codec parameters hold more heap bytes than
/// [`DemuxLimits::max_codec_parameter_bytes`](crate::limits::DemuxLimits::max_codec_parameter_bytes) allows.
///
/// The bytes are `extradata` plus every `coded_side_data` entry plus a
/// custom channel map — the three seats `AVCodecParameters` reaches the
/// heap through. A MOV `prof` atom lands in the second of those as an
/// ICC profile, which is where the honest large values live and where
/// the forged ones do too.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error(
  "the codec parameters on stream {stream_index} hold {bytes} heap bytes, over the {limit}-byte budget"
)]
pub struct ParametersTooLarge {
  stream_index: usize,
  bytes: usize,
  limit: usize,
}

impl ParametersTooLarge {
  /// Constructs a `ParametersTooLarge` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, bytes: usize, limit: usize) -> Self {
    Self {
      stream_index,
      bytes,
      limit,
    }
  }
  /// The `AVStream.index` whose parameters were refused.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The heap bytes the parameters declared.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The budget in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`ParametersError::Missing`] — and for the demuxer's
/// `DemuxError::ParametersMissing`.
///
/// Codec parameters arrived that were never allocated.
///
/// `ffmpeg_next::codec::Parameters` has safe constructors that hand
/// back a null-backed value when FFmpeg's allocation failed, and they
/// report nothing. Copying from one dereferences null, so it is
/// refused where it arrives — at construction, and again in the
/// copier — rather than crashing later somewhere that has forgotten
/// the allocator ever failed.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the codec parameters for stream {stream_index} were never allocated")]
pub struct ParametersMissing {
  stream_index: usize,
}

impl ParametersMissing {
  /// Constructs a `ParametersMissing` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize) -> Self {
    Self { stream_index }
  }
  /// The `AVStream.index` the parameters were offered for.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
}

/// Payload for [`ParametersError::Alloc`] — and for the demuxer's
/// `DemuxError::ParametersAlloc`.
///
/// Codec parameters for a track could not be allocated.
#[derive(thiserror::Error, Debug, Clone)]
#[error("out of memory allocating the codec parameters for stream {stream_index}")]
pub struct ParametersAlloc {
  stream_index: usize,
}

impl ParametersAlloc {
  /// Constructs a `ParametersAlloc` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize) -> Self {
    Self { stream_index }
  }
  /// The `AVStream.index` whose parameters could not be copied.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
}

/// Payload for [`ParametersError::Copy`] — and for the demuxer's
/// `DemuxError::ParametersCopy`.
///
/// Copying a track's codec parameters failed part way.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the codec parameters for stream {stream_index} could not be copied: {source}")]
pub struct ParametersCopy {
  stream_index: usize,
  #[source]
  source: ffmpeg_next::Error,
}

impl ParametersCopy {
  /// Constructs a `ParametersCopy` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, source: ffmpeg_next::Error) -> Self {
    Self {
      stream_index,
      source,
    }
  }
  /// The `AVStream.index` whose parameters could not be copied.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// What FFmpeg said.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn source(&self) -> &ffmpeg_next::Error {
    &self.source
  }
}

/// Payload for [`ParametersError::Opaque`] — and for the demuxer's
/// `DemuxError::ParametersOpaque`.
///
/// A channel layout arrived carrying `opaque` — a raw pointer FFmpeg
/// documents as "private data of the user".
///
/// [`CodecTicket`](crate::ticket::CodecTicket) is an **owned** mirror:
/// it outlives the `AVCodecParameters` it was read from, and it may
/// cross threads, so a pointer into somebody else's data is exactly
/// what it cannot carry. libavformat sets neither
/// `AVChannelLayout::opaque` nor `AVChannelCustom::opaque`, so no
/// demuxed stream reaches the mirror with one; if one ever does, the
/// mirror refuses rather than dropping the pointer in silence. That is
/// the same fail-closed answer `extras::measure_parameters` gives a
/// channel order it has never heard of, and for the same reason:
/// carrying on would be a guess about memory nobody here owns.
#[derive(thiserror::Error, Debug, Clone)]
#[error(
  "the channel layout for stream {stream_index} carries user-private data \
   ({}) that an owned codec ticket cannot mirror",
  match channel { Some(i) => format!("custom channel {i}"), None => "the layout".to_owned() },
)]
pub struct ParametersOpaque {
  stream_index: usize,
  channel: Option<usize>,
}

impl ParametersOpaque {
  /// Constructs a `ParametersOpaque` payload. `channel` names the
  /// custom-map entry when the pointer was on one, and is `None` when
  /// it was on the layout itself.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, channel: Option<usize>) -> Self {
    Self {
      stream_index,
      channel,
    }
  }
  /// The `AVStream.index` whose layout carried the pointer.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The custom-map entry the pointer was on, or `None` when it was on
  /// the layout itself.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn channel(&self) -> Option<usize> {
    self.channel
  }
}

/// Payload for [`ParametersError::ChannelMap`] — and for the demuxer's
/// `DemuxError::ParametersChannelMap`.
///
/// A channel layout declared `AV_CHANNEL_ORDER_CUSTOM` without the map
/// that order requires.
///
/// **This one is a crash, not a curiosity.** For a custom order,
/// `av_channel_layout_copy` — which is how
/// `avcodec_parameters_to_context` moves a layout into a decoder's
/// context — does
///
/// ```c
/// dst->u.map = av_malloc_array(src->nb_channels, sizeof(*dst->u.map));
/// if (!dst->u.map)
///     return AVERROR(ENOMEM);
/// memcpy(dst->u.map, src->u.map, src->nb_channels * sizeof(*src->u.map));
/// ```
///
/// with **no null check on `src->u.map`** — verified against FFmpeg
/// n9.0. A layout that names channels it has no map for therefore makes
/// libavcodec `memcpy` from a null pointer the moment a decoder opens
/// from it.
///
/// So the mirror refuses such a layout at the door rather than
/// reproducing it. An earlier draft carried it through, on the argument
/// that a malformed layout in should be a malformed layout out — the
/// round trip is faithful either way, and the parity comparator agreed.
/// That symmetry was the wrong test: faithfully reproducing a shape
/// whose only consumer dereferences null is not fidelity, it is
/// forwarding a crash. Refusing is the same fail-closed answer
/// `extras::measure_parameters` gives a channel order it has never
/// heard of.
#[derive(thiserror::Error, Debug, Clone)]
#[error(
  "the custom channel layout for stream {stream_index} declares {channels} channels \
   and carries no usable map for them"
)]
pub struct ParametersChannelMap {
  stream_index: usize,
  channels: i32,
}

impl ParametersChannelMap {
  /// Constructs a `ParametersChannelMap` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, channels: i32) -> Self {
    Self {
      stream_index,
      channels,
    }
  }
  /// The `AVStream.index` whose layout was malformed.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// The `nb_channels` the layout declared with no map to describe them.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn channels(&self) -> i32 {
    self.channels
  }
}

/// Payload for [`ParametersError::LayoutShape`] — and for the
/// demuxer's `DemuxError::ParametersLayoutShape`.
///
/// The non-custom half of what a channel layout can be wrong about, and
/// the half that went unchecked for eleven rounds of review on the
/// argument that an order describing its channels through a `uint64_t`
/// mask cannot be malformed. The mask is not the only field:
/// `nb_channels` is an `int` a caller writes, and FFmpeg's helpers
/// compute `nb_channels - popcount(mask)` and take an integer square
/// root of it without checking either.
///
/// See
/// [`layout_preflight`](crate::channel_layout::layout_preflight) for
/// the complete rule and why it is one function.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error(
  "the channel layout for stream {stream_index} declares order {order} with {channels} \
   channels, which is not a shape FFmpeg's own helpers can be given"
)]
pub struct ParametersLayoutShape {
  stream_index: usize,
  order: i32,
  channels: i32,
}

impl ParametersLayoutShape {
  /// Constructs a `ParametersLayoutShape` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(stream_index: usize, order: i32, channels: i32) -> Self {
    Self {
      stream_index,
      order,
      channels,
    }
  }
  /// The `AVStream.index` whose layout declared it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn stream_index(&self) -> usize {
    self.stream_index
  }
  /// `AVChannelLayout.order`, as the raw `c_int` it is on the wire.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn order(&self) -> i32 {
    self.order
  }
  /// `nb_channels`, as the layout declared it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn channels(&self) -> i32 {
    self.channels
  }
}

/// A failure at the FFmpeg crossing that belongs to no decoder and no
/// demuxer: an FFmpeg call that answered with an error — an allocation
/// it refused, most often.
///
/// What [`Frame::empty`](crate::frame::Frame::empty), the checked frame
/// allocators and the resampler's FFmpeg calls report. The decoder error
/// in `mediadecode-ffmpeg` converts it into its own `Ffmpeg` arm, so a
/// caller of the decoders meets the error it always met.
///
/// **Closed, so it is exhaustive**, for the reason [`ParametersError`]
/// is: the decoder error's conversion names every arm.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
pub enum CrossingError {
  /// An FFmpeg call reported an error.
  #[error("ffmpeg error: {0}")]
  Ffmpeg(#[from] ffmpeg_next::Error),
}

use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::Packet;

use crate::backend::Backend;

/// Crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned from [`crate::VideoDecoder`].
///
/// `Debug` is derived; the variants that wrap a payload struct
/// (`HwDeviceInitFailed`, `AllBackendsFailed`, `FallbackFailed`)
/// delegate their `Debug` to the payload, which is hand-written
/// where needed because [`ffmpeg_next::Packet`] (carried by
/// `AllBackendsFailed::unconsumed_packets` /
/// `FallbackFailed::unconsumed_packets`) does not derive
/// `Debug`. Those payloads summarize the packet count rather
/// than dumping each packet's fields, which would be both noisy
/// and useless for triage.
///
/// **Open fault taxonomy, so it is `#[non_exhaustive]`.** New ways to
/// fail are discovered — a backend, a ceiling, a corruption a codec
/// learns to report — and a consumer that meets one it has never heard
/// of should take its generic-fault path. That is exactly what the
/// wildcard arm this attribute forces is for. The two status
/// vocabularies opposite it,
/// [`Sent`](mediadecode::Sent) and [`Received`](mediadecode::Received),
/// are exhaustive for the mirror-image reason: their arms are the
/// substrate's fixed state set, and there the wildcard would be dead
/// weight hiding a state a consumer forgot.
#[derive(Debug, thiserror::Error, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
#[non_exhaustive]
pub enum Error {
  /// An underlying FFmpeg error.
  #[error("ffmpeg error: {0}")]
  Ffmpeg(#[from] ffmpeg_next::Error),

  /// A portable packet could not be rebuilt as an `AVPacket` on its
  /// way into a decoder — see [`crate::boundary::PacketBuildError`].
  #[error(transparent)]
  PacketBuild(#[from] crate::boundary::PacketBuildError),

  /// A stream's codec parameters hold more heap bytes than the
  /// decoder tier will copy — see
  /// [`crate::DEFAULT_MAX_CODEC_PARAMETER_BYTES`].
  ///
  /// The decoder tier has no options object of its own for this, so it
  /// applies the default ceiling. A caller that needs a larger one
  /// opens the parameters through the demux tier, where
  /// [`DemuxLimits`](crate::DemuxLimits) carries the seat.
  #[error(transparent)]
  ParametersTooLarge(#[from] crate::demuxer::ParametersTooLarge),

  /// A stream's channel layout is not a shape FFmpeg's own helpers can
  /// be given, so the decoder was not opened over it.
  ///
  /// **Structural and permanent, never an allocation failure.**
  /// `avcodec_parameters_to_context` reaches `av_channel_layout_copy`,
  /// whose `memcpy` reads the custom map with no null check of its own,
  /// and FFmpeg's describe and compare helpers compute
  /// `nb_channels - popcount(mask)` — and take an integer square root
  /// of it — without checking either. None of that is a shortage of
  /// memory and none of it will be different on the next attempt, which
  /// is why it does not share the allocation arm.
  ///
  /// See
  /// [`layout_preflight`](crate::channel_layout::layout_preflight) for
  /// the rule, which the demux admission pass and the outbound clone
  /// apply too.
  #[error(transparent)]
  MalformedChannelLayout(#[from] crate::demuxer::ParametersLayoutShape),

  /// A stream's channel layout declares a custom order without the map
  /// that order requires — the shape `av_channel_layout_copy` would
  /// `memcpy` from null. Structural and permanent, as above.
  #[error(transparent)]
  ChannelMapMissing(#[from] crate::demuxer::ParametersChannelMap),

  /// `avcodec_find_decoder` returned null for the input codec id. The id
  /// is reported as the raw integer (`AVCodecID` discriminant) — we do not
  /// construct the bindgen `AVCodecID` enum from a runtime value, since
  /// values outside our build's discriminant set would invoke UB.
  #[error("no decoder for codec id {0}")]
  NoCodec(u32),

  /// The CPU frame a hardware->CPU transfer would allocate is larger
  /// than [`FrameLimits::max_frame_bytes`](crate::FrameLimits::max_frame_bytes).
  ///
  /// The hardware road's own seat. `judge_buffer` — the allocator hook
  /// that applies the byte ceiling to aligned dimensions — is **not** a
  /// universal choke point: `ff_get_buffer` calls `hwaccel->alloc_frame`
  /// directly for VideoToolbox h264/hevc/vp9 and never reaches
  /// `get_buffer2` at all, and `av_hwframe_transfer_data` allocates its
  /// CPU destination outside both. This is the seat for that second
  /// road, judged before the transfer rather than after it.
  #[error(transparent)]
  HwTransferTooLarge(#[from] HwTransferTooLarge),

  /// A frame's allocation would have cost more than
  /// [`FrameLimits::max_frame_bytes`](crate::FrameLimits::max_frame_bytes),
  /// so it was refused in the allocator, before the allocation.
  #[error(transparent)]
  FrameBudgetExceeded(#[from] FrameBudgetExceeded),

  /// The stream's **coded** surface is over the frame ceiling, so the
  /// hardware format was declined before its pool could be built.
  ///
  /// The two dimension vocabularies: `max_pixels` is applied by
  /// `ff_set_dimensions` to a stream's *display* dims, and a cropped
  /// stream can display 32x32 out of a 1920x1088 coded surface. What
  /// gets allocated is the coded figure, so it is the one judged here —
  /// and it is judged in **bytes**, priced through the allocator-parity
  /// footprint against the caller's `max_frame_bytes`, because
  /// `max_pixels` carries the caller's logical pixel limit and nothing
  /// about cost.
  #[error(transparent)]
  HwSurfaceTooLarge(#[from] HwSurfaceTooLarge),

  /// The codec does not advertise a hardware configuration matching the
  /// requested backend (via `avcodec_get_hw_config`).
  #[error("codec does not support backend {0:?}")]
  BackendUnsupportedByCodec(Backend),

  /// `av_hwdevice_ctx_create` failed for the requested backend. See
  /// [`HwDeviceInitFailed`] for the payload details. `#[from]` gives
  /// a free `impl From<HwDeviceInitFailed> for Error`, so inner
  /// helpers that return `Result<_, HwDeviceInitFailed>` can be
  /// `?`-propagated into `Error` directly.
  #[error(transparent)]
  HwDeviceInitFailed(#[from] HwDeviceInitFailed),

  /// Auto-probe exhausted every backend in the platform's order. See
  /// [`AllBackendsFailed`] for the payload details (in particular the
  /// `unconsumed_packets` history that callers should replay through
  /// their own software decoder for non-seekable inputs). `#[from]`
  /// gives a free `impl From<AllBackendsFailed> for Error`.
  #[error(transparent)]
  AllBackendsFailed(#[from] AllBackendsFailed),

  /// Surfaced by [`crate::FfmpegVideoStreamDecoder`] when a HW->SW
  /// fallback attempt itself fails. See [`FallbackFailed`] for the
  /// payload details (in particular the rescued `unconsumed_packets`
  /// the HW path had already consumed from the caller). `#[from]`
  /// gives a free `impl From<FallbackFailed> for Error`.
  #[error(transparent)]
  FallbackFailed(#[from] FallbackFailed),

  /// A decoded picture alone exceeds the byte budget of the software video
  /// road's queue of pictures waiting for delivery
  /// ([`DecoderLimits::max_replay_bytes`](crate::DecoderLimits::max_replay_bytes))
  /// — a picture a fallback replay decoded, or one of the tail a one-thread
  /// decoder is drained of where the session restarts it at a keyframe. No
  /// drain can make room for it, so it is refused by name; see
  /// [`ReplayQueueFull`].
  #[error(transparent)]
  ReplayQueueFull(#[from] ReplayQueueFull),

  /// A software video decoder opened set to output pictures before their
  /// recovery — `AV_CODEC_FLAG_OUTPUT_CORRUPT` or `AV_CODEC_FLAG2_SHOW_ALL`
  /// set on its codec context after the open — is refused by name; see
  /// [`UnrecoveredOutput`].
  #[error(transparent)]
  UnrecoveredOutput(#[from] UnrecoveredOutput),

  /// A decoded picture holds an allocation the software video road's queue
  /// of pictures waiting for delivery cannot price — of no stated size, or
  /// owned by no buffer reference — so it is refused by name rather than
  /// queued as costing nothing; see [`UnpricedFrame`].
  #[error(transparent)]
  UnpricedFrame(#[from] UnpricedFrame),

  /// A post-commit fallback's software decoder wraps another implementation
  /// of the codec — a hardware or OS framework, or an external library —
  /// rather than being one of libavcodec's own, so the resync the fallback
  /// owes could never be proved; the fallback is refused by name at the
  /// open it would commit; see [`ResyncUnprovable`].
  #[error(transparent)]
  ResyncUnprovable(#[from] ResyncUnprovable),

  /// A software video decoder the session would open on its codec
  /// parameters while their extradata is unknown — a packet carrying a new
  /// extradata was refused with an error that does not say whether the
  /// decoder took it, a decode error among them — is refused by name; see
  /// [`ExtradataUnknown`].
  #[error(transparent)]
  ExtradataUnknown(#[from] ExtradataUnknown),

  /// A packet's `AV_PKT_DATA_NEW_EXTRADATA` is a record FFmpeg's decoder
  /// would reject, or apply only in part, without saying so — so the packet
  /// was refused before any decoder saw it, still the caller's; see
  /// [`ExtradataRejected`].
  #[error(transparent)]
  ExtradataRejected(#[from] ExtradataRejected),
}

/// Payload for [`Error::ExtradataRejected`].
///
/// A packet carrying `AV_PKT_DATA_NEW_EXTRADATA` changes a stream's codec
/// parameters from that packet on. FFmpeg's H.264 decoder applies the
/// record as it begins to decode the packet and drops what
/// `ff_h264_decode_extradata` answers (`h264_decode_frame`, FFmpeg 9's
/// h264dec.c): a record it rejects — an `avcC` record shorter than seven
/// bytes, one whose parameter set runs past its end — leaves the decoder on
/// its old NAL length size and parameter sets, and a parameter set it
/// cannot parse is skipped while the rest of the record applies. A session
/// that took such a record as the stream's would read every later packet,
/// and open every later decoder, on parameters the decoder serving never
/// adopted. So the packet is refused before any decoder sees it: nothing of
/// the session changes, and the packet is still the caller's — to send
/// again without the record, or to drop.
///
/// The record is read as FFmpeg 9 reads it, its bit reader and parameter
/// set parsers mirrored; a record whose verdict depends on what the decoder
/// holds already — a picture parameter set referring to a sequence
/// parameter set the record does not carry — is refused too, since this
/// crate does not read that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
  "a packet's new extradata for {codec:?} was refused before any decoder saw it: {reason}; the \
   decoder would have kept parameters other than the record's"
)]
pub struct ExtradataRejected {
  codec: crate::CodecId,
  reason: ExtradataRejection,
}

impl ExtradataRejected {
  /// Constructs an [`ExtradataRejected`] payload.
  #[inline]
  pub const fn new(codec: crate::CodecId, reason: ExtradataRejection) -> Self {
    Self { codec, reason }
  }
  /// The stream's codec.
  #[inline]
  pub const fn codec(&self) -> crate::CodecId {
    self.codec
  }
  /// What FFmpeg would have made of the record.
  #[inline]
  pub const fn reason(&self) -> ExtradataRejection {
    self.reason
  }
}

/// Why a packet's new extradata was refused ([`ExtradataRejected`]): what
/// FFmpeg's decoder would have made of the record instead of applying it
/// whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtradataRejection {
  /// An `avcC` record shorter than the seven bytes FFmpeg reads before its
  /// first parameter set: rejected whole.
  TooShort {
    /// The record's length.
    size: usize,
  },
  /// A parameter set whose length runs past the record: FFmpeg rejects the
  /// record there, the sets before it applied and its NAL length size not.
  Overrun(ParameterSet),
  /// A parameter set FFmpeg fails to parse, read every way it reads one:
  /// skipped, the decoder keeping the set it had of that id, while the rest
  /// of the record applies.
  Unparsed(ParameterSet),
  /// A parameter set FFmpeg fails to parse that is too large for the
  /// escaping retry it gives an `avcC` entry: the record rejected there.
  Oversized(ParameterSet),
  /// A picture parameter set referring to a sequence parameter set the
  /// record does not carry: whether FFmpeg stores it depends on what the
  /// decoder holds already.
  Unresolved,
}

impl core::fmt::Display for ExtradataRejection {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::TooShort { size } => write!(
        f,
        "an avcC record of {size} bytes, under the seven FFmpeg reads"
      ),
      Self::Overrun(set) => write!(f, "a {set} whose length runs past the record"),
      Self::Unparsed(set) => write!(f, "a {set} FFmpeg fails to parse, and would skip"),
      Self::Oversized(set) => write!(
        f,
        "a {set} FFmpeg fails to parse, too large for its escaping retry"
      ),
      Self::Unresolved => f.write_str(
        "a picture parameter set referring to a sequence parameter set the record does not carry",
      ),
    }
  }
}

/// A kind of parameter set a codec's extradata carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParameterSet {
  /// A sequence parameter set.
  Sequence,
  /// A picture parameter set.
  Picture,
}

impl core::fmt::Display for ParameterSet {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    f.write_str(match self {
      Self::Sequence => "sequence parameter set",
      Self::Picture => "picture parameter set",
    })
  }
}

/// Payload for [`Error::ExtradataUnknown`].
///
/// A packet carrying `AV_PKT_DATA_NEW_EXTRADATA` changes a stream's codec
/// parameters from that packet on, and FFmpeg's H.264 and HEVC decoders
/// apply it as they begin to decode the packet. The session takes the new
/// extradata for its own once the decoder takes the packet, and holds it
/// provisionally until the decoder is seen to have read the packet — it
/// answers "needs input" or the end, or, decoding a packet inside the
/// submission that hands it over, takes a later one: until then the packet
/// may still wait in libavcodec's input slot, unread. The extradata is
/// unknown when that cannot be told any more ([`ExtradataDoubt`]): a
/// decoder refuses the packet with an error that does not say whether it
/// got that far — any error libavcodec reports, invalid data among them —
/// or reports one before it was seen to read it, a flush drops the packet
/// unread, or the hardware fails and no fallback replaces it. A corrupt
/// packet at an extradata change leaves it unknown, then, until a packet
/// carrying a new one is taken: until then the session reads no H.264 or
/// HEVC resync anchor or switch point under its own, switches to no new
/// decoder, and refuses by this name to open a decoder on it — a
/// post-commit fallback's cold decoder, a reopen — rather than decode on
/// parameters that may be stale. A packet carrying its own new extradata is
/// read, and opened on, under that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
  "the stream's codec extradata is unknown: {doubt}; no decoder is opened on extradata that may \
   be stale"
)]
pub struct ExtradataUnknown {
  doubt: ExtradataDoubt,
}

impl ExtradataUnknown {
  /// Constructs an [`ExtradataUnknown`] payload.
  #[inline]
  pub const fn new(doubt: ExtradataDoubt) -> Self {
    Self { doubt }
  }
  /// What left the extradata unknown.
  #[inline]
  pub const fn doubt(&self) -> ExtradataDoubt {
    self.doubt
  }
}

/// What left a stream's codec extradata unknown ([`ExtradataUnknown`]):
/// each says a decoder may, or may not, have applied a packet's new
/// extradata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtradataDoubt {
  /// libavcodec reported this error before the decoder was seen to read the
  /// packet carrying the new extradata — refusing that packet, or later —
  /// and nothing ties an error to a packet: it can come from before the
  /// decode, which then drops the packet, or from an earlier packet.
  Reported(ffmpeg_next::Error),
  /// A refusal this crate made itself — a frame or a coded surface over its
  /// ceiling, among others — before the decoder was seen to read the packet
  /// carrying the new extradata, the error libavcodec reported with it not
  /// kept.
  Minted,
  /// A flush dropped what the decoder had not yet been seen to read, the
  /// packet carrying the new extradata among it.
  Flushed,
  /// The hardware decoder failed post-commit before it was seen to read the
  /// packet carrying the new extradata — on that packet, or later — and the
  /// software fallback that would have replaced it did not commit.
  HardwareFailed,
}

impl core::fmt::Display for ExtradataDoubt {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::Reported(error) => write!(
        f,
        "the decoder reported \"{error}\" before it was seen to read a packet carrying a new \
         extradata"
      ),
      Self::Minted => f.write_str(
        "this crate refused a picture before the decoder was seen to read a packet carrying a \
         new extradata",
      ),
      Self::Flushed => f.write_str(
        "a flush dropped a packet carrying a new extradata before the decoder was seen to read it",
      ),
      Self::HardwareFailed => f.write_str(
        "the hardware failed before it was seen to read a packet carrying a new extradata, and \
         the software fallback did not commit",
      ),
    }
  }
}

/// Payload for [`Error::ResyncUnprovable`].
///
/// A post-commit fallback opens a software decoder cold, mid-stream, and
/// drops the pictures up to the next random-access point; the session then
/// owes a proof that the pictures it delivers past that point come from
/// after it. Both proofs it has are invariants of libavcodec's own decoders:
/// FFmpeg's `h264` withholds every picture it has not recovered, and
/// libavcodec's decoders publish how many pictures their reorder buffer can
/// hold back (`has_b_frames`), holding none past that once they answer
/// "needs input". A decoder that wraps another implementation —
/// `h264_cuvid`, `h264_qsv`, `h264_v4l2m2m`, `h264_mediacodec`, `libdav1d`,
/// `libvpx-vp9`, … (`AVCodec.wrapper_name` set) — publishes no reorder bound
/// (`h264_cuvid` keeps a display delay of several pictures and leaves
/// `has_b_frames` at zero) and withholds nothing, and its "needs input" does
/// not say its pipeline is empty, so a picture from before the gap could
/// close it. The fallback is refused at the open it would commit: that
/// decoder is closed, nothing is committed, and the session stays where it
/// was.
///
/// Only this road needs a proof. A session opened on software from the
/// start, and a probe-era fallback, which replays the whole history, decode
/// on a wrapped implementation as on any other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
  "a post-commit fallback was refused: the software decoder for {codec:?}, {implementation}, \
   wraps {wrapper}, which publishes no reorder bound and withholds no unrecovered picture, so \
   the resync after the gap could not be proved",
  implementation = name_or_unread(.implementation),
  wrapper = name_or_unread(.wrapper),
)]
pub struct ResyncUnprovable {
  codec: crate::CodecId,
  implementation: Option<&'static str>,
  wrapper: Option<&'static str>,
}

impl ResyncUnprovable {
  /// Constructs a [`ResyncUnprovable`] payload.
  #[inline]
  pub const fn new(
    codec: crate::CodecId,
    implementation: Option<&'static str>,
    wrapper: Option<&'static str>,
  ) -> Self {
    Self {
      codec,
      implementation,
      wrapper,
    }
  }
  /// The stream's codec.
  #[inline]
  pub const fn codec(&self) -> crate::CodecId {
    self.codec
  }
  /// The software decoder that was opened and refused, by FFmpeg's name for
  /// it (`AVCodec.name`: `h264_cuvid`, `libdav1d`), or `None` where that
  /// name does not read as FFmpeg's ASCII.
  #[inline]
  pub const fn implementation(&self) -> Option<&'static str> {
    self.implementation
  }
  /// What it wraps (`AVCodec.wrapper_name`: `cuvid`, `libdav1d`), or `None`
  /// where that name does not read as FFmpeg's ASCII.
  #[inline]
  pub const fn wrapper(&self) -> Option<&'static str> {
    self.wrapper
  }
}

/// A name FFmpeg's tables gave, for a message — or what to say where it did
/// not read.
fn name_or_unread(name: &Option<&'static str>) -> &'static str {
  name.unwrap_or("(a name that does not read)")
}

/// What a decoded picture holds that the replay queue's budget cannot price
/// ([`UnpricedFrame`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnpricedHolding {
  /// `AVFrame.private_ref`: a reference libavcodec keeps for itself, of no
  /// stated size, which it clears before a frame leaves a decoder.
  PrivateRef,
  /// A side data entry whose bytes no buffer reference owns, or a side data
  /// table that does not hold its entries.
  SideData,
  /// More side data entries than a picture the queue takes may carry. A
  /// decoder makes one for each message it reads — FFmpeg's H.264 decoder
  /// one per unregistered SEI message, with no cap of its own — and the
  /// queue prices each alone, so it walks no table past the cap.
  SideDataEntries {
    /// The entries the picture carries.
    count: usize,
    /// The most a picture the queue takes may carry.
    cap: usize,
  },
}

impl core::fmt::Display for UnpricedHolding {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::PrivateRef => f.write_str("a private reference"),
      Self::SideData => f.write_str("side data no buffer reference owns"),
      Self::SideDataEntries { count, cap } => write!(
        f,
        "{count} side data entries, more than the {cap} a queued picture may carry"
      ),
    }
  }
}

/// Payload for [`Error::UnpricedFrame`].
///
/// The software video road's queue prices a decoded picture by every
/// allocation it owns — its pixel buffers, the side data table and every
/// entry in it, each entry's buffer, its metadata and the side data's,
/// `opaque_ref` and `hw_frames_ctx`, each at its payload rounded to the
/// allocator's alignment and the allocator's and the buffer's own overhead
/// — against [`DecoderLimits::max_replay_bytes`](crate::DecoderLimits::max_replay_bytes).
/// A picture holding an allocation whose size it cannot read, or more side
/// data entries than the queue prices, is never admitted as costing nothing:
/// it is refused by this name, naming what it holds, and released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
  "a decoded picture holds an allocation the software video decoder's replay budget cannot \
   price: {holding}"
)]
pub struct UnpricedFrame {
  holding: UnpricedHolding,
}

impl UnpricedFrame {
  /// Constructs an [`UnpricedFrame`] payload.
  #[inline]
  pub const fn new(holding: UnpricedHolding) -> Self {
    Self { holding }
  }
  /// What the picture holds that cannot be priced.
  #[inline]
  pub const fn holding(&self) -> UnpricedHolding {
    self.holding
  }
}

/// Payload for [`Error::UnrecoveredOutput`].
///
/// The session clears both flags on every software video decoder's codec
/// context before the open, and checks them after it, in every build. A
/// decoder found with either set would output pictures it has not
/// recovered — FFmpeg's H.264 decoder conceals the pictures it decodes from
/// references it never saw and hands them out — and the proof that a
/// post-commit resync happened on an H.264 stream is that FFmpeg withholds
/// every such picture. The decoder is closed and the open refused, with the
/// flags it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
  "a software video decoder opened set to output pictures before their recovery \
   (AV_CODEC_FLAG_OUTPUT_CORRUPT: {output_corrupt}, AV_CODEC_FLAG2_SHOW_ALL: {show_all}); \
   the post-commit resync of an H.264 stream is proved by FFmpeg withholding them"
)]
pub struct UnrecoveredOutput {
  output_corrupt: bool,
  show_all: bool,
}

impl UnrecoveredOutput {
  /// Constructs an [`UnrecoveredOutput`] payload.
  #[inline]
  pub const fn new(output_corrupt: bool, show_all: bool) -> Self {
    Self {
      output_corrupt,
      show_all,
    }
  }
  /// Whether `AV_CODEC_FLAG_OUTPUT_CORRUPT` was set.
  #[inline]
  pub const fn output_corrupt(&self) -> bool {
    self.output_corrupt
  }
  /// Whether `AV_CODEC_FLAG2_SHOW_ALL` was set.
  #[inline]
  pub const fn show_all(&self) -> bool {
    self.show_all
  }
}

/// Payload for [`Error::ReplayQueueFull`].
///
/// One budget spans the whole queue — what a replay left waiting and any
/// tail drained behind it. A drain that reaches it stops and resumes once
/// the caller has taken pictures, so pictures are never refused for the
/// queue being full; only a picture that alone exceeds the budget, which no
/// draining could make room for, is refused, by this name. The picture is
/// released with the refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
  "a decoded picture of {frame_bytes} bytes alone exceeds the software video decoder's replay \
   budget of {budget} bytes"
)]
pub struct ReplayQueueFull {
  frame_bytes: usize,
  budget: usize,
}

impl ReplayQueueFull {
  /// Constructs a [`ReplayQueueFull`] payload.
  #[inline]
  pub const fn new(frame_bytes: usize, budget: usize) -> Self {
    Self {
      frame_bytes,
      budget,
    }
  }
  /// The bytes the refused picture holds.
  #[inline]
  pub const fn frame_bytes(&self) -> usize {
    self.frame_bytes
  }
  /// The queue's byte budget it exceeds.
  #[inline]
  pub const fn budget(&self) -> usize {
    self.budget
  }
}

/// Payload for [`Error::HwDeviceInitFailed`].
///
/// `av_hwdevice_ctx_create` failed for the requested backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("hardware device init failed for {backend:?}: {source}")]
pub struct HwDeviceInitFailed {
  /// Backend that failed to initialise.
  backend: Backend,
  /// Underlying FFmpeg error.
  source: ffmpeg_next::Error,
}

impl HwDeviceInitFailed {
  /// Constructs a new [`HwDeviceInitFailed`] payload.
  #[inline]
  pub const fn new(backend: Backend, source: ffmpeg_next::Error) -> Self {
    Self { backend, source }
  }
  /// Backend that failed to initialise.
  #[inline]
  pub const fn backend(&self) -> Backend {
    self.backend
  }
  /// Underlying FFmpeg error.
  #[inline]
  pub const fn source(&self) -> &ffmpeg_next::Error {
    &self.source
  }
  /// Consume the payload, returning the backend identifier and the
  /// moved FFmpeg error so callers can take ownership without
  /// cloning.
  #[inline]
  pub fn into_parts(self) -> (Backend, ffmpeg_next::Error) {
    (self.backend, self.source)
  }
}

/// Where in the decoder's life a [`AllBackendsFailed`] was raised.
///
/// The [`crate::FfmpegVideoStreamDecoder`] wrapper routes its software-fallback
/// replay on **this explicit signal** rather than inferring origin from whether
/// `unconsumed_packets` is empty. Both origins can carry an empty
/// `unconsumed_packets` — a probe-era failure on the *first* packet (a
/// side-data / byte / packet cap trip, or an `av_packet_ref` ENOMEM) has no
/// prior history to surface, exactly like every post-commit failure — so
/// emptiness cannot disambiguate them. Conflating the two made the wrapper
/// treat a probe-era first-packet cap trip as post-commit: it would append a
/// clone of the borrowed current packet to an empty replay set and skip the
/// post-fallback `send_packet`, silently dropping that packet if the clone
/// failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IsVariant)]
pub enum FallbackOrigin {
  /// Raised while the inner decoder's probe was still active (before the first
  /// frame). `unconsumed_packets` is the probe's buffered history (possibly
  /// empty when the failure landed on the very first packet). The wrapper
  /// replays that history and then routes the still-unconsumed current packet
  /// to the new software decoder itself.
  Probe,
  /// Raised after the probe collapsed (the committed backend failed at
  /// runtime). `unconsumed_packets` is always empty — the probe buffer is gone
  /// — so the wrapper does not replay: it opens a software decoder cold,
  /// forwards only the failing call's current packet (or EOF), and resyncs at
  /// the next keyframe, accepting a bounded, logged gap (degrade-and-continue).
  PostCommit,
}

/// Payload for [`Error::AllBackendsFailed`].
///
/// Auto-probe exhausted every backend in the platform's order. Empty
/// `attempts` means the platform has no hardware backends listed in
/// [`crate::Backend`] for the current `target_os` — callers must
/// fall back to a software decoder of their choice.
///
/// `unconsumed_packets` holds the packets the decoder accepted from
/// the caller before the probe exhausted (refcounted shallow clones
/// of the packets fed via `send_packet`). For non-seekable inputs
/// (live streams, pipes, network sources) the caller cannot
/// re-demux from start, so this crate surfaces the buffered history
/// here so the caller can feed those packets directly into a
/// software decoder of their choice. When `AllBackendsFailed` comes
/// from [`crate::VideoDecoder::open`] (no packets were ever sent),
/// this vec is empty.
///
/// `origin` records whether the failure happened during the probe or after the
/// committed backend collapsed at runtime — the explicit signal the wrapper
/// routes on (see [`FallbackOrigin`]). It is never inferred from
/// `unconsumed_packets.is_empty()`, which both origins can satisfy.
///
/// `Debug` is hand-written: [`ffmpeg_next::Packet`] does not derive
/// `Debug`, so we print `[N packets]` instead of dumping per-packet
/// bytes, which would be both noisy and useless for triage.
#[derive(thiserror::Error)]
#[error("all hardware backends failed; attempts: {attempts:?}")]
pub struct AllBackendsFailed {
  /// Per-backend errors collected during probing, in the order tried.
  attempts: Vec<(Backend, Box<Error>)>,
  /// Packets the decoder consumed from the caller before exhaustion.
  /// Replay them through a software decoder for non-seekable inputs.
  unconsumed_packets: Vec<Packet>,
  /// Whether this was raised during the probe or post-commit. The wrapper's
  /// fallback replay routes on this, never on `unconsumed_packets` emptiness.
  origin: FallbackOrigin,
}

impl AllBackendsFailed {
  /// Constructs a probe-era [`AllBackendsFailed`] payload — raised while the
  /// inner decoder's probe is still active. `unconsumed_packets` is the probe's
  /// buffered history (possibly empty if the failure landed on the first
  /// packet). See [`FallbackOrigin::Probe`].
  ///
  /// Not `const fn`: the `Vec` arguments may carry destructors and
  /// the const evaluator can't prove their drop safe for arbitrary
  /// allocator state.
  #[inline]
  pub fn new(attempts: Vec<(Backend, Box<Error>)>, unconsumed_packets: Vec<Packet>) -> Self {
    Self {
      attempts,
      unconsumed_packets,
      origin: FallbackOrigin::Probe,
    }
  }
  /// Constructs a post-commit [`AllBackendsFailed`] payload — raised after the
  /// probe collapsed, when the committed backend failed at runtime.
  /// `unconsumed_packets` is always empty (the probe buffer is gone); the
  /// wrapper's retained GOP window supplies the replay set. See
  /// [`FallbackOrigin::PostCommit`].
  #[inline]
  pub fn new_post_commit(attempts: Vec<(Backend, Box<Error>)>) -> Self {
    Self {
      attempts,
      unconsumed_packets: Vec::new(),
      origin: FallbackOrigin::PostCommit,
    }
  }
  /// Per-backend errors collected during probing, in the order tried.
  #[inline]
  pub fn attempts(&self) -> &[(Backend, Box<Error>)] {
    &self.attempts
  }
  /// Where this failure was raised — the explicit probe-vs-post-commit signal
  /// the wrapper routes its fallback replay on.
  #[inline]
  pub const fn origin(&self) -> FallbackOrigin {
    self.origin
  }
  /// Packets the decoder consumed from the caller before exhaustion.
  /// Replay them through a software decoder for non-seekable inputs.
  #[inline]
  pub fn unconsumed_packets(&self) -> &[Packet] {
    &self.unconsumed_packets
  }
  /// Consume the payload, returning the moved unconsumed packets so
  /// non-seekable callers can replay them through a software decoder
  /// without cloning.
  #[inline]
  pub fn into_unconsumed_packets(self) -> Vec<Packet> {
    self.unconsumed_packets
  }
  /// Consume the payload, returning the moved attempts log and
  /// unconsumed packets.
  #[inline]
  pub fn into_parts(self) -> (Vec<(Backend, Box<Error>)>, Vec<Packet>) {
    (self.attempts, self.unconsumed_packets)
  }
}

impl std::fmt::Debug for AllBackendsFailed {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AllBackendsFailed")
      .field("attempts", &self.attempts)
      // `Packet` is not `Debug`; print just the count so the error is
      // still useful for triage without dumping per-packet bytes.
      .field(
        "unconsumed_packets",
        &format_args!("[{} packets]", self.unconsumed_packets.len()),
      )
      .field("origin", &self.origin)
      .finish()
  }
}

/// Payload for [`Error::FallbackFailed`].
///
/// Surfaced by [`crate::FfmpegVideoStreamDecoder`] when a HW->SW
/// fallback attempt itself fails — e.g. the SW decoder failed to
/// open, EOF replay returned EAGAIN past the bounded retry, or the
/// per-frame replay queue exceeded its cap. The HW decoder has
/// already consumed `unconsumed_packets` from the caller; we
/// surface them here so non-seekable inputs (pipes, live streams)
/// can drive their own decoder of last resort.
///
/// `Debug` is hand-written for the same reason as
/// [`AllBackendsFailed`]: [`ffmpeg_next::Packet`] does not derive
/// `Debug`.
#[derive(thiserror::Error)]
#[error("HW->SW fallback failed: {source}")]
pub struct FallbackFailed {
  /// Underlying error that aborted the fallback transition.
  source: Box<Error>,
  /// Packets that the HW path had consumed but had not yet decoded
  /// at fallback time. The caller can replay them through a
  /// software decoder of their choice.
  unconsumed_packets: Vec<Packet>,
}

impl FallbackFailed {
  /// Constructs a new [`FallbackFailed`] payload.
  ///
  /// Not `const fn`: the `Vec` argument may carry destructors.
  #[inline]
  pub fn new(source: Box<Error>, unconsumed_packets: Vec<Packet>) -> Self {
    Self {
      source,
      unconsumed_packets,
    }
  }
  /// Underlying error that aborted the fallback transition.
  #[inline]
  pub fn source(&self) -> &Error {
    &self.source
  }
  /// Packets that the HW path had consumed but had not yet decoded
  /// at fallback time.
  #[inline]
  pub fn unconsumed_packets(&self) -> &[Packet] {
    &self.unconsumed_packets
  }
  /// Consume the payload, returning the moved unconsumed packets so
  /// non-seekable callers can replay them through a software decoder
  /// without cloning.
  #[inline]
  pub fn into_unconsumed_packets(self) -> Vec<Packet> {
    self.unconsumed_packets
  }
  /// Consume the payload, returning the moved source error and
  /// unconsumed packets.
  #[inline]
  pub fn into_parts(self) -> (Box<Error>, Vec<Packet>) {
    (self.source, self.unconsumed_packets)
  }
}

impl std::fmt::Debug for FallbackFailed {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("FallbackFailed")
      .field("source", &self.source)
      .field(
        "unconsumed_packets",
        &format_args!("[{} packets]", self.unconsumed_packets.len()),
      )
      .finish()
  }
}

/// Payload for [`Error::HwTransferTooLarge`].
///
/// The CPU-side cost of a hardware->CPU download, priced before it
/// happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("hw->cpu transfer would allocate {bytes} bytes for one frame, over a ceiling of {limit}")]
pub struct HwTransferTooLarge {
  bytes: usize,
  limit: usize,
}

impl HwTransferTooLarge {
  /// Constructs a `HwTransferTooLarge` payload.
  #[inline]
  pub const fn new(bytes: usize, limit: usize) -> Self {
    Self { bytes, limit }
  }
  /// Bytes the destination frame would have cost.
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

/// Payload for [`Error::HwSurfaceTooLarge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
  "the hardware surface pool would cost {bytes} bytes, over a ceiling of {limit}; \
   the hardware format was declined before the pool was built"
)]
pub struct HwSurfaceTooLarge {
  bytes: i64,
  limit: i64,
}

impl HwSurfaceTooLarge {
  /// Constructs a `HwSurfaceTooLarge` payload.
  #[inline]
  pub const fn new(bytes: i64, limit: i64) -> Self {
    Self { bytes, limit }
  }
  /// What the pool would have cost, priced through the same
  /// allocator-parity footprint every other judge uses.
  #[inline]
  pub const fn bytes(&self) -> i64 {
    self.bytes
  }
  /// The ceiling in force.
  #[inline]
  pub const fn limit(&self) -> i64 {
    self.limit
  }
}

/// Which kind of frame a [`FrameBudgetExceeded`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameMedium {
  /// A picture.
  Video,
  /// An audio frame.
  Audio,
}

impl core::fmt::Display for FrameMedium {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    match self {
      Self::Video => f.write_str("picture"),
      Self::Audio => f.write_str("audio frame"),
    }
  }
}

/// Payload for [`Error::FrameBudgetExceeded`].
///
/// The allocator judge refused a frame whose real cost — priced through
/// the allocator-parity footprint, before `avcodec_default_get_buffer2`
/// ran — exceeds the caller's ceiling.
///
/// # Why this has a name
///
/// A `get_buffer2` callback can only answer libavcodec with an errno,
/// and `AVERROR(EINVAL)` is what libavcodec itself reports for corrupt
/// input. Without a name, a caller could not tell "this file is broken"
/// from "your budget refused this frame" — and only one of those is
/// worth retrying with a larger ceiling.
///
/// # Which frame
///
/// [`pts`](Self::pts) is the refused frame's presentation timestamp as
/// FFmpeg set it before the allocation — the timestamp of the packet it was
/// being decoded from, in the stream's time base. A software video decoder
/// on frame threads refuses a picture on a worker, for a packet sent
/// earlier, and FFmpeg's H.264 decoder can conceal it behind a later
/// picture of the same packet with no error to follow; such a refusal is
/// reported at the next `receive_frame`, as its own error ahead of the
/// decoder's next answer, and names no packet but the one its `pts` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
  "the {medium}{at} would allocate {bytes} bytes, over a ceiling of {limit}",
  at = at_pts(.pts)
)]
pub struct FrameBudgetExceeded {
  bytes: u64,
  limit: u64,
  medium: FrameMedium,
  pts: Option<i64>,
}

/// " at pts N" for a known `pts`, for a message; nothing otherwise.
fn at_pts(pts: &Option<i64>) -> String {
  pts.map_or_else(String::new, |pts| format!(" at pts {pts}"))
}

impl FrameBudgetExceeded {
  /// Constructs a `FrameBudgetExceeded` payload, of a frame whose `pts` is
  /// not known.
  #[inline]
  pub const fn new(bytes: u64, limit: u64, medium: FrameMedium) -> Self {
    Self {
      bytes,
      limit,
      medium,
      pts: None,
    }
  }
  /// This payload, of the frame whose presentation timestamp is `pts`.
  #[inline]
  #[must_use]
  pub const fn with_pts(mut self, pts: Option<i64>) -> Self {
    self.pts = pts;
    self
  }
  /// The refused frame's presentation timestamp, in the stream's time base,
  /// as FFmpeg set it before the allocation — the timestamp of the packet it
  /// was decoded from; `None` where it had none.
  #[inline]
  pub const fn pts(&self) -> Option<i64> {
    self.pts
  }
  /// What the frame would have cost.
  #[inline]
  pub const fn bytes(&self) -> u64 {
    self.bytes
  }
  /// The ceiling in force.
  #[inline]
  pub const fn limit(&self) -> u64 {
    self.limit
  }
  /// Whether the frame was a picture or audio.
  #[inline]
  pub const fn medium(&self) -> FrameMedium {
    self.medium
  }
}

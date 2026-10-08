//! `mediadecode::VideoStreamDecoder` impl with HW + SW fallback.
//!
//! [`FfmpegVideoStreamDecoder`] starts on the hardware path: an inner
//! [`crate::VideoDecoder`] that auto-probes VideoToolbox / VAAPI /
//! NVDEC / D3D11VA. When every HW backend fails — at `open` time
//! (no backend opens) or mid-stream ([`crate::Error::AllBackendsFailed`]
//! from `send_packet` / `receive_frame` / `send_eof`) — we transparently
//! fall back to a **software** `ffmpeg::decoder::Video` opened from the
//! same `Parameters`.
//!
//! Two HW-exhaustion shapes feed the same fallback, distinguished by an
//! **explicit origin** the `AllBackendsFailed` carries
//! ([`crate::error::FallbackOrigin`]) — *not* by whether its rescued
//! `unconsumed_packets` is empty (both shapes can be empty: a probe-era
//! failure on the first packet has no prior history, exactly like every
//! post-commit failure):
//!
//! * **Probe-era** (pre-first-frame, [`crate::error::FallbackOrigin::Probe`]):
//!   the inner decoder buffered every packet it consumed and surfaces them in
//!   `unconsumed_packets`. We **replay exactly those** through the SW decoder
//!   (lossless — no frame was delivered yet), then route the still-unconsumed
//!   current packet (the one the inner decoder failed on / refused) to SW
//!   ourselves. This is the original pre-runtime-fallback behaviour and is
//!   unchanged.
//! * **Post-commit** (after the first frame, the inner probe is gone,
//!   [`crate::error::FallbackOrigin::PostCommit`]): a runtime HW-decode failure
//!   — e.g. VideoToolbox choking on H.264 High 4:2:2 10-bit — is reclassified
//!   to `AllBackendsFailed` by the inner decoder with an **empty**
//!   `unconsumed_packets` (the probe buffer no longer exists). Here we
//!   **degrade and continue** rather than reconstruct: open the SW decoder with
//!   an empty replay set and let it **resync at the next keyframe**. Fed
//!   forward packets from the failure point, the SW decoder naturally produces
//!   nothing until that keyframe, then decodes normally from there. The bounded
//!   span from the failure point to the next keyframe is dropped — an accepted,
//!   **loudly logged** gap (a single `tracing::warn!`), not a silent one. The
//!   indexing pipeline this serves prefers a small logged gap over the
//!   error-prone mid-stream-reconstruction state machine a lossless replay
//!   would require (see findit-studio/mediadecode#12). The *bounded*-ness is
//!   **enforced, not assumed**, and the resync is **proved** — by FFmpeg's own
//!   output withholding for H.264, by the decoder's reorder bound for every
//!   other codec — never by matching a picture to a packet. A post-commit
//!   fallback enters a degraded-resync mode that holds until a picture the
//!   decoder outputs is decoded from a keyframe fed across the gap. The anchor
//!   is a key-flagged packet the decoder takes after the commit whose first
//!   picture the bitstream proves a random-access one — an H.264 IDR picture or
//!   a picture behind a recovery point SEI message, an HEVC IRAP picture; for a
//!   codec whose pictures this crate does not read, the key flag FFmpeg's
//!   parser set — since an intra picture resets the references of every picture
//!   after it that does not lead it; it is fed once the decoder's output is
//!   settled (drained to "needs input" since the last packet). For H.264 on
//!   FFmpeg's own `h264` decoder, the implementation the software road opens
//!   by name, the first picture out after the anchor closes the gap: opened
//!   with neither `AV_CODEC_FLAG_OUTPUT_CORRUPT` nor `AV_CODEC_FLAG2_SHOW_ALL`
//!   (an open that finds either set is refused, [`Error::UnrecoveredOutput`]),
//!   it outputs only pictures its recovery tracking has marked recovered, and
//!   a decoder opened cold across the gap starts with nothing recovered;
//!   another implementation of the codec, and an anchor after a decode error
//!   across the gap, take the reorder bound. For every other codec the only
//!   pictures from before the
//!   anchor that can still come out are the ones its reorder buffer holds, at
//!   most `has_b_frames` of them (the largest value read from just before the
//!   anchoring packet was submitted on — a keyframe can activate parameters
//!   that lower it while the pictures from before it still wait; none for VP8,
//!   VP9 and AV1, which do not reorder): the `has_b_frames + 1`-th picture
//!   output after the anchor is at or after it in decode order, and its
//!   delivery closes the gap. Nothing is drained or reset for it — the decoder
//!   that kept decoding keeps every picture. A concealed picture a lenient
//!   codec makes of a lone P-frame from the dropped span does not close the
//!   gap, nor, outside H.264, a picture still in the reorder buffer at the
//!   anchor. The end of the stream proves nothing more: the same proof applies
//!   there. If EOF is reached while the mode is still pending — no key-flagged
//!   packet was fed across the gap, or the pictures out after one never proved
//!   it — `receive_frame` escalates with a distinct
//!   [`VideoDecodeError::PostCommitNeverResynced`] (and a `tracing::error!`),
//!   counting the packets fed before a keyframe anchored the resync and those
//!   fed after it with the resync unproved, rather than surfacing a clean
//!   end-of-stream that would swallow the tail silently. So the gap is either
//!   bounded-and-logged (a resync happened) or reported-at-EOF (it never did) —
//!   never silent-and-unbounded.
//!
//!   The post-commit path retains and reconstructs **zero** frames: it opens
//!   SW cold, forwards only the failure arm's current packet (or EOF), and
//!   lets SW resync naturally. It never populates the replay-frame queue.
//!
//! The probe-era replay happens before the new packet (or the next
//! `receive_frame` poll) is processed, so a probe-era HW exhaustion on a
//! non-seekable input loses no compressed data. The post-commit path
//! intentionally accepts the next-keyframe gap.
//!
//! After the transition the decoder stays on SW for the rest of its
//! life — there's no probe-back-to-HW logic; once we've decided the
//! stream isn't HW-decodable, that decision is sticky.
//!
//! **Threads, after a fallback.** Every software decoder a session opens
//! decodes on the [`Threads`](crate::Threads) its limits ask for, except
//! the one a fallback commits: that one runs on one thread, because the
//! fallback is a transaction and only a one-thread decoder decides one
//! before it commits — a frame-threaded decoder reports a packet's failure
//! a packet per thread later. The session returns to its threads at the
//! next CLEAN random access point — a keyframe nothing after it references
//! past, proved so by its bitstream, such as an H.264 IDR; never an HEVC
//! CRA or an H.264 recovery point, whose leading pictures reference the GOP
//! before them, and never one inferred from the old decoder's
//! `has_b_frames` — or at the first keyframe after a seek, which discards
//! leading pictures by its own nature. There the one-thread decoder is drained, every picture it holds
//! delivered in order, and closed, and a decoder on the session's threads is
//! opened and fed from the keyframe on; after a post-commit degrade, not
//! before its resync. One software decoder is open at any instant, no
//! packet is decoded twice and no picture is lost, so there is never a
//! second decoded history for a budget to span. A stream whose keyframes
//! are all open stays on the one thread until a seek, and says so once a
//! minute of it has gone by.
//!
//! Frames produced by either path are converted via
//! [`crate::convert::av_frame_to_video_frame`] so the consumer sees
//! the same `mediadecode::VideoFrame<PixelFormat, VideoFrameExtra,
//! FfmpegBytes>` shape regardless of which backend produced it.

use std::collections::VecDeque;

/// Which keyframes a decoder can start at without losing a picture.
mod access;

/// The most pictures the software road's queue of pictures waiting for
/// delivery holds at once — a cheap second bound beside its byte budget
/// ([`DecoderLimits::max_replay_bytes`]). The queue takes a fallback
/// replay's pictures and the tail a one-thread decoder is drained of where
/// the session restarts it at a clean keyframe; a drain that reaches either
/// bound stops, resumable, rather than drop a picture (see `drain_into`).
const SW_REPLAY_FRAME_CAP: usize = 64;

use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::{Packet, codec::Parameters, frame};
use mediadecode::{
  Received, Sent, Timebase,
  decoder::{ScaledOutputCapability, VideoStreamDecoder},
  frame::VideoFrame,
  packet::VideoPacket,
};

use crate::{
  Backend, DecoderLimits, Error, Ffmpeg, Frame, VideoDecoder, boundary,
  convert::{self, ConvertError},
  decoder::{build_codec_context, try_clone_parameters},
  error::FallbackFailed,
  extras::{VideoFrameExtra, VideoPacketExtra},
  frame::alloc_av_video_frame,
};

/// Which decode path a video session takes — the choice
/// [`CarrierVideoStreamDecoder::open_as`] is given.
///
/// # The arms differ in what they PERMIT, not only in where they start
///
/// [`Auto`](Self::Auto) is a preference: it starts on hardware and is
/// free to end on software, at open or mid-stream. The other two are
/// **pins**, and a pin that a mid-stream failure could quietly undo
/// would not be one — so a session opened on either of them stays on
/// the path it was opened on for its whole life, and a hardware failure
/// that `Auto` would degrade through is reported instead.
///
/// That is the difference the two consumers of this door need. A
/// determinism comparison decodes *one stream* both ways and compares
/// the pixels; a run that silently swapped paths halfway would compare
/// nothing and say it had. An operator turning hardware off for a lane
/// over a driver that produces wrong pixels needs it to stay off.
///
/// # Observability is unchanged
///
/// [`is_hardware`](CarrierVideoStreamDecoder::is_hardware) and
/// [`is_software`](CarrierVideoStreamDecoder::is_software) read where a
/// session **is**, which stays a live reading — under
/// [`Auto`](Self::Auto) it can still change once, and under the pins it
/// answers what was pinned because nothing can move it.
///
/// This type deliberately grows **no** `is_*` predicates of its own,
/// where most vocabularies in this crate do. They would spell the
/// decoder's two questions a second time with a different meaning —
/// `path.is_software()` is *what was asked for* and
/// `decoder.is_software()` is *where it ended up*, and under
/// [`Auto`](Self::Auto) those genuinely differ. A caller that needs to
/// branch on the choice it made already holds the value and can
/// `match` it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecodePath {
  /// Probe the platform's hardware backends in order and fall back to
  /// software — at open, and again on a mid-stream hardware failure.
  ///
  /// What [`CarrierVideoStreamDecoder::open`] has always done, and what
  /// it still does.
  Auto,
  /// **This hardware backend, or nothing.** No other backend is probed
  /// and software is never opened.
  ///
  /// A backend that cannot be opened for the stream fails the
  /// [`open_as`](CarrierVideoStreamDecoder::open_as) call. A backend
  /// that opens and then fails to decode surfaces
  /// [`Error::AllBackendsFailed`] from the send or receive road that
  /// met it, carrying that backend and what it said — the same error
  /// [`Auto`](Self::Auto) treats as its cue to degrade, reported here
  /// because degrading is what this arm declines.
  Hardware(Backend),
  /// **Software, with no probe at all.**
  ///
  /// Opens `libavcodec`'s own decoder for the stream directly. There is
  /// no hardware in this session to fail, so there is nothing for it to
  /// fall back from — the terminal state [`Auto`](Self::Auto) reaches
  /// by degrading, entered on purpose.
  Software,
}

/// `mediadecode::VideoStreamDecoder` impl with transparent HW → SW
/// fallback.
pub struct CarrierVideoStreamDecoder<C: crate::FfmpegCarrier> {
  state: DecodeState,
  /// The path this session was opened on — see [`DecodePath`].
  ///
  /// Read for exactly one question, [`Self::may_open_software`]: whether
  /// a hardware exhaustion is this session's cue to degrade or its cue
  /// to report. Kept as the whole choice rather than reduced to that
  /// bit so a session can say what it *is*, not only what it allows.
  path: DecodePath,
  /// Codec parameters retained so we can open a software
  /// `ffmpeg::decoder::Video` if the HW probe exhausts.
  parameters: Parameters,
  /// HW-side scratch frame (filled by [`VideoDecoder::receive_frame`]).
  hw_scratch: Frame,
  /// SW-side scratch frame (filled by `ffmpeg::decoder::Video::receive_frame`).
  sw_scratch: frame::Video,
  /// Frames produced while draining the SW decoder during fallback
  /// replay (see [`Self::fall_back_to_sw`]), and the tail a restart at a
  /// clean keyframe drains. The trait's `receive_frame` delivers from this
  /// queue before pulling new frames from the SW decoder. Empty in
  /// steady-state operation.
  sw_replay_frames: ReplayQueue,
  /// A restart of the software decoder in progress — its drain stopped at
  /// the queue's budget — finished when the caller sends the keyframe
  /// again ([`Self::send_on_software`]).
  restart: Option<Restart>,
  /// The rescued packets a probe-era fallback's replay has not fed yet:
  /// the queue reached its budget first. They are fed, under the budget,
  /// before anything the caller sends next ([`Self::replay_pending`]).
  pending_history: VecDeque<Packet>,
  /// Whether that replay still owes the decoder the end of the stream. Once
  /// the decoder takes it, that end is the session's: [`Self::eof_sent`] is
  /// committed at once, whatever the drain after it meets
  /// ([`Self::replay_pending`]).
  pending_eof: bool,
  /// Whether that replay's last drain stopped at the queue's budget — a
  /// picture parked, or the queue full — with every packet and the end it
  /// owed fed: the decoder may still hold pictures the replay made. They are
  /// drained into the queue, under the budget, before anything the caller
  /// sends next is taken; until a drain finds the decoder with none ready,
  /// both send roads answer `MustDrain` ([`Self::replay_pending`]).
  replay_output_pending: bool,
  /// A decode error met while feeding what was pending — a replay's packet
  /// or a restart's drain — on a send, which answered `MustDrain` with the
  /// caller's packet untaken, or on a drain past the end, which feeds the
  /// replay itself. Reported on the drain, after the pictures queued before
  /// it.
  deferred_error: Option<Error>,
  /// Resource ceilings for the frames this decoder exports, and for the
  /// `AVCodecContext`s it opens — HW candidates, the SW fallback, and
  /// any decoder a later probe advance builds all get the same number.
  limits: DecoderLimits,
  /// `true` while the software decoder serving runs on one thread and
  /// [`limits`](Self::limits) asks for more: from a fallback, which
  /// commits the one-thread decoder that proved what it was fed, until
  /// the next clean keyframe (or the first after a seek), where
  /// [`send_on_software`](Self::send_on_software) drains that decoder,
  /// closes it and opens one on the session's threads
  /// ([`open_after_drain`](Self::open_after_drain)).
  sw_threads_pending: bool,
  /// `true` from a [`flush`](Self::flush_impl) until the next keyframe:
  /// the first keyframe after a seek is a switch point whatever kind it
  /// is, because the seek has already discarded what led it.
  seeked: bool,
  /// The timestamp of the first packet the one-thread decoder took after
  /// a fallback (or after a seek), while
  /// [`sw_threads_pending`](Self::sw_threads_pending) holds — the start of
  /// the minute [`Self::note_one_thread`] counts.
  one_thread_since: Option<i64>,
  /// `true` once the minute's warning has been given; it is given once.
  one_thread_warned: bool,
  /// Test-only: every open on the session's threads fails.
  #[cfg(test)]
  fail_threaded_opens: bool,
  /// Test-only: how many opens on the session's threads were attempted.
  #[cfg(test)]
  threaded_opens: usize,
  /// Test-only: the `has_b_frames` the decoder serving reports, in place of
  /// its own.
  #[cfg(test)]
  reorder_override: Option<usize>,
  /// Test-only: the `has_b_frames` the decoder serving reports once it has
  /// taken its next packet — the parameters a keyframe activates lowering
  /// it.
  #[cfg(test)]
  reorder_on_submit: Option<usize>,
  /// Test-only: the next packet the decoder serving takes is reported
  /// failed, as FFmpeg reports a packet it decoded with an error.
  #[cfg(test)]
  fail_after_submit: bool,
  /// `true` once `send_eof` has been called on the active decoder.
  /// Used to propagate EOF to the SW decoder when fallback fires
  /// during the drain phase — without this, codecs that hold tail
  /// frames at EOF would hang waiting for an EOF they already saw on
  /// the HW path.
  eof_sent: bool,
  /// `true` between a **post-commit** fallback firing and its resync: the
  /// delivery of a picture the stream's proof places at or after a
  /// key-flagged packet fed across the gap ([`Self::resync_on_output`]). A
  /// post-commit fallback opens SW cold and drops the bounded span up to the
  /// next keyframe; the promise is that the span is *bounded* — SW resyncs
  /// there. This flag makes the promise enforced rather than assumed: while
  /// it is set we have no proof SW ever recovered at a keyframe. A
  /// concealed picture a lenient codec emits from the gap does **not**
  /// clear it, nor, outside H.264, does a picture still in the reorder
  /// buffer at the anchor; if EOF is reached while it is still set the loss
  /// is escalated
  /// (a distinct loud error) rather than silently swallowing the whole tail.
  /// Probe-era fallbacks never set it — they replay losslessly and produce
  /// frames immediately.
  degraded_resync_pending: bool,
  /// `true` once a key-flagged packet fed across the open gap, its first
  /// picture proved a random-access one ([`Self::anchor`]), anchors the
  /// resync; reset by a decode error before the resync is proven, so the
  /// next such packet anchors again. No picture is matched to a
  /// packet: the anchor only starts the count the proof reads
  /// ([`Self::outputs_since_anchor`], [`Self::anchor_proof`]).
  degraded_anchored: bool,
  /// The largest `has_b_frames` the decoder serving has shown since just
  /// before the anchoring packet was submitted — how many pictures from
  /// before the anchor its reorder buffer may still hold. Read before the
  /// submission, because a keyframe can activate parameters that lower it
  /// (an HEVC SPS with fewer `num_reorder_pics`) while those pictures still
  /// wait; then after it, after every packet since and at every picture
  /// out, the largest kept. The bound reads it.
  anchor_reorder: usize,
  /// Whether the stream's keyframes reset every reference with no
  /// reordering (VP8, VP9, AV1): the bound is the first picture.
  anchor_resets: bool,
  /// How the anchored resync is proved, the stream's codec rule
  /// ([`access::KeyframeRule::proof`]): FFmpeg withholding every picture it
  /// has not recovered (H.264), so the first picture out closes the gap, or
  /// the reorder bound.
  anchor_proof: access::Proof,
  /// `true` once the software decoder reported a decode error across the
  /// open gap — on a send or a receive, before any proof closed it — until a
  /// proof closes the gap, a seek resets it, or a new post-commit gap opens
  /// on a new decoder. FFmpeg's H.264 decoder sets its recovery state, which
  /// it keeps, while it parses a picture, and a packet it reports failed may
  /// have been parsed in part: pictures from before the next anchor may come
  /// out marked recovered. So the next anchor is proved by the reorder bound
  /// instead of the withheld output ([`Self::check_resync_proof`]): the bound
  /// is sound whatever the decoder's own state.
  withheld_poisoned: bool,
  /// Whether the anchor is definitive — a clean random access point
  /// ([`access::Anchor::definitive`]); one that is not is superseded by the
  /// next that is, the count restarting there.
  anchor_definitive: bool,
  /// The H.264 recovery point the anchor stands on, if any: reported, never
  /// counted ([`Self::check_resync_proof`]).
  anchor_recovery: Option<access::RecoveryPoint>,
  /// Pictures the decoder serving has output, and the caller taken, since
  /// the anchor.
  outputs_since_anchor: usize,
  /// `true` while the software decoder holds no picture the caller has not
  /// taken: it answered "needs input" (or the end) since the last packet it
  /// took, or was reported failed on — FFmpeg's submission is not
  /// transactional, and a packet it reports failed may have left pictures
  /// ready. A key-flagged packet anchors only then, so the pictures from
  /// before it are the reorder buffer's alone.
  sw_output_settled: bool,
  /// Packets the software decoder took across the open gap before a
  /// key-flagged packet anchored the resync — the fallback window. Reported
  /// by the escalation ([`PostCommitNeverResynced::packets_before_anchor`]);
  /// reset whenever the gap closes, and on `flush`.
  packets_before_anchor: u64,
  /// Packets the software decoder took after the first anchor while the gap
  /// stayed open — through an un-anchor and any anchor after it: decoded,
  /// their pictures delivered, never proved to come from after the gap.
  /// Reported by the escalation ([`PostCommitNeverResynced::packets_unproven`]).
  packets_unproven: u64,
  /// Whether a key-flagged packet has anchored the resync since the gap
  /// opened, un-anchored since or not.
  anchor_seen: bool,
  /// Source-stream time base, used to label produced frames.
  time_base: Timebase,
  /// The lane this decoder captures into. A marker: the carrier
  /// appears in the frames it produces, not in its own state.
  /// `true` when the scratch frame holds a decoded frame whose
  /// conversion has **not committed** — see
  /// [`CarrierAudioStreamDecoder::scratch_pending`](crate::audio::CarrierAudioStreamDecoder)
  /// for the reasoning, which is the same on both roads.
  ///
  /// **This decoder has two scratches and can change which one is
  /// current, so the seat is enforced rather than merely recorded.**
  /// While it is set, `send_packet` and `send_eof` answer
  /// [`Sent::MustDrain`]: both are the roads that commit a
  /// hardware-to-software fallback, and a fallback under a parked frame
  /// would leave the retry reading the *other* scratch — delivering a
  /// stale frame, or refusing permanently and stranding a decoded one.
  /// Refusing makes the retry's state the state that parked it **by
  /// construction**, which is a stronger guarantee than remembering
  /// which road produced it.
  ///
  /// **The discipline is unchanged; only its spelling moved.** It was
  /// `VideoDecodeError::FramePending`, and the escape was already
  /// documented as "call `receive_frame`, or `flush` to abandon it" —
  /// which is to say it was back pressure wearing an error's clothes.
  /// Now it says so, and a caller can act on it without inspecting a
  /// backend-specific error type. The subtitle decoder keeps the same
  /// seat one road over, spelled the same way.
  scratch_pending: bool,
  _carrier: core::marker::PhantomData<C>,
}

/// Hardware-decode seam behind [`DecodeState::Hw`]. In production this is
/// the real [`VideoDecoder`]; tests substitute a fake to drive the
/// post-commit fallback path without a live GPU. Mirrors the subset of
/// `VideoDecoder`'s surface the wrapper drives on the HW path.
pub(crate) trait HwInner: Send {
  /// See [`VideoDecoder::send_packet`].
  fn send_packet(&mut self, packet: &Packet) -> Result<Sent, Error>;
  /// See [`VideoDecoder::receive_frame`].
  fn receive_frame(&mut self, frame: &mut Frame) -> Result<Received, Error>;
  /// See [`VideoDecoder::send_eof`].
  fn send_eof(&mut self) -> Result<Sent, Error>;
  /// See [`VideoDecoder::flush`]. Returns `Result` for a uniform seam even
  /// though the inherent method is infallible.
  fn flush(&mut self) -> Result<(), Error>;
  /// Downcast to the concrete [`VideoDecoder`] when this seam is the real
  /// HW decoder, so [`FfmpegVideoStreamDecoder::hardware_inner`] can keep
  /// exposing it. Returns `None` for a test fake.
  fn as_video_decoder(&self) -> Option<&VideoDecoder>;

  /// Whether a packet submitted **now** would be recorded for replay.
  ///
  /// The probe keeps a rescue history so that a decoder which exhausts
  /// every backend can hand the caller everything FFmpeg consumed since
  /// open. It records by `av_packet_ref`, and
  /// [`AllBackendsFailed::into_unconsumed_packets`] hands those
  /// recordings out as owned, **mutable** `Packet`s — which is why the
  /// view lane must not share its carrier's storage into a submission
  /// that could be recorded. See
  /// [`CarrierVideoStreamDecoder::send_packet_impl`].
  fn records_submissions(&self) -> bool;

  /// See [`VideoDecoder::scaled_output_capability`].
  ///
  /// Defaulted to the refusal so a test fake — which has no
  /// VideoToolbox road behind it, and therefore no stage — answers
  /// honestly without having to say so.
  fn scaled_output_capability(&self) -> ScaledOutputCapability {
    ScaledOutputCapability::Unsupported
  }

  /// See [`VideoDecoder::request_scaled_output`]. Defaulted to the
  /// refusal, for the same reason as above.
  fn request_scaled_output(&mut self, size: (u32, u32)) -> ScaledOutputCapability {
    let _ = size;
    ScaledOutputCapability::Unsupported
  }

  /// See [`VideoDecoder::cancel_scaled_output`]. Defaulted to nothing,
  /// because a seat that never accepts a request has none to withdraw.
  fn cancel_scaled_output(&mut self) {}

  /// The thread count libavcodec settled on for this decoder's context.
  /// Defaulted to one for a test fake, which has no context and decodes
  /// on the caller's thread.
  fn active_threads(&self) -> Option<core::num::NonZeroU32> {
    Some(core::num::NonZeroU32::MIN)
  }
}

impl HwInner for VideoDecoder {
  #[inline]
  fn records_submissions(&self) -> bool {
    self.is_probing()
  }

  #[inline]
  fn send_packet(&mut self, packet: &Packet) -> Result<Sent, Error> {
    VideoDecoder::send_packet(self, packet)
  }
  #[inline]
  fn receive_frame(&mut self, frame: &mut Frame) -> Result<Received, Error> {
    VideoDecoder::receive_frame(self, frame)
  }
  #[inline]
  fn send_eof(&mut self) -> Result<Sent, Error> {
    VideoDecoder::send_eof(self)
  }
  #[inline]
  fn flush(&mut self) -> Result<(), Error> {
    VideoDecoder::flush(self);
    Ok(())
  }
  #[inline]
  fn as_video_decoder(&self) -> Option<&VideoDecoder> {
    Some(self)
  }
  #[inline]
  fn scaled_output_capability(&self) -> ScaledOutputCapability {
    VideoDecoder::scaled_output_capability(self)
  }
  #[inline]
  fn request_scaled_output(&mut self, size: (u32, u32)) -> ScaledOutputCapability {
    VideoDecoder::request_scaled_output(self, size)
  }
  #[inline]
  fn cancel_scaled_output(&mut self) {
    VideoDecoder::cancel_scaled_output(self);
  }
  #[inline]
  fn active_threads(&self) -> Option<core::num::NonZeroU32> {
    VideoDecoder::active_threads(self)
  }
}

/// Internal: which backend is currently driving the decode.
enum DecodeState {
  /// Hardware-backed decoder (auto-probe). May transition to `Sw` on
  /// `AllBackendsFailed`. Boxed behind [`HwInner`] so tests can inject a
  /// fake HW decoder.
  Hw(Box<dyn HwInner>),
  /// Software decoder. Terminal state, together with [`Self::SwClosed`].
  Sw(SwDecoder),
  /// The software road with no decoder open: the one-thread decoder a
  /// fallback committed was drained and closed at a keyframe, and the
  /// decoder on the session's threads that replaces it could not be
  /// opened, nor one on a single thread. Nothing is resident; the next
  /// send opens a decoder again. See
  /// `CarrierVideoStreamDecoder::open_after_drain`.
  SwClosed,
}

/// The software road's queue of decoded pictures waiting for delivery, and
/// the bytes they hold ([`footprint`]).
///
/// It takes a fallback replay's pictures and the tail a one-thread decoder
/// is drained of where the session restarts it at a clean keyframe, and it
/// is delivered before anything the decoder serving outputs. Bounded by the
/// session's [`DecoderLimits::max_replay_bytes`] and by
/// [`SW_REPLAY_FRAME_CAP`] pictures: a drain stops once either is reached
/// (see [`ReplayQueue::full`]) and resumes after the caller has taken
/// pictures, so nothing is dropped to keep within them.
///
/// **The budget is a hard bound on the queue, at the picture's ACTUAL
/// size.** A picture's size is known only once it has been received, and a
/// picture may outgrow the one before it — a resolution or pixel-format
/// change. One that the queue cannot take within its budget is never
/// queued: it is [parked](Self::parked), the ONE picture held past the
/// queue, and the drain stops until the caller has taken enough for it.
#[derive(Default)]
struct ReplayQueue {
  frames: VecDeque<(frame::Video, usize)>,
  bytes: usize,
  /// The bytes of the last picture received into the queue: the size the
  /// next one is taken to have, a hint that stops a drain before it
  /// receives a picture the queue would likely not take. The admission is
  /// the picture's own size ([`Self::parked`]).
  last: usize,
  /// A picture received that the queue could not take within its budget:
  /// held here, after every queued picture in delivery order, until the
  /// caller has taken enough for it. At most one, and nothing more is
  /// received while it waits.
  parked: Option<(frame::Video, usize)>,
}

impl ReplayQueue {
  /// How many pictures wait.
  #[cfg(test)]
  fn len(&self) -> usize {
    self.frames.len()
  }

  fn is_empty(&self) -> bool {
    self.frames.is_empty() && self.parked.is_none()
  }

  /// The bytes the queued pictures hold.
  #[cfg(test)]
  fn bytes(&self) -> usize {
    self.bytes
  }

  /// The bytes of the parked picture, if one waits.
  #[cfg(test)]
  fn parked_bytes(&self) -> usize {
    self.parked.as_ref().map_or(0, |(_, bytes)| *bytes)
  }

  fn front(&self) -> Option<&frame::Video> {
    self
      .frames
      .front()
      .or(self.parked.as_ref())
      .map(|(frame, _)| frame)
  }

  fn pop_front(&mut self) -> Option<frame::Video> {
    let Some((frame, bytes)) = self.frames.pop_front() else {
      // The queue is empty: the parked picture, the last in order, is next.
      return self.parked.take().map(|(frame, _)| frame);
    };
    self.bytes = self.bytes.saturating_sub(bytes);
    Some(frame)
  }

  fn push_back(&mut self, frame: frame::Video, bytes: usize) {
    self.bytes = self.bytes.saturating_add(bytes);
    self.last = bytes;
    self.frames.push_back((frame, bytes));
  }

  fn append(&mut self, other: &mut Self) {
    // A picture parked here would come before `other`'s in delivery order;
    // the one caller appends a fresh replay to an empty queue.
    debug_assert!(
      self.parked.is_none(),
      "a replay appended behind a parked picture"
    );
    self.bytes = self.bytes.saturating_add(other.bytes);
    other.bytes = 0;
    if !other.frames.is_empty() {
      self.last = other.last;
    }
    self.frames.append(&mut other.frames);
    if self.parked.is_none() {
      self.parked = other.parked.take();
    }
  }

  fn clear(&mut self) {
    self.frames.clear();
    self.bytes = 0;
    self.last = 0;
    self.parked = None;
  }

  /// Whether a drain must stop here, before it receives another picture: a
  /// picture is parked, one the size of the last would likely carry the queue
  /// past `budget` bytes (the hint), or the queue holds the
  /// [`SW_REPLAY_FRAME_CAP`] pictures. An empty queue always takes one — a
  /// picture that alone passes the budget is refused by name (see
  /// `drain_into`).
  fn full(&self, budget: usize) -> bool {
    self.parked.is_some()
      || self.frames.len() >= SW_REPLAY_FRAME_CAP
      || (!self.frames.is_empty() && self.bytes.saturating_add(self.last) > budget)
  }

  /// Moves the parked picture into the queue where the queue can now take it
  /// within `budget` — the caller has taken enough, or taken everything.
  fn admit_parked(&mut self, budget: usize) {
    let fits = self.parked.as_ref().is_some_and(|(_, bytes)| {
      self.frames.is_empty() || self.bytes.saturating_add(*bytes) <= budget
    });
    if fits && let Some((frame, bytes)) = self.parked.take() {
      self.push_back(frame, bytes);
    }
  }
}

/// A switch to the session's threads in progress, at a clean keyframe the
/// caller is sending: the one-thread decoder a fallback committed is being
/// drained into the queue — told the stream ended, every picture it holds
/// queued — and the keyframe waits, re-offered after each `MustDrain`,
/// until it has given its last; then it is closed and a decoder on the
/// session's threads opened (see
/// `CarrierVideoStreamDecoder::open_after_drain`).
#[derive(Clone, Copy, Debug)]
struct Restart {
  /// Whether the decoder serving has taken the end of the stream yet: it
  /// answers back pressure while pictures it made wait to be read.
  eof_sent: bool,
}

/// A software decoder and the callback state its codec context points
/// at.
///
/// The state carries the allocator judge's byte budget and the
/// `get_format` declination; it has to outlive the `AVCodecContext`
/// that references it, which is why it is a field here rather than a
/// value dropped at the end of `open_sw_decoder`.
///
/// `Deref` so that every call site keeps talking to the decoder and
/// only the construction changed — this pairing is a lifetime fact, not
/// a new abstraction.
pub(crate) struct SwDecoder {
  decoder: ffmpeg_next::decoder::Video,
  /// Whether the implementation opened is FFmpeg's own H.264 decoder,
  /// `h264` by name — the one whose output gate withholds every picture it
  /// has not recovered, which the withheld resync proof stands on
  /// ([`access::KeyframeRule::proof`]). Any other implementation of the
  /// codec — a hardware wrapper such as `h264_cuvid`, `h264_qsv`, a V4L2
  /// memory-to-memory or a MediaCodec one — keeps no such gate.
  native_h264: bool,
  /// Declared **after** the decoder: fields drop in declaration order,
  /// so the codec context is freed before the state it points at.
  _callback_state: Box<crate::ffi::CallbackState>,
  /// The test-only census of software decoders alive on this thread.
  #[cfg(test)]
  _live: live_sw::Guard,
}

/// A test-only census of the software decoders alive on this thread,
/// the most alive at once since [`reset_peak`](live_sw::reset_peak) —
/// what "one decoded history at a time" is measured by, since a decoder
/// holds its own reference frames and in-flight pictures — and the
/// packets software decoders have taken since [`reset_sent`](live_sw::reset_sent),
/// which is what "no packet decoded twice" is measured by.
#[cfg(test)]
pub(crate) mod live_sw {
  use core::cell::Cell;

  std::thread_local! {
    static LIVE: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    static SENT: Cell<usize> = const { Cell::new(0) };
    static EOFS: Cell<usize> = const { Cell::new(0) };
  }

  /// One end of the stream a software decoder was sent and answered —
  /// taken, or refused as a second one; back pressure, which takes
  /// nothing, aside.
  pub(crate) fn note_eof() {
    EOFS.with(|eofs| eofs.set(eofs.get() + 1));
  }

  /// Starts the count of ends over.
  pub(crate) fn reset_eofs() {
    EOFS.with(|eofs| eofs.set(0));
  }

  /// The ends software decoders were sent and answered since the last
  /// reset.
  pub(crate) fn eofs() -> usize {
    EOFS.with(Cell::get)
  }

  /// One packet a software decoder took.
  pub(crate) fn note_sent() {
    SENT.with(|sent| sent.set(sent.get() + 1));
  }

  /// Starts the packet count over.
  pub(crate) fn reset_sent() {
    SENT.with(|sent| sent.set(0));
  }

  /// The packets software decoders have taken since the last reset.
  pub(crate) fn sent() -> usize {
    SENT.with(Cell::get)
  }

  /// One live software decoder.
  pub(crate) struct Guard;

  impl Guard {
    pub(crate) fn new() -> Self {
      LIVE.with(|count| {
        let (live, peak) = count.get();
        count.set((live + 1, peak.max(live + 1)));
      });
      Self
    }
  }

  impl Drop for Guard {
    fn drop(&mut self) {
      LIVE.with(|count| {
        let (live, peak) = count.get();
        count.set((live.saturating_sub(1), peak));
      });
    }
  }

  /// Starts the peak over at the number alive now.
  pub(crate) fn reset_peak() {
    LIVE.with(|count| {
      let (live, _) = count.get();
      count.set((live, live));
    });
  }

  /// The most software decoders alive at once since the last reset.
  pub(crate) fn peak() -> usize {
    LIVE.with(|count| count.get().1)
  }
}

/// Test-only: a fault a replay meets on the drain after the decoder takes
/// the end of the stream — where FFmpeg reports an error for a packet it
/// decodes only once told the stream ended.
#[cfg(test)]
pub(crate) mod replay_fault {
  use core::cell::Cell;

  std::thread_local! {
    static AFTER_EOF: Cell<bool> = const { Cell::new(false) };
  }

  /// The next replay that feeds the end of the stream fails on the drain
  /// after it.
  pub(crate) fn arm() {
    AFTER_EOF.with(|armed| armed.set(true));
  }

  /// Whether the armed fault fires now; it fires once.
  pub(crate) fn fire() -> bool {
    AFTER_EOF.with(|armed| armed.replace(false))
  }
}

/// Test-only: answers a software decoder gives the end of the stream
/// before it is told — back pressure, or a refusal — scripted in order, the
/// decoder itself told once the script is spent.
#[cfg(test)]
pub(crate) mod eof_script {
  use std::{cell::RefCell, collections::VecDeque};

  std::thread_local! {
    static ANSWERS: RefCell<VecDeque<ffmpeg_next::Error>> = const { RefCell::new(VecDeque::new()) };
  }

  /// The next ends a software decoder is sent are answered with `answers`.
  pub(crate) fn push(answers: impl IntoIterator<Item = ffmpeg_next::Error>) {
    ANSWERS.with(|script| script.borrow_mut().extend(answers));
  }

  /// The scripted answer to this end, if one is left.
  pub(crate) fn next() -> Option<ffmpeg_next::Error> {
    ANSWERS.with(|script| script.borrow_mut().pop_front())
  }
}

impl SwDecoder {
  /// The callback state this decoder's codec context points at.
  ///
  /// Handed out as a raw pointer so an error closure can consult it
  /// while the decoder itself is mutably borrowed — every software send
  /// / receive / EOF failure on this road goes through
  /// [`crate::decoder::software_exit`] with it, so a frame the
  /// allocator judge refused surfaces named instead of as the `EINVAL`
  /// libavcodec also uses for corrupt input.
  ///
  /// `Deref` alone was not enough: it exposes the decoder and hides the
  /// state, so every call site kept wrapping raw and the budget refusal
  /// had no way out on the whole software road — including the replay
  /// and cold-fallback helpers, which drop the state when they finish.
  pub(crate) fn state(&self) -> *const crate::ffi::CallbackState {
    &*self._callback_state
  }

  /// Hands `packet` to this decoder. Every packet this module gives a
  /// software decoder goes through here, so the test census counts each
  /// one the decoder took.
  pub(crate) fn submit(&mut self, packet: &Packet) -> Result<(), ffmpeg_next::Error> {
    let taken = self.decoder.send_packet(packet);
    #[cfg(test)]
    if taken.is_ok() {
      live_sw::note_sent();
    }
    taken
  }

  /// Tells this decoder the stream ended. Every end this module gives a
  /// software decoder goes through here, so the test census counts each
  /// one the decoder answered.
  pub(crate) fn send_eof(&mut self) -> Result<(), ffmpeg_next::Error> {
    #[cfg(test)]
    if let Some(answer) = eof_script::next() {
      return Err(answer);
    }
    let told = self.decoder.send_eof();
    #[cfg(test)]
    if !matches!(told, Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN)
    {
      live_sw::note_eof();
    }
    told
  }
}

impl core::ops::Deref for SwDecoder {
  type Target = ffmpeg_next::decoder::Video;
  fn deref(&self) -> &Self::Target {
    &self.decoder
  }
}

impl core::ops::DerefMut for SwDecoder {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.decoder
  }
}

/// What the cold SW decoder is fed on a **post-commit** degrade transition,
/// named by the failure arm so the three shapes stay mutually exclusive (a
/// current packet and EOF are never forwarded together). The post-commit path
/// retains no replay frames, so this is the *only* thing handed to the new SW
/// decoder at fallback time. See [`FfmpegVideoStreamDecoder::degrade_to_sw`].
enum PostCommitInput<'a> {
  /// `send_packet` arm: forward this current packet — the one the HW decoder
  /// refused (so it was never in any replay set). If it is a keyframe it is the
  /// resync anchor.
  Packet(&'a Packet),
  /// `receive_frame` arm: a frame-time failure has no current packet to forward.
  FrameTime,
  /// `send_eof` arm: EOF was pending on the HW path; re-forward it to the cold
  /// SW so tail-delaying codecs don't hang.
  Eof,
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierVideoStreamDecoder<C> {
  /// Opens a decoder for the given codec parameters with the default
  /// HW backend probe order. If the HW probe can't open any backend,
  /// falls back to a software `ffmpeg::decoder::Video` immediately —
  /// `open` only returns `Err` when both paths fail.
  ///
  /// Subsequent mid-stream `AllBackendsFailed` from the HW path
  /// triggers the same SW fallback (with rescued packets replayed).
  ///
  /// `limits` bounds what one decoded frame may cost. It is taken here
  /// rather than through a builder because half of it —
  /// [`DecoderLimits::max_pixels`] — is written into every
  /// `AVCodecContext` this decoder opens, and a context's ceiling
  /// cannot be moved after `avcodec_open2`. That includes the contexts
  /// opened later, by a mid-stream fallback or a probe advance: the
  /// limits are retained for exactly that reason.
  pub(crate) fn open_impl(
    parameters: Parameters,
    time_base: Timebase,
    limits: DecoderLimits,
  ) -> Result<Self, Error> {
    Self::open_as_impl(parameters, time_base, limits, DecodePath::Auto)
  }

  /// [`Self::open_impl`], with the decode path chosen rather than
  /// probed. `DecodePath::Auto` is the constructor above, verbatim.
  pub(crate) fn open_as_impl(
    parameters: Parameters,
    time_base: Timebase,
    limits: DecoderLimits,
    path: DecodePath,
  ) -> Result<Self, Error> {
    // ffmpeg-next's `Parameters` carries an optional `owner: Rc<dyn Any>`
    // (when constructed from `stream.parameters()` it points back at
    // the demuxer's `AVStream`). Upstream marks the type `Send`
    // anyway, which is unsound the moment a non-`None` owner is in
    // play — moving such a value across threads moves the `Rc`. We
    // sidestep this by always storing a deep-cloned `Parameters`
    // (`avcodec_parameters_copy` produces an owner-free copy), so
    // the `FfmpegVideoStreamDecoder`'s `Send` reachability never
    // depends on the caller's owner discipline.
    //
    // Use `try_clone_parameters` instead of `Parameters::clone` —
    // ffmpeg-next's `clone` calls `Parameters::new()` which can
    // return a `Parameters` whose inner pointer is null on OOM
    // (`avcodec_parameters_alloc` returns null without indication);
    // the subsequent `avcodec_parameters_copy` against that null
    // destination is C UB. Our checked helper surfaces the OOM as
    // an error instead.
    let owned_parameters = try_clone_parameters(&parameters, limits.max_codec_parameter_bytes())?;
    let hw_scratch = Frame::empty()?;
    let sw_scratch = alloc_av_video_frame()?;
    let state = match path {
      DecodePath::Auto => match VideoDecoder::open_with_frame_limits_timed(
        try_clone_parameters(&owned_parameters, limits.max_codec_parameter_bytes())?,
        limits,
        time_base,
      ) {
        Ok(hw) => DecodeState::Hw(Box::new(hw)),
        Err(Error::AllBackendsFailed(_)) => {
          // Open-time HW exhaustion: no rescued packets (open didn't
          // see any). Just open SW directly from our owned copy.
          let sw = open_sw_decoder(&owned_parameters, limits, Some(time_base))?;
          DecodeState::Sw(sw)
        }
        Err(other) => return Err(other),
      },
      // **The named backend, and no probe order at all.** Nothing is
      // tried before it and nothing after it, which is what makes the
      // arm a pin: an open that fails is the answer, where `Auto` would
      // have read the same failure as a reason to look elsewhere.
      DecodePath::Hardware(backend) => {
        DecodeState::Hw(Box::new(VideoDecoder::open_with_limits_timed(
          try_clone_parameters(&owned_parameters, limits.max_codec_parameter_bytes())?,
          backend,
          limits,
          time_base,
        )?))
      }
      // The software decoder, opened on purpose rather than reached by
      // degrading. `DecodeState::Sw` is terminal, so this session has
      // nothing to keep it on its path but the shape of the state
      // machine itself.
      DecodePath::Software => {
        DecodeState::Sw(open_sw_decoder(&owned_parameters, limits, Some(time_base))?)
      }
    };
    Ok(Self {
      state,
      path,
      parameters: owned_parameters,
      hw_scratch,
      sw_scratch,
      sw_replay_frames: ReplayQueue::default(),
      restart: None,
      pending_history: VecDeque::new(),
      pending_eof: false,
      replay_output_pending: false,
      deferred_error: None,
      eof_sent: false,
      degraded_resync_pending: false,
      degraded_anchored: false,
      anchor_reorder: 0,
      anchor_resets: false,
      anchor_proof: access::Proof::ReorderBound,
      withheld_poisoned: false,
      anchor_definitive: false,
      anchor_recovery: None,
      outputs_since_anchor: 0,
      sw_output_settled: true,
      packets_before_anchor: 0,
      packets_unproven: 0,
      anchor_seen: false,
      time_base,
      limits,
      sw_threads_pending: false,
      seeked: false,
      one_thread_since: None,
      one_thread_warned: false,
      #[cfg(test)]
      fail_threaded_opens: false,
      #[cfg(test)]
      threaded_opens: 0,
      #[cfg(test)]
      reorder_override: None,
      #[cfg(test)]
      reorder_on_submit: None,
      #[cfg(test)]
      fail_after_submit: false,
      scratch_pending: false,
      _carrier: core::marker::PhantomData,
    })
  }

  /// Returns `true` when this decoder has fallen back to the software
  /// path. `false` while still on the HW probe (the initial state).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn is_software_impl(&self) -> bool {
    matches!(self.state, DecodeState::Sw(_) | DecodeState::SwClosed)
  }

  /// Returns `true` while the HW probe is still active.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn is_hardware_impl(&self) -> bool {
    matches!(self.state, DecodeState::Hw(_))
  }

  /// The thread count libavcodec settled on for the decoder serving
  /// now — read off its opened context, so it follows a mid-stream
  /// fallback the way [`Self::is_software_impl`] does: one between a
  /// fallback and the next keyframe, the session's own after it, and
  /// none while no software decoder is open.
  pub(crate) fn active_threads_impl(&self) -> Option<core::num::NonZeroU32> {
    match &self.state {
      DecodeState::Hw(hw) => hw.active_threads(),
      // SAFETY: `sw` is the live opened software decoder; the pointer is
      // read for one field and not kept.
      DecodeState::Sw(sw) => crate::decoder::opened_threads(unsafe { sw.as_ptr() }),
      DecodeState::SwClosed => None,
    }
  }

  /// Whether this session can currently honor a
  /// [`Self::request_scaled_output_impl`] request. See
  /// [`ScaledOutputCapability`] for the determinism trade a caller
  /// takes on by requesting one.
  ///
  /// **[`ScaledOutputCapability::Supported`] on exactly one road: a
  /// live VideoToolbox session on an Apple target.** There, a
  /// `VTPixelTransferSession` sits between the decoded hardware frame
  /// and `av_hwframe_transfer_data` and resizes the `CVPixelBuffer` on
  /// the GPU, so the fitted picture is what crosses to the CPU — see
  /// [`crate::vtscale`] for the design, and
  /// [mediadecode#55](https://github.com/findit-studio/mediadecode/issues/55)
  /// for the ruling that chose it. Everything else answers
  /// `Unsupported`, and each refusal has its own reason rather than a
  /// shared shrug:
  ///
  /// - **A session that has degraded to software.** The stage is the
  ///   hardware road's; this answer follows the session, so it flips to
  ///   `Unsupported` the moment a fallback commits, and a caller that
  ///   asks again learns it.
  /// - **The other hardware backends.** [`Backend::Vaapi`],
  ///   [`Backend::Cuda`] and [`Backend::D3d11va`] are wired in source
  ///   (`Backend::av_hwdevice_type`, `probe_order`) but cannot be
  ///   compiled, run or verified on a non-Linux, non-Windows host, and
  ///   each has a native scaling seam of its own that this crate has
  ///   not built: NVDEC/CUVID in-decode scaling
  ///   ([#56](https://github.com/findit-studio/mediadecode/issues/56)),
  ///   VAAPI VPP
  ///   ([#57](https://github.com/findit-studio/mediadecode/issues/57)),
  ///   the D3D11 Video Processor
  ///   ([#58](https://github.com/findit-studio/mediadecode/issues/58)).
  ///   Filed rather than fabricated.
  /// - **Software.** See [`Self::request_scaled_output_impl`] for the
  ///   software road's own, separate refusal.
  ///
  /// What the VideoToolbox road did **not** get is decode-time
  /// scaling, and the distinction is worth keeping: inter prediction
  /// needs full-resolution reference frames, so every road decodes full
  /// size internally. What this seam saves is the GPU→CPU crossing and
  /// the CPU frame at the end of it — roughly thirtyfold on a 4K stream
  /// fitted to a 512-class box. A caller-owned `VTDecompressionSession`
  /// would save the same crossing and no more, which is why it stays
  /// #55's standing future enhancement rather than this release's work.
  ///
  /// A pure query: calling it requests nothing and changes nothing
  /// about what [`Self::receive_frame`] delivers.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) fn scaled_output_capability_impl(&self) -> ScaledOutputCapability {
    match &self.state {
      DecodeState::Hw(hw) => hw.scaled_output_capability(),
      DecodeState::Sw(_) | DecodeState::SwClosed => ScaledOutputCapability::Unsupported,
    }
  }

  /// Requests that this session emit pictures at `size` from the next
  /// frame on. See [`Self::scaled_output_capability_impl`] for which
  /// road can honor it at all.
  ///
  /// # A parked frame refuses
  ///
  /// While [`Self::scratch_pending`] holds a decoded picture whose
  /// conversion did not commit, this refuses. That frame is already
  /// decided — the retry delivers it from the scratch without
  /// consulting the stage — so accepting a new size would promise an
  /// extent the very next frame cannot have. Drain it and ask again;
  /// the same escape every other seat guarded by that flag offers.
  ///
  /// The refusal carries the same meaning as every other: the session
  /// returns to full coded size, so any request already standing is
  /// withdrawn. The parked picture keeps the extent it was decoded at.
  ///
  /// # When a mid-stream request takes effect
  ///
  /// On the **next** picture [`Self::receive_frame`] produces. The
  /// stage is consulted per frame, on the way out of the hardware
  /// decoder and before the GPU→CPU download, so a request never
  /// reaches back to a picture already decoded and never waits longer
  /// than the one being decoded now.
  ///
  /// # The two refusals this seat mints itself
  ///
  /// Neither is an error, and each **returns the session to full coded
  /// size**, dropping any request already standing — what the trait
  /// says this answer means, and the only reading a caller can act on
  /// without risking a second resample of an already-fitted picture:
  ///
  /// - **A zero extent.** A zero-extent picture is not a smaller
  ///   picture.
  /// - **An upscale.** The stage exists to move fewer bytes across the
  ///   GPU→CPU bus; enlarging moves more, and inventing detail the
  ///   decoder did not produce is the caller's business rather than a
  ///   decode session's. An *equal* size is not an upscale: it is
  ///   accepted, and the stage simply has nothing to do.
  ///
  /// # The software road's refusal has its own, different shape
  ///
  /// Worth naming rather than folding into "no backend does this yet":
  /// FFmpeg's software decoders have no *general* decode-time scaling
  /// seam. The one option that comes close — `AVCodecContext.lowres`
  /// (the CLI's `-lowres`) — falls short on three separate counts, any
  /// one of which would disqualify it as this seam's software answer:
  ///
  /// 1. **Narrow codec coverage.** `lowres` is wired only into the
  ///    legacy MPEG-family decoders (MPEG-1/2/4 part 2, H.263) that
  ///    still carry the low-resolution IDCT machinery it depends on.
  ///    HEVC, AV1 and VP9 — the codecs a modern HDR pipeline actually
  ///    decodes — implement no `lowres` support at all.
  /// 2. **The one codec that is wired is broken.** `lowres` on H.264
  ///    (also nominally covered) has been non-functional for years —
  ///    the decoder does not honor it correctly — so even the "old
  ///    family" half of the promise does not hold across the board.
  /// 3. **It is not a resize, it is reduced reconstruction.** Where it
  ///    does work, `lowres` decodes at a coarser IDCT precision
  ///    (`1<<lowres`), skipping reconstruction detail rather than
  ///    decoding in full and scaling the result — later inter frames
  ///    drift from a reference the decoder itself degraded, which is a
  ///    different (and worse) contract than "the same picture, smaller".
  ///
  /// So the software road's answer is not "unimplemented" the way the
  /// other hardware backends' is — it is "full-size decode, then the
  /// fused conform walk downstream", by design, on every codec this
  /// crate decodes in software.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) fn request_scaled_output_impl(&mut self, size: (u32, u32)) -> ScaledOutputCapability {
    // **A parked frame is already decided, so it may not be
    // re-promised.** [`Self::scratch_pending`] means a picture came out
    // of the decoder and its conversion did not commit; the retry
    // delivers *that* frame from the scratch without going back through
    // the stage, so it will arrive at whatever extent it already has.
    // Accepting a new size here would answer `Supported` and then hand
    // the caller a frame the new request never touched — the exact
    // silent mismatch [`Self::scaled_output_capability_impl`]'s promise
    // exists to rule out. Refusing changes nothing, which is the
    // contract for a refusal, and the caller's escape is the one this
    // seat already documents everywhere else: drain the frame, then ask
    // again.
    if self.scratch_pending {
      tracing::debug!(
        requested_width = size.0,
        requested_height = size.1,
        "mediadecode-ffmpeg: scaled-output request refused while a decoded frame is parked; \
         the session returns to full size — drain it and ask again"
      );
      // **A refusal means the same thing here as anywhere else.**
      // Returning `Unsupported` while an earlier request stayed armed
      // would leave the caller resampling pictures this session went on
      // fitting — the very double-scale the word exists to prevent. So
      // the standing request goes, and with it what was built for it.
      // The parked picture keeps the extent it was decoded at, which is
      // the same rule an *acceptance* has always carried.
      if let DecodeState::Hw(hw) = &mut self.state {
        hw.cancel_scaled_output();
      }
      return ScaledOutputCapability::Unsupported;
    }
    match &mut self.state {
      DecodeState::Hw(hw) => hw.request_scaled_output(size),
      DecodeState::Sw(_) | DecodeState::SwClosed => ScaledOutputCapability::Unsupported,
    }
  }

  /// Borrow the inner [`VideoDecoder`] when this decoder is still on the
  /// real HW path. Returns `None` after the SW fallback has fired (or, in
  /// tests, when the HW seam is a fake rather than a real decoder).
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) fn hardware_inner_impl(&self) -> Option<&VideoDecoder> {
    match &self.state {
      DecodeState::Hw(hw) => hw.as_video_decoder(),
      DecodeState::Sw(_) | DecodeState::SwClosed => None,
    }
  }

  /// Returns the time base associated with the source stream.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub(crate) const fn time_base_impl(&self) -> Timebase {
    self.time_base
  }

  /// Whether this session may open a software decoder in answer to a
  /// hardware exhaustion.
  ///
  /// **The one place the pin is enforced**, consulted by all three
  /// roads that can meet [`Error::AllBackendsFailed`] — the two send
  /// arms and the receive arm. It is one predicate rather than three
  /// conditions because the pin is one promise: a session opened on
  /// [`DecodePath::Hardware`] ends on hardware or ends in an error, and
  /// a road that forgot to ask would break that promise silently,
  /// which is the failure mode a caller cannot see.
  ///
  /// [`DecodePath::Software`] answers `true` and it costs nothing:
  /// `DecodeState::Sw` is terminal, so no hardware exhaustion can
  /// reach a road that asks. Answering for it by state rather than by
  /// pin would make the predicate say something it does not mean.
  #[cfg_attr(not(tarpaulin), inline(always))]
  const fn may_open_software(&self) -> bool {
    !matches!(self.path, DecodePath::Hardware(_))
  }

  /// Internal: **probe-era** transition from HW to SW. Replays the rescued
  /// packets (the inner decoder's buffered history, already accepted by the HW
  /// probe but not yet decoded) through the new SW decoder so the stream resumes
  /// seamlessly. No frame was delivered on the HW path yet, so replaying the
  /// history is lossless.
  ///
  /// Only the probe-era branches drive this. The **post-commit** path does
  /// *not* — it retains and reconstructs zero frames, opening SW cold via
  /// [`Self::degrade_to_sw`] and resyncing at the next keyframe instead of
  /// replaying. (That is why this method's replay/drain machinery — and the
  /// finding that the in-transaction drain doesn't cover later frame
  /// *conversion* — cannot affect the post-commit path: it never produces a
  /// post-commit replay frame to convert.)
  ///
  /// **Transactional**: drained replay frames accumulate in a local
  /// queue; we only commit them to `self.sw_replay_frames` and switch
  /// `self.state` to `Sw` after the replay (and EOF re-forwarding, if
  /// needed) succeed. On failure, the SW decoder, the local frame
  /// queue, and (where reachable) any consumed packets are dropped —
  /// `self` is left in its prior state.
  ///
  /// **Within the queue's budget.** The pictures the replay makes wait in
  /// the session's queue, under its byte budget
  /// ([`DecoderLimits::max_replay_bytes`]) and its
  /// [`SW_REPLAY_FRAME_CAP`]. A history whose pictures reach either before
  /// it is all fed is committed there: the packets it has not fed are kept
  /// ([`Self::pending_history`]) and fed under the budget, as the caller
  /// drains, before anything it sends next — so nothing is dropped. A
  /// history fed whole whose last drain stopped at the budget leaves the
  /// decoder holding pictures too ([`Self::replay_output_pending`]): they
  /// are drained into the queue before anything sent next is taken. The
  /// transaction covers what was fed before the commit; a packet fed after
  /// it that fails is an ordinary decode error, reported on the drain, its
  /// packet consumed.
  ///
  /// **EOF-aware**: when EOF was already accepted on the HW path
  /// (`self.eof_sent`), the new SW decoder also receives `send_eof()`
  /// after replay. Without this, codecs that delay tail frames hang
  /// forever in the drain phase.
  ///
  /// **EAGAIN-aware**: if SW's `send_packet` returns EAGAIN during
  /// replay, drain produced frames into the local queue and retry.
  ///
  /// `eof_pending` is passed as a **local** argument rather than read from
  /// `self.eof_sent`: callers must not mutate `self.eof_sent` before this
  /// transaction commits (see [`VideoStreamDecoder::send_eof`]), so the
  /// in-transaction SW EOF re-forward keys off the local flag and `self`'s
  /// EOF state is updated only after a clean commit.
  fn fall_back_to_sw(
    &mut self,
    unconsumed_packets: std::vec::Vec<ffmpeg_next::Packet>,
    eof_pending: bool,
  ) -> Result<(), Error> {
    tracing::info!(
      packets_replayed = unconsumed_packets.len(),
      eof_pending,
      "mediadecode-ffmpeg: HW probe exhausted, falling back to software decode",
    );
    // Wrap the internal worker so any failure path returns the
    // rescued packets to the caller via `Error::FallbackFailed`.
    // Without this, non-seekable streams (live feeds, pipes) would
    // lose every compressed byte the HW path had consumed when a
    // fallback transition fails partway.
    match self.fall_back_to_sw_inner(&unconsumed_packets, eof_pending) {
      Ok(fed) => {
        // What the budget left unfed is replayed as the caller drains.
        self
          .pending_history
          .extend(unconsumed_packets.into_iter().skip(fed));
        Ok(())
      }
      Err(source) => Err(Error::FallbackFailed(FallbackFailed::new(
        Box::new(source),
        unconsumed_packets,
      ))),
    }
  }

  /// Worker for [`Self::fall_back_to_sw`]. Returns the rescued packets
  /// untouched on the borrowed slice; the wrapper takes ownership of
  /// them and surfaces them in `FallbackFailed` if this returns Err. On
  /// success it answers how many of them the decoder took before the
  /// queue reached its budget; the wrapper keeps the rest.
  ///
  /// # The history is decoded once, on one thread, and that decoder is
  /// committed
  ///
  /// The transaction below rests on one property: every error a replayed
  /// packet can cause surfaces before the commit. A one-thread decoder
  /// has it — the final drain decodes everything it was handed. A
  /// frame-threaded decoder does not: it keeps up to one packet per
  /// thread in flight and answers "needs input" until it has that many,
  /// so a corrupt packet in a short history would surface only after the
  /// commit, as a plain decode error with the rescued packets gone.
  ///
  /// So the history is replayed into a decoder on one thread whatever
  /// the session's [`Threads`](crate::Threads), and the decoder that
  /// proved it is the one committed, with the pictures it produced. When
  /// the session asks for more threads it gets them back at the next
  /// keyframe — see [`Self::open_after_drain`] — and until then it
  /// decodes on this one. Nothing is decoded twice and no second decoder
  /// is opened beside this one.
  fn fall_back_to_sw_inner(
    &mut self,
    unconsumed_packets: &[ffmpeg_next::Packet],
    eof_pending: bool,
  ) -> Result<usize, Error> {
    let one_thread = self.limits.with_threads(crate::Threads::Single);
    let mut sw = open_sw_decoder(&self.parameters, one_thread, Some(self.time_base))?;
    let mut local_replay = ReplayQueue::default();
    let mut progress = Replay::default();
    let drained = replay_history(
      &mut sw,
      unconsumed_packets,
      eof_pending,
      &mut local_replay,
      self.limits.max_replay_bytes(),
      &mut progress,
    )?;
    // Commit: only after replay, any EOF forwarding, AND the final drain
    // succeeded — or the queue reached its budget first — do we move the
    // new SW decoder and queue into `self`.
    self.sw_replay_frames.append(&mut local_replay);
    self.state = DecodeState::Sw(sw);
    self.sw_threads_pending = self.session_threads_run();
    self.pending_eof = eof_pending && !progress.eof_sent;
    // A drain the budget stopped: the decoder may hold more of the replay's
    // pictures, and they come before anything the caller sends next.
    self.replay_output_pending = drained == Drained::Full;
    Ok(progress.fed)
  }

  /// Whether a probe-era fallback's replay left work before anything the
  /// caller sends next: packets or the end of the stream to feed
  /// ([`Self::replay_owes_input`]), or pictures the decoder may still hold
  /// that its last drain stopped short of ([`Self::replay_output_pending`]).
  fn has_pending_replay(&self) -> bool {
    self.replay_owes_input() || self.replay_output_pending
  }

  /// Whether that replay still owes the decoder packets, or the end of the
  /// stream.
  fn replay_owes_input(&self) -> bool {
    !self.pending_history.is_empty() || self.pending_eof
  }

  /// **The replay a fallback left, resumed**: the packets it has not fed,
  /// then the end of the stream if it owes it, fed to the decoder serving
  /// with every picture it makes queued — the parked picture first, once the
  /// queue can take it, and every picture the decoder still holds — stopping
  /// again where the queue reaches its budget. Answers whether nothing is
  /// left: nothing to feed, and a drain that found the decoder with no
  /// picture ready. A packet that fails is consumed with its error, so a
  /// retry never offers it twice.
  ///
  /// The end of the stream, once the decoder takes it, is the session's at
  /// once ([`Self::eof_sent`]) — before the drain after it, whose error the
  /// caller defers — so it is never sent to the decoder again.
  fn replay_pending(&mut self) -> Result<bool, Error> {
    if !self.has_pending_replay() {
      return Ok(true);
    }
    let DecodeState::Sw(sw) = &mut self.state else {
      return Ok(true);
    };
    let mut progress = Replay::default();
    let replayed = replay_history(
      sw,
      self.pending_history.make_contiguous(),
      self.pending_eof,
      &mut self.sw_replay_frames,
      self.limits.max_replay_bytes(),
      &mut progress,
    );
    self.pending_history.drain(..progress.fed);
    if progress.eof_sent {
      self.pending_eof = false;
      self.eof_sent = true;
    }
    if let Ok(drained) = replayed {
      self.replay_output_pending = drained == Drained::Full;
    }
    replayed?;
    Ok(!self.has_pending_replay())
  }

  /// **The session's threads, back at a clean keyframe**, once the
  /// one-thread decoder a fallback committed has given its last picture to
  /// the queue ([`Self::drain_for_restart`]): that decoder is closed, then a
  /// decoder on the session's [`Threads`](crate::Threads) is opened, which
  /// the keyframe the caller is sending is sent to next. Also where a
  /// session with no decoder open ([`DecodeState::SwClosed`]) opens one.
  ///
  /// # Why at a clean keyframe, and why nothing has to be proved
  ///
  /// [`Self::send_on_software`] restarts only at a clean random access
  /// point ([`access`]) — a keyframe nothing after it references past — or
  /// at the first keyframe after a seek, which has discarded what led it.
  /// So the new decoder starts clean there: it is fed every packet from the
  /// keyframe on, none of them twice, it decodes every picture the
  /// one-thread decoder would have, and a packet it cannot decode is an
  /// ordinary decode error with that packet still the caller's — there is
  /// no transaction to keep, because there is no history to hand over.
  ///
  /// # One decoder at a time
  ///
  /// The old decoder is dropped before the new one is opened, so at no
  /// instant is more than one software decoder open — the decoded history
  /// is the open decoder's alone, which is why no budget spans two.
  ///
  /// # When an open fails
  ///
  /// The session's threads are attempted **once**: a decoder on the
  /// session's threads that will not open is not retried, and a decoder on
  /// one thread is opened instead, for the rest of the session —
  /// [`active_threads`](Self::active_threads_impl) answers one. Should that
  /// fail too, the session is left with no decoder open
  /// ([`DecodeState::SwClosed`]), the error is returned — the packet was
  /// not taken — and the next send opens one again.
  fn open_after_drain(&mut self) -> Result<(), Error> {
    self.state = DecodeState::SwClosed;
    let one_thread = self.limits.with_threads(crate::Threads::Single);
    let sw = if self.sw_threads_pending {
      self.sw_threads_pending = false;
      match self.open_on_session_threads() {
        Ok(sw) => sw,
        Err(error) => {
          tracing::warn!(
            %error,
            "mediadecode-ffmpeg: the software decoder could not be opened on the session's \
             threads at a keyframe; the session stays on one thread for good",
          );
          open_sw_decoder(&self.parameters, one_thread, Some(self.time_base))?
        }
      }
    } else {
      open_sw_decoder(&self.parameters, one_thread, Some(self.time_base))?
    };
    self.state = DecodeState::Sw(sw);
    self.sw_output_settled = true;
    Ok(())
  }

  /// **A restart's drain, advanced**: the decoder serving is told the
  /// stream ended — once it has room for that — and every picture it holds
  /// is queued, until it has given its last or the queue reaches its budget
  /// (see `drain_into`). Answers whether it has given its last; if not, the
  /// caller drains and sends the keyframe again, and this resumes where it
  /// stopped. Nothing is dropped.
  fn drain_for_restart(&mut self) -> Result<bool, Error> {
    let Some(restart) = self.restart.as_mut() else {
      return Ok(true);
    };
    let DecodeState::Sw(sw) = &mut self.state else {
      return Ok(true);
    };
    let state = sw.state();
    let budget = self.limits.max_replay_bytes();
    let mut attempts: u32 = 0;
    // The end is taken only when the decoder takes it, or answers that it
    // already has (`AVERROR_EOF`: it is draining). Back pressure that never
    // lifts, and a refusal, leave it owed — the error goes to the caller's
    // drain, and the next send offers the end again. This is the decoder
    // being switched away from: the session's own end is not touched here.
    while !restart.eof_sent {
      match sw.send_eof() {
        Ok(()) | Err(ffmpeg_next::Error::Eof) => restart.eof_sent = true,
        Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
          if drain_into(sw, state, &mut self.sw_replay_frames, budget)? == Drained::Full {
            return Ok(false);
          }
          attempts += 1;
          if attempts > 16 {
            return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
              errno: ffmpeg_next::error::EAGAIN,
            }));
          }
        }
        Err(other) => return Err(crate::decoder::software_exit(state, other)),
      }
    }
    Ok(drain_into(sw, state, &mut self.sw_replay_frames, budget)? == Drained::Empty)
  }

  /// **A drained switch, completed**: the drained decoder is closed and one
  /// on the session's threads opened ([`Self::open_after_drain`]).
  fn finish_restart(&mut self) -> Result<(), Error> {
    if self.restart.take().is_none() {
      return Ok(());
    }
    self.open_after_drain()
  }

  /// The one open a switch attempts on the session's own threads. In tests
  /// it can be made to fail, and is counted.
  fn open_on_session_threads(&mut self) -> Result<SwDecoder, Error> {
    #[cfg(test)]
    {
      self.threaded_opens += 1;
      if self.fail_threaded_opens {
        return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
          errno: libc::ENOMEM,
        }));
      }
    }
    open_sw_decoder(&self.parameters, self.limits, Some(self.time_base))
  }

  /// The software road's send: what a fallback's replay left fed first,
  /// the decoder serving switched to the session's threads when this packet
  /// is a clean random access point it was waiting for (or a decoder opened
  /// when none is), then the packet handed to the decoder serving now.
  ///
  /// **The switch.** Only at a clean random access point ([`access`]) or at
  /// the first keyframe after a seek, and never across an open post-commit
  /// gap: the one-thread decoder a fallback committed is drained
  /// ([`Self::drain_for_restart`]) — the end of the stream sent, every
  /// picture it still holds queued, so they come out first and in order —
  /// then closed, and one on the session's threads opened
  /// ([`Self::open_after_drain`]). The drain stops where the queue reaches
  /// its budget and answers `MustDrain`; the caller drains and sends the
  /// keyframe again, and the drain resumes — nothing is dropped. A decoder
  /// opened at an open-GOP keyframe would drop or conceal the leading
  /// pictures the closed decoder could decode, and the session never trades
  /// a picture for threads.
  ///
  /// **The resync anchor.** Across an open post-commit gap, a key-flagged
  /// packet the decoder takes anchors the resync when its first picture is
  /// proved a random-access one ([`Self::anchor`]) — nothing is drained or
  /// reset for it. It is fed once the decoder's output is settled
  /// ([`Self::sw_output_settled`]); until then the send answers `MustDrain`,
  /// so the pictures from before the anchor that can still come out are
  /// the ones the reorder buffer holds. The stream's proof reads the
  /// pictures out after it ([`Self::resync_on_output`]): for H.264 the
  /// first closes the gap, FFmpeg withholding every picture it has not
  /// recovered; for the other codecs the bound counts past the buffer, its
  /// depth read before the packet is submitted — its parameters can lower it
  /// while those pictures still wait — and the largest read from there on
  /// kept ([`Self::anchor_reorder`]). A packet the decoder reports failed
  /// leaves the output unsettled — the submission is not transactional — so
  /// the next anchor waits for a drain too.
  ///
  /// A decode error met while feeding what was pending is kept for the
  /// drain ([`Self::deferred_error`]), and this send answers `MustDrain`
  /// with the packet still the caller's.
  fn send_on_software(
    &mut self,
    pkt: &Packet,
    phase: crate::decoder::SessionPhase,
  ) -> Result<Sent, VideoDecodeError> {
    // An error the drain has not reported yet comes before anything more.
    if self.deferred_error.is_some() {
      return Ok(Sent::MustDrain);
    }
    // A fallback's replay left packets to feed: they come before this one.
    let replayed = self.replay_pending();
    // It fed the end of the stream a `send_eof` left owed: the session has
    // ended, and this packet comes after it. An error the replay met waits
    // for the drain.
    if self.eof_sent {
      if let Err(error) = replayed {
        self.deferred_error = Some(error);
      }
      return Err(Self::after_eof());
    }
    match replayed {
      Ok(true) => {}
      Ok(false) => return Ok(Sent::MustDrain),
      Err(error) => {
        self.deferred_error = Some(error);
        return Ok(Sent::MustDrain);
      }
    }
    if self.restart.is_none()
      && matches!(self.state, DecodeState::Sw(_))
      && self.sw_threads_pending
      && !self.degraded_resync_pending
      && self.switch_point(pkt)
    {
      // One queue, one budget: a switch drains into the queue, so it begins
      // once the caller has emptied it. The packet is still the caller's,
      // to re-offer.
      if !self.sw_replay_frames.is_empty() {
        return Ok(Sent::MustDrain);
      }
      self.restart = Some(Restart { eof_sent: false });
    }
    if self.restart.is_some() {
      match self.drain_for_restart() {
        Ok(true) => {}
        Ok(false) => return Ok(Sent::MustDrain),
        Err(error) => {
          self.deferred_error = Some(error);
          return Ok(Sent::MustDrain);
        }
      }
      self.finish_restart().map_err(VideoDecodeError::Decode)?;
    }
    if matches!(self.state, DecodeState::SwClosed) {
      // No decoder open: one is opened, on one thread for good.
      self.open_after_drain().map_err(VideoDecodeError::Decode)?;
    }
    // A key-flagged packet across an open gap whose first picture is proved
    // a random-access one anchors the resync, fed once the decoder holds no
    // picture the caller has not taken.
    // Across an open gap: the first anchor, or a definitive one superseding
    // an anchor that is not.
    let anchoring = if self.degraded_resync_pending {
      self.anchor(pkt).filter(|anchor| {
        !self.degraded_anchored || (anchor.definitive() && !self.anchor_definitive)
      })
    } else {
      None
    };
    if anchoring.is_some() && !self.sw_output_settled {
      return Ok(Sent::MustDrain);
    }
    // The reorder depth before the anchor is submitted: the parameters it
    // activates can lower it while the pictures from before it still wait.
    let reorder_before = if anchoring.is_some() {
      self.live_reorder()
    } else {
      0
    };
    if pkt.is_key() {
      self.seeked = false;
    }
    if self.sw_threads_pending {
      self.note_one_thread(pkt);
    }
    let DecodeState::Sw(sw) = &mut self.state else {
      // `open_after_drain` leaves a decoder open or returns, and no other
      // road reaches here without one.
      return Err(VideoDecodeError::Decode(Error::Ffmpeg(
        ffmpeg_next::Error::Bug,
      )));
    };
    let st = sw.state();
    let submitted = sw.submit(pkt);
    #[cfg(test)]
    let submitted = match submitted {
      Ok(()) if core::mem::take(&mut self.fail_after_submit) => {
        Err(ffmpeg_next::Error::InvalidData)
      }
      other => other,
    };
    if let Err(e) = submitted {
      // Funnel, then gate. **Nothing below runs on back pressure**,
      // which is the point of returning here rather than falling
      // through: a packet libavcodec did not take must not be
      // counted across the resync gap or taken for an anchor, or a
      // caller's honest re-offer would double-count it.
      return match crate::decoder::software_send(st, e, phase) {
        Ok(sent) => Ok(sent),
        Err(error) => {
          // FFmpeg's submission is not transactional: a packet it reports
          // failed may have left pictures ready. The output is unsettled
          // until a drain answers "needs input" or the end, and no packet
          // anchors before that.
          self.sw_output_settled = false;
          // A packet that failed after the anchor: what follows it may
          // reference it, so the anchor is in doubt.
          self.unanchor();
          Err(VideoDecodeError::Decode(error))
        }
      };
    }
    #[cfg(test)]
    if let Some(depth) = self.reorder_on_submit.take() {
      self.reorder_override = Some(depth);
    }
    self.sw_output_settled = false;
    if let Some(anchor) = anchoring {
      self.anchor_resync(reorder_before, anchor);
    } else {
      // The depth after every packet taken since the anchor, kept if larger.
      self.observe_reorder();
      // Count packets crossing an unresolved post-commit resync gap so the
      // escalation at EOF can report how much was lost.
      self.count_degraded_packet();
    }
    Ok(Sent::Accepted)
  }

  /// Whether a decoder opened on the session's [`Threads`](crate::Threads)
  /// would decode on more than one: the session asks for more than one, and
  /// the codec threads — by frames, by slices, or on its own
  /// (`AV_CODEC_CAP_OTHER_THREADS`). A codec that cannot thread decodes on
  /// one whatever is asked, so a switch to the session's threads would only
  /// drain and reopen it: none is scheduled.
  fn session_threads_run(&self) -> bool {
    use ffmpeg_next::codec::Capabilities;
    self.limits.threads().thread_count() != 1
      && sw_codec(&self.parameters).is_ok_and(|codec| {
        codec.capabilities().intersects(
          Capabilities::FRAME_THREADS | Capabilities::SLICE_THREADS | Capabilities::OTHER_THREADS,
        )
      })
  }

  /// What `pkt` is as a post-commit resync anchor: a key-flagged packet the
  /// bitstream proves a random-access point, where this crate can read it —
  /// see [`access::KeyframeRule::anchor`]. A stale key flag, or a picture
  /// before the random-access one, anchors nothing.
  fn anchor(&self, pkt: &Packet) -> Option<access::Anchor> {
    let rule = self.keyframe_rule();
    if pkt.is_key() || rule.every_packet() {
      pkt.data().and_then(|data| rule.anchor(data))
    } else {
      None
    }
  }

  /// Whether `pkt` is a point to switch the software decoder at: a keyframe
  /// that is a clean random access point ([`Self::clean_keyframe`]), or the
  /// first keyframe after a seek, which has discarded what led it. For a
  /// codec that codes every picture alone, every packet is a keyframe here,
  /// its flag or not.
  fn switch_point(&self, pkt: &Packet) -> bool {
    (pkt.is_key() || self.keyframe_rule().every_packet())
      && (self.seeked || self.clean_keyframe(pkt))
  }

  /// The keyframe rule for this session's stream, read off its codec
  /// parameters: the codec, and how its extradata packs NAL units.
  fn keyframe_rule(&self) -> access::KeyframeRule {
    // SAFETY: the owned, deep-copied parameters' pointer, only read.
    let raw = unsafe { self.parameters.as_ptr() };
    if raw.is_null() {
      return access::KeyframeRule::Reordering;
    }
    // SAFETY: `raw` is that live pointer (checked non-null above).
    // `codec_id` is read as the 32-bit integer the field holds, never
    // formed into a bindgen enum; `extradata` is read for `extradata_size`
    // bytes, which FFmpeg allocated together.
    unsafe {
      let codec_id = core::ptr::read(core::ptr::addr_of!((*raw).codec_id) as *const i32);
      let data = (*raw).extradata;
      let size = usize::try_from((*raw).extradata_size).unwrap_or(0);
      let extradata = if data.is_null() || size == 0 {
        &[][..]
      } else {
        core::slice::from_raw_parts(data, size)
      };
      access::KeyframeRule::of(codec_id, extradata)
    }
  }

  /// Whether `pkt`, a keyframe, is a clean random access point for this
  /// stream — see [`access::KeyframeRule::is_clean`]: proved by its
  /// bitstream, never inferred from the decoder it would replace, whose
  /// `has_b_frames` FFmpeg raises only when it meets reordering — which an
  /// open GOP can introduce at this very keyframe.
  fn clean_keyframe(&self, pkt: &Packet) -> bool {
    pkt
      .data()
      .is_some_and(|data| self.keyframe_rule().is_clean(data))
  }

  /// **A fallback that outlives a minute on one thread says so, once.** A
  /// stream whose keyframes are all open — x265's default CRA cadence —
  /// never offers a clean point to switch at, so after a fallback it stays
  /// on the one-thread decoder until the caller seeks. Losing no picture is
  /// the rule; this warning is how a deployment learns what it costs: the
  /// first time a minute of stream time has gone by on one thread, it
  /// names the codec and why its keyframes are not clean.
  fn note_one_thread(&mut self, pkt: &Packet) {
    // A codec that codes every picture alone switches at its next packet.
    if self.one_thread_warned || self.keyframe_rule().every_packet() {
      return;
    }
    let Some(at) = pkt.pts().or_else(|| pkt.dts()) else {
      return;
    };
    let since = *self.one_thread_since.get_or_insert(at);
    let seconds = at.saturating_sub(since) as f64 * f64::from(self.time_base.num())
      / f64::from(self.time_base.den().get());
    if seconds < 60.0 {
      return;
    }
    self.one_thread_warned = true;
    let codec = crate::decoder::find_decoder(&self.parameters)
      .map(|codec| codec.name().to_owned())
      .unwrap_or_default();
    tracing::warn!(
      codec,
      reason = self.keyframe_rule().reason(),
      "mediadecode-ffmpeg: a software fallback has decoded a minute of stream on one thread \
       with no clean random access point to return to the session's threads at; it stays on \
       one thread until the caller seeks",
    );
  }

  /// **Post-commit** degrade-and-continue transition: open the SW decoder
  /// **cold** and forward only the failure-arm's input, retaining and
  /// reconstructing **zero** frames. This is the whole post-commit path: open
  /// SW, forward the current packet (or EOF), degrade-track — nothing is drained
  /// into `sw_replay_frames`, so there is no replayed frame to convert later and
  /// no terminal-drain transaction to reason about. SW naturally produces no
  /// frame until the next keyframe arrives across the gap, then decodes normally;
  /// the failure-point→next-keyframe span is the accepted, logged drop.
  ///
  /// **Transactional (SW-open only)**: `self.state` flips to `Sw` *only after*
  /// `open_sw_decoder` and the input forward succeed. On any failure the new SW
  /// decoder is dropped and the decoder is left on its prior HW state, the error
  /// surfaced as [`Error::FallbackFailed`] (with an empty rescue set — a
  /// post-commit failure never carries unconsumed packets). With no replay-frame
  /// retention there is nothing else to roll back.
  ///
  /// On a clean commit it enters degraded-resync mode (see
  /// [`Self::enter_degraded_resync`]); if the forwarded current packet is
  /// key-flagged, the resync is anchored at it at once.
  ///
  /// # `eof_pending`
  ///
  /// Whether the session's end-of-stream has already been **committed**,
  /// and so must be re-forwarded into the cold decoder. Carried as a
  /// local argument for the same two reasons the probe-era road carries
  /// it (see [`Self::fall_back_to_sw`]): it is read from `eof_sent`
  /// before anything is mutated, so a fallback that fails leaves no
  /// half-truth behind — and one question deserves one mechanism on
  /// both fallback roads.
  ///
  /// It is **not** expressed by selecting [`PostCommitInput::Eof`],
  /// even though that arm forwards the same call. That enum is named by
  /// the *failure arm* — which road raised the exhaustion — and the
  /// `warn!` each site emits says so; borrowing the EOF arm for a
  /// frame-time failure would make it lie about where the failure came
  /// from.
  fn degrade_to_sw(&mut self, input: PostCommitInput<'_>, eof_pending: bool) -> Result<(), Error> {
    match self.degrade_to_sw_inner(input, eof_pending) {
      Ok(()) => Ok(()),
      // **A budget refusal is not a fallback failure.** It travels
      // unwrapped, and the spelling was chosen rather than inherited:
      //
      // * `FallbackFailed` means the fallback *machinery* could not
      //   complete, and its contract is to hand back the unconsumed
      //   packets so a caller can re-drive them. On this road that set
      //   is empty by construction — the probe buffer is gone and no
      //   replay frames are retained — so the envelope carries no
      //   recovery affordance at all, only a label.
      // * And the label is the wrong one. Re-driving is the natural
      //   response to a fallback failure, and re-driving a budget
      //   refusal under the same limits refuses identically. Naming it
      //   a fallback failure invites an action that cannot succeed,
      //   while `FrameBudgetExceeded` names the one that can: raise
      //   the ceiling, or accept the refusal.
      //
      // So it keeps the same spelling here as on every other road. One
      // fact, one name.
      Err(budget @ Error::FrameBudgetExceeded(_)) => Err(budget),
      // Everything else really is the machinery failing, and keeps the
      // envelope — empty rescue set and all, which is what a
      // post-commit failure has to hand back.
      Err(source) => Err(Error::FallbackFailed(FallbackFailed::new(
        Box::new(source),
        std::vec::Vec::new(),
      ))),
    }
  }

  /// Worker for [`Self::degrade_to_sw`]. Opens SW cold, forwards the arm's input,
  /// and on success commits + enters degraded-resync mode. Returns `Err` (and
  /// commits nothing) if SW cannot open or the forward fails.
  ///
  /// The forward is decided on one thread, for the reason
  /// [`Self::fall_back_to_sw_inner`] gives: a frame-threaded decoder takes a
  /// packet without decoding it, so a forward that fails would be committed
  /// and fail afterwards instead of rolling back. The one-thread decoder that
  /// took the input is the one committed; the session returns to its
  /// [`Threads`](crate::Threads) at the next keyframe, which is also where
  /// this cold decoder resyncs — see [`Self::send_on_software`].
  fn degrade_to_sw_inner(
    &mut self,
    input: PostCommitInput<'_>,
    eof_pending: bool,
  ) -> Result<(), Error> {
    // The invariant [`PostCommitInput`] documents, stated where it can
    // be checked: a current packet and an end-of-stream are never
    // forwarded together. The send road cannot violate it — its own
    // gate refuses every packet once `eof_sent` is committed — so this
    // records the coupling rather than defending against it.
    debug_assert!(
      !(matches!(input, PostCommitInput::Packet(_)) && eof_pending),
      "a current packet and a committed EOF must never be forwarded together",
    );
    let one_thread = self.limits.with_threads(crate::Threads::Single);
    let mut sw = open_sw_decoder(&self.parameters, one_thread, Some(self.time_base))?;
    // Captured before the decoder is borrowed for the forward, and
    // before it can be dropped on the error road: this temporary
    // decoder owns the callback state, so a `judge_buffer` refusal
    // recorded during either forward below dies with it unless the
    // reason is collected here. That was the last software road still
    // wrapping libavcodec's `EINVAL` raw.
    let state = sw.state();
    let forwarded = match input {
      // The HW decoder REFUSED this packet, so it was never decoded; forward
      // it to the cold SW. A failure here surfaces (it is not silently
      // dropped) and rolls back to HW.
      PostCommitInput::Packet(pkt) => Some(pkt),
      // Neither of these forwards a packet; the end-of-stream below is
      // the only thing they can hand the cold decoder.
      PostCommitInput::FrameTime | PostCommitInput::Eof => None,
    };
    // The cold decoder's reorder depth before the forward, for an anchor.
    let reorder_before = reorder_depth(&sw);
    if let Some(pkt) = forwarded {
      sw.submit(pkt)
        .map_err(|e| crate::decoder::software_exit(state, e))?;
    }
    // **The end of the stream is re-forwarded here, on every arm that
    // has one, and that is the fix rather than an extra.**
    //
    // The cold decoder knows nothing: it was opened a moment ago, from
    // codec parameters alone. If the session had already been told the
    // stream ended and this new decoder is not, it answers `EAGAIN` to
    // every drain — which reaches the caller as
    // [`Received::NeedsInput`], an instruction to send another packet.
    // On a session whose end is committed there is no legal way to obey
    // that: both send gates refuse. The caller loops, or quietly
    // accepts a truncated tail, until `flush`.
    //
    // It used to be reachable only through the `Eof` failure arm, so
    // the frame-time road — a post-commit exhaustion raised *while
    // draining*, after EOF was accepted — opened cold and stayed cold.
    // A cold decoder has no buffered output, so this cannot answer
    // `EAGAIN` itself.
    if eof_pending {
      sw.send_eof()
        .map_err(|e| crate::decoder::software_exit(state, e))?;
    }
    // Commit: only after a clean open + forward. The session's own
    // threads come back at the next keyframe.
    self.state = DecodeState::Sw(sw);
    self.sw_threads_pending = self.session_threads_run();
    self.enter_degraded_resync();
    self.sw_output_settled = forwarded.is_none() && !eof_pending;
    if let Some(pkt) = forwarded {
      if pkt.is_key() {
        self.seeked = false;
      }
      if let Some(anchor) = self.anchor(pkt) {
        // The refused current packet is itself the resync anchor.
        self.anchor_resync(reorder_before, anchor);
      } else {
        self.count_degraded_packet();
      }
    }
    Ok(())
  }

  /// Enter post-commit degraded mode after a post-commit fallback commits: the
  /// SW decoder opened cold and the span up to the next keyframe is being
  /// dropped. We hold this mode until a key-flagged packet anchors the resync
  /// ([`Self::anchor_resync`]) and the stream's proof places a delivered
  /// picture at or after it ([`Self::resync_on_output`]), and the EOF
  /// escalation in [`VideoStreamDecoder::receive_frame`] reports it
  /// otherwise. Called only on the post-commit path, only after a clean
  /// commit. Resets the anchor and the gap counter.
  #[inline]
  fn enter_degraded_resync(&mut self) {
    self.degraded_resync_pending = true;
    self.degraded_anchored = false;
    self.anchor_reorder = 0;
    self.anchor_resets = false;
    self.anchor_proof = access::Proof::ReorderBound;
    self.withheld_poisoned = false;
    self.anchor_definitive = false;
    self.anchor_recovery = None;
    self.outputs_since_anchor = 0;
    self.packets_before_anchor = 0;
    self.packets_unproven = 0;
    self.anchor_seen = false;
  }

  /// **The resync anchored** at the key-flagged packet the decoder serving
  /// just took across the open gap: the count of pictures out since starts,
  /// against the stream's proof ([`Self::check_resync_proof`]) — for H.264
  /// the first picture out, for the other codecs the reorder buffer's depth,
  /// `reorder_before`, read before the packet was submitted, or the largest
  /// read since ([`Self::anchor_reorder`]), or none for a stream whose
  /// keyframes reset every reference (VP8, VP9, AV1). A definitive `anchor`
  /// taken while one that is not holds the gap supersedes it, the count
  /// restarting there — which costs an H.264 stream nothing, its first
  /// picture out closing the gap either way.
  fn anchor_resync(&mut self, reorder_before: usize, anchor: access::Anchor) {
    // An anchor after the first — the first un-anchored — is one more packet
    // fed after it.
    if self.anchor_seen {
      self.count_degraded_packet();
    }
    self.anchor_seen = true;
    self.degraded_anchored = true;
    self.anchor_reorder = reorder_before;
    let rule = self.keyframe_rule();
    self.anchor_resets = rule == access::KeyframeRule::Resets;
    // The withheld proof is FFmpeg's own H.264 decoder's, and holds only
    // while no decode error across the gap has left its recovery state in
    // doubt.
    let native = matches!(&self.state, DecodeState::Sw(sw) if sw.native_h264);
    self.anchor_proof = match rule.proof() {
      access::Proof::Withheld if !native || self.withheld_poisoned => access::Proof::ReorderBound,
      proof => proof,
    };
    self.anchor_definitive = anchor.definitive();
    self.anchor_recovery = anchor.recovery();
    if let Some(recovery) = self.anchor_recovery {
      // An approximate recovery point anchors too: what the resync proves is
      // the pictures a decoder started at this point produces, not a match
      // with a decode that ran through the gap (`access::RecoveryPoint`).
      tracing::debug!(
        recovery_frame_cnt = recovery.frames(),
        exact_match = recovery.exact_match(),
        broken_link = recovery.broken_link(),
        "mediadecode-ffmpeg: a post-commit resync anchored at an H.264 recovery point; the \
         decoder withholds its pictures until the recovery it signals",
      );
    }
    self.outputs_since_anchor = 0;
    self.check_resync_proof();
  }

  /// A decode error before the resync is proven: the anchor is in doubt, and
  /// the next key-flagged packet anchors again — proved by the reorder bound,
  /// whatever the codec: the error may have left FFmpeg's recovery state
  /// marking pictures from before that anchor recovered
  /// ([`Self::withheld_poisoned`]). A no-op outside an open gap.
  fn unanchor(&mut self) {
    if !self.degraded_resync_pending {
      return;
    }
    self.withheld_poisoned = true;
    if self.degraded_anchored {
      self.degraded_anchored = false;
      self.anchor_definitive = false;
      self.anchor_recovery = None;
      self.outputs_since_anchor = 0;
    }
  }

  /// The decoder serving's `has_b_frames`, read live ([`reorder_depth`]).
  fn live_reorder(&self) -> usize {
    #[cfg(test)]
    if let Some(depth) = self.reorder_override {
      return depth;
    }
    match &self.state {
      DecodeState::Sw(sw) => reorder_depth(sw),
      _ => 0,
    }
  }

  /// The reorder buffer's depth, observed while the resync is anchored: the
  /// bound keeps the largest it has seen ([`Self::anchor_reorder`]).
  fn observe_reorder(&mut self) {
    if self.degraded_resync_pending && self.degraded_anchored {
      self.anchor_reorder = self.anchor_reorder.max(self.live_reorder());
    }
  }

  /// **The resync's proof**, by the stream's codec rule
  /// ([`Self::anchor_proof`]); either closes the gap.
  ///
  /// - **Withheld output (H.264, on FFmpeg's own `h264`):** the first
  ///   picture out since the anchor. Another implementation of the codec
  ///   keeps no such gate, and its anchors take the reorder bound
  ///   ([`SwDecoder::native_h264`]). The session's software decoders are
  ///   opened with neither
  ///   `AV_CODEC_FLAG_OUTPUT_CORRUPT` nor `AV_CODEC_FLAG2_SHOW_ALL` — an
  ///   open that finds either set is refused by name
  ///   ([`Error::UnrecoveredOutput`], [`open_sw_decoder`]) — so FFmpeg's
  ///   H.264 decoder outputs only pictures it has recovered: from an IDR
  ///   picture on, from the recovery a recovery point signals (`frame_num +
  ///   recovery_frame_cnt`) on. A decoder opened cold across the gap starts
  ///   with nothing recovered, so the first picture it delivers after the
  ///   anchor is one its tracking recovered, and so is a picture from before
  ///   the anchor that comes out after it. Allowing the reorder
  ///   buffer's depth on top, as the bound does, left a short tail — a
  ///   one-picture tail at a depth of 2 — open at the end, and raised
  ///   `PostCommitNeverResynced` on a resync that happened. After a decode
  ///   error across the gap the next anchor takes the reorder bound instead
  ///   ([`Self::withheld_poisoned`]): a packet FFmpeg reports failed may
  ///   have been parsed in part, its recovery state set, and pictures from
  ///   before that anchor come out marked recovered.
  /// - **The reorder bound (every other codec):** once more pictures have
  ///   come out since the anchor than the reorder buffer could hold from
  ///   before it — the largest depth it has shown from just before the
  ///   anchor on, this reading included ([`Self::anchor_reorder`]), or none
  ///   for a stream whose keyframes reset every reference — the last of them
  ///   is at or after the anchor in decode order. FFmpeg's HEVC decoder
  ///   keeps no recovery gate for a stream it is already decoding.
  fn check_resync_proof(&mut self) {
    self.observe_reorder();
    let allowance = match self.anchor_proof {
      access::Proof::Withheld => 0,
      access::Proof::ReorderBound if self.anchor_resets => 0,
      access::Proof::ReorderBound => self.anchor_reorder,
    };
    if self.outputs_since_anchor > allowance {
      self.clear_degraded_resync();
    }
  }

  /// Count one packet the software decoder took across the open gap, for
  /// the EOF escalation: before the first anchor, the fallback window
  /// ([`Self::packets_before_anchor`]); after it, a packet decoded with the
  /// resync never proved ([`Self::packets_unproven`]). The first anchor
  /// itself is neither — it is what [`Self::anchor_seen`] records. A no-op
  /// once the gap has closed.
  #[inline]
  fn count_degraded_packet(&mut self) {
    if !self.degraded_resync_pending {
      return;
    }
    if self.anchor_seen {
      self.packets_unproven = self.packets_unproven.saturating_add(1);
    } else {
      self.packets_before_anchor = self.packets_before_anchor.saturating_add(1);
    }
  }

  /// The software decoder output a picture, and it was delivered: one more
  /// picture since the anchor, against the stream's proof
  /// ([`Self::check_resync_proof`]). Before an anchor — a concealed P-frame
  /// from the dropped span — the guard stays set, so the one-GOP bound stays
  /// enforced and the EOF escalation still fires if no picture comes out
  /// after a keyframe. Pictures the queue holds never reach here.
  /// Idempotent; a no-op outside degraded mode (steady state, probe-era
  /// replay).
  #[inline]
  fn resync_on_output(&mut self) {
    if self.degraded_resync_pending && self.degraded_anchored {
      self.outputs_since_anchor = self.outputs_since_anchor.saturating_add(1);
      self.check_resync_proof();
    }
  }

  /// Unconditionally reset post-commit degraded-mode state. Used where the gap
  /// is moot regardless of resync proof: a `flush` (seek/reset re-anchors the
  /// stream) and the cleanup after an EOF escalation has already fired (so a
  /// follow-up poll sees plain EOF, not a repeated escalation). The
  /// frame-delivery path uses the anchor-gated [`Self::resync_on_output`]
  /// instead.
  #[inline]
  fn clear_degraded_resync(&mut self) {
    self.degraded_resync_pending = false;
    self.degraded_anchored = false;
    self.anchor_reorder = 0;
    self.anchor_resets = false;
    self.anchor_proof = access::Proof::ReorderBound;
    self.withheld_poisoned = false;
    self.anchor_definitive = false;
    self.anchor_recovery = None;
    self.outputs_since_anchor = 0;
    self.packets_before_anchor = 0;
    self.packets_unproven = 0;
    self.anchor_seen = false;
  }

  /// The one place a delivered frame is committed.
  ///
  /// Every road that hands a frame to the caller passes through here —
  /// the hardware scratch, the software scratch, both replay-queue
  /// entries, and the retry of a parked frame — so the bookkeeping a
  /// delivery owes cannot be attached to some of them and forgotten on
  /// others. It was: a parked software frame delivered on the retry
  /// road skipped the resync check, so the last recovered
  /// frame of a degraded stream could leave the resync guard standing
  /// and turn a clean EOF into a false
  /// [`PostCommitNeverResynced`].
  ///
  /// `decoded` says where the frame came from: `true` for a picture the
  /// decoder serving output (a scratch, parked or not), `false` for one the
  /// queue held — a replay's, or the tail a switch drained.
  fn commit_delivery(
    &mut self,
    frame: VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, C::Buffer>,
    decoded: bool,
    dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, C::Buffer>,
  ) {
    // The seat is free once a carrier exists for what it held.
    self.scratch_pending = false;
    // A picture the decoder output is what the resync's proof counts toward
    // closing the gap. A no-op on every road that never entered degraded
    // mode, which is why it can be unconditional here.
    if decoded {
      self.resync_on_output();
    }
    *dst = frame;
  }

  /// Where this session is. See
  /// [`SessionPhase`](crate::decoder::SessionPhase).
  ///
  /// The wrapper never sees a probe — that lives inside the hardware
  /// seam, which derives its own — so only the committed pair is
  /// reachable from here.
  const fn phase(&self) -> crate::decoder::SessionPhase {
    if self.eof_sent {
      crate::decoder::SessionPhase::Draining
    } else {
      crate::decoder::SessionPhase::Streaming
    }
  }

  /// Reads a drain answer against the session's own committed end.
  ///
  /// Routes a settled end through the post-commit gap check.
  ///
  /// **The `NeedsInput`-past-the-end reading moved out of here.** It
  /// used to be this method's own comparison against `eof_sent` — one
  /// more road deriving the session's phase for itself, which is the
  /// habit [`SessionPhase`](crate::decoder::SessionPhase) ended. The
  /// classifier makes that reading now, for every road at once, and
  /// what is left here is the part that is genuinely this wrapper's:
  /// an end is not clean if a post-commit gap never closed.
  fn settle(&mut self, status: Received) -> Result<Received, VideoDecodeError> {
    match status {
      Received::Ended => self.ended(),
      other => Ok(other),
    }
  }

  /// The end of the stream, read against a post-commit gap that never
  /// closed.
  ///
  /// One place, because there are now two spellings that reach it — the
  /// substrate's `AVERROR_EOF` and a settled [`Received::NeedsInput`]
  /// past a committed end — and a lost tail must escalate on both. The
  /// flag is cleared as it fires so a caller draining to the end sees
  /// the escalation once and the plain end afterwards.
  fn ended(&mut self) -> Result<Received, VideoDecodeError> {
    // The end proves nothing the resync's proof has not: an anchor that
    // decoded has had a recovered picture out since (H.264), or more than the
    // bound — the ones held from before it, then its own — and one that
    // decoded to nothing has had none, or only the held ones, which is not a
    // resync. A gap still open here never closed.
    if !self.degraded_resync_pending {
      return Ok(Received::Ended);
    }
    let loss = PostCommitNeverResynced::new(
      self.packets_before_anchor,
      self.packets_unproven,
      self.anchor_seen,
    );
    tracing::error!(
      packets_before_anchor = loss.packets_before_anchor(),
      packets_unproven = loss.packets_unproven(),
      anchor_seen = loss.anchor_seen(),
      "mediadecode-ffmpeg: {loss}",
    );
    self.clear_degraded_resync();
    Err(VideoDecodeError::PostCommitNeverResynced(loss))
  }

  /// Internal: convert the active scratch frame into a
  /// `mediadecode::VideoFrame` and write into `dst`.
  fn deliver_frame(
    &mut self,
    dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, C::Buffer>,
  ) -> Result<Received, VideoDecodeError> {
    let av_frame = match &mut self.state {
      DecodeState::Hw(_) => unsafe { self.hw_scratch.as_inner_mut().as_ptr() },
      // `SwClosed` holds no parked frame — a switch is refused while one
      // is — and the software scratch is the one its road would use.
      DecodeState::Sw(_) | DecodeState::SwClosed => unsafe { self.sw_scratch.as_ptr() },
    };
    // SAFETY: the scratch frame is live — either just filled by the
    // inner decoder's `receive_frame`, or left holding a frame whose
    // conversion did not commit. Convert takes what it needs out of it,
    // so the scratch can be reused once this has committed.
    let converted = unsafe {
      convert::av_frame_to_video_frame_as::<C>(av_frame, self.time_base, self.limits.frame())
    };
    match converted {
      Ok(new_frame) => {
        self.commit_delivery(new_frame, true, dst);
        Ok(Received::Frame)
      }
      Err(e) => {
        // Park only what another attempt could survive.
        self.scratch_pending = e.parks_in_decode();
        Err(VideoDecodeError::Convert(e))
      }
    }
  }
}

#[cfg(test)]
impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierVideoStreamDecoder<C> {
  /// Build a decoder around an injected HW seam, bypassing the real probe.
  /// Lets tests drive the post-commit fallback path with a [`HwInner`] fake
  /// instead of a live GPU. The SW fallback still opens the **real**
  /// `ffmpeg::decoder::Video` from `parameters`, so a fallback in these tests
  /// genuinely decodes.
  pub(crate) fn from_hw_inner_for_test(
    hw: Box<dyn HwInner>,
    parameters: Parameters,
    time_base: Timebase,
  ) -> Result<Self, Error> {
    Self::from_hw_inner_for_test_as(hw, parameters, time_base, DecodePath::Auto)
  }

  /// [`Self::from_hw_inner_for_test`], with the session's
  /// [`DecodePath`] named.
  ///
  /// The seam a **pinned** session's mid-stream behaviour is driven
  /// through: a pin's promise is about what happens when the hardware
  /// fails after opening, and the only way to reach that on a machine
  /// whose GPU works is to inject a seam that fails on demand. See
  /// `a_hardware_pin_reports_a_mid_stream_exhaustion_instead_of_degrading`.
  pub(crate) fn from_hw_inner_for_test_as(
    hw: Box<dyn HwInner>,
    parameters: Parameters,
    time_base: Timebase,
    path: DecodePath,
  ) -> Result<Self, Error> {
    let limits = DecoderLimits::default();
    let owned_parameters = try_clone_parameters(&parameters, limits.max_codec_parameter_bytes())?;
    Ok(Self {
      state: DecodeState::Hw(hw),
      path,
      parameters: owned_parameters,
      hw_scratch: Frame::empty()?,
      sw_scratch: alloc_av_video_frame()?,
      sw_replay_frames: ReplayQueue::default(),
      restart: None,
      pending_history: VecDeque::new(),
      pending_eof: false,
      replay_output_pending: false,
      deferred_error: None,
      eof_sent: false,
      degraded_resync_pending: false,
      degraded_anchored: false,
      anchor_reorder: 0,
      anchor_resets: false,
      anchor_proof: access::Proof::ReorderBound,
      withheld_poisoned: false,
      anchor_definitive: false,
      anchor_recovery: None,
      outputs_since_anchor: 0,
      sw_output_settled: true,
      packets_before_anchor: 0,
      packets_unproven: 0,
      anchor_seen: false,
      time_base,
      limits,
      sw_threads_pending: false,
      seeked: false,
      one_thread_since: None,
      one_thread_warned: false,
      #[cfg(test)]
      fail_threaded_opens: false,
      #[cfg(test)]
      threaded_opens: 0,
      #[cfg(test)]
      reorder_override: None,
      #[cfg(test)]
      reorder_on_submit: None,
      #[cfg(test)]
      fail_after_submit: false,
      scratch_pending: false,
      _carrier: core::marker::PhantomData,
    })
  }

  /// The session with its software decoders on `threads` — for a lane
  /// whose subject is the fallback machinery rather than the threads, or
  /// one that names the threads it proves.
  pub(crate) const fn with_threads_for_test(mut self, threads: crate::Threads) -> Self {
    self.limits = self.limits.with_threads(threads);
    self
  }

  /// Whether `send_eof` has been committed on the active decoder. Lets the
  /// rollback tests assert that a failed EOF fallback restores (never
  /// half-mutates) `eof_sent`.
  pub(crate) const fn eof_sent_for_test(&self) -> bool {
    self.eof_sent
  }

  /// Whether a post-commit fallback is awaiting a keyframe-anchored resync.
  /// Lets the escalation tests observe the degraded-resync state machine.
  pub(crate) const fn degraded_resync_pending_for_test(&self) -> bool {
    self.degraded_resync_pending
  }

  /// Whether the resync across the unresolved post-commit gap is anchored
  /// (the decoder reset at a clean keyframe and fed it). Lets the gating
  /// tests confirm a concealed P-frame or a keyframe that is not clean does
  /// not anchor it.
  pub(crate) const fn degraded_anchored_for_test(&self) -> bool {
    self.degraded_anchored
  }

  /// Whether the anchor holding the open gap is definitive — a clean random
  /// access point that supersedes one that is not.
  pub(crate) const fn anchor_definitive_for_test(&self) -> bool {
    self.anchor_definitive
  }

  /// The decoder serving's `has_b_frames`, read live.
  pub(crate) fn reorder_for_test(&self) -> usize {
    self.live_reorder()
  }

  /// The decoder serving reports `depth` as its `has_b_frames` from here on,
  /// in place of its own.
  pub(crate) fn set_reorder_for_test(&mut self, depth: usize) {
    self.reorder_override = Some(depth);
  }

  /// The decoder serving reports `depth` as its `has_b_frames` once it has
  /// taken its next packet — the parameters a keyframe activates lowering
  /// it.
  pub(crate) fn reorder_on_next_packet_for_test(&mut self, depth: usize) {
    self.reorder_on_submit = Some(depth);
  }

  /// The next packet the decoder serving takes is reported failed — taken
  /// and decoded, its pictures left ready — as FFmpeg reports a packet it
  /// decoded with an error.
  pub(crate) fn fail_next_packet_for_test(&mut self) {
    self.fail_after_submit = true;
  }

  /// Every open on the session's threads fails from here on.
  pub(crate) const fn failing_threaded_opens_for_test(mut self) -> Self {
    self.fail_threaded_opens = true;
    self
  }

  /// How many opens on the session's threads were attempted.
  pub(crate) const fn threaded_opens_for_test(&self) -> usize {
    self.threaded_opens
  }

  /// How many pictures wait in the replay queue.
  pub(crate) fn sw_replay_len_for_test(&self) -> usize {
    self.sw_replay_frames.len()
  }

  /// The bytes the pictures waiting in the replay queue hold.
  pub(crate) fn sw_replay_bytes_for_test(&self) -> usize {
    self.sw_replay_frames.bytes()
  }

  /// The session with its replay queue's byte budget at `bytes`.
  pub(crate) const fn with_max_replay_bytes_for_test(mut self, bytes: usize) -> Self {
    self.limits = self.limits.with_max_replay_bytes(bytes);
    self
  }

  /// The bytes of the picture parked past the replay queue, if one waits.
  pub(crate) fn sw_replay_parked_bytes_for_test(&self) -> usize {
    self.sw_replay_frames.parked_bytes()
  }

  /// Whether the post-commit path retained any replay frames — must always be
  /// empty for a post-commit fallback (it retains zero). Lets the finding-1
  /// dissolution test assert no replay frame was ever queued.
  pub(crate) fn sw_replay_frames_is_empty_for_test(&self) -> bool {
    self.sw_replay_frames.is_empty()
  }

  /// Packets the software decoder took across the open gap before an
  /// anchor. Lets the counter test confirm packets crossing the gap are
  /// tallied (and cleared on resync).
  pub(crate) const fn packets_before_anchor_for_test(&self) -> u64 {
    self.packets_before_anchor
  }
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierVideoStreamDecoder<C> {
  /// The fault a submission after end-of-stream earns on this face.
  ///
  /// **Censused from the empty-seat road rather than invented.** With
  /// the seat free, a post-EOF `send_packet` or a repeated `send_eof`
  /// reaches libavcodec, which answers `AVERROR_EOF`, and all four
  /// roads through this wrapper — hardware and software, packet and
  /// EOF — surface it as exactly this value. The gates below short
  /// out to the same one so a parked seat cannot change *which* answer
  /// a caller gets, only how quickly. `the_post_eof_fault_is_the_one_the_substrate_gives`
  /// pins the two against each other.
  ///
  /// Deliberately **not** a new `VideoDecodeError` arm. The subtitle
  /// seam had to mint `AfterEof` because `avcodec_decode_subtitle2` has
  /// no state machine to refuse for it; this face already has an answer
  /// for the condition, and a second spelling for one fault on one
  /// surface is the disease this release is curing.
  fn after_eof() -> VideoDecodeError {
    VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))
  }

  pub(crate) fn send_packet_impl(
    &mut self,
    packet: &VideoPacket<VideoPacketExtra, C::Buffer>,
  ) -> Result<Sent, VideoDecodeError> {
    // **The end of the stream outranks the parked seat, and the order
    // is the whole point.**
    //
    // `Sent::MustDrain` is a promise: drain the output and this same
    // offer becomes acceptable. Past end-of-stream that promise is
    // false — draining empties the seat and the retry still faults,
    // until `flush`. Checking the seat first made the wrapper answer
    // `MustDrain` for a submission nothing could ever accept, which is
    // the same fault-under-back-pressure inversion the subtitle seam
    // carried: a caller obeying the contract loops, drains, re-offers,
    // and is refused anyway.
    //
    // It is reachable: `send_eof` is accepted and sets `eof_sent`, a
    // delayed tail frame comes out of the decoder, its carrier
    // allocation fails parkably, and the seat is taken on a session
    // that is already over.
    if !self.phase().accepts_input() {
      return Err(Self::after_eof());
    }
    // **Nothing is sent while a frame is parked.** Both send roads can
    // commit a hardware-to-software fallback, and a fallback under a
    // parked frame would leave the retry reading the other scratch. See
    // [`Self::scratch_pending`]. Nothing was consumed, so this is back
    // pressure and the packet is still the caller's to re-offer — which
    // is true precisely because the stream is not over, checked above.
    if self.scratch_pending {
      return Ok(Sent::MustDrain);
    }
    let phase = self.phase();
    // Scoped submission: the rebuilt `AVPacket` never leaves this call,
    // which is what lets the view lane share its buffer with libavcodec
    // rather than copy into it. See `boundary::with_ffmpeg_video_packet`.
    let limits = self.limits.packet_limits();
    // **The route depends on what this decoder does with what it is
    // sent.** While the hardware probe is open it `av_packet_ref`s
    // every accepted packet into a rescue history, and
    // `AllBackendsFailed::into_unconsumed_packets` hands those out as
    // owned, mutable `Packet`s — so a shared body would escape this
    // call as a live mutable alias of a carrier the caller may still be
    // reading. Inside that window the body is copied; once the probe
    // has committed, nothing is recorded and the send is zero-copy
    // again. The software road never records.
    let route = match &self.state {
      DecodeState::Hw(hw) if hw.records_submissions() => crate::carrier::BodyRoute::Copy,
      _ => crate::carrier::BodyRoute::Submission,
    };
    boundary::with_ffmpeg_video_packet::<C, _>(packet, limits, route, |av_pkt| {
      match &mut self.state {
        DecodeState::Hw(hw) => match hw.send_packet(av_pkt) {
          // The seam already classified libavcodec's back pressure, so
          // both states travel on unchanged. A keyframe the hardware took
          // is the first one after a seek, if one was pending: the next
          // keyframe the software road sees is not.
          Ok(status) => {
            if matches!(status, Sent::Accepted) && av_pkt.is_key() {
              self.seeked = false;
            }
            Ok(status)
          }
          Err(Error::AllBackendsFailed(p)) => {
            // **A pinned hardware session reports rather than degrades.**
            // See [`Self::may_open_software`]: this is the exhaustion
            // `DecodePath::Auto` reads as its cue to open software, and
            // the pin's whole content is that it is not that cue here.
            // Reported with the payload intact, so the caller keeps the
            // backend, its error, and any rescued packets.
            if !self.may_open_software() {
              return Err(VideoDecodeError::Decode(Error::AllBackendsFailed(p)));
            }
            // Route on the EXPLICIT origin, never on whether `rescued` is empty (a
            // probe-era first-packet cap trip is *also* empty).
            if p.origin().is_post_commit() {
              // Post-commit: DEGRADE AND CONTINUE. No lossless mid-stream
              // reconstruction — the SW decoder opens cold, retains zero replay
              // frames, and resyncs at the next keyframe. The current packet (the
              // one HW REFUSED) is forwarded to that cold SW: if it is the resync
              // keyframe SW decodes from it, otherwise SW drops it until a keyframe
              // arrives. The bounded span from here to that keyframe is dropped — a
              // loudly logged gap (see the `warn!`), not a silent one.
              tracing::warn!(
                backend = ?p.attempts().last().map(|(b, _)| *b),
                pts = ?av_pkt.pts(),
                "mediadecode-ffmpeg: HW decode failed post-commit; falling back to \
                 software, resyncing at next keyframe — a bounded span of frames \
                 may be dropped at this boundary",
              );
              // Transactional SW-open + current-packet forward; degrade-tracking
              // (incl. keyframe-anchor recording) happens inside on a clean commit.
              // A failure surfaces `FallbackFailed` and stays on HW.
              // A clean degrade forwarded this very packet into the
              // cold software decoder, so it was consumed.
              // `false`: this road is unreachable once the end is
              // committed — `send_packet_impl`'s first gate refuses
              // every packet past `eof_sent` — so there is no EOF to
              // re-forward, and forwarding one alongside a packet is
              // the pairing [`PostCommitInput`] forbids.
              return self
                .degrade_to_sw(PostCommitInput::Packet(av_pkt), false)
                .map(|()| Sent::Accepted)
                .map_err(VideoDecodeError::Decode);
            }
            // Probe-era: replay the inner decoder's buffered history (lossless —
            // no frame was delivered yet), then forward the still-unconsumed
            // current packet to SW.
            let rescued = p.into_unconsumed_packets();
            // `eof_pending` is the committed EOF state — never pre-mutated here.
            let eof_pending = self.eof_sent;
            self
              .fall_back_to_sw(rescued, eof_pending)
              .map_err(VideoDecodeError::Decode)?;
            // Forward the new (still-unconsumed) current packet to the
            // software road — the HW decoder REFUSED it, so it was not in the
            // replay set. It is the road's next packet like any other: a
            // keyframe here is where the session's threads come back. A
            // failure surfaces (it is not silently dropped), and back pressure
            // is reported as such rather than mistaken for one: the fallback
            // committed either way, and the caller re-offers the packet.
            self.send_on_software(av_pkt, phase)
          }
          Err(other) => Err(VideoDecodeError::Decode(other)),
        },
        DecodeState::Sw(_) | DecodeState::SwClosed => self.send_on_software(av_pkt, phase),
      }
    })
    .map_err(|e| VideoDecodeError::Decode(Error::PacketBuild(e)))?
  }

  pub(crate) fn receive_frame_impl(
    &mut self,
    dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, C::Buffer>,
  ) -> Result<Received, VideoDecodeError> {
    // Deliver any frames produced during SW fallback replay before
    // pulling new ones from the SW decoder. This is the queue
    // populated by `fall_back_to_sw` when SW returned EAGAIN during
    // packet replay, and by the drains that restart the software decoder at a
    // clean keyframe. None of them is a picture decoded after a resync
    // anchor, so their delivery never closes a post-commit gap.
    // **Peeked, not popped.** A replayed frame is the rescue history's
    // only copy: popping it before the conversion committed lost it to
    // any allocation failure, which is the one thing this queue exists
    // to prevent. It leaves the queue when a carrier exists for it.
    if let Some(replayed) = self.sw_replay_frames.front() {
      // SAFETY: `replayed` is a live AVFrame owned by this queue;
      // convert takes what it needs out of it.
      let converted = unsafe {
        convert::av_frame_to_video_frame_as::<C>(
          replayed.as_ptr(),
          self.time_base,
          self.limits.frame(),
        )
      };
      let new_frame = match converted {
        Ok(new_frame) => new_frame,
        Err(e) if e.parks_in_decode() => return Err(VideoDecodeError::Convert(e)),
        // A frame nothing can carry is dropped rather than re-offered
        // forever — the same rule the scratch seat follows.
        Err(e) => {
          self.sw_replay_frames.pop_front();
          return Err(VideoDecodeError::Convert(e));
        }
      };
      self.sw_replay_frames.pop_front();
      self.commit_delivery(new_frame, false, dst);
      return Ok(Received::Frame);
    }
    // A decode error a send met while feeding what was pending, reported
    // after the pictures queued before it.
    if let Some(error) = self.deferred_error.take() {
      return Err(VideoDecodeError::Decode(error));
    }
    // A restart's drain waits for the caller to send its keyframe again,
    // which carries it on: the drained decoder is not read from here.
    if self.restart.is_some() {
      return Ok(Received::NeedsInput);
    }
    // A replay a fallback left: the next send carries it on, or — past the
    // session's end, where nothing more can be sent — this drain does. An
    // error it meets waits behind the pictures it queued before it, which
    // are delivered first, from the top.
    if self.has_pending_replay() {
      if !self.eof_sent {
        return Ok(Received::NeedsInput);
      }
      if let Err(error) = self.replay_pending() {
        self.deferred_error = Some(error);
      }
      if !self.sw_replay_frames.is_empty()
        || self.deferred_error.is_some()
        || self.has_pending_replay()
      {
        return self.receive_frame_impl(dst);
      }
    }
    // A frame whose conversion did not commit is converted again before
    // the decoder is asked for another — see [`Self::scratch_pending`].
    // The scratch still holds it, and `deliver_frame` reads whichever
    // scratch the current state uses.
    if self.scratch_pending {
      return self.deliver_frame(dst);
    }
    let phase = self.phase();
    loop {
      match &mut self.state {
        DecodeState::Hw(hw) => match hw.receive_frame(&mut self.hw_scratch) {
          Ok(Received::Frame) => {
            // The frame is out of the decoder's queue from here; the
            // seat is what keeps it if the conversion cannot commit.
            self.scratch_pending = true;
            return self.deliver_frame(dst);
          }
          // The hardware seam already classified the two flow signals.
          // They still pass the session's own end: see [`Self::settle`].
          Ok(status) => return self.settle(status),
          Err(Error::AllBackendsFailed(p)) => {
            // The pin, on the receive road — see
            // [`Self::may_open_software`] and the identical gate on the
            // two send roads.
            if !self.may_open_software() {
              return Err(VideoDecodeError::Decode(Error::AllBackendsFailed(p)));
            }
            // HW exhausted at frame-time. There is no current packet here.
            // Route on the explicit origin.
            if p.origin().is_post_commit() {
              // Post-commit: DEGRADE AND CONTINUE — open SW cold (no current
              // packet to forward, no replay frames retained) and resync at the
              // next keyframe, dropping the bounded span up to it. Loud single
              // `warn!` marks that accepted gap. A clean commit enters degraded
              // mode; a SW-open failure surfaces `FallbackFailed` and stays HW.
              tracing::warn!(
                backend = ?p.attempts().last().map(|(b, _)| *b),
                "mediadecode-ffmpeg: HW decode failed post-commit at frame-time; \
                 falling back to software, resyncing at next keyframe — a bounded \
                 span of frames may be dropped at this boundary",
              );
              // **The committed end travels with the fallback.** Read
              // before anything mutates, exactly as the probe-era road
              // below reads it. Without it the cold decoder answers
              // `EAGAIN` forever on a session no send can feed.
              let eof_pending = self.eof_sent;
              self
                .degrade_to_sw(PostCommitInput::FrameTime, eof_pending)
                .map_err(VideoDecodeError::Decode)?;
              // Nothing to deliver yet — fall through to the loop; the next
              // iteration takes the Sw arm and pulls from the cold SW decoder.
              continue;
            }
            // Probe-era: replay the buffered history (lossless).
            let rescued = p.into_unconsumed_packets();
            // `eof_pending` is the committed EOF state — never pre-mutated here.
            let eof_pending = self.eof_sent;
            self
              .fall_back_to_sw(rescued, eof_pending)
              .map_err(VideoDecodeError::Decode)?;
            // Delivered from the top, where the replay's queue comes first
            // and what it left pending is carried on — preserving stream
            // order against whatever the SW decoder produces next.
            return self.receive_frame_impl(dst);
          }
          Err(other) => return Err(VideoDecodeError::Decode(other)),
        },
        DecodeState::Sw(sw) => {
          // Convert inline (rather than via `deliver_frame`, which borrows all
          // of `self`) so only the disjoint fields `sw_scratch` / `time_base`
          // are touched alongside the `self.state` borrow `sw` holds.
          let st = sw.state();
          match sw.receive_frame(&mut self.sw_scratch) {
            Ok(()) => {
              // The frame is out of the decoder's queue from here; the
              // seat is what keeps it if the conversion cannot commit.
              self.scratch_pending = true;
              // SAFETY: the scratch frame is live (just filled by
              // `receive_frame`); convert takes what it needs out of
              // it, so the scratch can be reused once this commits.
              let converted = unsafe {
                convert::av_frame_to_video_frame_as::<C>(
                  self.sw_scratch.as_ptr(),
                  self.time_base,
                  self.limits.frame(),
                )
              };
              let new_frame = match converted {
                Ok(new_frame) => new_frame,
                Err(e) => {
                  self.scratch_pending = e.parks_in_decode();
                  return Err(VideoDecodeError::Convert(e));
                }
              };
              // SW output a picture. The commit point counts it toward the
              // resync's proof only once a keyframe fed across the gap has
              // anchored it; a concealed P-frame from before the anchor does
              // not count (see `resync_on_output`).
              self.commit_delivery(new_frame, true, dst);
              return Ok(Received::Frame);
            }
            // Funnel first — so a recorded budget refusal is named
            // rather than laundered — read as a status second (`EAGAIN`
            // is `NeedsInput`, `Eof` is `Ended`, and the errno stops
            // inside this crate either way), and settled against the
            // session's own end third.
            //
            // That last step is where a post-commit resync that never
            // closed becomes [`VideoDecodeError::PostCommitNeverResynced`]
            // instead of a clean end that would swallow the tail — and
            // it now catches the end however the codec spelled it. See
            // [`Self::settle`] and [`Self::ended`].
            Err(e) => match crate::decoder::software_receive(st, e, phase) {
              Ok(status) => {
                // Nothing is ready: the decoder holds no picture the caller
                // has not taken.
                self.sw_output_settled = true;
                return self.settle(status);
              }
              Err(error) => {
                // A decode error leaves the output unsettled, and one before
                // the resync is proven leaves its anchor in doubt.
                self.sw_output_settled = false;
                self.unanchor();
                return Err(VideoDecodeError::Decode(error));
              }
            },
          }
        }
        // No decoder open (see [`DecodeState::SwClosed`]): nothing is
        // waiting in one, so the drain asks for the packet that opens the
        // next — or, past the session's end, the stream has ended.
        DecodeState::SwClosed => {
          return if self.eof_sent {
            self.ended()
          } else {
            Ok(Received::NeedsInput)
          };
        }
      }
    }
  }

  pub(crate) fn send_eof_impl(&mut self) -> Result<Sent, VideoDecodeError> {
    // The same two gates in the same order, for the same reason: a
    // repeated end-of-stream past a committed one is refused however
    // much is drained, so answering back pressure would be a promise
    // this face cannot keep. See [`Self::after_eof`].
    if !self.phase().accepts_input() {
      return Err(Self::after_eof());
    }
    // As `send_packet`: EOF can commit a fallback too, and the escalation
    // it may raise reads the resync standing a parked frame has not yet
    // had the chance to clear. Nothing was recorded, so drain and signal
    // again.
    if self.scratch_pending {
      return Ok(Sent::MustDrain);
    }
    // On the software road what is pending comes first: an error the drain
    // has not reported, the packets and the end a fallback's replay left —
    // whose end, once fed, is this one — and the pictures it left in the
    // decoder, and a restart's drain, whose decoder was told the stream
    // ended already and, drained, has nothing left to restart for.
    if matches!(self.state, DecodeState::Sw(_)) {
      if self.deferred_error.is_some() {
        return Ok(Sent::MustDrain);
      }
      let forwarding = self.pending_eof;
      let replayed = self.replay_pending();
      // The end the replay owed is this one. Once the decoder took it, it is
      // the session's — `replay_pending` committed it — and this send is
      // accepted whatever the drain after it met: that error waits for the
      // caller's drain, behind the pictures queued before it, and the end is
      // never sent again.
      if forwarding && self.eof_sent {
        if let Err(error) = replayed {
          self.deferred_error = Some(error);
        }
        return Ok(Sent::Accepted);
      }
      match replayed {
        Ok(true) => {}
        Ok(false) => return Ok(Sent::MustDrain),
        Err(error) => {
          self.deferred_error = Some(error);
          return Ok(Sent::MustDrain);
        }
      }
      if self.restart.is_some() {
        match self.drain_for_restart() {
          Ok(true) => {
            self.restart = None;
            self.eof_sent = true;
            return Ok(Sent::Accepted);
          }
          Ok(false) => return Ok(Sent::MustDrain),
          Err(error) => {
            self.deferred_error = Some(error);
            return Ok(Sent::MustDrain);
          }
        }
      }
    }
    let phase = self.phase();
    let outcome = match &mut self.state {
      DecodeState::Hw(hw) => match hw.send_eof() {
        // The seam classified libavcodec's back pressure already.
        Ok(status) => Ok(status),
        Err(Error::AllBackendsFailed(p)) => {
          // The pin, on the EOF road — see [`Self::may_open_software`].
          // Returned rather than folded into `outcome`: the commit below
          // fires only on `Ok(Sent::Accepted)`, so the two roads agree,
          // and leaving early keeps the fallback body at the nesting it
          // was written at.
          if !self.may_open_software() {
            return Err(VideoDecodeError::Decode(Error::AllBackendsFailed(p)));
          }
          // EOF is pending for this transaction, so the SW decoder must also
          // receive `send_eof` (codecs that delay tail frames hang otherwise).
          // We pass that intent locally rather than pre-setting `self.eof_sent`:
          // a fallback that fails returns `FallbackFailed` and stays on HW, and a
          // half-mutated `self.eof_sent = true` would then make a *later*
          // fallback inject an EOF into SW even though this `send_eof` errored.
          // `self.eof_sent` is committed only after the whole operation succeeds
          // (the `outcome` check below), keeping the fallback all-or-nothing.
          if p.origin().is_post_commit() {
            // Post-commit: DEGRADE AND CONTINUE — open SW cold, re-forward EOF
            // (no current packet, no replay frames). The cold SW produces no
            // frame from EOF alone, so the drain-to-EOF in `receive_frame`
            // escalates (`PostCommitNeverResynced`) unless a later keyframe-fed
            // poll resyncs first. A clean commit enters degraded mode; a SW-open
            // failure surfaces `FallbackFailed` and stays HW.
            tracing::warn!(
              backend = ?p.attempts().last().map(|(b, _)| *b),
              "mediadecode-ffmpeg: HW decode failed post-commit at EOF; falling \
               back to software — a bounded span of tail frames may be dropped",
            );
            // Both fallback roads forward the EOF inside their own
            // transaction, so a clean commit means it was recorded.
            // `true`: this *is* the end being sent. `eof_sent` is not
            // committed until the whole operation succeeds, so the
            // intent is passed locally rather than read back.
            self
              .degrade_to_sw(PostCommitInput::Eof, true)
              .map(|()| Sent::Accepted)
              .map_err(VideoDecodeError::Decode)
          } else {
            // Probe-era: replay the buffered history (lossless), re-forwarding
            // EOF inside the transaction. A replay the queue's budget stopped
            // has not fed the end yet: the caller drains and sends it again,
            // and the software road feeds what is left, the end last.
            let rescued = p.into_unconsumed_packets();
            match self.fall_back_to_sw(rescued, true) {
              // The end is not fed yet: it waits behind the packets.
              Ok(()) if self.replay_owes_input() => Ok(Sent::MustDrain),
              // The decoder took the end: it is the session's, and the
              // pictures it still holds come out on the drains past it.
              Ok(()) => Ok(Sent::Accepted),
              Err(error) => Err(VideoDecodeError::Decode(error)),
            }
          }
        }
        Err(other) => Err(VideoDecodeError::Decode(other)),
      },
      DecodeState::Sw(sw) => {
        let st = sw.state();
        match sw.send_eof() {
          Ok(()) => Ok(Sent::Accepted),
          Err(e) => crate::decoder::software_send(st, e, phase).map_err(VideoDecodeError::Decode),
        }
      }
      // No decoder is open, so none holds anything to flush out: the end
      // is taken, and the drain answers it once the queue is empty.
      DecodeState::SwClosed => Ok(Sent::Accepted),
    };
    // Commit EOF state only when the EOF was actually **taken** — a failed
    // fallback left `self.eof_sent` untouched (restored-by-construction: we
    // never mutated it), so HW stays EOF-not-yet-sent and a retry behaves
    // correctly.
    //
    // **`is_ok()` is not the test any more, and that is not a stylistic
    // change.** `Ok(Sent::MustDrain)` means the decoder did not take the
    // end-of-stream; recording `eof_sent` there would make a later fallback
    // inject an EOF into the software decoder for a signal that was never
    // accepted — the exact half-mutation the local `eof_pending` argument
    // exists to prevent on the failure road.
    if matches!(outcome, Ok(Sent::Accepted)) {
      self.eof_sent = true;
    }
    outcome
  }

  pub(crate) fn flush_impl(&mut self) -> Result<(), VideoDecodeError> {
    // Drop any frames buffered during SW fallback replay before
    // flushing the inner decoder — otherwise a seek/reset would
    // surface stale pre-flush frames on the next `receive_frame`.
    self.sw_replay_frames.clear();
    // So does what was pending: a replay's unfed packets, a restart's drain
    // (the flush below resets its decoder), an error not yet reported.
    self.pending_history.clear();
    self.pending_eof = false;
    self.replay_output_pending = false;
    self.restart = None;
    self.deferred_error = None;
    // And a parked frame belongs to the position being abandoned.
    self.scratch_pending = false;
    // Flush ends the drain phase; the decoder accepts new packets
    // after this, so reset EOF tracking.
    self.eof_sent = false;
    // A flush (seek/reset) re-anchors the stream — any in-flight post-commit
    // resync tracking from before the flush is moot. Clear it so the next EOF
    // doesn't escalate over a now-irrelevant pre-flush gap.
    self.clear_degraded_resync();
    // The first keyframe after a seek is a switch point whatever its kind:
    // the seek has discarded what led it. And the minute starts over.
    self.seeked = true;
    self.one_thread_since = None;
    self.sw_output_settled = true;
    match &mut self.state {
      // The HW seam's `flush` returns `Result` for a uniform trait; the
      // real `VideoDecoder::flush` is infallible (always `Ok`).
      DecodeState::Hw(hw) => hw.flush().map_err(VideoDecodeError::Decode)?,
      // **A one-thread decoder a fallback committed is flushed and kept.**
      // The session's threads come back at the first keyframe after the
      // flush — where a seek lands — by the same rule as anywhere else
      // (see [`Self::send_on_software`]); the flushed decoder has nothing
      // left to drain there.
      DecodeState::Sw(sw) => sw.flush(),
      DecodeState::SwClosed => {}
    }
    Ok(())
  }
}

macro_rules! video_lane_face {
  ($($lane:ty),+ $(,)?) => { $(
    impl CarrierVideoStreamDecoder<$lane> {
      /// Opens a video decoder for `parameters`, probing hardware
      /// backends in order and falling back to software.
      ///
      /// [`open_as`](Self::open_as)`(.., DecodePath::Auto)`, which is
      /// what this has always done.
      pub fn open(
        parameters: Parameters,
        time_base: Timebase,
        limits: DecoderLimits,
      ) -> Result<Self, Error> {
        Self::open_impl(parameters, time_base, limits)
      }

      /// Opens a video decoder on a **named decode path**.
      ///
      /// [`DecodePath::Auto`] is [`open`](Self::open) exactly; the
      /// other two arms pin the session to hardware or to software for
      /// its whole life. See [`DecodePath`] for what a pin promises and
      /// what it costs.
      ///
      /// Everything else about the session is unchanged — the same
      /// [`VideoStreamDecoder`] face, the same frames, the same
      /// [`is_hardware`](Self::is_hardware) / [`is_software`](Self::is_software)
      /// readings. The choice is *which decoder is behind them*, which
      /// is what a determinism comparison and a deployment policy each
      /// need and neither could reach.
      ///
      /// # Errors
      ///
      /// [`DecodePath::Hardware`] fails here when the named backend
      /// cannot be opened for the stream — where [`DecodePath::Auto`]
      /// would have gone on to software. [`DecodePath::Software`] fails
      /// only where libavcodec has no decoder for the stream, or the
      /// context cannot be built.
      ///
      /// # Examples
      ///
      /// ```no_run
      /// use mediadecode_ffmpeg::{DecodePath, DecoderLimits, FfmpegVideoStreamDecoder};
      /// # fn f(parameters: ffmpeg_next::codec::Parameters, time_base: mediadecode::Timebase)
      /// # -> Result<(), Box<dyn std::error::Error>> {
      /// // The same stream, decoded without a GPU anywhere in the story.
      /// let decoder = FfmpegVideoStreamDecoder::open_as(
      ///   parameters,
      ///   time_base,
      ///   DecoderLimits::default(),
      ///   DecodePath::Software,
      /// )?;
      /// assert!(decoder.is_software());
      /// # Ok(())
      /// # }
      /// ```
      pub fn open_as(
        parameters: Parameters,
        time_base: Timebase,
        limits: DecoderLimits,
        path: DecodePath,
      ) -> Result<Self, Error> {
        Self::open_as_impl(parameters, time_base, limits, path)
      }

      /// Whether this decoder is currently running on software.
      pub const fn is_software(&self) -> bool {
        self.is_software_impl()
      }

      /// Whether this decoder is currently running on hardware.
      pub const fn is_hardware(&self) -> bool {
        self.is_hardware_impl()
      }

      /// How many threads libavcodec decodes this session on, read back
      /// from the decoder serving now after `avcodec_open2` settled it.
      ///
      /// On the software road this is what the
      /// [`Threads`](crate::Threads) in the session's
      /// [`DecoderLimits`] resolved to: under
      /// [`Auto`](crate::Threads::Auto), one more thread than the host
      /// has cores (at most 16) for a codec that can thread, and one for
      /// a codec that cannot. The hardware road writes no thread fields
      /// and reads libavcodec's default of one. A live reading, like
      /// [`is_software`](Self::is_software): a session that degrades
      /// mid-stream answers for its software decoder from then on.
      ///
      /// `None` where libavcodec recorded no count — a codec that runs
      /// its own threads, such as libdav1d under `Auto`.
      pub fn active_threads(&self) -> Option<core::num::NonZeroU32> {
        self.active_threads_impl()
      }

      /// Whether this session can currently emit pictures at a
      /// caller-requested output size. See
      /// [`ScaledOutputCapability`] and
      /// [`Self::request_scaled_output`].
      ///
      /// `Supported` on a live VideoToolbox session on an Apple
      /// target, `Unsupported` everywhere else — including on a
      /// session that has degraded to software, which is why this
      /// reads the session's live state rather than a fact recorded
      /// once at open.
      pub fn scaled_output_capability(&self) -> ScaledOutputCapability {
        self.scaled_output_capability_impl()
      }

      /// Requests that this session emit pictures at `size` (width,
      /// height) from the next frame on, and reports whether the
      /// request was recorded. See
      /// [`VideoStreamDecoder::request_scaled_output`] for the full
      /// contract (never an error) and
      /// [`Self::scaled_output_capability`]'s documentation for which
      /// road can honor one, what a mid-stream request means, and the
      /// zero / upscale refusals this seat mints itself.
      pub fn request_scaled_output(&mut self, size: (u32, u32)) -> ScaledOutputCapability {
        self.request_scaled_output_impl(size)
      }

      /// The hardware wrapper, when one is in use.
      pub fn hardware_inner(&self) -> Option<&VideoDecoder> {
        self.hardware_inner_impl()
      }

      /// The stream timebase every produced timestamp is stamped with.
      pub const fn time_base(&self) -> Timebase {
        self.time_base_impl()
      }
    }

    impl VideoStreamDecoder for CarrierVideoStreamDecoder<$lane> {
      type Adapter = Ffmpeg;
      type Buffer = <$lane as crate::FfmpegCarrier>::Buffer;
      type Error = VideoDecodeError;

      fn send_packet(
        &mut self,
        packet: &VideoPacket<VideoPacketExtra, Self::Buffer>,
      ) -> Result<Sent, Self::Error> {
        self.send_packet_impl(packet)
      }

      fn receive_frame(
        &mut self,
        dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, Self::Buffer>,
      ) -> Result<Received, Self::Error> {
        self.receive_frame_impl(dst)
      }

      fn send_eof(&mut self) -> Result<Sent, Self::Error> {
        self.send_eof_impl()
      }

      fn flush(&mut self) -> Result<(), Self::Error> {
        self.flush_impl()
      }

      fn scaled_output_capability(&self) -> ScaledOutputCapability {
        self.scaled_output_capability_impl()
      }

      fn request_scaled_output(&mut self, size: (u32, u32)) -> ScaledOutputCapability {
        self.request_scaled_output_impl(size)
      }
    }
  )+ };
}

video_lane_face!(crate::View, crate::Owned);

/// How far a replay got: the packets the decoder took — a packet it
/// failed counted with them, consumed — and whether it took the end of the
/// stream.
#[derive(Default)]
struct Replay {
  fed: usize,
  eof_sent: bool,
}

/// How a drain into the queue stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drained {
  /// The decoder has no picture ready, for now or for good.
  Empty,
  /// The queue reached its budget first; the decoder may hold more.
  Full,
}

/// Feeds a software decoder `packets`, then the end of the stream when
/// `eof`, pulling every picture it makes into `queue` — the body of the
/// probe-era fallback's transaction (see `fall_back_to_sw_inner`) and of
/// the replay it leaves for the caller's drains (see `replay_pending`).
///
/// Stops, resumable, between packets once `queue` reaches `budget`
/// ([`ReplayQueue::full`]): `progress` says how far it got, and the rest is
/// fed by a later call. Nothing is dropped.
///
/// Answers how its last drain stopped: [`Drained::Full`] where the queue
/// reached its budget first — packets or the end left unfed, or, with all of
/// them fed, a picture parked or more the decoder may still hold — and
/// [`Drained::Empty`] where the decoder had no picture ready. A `Full`
/// replay is not over even when `progress` has fed everything: the caller
/// owes the decoder's pictures to the queue before it takes any input.
fn replay_history(
  sw: &mut SwDecoder,
  packets: &[ffmpeg_next::Packet],
  eof: bool,
  queue: &mut ReplayQueue,
  budget: usize,
  progress: &mut Replay,
) -> Result<Drained, Error> {
  // Bound before the decoder is mutably borrowed, so the error
  // closures below can still consult it.
  let sw_state = sw.state();
  for pkt in packets {
    if queue.full(budget) {
      return Ok(Drained::Full);
    }
    let mut attempts: u32 = 0;
    loop {
      match sw.submit(pkt) {
        Ok(()) => break,
        Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
          if drain_into(sw, sw_state, queue, budget)? == Drained::Full {
            return Ok(Drained::Full);
          }
          attempts += 1;
          if attempts > 16 {
            // A decoder that takes neither the packet nor gives a picture:
            // the packet is consumed with the error, so a retry moves on.
            progress.fed += 1;
            return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
              errno: ffmpeg_next::error::EAGAIN,
            }));
          }
        }
        Err(other) => {
          progress.fed += 1;
          return Err(crate::decoder::software_exit(sw_state, other));
        }
      }
    }
    progress.fed += 1;
    // What it made, pulled now, so an error it carries surfaces with it. A
    // picture parked here, after the last packet, is the answer too.
    if drain_into(sw, sw_state, queue, budget)? == Drained::Full {
      return Ok(Drained::Full);
    }
  }
  // Re-forward EOF if the HW path already saw it. SW EOF can also
  // return EAGAIN until prior output is drained — mirror the
  // packet-replay loop.
  if eof && !progress.eof_sent {
    let mut attempts: u32 = 0;
    loop {
      match sw.send_eof() {
        Ok(()) => {
          progress.eof_sent = true;
          #[cfg(test)]
          if replay_fault::fire() {
            return Err(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
          }
          break;
        }
        // Already draining: the decoder took the end before.
        Err(ffmpeg_next::Error::Eof) => {
          progress.eof_sent = true;
          break;
        }
        // Back pressure that never lifts, and a refusal, leave the end owed:
        // only the decoder taking it — or answering that it has — sends it.
        Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
          if drain_into(sw, sw_state, queue, budget)? == Drained::Full {
            return Ok(Drained::Full);
          }
          attempts += 1;
          if attempts > 16 {
            return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
              errno: ffmpeg_next::error::EAGAIN,
            }));
          }
        }
        Err(other) => return Err(crate::decoder::software_exit(sw_state, other)),
      }
    }
  }
  // Final drain BEFORE commit — the transactional commit boundary. The
  // EAGAIN-triggered drains above only fire when SW exerts backpressure mid
  // replay; a SW decoder that ACCEPTS every replayed packet (and the EOF)
  // without one then surfaces a non-transient error — `InvalidData` from a
  // corrupt replayed packet, or any other decode failure — only on the *next*
  // `receive_frame`. Without this drain that error would land after the
  // commit (frames appended, `state` flipped to `Sw`, rescued packets
  // dropped) and reach the caller as a plain decode failure, not
  // `FallbackFailed` — breaking probe-era recovery on non-seekable input.
  // Draining to EAGAIN/EOF here forces any such error to surface now, so it is
  // wrapped as `FallbackFailed` (retaining the rescued packets) and the
  // decoder stays on HW — nothing is committed. (Only the probe-era path
  // reaches this; the post-commit path degrades via `degrade_to_sw` and never
  // replays.) On a frame-threaded decoder the drain reaches only what its
  // threads have finished, which is why the transaction is decided on one
  // thread — see `fall_back_to_sw_inner`. A queue at its budget stops it, and
  // the answer says so: the pictures still in the decoder are owed to the
  // queue before the caller's next input is taken.
  drain_into(sw, sw_state, queue, budget)
}

/// Pulls the decoder's pictures into `queue` until it has none ready or the
/// queue would pass `budget` — checked before each picture is received,
/// taking the next to be the size of the last, so the decoder keeps a
/// picture that would not fit until the caller has taken enough, and nothing
/// received is dropped. A picture whose footprint alone exceeds `budget` can never be
/// queued under it — no progress is possible — and is refused by name,
/// [`Error::ReplayQueueFull`]; one whose footprint cannot be read — an
/// allocation of no stated size ([`footprint`]) — is refused by name too,
/// [`Error::UnpricedFrame`].
///
/// Error discipline: stop the drain **only** on the transient signals
/// EAGAIN / EOF (the decoder has no more output for now). Every other
/// `ffmpeg_next::Error` — e.g. `InvalidData` from a corrupt packet — is a
/// real decode failure and is propagated, so a non-recoverable error
/// surfaces instead of being silently swallowed.
fn drain_into(
  sw: &mut SwDecoder,
  state: *const crate::ffi::CallbackState,
  queue: &mut ReplayQueue,
  budget: usize,
) -> std::result::Result<Drained, Error> {
  loop {
    queue.admit_parked(budget);
    if queue.full(budget) {
      return Ok(Drained::Full);
    }
    let mut tmp = alloc_av_video_frame()?;
    match sw.receive_frame(&mut tmp) {
      Ok(()) => {
        let bytes = footprint(&tmp).map_err(Error::UnpricedFrame)?;
        if bytes > budget {
          tracing::error!(
            bytes,
            budget,
            "mediadecode-ffmpeg: a decoded picture alone exceeds the software decoder's \
             replay budget; refusing it rather than queue past the budget",
          );
          return Err(Error::ReplayQueueFull(crate::ReplayQueueFull::new(
            bytes, budget,
          )));
        }
        // The admission is the picture's own size: one the queue cannot take
        // within its budget waits parked, and the drain stops for the caller.
        if !queue.frames.is_empty() && queue.bytes.saturating_add(bytes) > budget {
          queue.parked = Some((tmp, bytes));
          return Ok(Drained::Full);
        }
        queue.push_back(tmp, bytes);
      }
      // EAGAIN / EOF: no more output for now — stop draining, success.
      Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
        return Ok(Drained::Empty);
      }
      Err(ffmpeg_next::Error::Eof) => return Ok(Drained::Empty),
      // Any other error is a genuine decode failure — surface it so it is
      // not masked.
      Err(other) => return Err(crate::decoder::software_exit(state, other)),
    }
  }
}

/// The bytes a decoded picture holds, every allocation it owns priced: the
/// buffers its pixels reference — `buf[]` and `extended_buf` — or, for one
/// that references none, an upper bound on what its format and dimensions
/// allocate (`crate::footprint::video_frame_bytes`); every side data entry's
/// buffer (SEI payloads, ICC profiles, …), whatever the pixels weigh; the
/// frame's metadata and each side data entry's; `opaque_ref` and
/// `hw_frames_ctx`. Pricing the pixels alone let small pictures carrying
/// large side data fill memory under a small budget.
///
/// A picture holding an allocation of no stated size is refused by name
/// ([`UnpricedFrame`](crate::UnpricedFrame)): a `private_ref` — libavcodec's
/// own, which it clears before a frame leaves a decoder — and side data no
/// buffer reference owns. What the budget cannot price is never admitted as
/// costing nothing.
fn footprint(frame: &frame::Video) -> Result<usize, crate::UnpricedFrame> {
  use crate::UnpricedHolding;
  let refused = |holding| Err(crate::UnpricedFrame::new(holding));
  // SAFETY: `frame` is a live `AVFrame`; its buffer reference pointers and
  // their `size`, its side data table — each entry's buffer reference,
  // payload pointer, size and metadata, never its type, which is a bindgen
  // enum — its dictionaries, read through FFmpeg's iterator, and three plain
  // integers are read, and no reference into FFmpeg memory is kept.
  unsafe {
    let raw = frame.as_ptr();
    let referenced = |buf: *const ffmpeg_next::ffi::AVBufferRef| {
      if buf.is_null() { 0 } else { (*buf).size }
    };
    let mut pixels: usize = 0;
    for &buf in &(*raw).buf {
      pixels = pixels.saturating_add(referenced(buf));
    }
    let extended = (*raw).extended_buf;
    let count = usize::try_from((*raw).nb_extended_buf).unwrap_or(0);
    if !extended.is_null() {
      for index in 0..count {
        pixels = pixels.saturating_add(referenced(*extended.add(index)));
      }
    }
    if pixels == 0 {
      pixels = crate::footprint::video_frame_bytes((*raw).format, (*raw).width, (*raw).height)
        .unwrap_or(0);
    }
    if !(*raw).private_ref.is_null() {
      return refused(UnpricedHolding::PrivateRef);
    }
    let mut total = pixels
      .saturating_add(referenced((*raw).opaque_ref))
      .saturating_add(referenced((*raw).hw_frames_ctx))
      .saturating_add(dictionary_bytes((*raw).metadata));
    let entries = usize::try_from((*raw).nb_side_data).unwrap_or(0);
    let table = (*raw).side_data;
    if entries > 0 && table.is_null() {
      return refused(UnpricedHolding::SideData);
    }
    for index in 0..entries {
      let entry = *table.add(index);
      if entry.is_null() {
        return refused(UnpricedHolding::SideData);
      }
      let buf = core::ptr::read(core::ptr::addr_of!((*entry).buf));
      let data = core::ptr::read(core::ptr::addr_of!((*entry).data));
      let size = core::ptr::read(core::ptr::addr_of!((*entry).size));
      if buf.is_null() && (!data.is_null() || size > 0) {
        return refused(UnpricedHolding::SideData);
      }
      let metadata = core::ptr::read(core::ptr::addr_of!((*entry).metadata));
      total = total
        .saturating_add(referenced(buf))
        .saturating_add(dictionary_bytes(metadata));
    }
    Ok(total)
  }
}

/// The bytes an `AVDictionary`'s entries hold: each entry and its two
/// NUL-terminated strings. Zero for a null dictionary.
///
/// # Safety
/// `dict` is null or a live `AVDictionary`.
unsafe fn dictionary_bytes(dict: *const ffmpeg_next::ffi::AVDictionary) -> usize {
  let mut total: usize = 0;
  if dict.is_null() {
    return total;
  }
  let mut entry: *const ffmpeg_next::ffi::AVDictionaryEntry = core::ptr::null();
  loop {
    // SAFETY: `dict` is live (the caller's promise) and `entry` is null or
    // the entry this iterator answered last.
    entry = unsafe { ffmpeg_next::ffi::av_dict_iterate(dict, entry) };
    if entry.is_null() {
      return total;
    }
    // SAFETY: a live entry's key and value are NUL-terminated strings the
    // dictionary owns.
    let (key, value) = unsafe {
      (
        core::ffi::CStr::from_ptr((*entry).key).to_bytes().len(),
        core::ffi::CStr::from_ptr((*entry).value).to_bytes().len(),
      )
    };
    total = total
      .saturating_add(core::mem::size_of::<ffmpeg_next::ffi::AVDictionaryEntry>())
      .saturating_add(key + 1)
      .saturating_add(value + 1);
  }
}

/// A software decoder's `has_b_frames`: how many pictures its reorder buffer
/// holds back. FFmpeg raises it as it discovers reordering, and the
/// parameters a keyframe activates can lower it.
fn reorder_depth(sw: &SwDecoder) -> usize {
  // SAFETY: `sw` is a live opened software decoder; one plain integer field
  // is read and the pointer is not kept.
  usize::try_from(unsafe { (*sw.as_ptr()).has_b_frames }).unwrap_or(0)
}

fn open_sw_decoder(
  parameters: &Parameters,
  limits: DecoderLimits,
  pkt_timebase: Option<Timebase>,
) -> Result<SwDecoder, Error> {
  // Use the checked codec-context builder — ffmpeg-next's
  // `Context::from_parameters` calls `Context::new()` which doesn't
  // null-check `avcodec_alloc_context3`'s return value before
  // running `avcodec_parameters_to_context` against it. Under
  // memory pressure that's C-level UB; `build_codec_context`
  // surfaces the OOM as an error instead.
  let (mut ctx, callback_state) = build_codec_context(parameters, limits, pkt_timebase)?;
  // **The threads, before the open that reads them.** Every software
  // session decodes on the threads its limits ask for — opened on
  // purpose, at open-time exhaustion, or at the first keyframe after a
  // mid-stream fallback, whose two transactions commit a one-thread
  // decoder opened here too (see `fall_back_to_sw_inner` and
  // `open_after_drain`). See [`crate::Threads`] for the cost and
  // `request_threads` for why the allocator judge is safe on
  // libavcodec's worker threads.
  crate::decoder::request_threads(&mut ctx, limits.threads());
  // **No unrecovered picture out, ever.** The post-commit resync's proof for
  // H.264 is FFmpeg withholding them
  // (`CarrierVideoStreamDecoder::check_resync_proof`): with neither flag,
  // FFmpeg's H.264 decoder outputs only the pictures it has recovered.
  withhold_unrecovered(&mut ctx);
  #[cfg(test)]
  unrecovered_output::apply(&mut ctx);
  // Opened without forming a bindgen enum from FFmpeg memory: the codec
  // is resolved off a raw `codec_id`, and the medium is proved off a raw
  // `codec_type`. See `crate::decoder::ensure_codec_type`.
  let codec = sw_codec(parameters)?;
  // The implementation, by name: the resync's proof is bound to it.
  let implementation = codec.name();
  #[cfg(test)]
  let implementation = sw_implementation::named().unwrap_or(implementation);
  let native_h264 = implementation == NATIVE_H264;
  let opened = ctx.decoder().open_as(codec).map_err(Error::Ffmpeg)?;
  // Checked in every build, after the open, which is what FFmpeg reads: a
  // decoder that would output unrecovered pictures is closed and refused by
  // name, never opened on the strength of a debug assertion.
  refuse_unrecovered_output(&opened)?;
  crate::decoder::ensure_video_codec_type(&opened)?;
  Ok(SwDecoder {
    decoder: ffmpeg_next::decoder::Video(opened),
    native_h264,
    _callback_state: callback_state,
    #[cfg(test)]
    _live: live_sw::Guard::new(),
  })
}

/// FFmpeg's own H.264 decoder, by name.
const NATIVE_H264: &str = "h264";

/// The software decoder for `parameters`' codec: for H.264, FFmpeg's own
/// [`NATIVE_H264`], by name — the implementation whose output gate the
/// withheld resync proof stands on — and otherwise, or where it is not built
/// in, the one `avcodec_find_decoder` answers (`crate::decoder::find_decoder`),
/// which may be any implementation of the codec. The session reads which it
/// opened ([`SwDecoder::native_h264`]).
fn sw_codec(parameters: &Parameters) -> Result<ffmpeg_next::Codec, Error> {
  // SAFETY: the parameters' pointer, only read; `codec_id` is read as the
  // 32-bit integer the field holds, never formed into a bindgen enum.
  let codec_id = unsafe {
    let raw = parameters.as_ptr();
    (!raw.is_null()).then(|| core::ptr::read(core::ptr::addr_of!((*raw).codec_id) as *const i32))
  };
  if codec_id == Some(crate::CodecId::H264.raw())
    && let Some(native) = ffmpeg_next::decoder::find_by_name(NATIVE_H264)
  {
    return Ok(native);
  }
  crate::decoder::find_decoder(parameters)
}

/// Test-only: the name the next software video decoder opened is taken to
/// have, in place of its own — how a session opened on another
/// implementation of a codec reads.
#[cfg(test)]
pub(crate) mod sw_implementation {
  use core::cell::Cell;

  std::thread_local! {
    static NAMED: Cell<Option<&'static str>> = const { Cell::new(None) };
  }

  /// The next open is taken to be of the implementation `name`.
  pub(crate) fn name_next(name: &'static str) {
    NAMED.with(|named| named.set(Some(name)));
  }

  /// The name armed for this open, once.
  pub(super) fn named() -> Option<&'static str> {
    NAMED.with(Cell::take)
  }
}

/// Clears `AV_CODEC_FLAG_OUTPUT_CORRUPT` and `AV_CODEC_FLAG2_SHOW_ALL` on a
/// codec context about to be opened: a session's software decoder outputs
/// no picture before its recovery (see `open_sw_decoder`, which checks the
/// opened decoder again with [`refuse_unrecovered_output`]).
fn withhold_unrecovered(ctx: &mut ffmpeg_next::codec::Context) {
  // SAFETY: `ctx` owns a live, not yet opened `AVCodecContext`; two plain
  // integer fields are read and written, and no reference into it is kept.
  unsafe {
    let raw = ctx.as_mut_ptr();
    (*raw).flags &= !(ffmpeg_next::ffi::AV_CODEC_FLAG_OUTPUT_CORRUPT as core::ffi::c_int);
    (*raw).flags2 &= !(ffmpeg_next::ffi::AV_CODEC_FLAG2_SHOW_ALL as core::ffi::c_int);
  }
}

/// Refuses an opened decoder that would output pictures before their
/// recovery — `AV_CODEC_FLAG_OUTPUT_CORRUPT` or `AV_CODEC_FLAG2_SHOW_ALL` set
/// — by name ([`Error::UnrecoveredOutput`]), with the flags it found.
fn refuse_unrecovered_output(opened: &ffmpeg_next::decoder::Opened) -> Result<(), Error> {
  // SAFETY: `opened` owns a live `AVCodecContext`; two plain integer fields
  // are read.
  let (output_corrupt, show_all) = unsafe {
    let raw = opened.as_ptr();
    (
      (*raw).flags & ffmpeg_next::ffi::AV_CODEC_FLAG_OUTPUT_CORRUPT as core::ffi::c_int != 0,
      (*raw).flags2 & ffmpeg_next::ffi::AV_CODEC_FLAG2_SHOW_ALL as core::ffi::c_int != 0,
    )
  };
  if output_corrupt || show_all {
    return Err(Error::UnrecoveredOutput(crate::UnrecoveredOutput::new(
      output_corrupt,
      show_all,
    )));
  }
  Ok(())
}

/// Test-only: the next software video decoder opened has the named flags
/// set on its codec context after the session clears them — what an open
/// that leaves either set looks like to the check after it.
#[cfg(test)]
pub(crate) mod unrecovered_output {
  use core::cell::Cell;

  std::thread_local! {
    static ARMED: Cell<(bool, bool)> = const { Cell::new((false, false)) };
  }

  /// The next open sets `AV_CODEC_FLAG_OUTPUT_CORRUPT` when
  /// `output_corrupt`, and `AV_CODEC_FLAG2_SHOW_ALL` when `show_all`.
  pub(crate) fn arm(output_corrupt: bool, show_all: bool) {
    ARMED.with(|armed| armed.set((output_corrupt, show_all)));
  }

  /// Sets what is armed on `ctx`, once.
  pub(super) fn apply(ctx: &mut ffmpeg_next::codec::Context) {
    let (output_corrupt, show_all) = ARMED.with(|armed| armed.replace((false, false)));
    // SAFETY: `ctx` owns a live, not yet opened `AVCodecContext`; two plain
    // integer fields are written, and no reference into it is kept.
    unsafe {
      let raw = ctx.as_mut_ptr();
      if output_corrupt {
        (*raw).flags |= ffmpeg_next::ffi::AV_CODEC_FLAG_OUTPUT_CORRUPT as core::ffi::c_int;
      }
      if show_all {
        (*raw).flags2 |= ffmpeg_next::ffi::AV_CODEC_FLAG2_SHOW_ALL as core::ffi::c_int;
      }
    }
  }
}

/// Payload for [`VideoDecodeError::PostCommitNeverResynced`].
///
/// A **post-commit** HW->SW fallback degraded the stream (dropping the
/// bounded span up to the next keyframe), and the software decoder reached
/// EOF without resyncing: no key-flagged packet was fed across the gap, or
/// the pictures out after one never proved it — for H.264 none came out, for
/// the other codecs they never passed the reorder bound. The "bounded,
/// logged gap" the post-commit path promises did not materialise, so the
/// loss is surfaced loudly here instead of being silently swallowed as a
/// clean end-of-stream.
///
/// It is returned once, as the `Err` of the `receive_frame` that reaches the
/// end, after every picture the decoder produced was delivered; the next
/// `receive_frame` answers `Ended`.
///
/// The packets the software decoder took across the gap are counted in two
/// parts, either side of the first key-flagged packet that anchored the
/// resync: [`Self::packets_before_anchor`], the fallback window, and
/// [`Self::packets_unproven`], decoded and their pictures delivered but never
/// proved to come from after the gap — through an un-anchor, and any anchor
/// after the first. The first anchor itself is in neither count;
/// [`Self::anchor_seen`] says whether there was one. What a resync proves,
/// at any anchor, is the pictures a decoder started at that random-access
/// point produces — not a bit-for-bit match with a decode that ran through
/// the gap: an approximate H.264 recovery point (`exact_match_flag` 0)
/// anchors as an exact one does, its flags reported in a debug trace. A packet the decoder
/// refused with an error was reported by that error, and is counted in
/// neither.
#[derive(Debug)]
pub struct PostCommitNeverResynced {
  packets_before_anchor: u64,
  packets_unproven: u64,
  anchor_seen: bool,
}

impl PostCommitNeverResynced {
  /// Constructs a `PostCommitNeverResynced` payload.
  #[inline]
  pub const fn new(packets_before_anchor: u64, packets_unproven: u64, anchor_seen: bool) -> Self {
    Self {
      packets_before_anchor,
      packets_unproven,
      anchor_seen,
    }
  }

  /// Packets the software decoder took across the gap before a key-flagged
  /// packet anchored the resync: the fallback window.
  #[inline]
  pub const fn packets_before_anchor(&self) -> u64 {
    self.packets_before_anchor
  }

  /// Packets the software decoder took after the first anchor with the
  /// resync never proved: decoded, their pictures delivered, never shown to
  /// come from after the gap.
  #[inline]
  pub const fn packets_unproven(&self) -> u64 {
    self.packets_unproven
  }

  /// Whether a key-flagged packet anchored the resync at all.
  #[inline]
  pub const fn anchor_seen(&self) -> bool {
    self.anchor_seen
  }
}

impl core::fmt::Display for PostCommitNeverResynced {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    write!(
      f,
      "post-commit HW->SW fallback never resynced before EOF: {} packets before a keyframe",
      self.packets_before_anchor
    )?;
    if self.anchor_seen {
      write!(
        f,
        ", {} after it with the resync never proved",
        self.packets_unproven
      )
    } else {
      f.write_str(", and no keyframe after them")
    }
  }
}

impl std::error::Error for PostCommitNeverResynced {}

/// Error type for [`FfmpegVideoStreamDecoder`] — **faults and the
/// send-side refusal**.
///
/// Every arm here is something that went wrong or something the push
/// face declined. The drain's *needs input* and *ended* are
/// [`Received`] states out of `receive_frame`; they used to arrive as
/// `Decode(Ffmpeg(Other { errno: EAGAIN }))` and `Decode(Ffmpeg(Eof))`,
/// which is to say they had no name at this tier at all.
/// [`Self::PostCommitNeverResynced`] is the deliberate exception on the
/// end-of-stream road: it is not "the stream ended", it is "the stream
/// ended and the tail was lost", which is a fault.
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
#[derive(thiserror::Error, Debug, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
#[non_exhaustive]
pub enum VideoDecodeError {
  /// The wrapped decoder (HW or SW) reported an error.
  #[error(transparent)]
  Decode(#[from] Error),
  /// Frame conversion from FFmpeg's native types to mediadecode's
  /// types failed.
  #[error(transparent)]
  Convert(#[from] ConvertError),
  /// A **post-commit** HW->SW fallback degraded the stream and the
  /// software decoder reached EOF without resyncing — no key-flagged packet
  /// fed across the gap, or the pictures out after one never proved it (for
  /// H.264 none came out; for the other codecs they never passed the reorder
  /// bound). Returned once, after every picture was delivered; the
  /// next `receive_frame` answers `Ended`; see the payload's own
  /// documentation.
  #[error(transparent)]
  PostCommitNeverResynced(#[from] PostCommitNeverResynced),
}

#[cfg(test)]
mod tests;

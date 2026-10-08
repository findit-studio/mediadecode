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
//!   settled (drained to "needs input" since the last packet). Both proofs
//!   are invariants of libavcodec's own decoders, so the post-commit fallback
//!   refuses, by name, a software decoder that wraps another implementation
//!   (`AVCodec.wrapper_name` set: `h264_cuvid`, `libdav1d`, …), which
//!   publishes no reorder bound and withholds nothing
//!   ([`Error::ResyncUnprovable`]). For H.264 on FFmpeg's own `h264`
//!   decoder, the first picture out after the anchor closes the gap: opened
//!   with neither `AV_CODEC_FLAG_OUTPUT_CORRUPT` nor `AV_CODEC_FLAG2_SHOW_ALL`
//!   (an open that finds either set is refused, [`Error::UnrecoveredOutput`]),
//!   it outputs only pictures its recovery tracking has marked recovered, and
//!   a decoder opened cold across the gap starts with nothing recovered; an
//!   anchor after a decode error across the gap takes the reorder bound. For
//!   every other codec the only pictures from before the
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
  ///
  /// **Their extradata is the active one.** A packet carrying
  /// `AV_PKT_DATA_NEW_EXTRADATA` — a container's sample description switch,
  /// a codec-private change — changes the stream's parameters from that
  /// packet on, and FFmpeg's H.264 and HEVC decoders apply it before they
  /// decode the packet.
  /// Once a decoder takes such a packet its extradata replaces these, in
  /// place ([`NewExtradata`]), so the keyframe rule reads the framing the
  /// decoder parses ([`Self::keyframe_rule`]) and every decoder opened
  /// later — a post-commit fallback's, a switch's — starts on the stream's
  /// current parameters. While the hardware probe records its history, what
  /// the hardware takes waits in [`Self::probe_extradata`] instead.
  parameters: Parameters,
  /// The extradata of the last packet carrying `AV_PKT_DATA_NEW_EXTRADATA`
  /// the hardware took while its probe recorded a history: a probe-era
  /// fallback replays that history from the parameters it started on,
  /// re-applying this in order, so it is not installed in
  /// [`Self::parameters`] yet. It is installed once nothing will replay it —
  /// at the hardware's first send after the probe committed, at a
  /// post-commit fallback and at a flush, which clears the history — and
  /// dropped by a probe-era fallback, which re-applies it.
  probe_extradata: Option<NewExtradata>,
  /// Set, with what left it so, when whether the decoder serving applied a
  /// packet's `AV_PKT_DATA_NEW_EXTRADATA` cannot be told
  /// ([`crate::ExtradataDoubt`]): a decoder refused the packet with an error
  /// that does not say ([`Taken::Unknown`]), reported one while the
  /// extradata was provisional ([`Self::extradata_provisional`]), or a flush
  /// dropped the packet unread. FFmpeg's H.264 and HEVC decoders apply a
  /// packet's new extradata as they begin to decode it, so a packet they
  /// decoded and reported failed has changed their framing, and one dropped
  /// before its decode — or, on a frame-threaded decoder, still waiting
  /// behind an earlier packet's error — has not; here which is not known. No
  /// error libavcodec reports says, invalid data included — only this
  /// crate's own refusals minted while the packet's picture was allocated do
  /// ([`taken_despite`]) — so a corrupt packet at an extradata change leaves
  /// the session's extradata unknown. Until a packet carrying a new
  /// extradata is taken, no H.264 or HEVC packet is read as a resync anchor
  /// or a switch point under the session's extradata, no switch opens a
  /// decoder on it, and a decoder the session must open on it — a
  /// post-commit fallback's, a reopen — is refused by name
  /// ([`Error::ExtradataUnknown`]); a packet carrying its own new extradata
  /// is read, and opened on, under that. A flush keeps it: the flushed
  /// decoder frames the stream by what it applied. A probe-era fallback's
  /// replay sets it from the history it replays.
  extradata_unknown: Option<crate::ExtradataDoubt>,
  /// `true` while the active extradata — installed in [`Self::parameters`],
  /// or kept in [`Self::probe_extradata`] — came with a packet the decoder
  /// serving accepted but has not been seen to read. libavcodec takes a
  /// packet into its input slot (`buffer_pkt`) and may decode it only at a
  /// later call: when a picture it decoded earlier still waits to be
  /// received, or a frame thread holds results to hand out first. A picture
  /// coming out proves nothing, since it can be that waiting one. What does:
  /// the decoder answering the end; the decoder answering "needs input",
  /// which it does with its input slot empty, where it decodes what a call
  /// hands it inside that call ([`SwDecoder::decodes_in_step`]; the
  /// hardware) — a frame-threaded decoder answers it as soon as a worker has
  /// the packet, decoded or not ([`read_every_packet`]); a later packet
  /// taken by a decoder that decodes in step, which libavcodec takes only
  /// into an empty slot; a decoder opened on the parameters in place of the
  /// one that took it. A flush while it is set drops the packet unread — the
  /// decoder kept, its framing the one before — and an error reported while
  /// it is set may be that packet's own (FFmpeg's HEVC decoder reports there
  /// a new extradata it could not parse): either leaves the extradata
  /// unknown ([`Self::extradata_unknown`]).
  extradata_provisional: bool,
  /// `true` once an H.264 sequence parameter set the session read — in its
  /// codec parameters, in a new extradata, among the units of a keyframe —
  /// permits arbitrary slice order (`access::KeyframeRule::H264`). For good:
  /// from then on no H.264 packet is clean or anchors, so no switch fires and
  /// an open post-commit gap ends escalated by name; said once ([`Self::note_sps`]).
  h264_aso: bool,
  /// `true` once an HEVC video parameter set the session read — in its
  /// codec parameters, in a new extradata, among the units of a keyframe —
  /// declares an auxiliary layer, or the software decoder serving negotiated
  /// an output format with alpha (`access::KeyframeRule::Hevc`): FFmpeg's
  /// HEVC decoder decodes that layer beside the base one, as the alpha plane
  /// of every picture, and nothing here proves where its pictures start. For
  /// good: from then on no HEVC packet is clean or anchors, so no switch
  /// fires and an open post-commit gap ends escalated by name; said once
  /// ([`Self::note_vps`]).
  hevc_alpha: bool,
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
  fail_after_submit: Option<ffmpeg_next::Error>,
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
  /// How the anchored resync is proved ([`resync_proof`]): FFmpeg
  /// withholding every picture it has not recovered (H.264), so the first
  /// picture out closes the gap, or the reorder bound — on one of
  /// libavcodec's own decoders; `None`, nothing closing the gap, on an
  /// implementation that wraps another, and before an anchor.
  anchor_proof: Option<access::Proof>,
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
  /// Whether the implementation opened is one of libavcodec's own decoders:
  /// its `AVCodec.wrapper_name` is null. The post-commit resync's proofs
  /// ([`resync_proof`]) are invariants of those decoders alone — FFmpeg's
  /// `h264` withholding every picture it has not recovered, the reorder
  /// bound `has_b_frames` that libavcodec's decoders publish — and an
  /// implementation that wraps another (`h264_cuvid`, `h264_qsv`,
  /// `h264_v4l2m2m`, `h264_mediacodec`, `libdav1d`, `libvpx-vp9`, …) keeps
  /// neither, so a post-commit fallback onto one is refused
  /// ([`Error::ResyncUnprovable`]).
  native: bool,
  /// What the implementation opened wraps, as its `wrapper_name` reads —
  /// `cuvid`, `libdav1d` — for that refusal's message alone; `None` for a
  /// native one, or a name that does not read. [`Self::native`] decides.
  wrapper: Option<&'static str>,
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
  ///
  /// A picture refused while the decoder decoded the packet inside this
  /// call, which it concealed and reported no error for
  /// ([`Self::concealed_refusal`]), is the packet's own failure: this
  /// answers it as the error the allocator judge gave FFmpeg, which every
  /// caller's funnel names ([`crate::decoder::software_exit`]) — the packet
  /// taken, as one the decoder reports failed is.
  pub(crate) fn submit(&mut self, packet: &Packet) -> Result<(), ffmpeg_next::Error> {
    self.decoder.send_packet(packet)?;
    #[cfg(test)]
    live_sw::note_sent();
    self.concealed_refusal().map_or(Ok(()), Err)
  }

  /// Tells this decoder the stream ended. Every end this module gives a
  /// software decoder goes through here, so the test census counts each
  /// one the decoder answered. The end runs no decode — FFmpeg 9's
  /// `avcodec_send_packet` decodes inside a submission only before draining
  /// starts (decode.c) — so it latches no refusal.
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

  /// **A frame refusal the call just made latched, on a decoder that
  /// decodes in step** ([`Self::decodes_in_step`]): the error the allocator
  /// judge answered FFmpeg, `AVERROR(EINVAL)`, for the caller's funnel to
  /// name. Such a decoder decodes only inside the call that hands it a
  /// packet or asks it for a picture, so the refusal is that call's own.
  /// FFmpeg's H.264 decoder conceals a picture it could not allocate where a
  /// later picture of the same packet starts — `decode_nal_units` drops the
  /// slice's error unless `AV_EF_EXPLODE` is set, and `h264_decode_frame`
  /// reports "no frame" only where no picture started (h264dec.c) — so the
  /// call can report success, or give another picture, with the refusal
  /// latched. Read here, in that call, a refusal is never left for a later
  /// call to name as its own, or to clear unnamed. `None` on a decoder that
  /// does not decode in step: its threads latch for packets whose answers
  /// are still to come ([`Self::funnel`]).
  fn concealed_refusal(&self) -> Option<ffmpeg_next::Error> {
    (self.decodes_in_step() && crate::ffi::frame_budget_declined(self.state())).then_some(
      ffmpeg_next::Error::Other {
        errno: libc::EINVAL,
      },
    )
  }

  /// The callback state the funnel collects a latched frame refusal from,
  /// for `answer`, which this decoder gave while the session was in
  /// `phase` ([`crate::decoder::software_exit`]). On a decoder that decodes
  /// in step, every answer's: a refusal latched in a call is that call's.
  /// On one that does not, back pressure collects none. A frame-threaded
  /// decoder decodes each packet on a worker, which allocates — and latches
  /// — whenever it runs (`frame_worker_thread`, pthread_frame.c), and an
  /// implementation that wraps another keeps a pipeline of its own; a
  /// refusal latched there belongs to a packet still in hand, and that
  /// packet's own answer names it — its error, or, for a picture concealed
  /// with no error to come, the end of the drain, the first answer the
  /// decoder gives with nothing left in hand. Null, collecting nothing, for
  /// that.
  fn funnel(
    &self,
    answer: ffmpeg_next::Error,
    phase: crate::decoder::SessionPhase,
  ) -> *const crate::ffi::CallbackState {
    let back_pressure = phase.accepts_input()
      && matches!(answer, ffmpeg_next::Error::Other { errno } if errno == ffmpeg_next::error::EAGAIN);
    if back_pressure && !self.decodes_in_step() {
      core::ptr::null()
    } else {
      self.state()
    }
  }

  /// Whether the output format this decoder negotiated carries an alpha
  /// component — for HEVC, that FFmpeg decodes an auxiliary layer as the
  /// alpha plane of its pictures. `AVCodecContext.pix_fmt` is read as the
  /// integer it holds, never formed into a bindgen enum, and mapped through
  /// the crate's own table ([`crate::boundary::from_av_pixel_format`]).
  fn outputs_alpha(&self) -> bool {
    // SAFETY: `self` is a live opened decoder; `pix_fmt` is read as the
    // 32-bit integer the field holds, and not kept.
    let raw =
      unsafe { core::ptr::read(core::ptr::addr_of!((*self.as_ptr()).pix_fmt).cast::<i32>()) };
    crate::pixdesc::carries_alpha(&crate::boundary::from_av_pixel_format(raw))
  }

  /// Whether this decoder decodes, inside a submission, the packet the
  /// submission queues: libavcodec's own implementation ([`Self::native`])
  /// with no frame threading active. A frame-threaded decoder hands a packet
  /// to a thread and reports what an earlier packet's thread met, and an
  /// implementation that wraps another keeps a pipeline of its own; on
  /// either, a picture refused during a submission may be an earlier
  /// packet's ([`taken_despite`]).
  fn decodes_in_step(&self) -> bool {
    // SAFETY: `self` is a live opened decoder; `active_thread_type` is a
    // plain `c_int`, read and not kept.
    let active = unsafe { (*self.as_ptr()).active_thread_type };
    self.native && active & ffmpeg_next::ffi::FF_THREAD_FRAME == 0
  }

  /// The refusal a post-commit fallback onto this decoder meets where its
  /// implementation is not libavcodec's own ([`Self::native`]): the stream's
  /// codec, and the implementation and its wrapper by name.
  fn resync_unprovable(&self) -> crate::ResyncUnprovable {
    // SAFETY: `self` is a live opened decoder. Its context's `codec_id` is
    // read as the 32-bit integer the field holds, never formed into a
    // bindgen enum; its `codec` is null or the `AVCodec` it was opened with,
    // an entry of libavcodec's static codec list, whose `name` pointer is
    // read through `addr_of!` without forming a reference to the entry
    // (whose `type` and `id` are bindgen enums) and is a string literal
    // valid for the process, as `table_text` requires.
    let (codec_id, implementation) = unsafe {
      let raw = self.as_ptr();
      let codec_id = core::ptr::read(core::ptr::addr_of!((*raw).codec_id) as *const i32);
      let codec = (*raw).codec;
      let implementation = if codec.is_null() {
        None
      } else {
        crate::ffi::table_text(
          core::ptr::addr_of!((*codec).name).read(),
          IMPLEMENTATION_NAME_MAX_BYTES,
        )
      };
      (codec_id, implementation)
    };
    crate::ResyncUnprovable::new(
      crate::CodecId::from_raw(codec_id),
      implementation,
      self.wrapper,
    )
  }
}

/// Upper bound on the NUL search for a codec implementation's name or its
/// wrapper's — FFmpeg's run to a couple of dozen bytes; the cap exists only
/// so that a corrupt table cannot turn the walk into an unbounded read.
const IMPLEMENTATION_NAME_MAX_BYTES: usize = 128;

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
      probe_extradata: None,
      extradata_unknown: None,
      extradata_provisional: false,
      h264_aso: false,
      hevc_alpha: false,
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
      anchor_proof: None,
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
      fail_after_submit: None,
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
      self.limits,
      &self.parameters,
      &mut progress,
    )?;
    // Commit: only after replay, any EOF forwarding, AND the final drain
    // succeeded — or the queue reached its budget first — do we move the
    // new SW decoder and queue into `self`.
    let in_step = sw.decodes_in_step();
    self.sw_replay_frames.append(&mut local_replay);
    self.state = DecodeState::Sw(sw);
    // The history was replayed from the parameters it started on, its own
    // new extradata re-applied in order: the last the decoder took is the
    // active one, what the hardware took is not installed again, and what
    // its refusals left in doubt is the replaced decoder's, not this one's.
    self.probe_extradata = None;
    self.extradata_unknown = None;
    let installed = match progress.extradata.take() {
      Some(extradata) => {
        extradata.install(&mut self.parameters);
        true
      }
      None => false,
    };
    // A replay that proved its record read ([`Replay::read`]), or whose
    // decoder answered "needs input" last where it decodes in step
    // ([`read_every_packet`]), has read every packet it took; one the budget
    // stopped may hold its last unread.
    self.extradata_provisional =
      installed && !(progress.read || (drained == Drained::Empty && in_step));
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
    let in_step = sw.decodes_in_step();
    let mut progress = Replay::default();
    let replayed = replay_history(
      sw,
      self.pending_history.make_contiguous(),
      self.pending_eof,
      &mut self.sw_replay_frames,
      self.limits,
      &self.parameters,
      &mut progress,
    );
    self.pending_history.drain(..progress.fed);
    // A packet the decoder took, the failing one among them where its
    // refusal says the decoder decoded it: its new extradata, if it carried
    // one, is the active one — read where the round proved it so
    // ([`Replay::read`]: a later packet taken in step, or its own refusal
    // saying it was decoded) or the decoder answered "needs input" last, on
    // a decoder that decodes in step ([`read_every_packet`]); provisional
    // otherwise. The proof is applied first: an error the round met after
    // it is no longer the unread packet's. A failing one whose refusal does
    // not say — a decode error among them — leaves it unknown, as does an
    // error the round met while one was provisional.
    let read = progress.read || (matches!(replayed, Ok(Drained::Empty)) && in_step);
    if let Some(extradata) = progress.extradata.take() {
      self.took_extradata(extradata, false, read);
    } else if read {
      self.extradata_read();
    }
    if let Some(doubt) = progress.unknown {
      self.extradata_in_doubt(doubt);
    } else if let Err(error) = &replayed {
      self.reported_while_provisional(doubt_of(error));
    }
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
  ///
  /// # On the packet's extradata
  ///
  /// Where the packet the new decoder is opened for carries a new extradata
  /// (`replacement`), the decoder opens on a copy of the session's parameters
  /// carrying it ([`Self::carrying`]), committed once the decoder serves
  /// ([`Self::commit_opened`]) — never on the retained parameters first,
  /// whose record may be one a decoder cannot open on.
  fn open_after_drain(&mut self, replacement: Option<NewExtradata>) -> Result<(), Error> {
    self.state = DecodeState::SwClosed;
    let carrying = self.carrying(replacement)?;
    let parameters = carrying.as_ref().unwrap_or(&self.parameters);
    let one_thread = self.limits.with_threads(crate::Threads::Single);
    let sw = if self.sw_threads_pending {
      self.sw_threads_pending = false;
      // The one open on the session's own threads. In tests it can be made
      // to fail, and is counted.
      #[cfg(test)]
      let refused = {
        self.threaded_opens += 1;
        self.fail_threaded_opens
      };
      #[cfg(not(test))]
      let refused = false;
      let threaded = if refused {
        Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
          errno: libc::ENOMEM,
        }))
      } else {
        open_sw_decoder(parameters, self.limits, Some(self.time_base))
      };
      match threaded {
        Ok(sw) => sw,
        Err(error) => {
          tracing::warn!(
            %error,
            "mediadecode-ffmpeg: the software decoder could not be opened on the session's \
             threads at a keyframe; the session stays on one thread for good",
          );
          open_sw_decoder(parameters, one_thread, Some(self.time_base))?
        }
      }
    } else {
      open_sw_decoder(parameters, one_thread, Some(self.time_base))?
    };
    self.state = DecodeState::Sw(sw);
    self.sw_output_settled = true;
    self.commit_opened(carrying);
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
    if drain_into(sw, state, &mut self.sw_replay_frames, budget)? == Drained::Full {
      return Ok(false);
    }
    // Told the end and drained, the decoder has nothing left in hand, and it
    // closes next: a refusal still latched — a picture concealed with no
    // error to come — is named now.
    match crate::decoder::frame_budget_declination_of(state) {
      Some(refusal) => Err(refusal),
      None => Ok(true),
    }
  }

  /// **A drained switch, completed**: the drained decoder is closed and one
  /// on the session's threads opened ([`Self::open_after_drain`]), on the
  /// new extradata the keyframe carries, if it carries one.
  fn finish_restart(&mut self, replacement: Option<NewExtradata>) -> Result<(), Error> {
    if self.restart.take().is_none() {
      return Ok(());
    }
    self.open_after_drain(replacement)
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
    // The extradata this packet carries, copied before anything of the
    // session moves: it becomes the active extradata once the decoder takes
    // the packet — or once a decoder opened on it for the packet serves
    // ([`Self::open_after_drain`]) — and until then the packet is read under
    // it ([`Self::rule_for`]).
    let mut extradata = NewExtradata::of(
      pkt,
      &self.parameters,
      self.limits.max_codec_parameter_bytes(),
    )
    .map_err(VideoDecodeError::Decode)?;
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
      self
        .finish_restart(extradata.take())
        .map_err(VideoDecodeError::Decode)?;
    }
    if matches!(self.state, DecodeState::SwClosed) {
      // No decoder open: one is opened, on one thread for good — on the
      // session's parameters, none while their extradata is unknown unless
      // this packet carries its own, which it opens on. The packet stays the
      // caller's.
      if let Some(doubt) = self.extradata_unknown
        && new_extradata(pkt).is_none()
      {
        return Err(VideoDecodeError::Decode(Error::ExtradataUnknown(
          crate::ExtradataUnknown::new(doubt),
        )));
      }
      self
        .open_after_drain(extradata.take())
        .map_err(VideoDecodeError::Decode)?;
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
    let in_step = sw.decodes_in_step();
    let submitted = sw.submit(pkt);
    #[cfg(test)]
    let submitted = match submitted {
      Ok(()) => self.fail_after_submit.take().map_or(Ok(()), Err),
      other => other,
    };
    if let Err(e) = submitted {
      // Funnel, then gate. **Nothing below runs on back pressure**,
      // which is the point of returning here rather than falling
      // through: a packet libavcodec did not take must not be
      // counted across the resync gap or taken for an anchor, or a
      // caller's honest re-offer would double-count it. A picture refused
      // while the decoder decoded this packet in step, which it concealed,
      // arrives here as the packet's own failure ([`SwDecoder::submit`]).
      return match crate::decoder::software_send(sw.funnel(e, phase), e, phase) {
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
          // Its own picture refused while the decoder decoded it in step: the
          // decoder read every packet before it. Any other error may be an
          // earlier packet's, a provisional extradata's unread one among them.
          let taken = taken_despite(e, &error, in_step);
          match taken {
            Taken::Yes => self.extradata_read(),
            Taken::Unknown(doubt) => self.reported_while_provisional(doubt),
            Taken::No => {}
          }
          // The new extradata it carries is the session's where the refusal
          // says the decoder decoded the packet, and unknown where it does
          // not say: a decode error does not.
          if let Some(extradata) = extradata {
            self.refused_with_extradata(extradata, taken, false);
          }
          Err(VideoDecodeError::Decode(error))
        }
      };
    }
    #[cfg(test)]
    if let Some(depth) = self.reorder_on_submit.take() {
      self.reorder_override = Some(depth);
    }
    self.sw_output_settled = false;
    // libavcodec takes a packet only into an empty input slot: a decoder
    // that decodes in step has read every packet before this one.
    if in_step {
      self.extradata_read();
    }
    // The decoder took it: its extradata is the stream's now, provisionally
    // until the decoder is seen to read it.
    if let Some(extradata) = extradata {
      self.took_extradata(extradata, false, false);
    }
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
      && crate::decoder::find_decoder(&self.parameters).is_ok_and(|codec| {
        codec.capabilities().intersects(
          Capabilities::FRAME_THREADS | Capabilities::SLICE_THREADS | Capabilities::OTHER_THREADS,
        )
      })
  }

  /// What `pkt` is as a post-commit resync anchor: a key-flagged packet the
  /// bitstream proves a random-access point, where this crate can read it —
  /// see [`access::KeyframeRule::anchor`] — read by the rule `pkt` is decoded
  /// under ([`Self::rule_for`]); none where that rule is unknown. A stale
  /// key flag, or a picture before the random-access one, anchors nothing.
  fn anchor(&self, pkt: &Packet) -> Option<access::Anchor> {
    let rule = self.rule_for(pkt)?;
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
  /// its flag or not. None while the session's extradata is unknown
  /// ([`Self::extradata_unknown`]) but a packet carrying its own: a switch
  /// opens a decoder on the session's parameters, and that decoder takes
  /// the packet's.
  fn switch_point(&self, pkt: &Packet) -> bool {
    if self.extradata_unknown.is_some() && new_extradata(pkt).is_none() {
      return false;
    }
    self.rule_for(pkt).is_some_and(|rule| {
      (pkt.is_key() || rule.every_packet()) && (self.seeked || self.clean_keyframe(pkt))
    })
  }

  /// The keyframe rule for this session's stream, read off its codec
  /// parameters: the codec, and how its active extradata packs NAL units
  /// ([`Self::parameters`]).
  fn keyframe_rule(&self) -> access::KeyframeRule {
    // SAFETY: the owned, deep-copied parameters' pointer, only read.
    let raw = unsafe { self.parameters.as_ptr() };
    if raw.is_null() {
      return access::KeyframeRule::Reordering;
    }
    // SAFETY: `raw` is that live pointer (checked non-null above).
    // `extradata` is read for `extradata_size` bytes, which FFmpeg
    // allocated together.
    unsafe {
      let data = (*raw).extradata;
      let size = usize::try_from((*raw).extradata_size).unwrap_or(0);
      let extradata = if data.is_null() || size == 0 {
        &[][..]
      } else {
        core::slice::from_raw_parts(data, size)
      };
      access::KeyframeRule::of(self.codec_id(), extradata)
        .permitting_aso(self.h264_aso)
        .declaring_alpha(self.hevc_alpha)
    }
  }

  /// The keyframe rule `pkt` is decoded under: for a packet carrying
  /// `AV_PKT_DATA_NEW_EXTRADATA`, the rule its extradata gives — FFmpeg's
  /// H.264 and HEVC decoders apply it before they decode that very packet,
  /// so a change of NAL length fields or of packing frames the packet's own
  /// units — and otherwise the active one ([`Self::keyframe_rule`]). The
  /// extradata becomes the active one when a decoder takes the packet
  /// ([`NewExtradata`]). `None` for a rule that reads the extradata while
  /// the session's is unknown ([`Self::extradata_unknown`]): the decoder may
  /// frame the packet's units either way.
  fn rule_for(&self, pkt: &Packet) -> Option<access::KeyframeRule> {
    match new_extradata(pkt) {
      Some(extradata) => Some(
        access::KeyframeRule::of(self.codec_id(), extradata)
          .permitting_aso(self.h264_aso)
          .declaring_alpha(self.hevc_alpha),
      ),
      None => {
        let rule = self.keyframe_rule();
        (self.extradata_unknown.is_none() || !rule.reads_extradata()).then_some(rule)
      }
    }
  }

  /// The stream's codec id, as the 32-bit integer its parameters hold.
  fn codec_id(&self) -> i32 {
    // SAFETY: the owned, deep-copied parameters' pointer, only read;
    // `codec_id` is read as the 32-bit integer the field holds, never formed
    // into a bindgen enum.
    unsafe {
      let raw = self.parameters.as_ptr();
      if raw.is_null() {
        return crate::CodecId::NONE.raw();
      }
      core::ptr::read(core::ptr::addr_of!((*raw).codec_id) as *const i32)
    }
  }

  /// Reads the H.264 sequence parameter sets `pkt` brings — in a new
  /// extradata it carries, and, for a keyframe, among its own units — and
  /// the active extradata's, for one that permits arbitrary slice order
  /// ([`Self::h264_aso`]); says so the first time, naming the reason. Every
  /// packet the session is sent passes here before any road takes it, so a
  /// sequence parameter set the hardware decoded is read as one the software
  /// road would decode.
  fn note_sps(&mut self, pkt: &Packet) {
    if self.h264_aso || self.codec_id() != crate::CodecId::H264.raw() {
      return;
    }
    let rule = match new_extradata(pkt) {
      Some(extradata) => access::KeyframeRule::of(self.codec_id(), extradata),
      None => self.keyframe_rule(),
    };
    let in_band = pkt.is_key() && pkt.data().is_some_and(|data| rule.units_permit_aso(data));
    if rule.permits_aso() || in_band {
      self.h264_aso = true;
      tracing::warn!(
        reason = rule.permitting_aso(true).reason(),
        "mediadecode-ffmpeg: this H.264 stream's sequence parameter set permits arbitrary slice \
         order (Baseline or Extended, without constraint_set1_flag); no keyframe of it is read \
         as a clean point or a resync anchor, so the session returns to its threads only at a \
         seek, and a post-commit fallback's gap ends escalated",
      );
    }
  }

  /// Reads the HEVC video parameter sets `pkt` brings — in a new extradata
  /// it carries, and, for a keyframe, among its own units — and the active
  /// extradata's, and the output format the software decoder serving
  /// negotiated, for an auxiliary layer, which FFmpeg decodes as an alpha
  /// plane ([`Self::hevc_alpha`]); says so the first time, naming the
  /// reason.
  fn note_vps(&mut self, pkt: &Packet) {
    if self.hevc_alpha || self.codec_id() != crate::CodecId::HEVC.raw() {
      return;
    }
    let rule = match new_extradata(pkt) {
      Some(extradata) => access::KeyframeRule::of(self.codec_id(), extradata),
      None => self.keyframe_rule(),
    };
    let in_band = pkt.is_key()
      && pkt
        .data()
        .is_some_and(|data| rule.units_declare_alpha(data));
    let output = matches!(&self.state, DecodeState::Sw(sw) if sw.outputs_alpha());
    if rule.declares_alpha() || in_band || output {
      self.hevc_alpha = true;
      tracing::warn!(
        reason = rule.declaring_alpha(true).reason(),
        "mediadecode-ffmpeg: this HEVC stream declares an auxiliary layer, which FFmpeg decodes as \
         an alpha plane beside the base layer; no keyframe of it is read as a clean point or a \
         resync anchor, so the session returns to its threads only at a seek, and a post-commit \
         fallback's gap ends escalated",
      );
    }
  }

  /// **What a decoder opened for a packet carrying `replacement` opens on**:
  /// with none, nothing here — the session's own parameters
  /// ([`Self::parameters`]); with one, a copy of them carrying it in place of
  /// their extradata, which the session commits only once the decoder opened
  /// on it serves ([`Self::commit_opened`]). The one opening for every road
  /// that opens a decoder for the packet it then hands it: a reopen
  /// ([`Self::open_after_drain`]), the post-commit fallback's cold decoder.
  ///
  /// The open reads the extradata — FFmpeg's HEVC decoder parses it there
  /// and fails the open on a record it cannot read (`hevc_decode_init`,
  /// hevc/hevcdec.c) — so a decoder opened on the retained parameters before
  /// the packet's record replaced them failed for good on an unreadable
  /// retained record, while the packet carrying the stream's replacement
  /// never reached a decoder.
  fn carrying(&self, replacement: Option<NewExtradata>) -> Result<Option<Parameters>, Error> {
    replacement
      .map(|extradata| {
        let mut parameters =
          try_clone_parameters(&self.parameters, self.limits.max_codec_parameter_bytes())?;
        extradata.install(&mut parameters);
        Ok(parameters)
      })
      .transpose()
  }

  /// The decoder opened on `opened_on` — a copy of the session's parameters
  /// carrying a packet's new extradata ([`Self::carrying`]), or `None` for
  /// the session's own — serves: those parameters are the session's, their
  /// extradata known and read, since the open applied it; opened on the
  /// session's own, it replaces a decoder that may not have read a
  /// provisional extradata's packet.
  fn commit_opened(&mut self, opened_on: Option<Parameters>) {
    if let Some(parameters) = opened_on {
      self.parameters = parameters;
      self.extradata_unknown = None;
    }
    self.extradata_read();
  }

  /// Installs what the hardware took while its probe recorded
  /// ([`Self::probe_extradata`]) as the active extradata: the probe's history
  /// will not be replayed.
  fn install_probe_extradata(&mut self) {
    if let Some(extradata) = self.probe_extradata.take() {
      extradata.install(&mut self.parameters);
    }
  }

  /// The decoder serving took a packet carrying `extradata`: it is the
  /// stream's now — installed, or, while the hardware's probe records
  /// (`probing`), kept for it ([`Self::probe_extradata`]) — and the
  /// session's extradata is known again; provisionally until the decoder is
  /// seen to read the packet, unless `read` says it was
  /// ([`Self::extradata_provisional`]).
  fn took_extradata(&mut self, extradata: NewExtradata, probing: bool, read: bool) {
    if probing {
      self.probe_extradata = Some(extradata);
    } else {
      extradata.install(&mut self.parameters);
    }
    self.extradata_unknown = None;
    self.extradata_provisional = !read;
  }

  /// The decoder serving refused a packet carrying `extradata`: where it
  /// decoded the packet all the same, the extradata is the stream's, read
  /// ([`Self::took_extradata`]); where that cannot be told, the session's
  /// extradata is unknown ([`Self::extradata_unknown`]); where it did not
  /// take it, nothing changes.
  fn refused_with_extradata(&mut self, extradata: NewExtradata, taken: Taken, probing: bool) {
    match taken {
      Taken::Yes => self.took_extradata(extradata, probing, true),
      Taken::Unknown(doubt) => self.extradata_in_doubt(doubt),
      Taken::No => {}
    }
  }

  /// Whether the decoder serving applied a packet's new extradata cannot be
  /// told, for `doubt`: the session's extradata is unknown
  /// ([`Self::extradata_unknown`]).
  fn extradata_in_doubt(&mut self, doubt: crate::ExtradataDoubt) {
    tracing::warn!(
      %doubt,
      "mediadecode-ffmpeg: whether the decoder applied a packet's new codec extradata cannot be \
       told; until a packet carrying extradata is taken, no resync anchor or switch is read under \
       the session's extradata, and no decoder is opened on it",
    );
    self.extradata_unknown = Some(doubt);
    self.extradata_provisional = false;
  }

  /// The decoder serving was seen to read the packet whose new extradata is
  /// provisional ([`Self::extradata_provisional`]): it is the stream's.
  fn extradata_read(&mut self) {
    self.extradata_provisional = false;
  }

  /// The decoder serving reported `doubt` while the active extradata was
  /// provisional ([`Self::extradata_provisional`]): the report may be the
  /// unread packet's own, so the extradata is unknown. Nothing otherwise.
  fn reported_while_provisional(&mut self, doubt: crate::ExtradataDoubt) {
    if self.extradata_provisional {
      self.extradata_in_doubt(doubt);
    }
  }

  /// **A decode error the software decoder serving answered a receive with**,
  /// `raw` as libavcodec gave it — or as the allocator judge gave it, for a
  /// picture a decode concealed ([`SwDecoder::concealed_refusal`]). The
  /// output is unsettled; an anchor before the resync is proven is in doubt
  /// ([`Self::unanchor`]); a provisional extradata is unknown, since the
  /// error may be its unread packet's ([`Self::reported_while_provisional`]).
  /// Every receive-side decode error takes this one road.
  fn failed_on_receive(&mut self, raw: ffmpeg_next::Error) {
    self.sw_output_settled = false;
    self.unanchor();
    self.reported_while_provisional(crate::ExtradataDoubt::Reported(raw));
  }

  /// Whether `pkt`, a keyframe, is a clean random access point for this
  /// stream — see [`access::KeyframeRule::is_clean`]: proved by its
  /// bitstream, under the rule it is decoded under ([`Self::rule_for`]),
  /// never inferred from the decoder it would replace, whose `has_b_frames`
  /// FFmpeg raises only when it meets reordering — which an open GOP can
  /// introduce at this very keyframe.
  fn clean_keyframe(&self, pkt: &Packet) -> bool {
    pkt
      .data()
      .is_some_and(|data| self.rule_for(pkt).is_some_and(|rule| rule.is_clean(data)))
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
  /// post-commit failure never carries unconsumed packets), or, for a budget
  /// refusal or a software decoder whose resync could not be proved
  /// ([`Error::ResyncUnprovable`]), by its own name. With no replay-frame
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
      // fact, one name. The ceiling on the codec parameters' heap bytes is
      // a budget refusal the same way.
      Err(budget @ (Error::FrameBudgetExceeded(_) | Error::ParametersTooLarge(_))) => Err(budget),
      // **Nor is a refusal of the implementation.** Re-driven, the fallback
      // opens the same decoder and is refused the same way; the name says
      // what can change that — a build whose decoder for the codec is
      // libavcodec's own, or a session opened on software at a
      // random-access point, which owes no proof.
      Err(unprovable @ Error::ResyncUnprovable(_)) => Err(unprovable),
      // Nor is a refusal to open on unknown extradata: re-driven, it meets
      // the same parameters until a packet carrying extradata is taken.
      Err(unknown @ Error::ExtradataUnknown(_)) => Err(unknown),
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
    // The cold decoder starts on the stream's current parameters: a new
    // extradata the hardware took while its probe recorded is the stream's
    // now, whatever this fallback comes to.
    self.install_probe_extradata();
    // Parameters whose extradata is unknown open no decoder, unless the
    // packet it is handed carries its own, which it takes before its body.
    if let Some(doubt) = self.extradata_unknown {
      let carries = matches!(input, PostCommitInput::Packet(pkt) if new_extradata(pkt).is_some());
      if !carries {
        return Err(Error::ExtradataUnknown(crate::ExtradataUnknown::new(doubt)));
      }
    }
    // The extradata the forwarded packet carries — one the parameters'
    // ceiling cannot hold refused before a decoder opens — is what the cold
    // decoder opens on, in a copy of the session's parameters committed with
    // it ([`Self::carrying`]).
    let extradata = match input {
      PostCommitInput::Packet(pkt) => NewExtradata::of(
        pkt,
        &self.parameters,
        self.limits.max_codec_parameter_bytes(),
      )?,
      PostCommitInput::FrameTime | PostCommitInput::Eof => None,
    };
    let carrying = self.carrying(extradata)?;
    let one_thread = self.limits.with_threads(crate::Threads::Single);
    let mut sw = open_sw_decoder(
      carrying.as_ref().unwrap_or(&self.parameters),
      one_thread,
      Some(self.time_base),
    )?;
    // **No proof, no commit.** This is the one road whose commit owes a
    // proof — the resync after the gap — and every proof is an invariant of
    // libavcodec's own decoders ([`resync_proof`]). A decoder that wraps
    // another implementation is closed here, before it is handed anything,
    // and the fallback refused by name.
    if !sw.native {
      return Err(Error::ResyncUnprovable(sw.resync_unprovable()));
    }
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
    // threads come back at the next keyframe. The parameters it opened on
    // are the session's.
    self.state = DecodeState::Sw(sw);
    self.commit_opened(carrying);
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
    self.anchor_proof = None;
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
    let native = matches!(&self.state, DecodeState::Sw(sw) if sw.native);
    self.anchor_proof = resync_proof(rule, native, self.withheld_poisoned);
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

  /// **The resync's proof**, by the stream's codec rule on one of
  /// libavcodec's own decoders ([`Self::anchor_proof`], [`resync_proof`]);
  /// either closes the gap, and without one the gap stays open.
  ///
  /// - **Withheld output (H.264, on FFmpeg's own `h264`):** the first
  ///   picture out since the anchor. The session's software decoders are
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
  /// - **None (an implementation that wraps another):** nothing closes the
  ///   gap, and the end escalates. No such decoder serves across a gap — the
  ///   post-commit fallback refuses one ([`Error::ResyncUnprovable`]) — so
  ///   this is the binding stated where the proof is read.
  fn check_resync_proof(&mut self) {
    self.observe_reorder();
    let allowance = match self.anchor_proof {
      Some(access::Proof::Withheld) => 0,
      Some(access::Proof::ReorderBound) if self.anchor_resets => 0,
      Some(access::Proof::ReorderBound) => self.anchor_reorder,
      None => return,
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
    self.anchor_proof = None;
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
      probe_extradata: None,
      extradata_unknown: None,
      extradata_provisional: false,
      h264_aso: false,
      hevc_alpha: false,
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
      anchor_proof: None,
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
      fail_after_submit: None,
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

  /// What left the session's extradata unknown, while it is.
  pub(crate) const fn extradata_unknown_for_test(&self) -> Option<crate::ExtradataDoubt> {
    self.extradata_unknown
  }

  /// Whether the active extradata came with a packet the decoder serving
  /// has not been seen to read.
  pub(crate) const fn extradata_provisional_for_test(&self) -> bool {
    self.extradata_provisional
  }

  /// The allocator judge of the software decoder serving refuses, once, the
  /// next picture whose `pts` is `pts`, as one over the frame budget.
  pub(crate) fn decline_picture_for_test(&self, pts: i64) {
    if let DecodeState::Sw(sw) = &self.state {
      crate::ffi::decline_picture_for_test(sw.state(), pts);
    }
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
    self.fail_after_submit = Some(ffmpeg_next::Error::InvalidData);
  }

  /// The next packet the decoder serving takes is reported failed with
  /// `error`.
  pub(crate) fn fail_next_packet_with_for_test(&mut self, error: ffmpeg_next::Error) {
    self.fail_after_submit = Some(error);
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

  /// The session with the heap bytes its codec parameters may hold at
  /// `bytes`.
  pub(crate) const fn with_max_codec_parameter_bytes_for_test(mut self, bytes: usize) -> Self {
    self.limits = self.limits.with_max_codec_parameter_bytes(bytes);
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
    // What the hardware takes while its probe records, a probe-era fallback
    // replays; once the probe has committed, a new extradata it took while it
    // recorded is the stream's ([`Self::probe_extradata`]).
    let probing = matches!(&self.state, DecodeState::Hw(hw) if hw.records_submissions());
    if matches!(self.state, DecodeState::Hw(_)) && !probing {
      self.install_probe_extradata();
    }
    boundary::with_ffmpeg_video_packet::<C, _>(packet, limits, route, |av_pkt| {
      self.note_sps(av_pkt);
      self.note_vps(av_pkt);
      // The extradata a packet for the hardware carries, copied before it
      // sees the packet: the stream's once it takes it.
      let extradata = if matches!(self.state, DecodeState::Hw(_)) {
        NewExtradata::of(
          av_pkt,
          &self.parameters,
          self.limits.max_codec_parameter_bytes(),
        )
        .map_err(VideoDecodeError::Decode)?
      } else {
        None
      };
      match &mut self.state {
        DecodeState::Hw(hw) => match hw.send_packet(av_pkt) {
          // The seam already classified libavcodec's back pressure, so
          // both states travel on unchanged. A keyframe the hardware took
          // is the first one after a seek, if one was pending: the next
          // keyframe the software road sees is not.
          Ok(status) => {
            if matches!(status, Sent::Accepted) {
              if av_pkt.is_key() {
                self.seeked = false;
              }
              // The hardware decodes what a submission hands it inside that
              // submission, one thread, and libavcodec takes a packet only
              // into an empty input slot: it has read every packet before
              // this one.
              self.extradata_read();
              if let Some(extradata) = extradata {
                self.took_extradata(extradata, probing, false);
              }
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
              let degraded = self.degrade_to_sw(PostCommitInput::Packet(av_pkt), false);
              if degraded.is_err() {
                // The hardware failed on this packet, and nothing replaced it:
                // whether it applied the new extradata the packet carries, or
                // read a provisional one's packet, cannot be told, and it is
                // still the decoder serving.
                if extradata.is_some() {
                  self.extradata_in_doubt(crate::ExtradataDoubt::HardwareFailed);
                } else {
                  self.reported_while_provisional(crate::ExtradataDoubt::HardwareFailed);
                }
              }
              return degraded
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
          Err(other) => {
            // No refusal of the hardware's says it decoded this packet
            // (`taken_by_hardware_despite`), and any may be an earlier
            // packet's, a provisional extradata's unread one among them.
            let taken = taken_by_hardware_despite(&other);
            match taken {
              Taken::Yes => self.extradata_read(),
              Taken::Unknown(doubt) => self.reported_while_provisional(doubt),
              Taken::No => {}
            }
            // The new extradata it carries is the session's where the
            // refusal says the hardware decoded the packet, and unknown
            // where it does not say: a decode error does not.
            if let Some(extradata) = extradata {
              self.refused_with_extradata(extradata, taken, probing);
            }
            Err(VideoDecodeError::Decode(other))
          }
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
          // Either says the hardware has read every packet it took.
          Ok(status) => {
            self.extradata_read();
            return self.settle(status);
          }
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
              if let Err(error) = self.degrade_to_sw(PostCommitInput::FrameTime, eof_pending) {
                // The hardware failed, and nothing replaced it.
                self.reported_while_provisional(crate::ExtradataDoubt::HardwareFailed);
                return Err(VideoDecodeError::Decode(error));
              }
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
          Err(other) => {
            self.reported_while_provisional(doubt_of(&other));
            return Err(VideoDecodeError::Decode(other));
          }
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
              // A picture this call's decode refused, concealed while it gave
              // this one: the call's own decode error, reported first, as
              // FFmpeg reports one it does not conceal ahead of the pictures
              // after it; this one waits in the scratch for the next call.
              if let Some(refusal) = sw.concealed_refusal() {
                let error = crate::decoder::software_exit(st, refusal);
                self.failed_on_receive(refusal);
                return Err(VideoDecodeError::Decode(error));
              }
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
            // [`Self::settle`] and [`Self::ended`]. A frame-threaded
            // decoder's back pressure collects no refusal: a worker latched
            // it for a packet whose own answer names it ([`SwDecoder::funnel`]).
            Err(e) => match crate::decoder::software_receive(sw.funnel(e, phase), e, phase) {
              Ok(status) => {
                // Nothing is ready: the decoder holds no picture the caller
                // has not taken. Whether it has read every packet it took
                // depends on its threading ([`read_every_packet`]).
                let read = read_every_packet(status, sw.decodes_in_step());
                self.sw_output_settled = true;
                if read {
                  self.extradata_read();
                }
                return self.settle(status);
              }
              Err(error) => {
                self.failed_on_receive(e);
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
      DecodeState::Sw(sw) => match sw.send_eof() {
        Ok(()) => Ok(Sent::Accepted),
        Err(e) => crate::decoder::software_send(sw.funnel(e, phase), e, phase)
          .map_err(VideoDecodeError::Decode),
      },
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
    // The flush clears the probe's history but not what the decoders
    // applied: a new extradata the hardware took is the stream's from here,
    // and the active one stays (a container re-sends its own after a seek
    // that crosses a change), as does one left unknown. A provisional one's
    // packet may still wait unread in the decoder's input slot, which the
    // flush empties: the decoder kept frames the stream by what it read
    // before, and which that is cannot be told.
    self.install_probe_extradata();
    if self.extradata_provisional {
      self.extradata_in_doubt(crate::ExtradataDoubt::Flushed);
    }
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
      //
      // A frame refusal still latched is cleared with it, after the flush:
      // it is a picture of the position the caller abandons — a frame
      // thread's for a packet the flush discards with its worker's results,
      // which the flush first waits for (`ff_thread_flush`), or one a drain
      // parked behind — and naming it after the seek would name a picture
      // the caller no longer wants.
      DecodeState::Sw(sw) => {
        sw.flush();
        let _ = crate::ffi::take_frame_budget_declination(sw.state());
      }
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
  /// The extradata the last packet the decoder took carried as
  /// `AV_PKT_DATA_NEW_EXTRADATA`, copied — a packet whose refusal says the
  /// decoder decoded it among them ([`taken_despite`]): the session's active
  /// extradata once the replay is its own ([`NewExtradata::install`]).
  extradata: Option<NewExtradata>,
  /// **Whether the decoder was seen to read the packet the active
  /// extradata came with** — [`Self::extradata`]'s, or, where the round
  /// took none, the one provisional before it: a later packet the decoder
  /// took, where it decodes in step, which libavcodec takes only into an
  /// empty input slot; or the packet's own refusal saying the decoder
  /// decoded it ([`Taken::Yes`]). The proof the round carries out, applied
  /// before its error is ([`CarrierVideoStreamDecoder::replay_pending`]):
  /// read, an error after it is no longer the unread packet's.
  read: bool,
  /// What left the replay's extradata unknown: the refusal of its last
  /// packet, which carried a new extradata, where it does not say whether
  /// the decoder took the packet ([`Taken::Unknown`]).
  unknown: Option<crate::ExtradataDoubt>,
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
/// Stops, resumable, between packets once `queue` reaches the budget
/// `limits` give it ([`ReplayQueue::full`]): `progress` says how far it got,
/// and the rest is fed by a later call. Nothing is dropped. A packet whose
/// new extradata would carry the session's `parameters` past the ceiling
/// `limits` give them is refused before the decoder sees it, unfed
/// ([`NewExtradata::of`]).
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
  limits: DecoderLimits,
  parameters: &Parameters,
  progress: &mut Replay,
) -> Result<Drained, Error> {
  let budget = limits.max_replay_bytes();
  // Bound before the decoder is mutably borrowed, so the error
  // closures below can still consult it.
  let sw_state = sw.state();
  let in_step = sw.decodes_in_step();
  for pkt in packets {
    if queue.full(budget) {
      return Ok(Drained::Full);
    }
    // Copied before the decoder sees the packet; kept once it takes it.
    let extradata = NewExtradata::of(pkt, parameters, limits.max_codec_parameter_bytes())?;
    let mut attempts: u32 = 0;
    loop {
      match sw.submit(pkt) {
        Ok(()) => {
          // Taken in step: every packet before it was read. Its own record,
          // if it carries one, is read only once something says so.
          if in_step {
            progress.read = true;
          }
          if extradata.is_some() {
            progress.extradata = extradata;
            progress.read = false;
          }
          break;
        }
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
          let error = crate::decoder::software_exit(sw_state, other);
          // The packet is consumed with its error. Where the refusal says the
          // decoder decoded it, the decoder read it and every packet before
          // it, and its new extradata is the replay's, read; where it does
          // not say — a decode error does not — the extradata is unknown.
          match taken_despite(other, &error, in_step) {
            Taken::Yes => {
              if extradata.is_some() {
                progress.extradata = extradata;
              }
              progress.read = true;
            }
            Taken::Unknown(cause) if extradata.is_some() => progress.unknown = Some(cause),
            Taken::Unknown(_) | Taken::No => {}
          }
          return Err(error);
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
///
/// **A picture refused while a receive's decode gave another**, which the
/// decoder concealed ([`SwDecoder::concealed_refusal`]), is that receive's
/// decode error, named and reported after the picture it gave is placed —
/// a drain reports an error after the pictures it queued before it. Where
/// that picture is parked, the refusal stays latched for the receive the
/// drain resumes with, which names it; where the picture cannot be queued
/// at all, the refusal is reported in place of the picture's own. The end
/// of the decoder's output names a refusal still latched whatever its
/// threading: nothing is left in hand to answer for it.
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
        let concealed = sw.concealed_refusal();
        let refused = |own: Error| {
          concealed.map_or(own, |refusal| crate::decoder::software_exit(state, refusal))
        };
        let bytes = footprint(&tmp).map_err(|unpriced| refused(Error::UnpricedFrame(unpriced)))?;
        if bytes > budget {
          tracing::error!(
            bytes,
            budget,
            "mediadecode-ffmpeg: a decoded picture alone exceeds the software decoder's \
             replay budget; refusing it rather than queue past the budget",
          );
          return Err(refused(Error::ReplayQueueFull(
            crate::ReplayQueueFull::new(bytes, budget),
          )));
        }
        // The admission is the picture's own size: one the queue cannot take
        // within its budget waits parked, and the drain stops for the caller.
        if !queue.frames.is_empty() && queue.bytes.saturating_add(bytes) > budget {
          queue.parked = Some((tmp, bytes));
          return Ok(Drained::Full);
        }
        queue.push_back(tmp, bytes);
        if let Some(refusal) = concealed {
          return Err(crate::decoder::software_exit(state, refusal));
        }
      }
      // EAGAIN / EOF: no more output for now — stop draining, success; a
      // refusal the receive's decode latched in step is its error.
      Err(ffmpeg_next::Error::Other { errno }) if errno == ffmpeg_next::error::EAGAIN => {
        return match sw.concealed_refusal() {
          Some(refusal) => Err(crate::decoder::software_exit(state, refusal)),
          None => Ok(Drained::Empty),
        };
      }
      Err(ffmpeg_next::Error::Eof) => {
        return match crate::decoder::frame_budget_declination_of(state) {
          Some(refusal) => Err(refusal),
          None => Ok(Drained::Empty),
        };
      }
      // Any other error is a genuine decode failure — surface it so it is
      // not masked.
      Err(other) => return Err(crate::decoder::software_exit(state, other)),
    }
  }
}

/// The alignment every allocation a decoded picture owns is priced at:
/// `av_malloc` aligns to 64 bytes where libavutil is built with AVX-512
/// (`ALIGN` in its `mem.c`; 32 or 16 otherwise), and an allocation's payload
/// is taken to fill whole units of it ([`allocation`]).
const ALLOCATION_ALIGN: usize = 64;

/// What each allocation a decoded picture owns costs besides its payload
/// ([`allocation`]): the allocator's own header and rounding, and, for a
/// buffer, the two reference-counting structs `av_buffer_alloc` allocates
/// beside its payload — FFmpeg 9's `AVBufferRef` (24 bytes: the buffer, its
/// data and its size) and `AVBuffer` (48 bytes: data, size, reference count,
/// free callback, opaque pointer and two flag words), each its own
/// `av_malloc`, so 64 bytes apiece once aligned. With a header of up to 16
/// bytes for each of the three allocations that is 176 bytes; it is taken as
/// 256, for allocators whose size classes round coarser, and charged to
/// every allocation priced, a buffer or not.
const ALLOCATION_OVERHEAD: usize = 256;

/// The most side data entries a picture the replay queue takes may carry. A
/// decoder makes one for each message it reads — FFmpeg's H.264 decoder one
/// per unregistered SEI message, with no cap of its own — and each is
/// priced alone, so a picture past this is refused by name before its table
/// is walked ([`crate::UnpricedHolding::SideDataEntries`]), and the walk is
/// bounded by it. A legitimate stream carries a handful — HDR metadata,
/// captions, a timecode and film grain are under 16 — and the frame
/// conversion keeps at most 64 (`crate::convert::SIDE_DATA_MAX_ENTRIES`).
const MAX_SIDE_DATA_ENTRIES: usize = 256;

/// What one allocation of `payload` bytes that a decoded picture owns is
/// priced at: the payload rounded up to [`ALLOCATION_ALIGN`], and
/// [`ALLOCATION_OVERHEAD`].
const fn allocation(payload: usize) -> usize {
  payload
    .div_ceil(ALLOCATION_ALIGN)
    .saturating_mul(ALLOCATION_ALIGN)
    .saturating_add(ALLOCATION_OVERHEAD)
}

/// The bytes a decoded picture holds, every separately allocated object it
/// owns priced as an [`allocation`] — its payload rounded up to the
/// allocator's alignment, and the allocator's and the buffer's own overhead:
///
/// - each buffer its pixels reference — `buf[]` and `extended_buf` — or, for
///   a picture that references none, an upper bound on what its format and
///   dimensions allocate (`crate::footprint::video_frame_bytes`);
/// - `opaque_ref` and `hw_frames_ctx`;
/// - its side data, whatever the pixels weigh: the table of entries, and for
///   each entry the entry, its buffer (SEI payloads, ICC profiles, …) and
///   its metadata;
/// - each dictionary — the frame's metadata, each side data entry's — and
///   each of its entries' two strings ([`dictionary_bytes`]).
///
/// Pricing payloads alone charged a minimal SEI entry its 16 bytes while it
/// held several allocations, so a picture admitted under the budget could
/// hold several times it.
///
/// A picture holding an allocation of no stated size is refused by name
/// ([`UnpricedFrame`](crate::UnpricedFrame)): a `private_ref` — libavcodec's
/// own, which it clears before a frame leaves a decoder — and side data no
/// buffer reference owns, or a side data table that does not hold its
/// entries; so is one carrying more than [`MAX_SIDE_DATA_ENTRIES`] side data
/// entries. What the budget cannot price is never admitted as costing
/// nothing.
fn footprint(frame: &frame::Video) -> Result<usize, crate::UnpricedFrame> {
  use crate::UnpricedHolding;
  let refused = |holding| Err(crate::UnpricedFrame::new(holding));
  // SAFETY: `frame` is a live `AVFrame`; its buffer reference pointers and
  // their `size`, its side data table — at most `MAX_SIDE_DATA_ENTRIES`
  // entries, each entry's buffer reference, payload pointer, size and
  // metadata, never its type, which is a bindgen enum — its dictionaries,
  // read through FFmpeg's iterator, and four plain integers are read, and no
  // reference into FFmpeg memory is kept.
  unsafe {
    let raw = frame.as_ptr();
    let referenced = |buf: *const ffmpeg_next::ffi::AVBufferRef| {
      if buf.is_null() {
        0
      } else {
        allocation((*buf).size)
      }
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
    let Ok(entries) = usize::try_from((*raw).nb_side_data) else {
      return refused(UnpricedHolding::SideData);
    };
    if entries > MAX_SIDE_DATA_ENTRIES {
      return refused(UnpricedHolding::SideDataEntries {
        count: entries,
        cap: MAX_SIDE_DATA_ENTRIES,
      });
    }
    if entries == 0 {
      return Ok(total);
    }
    let table = (*raw).side_data;
    if table.is_null() {
      return refused(UnpricedHolding::SideData);
    }
    // The table holds a pointer per entry, one allocation.
    total = total.saturating_add(allocation(
      entries * core::mem::size_of::<*mut ffmpeg_next::ffi::AVFrameSideData>(),
    ));
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
        .saturating_add(allocation(core::mem::size_of::<
          ffmpeg_next::ffi::AVFrameSideData,
        >()))
        .saturating_add(referenced(buf))
        .saturating_add(dictionary_bytes(metadata));
    }
    Ok(total)
  }
}

/// What an `AVDictionary` holds, each allocation priced ([`allocation`]): the
/// dictionary itself — its count and its entry array's pointer, 16 bytes in
/// FFmpeg 9's `dict.c` — the array of entries, and each entry's key and
/// value, NUL-terminated strings of their own. Zero for a null dictionary.
///
/// # Safety
/// `dict` is null or a live `AVDictionary`.
unsafe fn dictionary_bytes(dict: *const ffmpeg_next::ffi::AVDictionary) -> usize {
  /// FFmpeg 9's `struct AVDictionary`, opaque to its users: an `int` count
  /// and the entry array's pointer.
  const DICTIONARY_BYTES: usize = 16;
  if dict.is_null() {
    return 0;
  }
  let mut entries: usize = 0;
  let mut strings: usize = 0;
  let mut entry: *const ffmpeg_next::ffi::AVDictionaryEntry = core::ptr::null();
  loop {
    // SAFETY: `dict` is live (the caller's promise) and `entry` is null or
    // the entry this iterator answered last.
    entry = unsafe { ffmpeg_next::ffi::av_dict_iterate(dict, entry) };
    if entry.is_null() {
      break;
    }
    // SAFETY: a live entry's key and value are NUL-terminated strings the
    // dictionary owns.
    let (key, value) = unsafe {
      (
        core::ffi::CStr::from_ptr((*entry).key).to_bytes().len(),
        core::ffi::CStr::from_ptr((*entry).value).to_bytes().len(),
      )
    };
    entries += 1;
    strings = strings
      .saturating_add(allocation(key + 1))
      .saturating_add(allocation(value + 1));
  }
  allocation(DICTIONARY_BYTES)
    .saturating_add(allocation(entries.saturating_mul(core::mem::size_of::<
      ffmpeg_next::ffi::AVDictionaryEntry,
    >())))
    .saturating_add(strings)
}

/// **The proof table**: how a post-commit resync anchored on `rule`'s stream
/// is proved, on the implementation decoding it.
///
/// - **libavcodec's own, H.264:** [`access::Proof::Withheld`]; the reorder
///   bound after a decode error across the gap (`poisoned`).
/// - **libavcodec's own, every other codec:** [`access::Proof::ReorderBound`],
///   with none allowed where every keyframe resets every reference (VP8,
///   VP9, AV1).
/// - **An implementation that wraps another, any codec:** none.
///
/// Every proof is an invariant of libavcodec's own decoders: FFmpeg's `h264`
/// withholding every picture it has not recovered, the reorder depth
/// `has_b_frames` its decoders publish, and their "needs input" saying
/// nothing more is held. An implementation that wraps another publishes no
/// depth and withholds nothing, so it proves nothing — and the post-commit
/// fallback, the one road that owes a proof, refuses it at the open
/// ([`Error::ResyncUnprovable`]).
fn resync_proof(rule: access::KeyframeRule, native: bool, poisoned: bool) -> Option<access::Proof> {
  if !native {
    return None;
  }
  Some(match rule.proof() {
    access::Proof::Withheld if poisoned => access::Proof::ReorderBound,
    proof => proof,
  })
}

/// A software decoder's `has_b_frames`: how many pictures its reorder buffer
/// holds back. FFmpeg raises it as it discovers reordering, and the
/// parameters a keyframe activates can lower it.
fn reorder_depth(sw: &SwDecoder) -> usize {
  // SAFETY: `sw` is a live opened software decoder; one plain integer field
  // is read and the pointer is not kept.
  usize::try_from(unsafe { (*sw.as_ptr()).has_b_frames }).unwrap_or(0)
}

/// The extradata `pkt` carries as `AV_PKT_DATA_NEW_EXTRADATA`, if any: what
/// `av_packet_get_side_data` answers, the lookup FFmpeg's H.264 and HEVC
/// decoders make before they decode a packet — the first entry of that type.
/// An empty entry is none: those decoders apply nothing for it.
fn new_extradata(pkt: &Packet) -> Option<&[u8]> {
  use ffmpeg_next::packet::Ref;
  let mut size: usize = 0;
  // SAFETY: `pkt` is a live packet. The type is handed to FFmpeg as the
  // constant this build names, never formed from FFmpeg memory, and FFmpeg
  // compares it in C; the answer is null or `size` bytes the packet owns,
  // borrowed here for as long as `pkt` is.
  unsafe {
    let data = ffmpeg_next::ffi::av_packet_get_side_data(
      pkt.as_ptr(),
      ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
      &mut size,
    );
    (!data.is_null() && size > 0).then(|| core::slice::from_raw_parts(data, size))
  }
}

/// Whether a decoder took a packet whose submission it refused, as far as
/// the packet's new extradata goes: whether the decoder reached the point
/// where it applies it ([`taken_despite`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Taken {
  /// It decoded the packet, the extradata applied.
  Yes,
  /// It refused the packet before queueing it.
  No,
  /// The refusal does not say, for this.
  Unknown(crate::ExtradataDoubt),
}

/// Whether a software decoder took the packet whose submission it refused
/// with `raw`, which the software road's funnel named `named`; `in_step`
/// says the decoder decodes, inside a submission, the packet the submission
/// queues ([`SwDecoder::decodes_in_step`]).
///
/// **Only a refusal this crate minted while it decoded the packet says.**
/// FFmpeg's H.264 and HEVC decoders apply a packet's new extradata as they
/// begin to decode it, and nothing FFmpeg reports ties an error to the
/// packet that was just submitted, or says that packet got that far:
///
/// - **Invalid data and unimplemented features** (`AVERROR_INVALIDDATA`,
///   `AVERROR_PATCHWELCOME`) can come from before the codec sees the
///   packet — a parameter change it carries, a bitstream filter between the
///   queue and the decoder, either of which drops it — or from FFmpeg's
///   HEVC decoder failing to parse the new extradata itself; and on a
///   frame-threaded decoder from a packet submitted before it, reported
///   while this one still waits to be handed to a thread, where a flush
///   drops it. So they do not say, nor do an allocation failure, an invalid
///   argument and the rest.
/// - **The refusals this crate's callbacks mint while a picture is
///   allocated** — a frame or a coded surface over its ceiling
///   ([`Error::FrameBudgetExceeded`], [`Error::HwSurfaceTooLarge`]) — come
///   from inside a decode, past the point where the extradata is applied.
///   Where the decoder decodes the packet a submission queues inside that
///   submission (`in_step`), the picture is that packet's — such a decoder's
///   every call reads the refusal its own decode latched, none surviving it
///   ([`SwDecoder::concealed_refusal`]) — so they say it was taken.
///   On a frame-threaded decoder the picture may be one an earlier packet's
///   thread allocates: they do not say.
/// - **Back pressure and the end** refuse a packet before it is queued,
///   whatever a callback left latched: it was not taken.
fn taken_despite(raw: ffmpeg_next::Error, named: &Error, in_step: bool) -> Taken {
  match (raw, named) {
    (ffmpeg_next::Error::Eof, _) => Taken::No,
    (ffmpeg_next::Error::Other { errno }, _) if errno == ffmpeg_next::error::EAGAIN => Taken::No,
    (_, Error::FrameBudgetExceeded(_) | Error::HwSurfaceTooLarge(_)) if in_step => Taken::Yes,
    _ => Taken::Unknown(crate::ExtradataDoubt::Reported(raw)),
  }
}

/// What `error`, which a decoder answered before it was seen to read a
/// packet carrying a new extradata, leaves of that extradata
/// ([`crate::ExtradataDoubt`]): the error libavcodec reported, where it
/// is kept; the hardware's failure no fallback replaced; a refusal this
/// crate made.
fn doubt_of(error: &Error) -> crate::ExtradataDoubt {
  match error {
    Error::Ffmpeg(raw) => crate::ExtradataDoubt::Reported(*raw),
    Error::AllBackendsFailed(_) => crate::ExtradataDoubt::HardwareFailed,
    _ => crate::ExtradataDoubt::Minted,
  }
}

/// **Whether a software decoder's flow answer, `status`, shows it has read
/// every packet it took** — what makes a provisional extradata the stream's
/// (`extradata_provisional`). `in_step` says the decoder decodes, inside a
/// call, the packet that call hands it ([`SwDecoder::decodes_in_step`]).
///
/// - **The end does, on every decoder:** it answers it once it has decoded
///   all it took and answered for every packet, every frame thread's
///   result returned before it (`ff_thread_receive_frame`, pthread_frame.c).
/// - **"Needs input" does on a decoder that decodes in step**: it answers
///   `EAGAIN` with its input slot empty and the packet that sat there
///   decoded. **On a frame-threaded decoder it does not.** FFmpeg 9 hands a
///   packet to a worker (`submit_packet`) and, while not every thread has
///   one, returns `EAGAIN` without waiting for it (`ff_thread_receive_frame`,
///   pthread_frame.c): the worker may not have reached the packet's new
///   extradata, and a failure to apply it — FFmpeg's HEVC decoder fails a
///   packet whose extradata it cannot parse (`hevc_receive_frame`,
///   hevc/hevcdec.c) — is answered later, in thread order. Nor does it on
///   an implementation that wraps another, which keeps a pipeline of its
///   own. On such a decoder nothing short of the end proves the read, and
///   the extradata stays provisional until then: a flush leaves it unknown
///   ([`crate::ExtradataDoubt::Flushed`]), as does an error before it
///   ([`crate::ExtradataDoubt::Reported`]).
/// - **A picture proves nothing**: it can be one decoded before the packet.
fn read_every_packet(status: Received, in_step: bool) -> bool {
  match status {
    Received::Ended => true,
    Received::NeedsInput => in_step,
    Received::Frame => false,
  }
}

/// [`taken_despite`] for the hardware road, whose decoder answers this
/// crate's own errors: its funnel's raw FFmpeg error, or a refusal minted
/// while a picture was allocated, named in place of the raw error. Such a
/// refusal names no packet here, and is read as unknown: the raw error it
/// stands for — back pressure or the end, which refuse before the queue,
/// among what it may be — is not kept, and a probe advancing inside one
/// submission replays its history into the next candidate before it retries
/// the packet, so the picture refused may be a replayed one. Any other
/// error of its is this crate's own refusal, made before FFmpeg saw the
/// packet.
fn taken_by_hardware_despite(error: &Error) -> Taken {
  match error {
    Error::Ffmpeg(raw) => taken_despite(*raw, error, true),
    Error::FrameBudgetExceeded(_) | Error::HwSurfaceTooLarge(_) => {
      Taken::Unknown(crate::ExtradataDoubt::Minted)
    }
    _ => Taken::No,
  }
}

/// Extradata a packet carried as `AV_PKT_DATA_NEW_EXTRADATA`, copied the way
/// codec parameters hold theirs — `av_malloc`ed with
/// `AV_INPUT_BUFFER_PADDING_SIZE` zeroed bytes behind it — before the packet
/// is handed to a decoder ([`Self::of`]), so that installing it once the
/// decoder has taken the packet cannot fail ([`Self::install`]). A packet the
/// decoder refuses leaves the active extradata as it was, and this copy is
/// freed.
struct NewExtradata {
  data: core::ptr::NonNull<u8>,
  size: core::ffi::c_int,
}

// SAFETY: a `NewExtradata` owns its allocation alone and only frees it;
// FFmpeg's allocator frees from any thread.
unsafe impl Send for NewExtradata {}

impl NewExtradata {
  /// A copy of the extradata `pkt` carries ([`new_extradata`]), or `None`
  /// where it carries none. Refused before the packet goes anywhere, so the
  /// packet stays the caller's and nothing changes: where the allocation
  /// fails, and, by name ([`Error::ParametersTooLarge`]), where the session's
  /// `parameters` with this extradata in place of theirs would hold more
  /// heap bytes than `max_parameter_bytes` allows — measured as the open's
  /// choke point measures them (`crate::decoder::build_codec_context`), which
  /// would refuse them at the next decoder the session opens on them: a
  /// switch's, a post-commit fallback's, after the decoder serving is closed.
  fn of(
    pkt: &Packet,
    parameters: &Parameters,
    max_parameter_bytes: usize,
  ) -> Result<Option<Self>, Error> {
    let Some(extradata) = new_extradata(pkt) else {
      return Ok(None);
    };
    let padding = ffmpeg_next::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
    // SAFETY: only the pointer is read; a live one is measured, which
    // allocates nothing.
    let rest = unsafe {
      let raw = parameters.as_ptr();
      if raw.is_null() {
        Some(0)
      } else {
        crate::extras::measure_parameters(raw)
          .and_then(|footprint| footprint.total_without_extradata())
      }
    };
    let projected = rest
      .and_then(|rest| rest.checked_add(extradata.len()))
      .and_then(|bytes| bytes.checked_add(padding))
      .unwrap_or(usize::MAX);
    if projected > max_parameter_bytes {
      return Err(Error::ParametersTooLarge(
        crate::demuxer::ParametersTooLarge::new(0, projected, max_parameter_bytes),
      ));
    }
    let Ok(size) = core::ffi::c_int::try_from(extradata.len()) else {
      return Err(Error::Ffmpeg(ffmpeg_next::Error::InvalidData));
    };
    // SAFETY: a plain allocation of the padded size; null on failure.
    let data = unsafe { ffmpeg_next::ffi::av_malloc(extradata.len() + padding) }.cast::<u8>();
    let Some(data) = core::ptr::NonNull::new(data) else {
      return Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
        errno: libc::ENOMEM,
      }));
    };
    // SAFETY: `data` was just allocated for `extradata.len() + padding`
    // bytes and overlaps nothing; the extradata is copied in and the padding
    // zeroed, as libavcodec requires of codec parameters' extradata.
    unsafe {
      core::ptr::copy_nonoverlapping(extradata.as_ptr(), data.as_ptr(), extradata.len());
      core::ptr::write_bytes(data.as_ptr().add(extradata.len()), 0, padding);
    }
    Ok(Some(Self { data, size }))
  }

  /// Installs this as `parameters`' extradata, the old freed: the stream's
  /// active extradata from here on.
  fn install(self, parameters: &mut Parameters) {
    // SAFETY: only the pointer is read.
    let raw = unsafe { parameters.as_mut_ptr() };
    if raw.is_null() {
      return;
    }
    let this = core::mem::ManuallyDrop::new(self);
    // SAFETY: `raw` is the live `AVCodecParameters` the session owns alone
    // (a deep copy; the hardware decoder holds its own). Its extradata is
    // null or `av_malloc`ed by FFmpeg, and is freed here; ownership of this
    // copy's allocation moves into it, so `this` is not dropped.
    unsafe {
      ffmpeg_next::ffi::av_freep(core::ptr::addr_of_mut!((*raw).extradata).cast());
      (*raw).extradata = this.data.as_ptr();
      (*raw).extradata_size = this.size;
    }
  }
}

impl Drop for NewExtradata {
  fn drop(&mut self) {
    // SAFETY: `data` is this copy's own `av_malloc` allocation, not
    // installed (`install` does not drop).
    unsafe { ffmpeg_next::ffi::av_free(self.data.as_ptr().cast()) };
  }
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
  // `avcodec_find_decoder` answers libavcodec's own decoder for the codec
  // ahead of every wrapper wherever one is built in that decodes on its own
  // (FFmpeg's `av1`, which decodes only through a hardware accelerator, is
  // listed after the external AV1 libraries), so no implementation is
  // chosen by name here.
  let codec = crate::decoder::find_decoder(parameters)?;
  // Who implements it: libavcodec itself, or a wrapper around another
  // implementation. The resync's proofs hold on the first alone.
  let wrapper = wrapper_name(codec);
  let native = wrapper.is_null();
  // SAFETY: `wrapper` is null or a string literal in libavcodec's static
  // codec list, valid for the process.
  let wrapper = unsafe { crate::ffi::table_text(wrapper, IMPLEMENTATION_NAME_MAX_BYTES) };
  #[cfg(test)]
  let (native, wrapper) = match sw_implementation::wrapped() {
    Some(name) => (false, Some(name)),
    None => (native, wrapper),
  };
  let opened = ctx.decoder().open_as(codec).map_err(Error::Ffmpeg)?;
  // Checked in every build, after the open, which is what FFmpeg reads: a
  // decoder that would output unrecovered pictures is closed and refused by
  // name, never opened on the strength of a debug assertion.
  refuse_unrecovered_output(&opened)?;
  crate::decoder::ensure_video_codec_type(&opened)?;
  Ok(SwDecoder {
    decoder: ffmpeg_next::decoder::Video(opened),
    native,
    wrapper,
    _callback_state: callback_state,
    #[cfg(test)]
    _live: live_sw::Guard::new(),
  })
}

/// What `codec` wraps, `AVCodec.wrapper_name`: null exactly for one of
/// libavcodec's own decoders ("If this field is NULL, this is a builtin,
/// libavcodec native codec", FFmpeg's `codec.h`), and otherwise the
/// wrapper's name — `cuvid`, `qsv`, `v4l2m2m`, `mediacodec`, `libdav1d`. Read
/// raw off the codec pointer: the null decides, and the text only names the
/// wrapper in a refusal.
fn wrapper_name(codec: ffmpeg_next::Codec) -> *const core::ffi::c_char {
  // SAFETY: `codec` wraps a non-null pointer into libavcodec's static codec
  // list (`crate::decoder::find_decoder`); one pointer field is read through
  // `addr_of!`, never forming a reference to the entry, whose `type` and
  // `id` are bindgen enums.
  unsafe { core::ptr::addr_of!((*codec.as_ptr()).wrapper_name).read() }
}

/// Test-only: the next software video decoder opened reads as an
/// implementation that wraps another, by the wrapper `name`, whatever it
/// opened — how a session reads where its build answers a wrapper for the
/// codec.
#[cfg(test)]
pub(crate) mod sw_implementation {
  use core::cell::Cell;

  std::thread_local! {
    static WRAPPED: Cell<Option<&'static str>> = const { Cell::new(None) };
  }

  /// The next open reads as wrapped by `name`.
  pub(crate) fn wrapped_next(name: &'static str) {
    WRAPPED.with(|wrapped| wrapped.set(Some(name)));
  }

  /// The wrapper armed for this open, once.
  pub(super) fn wrapped() -> Option<&'static str> {
    WRAPPED.with(Cell::take)
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

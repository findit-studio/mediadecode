use super::*;

use mediadecode::decoder::VideoStreamDecoder;
use std::num::NonZeroI32;

// The hardware-fallback suite runs on the **owned** lane, because it
// replays one `Vec<Packet>` through several decoders: a borrowed source
// can only be copied, which is exactly what that lane is. Nothing the
// suite proves is lane-specific — probe order, keyframe gating, pool
// defence and the exit funnels are all decisions about contexts and
// formats, taken before a carrier exists. The view lane's end-to-end
// decoder coverage lives in `tests/view_carriers.rs`, where the packets
// come from a demuxer that hands them over.
use crate::{FfmpegBytes, FfmpegOwnedVideoStreamDecoder as FfmpegVideoStreamDecoder};

// ---------------------------------------------------------------------------
//  Fake-HW fallback seam: synthetic clip + driver
// ---------------------------------------------------------------------------

/// A synthetic encoded clip: real mpeg4 packets (so the SW decoder genuinely
/// decodes them) plus their key flags and PTS. Encoded with a moving pattern
/// and a fixed GOP so the stream has real keyframes and P-frames.
struct SyntheticClip {
  parameters: ffmpeg_next::codec::Parameters,
  /// Encoded packets in decode order.
  packets: Vec<Packet>,
}

/// Encode a small multi-GOP mpeg4 clip in-process. `gop` forces a keyframe
/// every `gop` frames; a moving diagonal gradient gives the encoder real
/// inter-frame prediction so P-frames actually appear. `max_b_frames == 0`
/// keeps decode order == display order (simple monotonic PTS).
fn encode_synthetic_clip(width: u32, height: u32, frames: usize, gop: u32) -> SyntheticClip {
  use ffmpeg_next as ff;
  ff::init().expect("ffmpeg init");

  let codec = ff::codec::encoder::find(ff::codec::Id::MPEG4).expect("mpeg4 encoder present");
  let ctx = ff::codec::context::Context::new_with_codec(codec);
  let mut enc = ctx.encoder().video().expect("video encoder context");
  enc.set_width(width);
  enc.set_height(height);
  enc.set_format(ff::format::Pixel::YUV420P);
  enc.set_time_base(ff::Rational::new(1, 25));
  enc.set_gop(gop);
  enc.set_max_b_frames(0);
  enc.set_bit_rate(500_000);
  let mut opened = enc.open_as(codec).expect("open encoder");
  let parameters = ff::codec::Parameters::from(&opened);

  let mut packets: Vec<Packet> = Vec::new();
  let drain = |opened: &mut ff::codec::encoder::Video, out: &mut Vec<Packet>| {
    loop {
      let mut pkt = Packet::empty();
      match opened.receive_packet(&mut pkt) {
        Ok(()) => out.push(pkt),
        Err(_) => break,
      }
    }
  };

  let mut frame = ff::frame::Video::new(ff::format::Pixel::YUV420P, width, height);
  for i in 0..frames as i64 {
    let ystride = frame.stride(0);
    {
      let data = frame.data_mut(0);
      for y in 0..height as usize {
        for x in 0..width as usize {
          data[y * ystride + x] = ((x + y + i as usize * 4) & 0xff) as u8;
        }
      }
    }
    let cstride = frame.stride(1);
    for p in 1..3usize {
      let data = frame.data_mut(p);
      for y in 0..(height as usize / 2) {
        for x in 0..(width as usize / 2) {
          data[y * cstride + x] = (128 + ((x as i64 - i) & 0x3f)) as u8;
        }
      }
    }
    frame.set_pts(Some(i));
    opened.send_frame(&frame).expect("send_frame");
    drain(&mut opened, &mut packets);
  }
  opened.send_eof().expect("encoder send_eof");
  drain(&mut opened, &mut packets);

  assert!(
    packets.len() >= 8,
    "synthetic clip needs enough packets ({} too few)",
    packets.len()
  );
  assert!(packets[0].is_key(), "first packet must be a keyframe");
  SyntheticClip {
    parameters,
    packets,
  }
}

/// The HW-exhaustion shape a [`FakeHw`] raises at its `fail_at_send`.
#[derive(Clone, Copy)]
enum FailShape {
  /// Post-commit runtime failure: empty rescue, `FallbackOrigin::PostCommit`.
  /// The wrapper degrades and continues — the SW decoder opens cold and
  /// resyncs at the next keyframe.
  PostCommit,
  /// Probe-era failure: `FallbackOrigin::Probe` carrying the decoder's
  /// buffered packet history (every packet accepted so far, in order). The
  /// wrapper replays that history losslessly, then forwards the current packet.
  ProbeEra,
}

/// A test HW seam modelling the runtime-failure flow.
///
/// * `inert()` — never driven (a placeholder seam).
/// * `never_failing(...)` — delivers a frame 1:1 for the whole clip.
/// * `failing(.., doom_from_send, fail_at_send, shape)` — models a HW backend
///   that decodes the early frames fine and then hits content it can't decode.
///   It delivers a well-formed CPU frame 1:1 for every accepted packet until
///   `doom_from_send`; from that send onward it still *accepts* packets but
///   delivers **no** frames for them; on the `fail_at_send` send it returns the
///   chosen [`FailShape`] without accepting that packet.
struct FakeHw {
  width: u32,
  height: u32,
  /// First `send_packet` index (0-based) from which packets are accepted but
  /// no frame is delivered — modelling a HW decoder that buffered packets but
  /// cannot produce frames from them.
  doom_from_send: usize,
  /// `send_packet` index at which to fail. `usize::MAX` => never fail.
  fail_at_send: usize,
  /// The exhaustion shape raised at `fail_at_send`.
  shape: FailShape,
  /// Number of `send_packet` calls seen so far.
  sends: usize,
  /// CPU frames queued by accepted pre-doom `send_packet`s, delivered FIFO by
  /// `receive_frame`. Each carries the accepted packet's PTS.
  ///
  /// **Built at send time, not at receive time.** A real hardware seam
  /// has the frame in hand by the time it is asked for; allocating it
  /// inside `receive_frame` made the fake's own allocation the first
  /// thing an allocator ceiling hit, which is not the thing under test
  /// when a lane caps the ceiling to refuse a *carrier*.
  queued: VecDeque<frame::Video>,
  /// Refcounted clones of every packet accepted so far — the probe-era
  /// `unconsumed_packets` history surfaced on a [`FailShape::ProbeEra`] failure.
  history: Vec<Packet>,
  /// When set, raise a **probe-era** exhaustion from `receive_frame`
  /// rather than from `send_packet`.
  ///
  /// That is the decoder's *second* delivery path onto the replay
  /// queue: `fall_back_to_sw` fills it inside the `receive_frame` call
  /// and the head is converted there and then. Reaching it needs a
  /// hardware seam that fails at frame time, which nothing else here
  /// does.
  fail_at_receive: bool,
}

impl FakeHw {
  fn inert() -> Self {
    Self {
      width: 0,
      height: 0,
      doom_from_send: usize::MAX,
      fail_at_send: usize::MAX,
      shape: FailShape::PostCommit,
      sends: 0,
      queued: VecDeque::new(),
      history: Vec::new(),
      fail_at_receive: false,
    }
  }

  fn failing(
    width: u32,
    height: u32,
    doom_from_send: usize,
    fail_at_send: usize,
    shape: FailShape,
  ) -> Self {
    Self {
      width,
      height,
      doom_from_send,
      fail_at_send,
      shape,
      sends: 0,
      queued: VecDeque::new(),
      history: Vec::new(),
      fail_at_receive: false,
    }
  }

  /// Accepts every packet, then raises probe-era exhaustion the first
  /// time a frame is asked for — the receive-time fallback road.
  fn failing_at_receive(width: u32, height: u32) -> Self {
    let mut hw = Self::failing(width, height, 0, usize::MAX, FailShape::ProbeEra);
    hw.fail_at_receive = true;
    hw
  }

  /// Never fails — stays on the HW path for the whole clip, delivering 1:1.
  fn never_failing(width: u32, height: u32) -> Self {
    Self::failing(width, height, usize::MAX, usize::MAX, FailShape::PostCommit)
  }
}

impl HwInner for FakeHw {
  fn records_submissions(&self) -> bool {
    // **This fake records exactly like the real probe does** — see
    // `send_packet` below, which `try_clone_packet`s (an
    // `av_packet_ref`) every accepted packet into `history` and hands
    // that history out through `AllBackendsFailed`. Saying so is what
    // makes the view lane copy into it, and what
    // `a_rescued_packet_never_aliases_a_view_carrier` checks.
    true
  }

  fn send_packet(&mut self, packet: &Packet) -> Result<Sent, Error> {
    let idx = self.sends;
    self.sends += 1;
    if idx == self.fail_at_send {
      // The packet is NOT accepted; raise the chosen exhaustion shape.
      return match self.shape {
        FailShape::PostCommit => Err(Error::AllBackendsFailed(
          crate::error::AllBackendsFailed::new_post_commit(Vec::new()),
        )),
        FailShape::ProbeEra => Err(Error::AllBackendsFailed(
          crate::error::AllBackendsFailed::new(Vec::new(), std::mem::take(&mut self.history)),
        )),
      };
    }
    // Accept the packet. Track it as probe-era history, and deliver a frame for
    // it only before the doomed span.
    if let Ok(cloned) = crate::decoder::try_clone_packet(packet) {
      self.history.push(cloned);
    }
    if idx < self.doom_from_send {
      let mut av = frame::Video::new(ffmpeg_next::format::Pixel::YUV420P, self.width, self.height);
      av.set_pts(packet.pts());
      self.queued.push_back(av);
    }
    Ok(Sent::Accepted)
  }

  fn receive_frame(&mut self, frame: &mut Frame) -> Result<Received, Error> {
    if self.fail_at_receive {
      // Once: the decoder falls back and never asks this seam again.
      self.fail_at_receive = false;
      return Err(Error::AllBackendsFailed(
        crate::error::AllBackendsFailed::new(Vec::new(), std::mem::take(&mut self.history)),
      ));
    }
    match self.queued.pop_front() {
      Some(av) => {
        *frame.as_inner_mut() = av;
        Ok(Received::Frame)
      }
      None => Ok(Received::NeedsInput),
    }
  }

  fn send_eof(&mut self) -> Result<Sent, Error> {
    Ok(Sent::Accepted)
  }

  fn flush(&mut self) -> Result<(), Error> {
    self.queued.clear();
    Ok(())
  }

  fn as_video_decoder(&self) -> Option<&VideoDecoder> {
    None
  }
}

/// A HW seam that decodes a prefix 1:1 and then raises a **post-commit**
/// `AllBackendsFailed` from `send_eof` — the only way to drive the `send_eof`
/// fallback arm (the general [`FakeHw`]'s `send_eof` always succeeds). Every
/// `send_packet` is accepted and (until the queue is drained) delivers a frame
/// FIFO, so the stream is fully HW-decoded right up to the EOF-time failure;
/// the SW fallback then opens cold and, fed only `send_eof` with no packets,
/// can never produce a frame.
struct FakeHwEofFails {
  width: u32,
  height: u32,
  /// PTS of accepted packets, delivered FIFO by `receive_frame`.
  queued: VecDeque<i64>,
}

impl FakeHwEofFails {
  fn new(width: u32, height: u32) -> Self {
    Self {
      width,
      height,
      queued: VecDeque::new(),
    }
  }
}

impl HwInner for FakeHwEofFails {
  fn records_submissions(&self) -> bool {
    // This one keeps no history: it fails at `send_eof`, post-commit,
    // with an empty rescue set.
    false
  }

  fn send_packet(&mut self, packet: &Packet) -> Result<Sent, Error> {
    self.queued.push_back(packet.pts().unwrap_or(0));
    Ok(Sent::Accepted)
  }

  fn receive_frame(&mut self, frame: &mut Frame) -> Result<Received, Error> {
    match self.queued.pop_front() {
      Some(pts) => {
        let mut av =
          frame::Video::new(ffmpeg_next::format::Pixel::YUV420P, self.width, self.height);
        av.set_pts(Some(pts));
        *frame.as_inner_mut() = av;
        Ok(Received::Frame)
      }
      None => Ok(Received::NeedsInput),
    }
  }

  fn send_eof(&mut self) -> Result<Sent, Error> {
    Err(Error::AllBackendsFailed(
      crate::error::AllBackendsFailed::new_post_commit(Vec::new()),
    ))
  }

  fn flush(&mut self) -> Result<(), Error> {
    self.queued.clear();
    Ok(())
  }

  fn as_video_decoder(&self) -> Option<&VideoDecoder> {
    None
  }
}

/// A hardware seam whose `send_eof` answers back pressure rather than
/// taking the end. Nothing else about it matters.
struct FakeHwEofBackpressures;

impl HwInner for FakeHwEofBackpressures {
  fn records_submissions(&self) -> bool {
    false
  }
  fn send_packet(&mut self, _: &Packet) -> Result<Sent, Error> {
    Ok(Sent::Accepted)
  }
  fn receive_frame(&mut self, _: &mut Frame) -> Result<Received, Error> {
    Ok(Received::NeedsInput)
  }
  fn send_eof(&mut self) -> Result<Sent, Error> {
    Ok(Sent::MustDrain)
  }
  fn flush(&mut self) -> Result<(), Error> {
    Ok(())
  }
  fn as_video_decoder(&self) -> Option<&VideoDecoder> {
    None
  }
}

/// **Class audit: `is_ok()` is not the commit test, and this is the
/// ordering that proves it.**
///
/// `send_eof` answering `Ok(Sent::MustDrain)` means the end-of-stream
/// was **not** recorded — but it is still an `Ok`. Committing
/// `eof_sent` off `is_ok()` would mark the transaction done for a
/// signal the decoder never took, and a *later* fallback would then
/// inject an EOF into the freshly-opened software decoder on the
/// strength of it. That is the same half-mutation the failed-fallback
/// lane above guards from the error side; this guards it from the side
/// the send-status vocabulary opened.
#[test]
fn back_pressured_eof_does_not_commit_the_eof_transaction() {
  let mut dec = unopenable_sw_decoder(Box::new(FakeHwEofBackpressures));
  assert!(
    !dec.eof_sent_for_test(),
    "precondition: eof_sent starts false"
  );

  for _ in 0..3 {
    assert!(
      matches!(dec.send_eof(), Ok(Sent::MustDrain)),
      "the seam refuses the end until its output is drained",
    );
    assert!(
      !dec.eof_sent_for_test(),
      "an end-of-stream that was not taken must not commit the transaction",
    );
  }
}

/// Drive the decoder over `clip`, draining every available frame after each
/// `send_packet` and after EOF. Returns the PTS of every delivered frame in
/// order. A `None` PTS surfaces as `i64::MIN` so a hole is visible.
fn drive(dec: &mut FfmpegVideoStreamDecoder, clip: &SyntheticClip) -> Vec<i64> {
  let mut out: Vec<i64> = Vec::new();
  let mut dst = crate::empty_owned_video_frame();

  let mut drain_frames = |dec: &mut FfmpegVideoStreamDecoder, out: &mut Vec<i64>| {
    loop {
      match dec.receive_frame(&mut dst) {
        Ok(Received::Frame) => out.push(dst.pts().map(|t| t.pts()).unwrap_or(i64::MIN)),
        Ok(Received::NeedsInput | Received::Ended) => break,
        Err(e) => panic!("receive_frame: {e:?}"),
      }
    }
  };

  for av_pkt in &clip.packets {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    drain_frames(dec, &mut out);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  drain_frames(dec, &mut out);
  out
}

/// Index of the keyframe that starts the `n`-th (1-based) GOP, i.e. the `n`-th
/// keyframe in decode order.
fn nth_keyframe(clip: &SyntheticClip, n: usize) -> usize {
  clip
    .packets
    .iter()
    .enumerate()
    .filter(|(_, p)| p.is_key())
    .nth(n - 1)
    .map(|(i, _)| i)
    .unwrap_or_else(|| panic!("clip must have at least {n} keyframes (multi-GOP)"))
}

// ---------------------------------------------------------------------------
//  Post-commit fallback: degrade-and-continue, resync at next keyframe
// ---------------------------------------------------------------------------

/// End-to-end: a fake HW decoder commits, decodes the first GOP, then fails
/// **post-commit mid-GOP**. The wrapper must (1) flip to software, (2) NOT panic
/// or error — the dropped span is an accepted, logged gap, and (3) resync at the
/// next **keyframe** and decode normally from there. The accepted loss is the
/// bounded span from the failure point to that keyframe, so the assertion is the
/// *resync* (every PTS from the next keyframe onward is delivered exactly once),
/// NOT zero loss.
///
/// The resync is **keyframe-gated**: the failure point and everything up to the
/// next keyframe are P-frames, and a lenient mpeg4 SW decoder emits *concealed*
/// frames from those lone P-frames. The degrade-resync guard must **not** clear
/// on those — only the frame delivered after the real keyframe is fed counts. We
/// feed the stream in two phases to pin this down: up to (but excluding) the
/// resync keyframe the guard stays pending and no keyframe is seen; feeding the
/// keyframe onward clears it.
#[test]
fn post_commit_failure_degrades_and_resyncs_at_next_keyframe() {
  let (w, h) = (128u32, 96u32);
  // Three+ GOPs so a failure two into GOP-2 still has a GOP-3 keyframe ahead to
  // resync on. GOP of 6 over 24 frames gives keyframes at 0, 6, 12, 18, ...
  let clip = encode_synthetic_clip(w, h, 24, 6);

  let second_key = nth_keyframe(&clip, 2);
  let third_key = nth_keyframe(&clip, 3);
  // Fail two P-frames into GOP-2 (a genuine mid-GOP runtime failure). The
  // forwarded current packet (idx fail_at) is a P-frame a cold mpeg4 decoder
  // accepts without InvalidData, so the fallback commits and SW conceals.
  let fail_at = second_key + 2;
  assert!(
    fail_at < third_key && !clip.packets[fail_at].is_key(),
    "fail target must be a mid-GOP P-frame before the next keyframe"
  );

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    // Deliver every frame up to the failure (doom == fail: HW keeps delivering
    // 1:1 right until it fails), then fail post-commit on `fail_at`.
    Box::new(FakeHw::failing(
      w,
      h,
      fail_at,
      fail_at,
      FailShape::PostCommit,
    )),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  assert!(dec.is_hardware(), "must start on the HW seam");

  let mut pts_out: Vec<i64> = Vec::new();
  let mut dst = crate::empty_owned_video_frame();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, out: &mut Vec<i64>| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => out.push(dst.pts().map(|t| t.pts()).unwrap_or(i64::MIN)),
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(e) => panic!("receive_frame: {e:?}"),
    }
  };

  // Phase 1: feed packets [0, third_key) — the HW prefix, the post-commit
  // failure at `fail_at`, and the gap's P-frames up to (not including) the
  // resync keyframe. Even if mpeg4 conceals frames from those lone P-frames, the
  // KEYFRAME-GATED guard must stay pending and no keyframe must be recorded.
  for av_pkt in clip.packets.iter().take(third_key) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    drain(&mut dec, &mut pts_out);
  }
  // (1) flipped to software at the post-commit failure.
  assert!(
    dec.is_software(),
    "post-commit HW failure must trigger the SW fallback"
  );
  // (2) keyframe-gating: no keyframe fed across the gap yet, so the guard holds
  // even though concealed P-frame frames may already have been delivered.
  assert!(
    dec.degraded_resync_pending_for_test(),
    "no keyframe fed across the gap yet — the resync guard must still be pending \
     (a concealed P-frame must not clear it)"
  );
  assert!(
    !dec.degraded_keyframe_seen_for_test(),
    "no keyframe has crossed the gap, so the keyframe-seen anchor must be unset"
  );

  // Phase 2: feed the resync keyframe and the remainder; the frame SW delivers
  // after the keyframe clears the guard.
  for av_pkt in clip.packets.iter().skip(third_key) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    drain(&mut dec, &mut pts_out);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  drain(&mut dec, &mut pts_out);

  // (3) the keyframe-anchored resync cleared the guard — no escalation at EOF.
  assert!(
    !dec.degraded_resync_pending_for_test(),
    "the keyframe-anchored resync must have cleared the guard before EOF"
  );

  // Every delivered frame carried a real PTS.
  assert!(
    !pts_out.contains(&i64::MIN),
    "no delivered frame may have a missing PTS: {pts_out:?}"
  );

  // Resync at the next keyframe — the load-bearing guarantee. Degrade-and-
  // continue ACCEPTS a bounded loss span [fail_at, third_key); whether a lenient
  // codec (mpeg4 here) also recovers some of it is NOT part of the contract, so
  // we assert the resync, never zero loss. Concretely, with the failure point
  // and the resync keyframe known:
  //   * no duplicates and no out-of-range PTS — the seam never corrupts output;
  //   * the HW-delivered prefix [0, fail_at) all surfaces (HW delivered it
  //     before failing);
  //   * the SW resync is real: every PTS from the next keyframe onward
  //     [third_key_pts, total) surfaces — SW opened cold, resynced at that
  //     keyframe, and decoded the remainder;
  //   * any frame NOT delivered lies only inside the bounded accepted gap
  //     [fail_at, third_key_pts) — nothing outside the gap is ever lost.
  let third_key_pts = clip.packets[third_key].pts().expect("keyframe has pts");
  let total = clip.packets.len() as i64;

  let unique: std::collections::HashSet<i64> = pts_out.iter().copied().collect();
  assert_eq!(
    unique.len(),
    pts_out.len(),
    "no duplicate PTS — the degrade path must not re-emit a frame: {pts_out:?}"
  );
  for &pts in &pts_out {
    assert!(
      (0..total).contains(&pts),
      "delivered PTS {pts} is outside the source range 0..{total}: {pts_out:?}"
    );
  }
  // HW-delivered prefix is fully present.
  for pts in 0..fail_at as i64 {
    assert!(
      unique.contains(&pts),
      "HW delivered PTS {pts} before failing; it must be present: {pts_out:?}"
    );
  }
  // SW resync from the next keyframe onward is fully present (the resync proof).
  for pts in third_key_pts..total {
    assert!(
      unique.contains(&pts),
      "SW must resync at the next keyframe and decode the remainder; PTS {pts} \
       (>= resync keyframe {third_key_pts}) is missing — no resync: {pts_out:?}"
    );
  }
  // Any loss is confined to the bounded accepted gap — nothing outside it.
  for pts in 0..total {
    if !unique.contains(&pts) {
      assert!(
        (fail_at as i64..third_key_pts).contains(&pts),
        "PTS {pts} was dropped but lies OUTSIDE the accepted [fail, keyframe) \
         gap [{fail_at}, {third_key_pts}); only the bounded gap may be lost: \
         {pts_out:?}"
      );
    }
  }
  // The accepted gap is bounded by ~one GOP, not the whole tail.
  assert!(
    (third_key_pts - fail_at as i64) <= 6,
    "the accepted gap must be bounded by ~one GOP; was {}",
    third_key_pts - fail_at as i64
  );
}

/// Sanity: with no injected failure the fake HW stays on the HW path for the
/// whole clip and delivers one frame per packet. Guards against the seam itself
/// dropping frames or spuriously falling back.
#[test]
fn fake_hw_without_failure_stays_on_hardware() {
  let (w, h) = (128u32, 96u32);
  let clip = encode_synthetic_clip(w, h, 12, 6);

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::never_failing(w, h)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  let pts_out = drive(&mut dec, &clip);

  assert!(dec.is_hardware(), "no failure => stays on the HW seam");
  assert_eq!(
    pts_out.len(),
    clip.packets.len(),
    "HW path must deliver one frame per packet"
  );
}

// ---------------------------------------------------------------------------
//  Probe-era fallback: still lossless (the original pre-#12 path)
// ---------------------------------------------------------------------------

/// The probe-era path is unchanged by the degrade-and-continue simplification:
/// a HW failure **before the first frame** surfaces the decoder's buffered
/// history in `unconsumed_packets`, which the wrapper replays losslessly
/// through SW (then forwards the still-unconsumed current packet). No frame was
/// ever delivered on the HW path, so every source frame must come out exactly
/// once — a probe-era fallback loses nothing.
#[test]
fn probe_era_failure_replays_history_losslessly() {
  let (w, h) = (128u32, 96u32);
  let clip = encode_synthetic_clip(w, h, 16, 6);

  // Fail a few packets in WITHOUT delivering any frame first (doom_from_send =
  // 0 => nothing is delivered on HW; every accepted packet is buffered as
  // probe history). The failing packet is not accepted; the buffered history is
  // packets [0, fail_at).
  let fail_at = 5;
  assert!(fail_at < clip.packets.len(), "fail target in range");

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, fail_at, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  assert!(dec.is_hardware(), "must start on the HW seam");

  let pts_out = drive(&mut dec, &clip);

  assert!(
    dec.is_software(),
    "probe-era HW failure must trigger the SW fallback"
  );
  // Lossless: the replayed history + forwarded current packet + the remaining
  // forwarded packets reconstruct the whole stream — every PTS exactly once.
  assert!(
    !pts_out.contains(&i64::MIN),
    "no delivered frame may have a missing PTS: {pts_out:?}"
  );
  let mut sorted = pts_out.clone();
  sorted.sort_unstable();
  let expected: Vec<i64> = (0..clip.packets.len() as i64).collect();
  assert_eq!(
    sorted, expected,
    "a probe-era fallback must lose no frames — every source PTS delivered \
     exactly once: {pts_out:?}"
  );
}

// ---------------------------------------------------------------------------
//  Transactional SW-open failure: stays on HW, surfaces FallbackFailed
// ---------------------------------------------------------------------------

/// A decoder whose stored `parameters` cannot open a SW decoder. An empty
/// `Parameters` has codec id `NONE`, so `open_sw_decoder` (`build_codec_context`
/// → `.decoder().video()`) fails — exactly the SW-open failure the transactional
/// rollback must survive.
fn unopenable_sw_decoder(hw: Box<dyn HwInner>) -> FfmpegVideoStreamDecoder {
  ffmpeg_next::init().expect("ffmpeg init");
  let params = ffmpeg_next::codec::Parameters::new();
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  FfmpegVideoStreamDecoder::from_hw_inner_for_test(hw, params, tb).expect("build test decoder")
}

/// On a post-commit fallback whose SW decoder fails to OPEN, the transition is
/// transactional: the wrapper surfaces `FallbackFailed` (carrying the rescued
/// packets — empty here, as post-commit always is) and stays on the HW state.
/// It must NOT silently commit a broken SW decoder or lose the HW path.
#[test]
fn post_commit_sw_open_failure_stays_on_hw_transactionally() {
  let (w, h) = (64u32, 64u32);
  // Fail post-commit on the very first send. The stored `Parameters` are empty,
  // so `open_sw_decoder` fails and the fallback must roll back to HW.
  let mut dec = unopenable_sw_decoder(Box::new(FakeHw::failing(w, h, 0, 0, FailShape::PostCommit)));
  assert!(dec.is_hardware(), "must start on the HW seam");

  // Build a throwaway packet to send (content is irrelevant — HW fails before
  // touching it).
  let mut raw = Packet::new(16);
  raw.set_pts(Some(0));
  let vpkt = boundary::video_packet_from_ffmpeg(&raw)
    .expect("a wrappable payload")
    .expect("packet has a buffer");

  let err = dec
    .send_packet(&vpkt)
    .expect_err("SW-open failure must surface an error");
  match err {
    VideoDecodeError::Decode(Error::FallbackFailed(_)) => {}
    other => panic!("expected FallbackFailed on SW-open failure, got {other:?}"),
  }
  assert!(
    dec.is_hardware(),
    "a failed fallback (SW could not open) must leave the decoder on its prior \
     HW state — transactional rollback, not a half-committed SW"
  );
}

// ---------------------------------------------------------------------------
//  Drain-error propagation: a non-transient SW decode error surfaces
// ---------------------------------------------------------------------------

/// Zero a packet's payload in place — enough to make the mpeg4 SW decoder
/// reject it with `InvalidData` ("header damaged") when it tries to decode it.
fn corrupt_packet_payload(pkt: &mut Packet) {
  if let Some(d) = pkt.data_mut() {
    for b in d.iter_mut() {
      *b = 0;
    }
  }
}

/// A non-transient SW decode error during the fallback replay drain must
/// SURFACE (as `FallbackFailed` carrying the replay packets), not be swallowed
/// and the fallback silently committed over corruption. Exercised via the
/// **probe-era** replay path (the only path that replays packets): we poison a
/// P-frame in the buffered history the SW decoder replays; when the drain
/// decodes it the SW decoder returns `InvalidData`, which the drain propagates.
///
/// Without the drain-error fix the drain treats `InvalidData` like EAGAIN/EOF
/// (`break`), swallowing it: the fallback "succeeds", masking the corruption.
#[test]
fn sw_replay_drain_surfaces_non_transient_decode_error() {
  let (w, h) = (128u32, 96u32);
  // Single long GOP so the whole buffered history (with the corrupt packet) is
  // replayed on the probe-era fallback.
  let mut clip = encode_synthetic_clip(w, h, 12, 100);
  let p1 = clip
    .packets
    .iter()
    .position(|p| !p.is_key())
    .expect("clip has P-frames");
  assert!(
    p1 + 2 < clip.packets.len(),
    "need packets after the corrupt one"
  );
  corrupt_packet_payload(&mut clip.packets[p1]);

  // Probe-era: deliver NO frames (doom_from_send = 0), accept-and-buffer every
  // packet as history, then fail probe-era a few packets after the corrupt one.
  // The buffered history the SW decoder replays is {keyframe, corrupt_P, P, ...}
  // → the drain surfaces InvalidData.
  let fail_at = p1 + 3;
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, fail_at, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  let mut dst = crate::empty_owned_video_frame();
  let mut err = None;
  for av_pkt in &clip.packets {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    if let Err(e) = dec.send_packet(&vpkt) {
      err = Some(e);
      break;
    }
    loop {
      match dec.receive_frame(&mut dst) {
        Ok(Received::Frame) => {}
        Ok(Received::NeedsInput | Received::Ended) => break,
        Err(e) => {
          err = Some(e);
          break;
        }
      }
    }
    if err.is_some() {
      break;
    }
  }

  let err = err.expect("the corrupt replayed packet must surface an error, not be swallowed");
  match err {
    VideoDecodeError::Decode(Error::FallbackFailed(f)) => {
      assert!(
        !f.unconsumed_packets().is_empty(),
        "FallbackFailed must carry the replay packets for recovery"
      );
      assert!(
        matches!(f.source(), Error::Ffmpeg(ffmpeg_next::Error::InvalidData)),
        "the surfaced error must be the SW InvalidData decode failure; got {:?}",
        f.source()
      );
    }
    other => panic!("expected FallbackFailed surfacing InvalidData, got {other:?}"),
  }

  assert!(
    dec.is_hardware(),
    "a failed fallback must leave the decoder on its prior (HW) state, not \
     commit SW over swallowed corruption"
  );
}

/// The transactional commit boundary: SW **ACCEPTS every replayed packet**
/// (no EAGAIN backpressure, so the mid-replay drains never fire) and only then
/// returns `InvalidData` from `receive_frame`. The drain-before-commit must
/// catch that deferred error so it surfaces as `FallbackFailed` (rescued
/// packets retained) and the decoder stays HW — NOT as a plain decode error
/// after a half-done commit (frames appended + `state` flipped to `Sw` +
/// rescued packets dropped), which would break probe-era recovery on
/// non-seekable input.
///
/// This is the deferred-error counterpart to
/// `sw_replay_drain_surfaces_non_transient_decode_error`: there the corrupt
/// packet sits mid-history so a *subsequent send's* EAGAIN-drain decodes it
/// early; here the corrupt packet is the LAST in the buffered history, so no
/// per-send drain ever touches it — only the final drain-before-commit does.
/// Without that drain the fallback would commit and the `InvalidData` would
/// reach the caller plainly on the first post-commit `receive_frame`.
#[test]
fn sw_replay_deferred_error_surfaces_fallback_failed_at_commit() {
  let (w, h) = (128u32, 96u32);
  // Single long GOP so the whole prefix is one replayed history with no
  // intervening keyframe; corrupt the LAST P-frame of that prefix.
  let mut clip = encode_synthetic_clip(w, h, 12, 100);
  // `fail_at` is probe-era: the buffered history is packets [0, fail_at). Put
  // the corrupt packet at fail_at - 1 (the last replayed packet) so the only
  // decode of it happens in the final drain-before-commit.
  let fail_at = 5;
  assert!(
    fail_at >= 2 && fail_at < clip.packets.len(),
    "need a multi-packet history with room for a corrupt tail"
  );
  let corrupt_idx = fail_at - 1;
  assert!(
    !clip.packets[corrupt_idx].is_key(),
    "the corrupt last-history packet must be a P-frame (a corrupt keyframe \
     could fail SW's send_packet instead of receive_frame)"
  );
  corrupt_packet_payload(&mut clip.packets[corrupt_idx]);

  // Probe-era, deliver NO frames (doom_from_send = 0): every accepted packet is
  // buffered as history; fail probe-era at `fail_at`. History replayed through
  // SW is {keyframe, clean P.., corrupt_P} — the sends accept it all, and the
  // final drain decodes corrupt_P and surfaces InvalidData.
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, fail_at, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  // Send exactly the history-then-failing packets; the failing send triggers
  // the fallback whose commit-time drain must surface the deferred error.
  let mut surfaced = None;
  let mut dst = crate::empty_owned_video_frame();
  for av_pkt in clip.packets.iter().take(fail_at + 1) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    match dec.send_packet(&vpkt) {
      // A full decoder would need draining first; here nothing is
      // queued, so the two send answers are handled the same way.
      Ok(Sent::Accepted | Sent::MustDrain) => {
        // Drain anything available (none expected pre-fallback — doom = 0).
        loop {
          match dec.receive_frame(&mut dst) {
            Ok(Received::Frame) => {}
            Ok(Received::NeedsInput | Received::Ended) => break,
            Err(e) => {
              surfaced = Some(e);
              break;
            }
          }
        }
      }
      Err(e) => {
        surfaced = Some(e);
        break;
      }
    }
    if surfaced.is_some() {
      break;
    }
  }

  let err = surfaced.expect(
    "the deferred InvalidData must surface at the fallback commit boundary, not be \
     committed over",
  );
  match err {
    VideoDecodeError::Decode(Error::FallbackFailed(f)) => {
      assert!(
        !f.unconsumed_packets().is_empty(),
        "FallbackFailed must retain the rescued replay packets for recovery"
      );
      assert!(
        matches!(f.source(), Error::Ffmpeg(ffmpeg_next::Error::InvalidData)),
        "the surfaced error must be the deferred SW InvalidData; got {:?}",
        f.source()
      );
    }
    other => panic!("expected FallbackFailed surfacing the deferred InvalidData, got {other:?}"),
  }
  assert!(
    dec.is_hardware(),
    "a deferred-error fallback caught at the commit boundary must leave the \
     decoder on its prior HW state — nothing committed"
  );
}

// ---------------------------------------------------------------------------
//  Failed EOF fallback: eof_sent is RESTORED (no half-mutation), stays HW
// ---------------------------------------------------------------------------

/// `send_eof` hits a post-commit HW failure whose SW decoder cannot open
/// (empty `Parameters`). The fallback returns `FallbackFailed`, so the decoder
/// stays HW — and `eof_sent` must be RESTORED to its prior value (`false`),
/// never left half-mutated `true`. A stale `eof_sent = true` would make a
/// *later* fallback inject EOF into the new SW decoder though this `send_eof`
/// errored.
#[test]
fn failed_eof_fallback_restores_eof_sent_and_stays_on_hw() {
  let (w, h) = (64u32, 64u32);
  // `FakeHwEofFails::send_eof` raises a post-commit `AllBackendsFailed`, driving
  // the send_eof fallback arm; the empty `Parameters` from `unopenable_sw_decoder`
  // make `open_sw_decoder` fail, so the fallback returns `FallbackFailed` and the
  // transaction must roll back (HW retained, `eof_sent` un-mutated).
  let mut dec = unopenable_sw_decoder(Box::new(FakeHwEofFails::new(w, h)));
  assert!(dec.is_hardware(), "must start on the HW seam");
  assert!(
    !dec.eof_sent_for_test(),
    "precondition: eof_sent starts false"
  );

  let err = dec
    .send_eof()
    .expect_err("a failed EOF fallback must surface an error");
  match err {
    VideoDecodeError::Decode(Error::FallbackFailed(_)) => {}
    other => panic!("expected FallbackFailed on the failed EOF fallback, got {other:?}"),
  }

  assert!(
    dec.is_hardware(),
    "a failed EOF fallback (SW could not open) must leave the decoder on its \
     prior HW state — transactional rollback"
  );
  assert!(
    !dec.eof_sent_for_test(),
    "eof_sent must be RESTORED to its prior value (false) after a failed EOF \
     fallback — a stale true would inject EOF into a later SW fallback"
  );

  // A subsequent operation must not see stale EOF: a normal send_eof on the
  // (still-HW, EOF-never-accepted) decoder behaves as a first EOF. Our seam's
  // send_eof keeps failing the same way, so this just re-confirms HW + the
  // rolled-back flag rather than silently succeeding off a stale `eof_sent`.
  let err2 = dec.send_eof().expect_err(
    "the still-HW decoder must re-attempt (and re-fail) EOF, not no-op off stale state",
  );
  assert!(
    matches!(err2, VideoDecodeError::Decode(Error::FallbackFailed(_))),
    "second send_eof must again drive the fallback (proving no stale-EOF short-circuit)"
  );
  assert!(
    !dec.eof_sent_for_test(),
    "still rolled back after the retry"
  );
}

// ---------------------------------------------------------------------------
//  Post-commit fallback that never resyncs before EOF: escalate, not silent
// ---------------------------------------------------------------------------

/// A post-commit fallback fires and the SW decoder reaches EOF without ever
/// producing a frame — no keyframe arrived across the gap, so the whole tail is
/// lost. The "bounded, logged gap" promise can't be kept (there is no resync),
/// so the loss must ESCALATE: a distinct `PostCommitNeverResynced` error at EOF,
/// NOT a silent empty tail surfaced as a clean end-of-stream.
///
/// Determinism note: a real (lenient) mpeg4 SW decoder will happily decode a
/// lone P-frame forwarded after a mid-stream fallback, *resyncing* and clearing
/// the pending flag — so "fed only P-frames to EOF" is not a reliable no-resync
/// trigger in a unit test (the resync keyframe being absent is an input
/// property, not something the test can force on a lenient decoder). The
/// unambiguous no-resync case is a **cold SW decoder fed no decodable input at
/// all**: we fail post-commit at `send_eof`, so the SW decoder opens cold,
/// receives only the re-forwarded EOF, and can categorically produce no frame.
/// `receive_frame` then returns EOF while the resync is still pending →
/// escalation. (`packets_lost` is 0 here: zero packets crossed to SW — the lost
/// tail was the HW-side frames the EOF-time failure stranded. The counter is
/// incremented for packets fed to SW across a gap entered from the
/// `send_packet` arm; this EOF-entry path forwards none.)
#[test]
fn post_commit_fallback_never_resyncing_escalates_at_eof() {
  let (w, h) = (128u32, 96u32);
  // A normal multi-GOP clip fully decoded on HW up to EOF; the EOF-time HW
  // failure then strands the tail and SW cannot resync from a cold EOF.
  let clip = encode_synthetic_clip(w, h, 12, 6);

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHwEofFails::new(w, h)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  let mut dst = crate::empty_owned_video_frame();
  let mut delivered = 0usize;
  let mut escalation = None;
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder,
                   delivered: &mut usize,
                   escalation: &mut Option<VideoDecodeError>| {
    loop {
      match dec.receive_frame(&mut dst) {
        Ok(Received::Frame) => *delivered += 1,
        Ok(Received::NeedsInput | Received::Ended) => break,
        Err(e @ VideoDecodeError::PostCommitNeverResynced(_)) => {
          *escalation = Some(e);
          break;
        }
        Err(e) => panic!("unexpected error draining frames: {e:?}"),
      }
    }
  };

  // HW decodes the whole stream 1:1 (no fallback yet).
  for av_pkt in &clip.packets {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    drain(&mut dec, &mut delivered, &mut escalation);
    assert!(
      escalation.is_none(),
      "no escalation while still on the HW path"
    );
  }
  assert!(dec.is_hardware(), "still HW until the EOF-time failure");
  assert_eq!(
    delivered,
    clip.packets.len(),
    "HW must deliver the whole stream before the EOF-time failure"
  );

  // EOF triggers the post-commit fallback; the cold SW decoder is fed only EOF.
  crate::accepted(
    dec.send_eof(),
    "send_eof drives the fallback but itself succeeds",
  );
  assert!(
    dec.is_software(),
    "the EOF-time failure fell back to software"
  );
  assert!(
    dec.degraded_resync_pending_for_test(),
    "post-commit fallback at EOF must enter degraded-resync mode (SW opened cold)"
  );

  // Draining the cold SW decoder hits EOF with the resync still pending →
  // escalation, not a silent empty tail.
  drain(&mut dec, &mut delivered, &mut escalation);

  let esc = escalation.expect(
    "a post-commit fallback whose SW decoder reaches EOF without resyncing must \
     ESCALATE, not silently swallow the tail as a clean end-of-stream",
  );
  let VideoDecodeError::PostCommitNeverResynced(p) = esc else {
    panic!("expected PostCommitNeverResynced, got {esc:?}");
  };
  let packets_lost = p.packets_lost();
  assert_eq!(
    packets_lost, 0,
    "no packets crossed to SW on the EOF-entry path; the lost tail was HW-side"
  );
  assert!(
    dec.is_software(),
    "the decoder did fall back to software (it just never resynced)"
  );
  // The flag is cleared after escalating so a follow-up poll sees the
  // ordinary end of the stream (not a repeated escalation).
  assert!(
    !dec.degraded_resync_pending_for_test(),
    "the degraded-resync flag must be cleared after the escalation fires"
  );
  let mut after = crate::empty_owned_video_frame();
  match dec.receive_frame(&mut after) {
    Ok(Received::Ended) => {}
    other => panic!("a poll after the escalation must be a clean end, got {other:?}"),
  }
}

/// The gap counter via the `send_packet` arm: packets forwarded to SW while a
/// post-commit resync is still pending are tallied, and the tally — together
/// with the pending flag — is CLEARED the moment SW resyncs. This covers the
/// bounded-and-logged (resync happened) outcome's bookkeeping, the complement
/// of the escalate-at-EOF outcome.
#[test]
fn post_commit_gap_counter_tallies_then_clears_on_resync() {
  let (w, h) = (128u32, 96u32);
  // Keyframes at 0, 6, 12, 18. Fail two P-frames into GOP-2 so a GOP-3 keyframe
  // is still ahead to resync on.
  let clip = encode_synthetic_clip(w, h, 24, 6);
  let second_key = nth_keyframe(&clip, 2);
  let third_key = nth_keyframe(&clip, 3);
  let fail_at = second_key + 2;
  assert!(
    fail_at < third_key && !clip.packets[fail_at].is_key(),
    "fail target must be a mid-GOP P-frame before the next keyframe"
  );

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(
      w,
      h,
      fail_at,
      fail_at,
      FailShape::PostCommit,
    )),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  // Feed packets [0, fail_at]: the prefix decodes on HW (no drain needed — the
  // fake buffers them), and the send at `fail_at` triggers the post-commit
  // fallback, which forwards that one current packet to the freshly-opened SW
  // decoder. We do NOT drain here: a single forwarded packet won't trip SW
  // backpressure, and not draining keeps the gap open so the tally is
  // observable before any resync frame clears it.
  let mut dst = crate::empty_owned_video_frame();
  for av_pkt in clip.packets.iter().take(fail_at + 1) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
  }
  assert!(
    dec.is_software(),
    "the mid-GOP failure fell back to software"
  );
  assert!(
    dec.degraded_resync_pending_for_test(),
    "the gap is still open (no resync frame drained yet)"
  );
  // Exactly the forwarded current packet crossed the gap from the send_packet
  // arm so far — the tally proves gap packets are counted.
  assert_eq!(
    dec.degraded_packets_since_fallback_for_test(),
    1,
    "the forwarded current packet must be tallied as crossing the gap"
  );

  // Drive to a KEYFRAME-ANCHORED resync. The forwarded current packet and the
  // gap P-frames are lone P-frames; mpeg4 will conceal frames from them, but the
  // keyframe-gated guard must NOT clear on those — only a frame delivered after
  // the resync keyframe (third_key) is fed counts. So we feed remaining packets,
  // draining as we go, and assert the guard stays pending until the keyframe is
  // reached, then clears once a frame is delivered after it. One poll per send:
  // `true` if a frame was delivered, `false` if the decoder wants input or has
  // ended.
  let mut try_poll = |dec: &mut FfmpegVideoStreamDecoder| -> bool {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => true,
      Ok(Received::NeedsInput | Received::Ended) => false,
      Err(e) => panic!("unexpected drain error: {e:?}"),
    }
  };
  // First, fully drain whatever the already-forwarded P-frame yields. Any
  // concealed frame here must leave the guard pending (no keyframe fed yet).
  while try_poll(&mut dec) {}
  assert!(
    dec.degraded_resync_pending_for_test(),
    "a concealed frame from the forwarded P-frame must NOT clear the guard — no \
     keyframe has crossed the gap yet"
  );
  assert!(
    !dec.degraded_keyframe_seen_for_test(),
    "no keyframe fed yet, so the keyframe-seen anchor must be unset"
  );

  // Feed remaining packets up to (not including) the resync keyframe: still all
  // P-frames, so concealed frames may land but the guard must stay pending.
  // Drain fully each time so the keyframe send below never hits SW backpressure.
  for av_pkt in clip.packets[(fail_at + 1)..third_key].iter() {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    while try_poll(&mut dec) {}
    assert!(
      dec.degraded_resync_pending_for_test() && !dec.degraded_keyframe_seen_for_test(),
      "concealed P-frame frames before the keyframe must not clear the guard or \
       set the keyframe anchor"
    );
  }

  // Feed the resync keyframe. Sending it records the anchor immediately (the
  // keyframe crossed the gap) — observe that BEFORE draining, since the resync
  // frame's delivery clears the whole degraded state. The guard is still pending
  // here: the anchor is set, but no post-keyframe frame has been delivered yet.
  assert!(third_key < clip.packets.len(), "clip has a third keyframe");
  let key_vpkt = boundary::video_packet_from_ffmpeg(&clip.packets[third_key])
    .expect("a wrappable payload")
    .expect("packet has a buffer");
  crate::accepted(dec.send_packet(&key_vpkt), "send_packet");
  assert!(
    dec.degraded_keyframe_seen_for_test(),
    "feeding the keyframe across the gap must record it as the resync anchor"
  );

  // Now drive (keyframe + remainder) draining until a post-keyframe frame lands
  // and clears the guard — the keyframe-anchored resync.
  let mut resynced = !dec.degraded_resync_pending_for_test();
  while !resynced && try_poll(&mut dec) {
    resynced = !dec.degraded_resync_pending_for_test();
  }
  for av_pkt in clip.packets[(third_key + 1)..].iter() {
    if resynced {
      break;
    }
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    while !resynced && try_poll(&mut dec) {
      resynced = !dec.degraded_resync_pending_for_test();
    }
  }
  assert!(
    resynced,
    "SW must resync once the keyframe is fed and produce a frame after it"
  );
  assert!(
    !dec.degraded_resync_pending_for_test(),
    "the keyframe-anchored resync must clear the pending flag"
  );
  assert_eq!(
    dec.degraded_packets_since_fallback_for_test(),
    0,
    "resync must reset the gap counter"
  );
}

// ---------------------------------------------------------------------------
//  Keyframe-gated resync (finding 2): a concealed P-frame must NOT clear it
// ---------------------------------------------------------------------------

/// **Finding-2 regression.** A post-commit fallback fires, then the SW decoder
/// emits *concealed* frames from lone P-frames **before any keyframe** arrives,
/// and EOF is reached with no keyframe ever fed. The resync guard is
/// **keyframe-gated**, so those concealed frames must NOT clear it: the loss
/// must still ESCALATE with `PostCommitNeverResynced` at EOF, exactly as if no
/// frame had been delivered. (Before the gate, the first concealed P-frame
/// cleared `degraded_resync_pending`, faking a resync that never happened and
/// silently swallowing the lost tail.)
///
/// Determinism: a cold mpeg4 SW decoder fed lone P-frames from a mid-GOP point
/// **does** emit concealed frames (verified), so this reliably exercises
/// "a frame was delivered but no keyframe was fed". We fail post-commit at
/// `second_key + 2` (a P-frame the cold decoder accepts without InvalidData),
/// forward it + the rest of GOP-2's P-frames, then send EOF — never feeding the
/// GOP-3 keyframe.
#[test]
fn post_commit_concealed_p_frame_does_not_clear_resync_escalates_at_eof() {
  let (w, h) = (128u32, 96u32);
  // Keyframes at 0, 6, 12, 18. Fail at second_key + 2 so the forwarded current
  // packet is a mid-GOP P-frame the cold mpeg4 decoder accepts and conceals.
  let clip = encode_synthetic_clip(w, h, 24, 6);
  let second_key = nth_keyframe(&clip, 2);
  let third_key = nth_keyframe(&clip, 3);
  let fail_at = second_key + 2;
  assert!(
    fail_at < third_key && !clip.packets[fail_at].is_key(),
    "fail target must be a mid-GOP P-frame before the next keyframe"
  );

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(
      w,
      h,
      fail_at,
      fail_at,
      FailShape::PostCommit,
    )),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  let mut dst = crate::empty_owned_video_frame();
  let mut concealed_frames = 0usize;
  let mut escalation: Option<VideoDecodeError> = None;
  // Drain available frames; route a `PostCommitNeverResynced` to `escalation`.
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder,
                   concealed: &mut usize,
                   escalation: &mut Option<VideoDecodeError>| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => *concealed += 1,
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(e @ VideoDecodeError::PostCommitNeverResynced(_)) => {
        *escalation = Some(e);
        break;
      }
      Err(e) => panic!("unexpected drain error: {e:?}"),
    }
  };

  // Feed packets [0, third_key): the HW prefix, the post-commit failure at
  // `fail_at`, and the GOP-2 P-frames — but NEVER the GOP-3 keyframe. Each drain
  // may deliver a concealed frame; none may clear the keyframe-gated guard.
  for av_pkt in clip.packets.iter().take(third_key) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    drain(&mut dec, &mut concealed_frames, &mut escalation);
    assert!(escalation.is_none(), "no escalation before EOF");
    if dec.is_software() {
      // Once degraded, the guard must stay pending and unanchored — no keyframe
      // has crossed the gap, only (possibly concealed) P-frame frames.
      assert!(
        dec.degraded_resync_pending_for_test(),
        "a concealed P-frame must not clear the keyframe-gated resync guard"
      );
      assert!(
        !dec.degraded_keyframe_seen_for_test(),
        "no keyframe was fed, so the keyframe-seen anchor must stay unset"
      );
    }
  }
  assert!(
    dec.is_software(),
    "the post-commit failure fell back to software"
  );
  assert!(
    concealed_frames > 0,
    "the cold mpeg4 SW decoder must have concealed at least one frame from the \
     lone P-frames (otherwise this test does not exercise the 'frame delivered \
     but no keyframe' path)"
  );
  assert!(
    dec.degraded_resync_pending_for_test(),
    "after feeding only P-frames the guard must still be pending — the concealed \
     frames did NOT count as a resync"
  );

  // EOF with no keyframe ever fed: the guard is still pending → escalate, not a
  // silent clean end-of-stream.
  crate::accepted(dec.send_eof(), "send_eof on the SW path");
  drain(&mut dec, &mut concealed_frames, &mut escalation);
  let esc = escalation.expect(
    "concealed P-frames must NOT have cleared the guard, so reaching EOF without a \
     keyframe must ESCALATE with PostCommitNeverResynced",
  );
  let VideoDecodeError::PostCommitNeverResynced(p) = esc else {
    panic!("expected PostCommitNeverResynced, got {esc:?}");
  };
  let packets_lost = p.packets_lost();
  assert!(
    packets_lost >= 1,
    "every forwarded gap packet (current P-frame + the GOP-2 tail) must be \
     tallied as lost; got {packets_lost}"
  );
  assert!(
    !dec.degraded_resync_pending_for_test(),
    "the guard is cleared after the escalation fires"
  );
}

// ---------------------------------------------------------------------------
//  Post-commit retains ZERO replay frames (finding 1 dissolution)
// ---------------------------------------------------------------------------

/// **Finding-1 dissolution.** The post-commit path retains and reconstructs no
/// replay frames at all — it opens SW cold and forwards only the current packet
/// (or EOF). So the drained-replay-frame queue (`sw_replay_frames`), whose
/// later per-frame *conversion* finding 1 was about, is never populated on the
/// post-commit path: there is no deferred conversion that could reopen the
/// recovery hole. We assert the queue is empty right after a post-commit
/// fallback fires and stays empty as the stream is driven — there is simply
/// nothing to convert-after-commit.
#[test]
fn post_commit_retains_no_replay_frames() {
  let (w, h) = (128u32, 96u32);
  let clip = encode_synthetic_clip(w, h, 24, 6);
  let second_key = nth_keyframe(&clip, 2);
  let third_key = nth_keyframe(&clip, 3);
  let fail_at = second_key + 2;
  assert!(
    fail_at < third_key && !clip.packets[fail_at].is_key(),
    "fail target must be a mid-GOP P-frame before the next keyframe"
  );

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(
      w,
      h,
      fail_at,
      fail_at,
      FailShape::PostCommit,
    )),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  assert!(
    dec.sw_replay_frames_is_empty_for_test(),
    "no replay frames before any fallback"
  );

  // Feed packets [0, fail_at] WITHOUT draining: the send at `fail_at` fires the
  // post-commit fallback. If the post-commit path drained frames into the replay
  // queue (the removed terminal-drain behaviour), they would sit there now.
  for av_pkt in clip.packets.iter().take(fail_at + 1) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    assert!(
      dec.sw_replay_frames_is_empty_for_test(),
      "the post-commit path must retain ZERO replay frames — nothing is drained \
       into the replay queue, so there is no deferred conversion (finding 1)"
    );
  }
  assert!(
    dec.is_software(),
    "the mid-GOP failure fell back to software"
  );
  assert!(
    dec.degraded_resync_pending_for_test(),
    "post-commit fallback entered degraded mode (sanity)"
  );

  // Drive the rest of the stream; the replay queue must remain empty throughout
  // — the SW decoder delivers directly from itself, never from a replay buffer.
  let mut dst = crate::empty_owned_video_frame();
  for av_pkt in clip.packets.iter().skip(fail_at + 1) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    loop {
      match dec.receive_frame(&mut dst) {
        Ok(Received::Frame) => {}
        Ok(Received::NeedsInput | Received::Ended) => break,
        Err(e) => panic!("unexpected drain error: {e:?}"),
      }
    }
    assert!(
      dec.sw_replay_frames_is_empty_for_test(),
      "the post-commit path never populates the replay queue"
    );
  }
}

// ---------------------------------------------------------------------------
//  Placeholder seam smoke check
// ---------------------------------------------------------------------------

/// The inert seam builds a decoder on the HW path without driving anything —
/// guards `from_hw_inner_for_test` + the trimmed struct against regressions.
#[test]
fn inert_seam_builds_on_hardware() {
  ffmpeg_next::init().expect("ffmpeg init");
  let params = ffmpeg_next::codec::Parameters::new();
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(Box::new(FakeHw::inert()), params, tb)
    .expect("build test decoder");
  assert!(dec.is_hardware(), "inert seam starts on the HW path");
  assert!(!dec.is_software());
}

// ---------------------------------------------------------------------------
//  Deferred real-fixture integration test
// ---------------------------------------------------------------------------

/// Real-hardware counterpart to
/// [`post_commit_failure_degrades_and_resyncs_at_next_keyframe`]: drive an
/// actual Sony FX3 H.264 **High 4:2:2 10-bit** clip through the real
/// VideoToolbox path and observe whether the post-commit degrade-and-continue
/// fallback survives a *real* H.264 codec (the synthetic tests use a lenient
/// mpeg4 SW decoder; this resolves whether that leniency masks a real defect —
/// see findit-studio/mediadecode#12).
///
/// This is an **instrumented experiment**, not a green-checkmark assertion. It
/// captures (a) the starting backend, (b) the HW→SW transition point, (c) the
/// per-frame PTS delivered and the gap at the fallback boundary, and (d)
/// whether the cold SW decoder resynced at the next keyframe and decoded the
/// remainder — or aborted on a pre-keyframe P-frame / never saw the keyframe.
/// All of it is printed under `--nocapture`. The hard assertions at the end
/// encode the **observed** real-codec behaviour on this fixture.
///
/// Gated on `MEDIADECODE_FX3_SAMPLE` (absolute path to the fixture); skips
/// cleanly when unset so `cargo test` stays green without it. Run with:
///
/// ```sh
/// MEDIADECODE_FX3_SAMPLE=/path/to/12_sony_fx3_xavc.mp4 \
///   cargo test -p mediadecode-ffmpeg --all-features \
///   fx3_high_422_10bit -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires a Sony FX3 H.264 High 4:2:2 10-bit fixture (user-provided); \
            set MEDIADECODE_FX3_SAMPLE to its path"]
fn fx3_high_422_10bit_falls_back_to_software_and_decodes_whole_stream() {
  use ffmpeg_next::{format, media};

  let Some(path) = std::env::var_os("MEDIADECODE_FX3_SAMPLE") else {
    eprintln!(
      "skipping: set MEDIADECODE_FX3_SAMPLE to the Sony FX3 H.264 422-10bit fixture path to run \
       this experiment"
    );
    return;
  };

  ffmpeg_next::init().expect("ffmpeg init");

  let mut input = format::input(&path).expect("open FX3 input");
  let stream = input
    .streams()
    .best(media::Type::Video)
    .expect("video stream");
  let stream_index = stream.index();
  // SAFETY: `stream.parameters()` exposes a live `*const AVCodecParameters`
  // for the duration of the borrow; reading the geometry fields is sound.
  let (expected_w, expected_h) = unsafe {
    let p = stream.parameters();
    ((*p.as_ptr()).width as u32, (*p.as_ptr()).height as u32)
  };
  // A nominal time base for frame labelling — the experiment only inspects the
  // coverage/ordering of the resulting PTS, not its real-time scale.
  let tb = Timebase::new(1, NonZeroI32::new(24).expect("nonzero"));

  let mut dec = match FfmpegVideoStreamDecoder::open(
    stream.parameters(),
    tb,
    crate::DecoderLimits::default(),
  ) {
    Ok(d) => d,
    Err(Error::AllBackendsFailed(p)) => {
      // No HW backend opened at all → the wrapper went straight to SW at
      // open-time (probe-era), never exercising the post-commit path. Nothing
      // to observe; record and skip rather than false-fail.
      eprintln!(
        "skipping: no hardware backend available at open ({} attempts) — the post-commit \
         degrade path needs a HW backend that COMMITS then fails at runtime",
        p.attempts().len()
      );
      return;
    }
    Err(e) => panic!("open FX3 decoder: {e:?}"),
  };

  let mut obs = Fx3Observation::new(dec.is_hardware());
  eprintln!(
    "FX3 experiment: {expected_w}x{expected_h}; started_on_hw={} (is_software={})",
    obs.started_on_hw,
    dec.is_software()
  );

  let mut dst = crate::empty_owned_video_frame();

  'feed: for (s, packet) in input.packets() {
    if s.index() != stream_index {
      continue;
    }
    let is_key = packet.is_key();
    let pkt_pts = packet.pts();
    let Some(vpkt) = boundary::video_packet_from_ffmpeg(&packet).expect("a wrappable payload")
    else {
      continue; // empty packet (no payload) — skip
    };

    // send_packet, draining on EAGAIN.
    let mut attempts = 0u32;
    loop {
      match dec.send_packet(&vpkt) {
        Ok(Sent::Accepted) => break,
        // Back pressure, named. This loop is the two-offer rule's
        // replacement: drain, then offer the same packet again.
        Ok(Sent::MustDrain) => {
          if let Err(err) = obs.drain(&mut dec, &mut dst) {
            obs.abort = Some(format!("during send #{} EAGAIN-drain: {err}", obs.send_idx));
            break 'feed;
          }
          attempts += 1;
          assert!(
            attempts <= 64,
            "send_packet stuck on EAGAIN at send #{}",
            obs.send_idx
          );
        }
        Err(e) => {
          // A non-transient error surfacing from `send_packet` itself — capture
          // the variant. This is where a forwarded current packet that the cold
          // SW rejects would land (Codex finding 1's send-arm shape).
          obs.abort = Some(format!(
            "send_packet #{} (key={is_key}, pts={pkt_pts:?}) errored: {e:?}",
            obs.send_idx
          ));
          break 'feed;
        }
      }
    }
    if let Err(err) = obs.drain(&mut dec, &mut dst) {
      obs.abort = Some(format!(
        "after send #{} (key={is_key}): {err}",
        obs.send_idx
      ));
      break 'feed;
    }
    obs.send_idx += 1;
  }

  // EOF + final drain (only if we did not already abort mid-feed).
  if obs.abort.is_none() {
    match dec.send_eof() {
      Ok(Sent::Accepted) => {
        if let Err(err) = obs.drain(&mut dec, &mut dst) {
          obs.abort = Some(format!("during post-EOF drain: {err}"));
        }
      }
      Ok(Sent::MustDrain) => {
        obs.abort = Some("send_eof asked for a drain after the feed loop drained".into());
      }
      Err(VideoDecodeError::PostCommitNeverResynced(p)) => {
        obs.escalated_never_resynced = Some(p.packets_lost());
      }
      Err(e) => obs.abort = Some(format!("send_eof errored: {e:?}")),
    }
  }

  // ----- Report -----------------------------------------------------------
  let ended_on_sw = dec.is_software();
  let unique: std::collections::HashSet<i64> = obs.pts_out.iter().copied().collect();
  eprintln!("FX3 experiment RESULT:");
  eprintln!("  started_on_hw        = {}", obs.started_on_hw);
  eprintln!(
    "  transitioned_to_sw   = {} (at send #{:?})",
    obs.transitioned_to_sw, obs.transition_send_idx
  );
  eprintln!("  ended_on_sw          = {ended_on_sw}");
  eprintln!(
    "  frames_delivered     = {} (unique pts = {})",
    obs.pts_out.len(),
    unique.len()
  );
  eprintln!("  delivered_pts        = {:?}", obs.pts_out);
  eprintln!(
    "  resync_pending@end   = {}",
    dec.degraded_resync_pending_for_test()
  );
  eprintln!(
    "  never_resynced_esc   = {:?}",
    obs.escalated_never_resynced
  );
  eprintln!("  abort                = {:?}", obs.abort);

  // ----- Assertions on the OBSERVED behaviour -----------------------------
  // (1) The fixture must commit on HW first — otherwise this is not the
  //     post-commit path and the experiment is inconclusive (skip-shaped).
  assert!(
    obs.started_on_hw,
    "expected to start on the VideoToolbox HW path; if it opened straight to SW the post-commit \
     path was never exercised on this run"
  );

  // (2) A real HW runtime failure must have driven a transparent mid-stream
  //     HW->SW transition (the core #12 fix behaviour).
  assert!(
    obs.transitioned_to_sw && ended_on_sw,
    "expected a transparent mid-stream HW->SW fallback on the real FX3 clip (VideoToolbox cannot \
     decode H.264 High 4:2:2 10-bit at runtime); observed transition={}, ended_on_sw={ended_on_sw}, \
     abort={:?}",
    obs.transitioned_to_sw,
    obs.abort
  );

  // (3) The drive must not have ABORTED on a hard error before EOF. A
  //     pre-keyframe P-frame InvalidData (Codex finding 1) or a missed
  //     keyframe surfacing as a hard error would land here.
  assert!(
    obs.abort.is_none(),
    "the degrade-and-continue path aborted before EOF on the real H.264 codec: {:?} — this would \
     be Codex R7's finding reproducing on a real (non-lenient) codec",
    obs.abort
  );

  // (4) The fallback must have RESYNCED at the next keyframe and decoded the
  //     remainder — i.e. it did NOT escalate `PostCommitNeverResynced`, and
  //     the resync guard is clear at EOF. A bounded gap at the failure
  //     boundary is acceptable; never reaching a keyframe is the failure.
  assert!(
    obs.escalated_never_resynced.is_none(),
    "the cold SW decoder never resynced at a keyframe before EOF (PostCommitNeverResynced, {:?} \
     packets lost) — the whole tail was dropped; Codex R7's finding 2 (HW swallowed the keyframe / \
     cold SW never saw it) reproduces on real H.264",
    obs.escalated_never_resynced
  );
  assert!(
    !dec.degraded_resync_pending_for_test(),
    "a post-commit resync was still pending at EOF — SW never proved a keyframe-anchored resync"
  );

  // (5) Having resynced, SW must have delivered a non-trivial set of frames
  //     from the remainder, every one a real PTS, no duplicates.
  assert!(
    !obs.pts_out.is_empty(),
    "no frames were delivered at all — neither HW prefix nor SW remainder"
  );
  assert!(
    !obs.pts_out.contains(&i64::MIN),
    "every delivered frame must carry a real PTS: {:?}",
    obs.pts_out
  );
  assert_eq!(
    unique.len(),
    obs.pts_out.len(),
    "the degrade path must not re-emit a frame (no duplicate PTS): {:?}",
    obs.pts_out
  );
}

/// Instrumentation accumulator for the FX3 experiment: the observed backend
/// trajectory (HW start, the HW→SW transition point), the delivered PTS, and
/// any terminal error / escalation. Bundled into one value so the drive loop's
/// drain step is a single method call instead of threading seven `&mut`s.
struct Fx3Observation {
  /// Whether the decoder opened on the HW path (the precondition for
  /// exercising the post-commit degrade path at all).
  started_on_hw: bool,
  /// Set once the SW path is first observed active mid-drive.
  transitioned_to_sw: bool,
  /// `send_packet` index at which the HW→SW transition was first observed.
  transition_send_idx: Option<usize>,
  /// 0-based index of the current `send_packet`, advanced by the drive loop.
  send_idx: usize,
  /// PTS of every delivered frame, in delivery order (`i64::MIN` marks a hole).
  pts_out: Vec<i64>,
  /// `Debug` of the terminal error if the drive aborted before EOF.
  abort: Option<String>,
  /// `packets_lost` if the fallback escalated `PostCommitNeverResynced`.
  escalated_never_resynced: Option<u64>,
}

impl Fx3Observation {
  fn new(started_on_hw: bool) -> Self {
    Self {
      started_on_hw,
      transitioned_to_sw: false,
      transition_send_idx: None,
      send_idx: 0,
      pts_out: Vec::new(),
      abort: None,
      escalated_never_resynced: None,
    }
  }

  /// Note the HW→SW transition the first time the SW path is observed active
  /// (which can be before the cold SW produces any frame — it withholds output
  /// until the resync keyframe).
  fn note_transition(&mut self, dec: &FfmpegVideoStreamDecoder, frame_pending: bool) {
    if !self.transitioned_to_sw && dec.is_software() {
      self.transitioned_to_sw = true;
      self.transition_send_idx = Some(self.send_idx);
      let detail = if frame_pending {
        format!("frames delivered so far: {}", self.pts_out.len())
      } else {
        "no frame yet — cold SW awaiting resync keyframe".to_string()
      };
      eprintln!(
        "  -> HW->SW transition observed at/after send #{} ({detail})",
        self.send_idx
      );
    }
  }

  /// Drain every ready frame, recording delivered PTS and any escalation.
  /// Returns `Err(Debug)` on a non-transient decode error — the decisive
  /// observation, since the most-feared shape (Codex finding 1) is the cold SW
  /// decoder returning `InvalidData` / missing-reference on a pre-keyframe
  /// P-frame.
  fn drain(
    &mut self,
    dec: &mut FfmpegVideoStreamDecoder,
    dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, FfmpegBytes>,
  ) -> Result<(), String> {
    loop {
      match dec.receive_frame(dst) {
        Ok(Received::Frame) => {
          self.note_transition(dec, true);
          let pts = VideoFrame::pts(dst).map(|t| t.pts()).unwrap_or(i64::MIN);
          self.pts_out.push(pts);
        }
        Ok(Received::NeedsInput) => {
          self.note_transition(dec, false);
          break;
        }
        Ok(Received::Ended) => break,
        Err(VideoDecodeError::PostCommitNeverResynced(p)) => {
          let packets_lost = p.packets_lost();
          self.escalated_never_resynced = Some(packets_lost);
          eprintln!(
            "  -> PostCommitNeverResynced at EOF: {packets_lost} packets fed to SW produced no \
             frame (no keyframe crossed the gap)"
          );
          break;
        }
        Err(e) => return Err(format!("{e:?}")),
      }
    }
    Ok(())
  }
}

/// The cold software fallback's two forwarding calls must not lose an
/// allocator refusal.
///
/// `degrade_to_sw_inner` opens a **temporary** software decoder,
/// forwards the failure arm's input into it, and drops it on any error.
/// That decoder owns the callback state, so a `judge_buffer` refusal
/// recorded during either forward dies with it unless the reason is
/// collected first — which is why the state is captured before the
/// forward rather than reached for after it.
///
/// # Reachability, stated
///
/// The post-commit fallback itself cannot be driven end to end on this
/// platform: it needs a hardware backend to commit and then fail
/// mid-stream, and VideoToolbox is the only backend here. So the seam
/// is driven directly — a real `SwDecoder` opened through the same
/// `open_sw_decoder`, with the same two calls routed the same way.
///
/// The **EOF arm** carries a further honesty note: production reaches
/// it only on a *cold* decoder, which has no buffered output and so
/// allocates nothing, meaning no budget refusal is reachable through it
/// in practice. The routing is there for uniformity — one funnel, every
/// exit — and what this lane proves is that the routing works when the
/// call does refuse, not that production can make it refuse.
#[test]
fn the_cold_fallback_forwards_keep_the_allocator_refusal() {
  use crate::{DecoderLimits, FrameLimits, error::FrameMedium};

  // 640x480 `yuv420p` costs about 460 KB once allocated; 64 KiB refuses
  // it, and the refusal has to arrive named rather than as the `EINVAL`
  // libavcodec also uses for corrupt input.
  let clip = encode_synthetic_clip(640, 480, 12, 3);
  let limits = DecoderLimits::new().with_frame(FrameLimits::new().with_max_frame_bytes(64 * 1024));

  let named = |e: &Error| match e {
    Error::FrameBudgetExceeded(p) => Some(*p),
    _ => None,
  };

  // **The packet arm**, exactly as `degrade_to_sw_inner` drives it:
  // capture the state, forward, route the error.
  let mut sw = super::open_sw_decoder(&clip.parameters, limits).expect("open sw");
  let state = sw.state();
  let refusal = sw
    .send_packet(&clip.packets[0])
    .map_err(|e| crate::decoder::software_exit(state, e))
    .expect_err("a 460 KB frame passed a 64 KiB ceiling");
  let payload = named(&refusal).expect("the packet arm lost the allocator refusal");
  assert_eq!(payload.medium(), FrameMedium::Video);
  assert_eq!(payload.limit(), 64 * 1024);
  assert!(payload.bytes() > payload.limit());

  // **The EOF arm**, driven on a decoder that has something to flush so
  // the call can actually refuse — see the reachability note above.
  let mut sw = super::open_sw_decoder(&clip.parameters, limits).expect("open sw");
  let state = sw.state();
  // Feed without collecting, so whatever the decoder buffers is still
  // pending when EOF arrives.
  let _ = sw.send_packet(&clip.packets[0]);
  if let Err(e) = sw
    .send_eof()
    .map_err(|e| crate::decoder::software_exit(state, e))
  {
    let payload = named(&e).expect("the EOF arm lost the allocator refusal");
    assert_eq!(payload.limit(), 64 * 1024);
  }

  // And under a budget that fits, the same forward succeeds — the seat
  // refuses cost, not fallbacks.
  let generous = DecoderLimits::new()
    .with_frame(FrameLimits::new().with_max_frame_bytes(crate::DEFAULT_MAX_FRAME_BYTES));
  let mut sw = super::open_sw_decoder(&clip.parameters, generous).expect("open sw");
  let state = sw.state();
  sw.send_packet(&clip.packets[0])
    .map_err(|e| crate::decoder::software_exit(state, e))
    .expect("an affordable frame must be accepted");
}

#[test]
fn a_rescued_packet_never_aliases_a_view_carrier() {
  use crate::{CarrierVideoStreamDecoder, View, boundary::video_packet_from_ffmpeg_in};
  use ffmpeg_next::packet::Ref;
  use mediadecode::decoder::VideoStreamDecoder;

  // **The scoped submission's proof has a hole on one road.** "Built,
  // lent, dropped inside this call" is true of the function — and false
  // of the probe, which `av_packet_ref`s every accepted packet into a
  // rescue history that `FallbackFailed::unconsumed_packets` hands back
  // to the caller as owned, **mutable** `Packet`s. A shared body would
  // leave that call as a live mutable alias of bytes a view carrier is
  // still lending.
  //
  // So while the history is being recorded, the body is copied. This
  // pins it from the outside, on the one road where the history is
  // observable: a probe-era failure whose SW replay also fails.
  let (w, h) = (128u32, 96u32);
  let mut clip = encode_synthetic_clip(w, h, 12, 100);
  let p1 = clip
    .packets
    .iter()
    .position(|p| !p.is_key())
    .expect("clip has P-frames");
  assert!(
    p1 + 2 < clip.packets.len(),
    "need packets after the corrupt one"
  );
  corrupt_packet_payload(&mut clip.packets[p1]);

  let fail_at = p1 + 3;
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, fail_at, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  // Retain every carrier — which is exactly what a consumer parking
  // view packets would do, and what makes an alias observable.
  let mut retained: Vec<crate::VideoPacket> = Vec::new();
  let mut dst = crate::boundary::empty_video_frame();
  let mut rescued: Vec<ffmpeg_next::Packet> = Vec::new();
  for av_pkt in &clip.packets {
    let Some(vpkt) =
      video_packet_from_ffmpeg_in(av_pkt.clone(), tb, crate::PacketLimits::default())
        .expect("a wrappable payload")
    else {
      continue;
    };
    match dec.send_packet(&vpkt) {
      Ok(Sent::Accepted | Sent::MustDrain) => {}
      Err(VideoDecodeError::Decode(Error::FallbackFailed(f))) => {
        retained.push(vpkt);
        rescued = f.into_unconsumed_packets();
        break;
      }
      Err(e) => panic!("send_packet: {e:?}"),
    }
    retained.push(vpkt);
    // Exhaustive, not `.is_ok()`: "needs input" is a success now, so a
    // predicate loop here would never leave.
    while matches!(dec.receive_frame(&mut dst), Ok(Received::Frame)) {}
  }

  assert!(
    !rescued.is_empty(),
    "the failed fallback must surface a rescue history to check",
  );
  let carriers: Vec<usize> = retained
    .iter()
    .map(|p| p.data().as_ref().as_ptr() as usize)
    .collect();
  for packet in &rescued {
    // SAFETY: the packet is live; `data` is a public field.
    let address = unsafe { (*packet.as_ptr()).data as usize };
    assert!(
      !carriers.contains(&address),
      "a rescued packet addresses a retained view carrier's storage — \
       `data_mut` on it would be an aliasing write",
    );
  }

  // And the rescued packets really are writable, which is what makes
  // the aliasing question live rather than theoretical. Writing through
  // every one of them must leave every carrier's bytes alone.
  let before: Vec<Vec<u8>> = retained
    .iter()
    .map(|p| p.data().as_ref().to_vec())
    .collect();
  {
    for packet in &mut rescued {
      if let Some(slot) = packet.data_mut() {
        for byte in slot.iter_mut() {
          *byte ^= 0xFF;
        }
      }
    }
  }
  for (packet, expected) in retained.iter().zip(before) {
    assert_eq!(
      packet.data().as_ref(),
      expected.as_slice(),
      "writing a rescued packet reached a retained view carrier",
    );
  }
}

#[test]
fn the_receive_time_fallback_queue_survives_a_failed_carrier() {
  use crate::{CarrierVideoStreamDecoder, View, boundary::video_packet_from_ffmpeg_in};
  use mediadecode::decoder::VideoStreamDecoder;

  // **The replay queue's second delivery path.** `fall_back_to_sw`
  // fills `sw_replay_frames` from inside `receive_frame` when the probe
  // is exhausted at *frame* time, and that branch converts the head
  // then and there. It used to `pop_front` first, so an allocation that
  // failed advanced past a frame the rescue history holds the only copy
  // of — the very loss the queue exists to prevent. It peeks now, like
  // the entry at the top of `receive_frame`.
  //
  // The ceiling is process-global, so this runs alone.
  crate::fault_subprocess::in_subprocess(
    "video::tests::the_receive_time_fallback_queue_survives_a_failed_carrier",
    || {
      let (w, h) = (64u32, 48u32);
      let clip = encode_synthetic_clip(w, h, 8, 100);
      let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

      // `cap_when_queued`: lower the ceiling on the first receive that
      // provably comes off the replay queue — checked with the queue's
      // own emptiness, so the lane cannot drift onto the scratch road
      // and quietly assert nothing.
      let drive = |cap_when_queued: bool| -> (Vec<Vec<u8>>, bool) {
        let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(
          Box::new(FakeHw::failing_at_receive(w, h)),
          clip.parameters.clone(),
          tb,
        )
        .expect("build test decoder");

        let mut frame = crate::boundary::empty_video_frame();
        let mut planes = Vec::new();
        let mut queue_was_hit = false;
        let mut armed = false;

        // **Send first, drain second.** The fake keeps every accepted
        // packet as rescue history and queues no frames of its own, so
        // the history is as deep as the clip by the time a frame is
        // asked for — which is what makes `fall_back_to_sw` replay more
        // than one packet and leave more than one frame on the queue.
        // Interleaving send and receive would fail the seam on the
        // first packet and give the queue a single frame, consumed in
        // the same call, leaving this lane nothing to cap.
        for av_pkt in &clip.packets {
          let Some(vpkt) =
            video_packet_from_ffmpeg_in(av_pkt.clone(), tb, crate::PacketLimits::default())
              .expect("a wrappable payload")
          else {
            continue;
          };
          if dec.send_packet(&vpkt).is_err() {
            break;
          }
        }

        loop {
          let capped = armed;
          if capped {
            armed = false;
            queue_was_hit = true;
            crate::fault_subprocess::cap_ffmpeg_allocations(16);
          }
          let got = dec.receive_frame(&mut frame);
          if capped {
            crate::fault_subprocess::uncap_ffmpeg_allocations();
          }
          match got {
            Ok(Received::Frame) => {
              planes.push(frame.planes()[0].data_ref().as_ref().to_vec());
              // The next receive will come off the queue, which is the
              // road this lane is for.
              if cap_when_queued && !queue_was_hit && !dec.sw_replay_frames_is_empty_for_test() {
                armed = true;
              }
            }
            // Nothing more from this packet — feed the next one.
            Ok(Received::NeedsInput | Received::Ended) => break,
            Err(VideoDecodeError::Convert(e)) => {
              assert!(capped, "no refusal was asked for here, got {e:?}");
              assert!(
                e.parks_in_decode(),
                "the ceiling must produce a parkable refusal, got {e:?}",
              );
              // Retry immediately, uncapped: the same frame must come
              // back rather than the one after it.
              continue;
            }
            Err(_) => break,
          }
        }
        (planes, queue_was_hit)
      };

      let (reference, _) = drive(false);
      assert!(
        reference.len() >= 2,
        "the receive-time fallback must deliver replayed frames to test with",
      );
      // **Where the ceiling can go, and where it cannot.** The very
      // first delivery on this road happens in the same call that runs
      // `fall_back_to_sw` — which opens a software decoder and replays
      // packets through it — so a ceiling there refuses the fallback
      // rather than the carrier, and nothing outside can lower it
      // between the two. What *is* reachable is the queue that
      // fallback filled: a carrier failing on a later delivery must
      // leave its frame at the head.
      let (recovered, queue_was_hit) = drive(true);
      assert!(
        queue_was_hit,
        "the ceiling never reached the replay queue — this lane would \
         assert nothing",
      );
      assert_eq!(
        recovered, reference,
        "a transient refusal must cost no replayed frame at all",
      );
    },
  );
}

// ---------------------------------------------------------------------------
//  R2: the end of the stream outranks the parked seat, on both send gates
// ---------------------------------------------------------------------------

/// The cross product this lane needs: a session that has **accepted**
/// end-of-stream *and* has a frame parked in its seat.
///
/// Reaching it takes both halves at once — `send_eof` committed, then a
/// delayed tail frame drained out of the decoder whose carrier
/// allocation fails parkably. `hw` picks which scratch holds it.
fn eof_with_a_parked_frame(
  hw: bool,
) -> (
  crate::CarrierVideoStreamDecoder<crate::View>,
  SyntheticClip,
  Timebase,
) {
  use crate::{CarrierVideoStreamDecoder, View, boundary::video_packet_from_ffmpeg_in};
  use mediadecode::decoder::VideoStreamDecoder;

  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 100);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  // A seam that never fails keeps us on the hardware scratch; one that
  // raises probe-era exhaustion drops us onto the real software decoder
  // with the history replayed losslessly, so both scratches — the two
  // the parked-seat gate exists for — are proved.
  let seam: Box<dyn HwInner> = if hw {
    Box::new(FakeHw::failing(
      w,
      h,
      usize::MAX,
      usize::MAX,
      FailShape::PostCommit,
    ))
  } else {
    Box::new(FakeHw::failing(w, h, 0, 2, FailShape::ProbeEra))
  };
  let mut dec =
    CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(seam, clip.parameters.clone(), tb)
      .expect("build test decoder");

  let packet = |index: usize| {
    video_packet_from_ffmpeg_in(
      clip.packets[index].clone(),
      tb,
      crate::PacketLimits::default(),
    )
    .expect("a wrappable payload")
    .expect("packet has a buffer")
  };
  let mut frame = crate::boundary::empty_video_frame();

  // **The two roads need different feeds, and saying so is the point.**
  // The hardware seam hands back exactly the frames it was given, so it
  // must still be holding one at EOF. The software road arrives through
  // a probe-era fallback, whose replayed history lands in a *queue*
  // rather than the scratch — so that queue is drained to empty first,
  // and only then is a real `sw.receive_frame` frame available to park.
  if hw {
    for index in 0..4 {
      crate::accepted(dec.send_packet(&packet(index)), "send_packet");
    }
  } else {
    for index in 0..3 {
      crate::accepted(dec.send_packet(&packet(index)), "send_packet");
    }
    while matches!(dec.receive_frame(&mut frame), Ok(Received::Frame)) {}
    // The feeder loop this reform made writable: the software decoder
    // has real output now, so it exerts real back pressure, and the
    // answer to that is to drain and re-offer the same packet.
    for index in 3..clip.packets.len().min(6) {
      let pkt = packet(index);
      loop {
        match dec.send_packet(&pkt).expect("no fault while feeding") {
          Sent::Accepted => break,
          Sent::MustDrain => while matches!(dec.receive_frame(&mut frame), Ok(Received::Frame)) {},
        }
      }
    }
  }
  assert_eq!(dec.is_hardware(), hw, "the intended road");
  assert!(
    dec.sw_replay_frames_is_empty_for_test(),
    "the replay queue must be empty, or the park below lands in it \
     instead of the scratch seat this lane is about",
  );

  // The end, accepted — this is what sets `eof_sent`.
  crate::accepted(dec.send_eof(), "send_eof");
  assert!(
    dec.eof_sent_for_test(),
    "precondition: the end must be committed for this lane to mean anything",
  );

  // Now park a tail frame: the ceiling refuses the carrier, and the
  // refusal is one another attempt could survive, so the seat keeps it.
  crate::fault_subprocess::cap_ffmpeg_allocations(16);
  let refused = dec.receive_frame(&mut frame);
  crate::fault_subprocess::uncap_ffmpeg_allocations();
  match refused {
    Err(VideoDecodeError::Convert(e)) => assert!(
      e.parks_in_decode(),
      "the ceiling must produce a parkable refusal, got {e:?}",
    ),
    other => panic!("expected a parkable refusal after EOF, got {other:?}"),
  }

  (dec, clip, tb)
}

/// **Regression: `Sent::MustDrain` is a promise, and past end-of-stream
/// it is one this face cannot keep.**
///
/// The arm's whole contract is *drain the output and this same offer
/// becomes acceptable*. With `eof_sent` committed it never becomes
/// acceptable — draining empties the seat and the retry faults anyway,
/// until `flush`. So a caller that obeys the contract loops, drains,
/// re-offers, and is refused: the same fault-under-back-pressure
/// inversion the subtitle seam carried, one surface over.
///
/// Both send gates, and both **before and after** the drain that the
/// bad answer would have sent the caller to do.
fn a_post_eof_send_is_a_fault_not_backpressure(hw: bool) {
  use crate::boundary::video_packet_from_ffmpeg_in;
  use mediadecode::decoder::VideoStreamDecoder;

  let (mut dec, clip, tb) = eof_with_a_parked_frame(hw);
  let packet = |index: usize| {
    video_packet_from_ffmpeg_in(
      clip.packets[index].clone(),
      tb,
      crate::PacketLimits::default(),
    )
    .expect("a wrappable payload")
    .expect("packet has a buffer")
  };

  let is_after_eof = |got: &Result<Sent, VideoDecodeError>| {
    matches!(
      got,
      Err(VideoDecodeError::Decode(Error::Ffmpeg(
        ffmpeg_next::Error::Eof
      )))
    )
  };

  // --- with the seat still parked -----------------------------------
  let sent = dec.send_packet(&packet(4));
  assert!(
    is_after_eof(&sent),
    "a packet after a committed EOF must be the fault even with the seat \
     parked — `MustDrain` here promises a retry that can never succeed; got {sent:?}",
  );
  let eof_again = dec.send_eof();
  assert!(
    is_after_eof(&eof_again),
    "the same for a repeated end-of-stream; got {eof_again:?}",
  );

  // --- the drain the bad answer would have prescribed ----------------
  // It succeeds (the parked frame is still deliverable), and it changes
  // nothing about the send side. That is the point: `MustDrain` would
  // have sent the caller here for nothing.
  let mut frame = crate::boundary::empty_video_frame();
  let mut drained = 0u32;
  for _ in 0..64 {
    match dec.receive_frame(&mut frame) {
      Ok(Received::Frame) => drained += 1,
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(e) => panic!("the parked frame must still be deliverable: {e:?}"),
    }
  }
  assert!(drained > 0, "the parked frame was never recovered");

  // --- with the seat free -------------------------------------------
  let sent_after = dec.send_packet(&packet(5));
  assert!(
    is_after_eof(&sent_after),
    "draining did not make the offer acceptable, which is exactly why the \
     parked answer must not have been `MustDrain`; got {sent_after:?}",
  );
  assert!(
    is_after_eof(&dec.send_eof()),
    "and the same for the repeated end-of-stream",
  );

  // `flush` is the only way back, and it really is one.
  dec.flush().expect("flush");
  assert!(
    !dec.eof_sent_for_test(),
    "flush must retract the committed end",
  );
  crate::accepted(dec.send_packet(&packet(0)), "flush reopened the send side");
}

/// The hardware scratch holds the parked frame.
#[test]
fn a_post_eof_send_is_a_fault_not_backpressure_on_the_hardware_road() {
  crate::fault_subprocess::in_subprocess(
    "video::tests::a_post_eof_send_is_a_fault_not_backpressure_on_the_hardware_road",
    || a_post_eof_send_is_a_fault_not_backpressure(true),
  );
}

/// And the software scratch, after a post-commit fallback put us there —
/// the two scratches are the reason the parked-seat gate exists at all,
/// so the ordering is proved against both.
#[test]
fn a_post_eof_send_is_a_fault_not_backpressure_on_the_software_road() {
  crate::fault_subprocess::in_subprocess(
    "video::tests::a_post_eof_send_is_a_fault_not_backpressure_on_the_software_road",
    || a_post_eof_send_is_a_fault_not_backpressure(false),
  );
}

/// A hardware seam that accepts everything and then raises a
/// **post-commit** exhaustion the first time a frame is asked for — the
/// frame-time fallback road, entered on a session whose end is already
/// committed.
struct FakeHwPostCommitAtFrameTime {
  raised: bool,
}

impl HwInner for FakeHwPostCommitAtFrameTime {
  fn records_submissions(&self) -> bool {
    false
  }
  fn send_packet(&mut self, _: &Packet) -> Result<Sent, Error> {
    Ok(Sent::Accepted)
  }
  fn receive_frame(&mut self, _: &mut Frame) -> Result<Received, Error> {
    if self.raised {
      return Ok(Received::NeedsInput);
    }
    self.raised = true;
    Err(Error::AllBackendsFailed(
      crate::error::AllBackendsFailed::new_post_commit(Vec::new()),
    ))
  }
  fn send_eof(&mut self) -> Result<Sent, Error> {
    Ok(Sent::Accepted)
  }
  fn flush(&mut self) -> Result<(), Error> {
    Ok(())
  }
  fn as_video_decoder(&self) -> Option<&VideoDecoder> {
    None
  }
}

/// **Regression: a protocol state with no satisfying operation must not
/// reach the caller.**
///
/// The road: hardware accepts end-of-stream, so `eof_sent` commits;
/// then a post-commit exhaustion arrives *while draining*, and the
/// frame-time fallback opens software cold. If the committed end does
/// not travel with that fallback, the cold decoder answers `EAGAIN`
/// forever — [`Received::NeedsInput`], an instruction to send another
/// packet — on a session where both send gates now refuse. The caller
/// can only spin or quietly keep a truncated tail.
///
/// This is an **interlock**, not a plain bug: the gates are correct and
/// the fallback was correct before them; together they closed every
/// exit. Before the gates existed, a repeated `send_eof` would have
/// re-armed the cold decoder by accident, which is the sort of luck a
/// protocol should not depend on.
///
/// What must be true afterwards is stated as the property rather than
/// the mechanism: **whatever the decoder answers, it is never
/// `NeedsInput`,** and the drain terminates.
#[test]
fn a_post_eof_frame_time_fallback_never_strands_the_caller_in_needs_input() {
  use crate::{CarrierVideoStreamDecoder, View, boundary::video_packet_from_ffmpeg_in};
  use mediadecode::decoder::VideoStreamDecoder;

  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 100);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(
    Box::new(FakeHwPostCommitAtFrameTime { raised: false }),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  let pkt =
    video_packet_from_ffmpeg_in(clip.packets[0].clone(), tb, crate::PacketLimits::default())
      .expect("a wrappable payload")
      .expect("packet has a buffer");
  crate::accepted(dec.send_packet(&pkt), "send_packet");

  // The end, accepted on the hardware seam — `eof_sent` commits here.
  crate::accepted(dec.send_eof(), "send_eof");
  assert!(
    dec.eof_sent_for_test(),
    "precondition: the end must be committed before the fallback fires",
  );

  // Drain. The first poll raises the post-commit exhaustion and takes
  // the frame-time fallback road.
  let mut frame = crate::boundary::empty_video_frame();
  let mut terminal = false;
  for _ in 0..64 {
    match dec.receive_frame(&mut frame) {
      Ok(Received::Frame) => {}
      Ok(Received::NeedsInput) => panic!(
        "stranded: the decoder asked for input on a session whose end is \
         committed, and both send gates refuse — no legal operation can \
         satisfy this answer",
      ),
      Ok(Received::Ended) => {
        terminal = true;
        break;
      }
      // The honest fault: the cold decoder was handed the end and had
      // nothing to give, so the tail really was lost and says so.
      Err(VideoDecodeError::PostCommitNeverResynced(_)) => {
        terminal = true;
        break;
      }
      Err(e) => panic!("unexpected fault while draining: {e:?}"),
    }
  }
  assert!(terminal, "the drain never reached a terminal answer");
  assert!(dec.is_software(), "the frame-time fallback did commit");

  // **Isolating the forwarding from the guard that also covers it.**
  //
  // Two things keep the caller out of `NeedsInput` here: the committed
  // end travelling with the fallback, and [`settle`] refusing to hand
  // back an unsatisfiable state. That is deliberate depth, but it means
  // the property above passes if only one of them is present — so this
  // asks the cold decoder itself, past the wrapper, which of the two
  // did the work. A decoder that was handed the end answers
  // `AVERROR_EOF`; one still cold answers `EAGAIN`.
  let DecodeState::Sw(sw) = &mut dec.state else {
    panic!("the software decoder must be the one in the seat");
  };
  let mut scratch = alloc_av_video_frame().expect("frame slot");
  let raw = sw
    .receive_frame(&mut scratch)
    .expect_err("a cold decoder handed only the end produces no frame");
  assert!(
    matches!(raw, ffmpeg_next::Error::Eof),
    "the cold software decoder never received the committed end — it answered \
     {raw:?}, which reaches a caller as `NeedsInput` and cannot be satisfied",
  );

  // And the session stays terminal: polling past the end keeps
  // answering the end, never sending the caller back for input.
  for _ in 0..3 {
    assert_eq!(
      dec
        .receive_frame(&mut frame)
        .expect("no fault past the end"),
      Received::Ended,
    );
  }
}

/// **The synthesized fault, checked against the substrate — reaching
/// both sides this time.**
///
/// The previous version of this lane was a tautology and passed for the
/// wrong reason: it called `send_eof` on the *wrapper*, which commits
/// `eof_sent`, so the later `send_packet` returned through the wrapper's
/// own gate. It compared [`CarrierVideoStreamDecoder::after_eof`] with
/// itself and would have passed had libavcodec diverged completely.
///
/// The lesson generalises past this one test: **revert-verification
/// catches a deleted gate, not a comparison that never crossed the
/// seam.** A parity pin has to reach both sides it claims to compare,
/// and be written so that it fails if either moves.
///
/// So this one goes around the gate: it reaches the raw inner software
/// decoder — the actual `ffmpeg::decoder::Video` — feeds it the flush
/// packet directly, and reads what libavcodec really answers to a
/// submission after end-of-stream.
#[test]
fn the_post_eof_fault_is_the_one_the_substrate_gives() {
  use crate::{CarrierVideoStreamDecoder, View, boundary::video_packet_from_ffmpeg_in};
  use mediadecode::decoder::VideoStreamDecoder;

  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 100);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  // Probe-era exhaustion puts the real software decoder in the seat —
  // the fake seam has no EOF state machine to interrogate.
  let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, 2, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  for index in 0..3 {
    let pkt = video_packet_from_ffmpeg_in(
      clip.packets[index].clone(),
      tb,
      crate::PacketLimits::default(),
    )
    .expect("a wrappable payload")
    .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&pkt), "send_packet");
  }
  assert!(
    dec.is_software(),
    "the substrate under test is libavcodec's"
  );

  // **Past the wrapper entirely.** `tests` is a child module, so the
  // private state is reachable; the point is that nothing below asks
  // the wrapper anything.
  let DecodeState::Sw(sw) = &mut dec.state else {
    panic!("the software decoder must be the one in the seat");
  };
  sw.send_eof().expect("the substrate takes the end");

  // What libavcodec actually answers a packet after the flush packet.
  let substrate = sw
    .send_packet(&clip.packets[3])
    .expect_err("libavcodec must refuse a packet after end-of-stream");
  assert!(
    matches!(substrate, ffmpeg_next::Error::Eof),
    "the substrate's post-EOF refusal moved: got {substrate:?}",
  );

  // Both wrapper roads wrap that value identically — the software road
  // through `software_exit` (which passes it through when no refusal
  // was recorded) and the hardware road through its own
  // `Err(e @ Eof) => Err(Error::Ffmpeg(e))` arm. Pinning the funnel's
  // output makes the hardware claim a checked identity rather than an
  // assertion: it is the same `Error::Ffmpeg` construction, on a value
  // the line above proved is what the substrate gives.
  let wrapped = crate::decoder::software_exit(core::ptr::null(), substrate);
  assert!(
    matches!(wrapped, Error::Ffmpeg(ffmpeg_next::Error::Eof)),
    "the funnel changed how a post-EOF refusal is wrapped: got {wrapped:?}",
  );

  // And that is exactly what the gates hand back without asking.
  let synthesized = CarrierVideoStreamDecoder::<View>::after_eof();
  assert!(
    matches!(
      synthesized,
      VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))
    ),
    "the synthesized post-EOF fault drifted from the substrate's: got {synthesized:?}",
  );
}

#[test]
fn a_parked_hardware_frame_is_delivered_before_any_fallback() {
  use crate::{CarrierVideoStreamDecoder, View, boundary::video_packet_from_ffmpeg_in};
  use mediadecode::decoder::VideoStreamDecoder;

  // **A parked frame pins the state that parked it.** The scratch a
  // retry reads is chosen by the *current* `DecodeState`, so a
  // hardware-to-software fallback committed while a hardware frame was
  // parked would send the retry to the software scratch — delivering a
  // stale frame, or refusing permanently and stranding a decoded one.
  // Both send roads can commit that fallback, so both refuse while the
  // seat is taken.
  crate::fault_subprocess::in_subprocess(
    "video::tests::a_parked_hardware_frame_is_delivered_before_any_fallback",
    || {
      let (w, h) = (64u32, 48u32);
      let clip = encode_synthetic_clip(w, h, 8, 100);
      let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
      let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(
        // The second send is the one that would commit a post-commit
        // fallback — which is exactly the transition that must not
        // happen underneath a parked frame.
        Box::new(FakeHw::failing(w, h, usize::MAX, 1, FailShape::PostCommit)),
        clip.parameters.clone(),
        tb,
      )
      .expect("build test decoder");

      let packet = |index: usize| {
        video_packet_from_ffmpeg_in(
          clip.packets[index].clone(),
          tb,
          crate::PacketLimits::default(),
        )
        .expect("a wrappable payload")
        .expect("packet has a buffer")
      };

      crate::accepted(dec.send_packet(&packet(0)), "send_packet");
      assert!(dec.is_hardware(), "the seam under test is the hardware one");

      // Park the hardware frame.
      let mut frame = crate::boundary::empty_video_frame();
      crate::fault_subprocess::cap_ffmpeg_allocations(16);
      let refused = dec.receive_frame(&mut frame);
      crate::fault_subprocess::uncap_ffmpeg_allocations();
      match refused {
        Err(VideoDecodeError::Convert(e)) => assert!(
          e.parks_in_decode(),
          "the ceiling must produce a parkable refusal, got {e:?}",
        ),
        other => panic!("expected a parkable refusal, got {other:?}"),
      }

      // **Nothing may be sent while it is parked** — this is the send
      // that could otherwise have committed a fallback underneath it.
      // The discipline is unchanged; it is spelled as back pressure
      // now, which is what it always was: nothing was consumed, and
      // the escape has always been `receive_frame` or `flush`.
      assert!(
        matches!(dec.send_packet(&packet(1)), Ok(Sent::MustDrain)),
        "a send under a parked frame must be told to drain first",
      );
      assert!(
        matches!(dec.send_eof(), Ok(Sent::MustDrain)),
        "EOF under a parked frame must be told to drain first too",
      );

      // The parked frame is still the hardware one, and it arrives.
      assert_eq!(
        dec.receive_frame(&mut frame).expect("the parked frame"),
        Received::Frame,
      );
      assert!(
        dec.is_hardware(),
        "no fallback can have happened while the frame was parked",
      );
      assert!(
        !frame.planes()[0].data_ref().as_ref().is_empty(),
        "the delivered frame must carry the decoded planes",
      );

      // And once the seat is free the send reaches the seam — this is
      // the one that would have committed the fallback underneath the
      // parked frame. Whether the cold software decoder then accepts a
      // lone P-frame is not this lane's business; that it is no longer
      // *refused* is.
      let after = dec.send_packet(&packet(1));
      assert!(
        !matches!(after, Ok(Sent::MustDrain)),
        "with the seat free the send must reach the seam, got {after:?}",
      );
    },
  );
}

#[test]
fn a_parked_recovery_frame_still_clears_the_resync_guard() {
  use crate::{CarrierVideoStreamDecoder, View, boundary::video_packet_from_ffmpeg_in};
  use mediadecode::decoder::VideoStreamDecoder;

  // **The bookkeeping a delivery owes must survive the retry road.**
  // A post-commit degrade leaves a keyframe-anchored resync guard
  // standing until a frame arrives after the keyframe. That frame's
  // delivery is what clears it — and a delivery that had been parked
  // and was re-attempted used to reach the caller through a road that
  // skipped `resync_on_frame`, so the guard survived the very frame
  // that should have cleared it and EOF escalated with a false
  // `PostCommitNeverResynced`.
  crate::fault_subprocess::in_subprocess(
    "video::tests::a_parked_recovery_frame_still_clears_the_resync_guard",
    || {
      let (w, h) = (128u32, 96u32);
      let clip = encode_synthetic_clip(w, h, 24, 6);
      let second_key = nth_keyframe(&clip, 2);
      let third_key = nth_keyframe(&clip, 3);
      let fail_at = second_key + 2;
      assert!(fail_at < third_key && !clip.packets[fail_at].is_key());

      let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
      let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(
        Box::new(FakeHw::failing(
          w,
          h,
          fail_at,
          fail_at,
          FailShape::PostCommit,
        )),
        clip.parameters.clone(),
        tb,
      )
      .expect("build test decoder");

      let packet = |index: usize| {
        video_packet_from_ffmpeg_in(
          clip.packets[index].clone(),
          tb,
          crate::PacketLimits::default(),
        )
        .expect("a wrappable payload")
        .expect("packet has a buffer")
      };
      let mut dst = crate::boundary::empty_video_frame();

      // Degrade post-commit, then walk to the resync keyframe.
      for index in 0..=fail_at {
        crate::accepted(dec.send_packet(&packet(index)), "send_packet");
      }
      assert!(
        dec.is_software(),
        "the mid-GOP failure fell back to software"
      );
      assert!(dec.degraded_resync_pending_for_test(), "the gap is open");

      // `true` only while frames are actually coming out — the two
      // non-frame states both stop the loop, and a fault still panics.
      let drain = |dec: &mut CarrierVideoStreamDecoder<View>,
                   dst: &mut crate::VideoFrame|
       -> bool { matches!(dec.receive_frame(dst), Ok(Received::Frame)) };
      while drain(&mut dec, &mut dst) {}
      for index in (fail_at + 1)..third_key {
        crate::accepted(dec.send_packet(&packet(index)), "send_packet");
        while drain(&mut dec, &mut dst) {}
      }
      crate::accepted(dec.send_packet(&packet(third_key)), "send the keyframe");
      assert!(
        dec.degraded_keyframe_seen_for_test(),
        "the keyframe crossed the gap and is the resync anchor",
      );
      assert!(
        dec.degraded_resync_pending_for_test(),
        "no post-keyframe frame has been delivered yet",
      );

      // **Park the recovery frame.** This is the delivery that clears
      // the guard, and it is going to fail its carrier first.
      let mut parked = false;
      for attempt in 0..64 {
        crate::fault_subprocess::cap_ffmpeg_allocations(16);
        let got = dec.receive_frame(&mut dst);
        crate::fault_subprocess::uncap_ffmpeg_allocations();
        match got {
          Err(VideoDecodeError::Convert(e)) if e.parks_in_decode() => {
            parked = true;
            break;
          }
          Ok(Received::Frame) => {
            assert!(
              dec.degraded_resync_pending_for_test(),
              "the guard cleared before the parked delivery — nothing left to test",
            );
          }
          Ok(Received::NeedsInput | Received::Ended) | Err(_) => {
            // No frame ready under this packet; feed the next one.
            let index = third_key + 1 + attempt;
            if index >= clip.packets.len() {
              break;
            }
            crate::accepted(dec.send_packet(&packet(index)), "send_packet");
          }
        }
      }
      assert!(parked, "the ceiling must park the recovery frame");
      assert!(
        dec.degraded_resync_pending_for_test(),
        "a parked frame has not been delivered, so the guard still stands",
      );

      // The retry delivers it — and the bookkeeping runs on that road.
      assert_eq!(
        dec
          .receive_frame(&mut dst)
          .expect("the parked recovery frame"),
        Received::Frame,
      );
      assert!(
        !dec.degraded_resync_pending_for_test(),
        "the retried delivery must clear the keyframe-anchored resync guard",
      );

      // And EOF is clean: no false escalation over a gap that did
      // resync.
      crate::accepted(dec.send_eof(), "send_eof");
      loop {
        match dec.receive_frame(&mut dst) {
          Ok(Received::Frame) => {}
          Ok(Received::NeedsInput | Received::Ended) => break,
          Err(VideoDecodeError::PostCommitNeverResynced(p)) => {
            panic!("false escalation after a resync that did happen: {p:?}");
          }
          Err(_) => break,
        }
      }
    },
  );
}

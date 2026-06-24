use super::*;

use mediadecode::decoder::VideoStreamDecoder;
use std::num::NonZeroU32;

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
  queued: VecDeque<i64>,
  /// Refcounted clones of every packet accepted so far — the probe-era
  /// `unconsumed_packets` history surfaced on a [`FailShape::ProbeEra`] failure.
  history: Vec<Packet>,
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
    }
  }

  /// Never fails — stays on the HW path for the whole clip, delivering 1:1.
  fn never_failing(width: u32, height: u32) -> Self {
    Self::failing(width, height, usize::MAX, usize::MAX, FailShape::PostCommit)
  }
}

impl HwInner for FakeHw {
  fn send_packet(&mut self, packet: &Packet) -> Result<(), Error> {
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
      self.queued.push_back(packet.pts().unwrap_or(0));
    }
    Ok(())
  }

  fn receive_frame(&mut self, frame: &mut Frame) -> Result<(), Error> {
    match self.queued.pop_front() {
      Some(pts) => {
        let mut av =
          frame::Video::new(ffmpeg_next::format::Pixel::YUV420P, self.width, self.height);
        av.set_pts(Some(pts));
        *frame.as_inner_mut() = av;
        Ok(())
      }
      None => Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
        errno: ffmpeg_next::error::EAGAIN,
      })),
    }
  }

  fn send_eof(&mut self) -> Result<(), Error> {
    Ok(())
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
  fn send_packet(&mut self, packet: &Packet) -> Result<(), Error> {
    self.queued.push_back(packet.pts().unwrap_or(0));
    Ok(())
  }

  fn receive_frame(&mut self, frame: &mut Frame) -> Result<(), Error> {
    match self.queued.pop_front() {
      Some(pts) => {
        let mut av =
          frame::Video::new(ffmpeg_next::format::Pixel::YUV420P, self.width, self.height);
        av.set_pts(Some(pts));
        *frame.as_inner_mut() = av;
        Ok(())
      }
      None => Err(Error::Ffmpeg(ffmpeg_next::Error::Other {
        errno: ffmpeg_next::error::EAGAIN,
      })),
    }
  }

  fn send_eof(&mut self) -> Result<(), Error> {
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

/// Drive the decoder over `clip`, draining every available frame after each
/// `send_packet` and after EOF. Returns the PTS of every delivered frame in
/// order. A `None` PTS surfaces as `i64::MIN` so a hole is visible.
fn drive(dec: &mut FfmpegVideoStreamDecoder, clip: &SyntheticClip) -> Vec<i64> {
  let mut out: Vec<i64> = Vec::new();
  let mut dst = crate::empty_video_frame();

  let mut drain_frames = |dec: &mut FfmpegVideoStreamDecoder, out: &mut Vec<i64>| {
    loop {
      match dec.receive_frame(&mut dst) {
        Ok(()) => out.push(dst.pts().map(|t| t.pts()).unwrap_or(i64::MIN)),
        Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })))
          if errno == ffmpeg_next::error::EAGAIN =>
        {
          break;
        }
        Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))) => break,
        Err(e) => panic!("receive_frame: {e:?}"),
      }
    }
  };

  for av_pkt in &clip.packets {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt).expect("packet has a buffer");
    dec.send_packet(&vpkt).expect("send_packet");
    drain_frames(dec, &mut out);
  }
  dec.send_eof().expect("send_eof");
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

  let tb = Timebase::new(1, NonZeroU32::new(25).expect("nonzero"));
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
  let mut dst = crate::empty_video_frame();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, out: &mut Vec<i64>| loop {
    match dec.receive_frame(&mut dst) {
      Ok(()) => out.push(dst.pts().map(|t| t.pts()).unwrap_or(i64::MIN)),
      Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })))
        if errno == ffmpeg_next::error::EAGAIN =>
      {
        break;
      }
      Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))) => break,
      Err(e) => panic!("receive_frame: {e:?}"),
    }
  };

  // Phase 1: feed packets [0, third_key) — the HW prefix, the post-commit
  // failure at `fail_at`, and the gap's P-frames up to (not including) the
  // resync keyframe. Even if mpeg4 conceals frames from those lone P-frames, the
  // KEYFRAME-GATED guard must stay pending and no keyframe must be recorded.
  for av_pkt in clip.packets.iter().take(third_key) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt).expect("packet has a buffer");
    dec.send_packet(&vpkt).expect("send_packet");
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt).expect("packet has a buffer");
    dec.send_packet(&vpkt).expect("send_packet");
    drain(&mut dec, &mut pts_out);
  }
  dec.send_eof().expect("send_eof");
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

  let tb = Timebase::new(1, NonZeroU32::new(25).expect("nonzero"));
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

  let tb = Timebase::new(1, NonZeroU32::new(25).expect("nonzero"));
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
  let tb = Timebase::new(1, NonZeroU32::new(25).expect("nonzero"));
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
  let vpkt = boundary::video_packet_from_ffmpeg(&raw).expect("packet has a buffer");

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
  let tb = Timebase::new(1, NonZeroU32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, fail_at, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  let mut dst = crate::empty_video_frame();
  let mut err = None;
  for av_pkt in &clip.packets {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt).expect("packet has a buffer");
    if let Err(e) = dec.send_packet(&vpkt) {
      err = Some(e);
      break;
    }
    loop {
      match dec.receive_frame(&mut dst) {
        Ok(()) => {}
        Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })))
          if errno == ffmpeg_next::error::EAGAIN =>
        {
          break;
        }
        Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))) => break,
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
  let tb = Timebase::new(1, NonZeroU32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, fail_at, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  // Send exactly the history-then-failing packets; the failing send triggers
  // the fallback whose commit-time drain must surface the deferred error.
  let mut surfaced = None;
  let mut dst = crate::empty_video_frame();
  for av_pkt in clip.packets.iter().take(fail_at + 1) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt).expect("packet has a buffer");
    match dec.send_packet(&vpkt) {
      Ok(()) => {
        // Drain anything available (none expected pre-fallback — doom = 0).
        loop {
          match dec.receive_frame(&mut dst) {
            Ok(()) => {}
            Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })))
              if errno == ffmpeg_next::error::EAGAIN =>
            {
              break;
            }
            Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))) => break,
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

  let tb = Timebase::new(1, NonZeroU32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHwEofFails::new(w, h)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");

  let mut dst = crate::empty_video_frame();
  let mut delivered = 0usize;
  let mut escalation = None;
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder,
                   delivered: &mut usize,
                   escalation: &mut Option<VideoDecodeError>| {
    loop {
      match dec.receive_frame(&mut dst) {
        Ok(()) => *delivered += 1,
        Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })))
          if errno == ffmpeg_next::error::EAGAIN =>
        {
          break;
        }
        Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))) => break,
        Err(e @ VideoDecodeError::PostCommitNeverResynced { .. }) => {
          *escalation = Some(e);
          break;
        }
        Err(e) => panic!("unexpected error draining frames: {e:?}"),
      }
    }
  };

  // HW decodes the whole stream 1:1 (no fallback yet).
  for av_pkt in &clip.packets {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt).expect("packet has a buffer");
    dec.send_packet(&vpkt).expect("send_packet");
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
  dec
    .send_eof()
    .expect("send_eof drives the fallback but itself succeeds");
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
  let VideoDecodeError::PostCommitNeverResynced { packets_lost } = esc else {
    panic!("expected PostCommitNeverResynced, got {esc:?}");
  };
  assert_eq!(
    packets_lost, 0,
    "no packets crossed to SW on the EOF-entry path; the lost tail was HW-side"
  );
  assert!(
    dec.is_software(),
    "the decoder did fall back to software (it just never resynced)"
  );
  // The flag is cleared after escalating so a follow-up poll sees plain EOF
  // (not a repeated escalation).
  assert!(
    !dec.degraded_resync_pending_for_test(),
    "the degraded-resync flag must be cleared after the escalation fires"
  );
  let mut after = crate::empty_video_frame();
  match dec.receive_frame(&mut after) {
    Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))) => {}
    other => panic!("a poll after the escalation must be plain EOF, got {other:?}"),
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

  let tb = Timebase::new(1, NonZeroU32::new(25).expect("nonzero"));
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
  let mut dst = crate::empty_video_frame();
  for av_pkt in clip.packets.iter().take(fail_at + 1) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt).expect("packet has a buffer");
    dec.send_packet(&vpkt).expect("send_packet");
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
  // `true` if a frame was delivered, `false` on EAGAIN/EOF.
  let mut try_poll = |dec: &mut FfmpegVideoStreamDecoder| -> bool {
    match dec.receive_frame(&mut dst) {
      Ok(()) => true,
      Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })))
        if errno == ffmpeg_next::error::EAGAIN =>
      {
        false
      }
      Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))) => false,
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt).expect("packet has a buffer");
    dec.send_packet(&vpkt).expect("send_packet");
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
  let key_vpkt =
    boundary::video_packet_from_ffmpeg(&clip.packets[third_key]).expect("packet has a buffer");
  dec.send_packet(&key_vpkt).expect("send_packet");
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt).expect("packet has a buffer");
    dec.send_packet(&vpkt).expect("send_packet");
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

  let tb = Timebase::new(1, NonZeroU32::new(25).expect("nonzero"));
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

  let mut dst = crate::empty_video_frame();
  let mut concealed_frames = 0usize;
  let mut escalation: Option<VideoDecodeError> = None;
  // Drain available frames; route a `PostCommitNeverResynced` to `escalation`.
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder,
                   concealed: &mut usize,
                   escalation: &mut Option<VideoDecodeError>| loop {
    match dec.receive_frame(&mut dst) {
      Ok(()) => *concealed += 1,
      Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })))
        if errno == ffmpeg_next::error::EAGAIN =>
      {
        break;
      }
      Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))) => break,
      Err(e @ VideoDecodeError::PostCommitNeverResynced { .. }) => {
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt).expect("packet has a buffer");
    dec.send_packet(&vpkt).expect("send_packet");
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
  dec.send_eof().expect("send_eof on the SW path");
  drain(&mut dec, &mut concealed_frames, &mut escalation);
  let esc = escalation.expect(
    "concealed P-frames must NOT have cleared the guard, so reaching EOF without a \
     keyframe must ESCALATE with PostCommitNeverResynced",
  );
  let VideoDecodeError::PostCommitNeverResynced { packets_lost } = esc else {
    panic!("expected PostCommitNeverResynced, got {esc:?}");
  };
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

  let tb = Timebase::new(1, NonZeroU32::new(25).expect("nonzero"));
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt).expect("packet has a buffer");
    dec.send_packet(&vpkt).expect("send_packet");
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
  let mut dst = crate::empty_video_frame();
  for av_pkt in clip.packets.iter().skip(fail_at + 1) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt).expect("packet has a buffer");
    dec.send_packet(&vpkt).expect("send_packet");
    loop {
      match dec.receive_frame(&mut dst) {
        Ok(()) => {}
        Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Other { errno })))
          if errno == ffmpeg_next::error::EAGAIN =>
        {
          break;
        }
        Err(VideoDecodeError::Decode(Error::Ffmpeg(ffmpeg_next::Error::Eof))) => break,
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
  let tb = Timebase::new(1, NonZeroU32::new(25).expect("nonzero"));
  let dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(Box::new(FakeHw::inert()), params, tb)
    .expect("build test decoder");
  assert!(dec.is_hardware(), "inert seam starts on the HW path");
  assert!(!dec.is_software());
}

// ---------------------------------------------------------------------------
//  Deferred real-fixture integration test
// ---------------------------------------------------------------------------

// TODO: user provides FX3 H.264 High 4:2:2 10-bit fixture; assert HW→SW
// fallback decodes from the next keyframe after the runtime HW failure. This is
// the real-hardware counterpart to
// `post_commit_failure_degrades_and_resyncs_at_next_keyframe`: open
// `FfmpegVideoStreamDecoder` on the actual VideoToolbox path, drive the FX3
// clip, and assert (a) it transparently falls back to SW mid-stream and (b) it
// resyncs at the next keyframe and decodes the remainder (the bounded gap at
// the failure boundary is the accepted, logged loss).
#[test]
#[ignore = "requires a Sony FX3 H.264 High 4:2:2 10-bit fixture (user-provided); \
            set MEDIADECODE_FX3_SAMPLE to its path"]
fn fx3_high_422_10bit_falls_back_to_software_and_decodes_whole_stream() {
  // Intentionally empty until the fixture is available — see TODO above.
}

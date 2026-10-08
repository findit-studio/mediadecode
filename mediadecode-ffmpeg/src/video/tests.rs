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
  encode_clip(codec, width, height, frames, ff::Dictionary::new(), |enc| {
    enc.set_gop(gop);
    enc.set_max_b_frames(0);
    enc.set_bit_rate(500_000);
  })
}

/// An H.264 clip from `libx264` in **closed** GOPs: an IDR every 8
/// frames, two B-frames between references (so decode order is not
/// display order), no scene cuts — nothing after an IDR references past
/// it.
fn encode_h264_closed_gops(width: u32, height: u32, frames: usize) -> SyntheticClip {
  encode_x26x(
    "libx264",
    "x264-params",
    "keyint=8:min-keyint=8:scenecut=0:bframes=2:b-adapt=0:open-gop=0",
    width,
    height,
    frames,
  )
}

/// An H.264 clip from `libx264` in **open** GOPs: a keyframe every 8
/// frames, each after the first an I-frame that is a recovery point rather
/// than an IDR, whose leading B-frames reference the GOP before it.
fn encode_h264_open_gops(width: u32, height: u32, frames: usize) -> SyntheticClip {
  encode_x26x(
    "libx264",
    "x264-params",
    "keyint=8:min-keyint=8:scenecut=0:bframes=2:b-adapt=0:open-gop=1",
    width,
    height,
    frames,
  )
}

/// An H.264 clip from `libx264` with no B-frames: an IDR every 8 frames,
/// no scene cuts, and decode order display order — its reorder depth 0, so
/// a decoder on one thread gives each picture back on the drain after its
/// packet.
fn encode_h264_without_b_frames(width: u32, height: u32, frames: usize) -> SyntheticClip {
  encode_x26x(
    "libx264",
    "x264-params",
    "keyint=8:min-keyint=8:scenecut=0:bframes=0",
    width,
    height,
    frames,
  )
}

/// An HEVC clip from `libx265` in **open** GOPs: a keyframe every 8
/// frames, each after the first a CRA whose two RASL leading pictures
/// reference the GOP before it.
fn encode_hevc_open_gops(width: u32, height: u32, frames: usize) -> SyntheticClip {
  encode_x26x(
    "libx265",
    "x265-params",
    "keyint=8:min-keyint=8:scenecut=0:bframes=2:b-adapt=0:open-gop=1:log-level=error",
    width,
    height,
    frames,
  )
}

/// An HEVC clip from `libx265` in **open** GOPs whose every keyframe — a CRA
/// after the first — carries its parameter sets, so a cold decoder can
/// start at one.
fn encode_hevc_cra_with_headers(width: u32, height: u32, frames: usize) -> SyntheticClip {
  encode_x26x(
    "libx265",
    "x265-params",
    "keyint=8:min-keyint=8:scenecut=0:bframes=2:b-adapt=0:open-gop=1:repeat-headers=1:log-level=error",
    width,
    height,
    frames,
  )
}

/// An HEVC clip from `libx265` in **closed** GOPs whose every keyframe — an
/// IDR picture — carries its parameter sets.
fn encode_hevc_idr_with_headers(width: u32, height: u32, frames: usize) -> SyntheticClip {
  encode_x26x(
    "libx265",
    "x265-params",
    "keyint=8:min-keyint=8:scenecut=0:bframes=2:b-adapt=0:open-gop=0:repeat-headers=1:log-level=error",
    width,
    height,
    frames,
  )
}

/// An MPEG-4 part 2 clip with B-frames: a keyframe every 6 frames and one
/// B-frame between references, so the decoder holds a picture back
/// (`has_b_frames` 1).
fn encode_mpeg4_with_b_frames(width: u32, height: u32, frames: usize) -> SyntheticClip {
  use ffmpeg_next as ff;
  ff::init().expect("ffmpeg init");
  let codec = ff::codec::encoder::find(ff::codec::Id::MPEG4).expect("mpeg4 encoder present");
  encode_clip(codec, width, height, frames, ff::Dictionary::new(), |enc| {
    enc.set_gop(6);
    enc.set_max_b_frames(1);
    enc.set_bit_rate(500_000);
  })
}

/// An MPEG-2 video clip in closed GOPs: a keyframe every `gop` frames, its
/// GOP header saying `closed_gop`, no B pictures — a stream whose every
/// keyframe its bitstream proves a clean random access point — its sequence
/// header in the codec parameters' extradata.
fn encode_mpeg2_closed_gops(width: u32, height: u32, frames: usize, gop: u32) -> SyntheticClip {
  use ffmpeg_next as ff;
  ff::init().expect("ffmpeg init");
  let codec = ff::codec::encoder::find(ff::codec::Id::MPEG2VIDEO).expect("mpeg2video encoder");
  // FFmpeg takes closed GOPs only with scene-change detection off.
  let mut options = ff::Dictionary::new();
  options.set("sc_threshold", "1000000000");
  encode_clip(codec, width, height, frames, options, |enc| {
    enc.set_gop(gop);
    enc.set_max_b_frames(0);
    enc.set_bit_rate(500_000);
    enc.set_frame_rate(Some(ff::Rational::new(25, 1)));
    // The sequence header in the codec parameters too, so a decoder opened
    // cold mid-GOP takes the pictures it is handed.
    enc.set_flags(ff::codec::Flags::CLOSED_GOP | ff::codec::Flags::GLOBAL_HEADER);
  })
}

/// A clip from the named external encoder, under its own parameter string.
fn encode_x26x(
  encoder: &str,
  params_key: &str,
  params: &str,
  width: u32,
  height: u32,
  frames: usize,
) -> SyntheticClip {
  use ffmpeg_next as ff;
  ff::init().expect("ffmpeg init");
  let codec = ff::codec::encoder::find_by_name(encoder)
    .unwrap_or_else(|| panic!("{encoder} is linked into this FFmpeg"));
  let mut options = ff::Dictionary::new();
  options.set(params_key, params);
  encode_clip(codec, width, height, frames, options, |_| {})
}

/// Paints the moving pattern into `frames` YUV 4:2:0 pictures and
/// encodes them with `codec`, configured by `configure` and opened with
/// `options`.
fn encode_clip(
  codec: ffmpeg_next::Codec,
  width: u32,
  height: u32,
  frames: usize,
  options: ffmpeg_next::Dictionary<'_>,
  configure: impl FnOnce(&mut ffmpeg_next::codec::encoder::video::Video),
) -> SyntheticClip {
  encode_clip_typed(codec, width, height, frames, options, configure, |_| {
    ffmpeg_next::picture::Type::None
  })
}

/// [`encode_clip`], frame `i` given the picture type `kind(i)` — the
/// encoder's own choice where that is `None`.
fn encode_clip_typed(
  codec: ffmpeg_next::Codec,
  width: u32,
  height: u32,
  frames: usize,
  options: ffmpeg_next::Dictionary<'_>,
  configure: impl FnOnce(&mut ffmpeg_next::codec::encoder::video::Video),
  kind: impl Fn(usize) -> ffmpeg_next::picture::Type,
) -> SyntheticClip {
  use ffmpeg_next as ff;
  let ctx = ff::codec::context::Context::new_with_codec(codec);
  let mut enc = ctx.encoder().video().expect("video encoder context");
  enc.set_width(width);
  enc.set_height(height);
  enc.set_format(ff::format::Pixel::YUV420P);
  enc.set_time_base(ff::Rational::new(1, 25));
  configure(&mut enc);
  let mut opened = enc.open_as_with(codec, options).expect("open encoder");
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
    frame.set_kind(kind(i as usize));
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
  /// When set, raise a **probe-era** exhaustion from `send_eof`, carrying
  /// every packet accepted — the end-of-stream fallback road.
  fail_at_eof: bool,
  /// When set, raise a **probe-era** exhaustion from `receive_frame`
  /// rather than from `send_packet`.
  ///
  /// That is the decoder's *second* delivery path onto the replay
  /// queue: `fall_back_to_sw` fills it inside the `receive_frame` call
  /// and the head is converted there and then. Reaching it needs a
  /// hardware seam that fails at frame time, which nothing else here
  /// does.
  fail_at_receive: bool,
  /// A `send_packet` index the seam refuses with this FFmpeg error, the
  /// packet not taken — the hardware reporting a packet failed.
  refuse_at_send: Option<(usize, ffmpeg_next::Error)>,
  /// A second `send_packet` index at which to raise the exhaustion shape,
  /// as at `fail_at_send`. `usize::MAX` => none.
  fail_again_at_send: usize,
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
      fail_at_eof: false,
      fail_at_receive: false,
      refuse_at_send: None,
      fail_again_at_send: usize::MAX,
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
      fail_at_eof: false,
      fail_at_receive: false,
      refuse_at_send: None,
      fail_again_at_send: usize::MAX,
    }
  }

  /// Accepts every packet, then raises probe-era exhaustion at the end of
  /// the stream — the fallback `send_eof` drives.
  fn failing_at_eof(width: u32, height: u32) -> Self {
    let mut hw = Self::failing(width, height, 0, usize::MAX, FailShape::ProbeEra);
    hw.fail_at_eof = true;
    hw
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

  /// Refuses the `send_packet` at index `at` with `error`, the packet not
  /// taken.
  fn refusing(mut self, at: usize, error: ffmpeg_next::Error) -> Self {
    self.refuse_at_send = Some((at, error));
    self
  }

  /// Raises the exhaustion shape again at the `send_packet` index `at`.
  fn failing_again_at(mut self, at: usize) -> Self {
    self.fail_again_at_send = at;
    self
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
    if let Some((at, error)) = self.refuse_at_send
      && idx == at
    {
      return Err(Error::Ffmpeg(error));
    }
    if idx == self.fail_at_send || idx == self.fail_again_at_send {
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
    if self.fail_at_eof {
      // Once: the decoder falls back and never asks this seam again.
      self.fail_at_eof = false;
      return Err(Error::AllBackendsFailed(
        crate::error::AllBackendsFailed::new(Vec::new(), std::mem::take(&mut self.history)),
      ));
    }
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
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
    !dec.degraded_anchored_for_test(),
    "no keyframe has crossed the gap, so the resync must not be anchored"
  );

  // Phase 2: feed the resync keyframe and the remainder; the frame SW delivers
  // after the keyframe clears the guard.
  for av_pkt in clip.packets.iter().skip(third_key) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
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
  let vpkt = boundary::video_packet_from_ffmpeg(&raw, mediadecode::Timebase::SECONDS)
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
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
/// escalation. (Both counts are 0 here and no keyframe was seen: zero packets
/// crossed to SW — the lost tail was the HW-side frames the EOF-time failure
/// stranded. The counts grow with packets fed to SW across a gap entered
/// from the `send_packet` arm; this EOF-entry path forwards none.)
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
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
  assert_eq!(
    (
      p.packets_before_anchor(),
      p.packets_unproven(),
      p.anchor_seen()
    ),
    (0, 0, false),
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
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
    dec.packets_before_anchor_for_test(),
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
    !dec.degraded_anchored_for_test(),
    "no keyframe fed yet, so the resync must not be anchored"
  );

  // Feed remaining packets up to (not including) the resync keyframe: still all
  // P-frames, so concealed frames may land but the guard must stay pending.
  // Drain fully each time so the keyframe send below never hits SW backpressure.
  for av_pkt in clip.packets[(fail_at + 1)..third_key].iter() {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    while try_poll(&mut dec) {}
    assert!(
      dec.degraded_resync_pending_for_test() && !dec.degraded_anchored_for_test(),
      "concealed P-frame frames before the keyframe must not clear the guard or \
       set the keyframe anchor"
    );
  }

  // Feed the resync keyframe. Sending it anchors the resync at once (the
  // decoder drained and reset, then fed the keyframe) — observe that BEFORE
  // draining, since the resync frame's delivery clears the whole degraded
  // state. The guard is still pending here: the resync is anchored, but no
  // picture has come out after the keyframe yet.
  assert!(third_key < clip.packets.len(), "clip has a third keyframe");
  let key_vpkt =
    boundary::video_packet_from_ffmpeg(&clip.packets[third_key], mediadecode::Timebase::SECONDS)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
  crate::accepted(dec.send_packet(&key_vpkt), "send_packet");
  assert!(
    dec.degraded_anchored_for_test(),
    "feeding a clean keyframe across the gap must anchor the resync"
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
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
    dec.packets_before_anchor_for_test(),
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
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
        !dec.degraded_anchored_for_test(),
        "no keyframe was fed, so the resync must stay unanchored"
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
  assert!(
    p.packets_before_anchor() >= 1 && p.packets_unproven() == 0 && !p.anchor_seen(),
    "every forwarded gap packet (current P-frame + the GOP-2 tail) must be \
     tallied before a keyframe that never came; got {p}"
  );
  assert!(
    !dec.degraded_resync_pending_for_test(),
    "the guard is cleared after the escalation fires"
  );
}

// ---------------------------------------------------------------------------
//  Post-commit retains ZERO replay frames (finding 1 dissolution)
// ---------------------------------------------------------------------------

/// **Finding-1 dissolution.** The post-commit fallback retains and
/// reconstructs no replay frames at all — it opens SW cold and forwards only
/// the current packet (or EOF). So the drained-replay-frame queue
/// (`sw_replay_frames`), whose later per-frame *conversion* finding 1 was
/// about, is not populated by the fallback. We assert the queue is empty right
/// after a post-commit fallback fires, and empty after every full drain as the
/// stream is driven: the one-thread decoder's tail the resync's anchor drains
/// into it (R4) is delivered — peeked, then popped once a carrier exists —
/// before anything decoded after the keyframe.
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
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
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

  // Drive the rest of the stream; after every full drain the replay queue is
  // empty — whatever the anchor's drain queued was delivered first.
  let mut dst = crate::empty_owned_video_frame();
  for av_pkt in clip.packets.iter().skip(fail_at + 1) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
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
      "a full drain leaves nothing in the replay queue"
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
    let Some(vpkt) = boundary::video_packet_from_ffmpeg(&packet, mediadecode::Timebase::SECONDS)
      .expect("a wrappable payload")
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
        obs.escalated_never_resynced = Some(p.to_string());
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
    "the cold SW decoder never resynced at a keyframe before EOF ({:?}) — the whole tail was \
     dropped; Codex R7's finding 2 (HW swallowed the keyframe / cold SW never saw it) reproduces \
     on real H.264",
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
  /// The loss, stated, if the fallback escalated `PostCommitNeverResynced`.
  escalated_never_resynced: Option<String>,
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
          eprintln!("  -> PostCommitNeverResynced at EOF: {p}");
          self.escalated_never_resynced = Some(p.to_string());
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
  // on the one-thread decoder that proves the forward, capture the
  // state, forward, route the error.
  let limits = limits.with_threads(crate::Threads::Single);
  let mut sw = super::open_sw_decoder(&clip.parameters, limits, None).expect("open sw");
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
  let mut sw = super::open_sw_decoder(&clip.parameters, limits, None).expect("open sw");
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
    .with_frame(FrameLimits::new().with_max_frame_bytes(crate::DEFAULT_MAX_FRAME_BYTES))
    .with_threads(crate::Threads::Single);
  let mut sw = super::open_sw_decoder(&clip.parameters, generous, None).expect("open sw");
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
        // On one thread: the queue holds the whole history only when the
        // committed decoder decodes each packet as it is fed, and a
        // frame-threaded one keeps a packet per thread in flight. The
        // threaded fallback is pinned by
        // `a_probe_era_fallback_continues_on_the_session_threads`.
        let mut dec = CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(
          Box::new(FakeHw::failing_at_receive(w, h)),
          clip.parameters.clone(),
          tb,
        )
        .expect("build test decoder")
        .with_threads_for_test(crate::Threads::Single);

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
  // On one thread: the allocation ceiling below has to land on the
  // carrier, and a frame-threaded receive allocates inside libavcodec
  // before the carrier is reached.
  let mut dec =
    CarrierVideoStreamDecoder::<View>::from_hw_inner_for_test(seam, clip.parameters.clone(), tb)
      .expect("build test decoder")
      .with_threads_for_test(crate::Threads::Single);

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
        dec.degraded_anchored_for_test(),
        "the keyframe crossed the gap and anchors the resync",
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

// ---------------------------------------------------------------------------
//  The decode-path pin (issue #50)
// ---------------------------------------------------------------------------

/// **The software door, open at last.**
///
/// Before `open_as` the only way to reach the software decoder as a
/// session was to pick a codec no hardware backend carries — which
/// changes the stream, and therefore compares nothing. This opens it on
/// the stream the caller actually has, and the session decodes.
#[test]
fn the_software_path_opens_without_probing_anything() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 12, 6);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  let mut dec = FfmpegVideoStreamDecoder::open_as(
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
    DecodePath::Software,
  )
  .expect("the software path opens for a stream libavcodec can decode");

  assert!(dec.is_software(), "the pin put the session on software");
  assert!(!dec.is_hardware());
  assert!(
    dec.hardware_inner().is_none(),
    "there is no hardware decoder in this session to borrow",
  );

  // And it really decodes — a door that opened onto nothing would pass
  // every assertion above.
  let mut dst = crate::empty_owned_video_frame();
  let mut delivered = 0usize;
  for av_pkt in &clip.packets {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    while let Ok(Received::Frame) = dec.receive_frame(&mut dst) {
      delivered += 1;
    }
  }
  crate::accepted(dec.send_eof(), "send_eof");
  while let Ok(Received::Frame) = dec.receive_frame(&mut dst) {
    delivered += 1;
  }
  assert_eq!(
    delivered,
    clip.packets.len(),
    "the pinned software session decoded every packet of the clip",
  );
}

/// `Auto` is the old constructor, unmoved — the promise `open_as` is
/// built on. Both sessions are asked the same questions and answer the
/// same way, on whatever path this machine's probe lands them.
#[test]
fn the_auto_path_is_the_bare_constructor() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  let bare = FfmpegVideoStreamDecoder::open(clip.parameters.clone(), tb, DecoderLimits::default())
    .expect(
      "the bare constructor opens: it falls back to software when no backend takes the stream",
    );
  let named = FfmpegVideoStreamDecoder::open_as(
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
    DecodePath::Auto,
  )
  .expect("the Auto arm opens wherever the bare constructor does");

  assert_eq!(bare.is_software(), named.is_software());
  assert_eq!(bare.is_hardware(), named.is_hardware());
}

/// A backend this platform does not probe, **named anyway**: the pin
/// fails the open rather than quietly handing back a software session.
///
/// That is the whole difference between a preference and a pin at open
/// time, and it is the failure a caller most needs to see — a
/// determinism run that silently got software twice would report that
/// the two paths agree.
#[test]
fn a_hardware_pin_on_an_absent_backend_fails_instead_of_falling_back() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  // Whatever this platform's probe order is, at least one of the four
  // backends is not in it — a single-backend platform leaves three, and
  // a platform with no order at all leaves four.
  let order = crate::backend::probe_order();
  let absent = [
    Backend::VideoToolbox,
    Backend::Vaapi,
    Backend::Cuda,
    Backend::D3d11va,
  ]
  .into_iter()
  .find(|backend| !order.contains(backend))
  .expect("no platform probes all four backends");

  let refused = FfmpegVideoStreamDecoder::open_as(
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
    DecodePath::Hardware(absent),
  );

  assert!(
    refused.is_err(),
    "a pin to {absent:?} must fail rather than open the software decoder the Auto arm \
     would have reached for",
  );
}

/// **The pin survives the mid-stream failure**, on the send road.
///
/// A hardware backend that opens and then cannot decode raises exactly
/// the exhaustion `Auto` reads as its cue to degrade. Under a pin that
/// cue is reported instead — the session stays on hardware, and the
/// caller learns the backend failed rather than silently receiving
/// software pixels for the rest of the stream.
#[test]
fn a_hardware_pin_reports_a_mid_stream_exhaustion_instead_of_degrading() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 12, 6);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  for shape in [FailShape::PostCommit, FailShape::ProbeEra] {
    let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
      Box::new(FakeHw::failing(w, h, 1, 1, shape)),
      clip.parameters.clone(),
      tb,
      DecodePath::Hardware(Backend::VideoToolbox),
    )
    .expect("build a pinned test decoder");

    let mut dst = crate::empty_owned_video_frame();
    let mut refusal = None;
    for av_pkt in &clip.packets {
      let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
        .expect("a wrappable payload")
        .expect("packet has a buffer");
      match dec.send_packet(&vpkt) {
        Ok(_) => while let Ok(Received::Frame) = dec.receive_frame(&mut dst) {},
        Err(e) => {
          refusal = Some(e);
          break;
        }
      }
    }

    let refusal = refusal.expect("the seam fails on the second packet, so a refusal must arrive");
    assert!(
      matches!(
        &refusal,
        VideoDecodeError::Decode(Error::AllBackendsFailed(_)),
      ),
      "the pinned session must report the exhaustion with its payload intact, got {refusal:?}",
    );
    assert!(
      dec.is_hardware(),
      "the pin holds after the refusal: nothing opened a software decoder behind it",
    );
    assert!(!dec.is_software());
  }
}

/// The same promise on the **receive** road, where the exhaustion
/// arrives with no packet in hand.
#[test]
fn a_hardware_pin_reports_a_frame_time_exhaustion_too() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
    Box::new(FakeHw::failing_at_receive(w, h)),
    clip.parameters.clone(),
    tb,
    DecodePath::Hardware(Backend::VideoToolbox),
  )
  .expect("build a pinned test decoder");

  let vpkt = boundary::video_packet_from_ffmpeg(&clip.packets[0], mediadecode::Timebase::SECONDS)
    .expect("a wrappable payload")
    .expect("packet has a buffer");
  crate::accepted(dec.send_packet(&vpkt), "send_packet");

  let mut dst = crate::empty_owned_video_frame();
  let refusal = dec
    .receive_frame(&mut dst)
    .expect_err("the seam fails the first time a frame is asked for");
  assert!(
    matches!(
      &refusal,
      VideoDecodeError::Decode(Error::AllBackendsFailed(_)),
    ),
    "got {refusal:?}",
  );
  assert!(dec.is_hardware(), "the pin holds on the receive road too");
}

/// And on the **EOF** road, the third and last place a hardware
/// exhaustion can reach the wrapper.
///
/// Covering all three is the point: the pin is one promise, and a road
/// that forgot to ask would break it in a way no caller could see.
#[test]
fn a_hardware_pin_reports_an_exhaustion_raised_at_eof() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 8, 4);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
    Box::new(FakeHwEofFails::new(w, h)),
    clip.parameters.clone(),
    tb,
    DecodePath::Hardware(Backend::VideoToolbox),
  )
  .expect("build a pinned test decoder");

  let vpkt = boundary::video_packet_from_ffmpeg(&clip.packets[0], mediadecode::Timebase::SECONDS)
    .expect("a wrappable payload")
    .expect("packet has a buffer");
  crate::accepted(dec.send_packet(&vpkt), "send_packet");

  let refusal = dec
    .send_eof()
    .expect_err("this seam raises its exhaustion from send_eof");
  assert!(
    matches!(
      &refusal,
      VideoDecodeError::Decode(Error::AllBackendsFailed(_)),
    ),
    "got {refusal:?}",
  );
  assert!(dec.is_hardware(), "the pin holds on the EOF road too");
}

/// **`Auto` still degrades**, checked beside the pin rather than
/// assumed: the guard added for the pin must not have quietened the
/// arm it was written around.
///
/// The same seam, the same failure packet and the same send road as the
/// pinned lane above — only the [`DecodePath`] differs, which is what
/// makes this a control rather than a second scenario. Probe-era,
/// because that road replays the keyframe history into the cold
/// software decoder and so commits on a two-packet prefix; the
/// post-commit road's own degrade is pinned by
/// `post_commit_failure_degrades_and_resyncs_at_next_keyframe`, which
/// gives it the mid-GOP failure point a cold decoder can accept.
#[test]
fn the_auto_path_still_degrades_where_a_pin_would_not() {
  let (w, h) = (64u32, 48u32);
  let clip = encode_synthetic_clip(w, h, 12, 6);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));

  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test_as(
    Box::new(FakeHw::failing(w, h, 1, 1, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
    DecodePath::Auto,
  )
  .expect("build an auto test decoder");

  let mut dst = crate::empty_owned_video_frame();
  for av_pkt in clip.packets.iter().take(2) {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    while let Ok(Received::Frame) = dec.receive_frame(&mut dst) {}
  }

  assert!(
    dec.is_software(),
    "the same seam, the same failure, and the Auto arm degrades — which is what makes the \
     pin's refusal a choice rather than a breakage",
  );
}

// ---------------------------------------------------------------------------
//  Decoder threads (mediagraph#537)
// ---------------------------------------------------------------------------

/// Decodes every packet of `clip` on the software road under `limits`,
/// draining after each send and at EOF, and answers each picture's
/// timestamp and plane bytes in output order, with the thread count the
/// session settled on.
#[allow(clippy::type_complexity)]
fn decode_on_software(
  clip: &SyntheticClip,
  limits: DecoderLimits,
) -> (
  Vec<(Option<mediadecode::Timestamp>, Vec<Vec<u8>>)>,
  Option<core::num::NonZeroU32>,
) {
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec =
    FfmpegVideoStreamDecoder::open_as(clip.parameters.clone(), tb, limits, DecodePath::Software)
      .expect("the software road opens");
  let threads = dec.active_threads();
  let mut dst = crate::empty_owned_video_frame();
  let mut pictures = Vec::new();
  let mut keep = |dst: &crate::OwnedVideoFrame| {
    let planes = dst
      .planes()
      .iter()
      .map(|plane| plane.data_ref().as_ref().to_vec())
      .collect();
    pictures.push((dst.pts(), planes));
  };
  for av_pkt in &clip.packets {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      keep(&dst);
    }
  }
  crate::accepted(dec.send_eof(), "send_eof");
  while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
    keep(&dst);
  }
  (pictures, threads)
}

/// **The request reaches the context.** Each arm writes its count into
/// `thread_count` and names both kinds in `thread_type`, on a context
/// built the way every software session's is and not yet opened — the
/// only moment libavcodec reads either field.
#[test]
fn each_threads_arm_writes_its_count_and_both_kinds_before_the_open() {
  let clip = encode_synthetic_clip(64, 48, 8, 4);
  let both = ffmpeg_next::ffi::FF_THREAD_FRAME | ffmpeg_next::ffi::FF_THREAD_SLICE;
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let huge = core::num::NonZeroU32::new(u32::MAX).expect("nonzero");
  for (threads, count) in [
    (crate::Threads::Auto, 0),
    (crate::Threads::Count(three), 3),
    (crate::Threads::Count(huge), core::ffi::c_int::MAX),
    (crate::Threads::Single, 1),
  ] {
    let (mut ctx, _state) =
      build_codec_context(&clip.parameters, DecoderLimits::default(), None).expect("a context");
    crate::decoder::request_threads(&mut ctx, threads);
    // SAFETY: `ctx` is the live context just built; both fields are
    // plain integers.
    let (written_count, written_type) = unsafe {
      let raw = ctx.as_ptr();
      ((*raw).thread_count, (*raw).thread_type)
    };
    assert_eq!(
      written_count, count,
      "{threads:?} writes thread_count {count}"
    );
    assert_eq!(
      written_type, both,
      "{threads:?} names frame and slice threading"
    );
  }
}

/// **Auto is the default, and it is not one thread.** The limits every
/// session takes by default carry `Auto`, and a software session opened
/// on them reports the count libavcodec resolved — more than one on a
/// multi-core host, for MPEG-4 part 2, which libavcodec frame-threads.
/// The explicit arms read back as asked.
#[test]
fn a_software_session_reports_the_threads_libavcodec_settled_on() {
  let clip = encode_synthetic_clip(64, 48, 8, 4);
  assert_eq!(DecoderLimits::default().threads(), crate::Threads::Auto);

  let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
  let (_, auto) = decode_on_software(&clip, DecoderLimits::default());
  let auto = auto.expect("mpeg4 runs on libavcodec's own threads").get();
  if cores > 1 {
    assert!(
      auto > 1,
      "Auto on a {cores}-core host resolved to {auto} thread(s) for mpeg4"
    );
  }
  assert!(
    auto <= 16,
    "Auto resolved past libavcodec's ceiling: {auto}"
  );

  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let (_, counted) = decode_on_software(
    &clip,
    DecoderLimits::default().with_threads(crate::Threads::Count(three)),
  );
  assert_eq!(
    counted,
    Some(three),
    "Count(3) frame-threads mpeg4 on three"
  );

  let (_, single) = decode_on_software(
    &clip,
    DecoderLimits::default().with_threads(crate::Threads::Single),
  );
  assert_eq!(single, Some(core::num::NonZeroU32::MIN), "Single is one");
}

/// **Frame threading changes when a picture comes out, never its
/// bytes.** The same MPEG-4 part 2 clip, with P-frames across several
/// GOPs, decodes to the same pictures — the same count, the same
/// timestamps in the same order, byte-identical planes — under `Auto`,
/// `Count(3)` and `Single`.
#[test]
fn frame_threads_decode_the_same_pictures_as_one_thread() {
  let clip = encode_synthetic_clip(96, 64, 40, 6);
  let (single, _) = decode_on_software(
    &clip,
    DecoderLimits::default().with_threads(crate::Threads::Single),
  );
  assert_eq!(single.len(), clip.packets.len(), "one picture per packet");

  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  for limits in [
    DecoderLimits::default(),
    DecoderLimits::default().with_threads(crate::Threads::Count(three)),
  ] {
    let (threaded, _) = decode_on_software(&clip, limits);
    assert_eq!(
      threaded.len(),
      single.len(),
      "{:?}: picture count",
      limits.threads()
    );
    for (index, (threaded, single)) in threaded.iter().zip(&single).enumerate() {
      assert_eq!(
        threaded.0,
        single.0,
        "{:?}: picture {index}'s timestamp",
        limits.threads()
      );
      assert!(
        threaded.1 == single.1,
        "{:?}: picture {index}'s planes differ from the one-thread decode",
        limits.threads()
      );
    }
  }
}

/// One picture a fallback session delivered: its timestamp, and its
/// planes when the software decoder produced it (a [`FakeHw`] picture's
/// planes are whatever its allocation held, so they are not compared).
type Delivered = (Option<mediadecode::Timestamp>, Option<Vec<Vec<u8>>>);

/// Drives every packet of `clip` through a session over `seam` on
/// `threads`, draining after each send and at EOF, and answers what it
/// delivered and the threads it settled on at the end.
fn decode_through_a_fallback(
  clip: &SyntheticClip,
  seam: FakeHw,
  threads: crate::Threads,
) -> (Vec<Delivered>, Option<core::num::NonZeroU32>) {
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec =
    FfmpegVideoStreamDecoder::from_hw_inner_for_test(Box::new(seam), clip.parameters.clone(), tb)
      .expect("build test decoder")
      .with_threads_for_test(threads);
  let mut dst = crate::empty_owned_video_frame();
  let mut delivered = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, delivered: &mut Vec<Delivered>| {
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      let planes = dec.is_software().then(|| {
        dst
          .planes()
          .iter()
          .map(|plane| plane.data_ref().as_ref().to_vec())
          .collect()
      });
      delivered.push((dst.pts(), planes));
    }
  };
  for av_pkt in &clip.packets {
    let vpkt = boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
      .expect("a wrappable payload")
      .expect("packet has a buffer");
    crate::accepted(dec.send_packet(&vpkt), "send_packet");
    drain(&mut dec, &mut delivered);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  drain(&mut dec, &mut delivered);
  assert!(
    dec.is_software(),
    "the seam failed, so the session ends on software"
  );
  (delivered, dec.active_threads())
}

/// **The probe-era fallback returns to the session's threads at the next
/// keyframe and loses nothing.** A seam that buffers five packets and
/// then exhausts hands them back as history; the fallback replays them
/// into a one-thread decoder and commits it, and at the next keyframe the
/// session drains it and goes on on its own threads — delivering the same
/// pictures, every one of them, as the same fallback on one thread.
#[test]
fn a_probe_era_fallback_returns_to_the_session_threads_at_the_next_keyframe() {
  let (w, h) = (96u32, 64u32);
  let clip = encode_mpeg2_closed_gops(w, h, 40, 6);
  let seam = || FakeHw::failing(w, h, 0, 5, FailShape::ProbeEra);

  let (single, single_threads) = decode_through_a_fallback(&clip, seam(), crate::Threads::Single);
  assert_eq!(single_threads, Some(core::num::NonZeroU32::MIN));
  assert_eq!(single.len(), clip.packets.len(), "a lossless replay");

  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let (threaded, threaded_threads) =
    decode_through_a_fallback(&clip, seam(), crate::Threads::Count(three));
  assert_eq!(
    threaded_threads,
    Some(three),
    "the session is back on its own threads"
  );
  assert_eq!(threaded, single, "the same pictures, in the same order");
}

/// **The post-commit degrade returns to the session's threads at the
/// keyframe after its resync.** A seam that delivers two GOPs of a
/// closed-GOP MPEG-2 stream and then fails at the third's keyframe leaves
/// the session on a cold one-thread decoder; that decoder resyncs at the
/// keyframe it was handed, and at the next — a closed GOP's, a clean random
/// access point — the session goes on on its own threads, delivering what
/// the same degrade on one thread delivers.
#[test]
fn a_post_commit_degrade_returns_to_the_session_threads_at_the_keyframe_after_its_resync() {
  let (w, h) = (96u32, 64u32);
  let clip = encode_mpeg2_closed_gops(w, h, 30, 6);
  // A keyframe: a cold MPEG-2 decoder takes no picture with nothing
  // before it to reference.
  let fail_at = nth_keyframe(&clip, 3);
  let seam = || FakeHw::failing(w, h, fail_at, fail_at, FailShape::PostCommit);

  let (single, _) = decode_through_a_fallback(&clip, seam(), crate::Threads::Single);
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let (threaded, threaded_threads) =
    decode_through_a_fallback(&clip, seam(), crate::Threads::Count(three));
  assert_eq!(threaded_threads, Some(three));
  assert!(
    single.iter().filter(|(_, planes)| planes.is_some()).count() >= 6,
    "the software decoder delivered the GOPs after the resync"
  );
  assert_eq!(threaded, single, "the same pictures, in the same order");
}

/// The video packet the push face takes for `av_pkt`.
fn pushed(av_pkt: &Packet) -> mediadecode::packet::VideoPacket<VideoPacketExtra, FfmpegBytes> {
  boundary::video_packet_from_ffmpeg(av_pkt, mediadecode::Timebase::SECONDS)
    .expect("a wrappable payload")
    .expect("packet has a buffer")
}

/// LAW (Codex R2, [high] ×2): over a whole session — a fallback, the
/// switch to the session's threads at the next keyframe, a seek, the
/// switch again — **at most one software decoder is open at any
/// instant**, and **no packet is decoded twice**: software decoders take
/// exactly the packets the session hands the software road, each once. On
/// both fallback roads.
#[test]
fn one_software_decoder_is_open_at_any_instant_and_no_packet_is_decoded_twice() {
  let (w, h) = (96u32, 64u32);
  let clip = encode_mpeg2_closed_gops(w, h, 30, 6);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  // A keyframe: a cold MPEG-2 decoder takes no picture with nothing
  // before it to reference.
  let post_commit_at = nth_keyframe(&clip, 2);

  for (road, seam, software_packets) in [
    // History 0..5, the refused current packet 5, then the rest.
    (
      "probe-era",
      FakeHw::failing(w, h, 0, 5, FailShape::ProbeEra),
      clip.packets.len(),
    ),
    // The hardware decodes up to the failure; software takes the rest.
    (
      "post-commit",
      FakeHw::failing(w, h, post_commit_at, post_commit_at, FailShape::PostCommit),
      clip.packets.len() - post_commit_at,
    ),
  ] {
    let mut dec =
      FfmpegVideoStreamDecoder::from_hw_inner_for_test(Box::new(seam), clip.parameters.clone(), tb)
        .expect("build test decoder")
        .with_threads_for_test(crate::Threads::Count(three));
    super::live_sw::reset_peak();
    super::live_sw::reset_sent();
    let mut dst = crate::empty_owned_video_frame();
    let mut drain = |dec: &mut FfmpegVideoStreamDecoder| {
      while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {}
    };
    for av_pkt in &clip.packets {
      crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
      drain(&mut dec);
    }
    crate::accepted(dec.send_eof(), "send_eof");
    drain(&mut dec);
    assert_eq!(dec.active_threads(), Some(three), "{road}: switched");
    assert_eq!(
      super::live_sw::sent(),
      software_packets,
      "{road}: every packet the software road was handed, decoded once"
    );

    // A seek, and the whole stream again from its first keyframe.
    dec.flush().expect("a flush");
    super::live_sw::reset_sent();
    for av_pkt in &clip.packets {
      crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
      drain(&mut dec);
    }
    crate::accepted(dec.send_eof(), "send_eof");
    drain(&mut dec);
    assert_eq!(
      super::live_sw::sent(),
      clip.packets.len(),
      "{road}: after the seek"
    );
    assert_eq!(
      super::live_sw::peak(),
      1,
      "{road}: a second software decoder was open beside the first"
    );
  }
}

/// The index of the first keyframe after packet `after`.
fn keyframe_after(clip: &SyntheticClip, after: usize) -> usize {
  clip
    .packets
    .iter()
    .enumerate()
    .skip(after + 1)
    .find(|(_, packet)| packet.is_key())
    .map(|(index, _)| index)
    .expect("the clip has a keyframe after the fallback")
}

/// Drives `clip` through a probe-era fallback at packet `fail_at` on
/// `threads`, answering the pictures it delivered and the threads after
/// each send.
fn threads_through_a_fallback(
  clip: &SyntheticClip,
  fail_at: usize,
  threads: crate::Threads,
) -> (
  Vec<Option<mediadecode::Timestamp>>,
  Vec<Option<core::num::NonZeroU32>>,
) {
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(16, 16, 0, fail_at, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(threads);
  let mut dst = crate::empty_owned_video_frame();
  let mut shown = Vec::new();
  let mut threads_after = Vec::new();
  for av_pkt in &clip.packets {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    threads_after.push(dec.active_threads());
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      shown.push(dst.pts());
    }
  }
  crate::accepted(dec.send_eof(), "send_eof");
  while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
    shown.push(dst.pts());
  }
  (shown, threads_after)
}

/// LAW (Codex R2, the authority's lossless rule): **a closed-GOP H.264
/// stream returns to the session's threads at its next IDR, and loses no
/// picture.** `libx264`'s IDR GOPs with B-frames: a probe-era fallback at
/// packet 3 decodes on one thread until the next IDR and on three from
/// it; every picture comes out, in presentation order, exactly as the same
/// fallback delivers them on one thread.
#[test]
fn a_closed_gop_h264_stream_switches_at_its_next_idr_and_loses_no_picture() {
  let clip = encode_h264_closed_gops(128, 96, 40);
  assert!(
    clip
      .packets
      .windows(2)
      .any(|pair| pair[1].pts() < pair[0].pts()),
    "the clip reorders: some packet displays before the one decoded ahead of it"
  );
  let idr = keyframe_after(&clip, 3);
  let three = core::num::NonZeroU32::new(3).expect("nonzero");

  let (single, _) = threads_through_a_fallback(&clip, 3, crate::Threads::Single);
  assert_eq!(single.len(), clip.packets.len(), "one picture per packet");
  let mut ordered = single.clone();
  ordered.sort();
  assert_eq!(single, ordered, "one thread delivers in presentation order");

  let (threaded, threads) = threads_through_a_fallback(&clip, 3, crate::Threads::Count(three));
  for (index, threads) in threads.iter().enumerate().skip(3) {
    let expected = if index < idr {
      core::num::NonZeroU32::MIN
    } else {
      three
    };
    assert_eq!(
      *threads,
      Some(expected),
      "after send {index} (next IDR {idr})"
    );
  }
  assert_eq!(
    threaded, single,
    "no picture lost, none moved, across the switch"
  );
}

/// LAW (Codex R2, the authority's lossless rule): **an open-GOP HEVC
/// stream never switches mid-stream, and loses no picture; a seek is
/// where it switches.** `libx265`'s CRA keyframes lead with RASL pictures
/// that reference the GOP before them, so none is a clean point: after a
/// probe-era fallback the session stays on one thread to the end and
/// delivers every picture the one-thread fallback delivers. After a seek
/// the first keyframe is a switch point — the seek discarded what led it —
/// and the stream decodes whole again on three threads.
#[test]
fn an_open_gop_hevc_stream_stays_on_one_thread_until_a_seek_and_loses_no_picture() {
  let clip = encode_hevc_open_gops(128, 96, 40);
  assert!(
    keyframe_after(&clip, 3) < clip.packets.len(),
    "the clip has keyframes after the fallback"
  );
  let three = core::num::NonZeroU32::new(3).expect("nonzero");

  let (single, _) = threads_through_a_fallback(&clip, 3, crate::Threads::Single);
  assert_eq!(single.len(), clip.packets.len(), "one picture per packet");
  let (threaded, threads) = threads_through_a_fallback(&clip, 3, crate::Threads::Count(three));
  assert_eq!(threaded.len(), single.len(), "no picture lost to a switch");
  assert_eq!(threaded, single, "none moved either");
  assert!(
    threads
      .iter()
      .skip(3)
      .all(|threads| *threads == Some(core::num::NonZeroU32::MIN)),
    "no CRA is a switch point: {threads:?}"
  );

  // The seek: the first keyframe after it switches, and the stream
  // decodes whole on three threads.
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(16, 16, 0, 3, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Count(three));
  let mut dst = crate::empty_owned_video_frame();
  for av_pkt in &clip.packets {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {}
  }
  dec.flush().expect("a seek");
  let mut decoded = 0usize;
  for (index, av_pkt) in clip.packets.iter().enumerate() {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    if index == 0 {
      assert_eq!(
        dec.active_threads(),
        Some(three),
        "the first keyframe after the seek switched"
      );
    }
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      decoded += 1;
    }
  }
  crate::accepted(dec.send_eof(), "send_eof");
  while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
    decoded += 1;
  }
  assert_eq!(
    decoded,
    clip.packets.len(),
    "the whole stream after the seek"
  );
}

/// LAW (Codex R2): **one thread from the fallback to the next keyframe,
/// the session's own from that keyframe on.** `active_threads` answers
/// the decoder serving at every send: the fallback's one-thread decoder
/// for the packets before the keyframe, and the session's threads from
/// the keyframe's own send.
#[test]
fn the_session_threads_return_at_the_first_keyframe_after_a_fallback() {
  let (w, h) = (96u32, 64u32);
  let clip = encode_mpeg2_closed_gops(w, h, 20, 6);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let next_keyframe = nth_keyframe(&clip, 2);
  assert!(next_keyframe > 4, "the seam fails inside the first GOP");
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, 3, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Count(three));
  let mut dst = crate::empty_owned_video_frame();
  for (index, av_pkt) in clip.packets.iter().enumerate() {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {}
    let expected = match index {
      0..3 => None,
      _ if index < next_keyframe => Some(core::num::NonZeroU32::MIN),
      _ => Some(three),
    };
    if let Some(expected) = expected {
      assert_eq!(
        dec.active_threads(),
        Some(expected),
        "after send {index} (next keyframe {next_keyframe})"
      );
    }
  }
}

/// LAW (Codex R1 [medium], under R2's rule): a fallback committed at the
/// end of the stream keeps its one-thread decoder — nothing is left to
/// thread — and so does the flush after it; the first keyframe after the
/// seek returns the session to its threads, on both fallback roads, and
/// the decoder there decodes the stream again.
#[test]
fn a_fallback_at_the_end_returns_to_the_session_threads_at_the_first_keyframe_after_a_seek() {
  let (w, h) = (96u32, 64u32);
  let clip = encode_synthetic_clip(w, h, 12, 6);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let three = core::num::NonZeroU32::new(3).expect("nonzero");

  // The probe-era road: every packet buffered, the end accepted, and
  // the exhaustion raised at the first receive — `eof_pending` holds.
  let probe_era: Box<dyn HwInner> = Box::new(FakeHw::failing_at_receive(w, h));
  // The post-commit road: the hardware decodes every packet and fails at
  // the end of the stream.
  let post_commit: Box<dyn HwInner> = Box::new(FakeHwEofFails::new(w, h));
  for (road, seam, drain_as_fed) in [
    ("probe-era", probe_era, false),
    ("post-commit", post_commit, true),
  ] {
    let mut dec =
      FfmpegVideoStreamDecoder::from_hw_inner_for_test(seam, clip.parameters.clone(), tb)
        .expect("build test decoder")
        .with_threads_for_test(crate::Threads::Count(three));
    let mut dst = crate::empty_owned_video_frame();
    for av_pkt in &clip.packets {
      crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
      if drain_as_fed {
        while let Ok(Received::Frame) = dec.receive_frame(&mut dst) {}
      }
    }
    let _ = dec.send_eof();
    while let Ok(Received::Frame) = dec.receive_frame(&mut dst) {}
    assert!(dec.is_software(), "{road}: the session fell back");
    assert_eq!(
      dec.active_threads(),
      Some(core::num::NonZeroU32::MIN),
      "{road}: committed at the end, on one thread"
    );

    dec.flush().expect("a flush");
    assert_eq!(
      dec.active_threads(),
      Some(core::num::NonZeroU32::MIN),
      "{road}: the flush keeps the decoder; the keyframe after it switches"
    );
    let mut decoded = 0usize;
    for (index, av_pkt) in clip.packets.iter().enumerate() {
      crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
      if index == 0 {
        assert_eq!(
          dec.active_threads(),
          Some(three),
          "{road}: the first keyframe after the seek returned the session to its threads"
        );
      }
      while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
        decoded += 1;
      }
    }
    crate::accepted(dec.send_eof(), "send_eof");
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      decoded += 1;
    }
    assert_eq!(
      decoded,
      clip.packets.len(),
      "{road}: the decoder on the session's threads decodes"
    );
  }
}

/// Drains every frame the session has ready, answering how many came out.
fn drain_ready(dec: &mut FfmpegVideoStreamDecoder) -> usize {
  let mut dst = crate::empty_owned_video_frame();
  let mut frames = 0;
  while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
    frames += 1;
  }
  frames
}

/// LAW (Codex R3, [high]): **a keyframe-only stream whose threaded decoder
/// will not open keeps its queue bounded.** Every packet is a clean
/// keyframe and every open on the session's threads fails. The threaded
/// open is attempted once, the session stays on one thread for good, and
/// a caller that sends without draining is answered `MustDrain` rather
/// than having switch after switch pile drained pictures into the queue —
/// which never holds more than the fallback's own replay. Nothing is lost.
#[test]
fn a_keyframe_only_stream_whose_threads_will_not_open_keeps_its_queue_bounded() {
  let (w, h) = (96u32, 64u32);
  let clip = encode_mpeg2_closed_gops(w, h, 40, 1);
  assert!(
    clip.packets.iter().all(Packet::is_key),
    "every packet a keyframe"
  );
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, 3, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Count(three))
  .failing_threaded_opens_for_test();

  let mut delivered = 0usize;
  let mut most_queued = 0usize;
  for av_pkt in &clip.packets {
    // A caller that sends without draining until it is told to.
    loop {
      match dec.send_packet(&pushed(av_pkt)).expect("send_packet") {
        Sent::Accepted => break,
        Sent::MustDrain => delivered += drain_ready(&mut dec),
      }
    }
    most_queued = most_queued.max(dec.sw_replay_len_for_test());
  }
  crate::accepted(dec.send_eof(), "send_eof");
  delivered += drain_ready(&mut dec);

  assert!(
    most_queued <= 3,
    "the queue held {most_queued} pictures; the replay's three are its most"
  );
  assert_eq!(
    dec.threaded_opens_for_test(),
    1,
    "the session's threads are attempted once"
  );
  assert_eq!(
    dec.active_threads(),
    Some(core::num::NonZeroU32::MIN),
    "and the session stays on one thread for good"
  );
  assert_eq!(delivered, clip.packets.len(), "no picture lost");
}

/// The bytes one decoded picture of `clip` holds in the replay queue: its
/// first packet decoded alone, on one thread.
fn picture_bytes(clip: &SyntheticClip) -> usize {
  let mut sw = super::open_sw_decoder(
    &clip.parameters,
    crate::DecoderLimits::default().with_threads(crate::Threads::Single),
    None,
  )
  .expect("a software decoder");
  sw.submit(&clip.packets[0]).expect("the first packet");
  sw.send_eof().expect("the end of the stream");
  let mut frame = alloc_av_video_frame().expect("a frame");
  sw.receive_frame(&mut frame).expect("a picture");
  super::footprint(&frame).expect("a picture the budget prices")
}

/// A probe-era fallback replaying nine packets of 4K pictures into a queue
/// whose byte budget is `budget(picture)`, a caller sending each packet
/// until it is taken and draining whenever it is told to. Answers how many
/// rounds the replay took, the most bytes the queue held, every picture's
/// timestamp in delivery order, the budget and one picture's bytes.
fn a_4k_replay_in_rounds(
  budget: impl Fn(usize) -> usize,
) -> (usize, usize, Vec<i64>, usize, usize) {
  let (w, h) = (3840u32, 2160u32);
  let clip = encode_synthetic_clip(w, h, 12, 12);
  let picture = picture_bytes(&clip);
  let budget = budget(picture);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(w, h, 0, 9, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Single)
  .with_max_replay_bytes_for_test(budget);

  let mut dst = crate::empty_owned_video_frame();
  let mut shown: Vec<i64> = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, shown: &mut Vec<i64>| {
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      shown.push(dst.pts().map_or(i64::MIN, |t| t.pts()));
    }
  };
  let mut rounds = 0usize;
  let mut most = 0usize;
  for av_pkt in &clip.packets {
    loop {
      match dec.send_packet(&pushed(av_pkt)).expect("send_packet") {
        Sent::Accepted => break,
        Sent::MustDrain => {
          rounds += 1;
          most = most.max(dec.sw_replay_bytes_for_test());
          drain(&mut dec, &mut shown);
        }
      }
    }
    most = most.max(dec.sw_replay_bytes_for_test());
    drain(&mut dec, &mut shown);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  drain(&mut dec, &mut shown);
  assert!(dec.is_software(), "the probe-era fallback committed");
  assert_eq!(
    shown,
    (0..clip.packets.len() as i64).collect::<Vec<_>>(),
    "every picture, once, in order"
  );
  (rounds, most, shown, budget, picture)
}

/// LAW (Codex R4, [high]; R6 row 4): **a replay whose pictures pass the
/// budget is drained in rounds, and loses nothing.** A probe-era fallback
/// replays nine packets of 4K pictures into a queue whose byte budget holds
/// two and a half of them. The replay stops each time another picture would
/// pass it, answering `MustDrain` with the current packet still the
/// caller's; the caller drains and sends it again, and the replay resumes
/// where it stopped. It takes several rounds; every picture comes out once,
/// in order; and the queue never passes its budget.
#[test]
fn a_replay_past_the_budget_is_drained_in_rounds_and_loses_nothing() {
  let (rounds, most, _, budget, _) = a_4k_replay_in_rounds(|picture| picture * 5 / 2);
  assert!(
    rounds >= 2,
    "the replay was drained in several rounds, not {rounds}"
  );
  assert!(
    most <= budget,
    "the queue held {most} bytes, past its budget of {budget}"
  );
}

/// LAW (Codex R6 row 4, [high]): **the budget is a hard bound.** Under a
/// budget of one and a half pictures — Codex's two 500 MiB pictures under
/// 512 MiB, at 4K — the queue holds one picture at a time: the second
/// waits in the decoder, never received while the first would make two
/// past the budget, until the caller has taken the first. The total never
/// passes the budget, and every picture still comes out once, in order.
/// A drain that read only the queue's bytes before each receive took the
/// second as well, two pictures where the budget holds one and a half.
#[test]
fn the_replay_budget_is_a_hard_bound() {
  let (rounds, most, _, budget, picture) = a_4k_replay_in_rounds(|picture| picture * 3 / 2);
  assert!(
    most <= budget,
    "the queue held {most} bytes, past its budget of {budget}"
  );
  assert!(most >= picture, "one picture at a time");
  assert!(rounds >= 8, "a round a picture, not {rounds}");
}

/// LAW (Codex R8, [high]): **a picture that outgrows the one before it waits
/// parked, and the queue never passes its budget.** A 128x96 GOP, then a
/// 256x192 one — the resolution changes mid-replay, and the first large
/// picture is four times the size the queue takes the next one to have. The
/// budget holds a large picture and half a small one: queued behind a small
/// picture, the large one would pass it. A probe-era fallback replays the
/// whole history on one thread, to a caller that takes one picture each time
/// it is told to drain. The large picture is received and found too large
/// for the queue: it is parked, the ONE picture held past the queue, and the
/// drain stops until the caller has taken enough. The queue never
/// holds more than its budget, and every picture comes out once, in order,
/// as the same fallback delivers them under the default budget. The drain
/// that pushed whatever it received past its check queued the large picture
/// behind the small ones, past the budget.
#[test]
fn a_picture_that_outgrows_the_last_waits_parked_and_the_queue_stays_within_its_budget() {
  let small = encode_h264_closed_gops(128, 96, 8);
  let large = encode_h264_closed_gops(256, 192, 8);
  let (small_bytes, large_bytes) = (picture_bytes(&small), picture_bytes(&large));
  let shift = small.packets.len() as i64;
  let mut packets = small.packets.clone();
  for packet in &large.packets {
    let mut later = packet.clone();
    later.set_pts(packet.pts().map(|pts| pts + shift));
    later.set_dts(packet.dts().map(|dts| dts + shift));
    packets.push(later);
  }
  let clip = SyntheticClip {
    parameters: small.parameters.clone(),
    packets,
  };
  let budget = large_bytes + small_bytes / 2;
  assert!(
    2 * small_bytes <= budget && budget < small_bytes + large_bytes,
    "a small picture queued, the hint lets a large one be received that the queue cannot \
     take: {small_bytes} / {large_bytes} under {budget}"
  );
  let fail_at = clip.packets.len() - 1;
  let (single, _) = threads_through_a_fallback(&clip, fail_at, crate::Threads::Single);

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(16, 16, 0, fail_at, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Single)
  .with_max_replay_bytes_for_test(budget);
  let mut dst = crate::empty_owned_video_frame();
  let mut shown = Vec::new();
  let (mut most, mut parked) = (0usize, 0usize);
  let watch = |dec: &FfmpegVideoStreamDecoder, most: &mut usize, parked: &mut usize| {
    *most = (*most).max(dec.sw_replay_bytes_for_test());
    *parked = (*parked).max(dec.sw_replay_parked_bytes_for_test());
  };
  for av_pkt in &clip.packets {
    loop {
      match dec.send_packet(&pushed(av_pkt)).expect("send_packet") {
        Sent::Accepted => break,
        // A caller that takes one picture each time it is told to drain: the
        // queue stays near its budget, small pictures in it when the large
        // one comes.
        Sent::MustDrain => {
          watch(&dec, &mut most, &mut parked);
          if let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
            shown.push(dst.pts());
          }
          watch(&dec, &mut most, &mut parked);
        }
      }
    }
    watch(&dec, &mut most, &mut parked);
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      watch(&dec, &mut most, &mut parked);
      shown.push(dst.pts());
    }
  }
  crate::accepted(dec.send_eof(), "send_eof");
  while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
    watch(&dec, &mut most, &mut parked);
    shown.push(dst.pts());
  }

  assert!(dec.is_software(), "the probe-era fallback committed");
  assert!(
    most <= budget,
    "the queue held {most} bytes, past its budget of {budget}"
  );
  assert_eq!(
    parked, large_bytes,
    "a large picture waited parked, the one picture past the queue"
  );
  assert_eq!(shown, single, "every picture once, in order");
}

/// LAW (Codex R9, [high]): **a replay whose last packet parks a picture
/// takes no input until the decoder has given the replay's last picture.**
/// Two 128x96 pictures, then a 256x192 IDR, all without B-frames: the
/// history a probe-era fallback replays on one thread, with a byte budget of
/// one large picture and half a small one. The two small pictures queue; the
/// large one, made by the history's last packet, does not fit behind them
/// and waits parked. The send that fell back answers `MustDrain` with the
/// caller's packet untaken — the software decoder took the history's three
/// packets and nothing more — and so does the end of the stream, sent then;
/// the drain gives the two small pictures, then the parked one. Sent again,
/// the packet is taken, and every picture comes out once, in order. Every history packet fed, the replay used to
/// count as over: the caller's packet went to the decoder behind a parked
/// picture.
#[test]
fn a_replay_whose_last_packet_parks_a_picture_takes_no_input_until_it_is_out() {
  let small = encode_h264_without_b_frames(128, 96, 8);
  let large = encode_h264_without_b_frames(256, 192, 8);
  let (small_bytes, large_bytes) = (picture_bytes(&small), picture_bytes(&large));
  let mut packets: Vec<Packet> = small.packets[..2].to_vec();
  for packet in &large.packets {
    let mut later = packet.clone();
    later.set_pts(packet.pts().map(|pts| pts + 2));
    later.set_dts(packet.dts().map(|dts| dts + 2));
    packets.push(later);
  }
  let clip = SyntheticClip {
    parameters: small.parameters.clone(),
    packets,
  };
  // The history: two small pictures, then the large IDR. The hardware fails
  // at the packet after it, which the fallback hands to the software road.
  let fail_at = 3;
  let budget = large_bytes + small_bytes / 2;
  assert!(
    3 * small_bytes <= budget && 2 * small_bytes + large_bytes > budget,
    "two small pictures queue and leave room for a third, and the large one does not fit \
     behind them: {small_bytes} / {large_bytes} under {budget}"
  );

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(16, 16, 0, fail_at, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Single)
  .with_max_replay_bytes_for_test(budget);
  let mut dst = crate::empty_owned_video_frame();
  for av_pkt in &clip.packets[..fail_at] {
    crate::accepted(
      dec.send_packet(&pushed(av_pkt)),
      "the hardware takes the history",
    );
    assert!(matches!(
      dec.receive_frame(&mut dst).expect("receive_frame"),
      Received::NeedsInput
    ));
  }
  super::live_sw::reset_sent();
  let answer = dec
    .send_packet(&pushed(&clip.packets[fail_at]))
    .expect("send_packet");
  assert!(dec.is_software(), "the probe-era fallback committed");
  assert!(
    matches!(answer, Sent::MustDrain),
    "the replay's last picture is parked: the send waits for the drain"
  );
  assert_eq!(
    super::live_sw::sent(),
    fail_at,
    "the decoder took the history's packets and not the caller's"
  );
  assert_eq!(
    dec.sw_replay_parked_bytes_for_test(),
    large_bytes,
    "the large picture waits parked"
  );
  // The end of the stream is input too: it waits the same way.
  super::live_sw::reset_eofs();
  assert!(
    matches!(dec.send_eof().expect("send_eof"), Sent::MustDrain),
    "the end waits for the drain as well"
  );
  assert_eq!(super::live_sw::eofs(), 0, "no end reached the decoder");
  assert_eq!(super::live_sw::sent(), fail_at, "nor any packet");
  let pts = |dst: &crate::OwnedVideoFrame| dst.pts().map(|t| t.pts());
  let mut shown = Vec::new();
  while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
    shown.push(pts(&dst));
  }
  assert_eq!(
    shown,
    [Some(0), Some(1), Some(2)],
    "the queued pictures, then the parked one"
  );
  for av_pkt in &clip.packets[fail_at..] {
    loop {
      match dec.send_packet(&pushed(av_pkt)).expect("send_packet") {
        Sent::Accepted => break,
        Sent::MustDrain => {
          while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
            shown.push(pts(&dst));
          }
        }
      }
    }
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      shown.push(pts(&dst));
    }
  }
  crate::accepted(dec.send_eof(), "send_eof");
  while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
    shown.push(pts(&dst));
  }
  let every: Vec<Option<i64>> = (0..clip.packets.len() as i64).map(Some).collect();
  assert_eq!(shown, every, "every picture once, in order");
}

/// LAW (Codex R10, [high]; restated by Codex R11): **a picture is priced by
/// every allocation it owns, and one it cannot price is refused by name.** A
/// 16x16 picture's pixel buffers, then 100 MiB of side data attached to it:
/// the footprint grows by the side data's 100 MiB and the three allocations
/// it took — the table, the entry and the buffer, each its payload rounded
/// up to 64 bytes and the overhead an allocation costs; a metadata entry
/// (the dictionary, its entry array, its key and its value) and an
/// `opaque_ref` count the same way. With the side data's bytes owned by no
/// buffer reference, the picture is refused as `UnpricedFrame`, naming the
/// side data. Pricing the pixel buffers alone, the picture cost its 16x16
/// pixels whatever it carried — 64 such pictures fit a budget of a few
/// hundred kilobytes and held gigabytes.
#[test]
fn a_picture_is_priced_by_every_allocation_it_owns_and_refused_where_it_cannot_be() {
  use ffmpeg_next::ffi;
  const SIDE_DATA: usize = 100 << 20;
  const OVERHEAD: usize = super::ALLOCATION_OVERHEAD;
  let mut picture = frame::Video::new(ffmpeg_next::format::Pixel::YUV420P, 16, 16);
  let pixels = super::footprint(&picture).expect("a picture of pixels alone");
  assert!(pixels > 0, "its pixel buffers are priced");
  // SAFETY: `picture` is a live frame this test owns; FFmpeg allocates the
  // side data, the dictionary entry and the buffer, and frees them with it.
  let side = unsafe {
    ffi::av_frame_new_side_data(
      picture.as_mut_ptr(),
      ffi::AVFrameSideDataType::AV_FRAME_DATA_SEI_UNREGISTERED,
      SIDE_DATA,
    )
  };
  assert!(!side.is_null(), "100 MiB of side data attached");
  // The table holds one 8-byte pointer and the entry is 40 bytes: 64 each
  // once aligned. 100 MiB is a multiple of 64.
  let side_data = (64 + OVERHEAD) + (64 + OVERHEAD) + (SIDE_DATA + OVERHEAD);
  assert_eq!(
    super::footprint(&picture),
    Ok(pixels + side_data),
    "the side data counts its 100 MiB and its three allocations"
  );
  let (key, value) = (c"comment", c"a frame metadata entry");
  // SAFETY: as above.
  let set = unsafe {
    ffi::av_dict_set(
      &mut (*picture.as_mut_ptr()).metadata,
      key.as_ptr(),
      value.as_ptr(),
      0,
    )
  };
  assert_eq!(set, 0, "a metadata entry set");
  assert!(
    key.to_bytes_with_nul().len() <= 64 && value.to_bytes_with_nul().len() <= 64,
    "each string fits one aligned unit"
  );
  // The dictionary (16 bytes), its one-entry array (16), the key and the
  // value: four allocations of one aligned unit each.
  let metadata = 4 * (64 + OVERHEAD);
  // SAFETY: as above; the frame takes the reference.
  unsafe { (*picture.as_mut_ptr()).opaque_ref = ffi::av_buffer_allocz(4096) };
  assert_eq!(
    super::footprint(&picture),
    Ok(pixels + side_data + metadata + (4096 + OVERHEAD)),
    "its metadata and its opaque reference count too"
  );
  // SAFETY: the side data's buffer reference is taken off it for the
  // refusal and put back before the frame frees it.
  unsafe {
    let buf = (*side).buf;
    (*side).buf = core::ptr::null_mut();
    let refused = super::footprint(&picture);
    (*side).buf = buf;
    assert_eq!(
      refused,
      Err(crate::UnpricedFrame::new(crate::UnpricedHolding::SideData)),
      "side data no buffer reference owns is refused by name"
    );
  }
}

/// A 16x16 picture carrying `entries` side data entries of `payload` bytes
/// each, attached the way a decoder attaches an unregistered SEI message.
fn picture_with_side_data(entries: usize, payload: usize) -> frame::Video {
  use ffmpeg_next::ffi;
  let mut picture = frame::Video::new(ffmpeg_next::format::Pixel::YUV420P, 16, 16);
  for _ in 0..entries {
    // SAFETY: `picture` is a live frame this function owns; FFmpeg allocates
    // the entry and its buffer, and frees them with the frame.
    let side = unsafe {
      ffi::av_frame_new_side_data(
        picture.as_mut_ptr(),
        ffi::AVFrameSideDataType::AV_FRAME_DATA_SEI_UNREGISTERED,
        payload,
      )
    };
    assert!(!side.is_null(), "a side data entry attached");
  }
  picture
}

/// LAW (Codex R11, [high]): **every side data entry is priced at the
/// allocations it holds, however small its payload: at least 256 bytes.** A
/// 16x16 picture carrying 64 entries of 16 bytes each — what an unregistered
/// SEI message holding only its UUID makes. Each entry holds the entry
/// itself, its slot in the table, its buffer's two reference-counting
/// structs and its payload, every one an allocation of its own, aligned and
/// with its allocator's header; the footprint grows by at least 256 bytes
/// for each. Priced by payload, each cost its 16 bytes, and a picture
/// admitted under the budget could hold several times it.
#[test]
fn every_side_data_entry_is_priced_at_the_allocations_it_holds() {
  const ENTRIES: usize = 64;
  const PAYLOAD: usize = 16;
  let pixels = super::footprint(&picture_with_side_data(0, PAYLOAD)).expect("pixels alone");
  let priced = super::footprint(&picture_with_side_data(ENTRIES, PAYLOAD)).expect("priced");
  assert!(
    priced - pixels >= ENTRIES * 256,
    "{ENTRIES} entries of {PAYLOAD} bytes priced at {} bytes, under {} — 256 apiece",
    priced - pixels,
    ENTRIES * 256
  );
}

/// `packet`, an Annex B access unit, with an SEI NAL unit carrying
/// `messages` unregistered user data messages — each its 16-byte UUID and
/// nothing more — before its first picture's unit.
fn with_unregistered_seis(packet: &Packet, messages: usize) -> Packet {
  let data = packet.data().expect("a payload");
  let start = (0..data.len().saturating_sub(3))
    .find(|&at| data[at..].starts_with(&[0, 0, 1]) && (1..=5).contains(&(data[at + 3] & 0x1f)))
    .expect("a picture's unit behind a start code");
  // Before a four-byte start code's leading zero, too.
  let start = if start > 0 && data[start - 1] == 0 {
    start - 1
  } else {
    start
  };
  let mut sei = vec![0, 0, 0, 1, 0x06];
  for _ in 0..messages {
    // `payload_type` 5, user data unregistered; `payload_size` 16; a UUID
    // with no zero byte, so no emulation prevention is due.
    sei.extend_from_slice(&[0x05, 0x10]);
    sei.extend_from_slice(&[0x55; 16]);
  }
  sei.push(0x80);
  let mut bytes = data[..start].to_vec();
  bytes.extend_from_slice(&sei);
  bytes.extend_from_slice(&data[start..]);
  let mut out = Packet::copy(&bytes);
  out.set_pts(packet.pts());
  out.set_dts(packet.dts());
  out.set_flags(packet.flags());
  out
}

/// The side data entries the picture `packet` decodes to carries, decoded
/// alone on one thread.
fn side_data_entries_of(clip: &SyntheticClip, packet: &Packet) -> usize {
  let mut sw = super::open_sw_decoder(
    &clip.parameters,
    crate::DecoderLimits::default().with_threads(crate::Threads::Single),
    None,
  )
  .expect("a software decoder");
  sw.submit(packet).expect("the packet");
  sw.send_eof().expect("the end of the stream");
  let mut picture = alloc_av_video_frame().expect("a frame");
  sw.receive_frame(&mut picture).expect("a picture");
  // SAFETY: `picture` is a live frame; one plain integer field is read.
  usize::try_from(unsafe { (*picture.as_ptr()).nb_side_data }).expect("a count")
}

/// LAW (Codex R11, [high]): **a picture carrying more side data entries
/// than the queue prices is refused by name, and released.** By hand: a
/// picture with 257 entries, one past the cap, is refused by `footprint` as
/// `UnpricedFrame`, naming the count and the cap (256), before its table is
/// walked; at the cap it is priced. From the decoder: an H.264 IDR picture
/// whose access unit carries an SEI NAL unit of 257 unregistered user data
/// messages — FFmpeg's decoder makes one side data entry for each, with no
/// cap of its own — decoded on one thread and drained into a queue whose
/// budget holds the picture many times over. The drain answers the refusal,
/// naming the count, and the picture is neither queued nor parked: it is
/// released with the error. Uncapped, the queue walked and admitted every
/// entry a stream could make.
#[test]
fn a_picture_past_the_side_data_cap_is_refused_by_name_and_released() {
  const CAP: usize = 256;
  let over = |count| {
    Err(crate::UnpricedFrame::new(
      crate::UnpricedHolding::SideDataEntries { count, cap: CAP },
    ))
  };
  assert!(
    super::footprint(&picture_with_side_data(CAP, 1)).is_ok(),
    "at the cap a picture is priced"
  );
  assert_eq!(
    super::footprint(&picture_with_side_data(CAP + 1, 1)),
    over(CAP + 1),
    "one past the cap is refused by name"
  );

  let clip = encode_h264_without_b_frames(64, 64, 8);
  let idr = &clip.packets[0];
  let own = side_data_entries_of(&clip, idr);
  let flooded = with_unregistered_seis(idr, CAP + 1);
  let mut sw = super::open_sw_decoder(
    &clip.parameters,
    crate::DecoderLimits::default().with_threads(crate::Threads::Single),
    None,
  )
  .expect("a software decoder");
  sw.submit(&flooded).expect("the flooded IDR");
  sw.send_eof().expect("the end of the stream");
  let state = sw.state();
  let mut queue = super::ReplayQueue::default();
  match super::drain_into(&mut sw, state, &mut queue, crate::DEFAULT_MAX_REPLAY_BYTES) {
    Err(Error::UnpricedFrame(refused)) => assert_eq!(
      Err(refused),
      over(own + CAP + 1),
      "one side data entry per message, on top of the picture's own {own}"
    ),
    Err(other) => panic!("refused by another name: {other:?}"),
    Ok(drained) => panic!("the flooded picture was admitted: {drained:?}"),
  }
  assert!(
    queue.frames.is_empty() && queue.parked.is_none() && queue.bytes == 0,
    "the picture is neither queued nor parked"
  );
}

/// LAW (Codex R4, [high]): **a switch's drain past the budget waits for the
/// caller, and loses nothing.** A closed-GOP H.264 stream with B-frames
/// falls back on its probe at packet 3, on three threads, with a byte
/// budget of one picture. At the next IDR the one-thread decoder's tail
/// passes the budget: the send answers `MustDrain` with the IDR still the
/// caller's, and the drained decoder stays open. Drained and sent again,
/// the drain resumes, the session switches, and every picture comes out
/// once, in presentation order — exactly as the same fallback delivers them
/// on one thread.
#[test]
fn a_switch_drain_past_the_budget_waits_for_the_caller_and_loses_nothing() {
  let clip = encode_h264_closed_gops(128, 96, 40);
  let picture = picture_bytes(&clip);
  let idr = keyframe_after(&clip, 3);
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let (single, _) = threads_through_a_fallback(&clip, 3, crate::Threads::Single);

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(16, 16, 0, 3, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Count(three))
  .with_max_replay_bytes_for_test(picture);
  let mut dst = crate::empty_owned_video_frame();
  let mut shown = Vec::new();
  let mut drained_at = Vec::new();
  for (index, av_pkt) in clip.packets.iter().enumerate() {
    loop {
      match dec.send_packet(&pushed(av_pkt)).expect("send_packet") {
        Sent::Accepted => break,
        Sent::MustDrain => {
          drained_at.push(index);
          while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
            shown.push(dst.pts());
          }
        }
      }
    }
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      shown.push(dst.pts());
    }
  }
  crate::accepted(dec.send_eof(), "send_eof");
  while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
    shown.push(dst.pts());
  }

  assert!(
    drained_at.contains(&idr),
    "the switch's drain at the IDR {idr} passed the budget and waited: {drained_at:?}"
  );
  assert_eq!(dec.active_threads(), Some(three), "the session switched");
  assert_eq!(shown, single, "no picture lost, none moved");
}

/// LAW (Codex R4, [high]): **only a picture that alone exceeds the budget
/// is refused, by name.** With a byte budget of one, no picture can ever be
/// queued under it: the probe-era replay's first picture is refused as
/// `ReplayQueueFull`, naming its bytes and the budget, and the fallback
/// fails whole — `FallbackFailed` hands back every rescued packet, and the
/// session stays where it was.
#[test]
fn only_a_picture_that_alone_exceeds_the_budget_is_refused_by_name() {
  let clip = encode_synthetic_clip(96, 64, 12, 6);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(96, 64, 0, 5, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_max_replay_bytes_for_test(1);
  for av_pkt in &clip.packets[..5] {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
  }
  match dec.send_packet(&pushed(&clip.packets[5])) {
    Err(VideoDecodeError::Decode(Error::FallbackFailed(failed))) => {
      assert!(
        matches!(
          failed.source(),
          Error::ReplayQueueFull(full) if full.budget() == 1 && full.frame_bytes() > 1
        ),
        "refused by name: {:?}",
        failed.source()
      );
      assert_eq!(
        failed.unconsumed_packets().len(),
        5,
        "every rescued packet handed back"
      );
    }
    other => panic!("expected the replay refused by name, got {other:?}"),
  }
  assert!(dec.is_hardware(), "the session stayed where it was");
}

/// The post-commit road the resync laws stand on: a clip with keyframes at
/// 0, 6, 12 and 18; the hardware decodes up to packet 8 and fails there
/// post-commit (a P-frame the cold decoder takes, as
/// `post_commit_concealed_p_frame_does_not_clear_resync_escalates_at_eof`
/// found); packets 8 to 10 go to the cold software decoder drained, and 11
/// is sent and left undrained — its concealed picture still in the decoder
/// — before the keyframe at 12.
fn before_the_anchor(clip: &SyntheticClip) -> FfmpegVideoStreamDecoder {
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, 8, 8, FailShape::PostCommit)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  for av_pkt in &clip.packets[..11] {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    drain_ready(&mut dec);
  }
  assert!(dec.degraded_resync_pending_for_test(), "the gap is open");
  crate::accepted(dec.send_packet(&pushed(&clip.packets[11])), "the last P");
  dec
}

/// Drives `clip` through a post-commit failure at packet `at`, every packet
/// sent and drained — a decode error a picture the gap dropped earns
/// tolerated — answering the timestamps of the pictures the software
/// decoder delivered, whether the gap was still open after each, and
/// whether the end escalated.
fn through_a_post_commit_failure(
  clip: &SyntheticClip,
  at: usize,
) -> (FfmpegVideoStreamDecoder, Vec<(i64, bool)>, bool) {
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, at, at, FailShape::PostCommit)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  let mut dst = crate::empty_owned_video_frame();
  let mut delivered = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, delivered: &mut Vec<(i64, bool)>| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => {
        if dec.is_software() {
          let pts = dst.pts().map_or(i64::MIN, |t| t.pts());
          delivered.push((pts, dec.degraded_resync_pending_for_test()));
        }
      }
      Ok(Received::NeedsInput | Received::Ended) => break false,
      Err(VideoDecodeError::PostCommitNeverResynced(_)) => break true,
      Err(VideoDecodeError::Decode(_)) => {}
      Err(other) => panic!("unexpected: {other:?}"),
    }
  };
  for av_pkt in &clip.packets {
    loop {
      match dec.send_packet(&pushed(av_pkt)) {
        Ok(Sent::Accepted) | Err(VideoDecodeError::Decode(_)) => break,
        Ok(Sent::MustDrain) => {
          assert!(
            !drain(&mut dec, &mut delivered),
            "no escalation before the end"
          );
        }
        Err(other) => panic!("send_packet: {other:?}"),
      }
    }
    assert!(
      !drain(&mut dec, &mut delivered),
      "no escalation before the end"
    );
  }
  crate::accepted(dec.send_eof(), "send_eof");
  let escalated = drain(&mut dec, &mut delivered);
  (dec, delivered, escalated)
}

/// LAW (the authority's row 6; Codex R7; restated by Codex R9): **an H.264
/// stream of recovery points resyncs at the first one, its first picture
/// out closing the gap.** `libx264`'s open GOPs flag each keyframe after the
/// first, an I-frame that is a recovery point rather than an IDR, and carry
/// before it a recovery point SEI message whose `recovery_frame_cnt` is 0.
/// The hardware fails post-commit at one; the cold software decoder takes
/// it — the resync anchor, though no packet after the gap is a clean random
/// access point — and the recovery point's own picture, the first out, closes
/// the gap: FFmpeg outputs only pictures it has recovered. The end is clean,
/// and every picture from the recovery point on comes out once.
#[test]
fn an_h264_recovery_point_stream_resyncs_at_its_first_picture_out() {
  let clip = encode_h264_open_gops(128, 96, 40);
  let at = keyframe_after(&clip, 3);
  let rule = super::access::KeyframeRule::of(crate::CodecId::H264.raw(), &[]);
  let clean = |packet: &Packet| packet.data().is_some_and(|data| rule.is_clean(data));
  assert!(
    clip.packets[at].is_key(),
    "the packet is flagged a keyframe"
  );
  let anchor = clip.packets[at]
    .data()
    .and_then(|data| rule.anchor(data))
    .expect("its recovery point SEI anchors it");
  assert_eq!(
    anchor.recovery().map(super::access::RecoveryPoint::frames),
    Some(0),
    "recovery_frame_cnt 0"
  );
  assert!(
    !anchor.definitive(),
    "a recovery point is not a clean point"
  );
  assert!(
    !clip.packets[at..].iter().any(clean),
    "no packet from the failure on is a clean random access point"
  );
  let anchor_pts = clip.packets[at].pts().expect("a pts");

  let (dec, delivered, escalated) = through_a_post_commit_failure(&clip, at);
  assert!(dec.is_software(), "the hardware failed post-commit");
  assert!(!escalated, "the end is clean: {delivered:?}");
  assert_eq!(
    delivered.first(),
    Some(&(anchor_pts, false)),
    "the recovery point's own picture comes out first and closes the gap: {delivered:?}"
  );
  let shown: Vec<i64> = delivered.iter().map(|&(pts, _)| pts).collect();
  let mut once = shown.clone();
  once.sort_unstable();
  once.dedup();
  assert_eq!(once.len(), shown.len(), "no picture twice: {shown:?}");
  let last = clip
    .packets
    .iter()
    .filter_map(Packet::pts)
    .max()
    .expect("a pts");
  for pts in anchor_pts..=last {
    assert!(
      shown.contains(&pts),
      "picture {pts} from the recovery point on is missing: {shown:?}"
    );
  }
}

/// `clip` with the recovery point SEI message before packet `at`'s picture
/// restated to say `recovery_frame_cnt` 2. `libx264` writes 0 —
/// `ue(v)` `1`, `exact_match_flag`, `broken_link_flag`, two bits of
/// `changing_slice_group_idc`, the payload's alignment — in one payload
/// byte, and 2, `011`, fits the same byte: the message keeps its size.
fn recovering_two_frames_on(clip: &SyntheticClip, at: usize) -> SyntheticClip {
  let original = &clip.packets[at];
  let mut data = original.data().expect("a payload").to_vec();
  let payload = data
    .windows(6)
    .position(|window| window == [0, 0, 1, 0x06, 0x06, 0x01])
    .expect("an SEI unit opening with a one-byte recovery point")
    + 6;
  let old = data[payload];
  assert_eq!(old & 0x80, 0x80, "x264's recovery_frame_cnt is 0");
  data[payload] = 0x60 | (((old >> 3) & 0x0f) << 1) | 1;
  let mut restated = Packet::copy(&data);
  restated.set_pts(original.pts());
  restated.set_dts(original.dts());
  restated.set_duration(original.duration());
  restated.set_flags(original.flags());
  let mut packets = clip.packets.clone();
  packets[at] = restated;
  SyntheticClip {
    parameters: clip.parameters.clone(),
    packets,
  }
}

/// LAW (Codex R7, [high]; restated by Codex R8 and R9): **a recovery point
/// closes the gap at its first picture out, whatever its count — FFmpeg
/// withholds the pictures before its recovery itself.** The session's
/// software decoders are opened with neither `AV_CODEC_FLAG_OUTPUT_CORRUPT`
/// nor `AV_CODEC_FLAG2_SHOW_ALL`, so FFmpeg's H.264 decoder outputs no
/// picture before the recovery a recovery point signals. A 37-frame
/// `libx264` open-GOP clip, the hardware failing post-commit at its last
/// keyframe (picture 32): its recovery point says 0, and 32, out first,
/// closes the gap; restated to say 2, FFmpeg withholds 32, and 33, out
/// first, closes it. Both tails end clean. Counting the 2 again (R7) left the
/// restated tail short, and allowing the reorder depth (R8) held 32 and 33
/// — then 33 and 34 — inside a gap the first of them had closed.
#[test]
fn a_recovery_point_closes_the_gap_at_its_first_picture_out_whatever_its_count() {
  let clip = encode_h264_open_gops(128, 96, 37);
  let at = clip
    .packets
    .iter()
    .rposition(Packet::is_key)
    .expect("a keyframe");
  let rule = super::access::KeyframeRule::of(crate::CodecId::H264.raw(), &[]);
  let two = recovering_two_frames_on(&clip, at);
  assert_eq!(
    two.packets[at]
      .data()
      .and_then(|data| rule.anchor(data))
      .and_then(super::access::Anchor::recovery)
      .map(super::access::RecoveryPoint::frames),
    Some(2),
    "the restated SEI says 2"
  );
  let (_, zero, zero_escalated) = through_a_post_commit_failure(&clip, at);
  let (_, restated, restated_escalated) = through_a_post_commit_failure(&two, at);
  assert!(
    !zero_escalated && !restated_escalated,
    "both tails end clean: {zero:?} / {restated:?}"
  );
  let open = |delivered: &[(i64, bool)]| delivered.iter().take_while(|&&(_, open)| open).count();
  assert!(
    !zero.is_empty() && !restated.is_empty(),
    "both tails deliver: {zero:?} / {restated:?}"
  );
  assert_eq!(
    (open(&zero), open(&restated)),
    (0, 0),
    "the first picture out closes the gap, said 0 or said 2: {zero:?} / {restated:?}"
  );
  let recovery_point = clip.packets[at].pts().expect("a pts");
  assert_eq!(
    zero.first().map(|&(pts, _)| pts),
    Some(recovery_point),
    "said 0, the recovery point's own picture comes out"
  );
  assert!(
    restated.iter().all(|&(pts, _)| pts != recovery_point),
    "said 2, FFmpeg withholds the recovery point's picture itself: {restated:?}"
  );
}

/// LAW (Codex R8, [high]; moved to HEVC by Codex R9): **a later IDR
/// supersedes a CRA that anchored the gap.** A 16-frame `libx265` open-GOP
/// clip, then a 16-frame closed-GOP one, every keyframe carrying its
/// parameter sets; the hardware fails post-commit at the first clip's CRA,
/// which anchors the gap — and the anchor goes stale, the decoder made to
/// report a reorder depth no tail closes. The second clip's IDR is a
/// definitive anchor: it supersedes the stale one, its count restarting at
/// the depth the decoder reports there, and the gap closes by the reorder
/// bound; the end is clean. Kept anchored at the CRA, the gap never closed,
/// and the end raised `PostCommitNeverResynced`. (On H.264 the first picture
/// out closes the gap whatever anchored it: see
/// `a_definitive_anchor_superseding_a_recovery_point_costs_an_h264_stream_nothing`.)
#[test]
fn a_later_idr_supersedes_a_cra_anchor() {
  let open = encode_hevc_cra_with_headers(128, 96, 16);
  let closed = encode_hevc_idr_with_headers(128, 96, 16);
  let shift = open.packets.len() as i64;
  let mut packets = open.packets.clone();
  for packet in &closed.packets {
    let mut later = packet.clone();
    later.set_pts(packet.pts().map(|pts| pts + shift));
    later.set_dts(packet.dts().map(|dts| dts + shift));
    packets.push(later);
  }
  let idr = open.packets.len();
  let clip = SyntheticClip {
    parameters: open.parameters.clone(),
    packets,
  };
  let at = keyframe_after(&clip, 3);
  assert!(at < idr, "the CRA is the first clip's");
  let rule = super::access::KeyframeRule::of(crate::CodecId::HEVC.raw(), &[]);
  assert_eq!(rule.proof(), super::access::Proof::ReorderBound);
  let read = |index: usize| {
    clip.packets[index]
      .data()
      .and_then(|data| rule.anchor(data))
      .expect("an anchor")
  };
  assert!(!read(at).definitive(), "a CRA is not definitive");
  assert!(read(idr).definitive(), "an IDR is");

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, at, at, FailShape::PostCommit)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  // The anchor at the CRA goes stale: no tail outlasts this depth.
  dec.set_reorder_for_test(50);
  let mut dst = crate::empty_owned_video_frame();
  let mut delivered: Vec<(i64, bool)> = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, delivered: &mut Vec<(i64, bool)>| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => {
        if dec.is_software() {
          let pts = dst.pts().map_or(i64::MIN, |t| t.pts());
          delivered.push((pts, dec.degraded_resync_pending_for_test()));
        }
      }
      Ok(Received::NeedsInput | Received::Ended) => break false,
      Err(VideoDecodeError::PostCommitNeverResynced(_)) => break true,
      Err(VideoDecodeError::Decode(_)) => {}
      Err(other) => panic!("unexpected: {other:?}"),
    }
  };
  for (index, av_pkt) in clip.packets.iter().enumerate() {
    if index == idr {
      assert!(
        dec.degraded_resync_pending_for_test()
          && dec.degraded_anchored_for_test()
          && !dec.anchor_definitive_for_test(),
        "the stale anchor still holds the gap at the IDR: {delivered:?}"
      );
      // The decoder's depth, as it reports it from here on.
      dec.set_reorder_for_test(2);
    }
    loop {
      match dec.send_packet(&pushed(av_pkt)) {
        Ok(Sent::Accepted) | Err(VideoDecodeError::Decode(_)) => break,
        Ok(Sent::MustDrain) => {
          assert!(
            !drain(&mut dec, &mut delivered),
            "no escalation before the end"
          );
        }
        Err(other) => panic!("send_packet: {other:?}"),
      }
    }
    if index == idr {
      assert!(
        dec.anchor_definitive_for_test(),
        "the IDR superseded the stale anchor"
      );
    }
    assert!(
      !drain(&mut dec, &mut delivered),
      "no escalation before the end"
    );
  }
  crate::accepted(dec.send_eof(), "send_eof");
  let escalated = drain(&mut dec, &mut delivered);
  assert!(
    !escalated,
    "the IDR superseded the stale anchor: {delivered:?}"
  );
  assert!(
    delivered.iter().any(|&(pts, open)| !open && pts >= shift),
    "the gap closed after the IDR: {delivered:?}"
  );
}

/// LAW (Codex R9, [high]): **after an H.264 anchor the first picture out
/// closes the gap, however short the tail.** FFmpeg's H.264 decoder, opened
/// with neither `AV_CODEC_FLAG_OUTPUT_CORRUPT` nor `AV_CODEC_FLAG2_SHOW_ALL`,
/// outputs only pictures it has recovered. Two 33-frame `libx264` clips with
/// two B-frames between references — the decoder's reorder depth 2 — a
/// closed-GOP one whose last packet is an IDR picture, and an open-GOP one
/// whose last keyframe, a recovery point, leads one B-frame. The hardware
/// fails post-commit at that keyframe; the cold software decoder takes it
/// and what follows, and delivers one picture, the keyframe's own — FFmpeg
/// withholds the leading B-frame, which references the GOP it never saw.
/// That picture closes the gap, and the end is clean. Under the reorder
/// bound the one picture fell short of the three a depth of 2 asks, and the
/// end raised `PostCommitNeverResynced` on a resync that happened.
#[test]
fn after_an_h264_anchor_the_first_picture_out_closes_the_gap_however_short_the_tail() {
  let rule = super::access::KeyframeRule::of(crate::CodecId::H264.raw(), &[]);
  for (clip, definitive) in [
    (encode_h264_closed_gops(128, 96, 33), true),
    (encode_h264_open_gops(128, 96, 33), false),
  ] {
    let at = clip
      .packets
      .iter()
      .rposition(Packet::is_key)
      .expect("a keyframe");
    let anchor = clip.packets[at]
      .data()
      .and_then(|data| rule.anchor(data))
      .expect("the keyframe anchors");
    assert_eq!(
      anchor.definitive(),
      definitive,
      "an IDR picture, or a recovery point"
    );
    let keyframe = clip.packets[at].pts().expect("a pts");
    let (dec, delivered, escalated) = through_a_post_commit_failure(&clip, at);
    assert!(dec.is_software(), "the hardware failed post-commit");
    assert_eq!(dec.reorder_for_test(), 2, "a reorder depth of 2");
    assert_eq!(
      delivered,
      [(keyframe, false)],
      "one picture out, the keyframe's own, closing the gap"
    );
    assert!(!escalated, "the end is clean");
  }
}

/// LAW (Codex R9, [high]): **a definitive anchor superseding a recovery
/// point costs an H.264 stream nothing.** A 16-frame `libx264` open-GOP clip
/// cut after its recovery point, then a closed-GOP clip's IDR picture. The
/// hardware fails post-commit at the recovery point, which anchors the gap;
/// the decoder, at a reorder depth of 2, holds its picture back, so the gap
/// is still open when the IDR comes, and the IDR — definitive — supersedes
/// the anchor, its count restarting. Two pictures come out after it, the
/// recovery point's and the IDR's: the first closes the gap, and the end is
/// clean. Under the reorder bound the restarted count fell short of three,
/// and the end raised `PostCommitNeverResynced` on a resync that happened.
#[test]
fn a_definitive_anchor_superseding_a_recovery_point_costs_an_h264_stream_nothing() {
  let open = encode_h264_open_gops(128, 96, 16);
  let closed = encode_h264_closed_gops(128, 96, 16);
  let at = keyframe_after(&open, 3);
  let shift = open.packets.len() as i64;
  let mut packets = open.packets[..=at].to_vec();
  let mut idr = closed.packets[0].clone();
  idr.set_pts(closed.packets[0].pts().map(|pts| pts + shift));
  idr.set_dts(closed.packets[0].dts().map(|dts| dts + shift));
  packets.push(idr);
  let clip = SyntheticClip {
    parameters: open.parameters.clone(),
    packets,
  };
  let rule = super::access::KeyframeRule::of(crate::CodecId::H264.raw(), &[]);
  let read = |index: usize| {
    clip.packets[index]
      .data()
      .and_then(|data| rule.anchor(data))
      .expect("an anchor")
  };
  assert!(!read(at).definitive(), "a recovery point is not definitive");
  assert!(read(at + 1).definitive(), "an IDR is");
  let recovery_point = clip.packets[at].pts().expect("a pts");

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, at, at, FailShape::PostCommit)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  let mut dst = crate::empty_owned_video_frame();
  let mut delivered: Vec<(i64, bool)> = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, delivered: &mut Vec<(i64, bool)>| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => {
        if dec.is_software() {
          let pts = dst.pts().map_or(i64::MIN, |t| t.pts());
          delivered.push((pts, dec.degraded_resync_pending_for_test()));
        }
      }
      Ok(Received::NeedsInput | Received::Ended) => break false,
      Err(VideoDecodeError::PostCommitNeverResynced(_)) => break true,
      Err(other) => panic!("unexpected: {other:?}"),
    }
  };
  for (index, av_pkt) in clip.packets.iter().enumerate() {
    loop {
      match dec.send_packet(&pushed(av_pkt)) {
        Ok(Sent::Accepted) => break,
        Ok(Sent::MustDrain) => {
          assert!(
            !drain(&mut dec, &mut delivered),
            "no escalation before the end"
          );
        }
        Err(other) => panic!("send_packet: {other:?}"),
      }
    }
    if index == at {
      assert!(
        dec.degraded_anchored_for_test() && !dec.anchor_definitive_for_test(),
        "the recovery point anchors the gap"
      );
    }
    assert!(
      !drain(&mut dec, &mut delivered),
      "no escalation before the end"
    );
  }
  assert!(
    dec.degraded_resync_pending_for_test() && dec.anchor_definitive_for_test(),
    "the gap still open, the IDR superseded the recovery point: {delivered:?}"
  );
  crate::accepted(dec.send_eof(), "send_eof");
  let escalated = drain(&mut dec, &mut delivered);
  assert_eq!(dec.reorder_for_test(), 2, "a reorder depth of 2");
  assert_eq!(
    delivered,
    [(recovery_point, false), (shift, false)],
    "the recovery point's picture, out first, closes the gap; then the IDR's"
  );
  assert!(!escalated, "the end is clean");
}

/// LAW (Codex R10, [high]): **after a decode error across the gap, the next
/// H.264 anchor is proved by the reorder bound.** A closed-GOP `libx264`
/// clip with two B-frames between references, IDR pictures at 0, 8 and 16,
/// its parameter sets in the codec parameters so the cold decoder takes the
/// gap's pictures; the hardware fails post-commit at packet 3. The IDR at 8 is taken by the
/// decoder and reported failed, as FFmpeg reports a packet it parsed in part
/// — its recovery state set all the same — and anchors nothing; the IDR at
/// 16 anchors. Its first picture out is from before it — the reorder buffer
/// held it, and FFmpeg's decoder, recovered at 8, marks it recovered — so
/// it does not close the gap: the bound, a depth of 2, does, at the third
/// picture out, and the end is clean. With the withheld proof kept after the
/// error, that picture from before the anchor closed the gap.
#[test]
fn after_a_decode_error_across_the_gap_the_next_h264_anchor_is_proved_by_the_bound() {
  let clip = encode_h264_with_extradata(128, 96, 24);
  let idrs: Vec<usize> = clip
    .packets
    .iter()
    .enumerate()
    .filter(|(_, packet)| packet.is_key())
    .map(|(index, _)| index)
    .collect();
  assert_eq!(idrs.len(), 3, "three closed GOPs");
  let (failing, good, at) = (idrs[1], idrs[2], 3);
  let good_pts = clip.packets[good].pts().expect("a pts");

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, at, at, FailShape::PostCommit)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  let mut dst = crate::empty_owned_video_frame();
  let mut delivered: Vec<(i64, bool)> = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, delivered: &mut Vec<(i64, bool)>| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => {
        if dec.is_software() {
          let pts = dst.pts().map_or(i64::MIN, |t| t.pts());
          delivered.push((pts, dec.degraded_resync_pending_for_test()));
        }
      }
      Ok(Received::NeedsInput | Received::Ended) => break false,
      Err(VideoDecodeError::PostCommitNeverResynced(_)) => break true,
      Err(VideoDecodeError::Decode(_)) => {}
      Err(other) => panic!("unexpected: {other:?}"),
    }
  };
  let mut anchored_at = None;
  let mut failed = false;
  for (index, av_pkt) in clip.packets.iter().enumerate() {
    if index == failing {
      dec.fail_next_packet_for_test();
    }
    loop {
      match dec.send_packet(&pushed(av_pkt)) {
        Ok(Sent::Accepted) => break,
        Err(VideoDecodeError::Decode(_)) => {
          failed |= index == failing;
          break;
        }
        Ok(Sent::MustDrain) => {
          assert!(
            !drain(&mut dec, &mut delivered),
            "no escalation before the end"
          );
        }
        Err(other) => panic!("send_packet: {other:?}"),
      }
    }
    if index == failing {
      assert!(
        failed && !dec.degraded_anchored_for_test(),
        "the IDR at {failing} is reported failed and anchors nothing"
      );
    }
    if index == good {
      assert!(
        dec.degraded_resync_pending_for_test() && dec.degraded_anchored_for_test(),
        "the IDR at {good} anchors the open gap: {delivered:?}"
      );
      anchored_at = Some(delivered.len());
    }
    assert!(
      !drain(&mut dec, &mut delivered),
      "no escalation before the end"
    );
  }
  crate::accepted(dec.send_eof(), "send_eof");
  let escalated = drain(&mut dec, &mut delivered);
  let after = &delivered[anchored_at.expect("the good IDR anchored")..];
  assert!(
    after
      .first()
      .is_some_and(|&(pts, open)| pts < good_pts && open),
    "the first picture out after the anchor is from before it and leaves the gap open: {after:?}"
  );
  assert_eq!(
    after.iter().position(|&(_, open)| !open),
    Some(2),
    "the bound, a depth of 2, closes the gap at the third picture out: {after:?}"
  );
  assert!(!escalated, "the end is clean");
}

/// LAW (Codex R11, [high]; restating Codex R10's): **a post-commit fallback
/// onto a decoder that wraps another implementation is refused by name, and
/// nothing is committed.** A 16-frame `libx264` open-GOP clip, the hardware
/// failing post-commit at its recovery point (8). On FFmpeg's own `h264` the
/// recovery point's picture, out first, closes the gap. With the next open
/// reading as wrapped (a test seam: `cuvid`, whose `h264_cuvid` keeps a
/// display delay it never publishes), the send that would commit the
/// fallback answers `Error::ResyncUnprovable`, naming the codec, the
/// implementation opened and its wrapper: the wrapped decoder took no
/// packet and is closed, the session is still on the hardware, and no gap
/// is open. A session opened on software, and a probe-era fallback, decode
/// the same clip whole on a wrapped decoder: they owe no proof. R10 proved a
/// wrapped decoder by the reorder bound, which it does not publish.
#[test]
fn a_post_commit_fallback_onto_a_wrapped_decoder_is_refused_by_name() {
  let clip = encode_h264_open_gops(128, 96, 16);
  let at = keyframe_after(&clip, 3);
  let recovery_point = clip.packets[at].pts().expect("a pts");
  let (_, native, native_escalated) = through_a_post_commit_failure(&clip, at);
  assert_eq!(
    native.first(),
    Some(&(recovery_point, false)),
    "on `h264` the first picture out closes the gap: {native:?}"
  );
  assert!(!native_escalated, "the end is clean: {native:?}");

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, at, at, FailShape::PostCommit)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  for av_pkt in &clip.packets[..at] {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    drain_ready(&mut dec);
  }
  super::live_sw::reset_peak();
  super::live_sw::reset_sent();
  super::sw_implementation::wrapped_next("cuvid");
  match dec.send_packet(&pushed(&clip.packets[at])) {
    Err(VideoDecodeError::Decode(Error::ResyncUnprovable(refused))) => assert_eq!(
      (refused.codec(), refused.implementation(), refused.wrapper()),
      (crate::CodecId::H264, Some("h264"), Some("cuvid")),
      "the codec, the implementation opened and its wrapper are named"
    ),
    Err(other) => panic!("refused by another name: {other:?}"),
    Ok(sent) => panic!("the fallback onto a wrapped decoder committed: {sent:?}"),
  }
  assert_eq!(super::live_sw::peak(), 1, "the wrapped decoder was opened");
  assert_eq!(super::live_sw::sent(), 0, "and handed nothing");
  super::live_sw::reset_peak();
  assert_eq!(
    super::live_sw::peak(),
    0,
    "and closed: no software decoder is open"
  );
  assert!(!dec.is_software(), "the session is still on the hardware");
  assert!(
    !dec.degraded_resync_pending_for_test(),
    "no gap is open: nothing was committed"
  );

  // Opened on software, the same clip decodes whole on a wrapped decoder.
  super::sw_implementation::wrapped_next("cuvid");
  let mut software = FfmpegVideoStreamDecoder::open_as(
    clip.parameters.clone(),
    tb,
    DecoderLimits::default(),
    DecodePath::Software,
  )
  .expect("a session opened on software, on a wrapped decoder");
  let mut dst = crate::empty_owned_video_frame();
  let mut decoded = 0;
  for av_pkt in &clip.packets {
    crate::accepted(software.send_packet(&pushed(av_pkt)), "send_packet");
    while let Received::Frame = software.receive_frame(&mut dst).expect("receive_frame") {
      decoded += 1;
    }
  }
  crate::accepted(software.send_eof(), "send_eof");
  while let Received::Frame = software.receive_frame(&mut dst).expect("receive_frame") {
    decoded += 1;
  }
  assert_eq!(
    decoded,
    clip.packets.len(),
    "every picture, opened on software"
  );

  // So does a probe-era fallback, which replays the whole history.
  super::sw_implementation::wrapped_next("cuvid");
  let (replayed, _) = threads_through_a_fallback(&clip, 3, crate::Threads::Single);
  assert_eq!(
    replayed.len(),
    clip.packets.len(),
    "every picture, through a probe-era fallback"
  );
}

/// LAW (Codex R11, [high]): **every codec this suite decodes is decoded, in
/// this build, by libavcodec's own decoder.** For each codec the fixtures
/// encode — H.264, HEVC, MPEG-4 part 2, MPEG-2, H.263, ProRes, Ut Video, VP8
/// — and VP9, whose keyframes reset every reference, the decoder
/// `avcodec_find_decoder` answers, which the software road opens, has a null
/// `wrapper_name`: the invariant the post-commit resync laws, and that
/// fallback in CI's builds, stand on. AV1 is held to it only where the build
/// answers a native decoder first: FFmpeg's own `av1` decodes only through a
/// hardware accelerator and is listed after `libdav1d`, a wrapper. Where
/// `libdav1d` answers (Homebrew's FFmpeg 9 does), the software road reads it
/// as wrapped and names it, and a post-commit fallback on AV1 is refused by
/// name.
#[test]
fn the_software_road_opens_libavcodecs_own_decoder_for_every_fixture_codec() {
  use ffmpeg_next::ffi::{AVCodecID, AVMediaType};
  let parameters_of = |id: AVCodecID| {
    let mut parameters = Parameters::new();
    // SAFETY: `parameters` owns a fresh `AVCodecParameters`; two fields are
    // written with values of their own types.
    unsafe {
      (*parameters.as_mut_ptr()).codec_type = AVMediaType::AVMEDIA_TYPE_VIDEO;
      (*parameters.as_mut_ptr()).codec_id = id;
    }
    parameters
  };
  for id in [
    AVCodecID::AV_CODEC_ID_H264,
    AVCodecID::AV_CODEC_ID_HEVC,
    AVCodecID::AV_CODEC_ID_MPEG4,
    AVCodecID::AV_CODEC_ID_MPEG2VIDEO,
    AVCodecID::AV_CODEC_ID_H263,
    AVCodecID::AV_CODEC_ID_PRORES,
    AVCodecID::AV_CODEC_ID_UTVIDEO,
    AVCodecID::AV_CODEC_ID_VP8,
    AVCodecID::AV_CODEC_ID_VP9,
  ] {
    let codec = crate::decoder::find_decoder(&parameters_of(id)).expect("a decoder for the codec");
    assert!(
      super::wrapper_name(codec).is_null(),
      "{id:?}: the software road opens `{}`, a wrapper",
      codec.name()
    );
  }
  let av1 = parameters_of(AVCodecID::AV_CODEC_ID_AV1);
  let Ok(codec) = crate::decoder::find_decoder(&av1) else {
    return;
  };
  let sw = super::open_sw_decoder(&av1, DecoderLimits::default(), None).expect("an AV1 decoder");
  assert_eq!(
    (sw.native, sw.wrapper.is_some()),
    (
      super::wrapper_name(codec).is_null(),
      !super::wrapper_name(codec).is_null()
    ),
    "AV1 on `{}`: the software road reads its wrapper as the codec list holds it",
    codec.name()
  );
  if !sw.native {
    let refused = sw.resync_unprovable();
    assert_eq!(
      (refused.codec(), refused.implementation()),
      (crate::CodecId::AV1, Some(codec.name())),
      "the refusal names the codec and the implementation: {refused}"
    );
  }
}

/// LAW (Codex R9, [high]): **a software video decoder set to output
/// pictures before their recovery is refused at the open, by name, in every
/// build.** The session clears `AV_CODEC_FLAG_OUTPUT_CORRUPT` and
/// `AV_CODEC_FLAG2_SHOW_ALL` before the open; made to find either set after
/// it, the open on the software road is refused with
/// `Error::UnrecoveredOutput`, naming the flag it found. It was a debug
/// assertion: a release build opened the decoder, and the H.264 resync's
/// proof — FFmpeg withholding every picture it has not recovered — would
/// have been false.
#[test]
fn a_decoder_set_to_output_unrecovered_pictures_is_refused_at_the_open_by_name() {
  let clip = encode_h264_closed_gops(128, 96, 8);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let open = || {
    FfmpegVideoStreamDecoder::open_as(
      clip.parameters.clone(),
      tb,
      DecoderLimits::default(),
      DecodePath::Software,
    )
  };
  for (output_corrupt, show_all) in [(true, false), (false, true)] {
    super::unrecovered_output::arm(output_corrupt, show_all);
    match open() {
      Err(Error::UnrecoveredOutput(found)) => assert_eq!(
        (found.output_corrupt(), found.show_all()),
        (output_corrupt, show_all),
        "the flag found is named"
      ),
      Err(other) => panic!("refused by another name: {other:?}"),
      Ok(_) => panic!(
        "opened with AV_CODEC_FLAG_OUTPUT_CORRUPT {output_corrupt}, AV_CODEC_FLAG2_SHOW_ALL \
         {show_all}"
      ),
    }
  }
  assert!(
    open().is_ok(),
    "with neither flag set, the same open succeeds"
  );
}

/// LAW (the authority's row 6): **an HEVC stream of CRAs resyncs and ends
/// clean.** `libx265`'s open GOPs: every keyframe after the first a CRA,
/// none of them a clean random access point. The hardware fails
/// post-commit at one; the cold software decoder takes it, its RASL
/// leaders dropped, and the gap closes by the reorder bound. The end is
/// clean, and every picture from the CRA on comes out once.
#[test]
fn an_hevc_cra_stream_resyncs_and_ends_clean() {
  let clip = encode_hevc_cra_with_headers(128, 96, 40);
  let at = keyframe_after(&clip, 3);
  assert!(
    clip.packets[at].is_key(),
    "the packet is flagged a keyframe"
  );
  let anchor_pts = clip.packets[at].pts().expect("a pts");

  let (dec, delivered, escalated) = through_a_post_commit_failure(&clip, at);
  assert!(dec.is_software(), "the hardware failed post-commit");
  assert!(!escalated, "the end is clean: {delivered:?}");
  assert!(
    delivered.iter().any(|&(_, open)| !open),
    "the gap closed before the end: {delivered:?}"
  );
  let shown: Vec<i64> = delivered.iter().map(|&(pts, _)| pts).collect();
  let mut once = shown.clone();
  once.sort_unstable();
  once.dedup();
  assert_eq!(once.len(), shown.len(), "no picture twice: {shown:?}");
  let last = clip
    .packets
    .iter()
    .filter_map(Packet::pts)
    .max()
    .expect("a pts");
  for pts in anchor_pts..=last {
    assert!(
      shown.contains(&pts),
      "picture {pts} from the CRA on is missing: {shown:?}"
    );
  }
}

/// LAW (the authority's row 6): **the anchor waits for the picture the
/// decoder holds, and the bound closes the gap past it.** Packet 11's
/// concealed picture waits in the decoder when keyframe 12 is sent, so the
/// send answers `MustDrain`: the anchor is fed only once nothing the caller
/// has not taken is left. Drained, packet 11's picture comes out with the
/// gap still open. Keyframe 12 then anchors the resync, the gap still open
/// before any picture follows it; this stream reorders nothing
/// (`has_b_frames` 0), so the first picture after the anchor closes it, and
/// pictures 12 to 23 each come out once.
#[test]
fn the_anchor_waits_for_the_held_picture_and_the_bound_closes_the_gap() {
  let clip = encode_synthetic_clip(128, 96, 24, 6);
  assert_eq!(nth_keyframe(&clip, 3), 12);
  let mut dec = before_the_anchor(&clip);
  assert_eq!(
    dec
      .send_packet(&pushed(&clip.packets[12]))
      .expect("send_packet"),
    Sent::MustDrain,
    "the anchor waits for the picture packet 11 left in the decoder"
  );
  assert!(!dec.degraded_anchored_for_test(), "not anchored yet");

  let mut dst = crate::empty_owned_video_frame();
  let mut delivered: Vec<(i64, bool)> = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, delivered: &mut Vec<(i64, bool)>| {
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      let pts = dst.pts().map_or(i64::MIN, |t| t.pts());
      delivered.push((pts, dec.degraded_resync_pending_for_test()));
    }
  };
  drain(&mut dec, &mut delivered);
  assert_eq!(
    delivered,
    [(11, true)],
    "packet 11's picture, the gap still open"
  );
  crate::accepted(dec.send_packet(&pushed(&clip.packets[12])), "the anchor");
  assert!(
    dec.degraded_anchored_for_test(),
    "keyframe 12 anchors the resync"
  );
  assert!(
    dec.degraded_resync_pending_for_test(),
    "no picture has come out since the anchor"
  );
  drain(&mut dec, &mut delivered);
  for av_pkt in &clip.packets[13..] {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    drain(&mut dec, &mut delivered);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  drain(&mut dec, &mut delivered);
  assert_eq!(
    delivered[1],
    (12, false),
    "the first picture after the anchor closes the gap"
  );
  assert_eq!(
    delivered[1..]
      .iter()
      .map(|&(pts, _)| pts)
      .collect::<Vec<_>>(),
    (12..24).collect::<Vec<i64>>(),
    "pictures 12 to 23, each once"
  );
}

/// LAW (the authority's row 6): **the gap closes at the picture past the
/// reorder bound, never at one the reorder buffer held.** An MPEG-4 part 2
/// stream with B-frames: the decoder holds a picture back
/// (`has_b_frames` 1). The hardware fails post-commit mid-GOP; the cold
/// software decoder conceals until the next keyframe, which anchors the
/// resync while the reorder buffer still holds a picture from before it.
/// That picture comes out first after the anchor and leaves the gap open;
/// the next — the `has_b_frames + 1`-th — closes it.
#[test]
fn the_gap_closes_at_the_picture_past_the_reorder_bound() {
  let clip = encode_mpeg4_with_b_frames(128, 96, 30);
  let anchor = keyframe_after(&clip, 8);
  // A mid-GOP packet a cold decoder takes, before the anchor.
  let at = (anchor.saturating_sub(4)..anchor)
    .find(|&index| {
      !clip.packets[index].is_key() && {
        let mut sw = super::open_sw_decoder(
          &clip.parameters,
          crate::DecoderLimits::default().with_threads(crate::Threads::Single),
          None,
        )
        .expect("a software decoder");
        sw.submit(&clip.packets[index]).is_ok()
      }
    })
    .expect("a mid-GOP packet a cold decoder takes");
  let pre: Vec<i64> = clip.packets[..anchor]
    .iter()
    .filter_map(Packet::pts)
    .collect();

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, at, at, FailShape::PostCommit)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  let mut dst = crate::empty_owned_video_frame();
  let mut drain_quiet = |dec: &mut FfmpegVideoStreamDecoder| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) | Err(VideoDecodeError::Decode(_)) => {}
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(other) => panic!("unexpected: {other:?}"),
    }
  };
  for av_pkt in &clip.packets[..anchor] {
    match dec.send_packet(&pushed(av_pkt)) {
      Ok(Sent::Accepted) | Err(VideoDecodeError::Decode(_)) => {}
      other => panic!("send_packet: {other:?}"),
    }
    drain_quiet(&mut dec);
  }
  assert!(dec.degraded_resync_pending_for_test(), "the gap is open");
  crate::accepted(
    dec.send_packet(&pushed(&clip.packets[anchor])),
    "the anchor",
  );
  assert!(
    dec.degraded_anchored_for_test(),
    "the keyframe anchors the resync"
  );
  let reorder = dec.reorder_for_test();
  assert_eq!(reorder, 1, "the decoder holds one picture back");

  let mut after: Vec<(i64, bool)> = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, after: &mut Vec<(i64, bool)>| {
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      let pts = dst.pts().map_or(i64::MIN, |t| t.pts());
      after.push((pts, dec.degraded_resync_pending_for_test()));
    }
  };
  drain(&mut dec, &mut after);
  for av_pkt in &clip.packets[anchor + 1..] {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    drain(&mut dec, &mut after);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  drain(&mut dec, &mut after);

  assert!(
    pre.contains(&after[0].0) && after[0].1,
    "the first picture after the anchor is the reorder buffer's, and leaves the gap open: {after:?}"
  );
  assert!(
    !after[reorder].1,
    "the {}th picture after the anchor closes the gap: {after:?}",
    reorder + 1
  );
}

/// LAW (the authority's row 6): **a keyframe that decodes to nothing
/// leaves the loss reported, and the loss is the fallback window.** The
/// anchor's payload is damaged: drained of packet 11's picture, the decoder
/// is fed keyframe 12, which fails and anchors nothing. At the end of the
/// stream the gap is still open, and it escalates as
/// `PostCommitNeverResynced` — counting the four packets fed across the gap
/// before it, 8 to 11, and no anchor: the damaged keyframe was refused, and
/// its own error reported it.
#[test]
fn a_keyframe_that_decodes_to_nothing_leaves_the_loss_reported() {
  let mut clip = encode_synthetic_clip(128, 96, 24, 6);
  assert_eq!(nth_keyframe(&clip, 3), 12);
  corrupt_packet_payload(&mut clip.packets[12]);
  let mut dec = before_the_anchor(&clip);
  let mut dst = crate::empty_owned_video_frame();
  let mut lost = None;
  let mut ended = false;
  let mut eof_sent = false;
  let mut anchor_sent = false;
  for _ in 0..32 {
    if !anchor_sent {
      match dec.send_packet(&pushed(&clip.packets[12])) {
        Ok(Sent::MustDrain) => {}
        // Taken, or refused for its damage: either way it is out of the
        // caller's hands.
        Ok(Sent::Accepted) | Err(VideoDecodeError::Decode(_)) => anchor_sent = true,
        Err(other) => panic!("the anchor: {other:?}"),
      }
    }
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => {
        let pts = dst.pts().map_or(i64::MIN, |t| t.pts());
        assert!(pts < 12, "no picture comes from the damaged anchor");
      }
      Ok(Received::NeedsInput) if anchor_sent && !eof_sent => {
        crate::accepted(dec.send_eof(), "send_eof");
        eof_sent = true;
      }
      Ok(Received::NeedsInput) => {}
      Ok(Received::Ended) => {
        ended = true;
        break;
      }
      Err(VideoDecodeError::PostCommitNeverResynced(loss)) => {
        lost = Some((
          loss.packets_before_anchor(),
          loss.packets_unproven(),
          loss.anchor_seen(),
        ));
        break;
      }
      // The damaged anchor's own decode error, reported where it met.
      Err(VideoDecodeError::Decode(_)) => {}
      Err(other) => panic!("unexpected: {other:?}"),
    }
  }
  assert!(
    !ended,
    "the gap never closed, so the end does not end clean"
  );
  assert_eq!(
    lost,
    Some((4, 0, false)),
    "the fallback window, packets 8 to 11, and no anchor: the damaged keyframe was refused"
  );
}

/// LAW (Codex R5 row 1, [high]): **the reorder bound keeps the largest
/// depth from just before the anchor on.** A keyframe can activate
/// parameters that lower `has_b_frames` — an HEVC SPS with fewer
/// `num_reorder_pics` — while pictures from before it still wait in the
/// decoder, so the depth is read before the anchoring packet is submitted,
/// and the bound keeps the largest of that, the depth after it and every
/// depth since. Here the decoder serving reports a depth of 2 until it takes
/// keyframe 12 and 0 after it: the bound stays 2, and pictures 12 and 13
/// leave the gap open. Raised to 4 before packet 14 and lowered to 0 again
/// before 15, the bound stays 4: 14 and 15 leave the gap open too, and 16,
/// the fifth picture out after the anchor, closes it.
#[test]
fn the_reorder_bound_keeps_the_largest_depth_from_before_the_anchor_on() {
  let clip = encode_synthetic_clip(128, 96, 24, 6);
  assert_eq!(nth_keyframe(&clip, 3), 12);
  let mut dec = before_the_anchor(&clip);
  let mut dst = crate::empty_owned_video_frame();
  let mut delivered: Vec<(i64, bool)> = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, delivered: &mut Vec<(i64, bool)>| {
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      let pts = dst.pts().map_or(i64::MIN, |t| t.pts());
      delivered.push((pts, dec.degraded_resync_pending_for_test()));
    }
  };
  drain(&mut dec, &mut delivered);
  assert_eq!(
    delivered,
    [(11, true)],
    "packet 11's picture, the gap still open"
  );
  delivered.clear();

  dec.set_reorder_for_test(2);
  dec.reorder_on_next_packet_for_test(0);
  crate::accepted(dec.send_packet(&pushed(&clip.packets[12])), "the anchor");
  assert!(
    dec.degraded_anchored_for_test(),
    "keyframe 12 anchors the resync"
  );
  assert_eq!(dec.reorder_for_test(), 0, "the anchor lowered the depth");
  drain(&mut dec, &mut delivered);
  crate::accepted(dec.send_packet(&pushed(&clip.packets[13])), "packet 13");
  drain(&mut dec, &mut delivered);
  dec.set_reorder_for_test(4);
  crate::accepted(dec.send_packet(&pushed(&clip.packets[14])), "packet 14");
  drain(&mut dec, &mut delivered);
  dec.set_reorder_for_test(0);
  for av_pkt in &clip.packets[15..17] {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    drain(&mut dec, &mut delivered);
  }
  assert_eq!(
    delivered,
    [(12, true), (13, true), (14, true), (15, true), (16, false)],
    "the gap stays open while the pictures out since the anchor are within the largest depth, \
     4, and closes at the fifth"
  );
}

/// The MPEG-4 B-frame road the depth-1 laws stand on: a clip whose decoder
/// holds a picture back (`has_b_frames` 1). The hardware fails post-commit
/// at a mid-GOP packet a cold decoder takes, a few packets before the
/// keyframe after packet 8, and every packet up to that keyframe goes to the
/// cold software decoder, drained — a decode error a picture the gap
/// dropped earns tolerated. Answers the decoder, the keyframe's index, the
/// timestamps of the packets before it, and how many of them the cold
/// decoder took.
fn across_a_b_frame_gap(clip: &SyntheticClip) -> (FfmpegVideoStreamDecoder, usize, Vec<i64>, u64) {
  let anchor = keyframe_after(clip, 8);
  // A mid-GOP packet a cold decoder takes, before the anchor.
  let at = (anchor.saturating_sub(4)..anchor)
    .find(|&index| {
      !clip.packets[index].is_key() && {
        let mut sw = super::open_sw_decoder(
          &clip.parameters,
          crate::DecoderLimits::default().with_threads(crate::Threads::Single),
          None,
        )
        .expect("a software decoder");
        sw.submit(&clip.packets[index]).is_ok()
      }
    })
    .expect("a mid-GOP packet a cold decoder takes");
  let pre: Vec<i64> = clip.packets[..anchor]
    .iter()
    .filter_map(Packet::pts)
    .collect();
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, at, at, FailShape::PostCommit)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  let mut dst = crate::empty_owned_video_frame();
  let mut drain_quiet = |dec: &mut FfmpegVideoStreamDecoder| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) | Err(VideoDecodeError::Decode(_)) => {}
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(other) => panic!("unexpected: {other:?}"),
    }
  };
  let mut taken = 0u64;
  for (index, av_pkt) in clip.packets[..anchor].iter().enumerate() {
    match dec.send_packet(&pushed(av_pkt)) {
      Ok(Sent::Accepted) => taken += u64::from(index >= at),
      Err(VideoDecodeError::Decode(_)) => {}
      other => panic!("send_packet: {other:?}"),
    }
    drain_quiet(&mut dec);
  }
  assert!(dec.is_software(), "the hardware failed post-commit");
  assert!(dec.degraded_resync_pending_for_test(), "the gap is open");
  (dec, anchor, pre, taken)
}

/// A B-frame from before packet `before`, flagged as a keyframe — a
/// key-flagged packet a decoder past it takes and, its time out of order,
/// decodes to nothing. A copy of its own: the push face takes no payload
/// the clip still shares.
fn a_stale_b_frame_flagged_key(clip: &SyntheticClip, before: usize) -> Packet {
  // Decode order puts a B-frame after a later picture.
  let stale = (1..before)
    .find(|&index| clip.packets[index].pts() < clip.packets[index - 1].pts())
    .expect("a B-frame");
  let mut flagged = Packet::copy(clip.packets[stale].data().expect("a payload"));
  flagged.set_pts(clip.packets[stale].pts());
  flagged.set_dts(clip.packets[stale].dts());
  flagged.set_flags(ffmpeg_next::packet::Flags::KEY);
  flagged
}

/// LAW (Codex R5 row 2, [high]): **at the end of the stream the reorder
/// bound is the proof too.** On the MPEG-4 B-frame road (`has_b_frames` 1)
/// the cold decoder holds one picture from before the keyframe. In the
/// keyframe's place it is fed a stale B-frame flagged as a keyframe, which
/// it takes and, its time out of order, decodes to nothing: an anchor with
/// no resync behind it. At the end the held picture comes out — one picture
/// since the anchor, within the bound — so the gap never closed, and the end
/// escalates `PostCommitNeverResynced` after that picture rather than pass
/// as clean because a picture came out.
#[test]
fn an_anchor_that_decodes_to_nothing_is_not_proved_by_the_end_of_the_stream() {
  let clip = encode_mpeg4_with_b_frames(128, 96, 30);
  let (mut dec, anchor, pre, _) = across_a_b_frame_gap(&clip);
  assert_eq!(
    dec.reorder_for_test(),
    1,
    "the decoder holds one picture back"
  );
  let flagged = a_stale_b_frame_flagged_key(&clip, anchor);
  crate::accepted(
    dec.send_packet(&pushed(&flagged)),
    "the stale B-frame, flagged a keyframe",
  );
  assert!(dec.degraded_anchored_for_test(), "it anchors the resync");
  assert_eq!(drain_ready(&mut dec), 0, "and decodes to nothing");
  crate::accepted(dec.send_eof(), "send_eof");
  let mut dst = crate::empty_owned_video_frame();
  let mut out = Vec::new();
  let escalated = loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => out.push(dst.pts().map_or(i64::MIN, |t| t.pts())),
      Ok(Received::Ended) => break false,
      Err(VideoDecodeError::PostCommitNeverResynced(_)) => break true,
      other => panic!("draining to the end: {other:?}"),
    }
  };
  assert!(
    out.len() == 1 && pre.contains(&out[0]),
    "the one picture held from before the anchor comes out: {out:?}"
  );
  assert!(escalated, "the gap never closed, so the end escalates");
}

/// LAW (Codex R5 row 3, [high]): **a packet the decoder reports failed
/// leaves the output unsettled, and the next anchor waits for a drain.**
/// FFmpeg's submission is not transactional: a packet it reports failed may
/// have left a picture ready. Drained of everything before it, the cold
/// decoder takes packet 11 and reports it failed, its concealed picture left
/// ready. Keyframe 12 then answers `MustDrain` — no anchor before a drain has
/// answered "needs input". Drained, packet 11's picture comes out with the
/// gap still open; sent again, keyframe 12 anchors, and its own picture, not
/// packet 11's, closes the gap.
#[test]
fn a_packet_reported_failed_leaves_the_output_unsettled_until_a_drain() {
  let clip = encode_synthetic_clip(128, 96, 24, 6);
  assert_eq!(nth_keyframe(&clip, 3), 12);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, 8, 8, FailShape::PostCommit)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  for av_pkt in &clip.packets[..11] {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    drain_ready(&mut dec);
  }
  assert!(dec.degraded_resync_pending_for_test(), "the gap is open");

  dec.fail_next_packet_for_test();
  match dec.send_packet(&pushed(&clip.packets[11])) {
    Err(VideoDecodeError::Decode(_)) => {}
    other => panic!("packet 11 is reported failed: {other:?}"),
  }
  assert_eq!(
    dec
      .send_packet(&pushed(&clip.packets[12]))
      .expect("send_packet"),
    Sent::MustDrain,
    "no anchor before a drain"
  );
  assert!(!dec.degraded_anchored_for_test(), "not anchored yet");

  let mut dst = crate::empty_owned_video_frame();
  let mut delivered: Vec<(i64, bool)> = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, delivered: &mut Vec<(i64, bool)>| {
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      let pts = dst.pts().map_or(i64::MIN, |t| t.pts());
      delivered.push((pts, dec.degraded_resync_pending_for_test()));
    }
  };
  drain(&mut dec, &mut delivered);
  assert_eq!(
    delivered,
    [(11, true)],
    "packet 11's picture, the gap still open"
  );
  crate::accepted(dec.send_packet(&pushed(&clip.packets[12])), "the anchor");
  assert!(
    dec.degraded_anchored_for_test(),
    "keyframe 12 anchors the resync"
  );
  drain(&mut dec, &mut delivered);
  assert_eq!(
    delivered,
    [(11, true), (12, false)],
    "keyframe 12's own picture closes the gap"
  );
}

/// LAW (Codex R5 row 5, [high]): **past the end, a replay's error waits
/// behind the pictures it queued before it.** A probe-era fallback raised
/// at the first drain after the end replays twelve packets through a queue
/// whose budget holds two and a half pictures, so the drains feed it in
/// rounds. Packet 5 is damaged: the round that feeds it queues pictures 3
/// and 4 first, then meets its error. The drain delivers 3 and 4, then the
/// error, then the rest — the error never jumps a picture decoded before it.
#[test]
fn past_the_end_a_replay_error_waits_behind_the_pictures_queued_before_it() {
  let (w, h) = (96u32, 64u32);
  let mut clip = encode_synthetic_clip(w, h, 12, 100);
  let budget = picture_bytes(&clip) * 5 / 2;
  corrupt_packet_payload(&mut clip.packets[5]);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing_at_receive(w, h)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Single)
  .with_max_replay_bytes_for_test(budget);
  for av_pkt in &clip.packets {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
  }
  crate::accepted(dec.send_eof(), "send_eof");

  let mut dst = crate::empty_owned_video_frame();
  let mut seen: Vec<Option<i64>> = Vec::new();
  loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => seen.push(Some(dst.pts().map_or(i64::MIN, |t| t.pts()))),
      Ok(Received::Ended) => break,
      Err(VideoDecodeError::Decode(_)) => seen.push(None),
      other => panic!("draining past the end: {other:?} after {seen:?}"),
    }
    assert!(seen.len() < 64, "the drain ends: {seen:?}");
  }
  assert!(dec.is_software(), "the probe-era fallback committed");
  assert_eq!(
    seen[..6],
    [Some(0), Some(1), Some(2), Some(3), Some(4), None],
    "pictures 3 and 4, queued before the damaged packet, come out before its error: {seen:?}"
  );
}

/// LAW (Codex R5 row 6, [high]): **the end a replay owed is the session's
/// the moment the decoder takes it, and it is never sent again.** A
/// probe-era fallback raised by `send_eof` replays eleven packets through a
/// queue whose budget holds two and a half pictures, so each `send_eof`
/// feeds a round of two and answers `MustDrain`, until the last, which feeds
/// picture 10 and the end — and the drain after the end fails. That
/// `send_eof` is accepted: the end was committed when the decoder took it.
/// The drain then delivers picture 10 and then the error, and the decoder is
/// told the stream ended exactly once — a caller that obeys every
/// `MustDrain` never sends a second end into a decoder that has one.
#[test]
fn the_end_a_replay_owed_is_committed_when_the_decoder_takes_it() {
  let (w, h) = (96u32, 64u32);
  let clip = encode_synthetic_clip(w, h, 11, 100);
  assert_eq!(clip.packets.len(), 11, "one packet a picture");
  let budget = picture_bytes(&clip) * 5 / 2;
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing_at_eof(w, h)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Single)
  .with_max_replay_bytes_for_test(budget);
  for av_pkt in &clip.packets {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
  }
  super::replay_fault::arm();
  super::live_sw::reset_eofs();

  let mut dst = crate::empty_owned_video_frame();
  let mut seen: Vec<Option<i64>> = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, seen: &mut Vec<Option<i64>>| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => seen.push(Some(dst.pts().map_or(i64::MIN, |t| t.pts()))),
      Err(VideoDecodeError::Decode(_)) => seen.push(None),
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(other) => panic!("draining: {other:?} after {seen:?}"),
    }
  };
  let mut fault = None;
  let mut rounds = 0usize;
  loop {
    match dec.send_eof() {
      Ok(Sent::Accepted) => break,
      Ok(Sent::MustDrain) => {
        rounds += 1;
        assert!(rounds < 16, "the replay advances: {seen:?}");
        drain(&mut dec, &mut seen);
      }
      Err(error) => {
        fault = Some(error);
        break;
      }
    }
  }
  assert!(
    fault.is_none(),
    "the end, once the decoder took it, is never sent again: {fault:?}"
  );
  assert!(dec.is_software(), "the probe-era fallback committed");
  assert!(dec.eof_sent_for_test(), "the session's end is committed");
  assert_eq!(
    seen,
    (0..10).map(Some).collect::<Vec<_>>(),
    "the rounds before the last delivered pictures 0 to 9"
  );
  drain(&mut dec, &mut seen);
  assert_eq!(
    seen[10..],
    [Some(10), None],
    "the last round's picture, then the error held behind it: {seen:?}"
  );
  assert!(
    matches!(dec.receive_frame(&mut dst), Ok(Received::Ended)),
    "and the end"
  );
  assert_eq!(
    super::live_sw::eofs(),
    1,
    "the decoder was told the stream ended once"
  );
}

/// Sends EOF and drains to the end, answering the escalation if there was
/// one — every picture before it delivered, a decode error tolerated.
fn the_end(dec: &mut FfmpegVideoStreamDecoder) -> Option<PostCommitNeverResynced> {
  crate::accepted(dec.send_eof(), "send_eof");
  let mut dst = crate::empty_owned_video_frame();
  loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) | Err(VideoDecodeError::Decode(_)) => {}
      Ok(Received::Ended) => break None,
      Err(VideoDecodeError::PostCommitNeverResynced(loss)) => break Some(loss),
      other => panic!("draining to the end: {other:?}"),
    }
  }
}

/// LAW (Codex R5 row 7, [medium]): **an anchor that decodes to nothing is
/// seen, with nothing after it.** On the MPEG-4 B-frame road the cold
/// decoder takes the gap's packets, then a key-flagged packet that decodes
/// to nothing — a stale B-frame — and the stream ends. The escalation counts
/// the gap's packets before the keyframe, none after it, and says a
/// keyframe was seen: "N packets before a keyframe, 0 after it with the
/// resync never proved".
#[test]
fn an_empty_anchor_is_seen_with_the_packets_before_it_counted() {
  let clip = encode_mpeg4_with_b_frames(128, 96, 30);
  let (mut dec, anchor, _, taken) = across_a_b_frame_gap(&clip);
  assert!(taken >= 1, "the cold decoder took the gap's packets");
  crate::accepted(
    dec.send_packet(&pushed(&a_stale_b_frame_flagged_key(&clip, anchor))),
    "the stale B-frame, flagged a keyframe",
  );
  assert!(dec.degraded_anchored_for_test(), "it anchors the resync");
  let loss = the_end(&mut dec).expect("the gap never closed");
  assert_eq!(
    (
      loss.packets_before_anchor(),
      loss.packets_unproven(),
      loss.anchor_seen()
    ),
    (taken, 0, true),
    "the gap's packets before the keyframe, none after it, the keyframe seen"
  );
  assert_eq!(
    loss.to_string(),
    format!(
      "post-commit HW->SW fallback never resynced before EOF: {taken} packets before a \
       keyframe, 0 after it with the resync never proved"
    )
  );
}

/// LAW (Codex R5 row 7, [medium]): **the packets after the anchor are
/// counted, through an un-anchor.** The cold decoder takes packets 8 to 11
/// across the gap, and keyframe 12 anchors the resync while the decoder
/// serving reports a reorder depth of 3, so pictures 12 to 14 leave the gap
/// open. Packet 15 is reported failed, un-anchoring it; 16 and 17 are taken
/// and the stream ends before keyframe 18. The escalation counts the four
/// packets before the keyframe and the four taken after it — 13, 14, 16 and
/// 17 — where the single count it replaces lost 13 and 14 and put 16 and 17
/// in the window.
#[test]
fn the_packets_after_the_anchor_are_counted_through_an_unanchor() {
  let clip = encode_synthetic_clip(128, 96, 24, 6);
  assert_eq!(nth_keyframe(&clip, 3), 12);
  let mut dec = before_the_anchor(&clip);
  drain_ready(&mut dec);
  dec.set_reorder_for_test(3);
  for (index, av_pkt) in clip.packets.iter().enumerate().take(18).skip(12) {
    if index == 15 {
      dec.fail_next_packet_for_test();
      match dec.send_packet(&pushed(av_pkt)) {
        Err(VideoDecodeError::Decode(_)) => {}
        other => panic!("packet 15 is reported failed: {other:?}"),
      }
      assert!(!dec.degraded_anchored_for_test(), "the failure un-anchors");
    } else {
      crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    }
    drain_ready(&mut dec);
    assert!(dec.degraded_resync_pending_for_test(), "the gap stays open");
  }
  let loss = the_end(&mut dec).expect("the gap never closed");
  assert_eq!(
    (
      loss.packets_before_anchor(),
      loss.packets_unproven(),
      loss.anchor_seen()
    ),
    (4, 4, true),
    "packets 8 to 11 before the keyframe; 13, 14, 16 and 17 after it: {loss}"
  );
}

/// An H.264 clip from `libx264` in closed GOPs — an IDR every 8 frames, two
/// B-frames between references — with its SPS and PPS in the codec
/// parameters' extradata, so a decoder opened cold from them takes a P
/// slice without waiting for an IDR's.
fn encode_h264_with_extradata(width: u32, height: u32, frames: usize) -> SyntheticClip {
  use ffmpeg_next as ff;
  ff::init().expect("ffmpeg init");
  let codec = ff::codec::encoder::find_by_name("libx264").expect("libx264 is linked");
  let mut options = ff::Dictionary::new();
  options.set(
    "x264-params",
    "keyint=8:min-keyint=8:scenecut=0:bframes=2:b-adapt=0:open-gop=0:log-level=error",
  );
  encode_clip(codec, width, height, frames, options, |enc| {
    enc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
  })
}

/// The NAL units of an Annex B byte stream, in order, their start codes and
/// trailing zero bytes stripped.
fn annexb_units(data: &[u8]) -> Vec<&[u8]> {
  let mut begins = Vec::new();
  let mut at = 0;
  while at + 3 <= data.len() {
    if data[at..].starts_with(&[0, 0, 1]) {
      begins.push(at + 3);
      at += 3;
    } else {
      at += 1;
    }
  }
  begins
    .iter()
    .enumerate()
    .map(|(index, &begin)| {
      let end = begins.get(index + 1).map_or(data.len(), |&next| next - 3);
      let mut unit = &data[begin..end];
      while let [rest @ .., 0] = unit {
        unit = rest;
      }
      unit
    })
    .collect()
}

/// An `avcC` record carrying `sps` and `pps`, whose NAL length fields are
/// `length_size` bytes wide.
fn avcc(sps: &[u8], pps: &[u8], length_size: u8) -> Vec<u8> {
  let mut record = vec![1, sps[1], sps[2], sps[3], 0xFC | (length_size - 1), 0xE1];
  record.extend_from_slice(&u16::try_from(sps.len()).expect("a short SPS").to_be_bytes());
  record.extend_from_slice(sps);
  record.push(1);
  record.extend_from_slice(&u16::try_from(pps.len()).expect("a short PPS").to_be_bytes());
  record.extend_from_slice(pps);
  record
}

/// `units` one after another, each behind a big-endian length field
/// `length_size` bytes wide.
fn length_prefixed(units: &[&[u8]], length_size: usize) -> Vec<u8> {
  let mut out = Vec::new();
  for unit in units {
    assert!(
      unit.len() < 1 << (8 * length_size),
      "a unit fits its length field"
    );
    let length = unit.len().to_be_bytes();
    out.extend_from_slice(&length[length.len() - length_size..]);
    out.extend_from_slice(unit);
  }
  out
}

/// `packet`'s timing and flags around `payload`.
fn repacked(packet: &Packet, payload: &[u8]) -> Packet {
  let mut out = Packet::copy(payload);
  out.set_pts(packet.pts());
  out.set_dts(packet.dts());
  out.set_duration(packet.duration());
  out.set_flags(packet.flags());
  out
}

/// `packet` carrying `extradata` as `AV_PKT_DATA_NEW_EXTRADATA`.
fn with_new_extradata(mut packet: Packet, extradata: &[u8]) -> Packet {
  use ffmpeg_next::packet::Mut;
  // SAFETY: `packet` is a live packet this function owns; FFmpeg allocates
  // the side data, padded, and frees it with the packet; `extradata` is
  // copied into exactly the bytes it allocated.
  unsafe {
    let slot = crate::ffi::packet_new_side_data(
      packet.as_mut_ptr(),
      ffmpeg_next::ffi::AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA as i32,
      extradata.len(),
    )
    .expect("new extradata attached");
    core::ptr::copy_nonoverlapping(extradata.as_ptr(), slot, extradata.len());
  }
  packet
}

/// The extradata `parameters` carry.
fn extradata_of(parameters: &Parameters) -> Vec<u8> {
  // SAFETY: `parameters` owns a live `AVCodecParameters`, whose extradata
  // is null or `extradata_size` bytes long.
  unsafe {
    let raw = parameters.as_ptr();
    let size = usize::try_from((*raw).extradata_size).unwrap_or(0);
    if (*raw).extradata.is_null() || size == 0 {
      Vec::new()
    } else {
      core::slice::from_raw_parts((*raw).extradata, size).to_vec()
    }
  }
}

/// Replaces `parameters`' extradata with `extradata`, padded as libavcodec
/// reads it.
fn set_extradata(parameters: &mut Parameters, extradata: &[u8]) {
  use ffmpeg_next::ffi;
  let padded = extradata.len() + ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
  // SAFETY: `parameters` owns a live `AVCodecParameters`; its extradata is
  // freed and replaced by a zeroed allocation of the padded size holding
  // `extradata`.
  unsafe {
    let raw = parameters.as_mut_ptr();
    ffi::av_freep(core::ptr::addr_of_mut!((*raw).extradata).cast());
    let data = ffi::av_mallocz(padded).cast::<u8>();
    assert!(!data.is_null(), "extradata allocated");
    core::ptr::copy_nonoverlapping(extradata.as_ptr(), data, extradata.len());
    (*raw).extradata = data;
    (*raw).extradata_size = i32::try_from(extradata.len()).expect("a short record");
  }
}

/// The SPS and PPS `libx264` put in a clip's extradata.
fn sps_and_pps(clip: &SyntheticClip) -> (Vec<u8>, Vec<u8>) {
  let extradata = extradata_of(&clip.parameters);
  let units = annexb_units(&extradata);
  let of = |kind: u8| {
    units
      .iter()
      .find(|unit| unit.first().is_some_and(|header| header & 0x1f == kind))
      .map(|unit| unit.to_vec())
      .expect("the parameter set")
  };
  (of(7), of(8))
}

/// A `libx264` clip in closed GOPs — an IDR every 8 frames, two B-frames
/// between references — packed `avcC`, whose NAL length fields change from
/// four bytes to two at its third IDR (`change`, decode index 16): the codec
/// parameters carry the four-byte `avcC` record and the packets before
/// `change` four-byte fields; the packets from `change` on carry two-byte
/// fields, and `change`'s packet carries the two-byte record as
/// `AV_PKT_DATA_NEW_EXTRADATA` — what a container's sample description
/// switch hands a decoder. Answers the clip and `change`.
fn encode_h264_avcc_whose_length_size_changes(
  width: u32,
  height: u32,
  frames: usize,
) -> (SyntheticClip, usize) {
  let annexb = encode_h264_with_extradata(width, height, frames);
  let (sps, pps) = sps_and_pps(&annexb);
  let (four, two) = (avcc(&sps, &pps, 4), avcc(&sps, &pps, 2));
  let change = nth_keyframe(&annexb, 3);
  let packets = annexb
    .packets
    .iter()
    .enumerate()
    .map(|(index, packet)| {
      let units = annexb_units(packet.data().expect("a payload"));
      let length_size = if index < change { 4 } else { 2 };
      let packed = repacked(packet, &length_prefixed(&units, length_size));
      if index == change {
        with_new_extradata(packed, &two)
      } else {
        packed
      }
    })
    .collect();
  let mut parameters = annexb.parameters.clone();
  set_extradata(&mut parameters, &four);
  (
    SyntheticClip {
      parameters,
      packets,
    },
    change,
  )
}

/// LAW (Codex R11, [medium]): **across a new extradata, the IDR after the
/// change anchors a post-commit resync, and the gap closes.** A 32-frame
/// `avcC` stream whose NAL length fields go from four bytes to two at its
/// IDR 16, which carries the two-byte record as `AV_PKT_DATA_NEW_EXTRADATA`
/// (the next IDR, 24, carries none). The hardware fails post-commit before
/// the change, at packet 12: the cold software decoder takes 16 and reads it
/// under the record it carries, an IDR, which anchors; its picture, out
/// first, closes the gap, and 16 to 31 come out with the end clean. Then the
/// hardware fails after the change, at packet 20, having taken 16: the cold
/// decoder opens on the record the hardware took, and the IDR 24, which
/// carries none, anchors under it. Read under the parameters as opened, a
/// two-byte field is half of a four-byte one: nothing anchors, and the end
/// escalates.
#[test]
fn across_a_new_extradata_the_idr_after_the_change_anchors_the_resync() {
  let (clip, change) = encode_h264_avcc_whose_length_size_changes(128, 96, 32);
  assert_eq!(change, 16, "the change is at the third IDR");
  let pts_from = |from: usize| {
    let mut pts: Vec<i64> = clip.packets[from..]
      .iter()
      .map(|packet| packet.pts().expect("a pts"))
      .collect();
    pts.sort_unstable();
    pts
  };
  for (fail_at, anchor) in [(12, change), (20, 24)] {
    let (dec, delivered, escalated) = through_a_post_commit_failure(&clip, fail_at);
    assert!(
      dec.is_software(),
      "failing at {fail_at}: the fallback committed its cold decoder"
    );
    assert!(
      !escalated,
      "failing at {fail_at}: the end is clean: {delivered:?}"
    );
    assert_eq!(
      delivered.first(),
      Some(&(clip.packets[anchor].pts().expect("a pts"), false)),
      "failing at {fail_at}: the IDR {anchor}'s picture, out first, closes the gap: {delivered:?}"
    );
    assert_eq!(
      delivered.iter().map(|&(pts, _)| pts).collect::<Vec<_>>(),
      pts_from(anchor),
      "failing at {fail_at}: every picture from the IDR {anchor} on, once, in order"
    );
  }
}

/// LAW (Codex R11, [medium]): **across a new extradata, the session's
/// threads come back at the IDR after the change, and no picture is lost.**
/// The same stream, a probe-era fallback at packet 10 on three threads: the
/// one-thread decoder replays 0 to 9, and the next clean random access point
/// is the IDR 16, read under the two-byte record it carries — the switch
/// fires there, one thread up to it and three from it, and every picture
/// comes out as the same fallback on one thread delivers it. Read under the
/// parameters as opened, no IDR after the change is clean, and the session
/// stays on one thread to the end.
#[test]
fn across_a_new_extradata_the_switch_fires_at_the_idr_after_the_change() {
  let (clip, change) = encode_h264_avcc_whose_length_size_changes(128, 96, 32);
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let (single, _) = threads_through_a_fallback(&clip, 10, crate::Threads::Single);
  assert_eq!(single.len(), clip.packets.len(), "one picture per packet");
  let (shown, threads_after) = threads_through_a_fallback(&clip, 10, crate::Threads::Count(three));
  assert_eq!(
    (threads_after[change - 1], threads_after[change]),
    (Some(core::num::NonZeroU32::MIN), Some(three)),
    "one thread up to the IDR {change}, three from it: {threads_after:?}"
  );
  assert_eq!(shown, single, "the same pictures, in the same order");
}

/// LAW (Codex R11, [medium]): **a decoder opened after a new extradata starts
/// on it, and decodes what a straight decode does.** The same stream, a
/// probe-era fallback at packet 18, past the change, on three threads: the
/// one-thread decoder replays 0 to 17, the change among them, and the session
/// switches at the IDR 24, which carries no record — the decoder opened there
/// starts on the session's parameters. Every picture, 24 to 31 among them,
/// comes out byte-identical to a straight software decode of the stream, in
/// the same order. Opened on the parameters as they were, the decoder reads
/// two-byte fields as four.
#[test]
fn a_decoder_opened_after_a_new_extradata_decodes_what_a_straight_decode_does() {
  let (clip, _) = encode_h264_avcc_whose_length_size_changes(128, 96, 32);
  let (straight, _) = decode_on_software(
    &clip,
    DecoderLimits::default().with_threads(crate::Threads::Single),
  );
  assert_eq!(
    straight.len(),
    clip.packets.len(),
    "the straight decode is whole"
  );
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let (delivered, threads) = decode_through_a_fallback(
    &clip,
    FakeHw::failing(128, 96, 0, 18, FailShape::ProbeEra),
    crate::Threads::Count(three),
  );
  assert_eq!(threads, Some(three), "the session switched to its threads");
  assert_eq!(delivered.len(), straight.len(), "every picture");
  for (index, ((pts, planes), (straight_pts, straight_planes))) in
    delivered.iter().zip(&straight).enumerate()
  {
    assert_eq!(pts, straight_pts, "picture {index}'s timestamp");
    assert!(
      planes.as_ref() == Some(straight_planes),
      "picture {index}'s planes differ from the straight decode's"
    );
  }
}

/// `packet`, which carries a new extradata and two-byte NAL length fields,
/// with its first unit's length field run past the packet: FFmpeg takes it,
/// applies the record, and reports its body invalid.
fn with_corrupt_body(packet: &Packet) -> Packet {
  let extradata = super::new_extradata(packet)
    .expect("the packet carries a new extradata")
    .to_vec();
  let mut payload = packet.data().expect("a payload").to_vec();
  payload[..2].copy_from_slice(&[0xff, 0xff]);
  with_new_extradata(repacked(packet, &payload), &extradata)
}

/// LAW (Codex R12, [medium]; restated by Codex R13, [medium]): **a new
/// extradata on a packet the decoder reported invalid is left unknown, and
/// nothing is anchored or switched under it.** The R11 stream, its IDR 16 —
/// the change — with a body whose first NAL length field runs past the
/// packet: FFmpeg's H.264 decoder takes it, applies the two-byte record and
/// reports the body invalid; but invalid data can as well come from before
/// the decode, which then drops the packet, or, on a frame-threaded decoder,
/// from an earlier packet while this one waits, so it does not say. After a
/// post-commit failure at 12, the IDR 24, which carries no record, is read
/// under no record: nothing anchors, its picture and the rest come out with
/// the gap open, and the end escalates by name. After a probe-era fallback
/// at 10 on three threads, no switch fires at 24: the session decodes on one
/// thread to the end, and the decoder serving, which did apply the record,
/// still decodes 24 to 31 byte-identical to a straight decode of the
/// uncorrupted stream. Taken for the decoder's, invalid data anchored 24 and
/// closed the gap, and the switch fired there.
#[test]
fn a_new_extradata_on_a_packet_reported_invalid_is_left_unknown() {
  let (clip, change) = encode_h264_avcc_whose_length_size_changes(128, 96, 32);
  let mut packets = clip.packets.clone();
  packets[change] = with_corrupt_body(&clip.packets[change]);
  let corrupt = SyntheticClip {
    parameters: clip.parameters.clone(),
    packets,
  };
  let pts_of = |index: usize| clip.packets[index].pts().expect("a pts");

  let (dec, delivered, escalated) = through_a_post_commit_failure(&corrupt, 12);
  assert!(dec.is_software(), "the fallback committed its cold decoder");
  assert_eq!(
    delivered.first().map(|&(pts, _)| pts),
    Some(pts_of(24)),
    "the IDR 24's picture comes out first: {delivered:?}"
  );
  assert!(
    delivered.iter().all(|&(_, open)| open),
    "no picture closes the gap: {delivered:?}"
  );
  assert!(escalated, "the end escalates by name");

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, 0, 10, FailShape::ProbeEra)),
    corrupt.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Count(three));
  let mut dst = crate::empty_owned_video_frame();
  let mut shown: Vec<(i64, Vec<Vec<u8>>)> = Vec::new();
  let mut threads_after = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, shown: &mut Vec<(i64, Vec<Vec<u8>>)>| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => shown.push((
        dst.pts().map_or(i64::MIN, |t| t.pts()),
        dst
          .planes()
          .iter()
          .map(|plane| plane.data_ref().as_ref().to_vec())
          .collect(),
      )),
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(VideoDecodeError::Decode(_)) => {}
      Err(other) => panic!("unexpected: {other:?}"),
    }
  };
  for (index, av_pkt) in corrupt.packets.iter().enumerate() {
    loop {
      match dec.send_packet(&pushed(av_pkt)) {
        Ok(Sent::Accepted) => break,
        Err(VideoDecodeError::Decode(_)) if index == change => break,
        Ok(Sent::MustDrain) => drain(&mut dec, &mut shown),
        Err(other) => panic!("send_packet {index}: {other:?}"),
      }
    }
    threads_after.push(dec.active_threads());
    drain(&mut dec, &mut shown);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  drain(&mut dec, &mut shown);
  assert!(
    threads_after[10..]
      .iter()
      .all(|&threads| threads == Some(core::num::NonZeroU32::MIN)),
    "one thread from the fallback to the end, no switch at the IDR 24: {threads_after:?}"
  );
  let (straight, _) = decode_on_software(
    &clip,
    DecoderLimits::default().with_threads(crate::Threads::Single),
  );
  let from_24 = |pictures: Vec<(i64, Vec<Vec<u8>>)>| -> Vec<(i64, Vec<Vec<u8>>)> {
    pictures
      .into_iter()
      .filter(|&(pts, _)| pts >= pts_of(24))
      .collect()
  };
  let straight = from_24(
    straight
      .into_iter()
      .map(|(pts, planes)| (pts.map_or(i64::MIN, |t| t.pts()), planes))
      .collect(),
  );
  let served = from_24(shown);
  assert_eq!(served.len(), 8, "24 to 31");
  assert!(
    served == straight,
    "the decoder serving decodes 24 to 31 as a straight decode does"
  );
}

/// LAW (Codex R12, [medium]; restated by Codex R13, [medium]): **a replay
/// leaves unknown the new extradata of a packet the decoder reported
/// invalid.** A one-thread decoder on the stream's four-byte record replays
/// 0 to 16, the IDR 16 corrupted as above: the replay stops at 16 with
/// FFmpeg's invalid-data refusal, 16 counted fed — consumed with its error
/// — and the two-byte record it carried is not the replay's: the refusal,
/// which does not say whether the decoder got as far as the record, is kept
/// as the replay's unknown, which the session takes for its own. Taken for
/// the decoder's, the record was the replay's and nothing was unknown.
#[test]
fn a_replay_leaves_unknown_the_new_extradata_of_a_packet_reported_invalid() {
  let (clip, change) = encode_h264_avcc_whose_length_size_changes(128, 96, 32);
  let mut packets = clip.packets[..=change].to_vec();
  packets[change] = with_corrupt_body(&clip.packets[change]);
  let mut sw = super::open_sw_decoder(
    &clip.parameters,
    crate::DecoderLimits::default().with_threads(crate::Threads::Single),
    None,
  )
  .expect("a software decoder");
  let mut queue = super::ReplayQueue::default();
  let mut progress = super::Replay::default();
  let replayed = super::replay_history(
    &mut sw,
    &packets,
    false,
    &mut queue,
    crate::DecoderLimits::default(),
    &clip.parameters,
    &mut progress,
  );
  assert!(
    matches!(
      replayed,
      Err(Error::Ffmpeg(ffmpeg_next::Error::InvalidData))
    ),
    "the replay stops at the corrupted body: {replayed:?}"
  );
  assert_eq!(progress.fed, change + 1, "16 is consumed with its error");
  assert!(
    progress.extradata.is_none(),
    "the two-byte record is not the replay's"
  );
  assert_eq!(
    progress.unknown,
    Some(crate::ExtradataDoubt::Reported(
      ffmpeg_next::Error::InvalidData
    )),
    "the refusal leaves it unknown"
  );
}

/// Drives the R11 stream through a session on `seam`, `before(index, dec)`
/// run ahead of each send, every packet sent and drained — a decode error
/// a send or a picture meets tolerated — and answers the session, the
/// timestamps of the pictures software delivered and whether the gap was
/// still open after each, the errors the sends answered, and whether the
/// end escalated.
#[allow(clippy::type_complexity)]
fn through_the_change(
  clip: &SyntheticClip,
  seam: FakeHw,
  mut before: impl FnMut(usize, &mut FfmpegVideoStreamDecoder),
) -> (
  FfmpegVideoStreamDecoder,
  Vec<(i64, bool)>,
  Vec<(usize, Error)>,
  bool,
) {
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec =
    FfmpegVideoStreamDecoder::from_hw_inner_for_test(Box::new(seam), clip.parameters.clone(), tb)
      .expect("build test decoder");
  let mut dst = crate::empty_owned_video_frame();
  let mut delivered = Vec::new();
  let mut refusals = Vec::new();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, delivered: &mut Vec<(i64, bool)>| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => {
        if dec.is_software() {
          let pts = dst.pts().map_or(i64::MIN, |t| t.pts());
          delivered.push((pts, dec.degraded_resync_pending_for_test()));
        }
      }
      Ok(Received::NeedsInput | Received::Ended) => break false,
      Err(VideoDecodeError::PostCommitNeverResynced(_)) => break true,
      Err(VideoDecodeError::Decode(_)) => {}
      Err(other) => panic!("unexpected: {other:?}"),
    }
  };
  for (index, av_pkt) in clip.packets.iter().enumerate() {
    before(index, &mut dec);
    loop {
      match dec.send_packet(&pushed(av_pkt)) {
        Ok(Sent::Accepted) => break,
        Ok(Sent::MustDrain) => {
          assert!(
            !drain(&mut dec, &mut delivered),
            "no escalation before the end"
          );
        }
        Err(VideoDecodeError::Decode(error)) => {
          refusals.push((index, error));
          break;
        }
        Err(other) => panic!("send_packet {index}: {other:?}"),
      }
    }
    assert!(
      !drain(&mut dec, &mut delivered),
      "no escalation before the end"
    );
  }
  let escalated = match dec.send_eof() {
    Ok(_) => drain(&mut dec, &mut delivered),
    Err(_) => false,
  };
  (dec, delivered, refusals, escalated)
}

/// LAW (Codex R12, [medium]): **a new extradata whose refusal does not say
/// whether the decoder took it leaves the session's unknown, and no anchor
/// is read under it.** After a post-commit failure at 12, the decoder takes
/// the IDR 16 and the session is told the packet failed with an allocation
/// failure (a test seam) — which FFmpeg can report from before a packet is
/// queued or while it is decoded. The IDR 24, carrying no record, is read
/// under neither: nothing anchors, and the end escalates by name
/// (`PostCommitNeverResynced`). Taken for the decoder's, the two-byte record
/// anchored 24 — on a decoder that might still frame the stream by four.
#[test]
fn a_new_extradata_left_in_doubt_anchors_nothing() {
  let (clip, change) = encode_h264_avcc_whose_length_size_changes(128, 96, 32);
  let (dec, delivered, refusals, escalated) = through_the_change(
    &clip,
    FakeHw::failing(128, 96, 12, 12, FailShape::PostCommit),
    |index, dec| {
      if index == change {
        dec.fail_next_packet_with_for_test(ffmpeg_next::Error::Other {
          errno: libc::ENOMEM,
        });
      }
    },
  );
  assert!(dec.is_software(), "the fallback committed its cold decoder");
  assert!(
    refusals.iter().any(|(index, _)| *index == change),
    "the IDR 16 was reported failed: {refusals:?}"
  );
  assert!(
    delivered.iter().all(|&(_, open)| open),
    "no picture closed the gap: {delivered:?}"
  );
  assert!(escalated, "the end escalates by name");
}

/// LAW (Codex R12, [medium]; widened by Codex R13, [medium]): **a decoder
/// the session must open on unknown extradata is refused by name.** The
/// hardware takes 0 to 15 and refuses the IDR 16, the change, with an
/// allocation failure, then again with invalid data — neither says whether
/// it took the packet. At 20 it fails post-commit: the cold software decoder
/// the fallback would open on the session's parameters is refused as
/// `ExtradataUnknown`, naming the refusal; nothing is committed, and the
/// session stays on the hardware with no gap open. Taken for the
/// hardware's, the two-byte record opened the cold decoder.
#[test]
fn a_fallback_onto_unknown_extradata_is_refused_by_name() {
  let (clip, change) = encode_h264_avcc_whose_length_size_changes(128, 96, 32);
  let enomem = ffmpeg_next::Error::Other {
    errno: libc::ENOMEM,
  };
  for refusal in [enomem, ffmpeg_next::Error::InvalidData] {
    let (dec, _, refusals, _) = through_the_change(
      &clip,
      FakeHw::failing(128, 96, 20, 20, FailShape::PostCommit).refusing(change, refusal),
      |_, _| {},
    );
    let refused: Vec<String> = refusals
      .iter()
      .map(|(index, error)| format!("{index}: {error:?}"))
      .collect();
    assert!(
      matches!(refusals.first(), Some((16, Error::Ffmpeg(raw))) if *raw == refusal),
      "{refusal:?}: the hardware refused the IDR 16: {refused:?}"
    );
    assert!(
      refusals.iter().any(|(index, error)| *index == 20
        && matches!(error, Error::ExtradataUnknown(unknown)
          if unknown.doubt() == crate::ExtradataDoubt::Reported(refusal))),
      "{refusal:?}: the fallback at 20 is refused by name, naming the refusal: {refused:?}"
    );
    assert!(!dec.is_software(), "{refusal:?}: nothing was committed");
    assert!(
      !dec.degraded_resync_pending_for_test(),
      "{refusal:?}: and no gap is open"
    );
  }
}

/// A `libx264` clip in closed GOPs — an IDR every 8 frames, two B-frames
/// between references — packed `avcC` with four-byte NAL length fields
/// throughout, its codec parameters carrying the record. Answers the clip
/// and its SPS and PPS.
fn encode_h264_avcc(width: u32, height: u32, frames: usize) -> (SyntheticClip, Vec<u8>, Vec<u8>) {
  let annexb = encode_h264_with_extradata(width, height, frames);
  let (sps, pps) = sps_and_pps(&annexb);
  let packets = annexb
    .packets
    .iter()
    .map(|packet| {
      let units = annexb_units(packet.data().expect("a payload"));
      repacked(packet, &length_prefixed(&units, 4))
    })
    .collect();
  let mut parameters = annexb.parameters.clone();
  set_extradata(&mut parameters, &avcc(&sps, &pps, 4));
  (
    SyntheticClip {
      parameters,
      packets,
    },
    sps,
    pps,
  )
}

/// LAW (Codex R12, [medium]): **a new extradata the parameters' ceiling
/// cannot hold is refused by name before the decoder takes it, and nothing
/// changes.** A four-byte `avcC` stream, a session whose codec parameters
/// may hold 16 bytes more than they open with, its packet 5 — no keyframe —
/// carrying as `AV_PKT_DATA_NEW_EXTRADATA` the same record with nine more
/// copies of its PPS. On a probe-era fallback at 3 on three
/// threads, the send of 5 is refused as `ParametersTooLarge`, naming the
/// bytes the parameters would hold and the ceiling: no decoder took the
/// packet, the parameters keep their record, the session stays on one
/// thread; sent without 5, the stream switches to three threads at the IDR
/// 8, the decoder that opens there within the ceiling. On the hardware,
/// probing, the same send is refused before the hardware sees it. Taken,
/// the record carried the parameters past the ceiling, and the restart at
/// 8 — the one-thread decoder already drained and closed — was refused as
/// `ParametersTooLarge`, as was every send after it.
#[test]
fn a_new_extradata_past_the_parameters_ceiling_is_refused_before_the_decoder_takes_it() {
  let (clip, sps, pps) = encode_h264_avcc(128, 96, 16);
  let record = extradata_of(&clip.parameters);
  // SAFETY: the clip's live parameters, measured; nothing is allocated.
  let opened = unsafe { crate::extras::measure_parameters(clip.parameters.as_ptr()) }
    .and_then(|footprint| footprint.total())
    .expect("parameters this crate measures");
  let ceiling = opened + 16;
  let mut padded = record.clone();
  let pps_count = 8 + sps.len();
  let growth = 9 * (2 + pps.len());
  padded[pps_count] = 10;
  for _ in 0..9 {
    padded.extend_from_slice(&u16::try_from(pps.len()).expect("a short PPS").to_be_bytes());
    padded.extend_from_slice(&pps);
  }
  let mut packets = clip.packets.clone();
  packets[5] = with_new_extradata(packets[5].clone(), &padded);
  assert!(!packets[5].is_key(), "5 is no keyframe");
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, 0, 3, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Count(three))
  .with_max_codec_parameter_bytes_for_test(ceiling);
  let mut dst = crate::empty_owned_video_frame();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) | Err(VideoDecodeError::Decode(_)) => {}
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(other) => panic!("unexpected: {other:?}"),
    }
  };
  let mut too_large = Vec::new();
  for (index, av_pkt) in packets.iter().enumerate() {
    let taken = super::live_sw::sent();
    loop {
      match dec.send_packet(&pushed(av_pkt)) {
        Ok(Sent::Accepted) => break,
        Ok(Sent::MustDrain) => drain(&mut dec),
        Err(VideoDecodeError::Decode(Error::ParametersTooLarge(refused))) => {
          too_large.push((index, refused.bytes(), refused.limit()));
          if index == 5 {
            assert_eq!(super::live_sw::sent(), taken, "no decoder took 5");
            assert_eq!(
              extradata_of(&dec.parameters),
              record,
              "the parameters keep their record"
            );
            assert_eq!(
              dec.active_threads(),
              Some(core::num::NonZeroU32::MIN),
              "the session is still on one thread"
            );
          }
          break;
        }
        Err(VideoDecodeError::Decode(_)) => break,
        Err(other) => panic!("send_packet {index}: {other:?}"),
      }
    }
    drain(&mut dec);
  }
  assert_eq!(
    too_large,
    vec![(5, ceiling - 16 + growth, ceiling)],
    "5 alone is refused by name, before the decoder takes it"
  );
  assert_eq!(
    dec.active_threads(),
    Some(three),
    "the switch at 8 opened within the ceiling"
  );

  let mut hw = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::never_failing(128, 96)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_max_codec_parameter_bytes_for_test(ceiling);
  for av_pkt in &packets[..5] {
    crate::accepted(hw.send_packet(&pushed(av_pkt)), "send_packet");
  }
  assert!(
    matches!(
      hw.send_packet(&pushed(&packets[5])),
      Err(VideoDecodeError::Decode(Error::ParametersTooLarge(_)))
    ),
    "the hardware send is refused by name"
  );
  assert!(
    hw.probe_extradata.is_none() && extradata_of(&hw.parameters) == record,
    "nothing changes"
  );
}

/// LAW (Codex R14, [medium]): **a packet no decoder took leaves the
/// session's sticky readings as they were.** A four-byte `avcC` x264 stream
/// (High profile), a session whose codec parameters may hold 16 bytes more
/// than they open with, its packet 5 — no keyframe — carrying as
/// `AV_PKT_DATA_NEW_EXTRADATA` the stream's record restated as Baseline
/// without `constraint_set1_flag`, a profile that permits arbitrary slice
/// order, with nine more copies of its PPS: past the ceiling. On the
/// software road (a post-commit failure at 3) and on the hardware (which
/// fails post-commit at the IDR 8), 5 is refused as `ParametersTooLarge`
/// before any decoder sees it, the session reads no arbitrary slice order,
/// and the IDR 8 anchors the resync: a picture out after it closes the gap,
/// and the end is clean. Read before the refusal, the record's profile
/// stuck: nothing anchored, and the end escalated by name.
#[test]
fn a_packet_no_decoder_took_leaves_the_sticky_readings_as_they_were() {
  let (clip, sps, pps) = encode_h264_avcc(128, 96, 16);
  let record = extradata_of(&clip.parameters);
  // SAFETY: the clip's live parameters, measured; nothing is allocated.
  let opened = unsafe { crate::extras::measure_parameters(clip.parameters.as_ptr()) }
    .and_then(|footprint| footprint.total())
    .expect("parameters this crate measures");
  let ceiling = opened + 16;
  let mut baseline = record.clone();
  baseline[1] = 66;
  baseline[2] = 0x80;
  baseline[8 + sps.len()] = 10;
  for _ in 0..9 {
    baseline.extend_from_slice(&u16::try_from(pps.len()).expect("a short PPS").to_be_bytes());
    baseline.extend_from_slice(&pps);
  }
  assert!(
    super::access::KeyframeRule::of(crate::CodecId::H264.raw(), &baseline).permits_aso(),
    "the restated record permits arbitrary slice order"
  );
  let mut packets = clip.packets.clone();
  packets[5] = with_new_extradata(packets[5].clone(), &baseline);
  assert!(
    !packets[5].is_key() && packets[8].is_key(),
    "5 is no keyframe, 8 is the IDR"
  );
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  // The hardware's seventh send is the packet 8: 5 never reaches it.
  for (name, fail_at) in [("the software road", 3), ("the hardware", 7)] {
    let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
      Box::new(FakeHw::failing(
        128,
        96,
        fail_at,
        fail_at,
        FailShape::PostCommit,
      )),
      clip.parameters.clone(),
      tb,
    )
    .expect("build test decoder")
    .with_max_codec_parameter_bytes_for_test(ceiling);
    let mut dst = crate::empty_owned_video_frame();
    let mut delivered = Vec::new();
    let mut refusals = Vec::new();
    for (index, av_pkt) in packets.iter().enumerate() {
      loop {
        match dec.send_packet(&pushed(av_pkt)) {
          Ok(Sent::Accepted) => break,
          Ok(Sent::MustDrain) => {
            assert!(
              !drained(&mut dec, &mut dst).1,
              "{name}: no escalation before the end"
            );
          }
          Err(VideoDecodeError::Decode(error)) => {
            refusals.push((index, format!("{error:?}")));
            break;
          }
          Err(other) => panic!("{name}: send_packet {index}: {other:?}"),
        }
      }
      let (shown, escalated) = drained(&mut dec, &mut dst);
      assert!(!escalated, "{name}: no escalation before the end");
      if dec.is_software() {
        delivered.extend(shown);
      }
    }
    crate::accepted(dec.send_eof(), "send_eof");
    let (tail, escalated) = drained(&mut dec, &mut dst);
    delivered.extend(tail);
    assert!(
      matches!(refusals.as_slice(), [(5, refused)] if refused.starts_with("ParametersTooLarge")),
      "{name}: 5 alone is refused, before any decoder sees it: {refusals:?}"
    );
    assert!(dec.is_software(), "{name}: the fallback committed");
    assert!(
      !escalated && delivered.iter().any(|&(_, open)| !open),
      "{name}: the IDR 8 anchors and the gap closes: {delivered:?}"
    );
    assert!(
      !dec.h264_aso,
      "{name}: no arbitrary slice order is read off a packet no decoder took"
    );
  }
}

/// LAW (Codex R6 row 1, [high]): **a stale key flag anchors nothing.** An
/// H.264 stream whose SPS and PPS ride in its codec parameters, so the cold
/// software decoder takes P slices: the hardware fails post-commit on a
/// P-frame and the decoder decodes across the gap. A P-frame packet flagged
/// a keyframe — a stale flag — is taken and anchors nothing: its first
/// picture is a P slice, which no rule of the bitstream makes a random-access
/// one, and the pictures it leads are no part of the reorder bound. The IDR
/// after it anchors the resync.
#[test]
fn a_stale_key_flag_anchors_nothing() {
  let clip = encode_h264_with_extradata(128, 96, 40);
  let second = keyframe_after(&clip, 0);
  let third = keyframe_after(&clip, second);
  let at = second + 1;
  assert!(
    !clip.packets[at].is_key() && !clip.packets[third - 1].is_key(),
    "P and B packets around the GOP boundary"
  );
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, at, at, FailShape::PostCommit)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder");
  let mut dst = crate::empty_owned_video_frame();
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) | Err(VideoDecodeError::Decode(_)) => {}
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(other) => panic!("unexpected: {other:?}"),
    }
  };
  for av_pkt in &clip.packets[..third - 1] {
    match dec.send_packet(&pushed(av_pkt)) {
      Ok(Sent::Accepted) | Err(VideoDecodeError::Decode(_)) => {}
      other => panic!("send_packet: {other:?}"),
    }
    drain(&mut dec);
  }
  assert!(dec.degraded_resync_pending_for_test(), "the gap is open");

  let original = &clip.packets[third - 1];
  let mut stale = Packet::copy(original.data().expect("a payload"));
  stale.set_pts(original.pts());
  stale.set_dts(original.dts());
  stale.set_flags(ffmpeg_next::packet::Flags::KEY);
  crate::accepted(
    dec.send_packet(&pushed(&stale)),
    "the P-frame flagged a keyframe",
  );
  assert!(
    !dec.degraded_anchored_for_test(),
    "a stale key flag anchors nothing"
  );
  drain(&mut dec);
  crate::accepted(dec.send_packet(&pushed(&clip.packets[third])), "the IDR");
  assert!(
    dec.degraded_anchored_for_test(),
    "the IDR anchors the resync"
  );
}

/// Rewrites the VOL header in an MPEG-4 part 2 `packet` (start code
/// `00 00 01 2x`) to declare low delay, answering whether it found one.
/// FFmpeg's encoder writes `random_accessible_vol` (1 bit), the object type
/// (8), an object layer identifier (1, set: its version 4 and priority 3),
/// `aspect_ratio_info` (4; 15 adds 16 bits of ratio), `vol_control_parameters`
/// (1, set), `chroma_format` (2), then `low_delay`.
fn declare_low_delay(packet: &mut [u8]) -> bool {
  let Some(at) = packet
    .windows(4)
    .position(|w| w[..3] == [0, 0, 1] && w[3] & 0xf0 == 0x20)
  else {
    return false;
  };
  let bit = |packet: &[u8], index: usize| packet[index / 8] & (0x80 >> (index % 8)) != 0;
  let mut index = (at + 4) * 8 + 1 + 8;
  if bit(packet, index) {
    index += 4 + 3;
  }
  index += 1;
  let aspect = (0..4).fold(0u8, |value, offset| {
    (value << 1) | u8::from(bit(packet, index + offset))
  });
  index += 4 + if aspect == 15 { 16 } else { 0 };
  if !bit(packet, index) {
    return false;
  }
  index += 1 + 2;
  packet[index / 8] |= 0x80 >> (index % 8);
  true
}

/// An MPEG-4 part 2 clip whose first B picture leads its second keyframe,
/// in a stream that declares low delay. Frames 1 to 4 are forced P, so no B
/// picture comes before frame 5, which the encoder makes a B picture before
/// the keyframe at 6 — an open GOP there, its B picture referencing frame 4
/// — and every VOL header is rewritten to declare low delay, as encoders
/// that pack B pictures in low-delay streams do: FFmpeg's decoder reports no
/// reordering until it meets that B picture, after the keyframe.
fn encode_mpeg4_first_b_gop_at_a_keyframe(width: u32, height: u32, frames: usize) -> SyntheticClip {
  use ffmpeg_next as ff;
  ff::init().expect("ffmpeg init");
  let codec = ff::codec::encoder::find(ff::codec::Id::MPEG4).expect("mpeg4 encoder present");
  let clip = encode_clip_typed(
    codec,
    width,
    height,
    frames,
    ff::Dictionary::new(),
    |enc| {
      enc.set_gop(6);
      enc.set_max_b_frames(1);
      enc.set_bit_rate(500_000);
    },
    |index| {
      if (1..=4).contains(&index) {
        ff::picture::Type::P
      } else {
        ff::picture::Type::None
      }
    },
  );
  let mut declared = 0;
  let packets = clip
    .packets
    .iter()
    .map(|original| {
      let mut copy = Packet::copy(original.data().expect("a payload"));
      if declare_low_delay(copy.data_mut().expect("a writable copy")) {
        declared += 1;
      }
      copy.set_pts(original.pts());
      copy.set_dts(original.dts());
      copy.set_flags(original.flags());
      copy
    })
    .collect();
  assert!(
    declared >= 2,
    "every keyframe carries a VOL header to rewrite"
  );
  SyntheticClip {
    parameters: clip.parameters,
    packets,
  }
}

/// LAW (Codex R6 row 2, [high]): **a codec whose keyframes this crate
/// cannot prove clean returns to the session's threads only at a seek, and
/// loses nothing.** An MPEG-4 part 2 stream whose first B picture leads its
/// second keyframe — an open GOP there — in a stream declaring low delay:
/// before that keyframe the decoder reports `has_b_frames` 0, and a switch
/// trusting it would close the decoder holding frame 4 and open one at the
/// keyframe that cannot decode the B picture leading it. A probe-era
/// fallback on three threads stays on one through the stream, and delivers
/// every picture the same fallback on one thread delivers.
#[test]
fn a_codec_whose_keyframes_cannot_be_proved_clean_switches_only_at_a_seek() {
  let clip = encode_mpeg4_first_b_gop_at_a_keyframe(96, 64, 24);
  let keyframe = keyframe_after(&clip, 0);
  assert!(
    clip.packets[keyframe + 1].pts() < clip.packets[keyframe].pts(),
    "a B picture leads the second keyframe"
  );
  let (single, _) = threads_through_a_fallback(&clip, 2, crate::Threads::Single);

  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(16, 16, 0, 2, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Count(three));
  let mut dst = crate::empty_owned_video_frame();
  let mut shown = Vec::new();
  let mut threads = Vec::new();
  for (index, av_pkt) in clip.packets.iter().enumerate() {
    if index == keyframe {
      assert_eq!(
        dec.reorder_for_test(),
        0,
        "the decoder reports no reordering before the keyframe"
      );
    }
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    threads.push(dec.active_threads());
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      shown.push(dst.pts());
    }
  }
  crate::accepted(dec.send_eof(), "send_eof");
  while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
    shown.push(dst.pts());
  }
  assert_eq!(
    shown, single,
    "every picture the one-thread fallback delivers"
  );
  assert!(
    threads
      .iter()
      .all(|&count| count == Some(core::num::NonZeroU32::MIN)),
    "no switch mid-stream: {threads:?}"
  );
}

/// A probe-era fallback raised by `send_eof` on an MPEG-4 clip with
/// B-frames — its decoder holds a picture back until it is told the end —
/// replayed through a queue whose budget holds two and a half pictures, so
/// the end is fed in a later round; the ends software decoders are sent are
/// answered first by `answers`. A caller that obeys every `MustDrain` sends
/// the end until it is taken, then drains to the end. Answers the clip's
/// length, the pictures delivered, the decode errors met, and the fault a
/// sent end met, if any.
fn an_end_answered(
  answers: &[ffmpeg_next::Error],
) -> (usize, usize, usize, Option<VideoDecodeError>) {
  let (w, h) = (96u32, 64u32);
  let clip = encode_mpeg4_with_b_frames(w, h, 11);
  let budget = picture_bytes(&clip) * 5 / 2;
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing_at_eof(w, h)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Single)
  .with_max_replay_bytes_for_test(budget);
  for av_pkt in &clip.packets {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
  }
  super::eof_script::push(answers.iter().copied());
  let mut dst = crate::empty_owned_video_frame();
  let (mut pictures, mut errors) = (0usize, 0usize);
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, pictures: &mut usize, errors: &mut usize| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => *pictures += 1,
      Err(VideoDecodeError::Decode(_)) => *errors += 1,
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(other) => panic!("draining: {other:?}"),
    }
  };
  let mut fault = None;
  for _ in 0..32 {
    match dec.send_eof() {
      Ok(Sent::Accepted) => break,
      Ok(Sent::MustDrain) => drain(&mut dec, &mut pictures, &mut errors),
      Err(error) => {
        fault = Some(error);
        break;
      }
    }
  }
  drain(&mut dec, &mut pictures, &mut errors);
  (clip.packets.len(), pictures, errors, fault)
}

/// LAW (Codex R6 row 3, [high]): **an end the decoder answers with back
/// pressure until the retries run out is still owed, and is sent again.**
/// The replay's end meets `EAGAIN` seventeen times — past the sixteen one
/// round tries — and the round reports the back pressure with the end
/// still pending: the session's end is not committed, the caller's
/// `send_eof` answers `MustDrain`, and sent again, the end reaches the
/// decoder, which gives up the picture it held. Taken as told, it was
/// committed untold: the session drained a decoder that never ended, and
/// the held picture was lost.
#[test]
fn an_end_held_back_past_the_retries_is_owed_and_sent_again() {
  let eagain = ffmpeg_next::Error::Other {
    errno: ffmpeg_next::error::EAGAIN,
  };
  let (length, pictures, errors, fault) = an_end_answered(&[eagain; 17]);
  assert!(fault.is_none(), "no end faulted: {fault:?}");
  assert_eq!(errors, 1, "the back pressure, reported once");
  assert_eq!(pictures, length, "every picture, the held one too");
}

/// LAW (Codex R6 row 3, [high]): **an end the decoder refuses is still
/// owed, and is sent again.** The replay's end is refused once: the round
/// reports the refusal with the end still pending, and the caller's retry
/// sends it — the decoder takes it and gives up the picture it held. Taken
/// as told, the refusal committed the session's end over a decoder that
/// never had one, and the held picture was lost.
#[test]
fn a_refused_end_is_owed_and_sent_again() {
  let (length, pictures, errors, fault) = an_end_answered(&[ffmpeg_next::Error::InvalidData]);
  assert!(fault.is_none(), "no end faulted: {fault:?}");
  assert_eq!(errors, 1, "the refusal, reported once");
  assert_eq!(pictures, length, "every picture, the held one too");
}

/// LAW (Codex R6 row 3, [high]): **a switch whose old decoder refuses the
/// end loses nothing.** A closed-GOP H.264 stream with B-frames falls back
/// on its probe and switches to three threads at the next IDR, draining the
/// one-thread decoder first; that decoder refuses the end once. The switch
/// waits with the refusal reported and the end still owed to the decoder
/// being switched away from, sends it again, and drains it: every picture
/// comes out, in the order the same fallback on one thread delivers. Taken as
/// told, the refused end let the switch close a decoder still holding the
/// pictures its B-frames kept back.
#[test]
fn a_switch_whose_old_decoder_refuses_the_end_loses_nothing() {
  let clip = encode_h264_closed_gops(128, 96, 40);
  let idr = keyframe_after(&clip, 3);
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let (single, _) = threads_through_a_fallback(&clip, 3, crate::Threads::Single);

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(16, 16, 0, 3, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Count(three));
  let mut dst = crate::empty_owned_video_frame();
  let mut shown = Vec::new();
  let mut errors = 0usize;
  let mut drain = |dec: &mut FfmpegVideoStreamDecoder, shown: &mut Vec<_>, errors: &mut usize| loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => shown.push(dst.pts()),
      Err(VideoDecodeError::Decode(_)) => *errors += 1,
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(other) => panic!("draining: {other:?}"),
    }
  };
  for (index, av_pkt) in clip.packets.iter().enumerate() {
    if index == idr {
      super::eof_script::push([ffmpeg_next::Error::InvalidData]);
    }
    loop {
      match dec.send_packet(&pushed(av_pkt)).expect("send_packet") {
        Sent::Accepted => break,
        Sent::MustDrain => drain(&mut dec, &mut shown, &mut errors),
      }
    }
    drain(&mut dec, &mut shown, &mut errors);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  drain(&mut dec, &mut shown, &mut errors);

  assert_eq!(errors, 1, "the refusal, reported once");
  assert_eq!(dec.active_threads(), Some(three), "the session switched");
  assert_eq!(shown, single, "no picture lost, none moved");
}

/// An H.263 clip — a codec FFmpeg decodes serially, with neither frame nor
/// slice threading — at 128x96, a keyframe every `gop` frames.
fn encode_h263(frames: usize, gop: u32) -> SyntheticClip {
  use ffmpeg_next as ff;
  ff::init().expect("ffmpeg init");
  let codec = ff::codec::encoder::find(ff::codec::Id::H263).expect("h263 encoder present");
  encode_clip(codec, 128, 96, frames, ff::Dictionary::new(), |enc| {
    enc.set_gop(gop);
    enc.set_max_b_frames(0);
    enc.set_bit_rate(200_000);
  })
}

/// LAW (Codex R6 row 5, [medium]): **a serial codec reports one thread,
/// and schedules no switch.** H.263 decodes on one thread whatever is
/// asked. Opened on the software road under `Threads::Count(8)`, the
/// session reports the threads its decoder decodes on — one, read off what
/// is active, not the eight asked for. After a fallback on the same request,
/// a seek makes the next keyframe a switch point; none is taken: reopening
/// a decoder that cannot thread would only drain and close the one serving,
/// so no switch is scheduled, and the session stays on its one thread.
#[test]
fn a_serial_codec_reports_one_thread_and_schedules_no_switch() {
  let clip = encode_h263(24, 6);
  let eight = core::num::NonZeroU32::new(8).expect("nonzero");
  let (pictures, threads) = decode_on_software(
    &clip,
    crate::DecoderLimits::default().with_threads(crate::Threads::Count(eight)),
  );
  assert_eq!(pictures.len(), clip.packets.len(), "the clip decodes");
  assert_eq!(
    threads,
    Some(core::num::NonZeroU32::MIN),
    "the decoder decodes on one thread"
  );

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(16, 16, 0, 3, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Count(eight));
  for _ in 0..2 {
    for av_pkt in &clip.packets {
      crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
      drain_ready(&mut dec);
      assert_eq!(
        dec.active_threads(),
        Some(core::num::NonZeroU32::MIN),
        "one thread"
      );
    }
    // A seek: the first keyframe after it is a switch point.
    dec.flush().expect("a flush");
  }
  assert!(dec.is_software(), "the fallback committed");
  assert_eq!(dec.threaded_opens_for_test(), 0, "no switch was scheduled");
}

/// A Ut Video clip — a codec that codes every picture alone, its decoder
/// frame-threaded — at 96x64.
fn encode_utvideo(frames: usize) -> SyntheticClip {
  use ffmpeg_next as ff;
  ff::init().expect("ffmpeg init");
  let codec = ff::codec::encoder::find_by_name("utvideo").expect("utvideo encoder present");
  encode_clip(codec, 96, 64, frames, ff::Dictionary::new(), |_| {})
}

/// An Apple ProRes clip — a codec that codes every picture alone, its
/// decoder frame- and slice-threaded — at 96x64, in the 10-bit 4:2:2 its
/// encoder takes.
fn encode_prores(frames: usize) -> SyntheticClip {
  use ffmpeg_next as ff;
  ff::init().expect("ffmpeg init");
  let codec = ff::codec::encoder::find_by_name("prores_ks").expect("prores_ks encoder present");
  let mut enc = ff::codec::context::Context::new_with_codec(codec)
    .encoder()
    .video()
    .expect("video encoder context");
  enc.set_width(96);
  enc.set_height(64);
  enc.set_format(ff::format::Pixel::YUV422P10LE);
  enc.set_time_base(ff::Rational::new(1, 25));
  let mut opened = enc
    .open_as_with(codec, ff::Dictionary::new())
    .expect("open encoder");
  let parameters = ff::codec::Parameters::from(&opened);
  let drain = |opened: &mut ff::codec::encoder::Video, out: &mut Vec<Packet>| {
    let mut pkt = Packet::empty();
    while opened.receive_packet(&mut pkt).is_ok() {
      out.push(core::mem::replace(&mut pkt, Packet::empty()));
    }
  };
  let mut frame = ff::frame::Video::new(ff::format::Pixel::YUV422P10LE, 96, 64);
  let mut packets = Vec::new();
  for i in 0..frames {
    // Little-endian 10-bit samples; a chroma plane is half as wide.
    for (plane, samples) in [(0, 96), (1, 48), (2, 48)] {
      let stride = frame.stride(plane);
      let data = frame.data_mut(plane);
      for y in 0..64 {
        for x in 0..samples {
          let sample = ((x + y + i * 4 + plane * 128) & 0x3ff) as u16;
          data[y * stride + 2 * x..][..2].copy_from_slice(&sample.to_le_bytes());
        }
      }
    }
    frame.set_pts(Some(i as i64));
    opened.send_frame(&frame).expect("send_frame");
    drain(&mut opened, &mut packets);
  }
  opened.send_eof().expect("encoder send_eof");
  drain(&mut opened, &mut packets);
  SyntheticClip {
    parameters,
    packets,
  }
}

/// Feeds `clip` through a probe-era fallback at packet 3 on three threads,
/// and checks the session is back on them from the very next packet — the
/// one the hardware refused — and that every picture comes out as the same
/// fallback on one thread delivers it.
fn switches_at_the_very_next_packet(clip: &SyntheticClip, what: &str) {
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let (single, _) = threads_through_a_fallback(clip, 3, crate::Threads::Single);

  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(16, 16, 0, 3, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Count(three));
  let mut dst = crate::empty_owned_video_frame();
  let mut threaded = Vec::new();
  let mut threads = Vec::new();
  for av_pkt in &clip.packets {
    // The switch drains into the queue the replay filled, so it waits for
    // the caller to empty it: the packet is sent again after the drain.
    loop {
      match dec.send_packet(&pushed(av_pkt)).expect("send_packet") {
        Sent::Accepted => break,
        Sent::MustDrain => {
          while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
            threaded.push(dst.pts());
          }
        }
      }
    }
    threads.push(dec.active_threads());
    while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
      threaded.push(dst.pts());
    }
  }
  crate::accepted(dec.send_eof(), "send_eof");
  while let Received::Frame = dec.receive_frame(&mut dst).expect("receive_frame") {
    threaded.push(dst.pts());
  }
  assert_eq!(threaded, single, "{what}: no picture lost, none moved");
  assert_eq!(
    threads[..3],
    [Some(core::num::NonZeroU32::MIN); 3],
    "{what}: the hardware before the fallback reports one"
  );
  assert!(
    threads[3..].iter().all(|&count| count == Some(three)),
    "{what}: three threads from the very next packet: {threads:?}"
  );
}

/// LAW (the coordinator's row 6): **a codec that codes every picture alone
/// switches at the very next packet, and loses nothing.** ProRes's and Ut
/// Video's descriptors carry `AV_CODEC_PROP_INTRA_ONLY`: no picture
/// references another, so every packet is a clean switch point and a
/// resync anchor. A probe-era fallback at packet 3 on three threads replays
/// the history on one thread and switches at the packet it was refused —
/// the very next — on three from there; every picture comes out as the
/// same fallback on one thread delivers it. The same holds where a
/// container flags only the first packet key: the rule takes every packet,
/// its flag or not.
#[test]
fn an_intra_only_codec_switches_at_the_very_next_packet_and_loses_nothing() {
  use ffmpeg_next::ffi::AVCodecID;
  for (codec, clip) in [
    (AVCodecID::AV_CODEC_ID_PRORES, encode_prores(20)),
    (AVCodecID::AV_CODEC_ID_UTVIDEO, encode_utvideo(20)),
  ] {
    assert_eq!(
      super::access::KeyframeRule::of(codec as i32, &[]),
      super::access::KeyframeRule::IntraOnly,
      "{codec:?}: the descriptor says intra-only"
    );
    assert!(
      clip.packets.iter().all(Packet::is_key),
      "{codec:?}: FFmpeg flags every packet of an intra-only codec key"
    );
    switches_at_the_very_next_packet(&clip, &format!("{codec:?}"));

    let mut packets = clip.packets.clone();
    for pkt in &mut packets[1..] {
      pkt.set_flags(pkt.flags() - ffmpeg_next::packet::Flags::KEY);
    }
    let first_only = SyntheticClip {
      parameters: clip.parameters.clone(),
      packets,
    };
    switches_at_the_very_next_packet(
      &first_only,
      &format!("{codec:?}, the first packet alone key"),
    );
  }
}

/// An H.264 clip from `libx264`'s Baseline profile — an IDR every 8 frames,
/// no B-frames, its SPS and PPS repeated before every IDR — as encoded,
/// Constrained Baseline (`constraint_set1_flag` set), or, with
/// `arbitrary_slice_order`, with that flag cleared in every SPS: a stream
/// whose SPS permits arbitrary slice order.
fn encode_h264_baseline(
  width: u32,
  height: u32,
  frames: usize,
  arbitrary_slice_order: bool,
) -> SyntheticClip {
  use ffmpeg_next as ff;
  ff::init().expect("ffmpeg init");
  let codec = ff::codec::encoder::find_by_name("libx264").expect("libx264 is linked");
  let mut options = ff::Dictionary::new();
  options.set("profile", "baseline");
  options.set(
    "x264-params",
    "keyint=8:min-keyint=8:scenecut=0:log-level=error",
  );
  let clip = encode_clip(codec, width, height, frames, options, |_| {});
  if !arbitrary_slice_order {
    return clip;
  }
  let packets = clip
    .packets
    .iter()
    .map(|packet| {
      let units = annexb_units(packet.data().expect("a payload"));
      let mut bytes = Vec::new();
      for unit in units {
        let mut unit = unit.to_vec();
        if unit[0] & 0x1f == 7 {
          assert_eq!(unit[1], 66, "a Baseline SPS");
          assert_ne!(unit[2] & 0x40, 0, "as encoded, Constrained Baseline");
          unit[2] &= !0x40;
        }
        bytes.extend_from_slice(&[0, 0, 0, 1]);
        bytes.extend_from_slice(&unit);
      }
      repacked(packet, &bytes)
    })
    .collect();
  SyntheticClip {
    parameters: clip.parameters.clone(),
    packets,
  }
}

/// LAW (Codex R13, [high]): **a stream whose sequence parameter set permits
/// arbitrary slice order never switches at a keyframe and never anchors a
/// resync.** An x264 Baseline stream, its SPS before every IDR, as encoded
/// (Constrained Baseline) and with `constraint_set1_flag` cleared in every
/// SPS. On a probe-era fallback at 3 on three threads the first switches at
/// its IDR 8 and the second stays on one thread to the end; through a
/// post-commit failure at that IDR — its SPS and PPS with it, what a cold
/// decoder of this stream needs — the first resyncs at 8 and ends clean,
/// the second anchors nothing and its end escalates by name. Reading the MB-0
/// slice alone, the second switched and resynced at slices its stream's
/// order does not vouch for.
#[test]
fn a_stream_permitting_arbitrary_slice_order_never_switches_or_anchors() {
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  for (arbitrary, name) in [(false, "Constrained Baseline"), (true, "Baseline")] {
    let clip = encode_h264_baseline(128, 96, 16, arbitrary);
    assert!(clip.packets[8].is_key(), "an IDR at 8");
    let (_, threads_after) = threads_through_a_fallback(&clip, 3, crate::Threads::Count(three));
    assert_eq!(
      threads_after[8],
      if arbitrary {
        Some(core::num::NonZeroU32::MIN)
      } else {
        Some(three)
      },
      "{name}: the switch at the IDR 8 fires {}: {threads_after:?}",
      !arbitrary
    );
    let (_, delivered, escalated) = through_a_post_commit_failure(&clip, 8);
    assert_eq!(
      escalated, arbitrary,
      "{name}: the end escalates {arbitrary}: {delivered:?}"
    );
    assert_eq!(
      delivered.iter().any(|&(_, open)| !open),
      !arbitrary,
      "{name}: a picture closes the gap {}: {delivered:?}",
      !arbitrary
    );
  }
}

/// LAW (Codex R13, [medium]; restated by pre-R14 row 3): **only a refusal
/// this crate minted while the packet's own picture was allocated says a
/// decoder took the packet.** Invalid data, `AVERROR_PATCHWELCOME`, an
/// allocation failure and an invalid argument leave a packet's new
/// extradata unknown, on the software road and the hardware's alike,
/// whatever the decoder: none ties an error to the packet just submitted. A
/// frame or a coded surface refused over its ceiling says the packet was
/// decoded where the decoder decodes, inside a submission, the packet it
/// queues — a software decoder of libavcodec's own on one thread — and
/// nothing on a frame-threaded one, whose picture may be an earlier
/// packet's, nor on the hardware, whose funnel keeps no raw error and whose
/// probe may replay a history inside one submission: there it is unknown,
/// minted. Back pressure and the end say the packet was not taken, whatever
/// a callback left latched. FFmpeg's `h264` opened on one thread decodes in
/// step; on three, frame-threaded, it does not, nor does a decoder that
/// reads as wrapped. Taken for the decoder's, invalid data installed an
/// extradata the decoder may never have applied.
#[test]
fn only_a_refusal_minted_while_its_picture_was_allocated_says_a_packet_was_taken() {
  use super::{Taken, taken_by_hardware_despite, taken_despite};
  let eagain = ffmpeg_next::Error::Other {
    errno: ffmpeg_next::error::EAGAIN,
  };
  let einval = ffmpeg_next::Error::Other {
    errno: libc::EINVAL,
  };
  for raw in [
    ffmpeg_next::Error::InvalidData,
    ffmpeg_next::Error::PatchWelcome,
    ffmpeg_next::Error::Other {
      errno: libc::ENOMEM,
    },
    einval,
  ] {
    for in_step in [true, false] {
      assert_eq!(
        taken_despite(raw, &Error::Ffmpeg(raw), in_step),
        Taken::Unknown(crate::ExtradataDoubt::Reported(raw)),
        "{raw:?}, in step {in_step}"
      );
    }
    assert_eq!(
      taken_by_hardware_despite(&Error::Ffmpeg(raw)),
      Taken::Unknown(crate::ExtradataDoubt::Reported(raw)),
      "{raw:?} on the hardware"
    );
  }
  let minted = [
    Error::FrameBudgetExceeded(crate::error::FrameBudgetExceeded::new(
      1 << 30,
      1 << 20,
      crate::error::FrameMedium::Video,
    )),
    Error::HwSurfaceTooLarge(crate::error::HwSurfaceTooLarge::new(1 << 30, 1 << 20)),
  ];
  for named in &minted {
    assert_eq!(
      taken_despite(einval, named, true),
      Taken::Yes,
      "{named:?} in step"
    );
    assert_eq!(
      taken_despite(einval, named, false),
      Taken::Unknown(crate::ExtradataDoubt::Reported(einval)),
      "{named:?} on frame threads"
    );
    assert_eq!(
      taken_by_hardware_despite(named),
      Taken::Unknown(crate::ExtradataDoubt::Minted),
      "{named:?} on the hardware"
    );
    for before_the_queue in [eagain, ffmpeg_next::Error::Eof] {
      assert_eq!(
        taken_despite(before_the_queue, named, true),
        Taken::No,
        "{named:?} latched over {before_the_queue:?}"
      );
    }
  }
  for before_the_queue in [eagain, ffmpeg_next::Error::Eof] {
    assert_eq!(
      taken_despite(before_the_queue, &Error::Ffmpeg(before_the_queue), false),
      Taken::No,
      "{before_the_queue:?}"
    );
    assert_eq!(
      taken_by_hardware_despite(&Error::Ffmpeg(before_the_queue)),
      Taken::No,
      "{before_the_queue:?} on the hardware"
    );
  }

  let mut h264 = Parameters::new();
  // SAFETY: `h264` owns a fresh `AVCodecParameters`; two fields are written
  // with values of their own types.
  unsafe {
    (*h264.as_mut_ptr()).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
    (*h264.as_mut_ptr()).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_H264;
  }
  let in_step_on = |threads| {
    super::open_sw_decoder(&h264, DecoderLimits::default().with_threads(threads), None)
      .expect("an H.264 decoder")
      .decodes_in_step()
  };
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  assert!(
    in_step_on(crate::Threads::Single),
    "`h264` on one thread decodes in step"
  );
  assert!(
    !in_step_on(crate::Threads::Count(three)),
    "on three, frame-threaded, it does not"
  );
  super::sw_implementation::wrapped_next("cuvid");
  assert!(
    !in_step_on(crate::Threads::Single),
    "nor does one that reads as wrapped"
  );
}

/// Receives until the session answers "needs input" or the end, a decode
/// error a picture meets tolerated; answers the timestamps of the pictures
/// out, each with whether a post-commit gap was still open, and whether the
/// end escalated.
fn drained(
  dec: &mut FfmpegVideoStreamDecoder,
  dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, FfmpegBytes>,
) -> (Vec<(i64, bool)>, bool) {
  let mut delivered = Vec::new();
  loop {
    match dec.receive_frame(dst) {
      Ok(Received::Frame) => delivered.push((
        dst.pts().map_or(i64::MIN, |t| t.pts()),
        dec.degraded_resync_pending_for_test(),
      )),
      Ok(Received::NeedsInput | Received::Ended) => return (delivered, false),
      Err(VideoDecodeError::PostCommitNeverResynced(_)) => return (delivered, true),
      Err(VideoDecodeError::Decode(_)) => {}
      Err(other) => panic!("unexpected: {other:?}"),
    }
  }
}

/// Sends `pkt` until the session takes it, draining whenever it asks.
fn sent_through(
  dec: &mut FfmpegVideoStreamDecoder,
  dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, FfmpegBytes>,
  pkt: &Packet,
) {
  loop {
    match dec.send_packet(&pushed(pkt)) {
      Ok(Sent::Accepted) => return,
      Ok(Sent::MustDrain) => {
        drained(dec, dst);
      }
      Err(other) => panic!("send_packet: {other:?}"),
    }
  }
}

/// LAW (pre-R14 row 1): **a new extradata the hardware has not been seen to
/// read is provisional, and a flush then leaves it unknown.** The R11
/// stream: the hardware takes 0 to 16, the IDR 16 carrying the two-byte
/// record, and the caller seeks before draining 16 — libavcodec may still
/// hold it unread in its input slot, which the flush empties, the decoder
/// kept on the framing it read before. The extradata is unknown, by the
/// flush: at the seek's IDR 24, which carries no record, the hardware fails
/// post-commit, and the cold decoder the session would open on its
/// parameters is refused by name. Drained to "needs input" before the seek,
/// the hardware has read 16: the extradata is known, the fallback at 24
/// opens on the two-byte record, 24 anchors, and the end is clean. Taken
/// for good at the hardware's acceptance, the record opened the cold decoder
/// after the flush.
#[test]
fn a_flush_before_the_hardware_reads_a_new_extradata_leaves_it_unknown() {
  let (clip, change) = encode_h264_avcc_whose_length_size_changes(128, 96, 32);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  for drain_before_the_seek in [false, true] {
    let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
      Box::new(FakeHw::failing(
        128,
        96,
        usize::MAX,
        change + 1,
        FailShape::PostCommit,
      )),
      clip.parameters.clone(),
      tb,
    )
    .expect("build test decoder");
    let mut dst = crate::empty_owned_video_frame();
    for (index, av_pkt) in clip.packets[..=change].iter().enumerate() {
      sent_through(&mut dec, &mut dst, av_pkt);
      if index < change || drain_before_the_seek {
        drained(&mut dec, &mut dst);
      }
    }
    dec.flush().expect("a seek");
    let left = dec.extradata_unknown_for_test();
    let at_24 = dec.send_packet(&pushed(&clip.packets[24]));
    if drain_before_the_seek {
      assert!(
        matches!(at_24, Ok(Sent::Accepted)),
        "the fallback at 24 commits: {at_24:?}"
      );
      assert!(dec.is_software(), "on the cold decoder");
      let mut delivered = drained(&mut dec, &mut dst).0;
      for av_pkt in &clip.packets[25..] {
        sent_through(&mut dec, &mut dst, av_pkt);
        delivered.extend(drained(&mut dec, &mut dst).0);
      }
      crate::accepted(dec.send_eof(), "send_eof");
      let (tail, escalated) = drained(&mut dec, &mut dst);
      delivered.extend(tail);
      assert!(!escalated, "the end is clean: {delivered:?}");
      assert!(
        delivered.iter().any(|&(_, open)| !open),
        "24 anchored and the gap closed: {delivered:?}"
      );
    } else {
      assert!(
        matches!(
          at_24,
          Err(VideoDecodeError::Decode(Error::ExtradataUnknown(unknown)))
            if unknown.doubt() == crate::ExtradataDoubt::Flushed
        ),
        "the fallback at 24 is refused by name: {at_24:?}"
      );
      assert!(!dec.is_software(), "nothing was committed");
    }
    assert_eq!(
      left,
      (!drain_before_the_seek).then_some(crate::ExtradataDoubt::Flushed),
      "drained {drain_before_the_seek}: what the flush leaves"
    );
  }
}

/// LAW (pre-R14 row 1; restated by Codex R14, [medium]): **on the software
/// road too, a new extradata is provisional until the decoder is seen to
/// read its packet, and a flush then leaves it unknown.** The R11 stream
/// through a probe-era fallback at 10: the decoder serving takes the IDR 16
/// and its two-byte record, then the caller seeks. On one thread, seen read
/// — the decoder, decoding what a call hands it inside that call, answered
/// "needs input" after it, or took 17 into an input slot it takes packets
/// into only empty — the record stays known; not seen read, nothing after
/// 16, the flush leaves it unknown. On three threads the switch at 16 opens
/// its decoder on the record 16 carries, which the open applies: known
/// whatever follows, 17, a drain or the end. (A frame-threaded decoder that
/// takes a record with a packet holds it provisional through "needs input":
/// `on_frame_threads_a_new_extradata_stays_provisional_until_the_end`.)
/// Taken for good at the acceptance, every case read known.
#[test]
fn a_flush_before_the_software_decoder_reads_a_new_extradata_leaves_it_unknown() {
  #[derive(Clone, Copy, Debug)]
  enum After {
    Nothing,
    Packet,
    Drain,
    End,
  }
  let (clip, change) = encode_h264_avcc_whose_length_size_changes(128, 96, 32);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  for (threads, after, unknown) in [
    (crate::Threads::Single, After::Nothing, true),
    (crate::Threads::Single, After::Packet, false),
    (crate::Threads::Single, After::Drain, false),
    (crate::Threads::Count(three), After::Packet, false),
    (crate::Threads::Count(three), After::Drain, false),
    (crate::Threads::Count(three), After::End, false),
  ] {
    let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
      Box::new(FakeHw::failing(128, 96, 0, 10, FailShape::ProbeEra)),
      clip.parameters.clone(),
      tb,
    )
    .expect("build test decoder")
    .with_threads_for_test(threads);
    let mut dst = crate::empty_owned_video_frame();
    for av_pkt in &clip.packets[..change] {
      sent_through(&mut dec, &mut dst, av_pkt);
      drained(&mut dec, &mut dst);
    }
    sent_through(&mut dec, &mut dst, &clip.packets[change]);
    assert!(dec.is_software(), "{threads:?}: on the software road");
    match after {
      After::Nothing => {}
      After::Packet => sent_through(&mut dec, &mut dst, &clip.packets[change + 1]),
      After::Drain => {
        drained(&mut dec, &mut dst);
      }
      After::End => {
        crate::accepted(dec.send_eof(), "send_eof");
        drained(&mut dec, &mut dst);
      }
    }
    dec.flush().expect("a seek");
    assert_eq!(
      dec.extradata_unknown_for_test(),
      unknown.then_some(crate::ExtradataDoubt::Flushed),
      "{threads:?}, {after:?} after 16: what the flush leaves"
    );
  }
}

/// LAW (Codex R14, [high]): **"needs input" shows a packet read only on a
/// decoder that decodes in step; the end shows it on every decoder.** The
/// witness, over scripted answers: the end, in step or not, reads every
/// packet the decoder took; "needs input" does in step, and not on a
/// frame-threaded decoder (or one that wraps another), which answers it
/// once a worker has the packet; a picture never does. Read as in step on
/// frame threads, "needs input" marked a packet no worker had decoded read.
#[test]
fn needs_input_shows_a_packet_read_only_on_a_decoder_that_decodes_in_step() {
  use super::read_every_packet;
  for (status, in_step, read) in [
    (Received::Ended, true, true),
    (Received::Ended, false, true),
    (Received::NeedsInput, true, true),
    (Received::NeedsInput, false, false),
    (Received::Frame, true, false),
    (Received::Frame, false, false),
  ] {
    assert_eq!(
      read_every_packet(status, in_step),
      read,
      "{status:?}, in step {in_step}"
    );
  }
}

/// An `hvcC` record FFmpeg cannot read: its one array's one unit claims 64
/// bytes the record does not hold ("Invalid NAL unit size in extradata",
/// `ff_hevc_decode_extradata`).
fn malformed_hvcc() -> Vec<u8> {
  let mut record = vec![1u8; 23];
  record[21] = 0x03;
  record[22] = 1;
  record.extend_from_slice(&[0x20, 0x00, 0x01, 0x00, 0x40]);
  record
}

/// LAW (Codex R14, [high]): **on frame threads, a new extradata stays
/// provisional through "needs input", and an error after it leaves it
/// unknown; the end reads it.** FFmpeg 9 hands each packet to a worker and,
/// while not every thread has one, answers "needs input" without waiting for
/// it (`submit_packet`, `ff_thread_receive_frame`). Sessions on three
/// threads of FFmpeg's own decoders: the R11 H.264 stream's IDR 16, its
/// two-byte record intact and its body corrupted, which `h264` reports
/// invalid — `h264_decode_frame` applies a packet's record and ignores the
/// record's own failure, so a body is what fails an H.264 packet — and an
/// HEVC stream's packet 10 carrying a malformed `hvcC` record, which
/// `hevc_receive_frame` fails to apply and so fails the packet. After each
/// one's send the drain answers "needs input" with the record still
/// provisional; the drain of the end reports the worker's error, and the
/// record is unknown (`Reported(InvalidData)`). The H.264 stream whole:
/// provisional after "needs input", known at the end, and unknown
/// (`Flushed`) where the caller seeks after "needs input" instead. Read at a
/// frame-threaded "needs input", the record was known before any worker had
/// decoded its packet: the error after left it so, and so did the seek.
#[test]
fn on_frame_threads_a_new_extradata_stays_provisional_until_the_end() {
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  let (h264, change) = encode_h264_avcc_whose_length_size_changes(128, 96, 32);
  let mut corrupt = h264.packets.clone();
  corrupt[change] = with_corrupt_body(&h264.packets[change]);
  let hevc = encode_hevc_idr_with_headers(128, 96, 24);
  let malformed_at = 10;
  assert!(
    !hevc.packets[malformed_at].is_key(),
    "the HEVC packet is no keyframe"
  );
  let mut malformed = hevc.packets.clone();
  malformed[malformed_at] =
    with_new_extradata(hevc.packets[malformed_at].clone(), &malformed_hvcc());
  let reported = Some(crate::ExtradataDoubt::Reported(
    ffmpeg_next::Error::InvalidData,
  ));
  for (name, parameters, packets, at, seek, left) in [
    (
      "h264, its body corrupted",
      &h264.parameters,
      &corrupt,
      change,
      false,
      reported,
    ),
    (
      "h264, whole",
      &h264.parameters,
      &h264.packets,
      change,
      false,
      None,
    ),
    (
      "h264, whole, a seek after \"needs input\"",
      &h264.parameters,
      &h264.packets,
      change,
      true,
      Some(crate::ExtradataDoubt::Flushed),
    ),
    (
      "hevc, its record malformed",
      &hevc.parameters,
      &malformed,
      malformed_at,
      false,
      reported,
    ),
  ] {
    let mut dec = FfmpegVideoStreamDecoder::open_as(
      parameters.clone(),
      tb,
      DecoderLimits::default().with_threads(crate::Threads::Count(three)),
      DecodePath::Software,
    )
    .expect("the software road opens");
    assert_eq!(dec.active_threads(), Some(three), "{name}: frame threads");
    let mut dst = crate::empty_owned_video_frame();
    let mut log = Vec::new();
    for av_pkt in &packets[..=at] {
      crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
      answered(&mut dec, &mut dst, &mut log);
    }
    assert_eq!(
      log.last(),
      Some(&Answer::NeedsInput),
      "{name}: the drain after {at} answers \"needs input\""
    );
    assert!(
      dec.extradata_provisional_for_test(),
      "{name}: the record is still provisional"
    );
    assert_eq!(
      dec.extradata_unknown_for_test(),
      None,
      "{name}: not unknown yet"
    );
    if seek {
      dec.flush().expect("a seek");
    } else {
      crate::accepted(dec.send_eof(), "send_eof");
      answered(&mut dec, &mut dst, &mut log);
      assert_eq!(log.last(), Some(&Answer::Ended), "{name}: the end");
    }
    assert_eq!(
      dec.extradata_unknown_for_test(),
      left,
      "{name}: what the record is left: {log:?}"
    );
    assert!(
      !dec.extradata_provisional_for_test(),
      "{name}: provisional no more"
    );
  }
}

/// An HEVC clip from `libx265` in closed GOPs with no B-frames: an IDR every
/// 8 frames carrying its parameter sets, decode order display order — a
/// decoder on one thread gives each picture back from the call that decodes
/// it.
fn encode_hevc_without_b_frames(width: u32, height: u32, frames: usize) -> SyntheticClip {
  encode_x26x(
    "libx265",
    "x265-params",
    "keyint=8:min-keyint=8:scenecut=0:bframes=0:open-gop=0:repeat-headers=1:log-level=error",
    width,
    height,
    frames,
  )
}

/// The parameter sets a start-coded HEVC packet carries — its VPS, SPS and
/// PPS units — as a start-coded record.
fn hevc_parameter_sets_of(packet: &Packet) -> Vec<u8> {
  annexb_units(packet.data().expect("a payload"))
    .into_iter()
    .filter(|unit| matches!(unit.first().map(|head| (head >> 1) & 0x3f), Some(32..=34)))
    .flat_map(|unit| [0u8, 0, 0, 1].into_iter().chain(unit.iter().copied()))
    .collect()
}

/// LAW (Codex R14, [medium]): **a decoder opened for a packet carrying a new
/// extradata opens on it, never first on retained parameters a decoder
/// cannot open on, and the packet is fed.** FFmpeg's HEVC decoder parses
/// the extradata at the open and fails the open on a record it cannot read
/// (`hevc_decode_init`). An `x265` stream, IDRs at 0, 8 and 16 carrying their
/// parameter sets, the IDR 8 carrying them as `AV_PKT_DATA_NEW_EXTRADATA`
/// too, and a malformed `hvcC` record left in the session's parameters.
/// On the software road — a probe-era fallback at 3 on three threads — 7,
/// carrying the malformed record, is taken unread behind a waiting picture,
/// fails when decoded, and leaves the record unknown and retained; the
/// switch at the IDR 8 opens its decoder on a copy of the parameters
/// carrying 8's record: the send is accepted, three threads serve, the record
/// is the session's, and the stream decodes to a clean end. On the
/// hardware, which takes 5 carrying the malformed record and fails
/// post-commit at 8, the cold decoder opens on 8's record, takes 8, and the
/// stream ends clean. Opened on the retained parameters first, the switch's
/// two opens failed and left no decoder open, the re-offered 8's reopen
/// failed again, and the fallback failed.
#[test]
fn a_decoder_opened_for_a_packet_carrying_a_new_extradata_opens_on_it() {
  let clip = encode_hevc_without_b_frames(128, 96, 24);
  let (at, before) = (8, 7);
  assert!(
    clip.packets[at].is_key() && !clip.packets[before].is_key(),
    "the IDR 8, after a P picture"
  );
  let record = hevc_parameter_sets_of(&clip.packets[at]);
  assert!(!record.is_empty(), "the IDR carries its parameter sets");
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let three = core::num::NonZeroU32::new(3).expect("nonzero");

  // The software road: the switch at 8.
  let mut packets = clip.packets.clone();
  packets[before] = with_new_extradata(packets[before].clone(), &malformed_hvcc());
  packets[at] = with_new_extradata(packets[at].clone(), &record);
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, 0, 3, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Count(three));
  let mut dst = crate::empty_owned_video_frame();
  for av_pkt in &packets[..before - 1] {
    sent_through(&mut dec, &mut dst, av_pkt);
    drained(&mut dec, &mut dst);
  }
  assert!(dec.is_software(), "on the software road");
  crate::accepted(
    dec.send_packet(&pushed(&packets[before - 1])),
    "the picture before 7, left waiting",
  );
  crate::accepted(
    dec.send_packet(&pushed(&packets[before])),
    "7 taken unread, behind it",
  );
  drained(&mut dec, &mut dst);
  assert_eq!(
    dec.extradata_unknown_for_test(),
    Some(crate::ExtradataDoubt::Reported(
      ffmpeg_next::Error::InvalidData
    )),
    "7's record fails when decoded and is unknown"
  );
  assert_eq!(
    extradata_of(&dec.parameters),
    malformed_hvcc(),
    "and retained"
  );
  let mut accepted = false;
  let mut refused = Vec::new();
  for _offer in 0..3 {
    match dec.send_packet(&pushed(&packets[at])) {
      Ok(Sent::Accepted) => {
        accepted = true;
        break;
      }
      Ok(Sent::MustDrain) => {
        drained(&mut dec, &mut dst);
      }
      Err(error) => refused.push(format!("{error:?}")),
    }
  }
  assert!(
    accepted && refused.is_empty(),
    "the switch at 8 opens on 8's record and takes it: {refused:?}"
  );
  assert_eq!(
    dec.active_threads(),
    Some(three),
    "three threads serve from 8"
  );
  assert_eq!(
    dec.extradata_unknown_for_test(),
    None,
    "8's record is known"
  );
  assert_eq!(extradata_of(&dec.parameters), record, "and the session's");
  let mut delivered = drained(&mut dec, &mut dst).0;
  for av_pkt in &packets[at + 1..] {
    sent_through(&mut dec, &mut dst, av_pkt);
    delivered.extend(drained(&mut dec, &mut dst).0);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  let (tail, escalated) = drained(&mut dec, &mut dst);
  delivered.extend(tail);
  assert!(!escalated, "the software road ends clean");
  let from_8: Vec<i64> = clip.packets[at..]
    .iter()
    .map(|packet| packet.pts().expect("a pts"))
    .collect();
  assert!(
    delivered
      .iter()
      .map(|&(pts, _)| pts)
      .filter(|&pts| pts >= from_8[0])
      .eq(from_8.iter().copied()),
    "every picture from 8 on, once, in order: {delivered:?}"
  );

  // The hardware's post-commit fallback at 8.
  let mut packets = clip.packets.clone();
  packets[5] = with_new_extradata(packets[5].clone(), &malformed_hvcc());
  packets[at] = with_new_extradata(packets[at].clone(), &record);
  let (dec, delivered, refusals, escalated) = through_the_change(
    &SyntheticClip {
      parameters: clip.parameters.clone(),
      packets,
    },
    FakeHw::failing(128, 96, usize::MAX, at, FailShape::PostCommit),
    |_, _| {},
  );
  let refused: Vec<String> = refusals
    .iter()
    .map(|(index, error)| format!("{index}: {error:?}"))
    .collect();
  assert!(
    refusals.is_empty(),
    "the fallback at 8 opens on 8's record: {refused:?}"
  );
  assert!(dec.is_software(), "the cold decoder serves");
  assert_eq!(
    extradata_of(&dec.parameters),
    record,
    "8's record is the session's"
  );
  assert!(!escalated, "the hardware road ends clean: {delivered:?}");
}

/// The SPS and PPS units a start-coded H.264 packet carries, as a
/// start-coded record.
fn h264_parameter_sets_of(packet: &Packet) -> Vec<u8> {
  annexb_units(packet.data().expect("a payload"))
    .into_iter()
    .filter(|unit| matches!(unit.first().map(|head| head & 0x1f), Some(7 | 8)))
    .flat_map(|unit| [0u8, 0, 0, 1].into_iter().chain(unit.iter().copied()))
    .collect()
}

/// LAW (Codex R14, [medium]): **a replay round carries out the proof that
/// the decoder read a new extradata's packet, and applies it before its
/// error.** An all-intra H.264 stream whose packet 6 carries its parameter
/// sets as `AV_PKT_DATA_NEW_EXTRADATA`, and a probe-era fallback at 10 on
/// one thread whose replay budget holds two and a half pictures: the history
/// is fed two packets a round as the caller drains, each packet decoded
/// inside its submission. With 6's picture refused by the allocator judge
/// (scripted), the round that feeds 6 meets `FrameBudgetExceeded` — minted
/// while the decoder decoded 6 in step, after it applied 6's record — and
/// the record is the session's, known. With 6 whole, the round feeds 6 and 7
/// and stops at the budget: 7, taken by a decoder that decodes in step,
/// proves 6 read, and a seek after the round leaves the record known.
/// Dropped from the round, the proof left the refused one's record unknown
/// (`Minted`), and the whole one's unknown after the seek (`Flushed`).
#[test]
fn a_replay_round_applies_the_proof_that_a_record_was_read_before_its_error() {
  let clip = encode_h264_all_intra(128, 96, 16);
  let (with_record, fail_at) = (6, 10);
  let record = h264_parameter_sets_of(&clip.packets[0]);
  assert!(!record.is_empty(), "the stream carries its parameter sets");
  let mut packets = clip.packets.clone();
  packets[with_record] = with_new_extradata(packets[with_record].clone(), &record);
  let pts = packets[with_record].pts().expect("a pts");
  let budget = picture_bytes(&clip) * 5 / 2;
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  for refused in [true, false] {
    let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
      Box::new(FakeHw::failing(128, 96, 0, fail_at, FailShape::ProbeEra)),
      clip.parameters.clone(),
      tb,
    )
    .expect("build test decoder")
    .with_threads_for_test(crate::Threads::Single)
    .with_max_replay_bytes_for_test(budget);
    let mut dst = crate::empty_owned_video_frame();
    for av_pkt in &packets[..fail_at] {
      crate::accepted(dec.send_packet(&pushed(av_pkt)), "the hardware takes it");
    }
    let mut answers = Vec::new();
    let mut checked = false;
    for _round in 0..16 {
      match dec.send_packet(&pushed(&packets[fail_at])) {
        Ok(Sent::Accepted) => break,
        Ok(Sent::MustDrain) => {}
        Err(other) => panic!("send_packet {fail_at}: {other:?}"),
      }
      // The packets the replay has yet to feed: 6 is next at four.
      let left = dec.pending_history.len();
      if refused && left == 4 {
        dec.decline_picture_for_test(pts);
      }
      if left == if refused { 3 } else { 2 } {
        if refused {
          answered(&mut dec, &mut dst, &mut answers);
          assert!(
            answers.contains(&Answer::Refused),
            "the round that fed 6 met the refusal: {answers:?}"
          );
        } else {
          dec
            .flush()
            .expect("a seek after the round that fed 6 and 7");
        }
        assert_eq!(
          dec.extradata_unknown_for_test(),
          None,
          "refused {refused}: the record is known"
        );
        assert!(
          !dec.extradata_provisional_for_test(),
          "refused {refused}: and read"
        );
        assert_eq!(
          extradata_of(&dec.parameters),
          record,
          "refused {refused}: the record is the session's"
        );
        checked = true;
        break;
      }
      answered(&mut dec, &mut dst, &mut answers);
    }
    assert!(checked, "refused {refused}: the round that fed 6 ran");
  }
}

/// LAW (pre-R14 row 1): **a decode error reported while a new extradata is
/// unread leaves it unknown.** On one thread, the caller sends 15 without
/// draining, so a picture 15's submission decoded waits to be received, and
/// libavcodec takes the IDR 16 — its body corrupted as R12's law corrupts
/// it, its two-byte record intact — into its input slot without decoding it:
/// the send answers taken. The drain hands out the waiting picture, then
/// decodes 16 and reports it invalid. Nothing ties that error to 16, and
/// FFmpeg's HEVC decoder reports there a new extradata it could not parse:
/// the record is unknown, by the error. Left provisional, the drain's "needs
/// input" after the error read it as known.
#[test]
fn a_decode_error_while_a_new_extradata_is_unread_leaves_it_unknown() {
  let (clip, change) = encode_h264_avcc_whose_length_size_changes(128, 96, 32);
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut dec = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
    Box::new(FakeHw::failing(128, 96, 0, 10, FailShape::ProbeEra)),
    clip.parameters.clone(),
    tb,
  )
  .expect("build test decoder")
  .with_threads_for_test(crate::Threads::Single);
  let mut dst = crate::empty_owned_video_frame();
  for av_pkt in &clip.packets[..change - 1] {
    sent_through(&mut dec, &mut dst, av_pkt);
    drained(&mut dec, &mut dst);
  }
  sent_through(&mut dec, &mut dst, &clip.packets[change - 1]);
  let corrupt = with_corrupt_body(&clip.packets[change]);
  let at_16 = dec.send_packet(&pushed(&corrupt));
  assert!(
    matches!(at_16, Ok(Sent::Accepted)),
    "16 waits in the input slot, taken without a decode: {at_16:?}"
  );
  assert!(
    dec.extradata_provisional_for_test(),
    "its record is provisional"
  );
  let mut errors = Vec::new();
  loop {
    match dec.receive_frame(&mut dst) {
      Ok(Received::Frame) => {}
      Ok(Received::NeedsInput | Received::Ended) => break,
      Err(VideoDecodeError::Decode(error)) => errors.push(error),
      Err(other) => panic!("unexpected: {other:?}"),
    }
  }
  assert!(
    matches!(
      errors.as_slice(),
      [Error::Ffmpeg(ffmpeg_next::Error::InvalidData)]
    ),
    "the drain decodes 16 and reports it invalid: {errors:?}"
  );
  assert_eq!(
    dec.extradata_unknown_for_test(),
    Some(crate::ExtradataDoubt::Reported(
      ffmpeg_next::Error::InvalidData
    )),
    "the record is unknown, by the error"
  );
}

/// LAW (pre-R14 row 2): **a hardware that fails post-commit on a packet
/// carrying a new extradata, and that no fallback replaces, leaves the
/// extradata unknown.** The R11 stream on hardware that fails post-commit
/// at the IDR 16, the change. With 16's body corrupted as R12's law
/// corrupts it, the cold decoder's forward fails too and nothing commits:
/// the hardware, still serving, may or may not have applied the two-byte
/// record, so the extradata is unknown (`HardwareFailed`), and when the
/// hardware fails again at 20, the cold decoder the session would open on
/// its parameters is refused by name. With 16 whole, the fallback at 16
/// commits on the record 16 carries and decodes on. Left unclassified, the
/// failed fallback kept the four-byte record known, and the fallback at 20
/// opened on it.
#[test]
fn a_hardware_failure_on_a_new_extradata_that_no_fallback_replaces_leaves_it_unknown() {
  let (clip, change) = encode_h264_avcc_whose_length_size_changes(128, 96, 32);
  let mut corrupt = clip.packets.clone();
  corrupt[change] = with_corrupt_body(&clip.packets[change]);
  let corrupt = SyntheticClip {
    parameters: clip.parameters.clone(),
    packets: corrupt,
  };
  for (stream, whole) in [(&corrupt, false), (&clip, true)] {
    let (dec, _, refusals, _) = through_the_change(
      stream,
      FakeHw::failing(128, 96, usize::MAX, change, FailShape::PostCommit).failing_again_at(20),
      |_, _| {},
    );
    let refused: Vec<String> = refusals
      .iter()
      .map(|(index, error)| format!("{index}: {error:?}"))
      .collect();
    if whole {
      assert!(
        refusals.is_empty(),
        "whole: the fallback at 16 commits and the stream decodes on: {refused:?}"
      );
      assert!(dec.is_software(), "whole: on the cold decoder");
      assert_eq!(dec.extradata_unknown_for_test(), None, "whole: known");
      continue;
    }
    assert!(
      matches!(refusals.first(), Some((16, Error::FallbackFailed(_)))),
      "the fallback at 16 fails: {refused:?}"
    );
    assert!(
      refusals.iter().any(|(index, error)| *index == 20
        && matches!(error, Error::ExtradataUnknown(unknown)
          if unknown.doubt() == crate::ExtradataDoubt::HardwareFailed)),
      "the fallback at 20 is refused by name: {refused:?}"
    );
    assert!(!dec.is_software(), "nothing was committed");
    assert_eq!(
      dec.extradata_unknown_for_test(),
      Some(crate::ExtradataDoubt::HardwareFailed),
      "the extradata is unknown, by the hardware's failure"
    );
  }
}

/// An all-intra H.264 clip from `libx264`: every picture an IDR behind its
/// own parameter sets, no B-frames — each packet decodes alone, and a
/// decoder on one thread gives each picture back at once.
fn encode_h264_all_intra(width: u32, height: u32, frames: usize) -> SyntheticClip {
  encode_x26x(
    "libx264",
    "x264-params",
    "keyint=1:min-keyint=1:scenecut=0:bframes=0",
    width,
    height,
    frames,
  )
}

/// `clip`'s packets with `at` and the one after it sent as one packet,
/// stamped as `at`: two pictures, the second of which starts where the
/// first is refused its allocation — behind which FFmpeg's H.264 decoder
/// conceals the refused one.
fn with_two_pictures_at(clip: &SyntheticClip, at: usize) -> Vec<Packet> {
  let mut packets = clip.packets.clone();
  let both = [
    clip.packets[at].data().expect("a payload"),
    clip.packets[at + 1].data().expect("a payload"),
  ]
  .concat();
  packets[at] = repacked(&clip.packets[at], &both);
  packets.remove(at + 1);
  packets
}

/// One answer a session gave a receive.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
  /// A picture, by its timestamp.
  Picture(i64),
  /// A picture refused over the frame budget, by name.
  Refused,
  /// Any other error.
  Failed(String),
  /// "Needs input".
  NeedsInput,
  /// The end.
  Ended,
}

/// Receives from `dec` until it answers "needs input" or the end, every
/// answer recorded in `log`.
fn answered(
  dec: &mut FfmpegVideoStreamDecoder,
  dst: &mut VideoFrame<mediadecode::PixelFormat, VideoFrameExtra, FfmpegBytes>,
  log: &mut Vec<Answer>,
) {
  loop {
    let answer = match dec.receive_frame(dst) {
      Ok(Received::Frame) => Answer::Picture(dst.pts().map_or(i64::MIN, |t| t.pts())),
      Ok(Received::NeedsInput) => Answer::NeedsInput,
      Ok(Received::Ended) => Answer::Ended,
      Err(VideoDecodeError::Decode(Error::FrameBudgetExceeded(_))) => Answer::Refused,
      Err(other) => Answer::Failed(format!("{other:?}")),
    };
    let settled = matches!(answer, Answer::NeedsInput | Answer::Ended);
    log.push(answer);
    if settled {
      return;
    }
  }
}

/// The timestamps of the pictures `log` records, in order.
fn pictures_in(log: &[Answer]) -> Vec<i64> {
  log
    .iter()
    .filter_map(|answer| match answer {
      Answer::Picture(pts) => Some(*pts),
      _ => None,
    })
    .collect()
}

/// How many refusals `log` records.
fn refusals_in(log: &[Answer]) -> usize {
  log
    .iter()
    .filter(|answer| **answer == Answer::Refused)
    .count()
}

/// LAW (Codex R14, [high]): **a picture refused while a decode on one
/// thread gave another is named by the call that ran the decode, and by no
/// other.** An all-intra H.264 stream on FFmpeg's `h264`, one thread, its
/// packet 5 carrying two pictures, the first refused its allocation by the
/// allocator judge (scripted): FFmpeg conceals it — `decode_nal_units` drops
/// the slice's error and the second picture starts — so the decode reports
/// success. Sent with no picture waiting, 5 is decoded inside its send,
/// which answers `FrameBudgetExceeded`; the packet after it, sent at once, is
/// taken. Sent while a picture waits, 5 is taken unread, and the receive
/// that decodes it answers `FrameBudgetExceeded` ahead of the picture it
/// gave, which the next receive delivers; a send right after is taken. Every
/// picture but the refused one comes out, once, in order, and no second
/// refusal comes. Forgotten at the next submission, the refusal was named
/// by nothing: the send of 5 answered `Accepted`, the receive gave the
/// picture, and the picture's loss was silent.
#[test]
fn a_picture_refused_in_a_decode_is_named_by_the_call_that_ran_it() {
  let clip = encode_h264_all_intra(128, 96, 12);
  let k = 5;
  let packets = with_two_pictures_at(&clip, k);
  let pts: Vec<i64> = packets
    .iter()
    .map(|packet| packet.pts().expect("a pts"))
    .collect();
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let open = || {
    FfmpegVideoStreamDecoder::open_as(
      clip.parameters.clone(),
      tb,
      DecoderLimits::default().with_threads(crate::Threads::Single),
      DecodePath::Software,
    )
    .expect("the software road opens")
  };

  // Decoded inside its send.
  let mut dec = open();
  let mut dst = crate::empty_owned_video_frame();
  let mut log = Vec::new();
  for av_pkt in &packets[..k] {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    answered(&mut dec, &mut dst, &mut log);
  }
  dec.decline_picture_for_test(pts[k]);
  let at_k = dec.send_packet(&pushed(&packets[k]));
  assert!(
    matches!(
      at_k,
      Err(VideoDecodeError::Decode(Error::FrameBudgetExceeded(_)))
    ),
    "the send of {k}, which decoded it, names the picture it refused: {at_k:?}"
  );
  crate::accepted(
    dec.send_packet(&pushed(&packets[k + 1])),
    "the packet after, sent at once",
  );
  answered(&mut dec, &mut dst, &mut log);
  for av_pkt in &packets[k + 2..] {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    answered(&mut dec, &mut dst, &mut log);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  answered(&mut dec, &mut dst, &mut log);
  assert_eq!(
    refusals_in(&log),
    0,
    "decoded at the send: no other call names a refusal: {log:?}"
  );
  assert_eq!(
    pictures_in(&log),
    pts,
    "decoded at the send: every picture but the refused one, once, in order"
  );

  // Decoded inside a receive.
  let mut dec = open();
  let mut log = Vec::new();
  for av_pkt in &packets[..k - 1] {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    answered(&mut dec, &mut dst, &mut log);
  }
  crate::accepted(
    dec.send_packet(&pushed(&packets[k - 1])),
    "the picture before, left waiting",
  );
  dec.decline_picture_for_test(pts[k]);
  crate::accepted(
    dec.send_packet(&pushed(&packets[k])),
    "taken unread, behind the waiting picture",
  );
  let mut next = |dec: &mut FfmpegVideoStreamDecoder| match dec.receive_frame(&mut dst) {
    Ok(Received::Frame) => Answer::Picture(dst.pts().map_or(i64::MIN, |t| t.pts())),
    Ok(Received::NeedsInput) => Answer::NeedsInput,
    Ok(Received::Ended) => Answer::Ended,
    Err(VideoDecodeError::Decode(Error::FrameBudgetExceeded(_))) => Answer::Refused,
    Err(other) => Answer::Failed(format!("{other:?}")),
  };
  assert_eq!(
    next(&mut dec),
    Answer::Picture(pts[k - 1]),
    "the waiting picture"
  );
  assert_eq!(
    next(&mut dec),
    Answer::Refused,
    "the receive that decodes {k} names the picture it refused, ahead of the one it gave"
  );
  assert_eq!(
    next(&mut dec),
    Answer::Picture(pts[k]),
    "the picture it gave"
  );
  crate::accepted(
    dec.send_packet(&pushed(&packets[k + 1])),
    "a send right after",
  );
  let mut rest = Vec::new();
  answered(&mut dec, &mut dst, &mut rest);
  for av_pkt in &packets[k + 2..] {
    crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
    answered(&mut dec, &mut dst, &mut rest);
  }
  crate::accepted(dec.send_eof(), "send_eof");
  answered(&mut dec, &mut dst, &mut rest);
  assert_eq!(
    refusals_in(&log) + refusals_in(&rest),
    0,
    "decoded in a receive: no other call names a refusal: {log:?} {rest:?}"
  );
  let mut shown = pictures_in(&log);
  shown.extend([pts[k - 1], pts[k]]);
  shown.extend(pictures_in(&rest));
  assert_eq!(
    shown, pts,
    "decoded in a receive: every picture but the refused one, once, in order"
  );
}

/// LAW (Codex R14, [high]): **on frame threads, a picture a worker refused
/// and concealed is named at the end of the drain, and cleared by a seek.**
/// The same stream on three threads. FFmpeg decodes each packet on a worker
/// (`frame_worker_thread` keeps the decode's result, here success), so the
/// worker that conceals the refused picture of the two-picture packet 5
/// leaves a refusal no error of 5's will ever carry. The send of 7 waits for
/// 5's worker; the drains after it answer "needs input" with the refusal
/// still latched — back pressure collects none, a worker latching for a
/// packet whose answer may be to come. The end of the drain, with every
/// worker done, names it, once, after every picture. A seek after 7 clears
/// it with the pictures it abandons, and the stream decoded on from the
/// seek ends clean. Collected at back pressure, the refusal came out of the
/// drain after 7, mid-stream; kept through the seek, it was named after it.
#[test]
fn on_frame_threads_a_concealed_refusal_is_named_at_the_end_and_cleared_by_a_seek() {
  let clip = encode_h264_all_intra(128, 96, 12);
  let k = 5;
  let packets = with_two_pictures_at(&clip, k);
  let pts: Vec<i64> = packets
    .iter()
    .map(|packet| packet.pts().expect("a pts"))
    .collect();
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let three = core::num::NonZeroU32::new(3).expect("nonzero");
  for seek in [false, true] {
    let mut dec = FfmpegVideoStreamDecoder::open_as(
      clip.parameters.clone(),
      tb,
      DecoderLimits::default().with_threads(crate::Threads::Count(three)),
      DecodePath::Software,
    )
    .expect("the software road opens");
    assert_eq!(dec.active_threads(), Some(three), "frame threads");
    let mut dst = crate::empty_owned_video_frame();
    let mut log = Vec::new();
    for (index, av_pkt) in packets[..=k + 2].iter().enumerate() {
      if index == k {
        dec.decline_picture_for_test(pts[k]);
      }
      crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
      answered(&mut dec, &mut dst, &mut log);
    }
    assert_eq!(
      refusals_in(&log),
      0,
      "seek {seek}: {k}'s worker is done, its refusal held through back pressure: {log:?}"
    );
    if seek {
      dec.flush().expect("a seek");
      log.clear();
    }
    for av_pkt in &packets[k + 3..] {
      crate::accepted(dec.send_packet(&pushed(av_pkt)), "send_packet");
      answered(&mut dec, &mut dst, &mut log);
    }
    crate::accepted(dec.send_eof(), "send_eof");
    answered(&mut dec, &mut dst, &mut log);
    if seek {
      assert_eq!(
        refusals_in(&log),
        0,
        "after the seek the stream ends clean: {log:?}"
      );
      assert_eq!(
        pictures_in(&log),
        pts[k + 3..].to_vec(),
        "after the seek, every picture from it on"
      );
    } else {
      assert_eq!(
        refusals_in(&log),
        1,
        "the end names the refusal once: {log:?}"
      );
      assert_eq!(
        &log[log.len() - 2..],
        &[Answer::Refused, Answer::Ended],
        "named at the end, after every picture: {log:?}"
      );
      assert_eq!(
        pictures_in(&log),
        pts,
        "every picture but the refused one, once, in order"
      );
    }
  }
}

/// An HEVC video parameter set NAL unit of id `id` for `layers_minus1` + 1
/// layers (as many layer sets, at most two): one sub-layer, Main profile at
/// level 3.1, no timing information; where `mask` is given, with the
/// extension FFmpeg 9 reads whole for two layers (`decode_vps_ext`), its
/// `scalability_mask_flag` `mask`, two bits of `dimension_id` for each type
/// — the multiview type's view order index 1, the auxiliary type's `AuxId`
/// `aux_id` (1 alpha, 2 depth) — and the second layer's `nuh_layer_id` 1.
fn hevc_vps(id: u8, layers_minus1: u8, mask: Option<u16>, aux_id: u8) -> Vec<u8> {
  const AUXILIARY: u16 = 1 << (15 - 3);
  let ue = |value: u32| -> String {
    let coded = value + 1;
    let width = 32 - coded.leading_zeros();
    format!("{}{coded:b}", "0".repeat(width as usize - 1))
  };
  let mut bits = format!("{id:04b}"); // vps_video_parameter_set_id
  bits += "11";
  bits += &format!("{layers_minus1:06b}");
  bits += "0001"; // vps_max_sub_layers_minus1, vps_temporal_id_nesting_flag
  bits += &"1".repeat(16);
  bits += "00000001";
  bits += "01100000000000000000000000000000";
  bits += "1001";
  bits += &"0".repeat(44);
  bits += "01011101";
  bits += &("1".to_owned() + &ue(4) + &ue(2) + &ue(0));
  bits += &format!("{layers_minus1:06b}");
  let layer_sets_minus1 = u32::from(layers_minus1 > 0);
  bits += &ue(layer_sets_minus1);
  for _ in 0..layer_sets_minus1 {
    bits += &"1".repeat(usize::from(layers_minus1) + 1);
  }
  bits += "0"; // vps_timing_info_present_flag
  match mask {
    Some(mask) => {
      bits += "1";
      while bits.len() % 8 != 0 {
        bits += "1";
      }
      bits += "010111010"; // profile_tier_level(0, 0), splitting_flag
      bits += &format!("{mask:016b}");
      bits += &"001".repeat(mask.count_ones() as usize);
      bits += "0"; // vps_nuh_layer_id_present_flag
      for index in 0..16 {
        if mask & (1 << (15 - index)) != 0 {
          let dimension = if 1 << (15 - index) == AUXILIARY {
            aux_id
          } else {
            1
          };
          bits += &format!("{dimension:02b}");
        }
      }
      bits += "0000"; // view_id_len
      bits += "1"; // direct_dependency_flag[1][0]
      bits += "000"; // sub-layers, max_tid_ref, default_ref_layers_active
      bits += &(ue(0) + &ue(0)); // one profile_tier_level, num_add_olss
      bits += "00"; // default_output_layer_idc
      bits += &ue(0); // vps_num_rep_formats_minus1
      bits += &format!("{:016b}{:016b}", 128u16, 96u16);
      bits += "10100000000"; // chroma and bit depth present: 4:2:0, eight bits
      bits += "0000"; // conformance window, one active ref layer, aligned, sub-layer info
      bits += &ue(0).repeat(4); // the DPB sizes of both output layers
      bits += &ue(0); // direct_dep_type_len_minus2
      bits += "0"; // direct_dependency_all_layers_flag
      bits += &ue(0); // vps_non_vui_extension_length
      bits += "0"; // vps_vui_present_flag
    }
    None => bits += "0",
  }
  bits += "1";
  let payload: Vec<u8> = bits
    .as_bytes()
    .chunks(8)
    .map(|chunk| {
      chunk
        .iter()
        .enumerate()
        .filter(|&(_, &bit)| bit == b'1')
        .fold(0u8, |byte, (index, _)| byte | (0x80 >> index))
    })
    .collect();
  let mut unit = vec![0x40, 0x01];
  let mut zeros = 0;
  for byte in payload {
    if zeros >= 2 && byte <= 3 {
      unit.push(3);
      zeros = 0;
    }
    zeros = if byte == 0 { zeros + 1 } else { 0 };
    unit.push(byte);
  }
  unit
}

/// A spare HEVC video parameter set NAL unit — id 5, which no sequence
/// parameter set of the `x265` fixtures refers to, so FFmpeg stores it and
/// decodes the stream as before ([`hevc_vps`]), its auxiliary type alpha.
fn spare_vps(layers_minus1: u8, mask: Option<u16>) -> Vec<u8> {
  hevc_vps(5, layers_minus1, mask, 1)
}

/// LAW (pre-R14 row 4; restated by Codex R14, [medium]): **an HEVC stream
/// declaring an auxiliary layer FFmpeg decodes as alpha anchors nothing, and
/// a post-commit gap in it ends escalated by name; one whose extension
/// FFmpeg ignores anchors as before.** The R6 CRA stream (`x265`, every
/// keyframe carrying its parameter sets), the hardware failing post-commit
/// at a CRA whose packet also carries a spare video parameter set. Two
/// layers declaring the auxiliary type — what FFmpeg decodes beside the base
/// layer as an alpha plane — and the CRA and every keyframe after it anchor
/// nothing, no picture closes the gap, and the end escalates by name; one
/// layer, two in multiview, or three with an auxiliary mask — more layers
/// than FFmpeg decodes, its extension ignored and the base layer decoded
/// alone — and the CRA anchors and the end is clean, as without it. Read
/// without the VPS, the auxiliary stream resynced at the CRA; read by the
/// mask alone, the three-layer one escalated.
#[test]
fn an_hevc_stream_declaring_an_auxiliary_layer_anchors_nothing_and_escalates() {
  const MULTIVIEW: u16 = 1 << (15 - 1);
  const AUXILIARY: u16 = 1 << (15 - 3);
  let clip = encode_hevc_cra_with_headers(128, 96, 40);
  let at = keyframe_after(&clip, 3);
  let data = clip.packets[at].data().expect("a payload").to_vec();
  assert!(
    data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1]),
    "the fixture is start-coded"
  );
  for (name, layers_minus1, mask, declares) in [
    ("one layer", 0, None, false),
    ("multiview", 1, Some(MULTIVIEW), false),
    ("auxiliary", 1, Some(AUXILIARY), true),
    ("three layers, an auxiliary mask", 2, Some(AUXILIARY), false),
  ] {
    let mut packets = clip.packets.clone();
    packets[at] = repacked(
      &clip.packets[at],
      &[&[0, 0, 0, 1][..], &spare_vps(layers_minus1, mask), &data].concat(),
    );
    let with = SyntheticClip {
      parameters: clip.parameters.clone(),
      packets,
    };
    let (dec, delivered, escalated) = through_a_post_commit_failure(&with, at);
    assert!(dec.is_software(), "{name}: the hardware failed post-commit");
    assert_eq!(
      escalated, declares,
      "{name}: the end escalates {declares}: {delivered:?}"
    );
    assert_eq!(
      delivered.iter().any(|&(_, open)| !open),
      !declares,
      "{name}: a picture closes the gap {}: {delivered:?}",
      !declares
    );
  }
}

/// `packet`, a start-coded HEVC packet, with its video parameter set unit
/// replaced by `vps`.
fn with_vps(packet: &Packet, vps: &[u8]) -> Packet {
  let units = annexb_units(packet.data().expect("a payload"));
  assert!(
    units
      .iter()
      .any(|unit| unit.first().is_some_and(|head| (head >> 1) & 0x3f == 32)),
    "the packet carries a video parameter set"
  );
  let payload: Vec<u8> = units
    .iter()
    .flat_map(|unit| {
      let unit: &[u8] = if unit.first().is_some_and(|head| (head >> 1) & 0x3f == 32) {
        vps
      } else {
        unit
      };
      [0u8, 0, 0, 1].into_iter().chain(unit.iter().copied())
    })
    .collect();
  repacked(packet, &payload)
}

/// LAW (Codex R14, [medium]): **FFmpeg 9 reads the video parameter sets
/// these laws build as this crate does.** An `x265` stream's first packet,
/// its own video parameter set (id 0, which its sequence parameter set
/// refers to) replaced, decoded by FFmpeg's `hevc` on one thread: FFmpeg
/// negotiates an output format with alpha — `ff_hevc_is_alpha_video`
/// answering yes as `get_format` builds its list — exactly where this crate
/// reads the set as declaring an auxiliary layer: two layers, alpha or
/// depth; not for one layer, two in multiview, or three with an auxiliary
/// mask, each of which it stores and decodes the packet under; nor for a set
/// without its base layer internal, which it refuses, failing the packet at
/// that unit. The laws above rest on this reading.
#[test]
fn ffmpeg_reads_the_video_parameter_sets_these_laws_build_as_this_crate_does() {
  const MULTIVIEW: u16 = 1 << (15 - 1);
  const AUXILIARY: u16 = 1 << (15 - 3);
  let clip = encode_hevc_without_b_frames(128, 96, 8);
  let base_not_internal = {
    let mut vps = hevc_vps(0, 1, Some(AUXILIARY), 1);
    // vps_base_layer_internal_flag, the fifth bit of the payload.
    vps[2] &= !0x08;
    vps
  };
  for (name, vps, alpha, decoded) in [
    ("one layer", hevc_vps(0, 0, None, 1), false, true),
    ("multiview", hevc_vps(0, 1, Some(MULTIVIEW), 1), false, true),
    ("auxiliary", hevc_vps(0, 1, Some(AUXILIARY), 1), true, true),
    (
      "auxiliary, of depth",
      hevc_vps(0, 1, Some(AUXILIARY), 2),
      true,
      true,
    ),
    (
      "three layers, an auxiliary mask",
      hevc_vps(0, 2, Some(AUXILIARY), 1),
      false,
      true,
    ),
    (
      "auxiliary, its base layer not internal",
      base_not_internal,
      false,
      false,
    ),
  ] {
    let rule = super::access::KeyframeRule::of(
      crate::CodecId::HEVC.raw(),
      &[&[0, 0, 0, 1][..], &vps].concat(),
    );
    assert_eq!(rule.declares_alpha(), alpha, "{name}: this crate's reading");
    let mut sw = super::open_sw_decoder(
      &clip.parameters,
      DecoderLimits::default().with_threads(crate::Threads::Single),
      None,
    )
    .expect("an HEVC decoder");
    let submitted = sw.submit(&with_vps(&clip.packets[0], &vps));
    assert_eq!(
      submitted.is_ok(),
      decoded,
      "{name}: FFmpeg stores the set and decodes the packet: {submitted:?}"
    );
    assert_eq!(sw.outputs_alpha(), alpha, "{name}: FFmpeg's reading");
  }
}

/// LAW (pre-R14 row 4): **a software decoder whose negotiated output
/// carries alpha reads as one decoding an auxiliary layer.** FFmpeg's HEVC
/// decoder negotiates an alpha format (`yuva420p` and kin) only for a stream
/// whose auxiliary layer it decodes as the alpha plane; the session reads
/// that off the decoder serving as well as off the video parameter sets it
/// sees. An HEVC decoder whose context holds `yuva420p` or `yuva444p10le`
/// reads as one; `yuv420p`, `gray` and no format yet do not. (`libx265` on
/// this build cannot encode an alpha layer, so no stream here exercises the
/// negotiation itself.)
#[test]
fn a_decoder_whose_output_carries_alpha_reads_as_decoding_an_auxiliary_layer() {
  use ffmpeg_next::ffi::AVPixelFormat as F;
  let mut hevc = Parameters::new();
  // SAFETY: `hevc` owns a fresh `AVCodecParameters`; two fields are written
  // with values of their own types.
  unsafe {
    (*hevc.as_mut_ptr()).codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_VIDEO;
    (*hevc.as_mut_ptr()).codec_id = ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_HEVC;
  }
  let mut sw = super::open_sw_decoder(
    &hevc,
    DecoderLimits::default().with_threads(crate::Threads::Single),
    None,
  )
  .expect("an HEVC decoder");
  for (format, alpha) in [
    (F::AV_PIX_FMT_NONE, false),
    (F::AV_PIX_FMT_YUV420P, false),
    (F::AV_PIX_FMT_GRAY8, false),
    (F::AV_PIX_FMT_YUVA420P, true),
    (F::AV_PIX_FMT_YUVA444P10LE, true),
  ] {
    // SAFETY: the live opened context's `pix_fmt`, written with a known
    // constant of its own type; nothing is decoded on it after.
    unsafe {
      (*sw.as_mut_ptr()).pix_fmt = format;
    }
    assert_eq!(sw.outputs_alpha(), alpha, "{format:?}");
  }
}

/// The extradata `pkt` carries as `AV_PKT_DATA_NEW_EXTRADATA`, if any.
fn new_extradata_of(pkt: &Packet) -> Option<Vec<u8>> {
  super::new_extradata(pkt).map(<[u8]>::to_vec)
}

/// LAW (Codex R15, [high]): **a packet whose new extradata FFmpeg's H.264
/// decoder would reject, or apply only in part, is refused by name before
/// any decoder sees it, and nothing of the session changes.** A four-byte
/// `avcC` x264 stream (High), its packet 5 — no keyframe — carrying as
/// `AV_PKT_DATA_NEW_EXTRADATA` the five-byte record `01 42 00 1e fc`, which
/// `ff_h264_decode_extradata` rejects (shorter than seven bytes) after
/// marking the stream `avcC` and before its NAL length size or parameter
/// sets — while `h264_decode_frame` drops its answer; or the stream's own
/// record whose sequence parameter set's id reads as 32, which FFmpeg skips,
/// the rest applied. On the software road (one thread) and on the hardware,
/// probing, the send of 5 is refused as `ExtradataRejected`, naming the
/// reason: no decoder took the packet, the parameters keep their record,
/// nothing is provisional or unknown, no arbitrary slice order is read (the
/// five-byte record claims Baseline). Sent again without the record, 5 is
/// decoded under the old parameters, and every picture comes out as a
/// straight decode gives it. A well-formed replacement — the record with a
/// second copy of its PPS — is taken as before, the session's from then on.
/// Without the refusal, the five-byte record was taken and became the
/// session's parameters.
#[test]
fn a_new_extradata_ffmpeg_would_not_apply_whole_is_refused_before_any_decoder_sees_it() {
  let (clip, sps, pps) = encode_h264_avcc(128, 96, 16);
  let record = extradata_of(&clip.parameters);
  let mut bad_sps = sps.clone();
  // `seq_parameter_set_id` is the first field after the level byte: `00 00
  // 01 00 1x...` reads a 32 through `get_ue_golomb_31`.
  bad_sps[4] = 0x04;
  let bad_record = avcc(&bad_sps, &pps, 4);
  let mut replacement = record.clone();
  let pps_count = 8 + sps.len();
  replacement[pps_count] = 2;
  replacement.extend_from_slice(&u16::try_from(pps.len()).expect("a short PPS").to_be_bytes());
  replacement.extend_from_slice(&pps);
  assert_eq!(
    super::params::h264_record(&replacement),
    Ok(()),
    "the replacement is one FFmpeg applies whole"
  );
  let tb = Timebase::new(1, NonZeroI32::new(25).expect("nonzero"));
  let mut straight: Vec<i64> = clip
    .packets
    .iter()
    .map(|packet| packet.pts().expect("a pts"))
    .collect();
  straight.sort_unstable();
  for (name, refused, reason) in [
    (
      "five bytes",
      vec![0x01, 0x42, 0x00, 0x1e, 0xfc],
      crate::ExtradataRejection::TooShort { size: 5 },
    ),
    (
      "a sequence set FFmpeg skips",
      bad_record.clone(),
      crate::ExtradataRejection::Unparsed(crate::ParameterSet::Sequence),
    ),
  ] {
    let carrying = with_new_extradata(clip.packets[5].clone(), &refused);
    assert!(!carrying.is_key(), "5 is no keyframe");

    // The software road.
    let mut dec = FfmpegVideoStreamDecoder::open_as(
      clip.parameters.clone(),
      tb,
      DecoderLimits::default().with_threads(crate::Threads::Single),
      DecodePath::Software,
    )
    .expect("the software road opens");
    let mut dst = crate::empty_owned_video_frame();
    let mut delivered = Vec::new();
    for av_pkt in &clip.packets[..5] {
      sent_through(&mut dec, &mut dst, av_pkt);
      delivered.extend(drained(&mut dec, &mut dst).0);
    }
    let taken = super::live_sw::sent();
    match dec.send_packet(&pushed(&carrying)) {
      Err(VideoDecodeError::Decode(Error::ExtradataRejected(rejected))) => {
        assert_eq!(rejected.reason(), reason, "{name}: the reason");
        assert_eq!(rejected.codec(), crate::CodecId::H264, "{name}: the codec");
      }
      other => panic!("{name}: the send of 5 is refused by name: {other:?}"),
    }
    assert_eq!(super::live_sw::sent(), taken, "{name}: no decoder took 5");
    assert_eq!(
      extradata_of(&dec.parameters),
      record,
      "{name}: the parameters keep their record"
    );
    assert!(
      !dec.extradata_provisional_for_test() && dec.extradata_unknown_for_test().is_none(),
      "{name}: nothing provisional, nothing unknown"
    );
    assert!(!dec.h264_aso, "{name}: no arbitrary slice order is read");
    for av_pkt in &clip.packets[5..] {
      sent_through(&mut dec, &mut dst, av_pkt);
      delivered.extend(drained(&mut dec, &mut dst).0);
    }
    crate::accepted(dec.send_eof(), "send_eof");
    delivered.extend(drained(&mut dec, &mut dst).0);
    assert_eq!(
      delivered.iter().map(|&(pts, _)| pts).collect::<Vec<_>>(),
      straight,
      "{name}: 5 sent again without the record decodes under the old parameters, every \
       picture as a straight decode gives it"
    );

    // The hardware, probing.
    let mut hw = FfmpegVideoStreamDecoder::from_hw_inner_for_test(
      Box::new(FakeHw::never_failing(128, 96)),
      clip.parameters.clone(),
      tb,
    )
    .expect("build test decoder");
    for av_pkt in &clip.packets[..5] {
      crate::accepted(hw.send_packet(&pushed(av_pkt)), "send_packet");
    }
    assert!(
      matches!(
        hw.send_packet(&pushed(&carrying)),
        Err(VideoDecodeError::Decode(Error::ExtradataRejected(rejected))) if rejected.reason() == reason
      ),
      "{name}: the hardware send is refused by name"
    );
    assert!(
      hw.probe_extradata.is_none() && extradata_of(&hw.parameters) == record,
      "{name}: nothing changes on the hardware road"
    );
  }

  // A well-formed replacement is taken, and is the session's.
  let mut dec = FfmpegVideoStreamDecoder::open_as(
    clip.parameters.clone(),
    tb,
    DecoderLimits::default().with_threads(crate::Threads::Single),
    DecodePath::Software,
  )
  .expect("the software road opens");
  let mut dst = crate::empty_owned_video_frame();
  for av_pkt in &clip.packets[..5] {
    sent_through(&mut dec, &mut dst, av_pkt);
    drained(&mut dec, &mut dst);
  }
  let carrying = with_new_extradata(clip.packets[5].clone(), &replacement);
  assert_eq!(
    new_extradata_of(&carrying).as_deref(),
    Some(&replacement[..]),
    "the packet carries the replacement"
  );
  sent_through(&mut dec, &mut dst, &carrying);
  assert_eq!(
    extradata_of(&dec.parameters),
    replacement,
    "a well-formed replacement is the session's once the decoder takes it"
  );
}

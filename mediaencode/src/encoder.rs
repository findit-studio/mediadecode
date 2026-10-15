//! The encoder faces — raw frames in, compressed packets out, in the push
//! rhythm the decoders already keep.
//!
//! # The same rhythm, run the other way
//!
//! A decoder takes packets and gives frames; an encoder takes frames and
//! gives packets. Everything else is shared. `send_frame` answers
//! [`Sent`] — *accepted*, or *must drain* when the encoder's output queue
//! is full and the same frame has to be offered again after a drain —
//! and `receive_packet` answers [`Received`] — *produced* (a packet was
//! written into `dst`), *needs input*, or *ended*. They are the
//! vocabulary's own two enums, so a consumer that already drives a
//! decoder drives an encoder with the same loop:
//!
//! ```
//! use mediaencode::{EncodeError, Received, Sent, VideoEncoder};
//! # use mediaencode::mediaspine::{adapter::VideoAdapter, frame::VideoFrame, packet::VideoPacket};
//! # type Frame<E> = VideoFrame<
//! #   <<E as VideoEncoder>::Adapter as VideoAdapter>::PixelFormat,
//! #   <<E as VideoEncoder>::Adapter as VideoAdapter>::FrameExtra,
//! #   <E as VideoEncoder>::Buffer,
//! # >;
//! # type Packet<E> = VideoPacket<
//! #   <<E as VideoEncoder>::Adapter as VideoAdapter>::PacketExtra,
//! #   <E as VideoEncoder>::Buffer,
//! # >;
//! /// Encodes every frame, then drains the encoder to its end, handing each
//! /// packet to `out` as it is produced.
//! fn encode_all<E: VideoEncoder>(
//!   encoder: &mut E,
//!   frames: &[Frame<E>],
//!   dst: &mut Packet<E>,
//!   mut out: impl FnMut(&Packet<E>),
//! ) -> Result<(), EncodeError<E::Error>> {
//!   for frame in frames {
//!     // Offer, and on back pressure drain and offer the same frame again.
//!     while let Sent::MustDrain = encoder.send_frame(frame)? {
//!       while let Received::Produced = encoder.receive_packet(dst)? {
//!         out(dst);
//!       }
//!     }
//!   }
//!   // The end is signalled the same way, and then the delayed packets —
//!   // an encoder that reorders holds some back — come out until `Ended`.
//!   while let Sent::MustDrain = encoder.send_eof()? {
//!     while let Received::Produced = encoder.receive_packet(dst)? {
//!       out(dst);
//!     }
//!   }
//!   loop {
//!     match encoder.receive_packet(dst)? {
//!       Received::Produced => out(dst),
//!       Received::Ended => return Ok(()),
//!       // After the end has been accepted there is no input to wait on.
//!       Received::NeedsInput => continue,
//!     }
//!   }
//! }
//! ```
//!
//! An audio encoder with a fixed frame size takes every frame but the
//! stream's last at exactly that size. The last — usually short — goes
//! through [`send_final_frame`](AudioEncoder::send_final_frame), which
//! ends the sending side as `send_eof` does:
//!
//! ```
//! use mediaencode::{AudioEncoder, EncodeError, Received, Sent};
//! # use mediaencode::mediaspine::{adapter::AudioAdapter, frame::AudioFrame, packet::AudioPacket};
//! # type Frame<E> = AudioFrame<
//! #   <<E as AudioEncoder>::Adapter as AudioAdapter>::SampleFormat,
//! #   <<E as AudioEncoder>::Adapter as AudioAdapter>::ChannelLayout,
//! #   <<E as AudioEncoder>::Adapter as AudioAdapter>::FrameExtra,
//! #   <E as AudioEncoder>::Buffer,
//! # >;
//! # type Packet<E> = AudioPacket<
//! #   <<E as AudioEncoder>::Adapter as AudioAdapter>::PacketExtra,
//! #   <E as AudioEncoder>::Buffer,
//! # >;
//! /// Encodes `full` — frames of the encoder's frame size — then `last`,
//! /// which may be shorter and ends the stream, and drains the encoder
//! /// to its end.
//! fn encode_audio<E: AudioEncoder>(
//!   encoder: &mut E,
//!   full: &[Frame<E>],
//!   last: &Frame<E>,
//!   dst: &mut Packet<E>,
//!   mut out: impl FnMut(&Packet<E>),
//! ) -> Result<(), EncodeError<E::Error>> {
//!   for frame in full {
//!     while let Sent::MustDrain = encoder.send_frame(frame)? {
//!       while let Received::Produced = encoder.receive_packet(dst)? {
//!         out(dst);
//!       }
//!     }
//!   }
//!   // The last frame ends the sending side, as `send_eof` would; on
//!   // back pressure the same final frame is offered again.
//!   while let Sent::MustDrain = encoder.send_final_frame(last)? {
//!     while let Received::Produced = encoder.receive_packet(dst)? {
//!       out(dst);
//!     }
//!   }
//!   loop {
//!     match encoder.receive_packet(dst)? {
//!       Received::Produced => out(dst),
//!       Received::Ended => return Ok(()),
//!       Received::NeedsInput => continue,
//!     }
//!   }
//! }
//! ```
//!
//! # Where the faces differ from the decoders'
//!
//! - **`flush` resets, and says so.** A decoder's flush always succeeds;
//!   an encoder's may not be able to — some codecs cannot discard their
//!   state mid-stream and start a new coded sequence. [`Flushed`] answers
//!   which happened: [`Reset`](Flushed::Reset), or
//!   [`Refused`](Flushed::Refused) by name, leaving the session exactly
//!   as it was. Neither is a fault.
//! - **A video encoder can be asked for a keyframe**:
//!   [`force_keyframe`](VideoEncoder::force_keyframe) codes the next frame
//!   sent as one.
//! - **An audio encoder states its frame size**: most codecs take a fixed
//!   number of samples per frame (1024 for AAC), and
//!   [`frame_size`](AudioEncoder::frame_size) says how many. Every frame
//!   `send_frame` takes carries exactly that many; the stream's last,
//!   which may carry fewer, is sent with
//!   [`send_final_frame`](AudioEncoder::send_final_frame) and ends the
//!   stream.
//! - **The refusals every encoder shares have names.** A frame of the
//!   wrong shape, or one sent after the end, is refused with an
//!   [`EncodeRefusal`] — the same one whichever backend refused it —
//!   inside [`EncodeError`], beside the backend's own faults.
//!
//! # Construction is not on these traits
//!
//! Opening an encoder is each backend's own business, as opening a
//! decoder is: it takes [`EncoderSettings`](crate::EncoderSettings) and
//! whatever the backend needs besides.

use core::num::NonZeroU32;

use derive_more::IsVariant;
use mediaspine::{
  Received, Sent,
  adapter::{AudioAdapter, VideoAdapter},
  frame::{AudioFrame, Dimensions, VideoFrame},
  packet::{AudioPacket, VideoPacket},
};

/// What a flush answered.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, IsVariant)]
#[must_use = "a refused flush left the session as it was"]
pub enum Flushed {
  /// The session is back in its freshly opened state: every buffered
  /// frame and packet is discarded, an ended stream is open again, and
  /// the next frame sent starts a new coded sequence.
  Reset,
  /// The codec cannot reset mid-stream. Nothing was discarded and the
  /// session is exactly as it was; to start over, end it (`send_eof` and
  /// drain) and open a new one.
  Refused,
}

/// A refusal every encoder names the same way.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EncodeRefusal {
  /// The frame's dimensions are not the ones the encoder was opened
  /// with. An encoder does not scale; a frame is converted before it is
  /// sent.
  #[error(
    "the frame is {}x{}, the encoder was opened for {}x{}",
    .offered.width(), .offered.height(), .expected.width(), .expected.height()
  )]
  Dimensions {
    /// What the encoder was opened with.
    expected: Dimensions,
    /// What the frame carries.
    offered: Dimensions,
  },
  /// The frame's pixel or sample format is not the one the encoder was
  /// opened with.
  #[error("the frame's format is not the one the encoder was opened with")]
  Format,
  /// An audio frame carries a sample count the encoder cannot take:
  /// through [`send_frame`](AudioEncoder::send_frame), any count but the
  /// encoder's [frame size](AudioEncoder::frame_size); through
  /// [`send_final_frame`](AudioEncoder::send_final_frame), more than it.
  #[error("the frame carries {offered} samples per channel, the encoder takes {expected}")]
  FrameSize {
    /// The encoder's frame size.
    expected: u32,
    /// The frame's sample count per channel.
    offered: u32,
  },
  /// A frame was sent after the end of the stream was accepted — by
  /// `send_eof`, or by an audio encoder's
  /// [`send_final_frame`](AudioEncoder::send_final_frame). Only a flush
  /// that answers [`Flushed::Reset`] opens the stream again.
  #[error("the stream has ended")]
  Ended,
}

/// How an encoder call failed: a refusal every encoder names the same
/// way, or the backend's own fault.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError<E> {
  /// A refusal every encoder names the same way.
  #[error(transparent)]
  Refused(#[from] EncodeRefusal),
  /// The backend's own fault.
  #[error(transparent)]
  Fault(E),
}

/// Push-style video encoder. The caller submits raw frames and drains
/// compressed packets.
///
/// See the [module docs](self) for the rhythm, which is the decoders'.
pub trait VideoEncoder {
  /// Backend-specific vocabulary.
  type Adapter: VideoAdapter;
  /// Buffer type held by the frames this encoder takes and the packets
  /// it produces.
  type Buffer: AsRef<[u8]>;
  /// Encoder-specific fault type.
  type Error;

  /// Submits one raw frame.
  ///
  /// [`Sent::Accepted`] means the frame was consumed. [`Sent::MustDrain`]
  /// means it was **not** — drain through
  /// [`receive_packet`](Self::receive_packet) and offer the same frame
  /// again. A frame the encoder cannot take is refused by name
  /// ([`EncodeRefusal::Dimensions`], [`EncodeRefusal::Format`],
  /// [`EncodeRefusal::Ended`]).
  fn send_frame(
    &mut self,
    frame: &VideoFrame<
      <Self::Adapter as VideoAdapter>::PixelFormat,
      <Self::Adapter as VideoAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<Sent, EncodeError<Self::Error>>;

  /// Drains one compressed packet into `dst`.
  ///
  /// [`Received::Produced`] means `dst` was written.
  /// [`Received::NeedsInput`] and [`Received::Ended`] are the rhythm's
  /// other two answers, and neither is an error.
  fn receive_packet(
    &mut self,
    dst: &mut VideoPacket<<Self::Adapter as VideoAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<Received, EncodeError<Self::Error>>;

  /// Signals the end of the stream.
  ///
  /// Answers [`Sent`] as [`send_frame`](Self::send_frame) does: an
  /// encoder with undrained output may be unable to take the signal yet.
  /// Once it is accepted, [`receive_packet`](Self::receive_packet)
  /// produces the packets the encoder held back, then answers
  /// [`Received::Ended`].
  fn send_eof(&mut self) -> Result<Sent, EncodeError<Self::Error>>;

  /// Resets the session to its freshly opened state, or refuses by name
  /// where the codec cannot — see [`Flushed`].
  fn flush(&mut self) -> Result<Flushed, EncodeError<Self::Error>>;

  /// Codes the next frame sent as a keyframe — a point a decoder can
  /// start from, and a cut can land on.
  fn force_keyframe(&mut self);
}

/// Push-style audio encoder. The caller submits raw frames and drains
/// compressed packets.
///
/// See the [module docs](self) for the rhythm, which is the decoders'.
pub trait AudioEncoder {
  /// Backend-specific vocabulary.
  type Adapter: AudioAdapter;
  /// Buffer type held by the frames this encoder takes and the packets
  /// it produces.
  type Buffer: AsRef<[u8]>;
  /// Encoder-specific fault type.
  type Error;

  /// The number of samples per channel every frame sent through
  /// [`send_frame`](Self::send_frame) must carry, or `None` where the
  /// encoder takes frames of any size. The stream's last frame, sent
  /// through [`send_final_frame`](Self::send_final_frame), may carry
  /// fewer.
  fn frame_size(&self) -> Option<NonZeroU32>;

  /// Submits one raw frame.
  ///
  /// Answers as [`VideoEncoder::send_frame`] does. A frame of any size
  /// but the [frame size](Self::frame_size) is refused by name
  /// ([`EncodeRefusal::FrameSize`]), with no exception for a short last
  /// frame: the stream's last frame goes through
  /// [`send_final_frame`](Self::send_final_frame), which takes one.
  fn send_frame(
    &mut self,
    frame: &AudioFrame<
      <Self::Adapter as AudioAdapter>::SampleFormat,
      <Self::Adapter as AudioAdapter>::ChannelLayout,
      <Self::Adapter as AudioAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<Sent, EncodeError<Self::Error>>;

  /// Submits the stream's last frame, and ends the sending side with it.
  ///
  /// The frame may carry fewer samples per channel than the
  /// [frame size](Self::frame_size) — a codec's last frame usually does
  /// — but not more ([`EncodeRefusal::FrameSize`]); otherwise it is
  /// refused as [`send_frame`](Self::send_frame) refuses.
  /// [`Sent::MustDrain`] means the frame was **not** consumed and the
  /// stream has not ended: drain through
  /// [`receive_packet`](Self::receive_packet) and offer the same final
  /// frame again.
  ///
  /// [`Sent::Accepted`] ends the stream exactly as an accepted
  /// [`send_eof`](Self::send_eof) does: `receive_packet` produces the
  /// packets the encoder held back, this frame's among them, then
  /// answers [`Received::Ended`], and a later `send_frame` or
  /// `send_final_frame` is refused by name ([`EncodeRefusal::Ended`]).
  /// A stream whose samples fill their last frame exactly may end either
  /// way: that frame through `send_final_frame`, or through `send_frame`
  /// and then `send_eof`.
  fn send_final_frame(
    &mut self,
    frame: &AudioFrame<
      <Self::Adapter as AudioAdapter>::SampleFormat,
      <Self::Adapter as AudioAdapter>::ChannelLayout,
      <Self::Adapter as AudioAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<Sent, EncodeError<Self::Error>>;

  /// Drains one compressed packet into `dst`, answering as
  /// [`VideoEncoder::receive_packet`] does.
  fn receive_packet(
    &mut self,
    dst: &mut AudioPacket<<Self::Adapter as AudioAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<Received, EncodeError<Self::Error>>;

  /// Signals the end of the stream, answering as
  /// [`VideoEncoder::send_eof`] does. A stream whose last frame is short
  /// ends with [`send_final_frame`](Self::send_final_frame) instead.
  fn send_eof(&mut self) -> Result<Sent, EncodeError<Self::Error>>;

  /// Resets the session, or refuses by name — see [`Flushed`].
  fn flush(&mut self) -> Result<Flushed, EncodeError<Self::Error>>;
}

#[cfg(test)]
mod tests {
  use core::{marker::PhantomData, num::NonZeroI32};

  use mediaspine::{Timebase, Timestamp, frame::Plane, packet::PacketFlags};

  use super::*;

  struct VLoop;
  impl VideoAdapter for VLoop {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  struct ALoop;
  impl AudioAdapter for ALoop {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  type Bytes = &'static [u8];
  type VFrame = VideoFrame<u32, (), Bytes>;
  type VPacket = VideoPacket<(), Bytes>;
  type AFrame = AudioFrame<u32, u32, (), Bytes>;
  type APacket = AudioPacket<(), Bytes>;

  #[derive(Debug, PartialEq, Eq)]
  struct Fault;

  fn ms_tb() -> Timebase {
    Timebase::new(1, NonZeroI32::new(1000).expect("non-zero"))
  }

  const EMPTY: &[u8] = &[];

  fn frame(width: u32, height: u32, pts: i64) -> VFrame {
    VideoFrame::new(
      Dimensions::new(width, height),
      0,
      [Plane::new(EMPTY, 0); 4],
      0,
      (),
    )
    .with_pts(Some(Timestamp::new(pts, ms_tb())))
  }

  fn packet() -> VPacket {
    VideoPacket::new(EMPTY, ())
  }

  /// A video encoder with a **one-packet output queue** and a reorder
  /// delay of `delay` packets held back until the end — the smallest
  /// thing that can answer every state of the rhythm and mean it.
  struct Squeeze {
    dimensions: Dimensions,
    delay: u8,
    /// The pts of the packet waiting to be drained, if any.
    queued: Option<i64>,
    /// Packets still held back by the reorder delay.
    held: u8,
    eof: bool,
    keyframe_next: bool,
    /// Accepted frames, so a test can prove a back-pressured frame was
    /// not silently eaten.
    accepted: u32,
  }

  impl Squeeze {
    fn new(delay: u8) -> Self {
      Self {
        dimensions: Dimensions::new(64, 32),
        delay,
        queued: None,
        held: 0,
        eof: false,
        keyframe_next: false,
        accepted: 0,
      }
    }
  }

  impl VideoEncoder for Squeeze {
    type Adapter = VLoop;
    type Buffer = Bytes;
    type Error = Fault;

    fn send_frame(&mut self, frame: &VFrame) -> Result<Sent, EncodeError<Fault>> {
      if self.eof {
        return Err(EncodeRefusal::Ended.into());
      }
      if frame.dimensions() != self.dimensions {
        return Err(
          EncodeRefusal::Dimensions {
            expected: self.dimensions,
            offered: frame.dimensions(),
          }
          .into(),
        );
      }
      if self.queued.is_some() {
        // Nothing consumed: the caller still owns this frame.
        return Ok(Sent::MustDrain);
      }
      self.accepted += 1;
      if self.held < self.delay {
        // Held back by the reorder delay; it comes out at the end.
        self.held += 1;
        return Ok(Sent::Accepted);
      }
      self.queued = frame.pts().map(|pts| pts.pts());
      Ok(Sent::Accepted)
    }

    fn receive_packet(&mut self, dst: &mut VPacket) -> Result<Received, EncodeError<Fault>> {
      let pts = if let Some(pts) = self.queued.take() {
        Some(Timestamp::new(pts, ms_tb()))
      } else if self.eof && self.held > 0 {
        // A held packet: its timestamp is the reorder's business.
        self.held -= 1;
        None
      } else {
        return Ok(if self.eof {
          Received::Ended
        } else {
          Received::NeedsInput
        });
      };
      dst.set_pts(pts);
      dst.set_flags(if core::mem::take(&mut self.keyframe_next) {
        PacketFlags::KEY
      } else {
        PacketFlags::empty()
      });
      Ok(Received::Produced)
    }

    fn send_eof(&mut self) -> Result<Sent, EncodeError<Fault>> {
      if self.queued.is_some() {
        return Ok(Sent::MustDrain);
      }
      self.eof = true;
      Ok(Sent::Accepted)
    }

    fn flush(&mut self) -> Result<Flushed, EncodeError<Fault>> {
      self.queued = None;
      self.held = 0;
      self.eof = false;
      self.keyframe_next = false;
      Ok(Flushed::Reset)
    }

    fn force_keyframe(&mut self) {
      self.keyframe_next = true;
    }
  }

  /// An audio encoder with a fixed frame size, a one-packet output queue,
  /// and a codec that cannot reset.
  struct FixedAac {
    queued: bool,
    ended: bool,
    /// Accepted frames, so a test can prove a refused or back-pressured
    /// frame was not consumed.
    accepted: u32,
  }

  impl FixedAac {
    const FRAME_SIZE: u32 = 1024;

    fn new() -> Self {
      Self {
        queued: false,
        ended: false,
        accepted: 0,
      }
    }

    /// What `send_frame` and `send_final_frame` share: the end, the size
    /// rule — exact for a frame, at most the frame size for the last —
    /// then back pressure.
    fn submit(&mut self, frame: &AFrame, last: bool) -> Result<Sent, EncodeError<Fault>> {
      if self.ended {
        return Err(EncodeRefusal::Ended.into());
      }
      let offered = frame.nb_samples();
      let fits = if last {
        offered <= Self::FRAME_SIZE
      } else {
        offered == Self::FRAME_SIZE
      };
      if !fits {
        return Err(
          EncodeRefusal::FrameSize {
            expected: Self::FRAME_SIZE,
            offered,
          }
          .into(),
        );
      }
      if self.queued {
        // Nothing consumed, and for a final frame nothing ended.
        return Ok(Sent::MustDrain);
      }
      self.queued = true;
      self.accepted += 1;
      if last {
        self.ended = true;
      }
      Ok(Sent::Accepted)
    }
  }

  impl AudioEncoder for FixedAac {
    type Adapter = ALoop;
    type Buffer = Bytes;
    type Error = Fault;

    fn frame_size(&self) -> Option<NonZeroU32> {
      NonZeroU32::new(Self::FRAME_SIZE)
    }

    fn send_frame(&mut self, frame: &AFrame) -> Result<Sent, EncodeError<Fault>> {
      self.submit(frame, false)
    }

    fn send_final_frame(&mut self, frame: &AFrame) -> Result<Sent, EncodeError<Fault>> {
      self.submit(frame, true)
    }

    fn receive_packet(&mut self, _: &mut APacket) -> Result<Received, EncodeError<Fault>> {
      if core::mem::take(&mut self.queued) {
        return Ok(Received::Produced);
      }
      Ok(if self.ended {
        Received::Ended
      } else {
        Received::NeedsInput
      })
    }

    fn send_eof(&mut self) -> Result<Sent, EncodeError<Fault>> {
      if self.queued {
        return Ok(Sent::MustDrain);
      }
      self.ended = true;
      Ok(Sent::Accepted)
    }

    fn flush(&mut self) -> Result<Flushed, EncodeError<Fault>> {
      // The codec keeps state it cannot discard: refused, and nothing
      // about the session changes.
      Ok(Flushed::Refused)
    }
  }

  fn audio_frame(samples: u32) -> AFrame {
    AudioFrame::new(48_000, samples, 2, 0, 0, [Plane::new(EMPTY, 0); 8], 1, ())
  }

  /// Drains until the answer is not `Produced`, at most `bound` times,
  /// answering how many packets came out and what stopped the drain.
  fn drain<E: VideoEncoder>(
    encoder: &mut E,
    dst: &mut VideoPacket<<E::Adapter as VideoAdapter>::PacketExtra, E::Buffer>,
    bound: u32,
  ) -> Result<(u32, Received), EncodeError<E::Error>> {
    let mut produced = 0;
    for _ in 0..bound {
      match encoder.receive_packet(dst)? {
        Received::Produced => produced += 1,
        other => return Ok((produced, other)),
      }
    }
    Ok((produced, Received::Produced))
  }

  /// **The two-offer rule, written once against the encoder face.** The
  /// loop offers until `Accepted`, draining on `MustDrain`, and the
  /// back-pressured frame is offered again, not lost.
  #[test]
  fn the_feeder_loop_is_writable_against_the_face_alone() {
    let mut encoder = Squeeze::new(0);
    let mut dst = packet();
    let (mut offers, mut drained) = (0u32, 0u32);
    for pts in 0..3 {
      let frame = frame(64, 32, pts);
      loop {
        offers += 1;
        match encoder.send_frame(&frame).expect("no fault") {
          Sent::Accepted => break,
          Sent::MustDrain => drained += drain(&mut encoder, &mut dst, 8).expect("no fault").0,
        }
      }
    }
    assert_eq!(encoder.accepted, 3, "every frame consumed exactly once");
    assert_eq!((offers, drained), (5, 2), "two refused offers, two drains");
  }

  /// **`send_eof` drains to `Ended`.** The packets the reorder delay held
  /// back come out after the end is accepted, and then the answer is
  /// `Ended` — never `NeedsInput`, which would send a caller round a loop
  /// that cannot finish.
  #[test]
  fn send_eof_drains_the_held_packets_then_ends() {
    let mut encoder = Squeeze::new(2);
    let mut dst = packet();
    // Two frames held back by the reorder delay, the third queued.
    for pts in 0..3 {
      assert_eq!(
        encoder.send_frame(&frame(64, 32, pts)).expect("no fault"),
        Sent::Accepted
      );
    }
    // One packet is queued; the end cannot be recorded until it drains.
    assert_eq!(encoder.send_eof().expect("no fault"), Sent::MustDrain);
    assert_eq!(
      drain(&mut encoder, &mut dst, 8).expect("no fault"),
      (1, Received::NeedsInput)
    );
    assert_eq!(encoder.send_eof().expect("no fault"), Sent::Accepted);
    assert_eq!(
      drain(&mut encoder, &mut dst, 8).expect("no fault"),
      (2, Received::Ended),
      "the two held packets, then the end",
    );
    assert!(
      encoder
        .receive_packet(&mut dst)
        .expect("no fault")
        .is_ended(),
      "the end is sticky"
    );
  }

  #[test]
  fn a_frame_after_the_end_is_refused_by_name() {
    let mut encoder = Squeeze::new(0);
    assert_eq!(encoder.send_eof().expect("no fault"), Sent::Accepted);
    assert!(matches!(
      encoder.send_frame(&frame(64, 32, 0)),
      Err(EncodeError::Refused(EncodeRefusal::Ended))
    ));
  }

  #[test]
  fn a_frame_of_the_wrong_dimensions_is_refused_by_name() {
    let mut encoder = Squeeze::new(0);
    match encoder.send_frame(&frame(32, 32, 0)) {
      Err(EncodeError::Refused(EncodeRefusal::Dimensions { expected, offered })) => {
        assert_eq!(expected, Dimensions::new(64, 32));
        assert_eq!(offered, Dimensions::new(32, 32));
      }
      other => panic!("expected the dimensions refusal, got {other:?}"),
    }
    assert_eq!(encoder.accepted, 0, "a refused frame is not consumed");
  }

  /// **`flush` resets where it can and is refused by name where it
  /// cannot.** A reset opens an ended stream again; a refusal leaves the
  /// session exactly as it was — the queued packet still drains.
  #[test]
  fn flush_resets_where_it_can_and_is_refused_by_name_where_it_cannot() {
    let mut video = Squeeze::new(1);
    let mut dst = packet();
    // One frame held back by the reorder delay, then the end.
    assert_eq!(
      video.send_frame(&frame(64, 32, 0)).expect("no fault"),
      Sent::Accepted
    );
    assert_eq!(video.send_eof().expect("no fault"), Sent::Accepted);
    assert_eq!(video.flush().expect("no fault"), Flushed::Reset);
    assert_eq!(
      video
        .send_frame(&frame(64, 32, 1))
        .expect("an ended stream is open again"),
      Sent::Accepted
    );
    assert_eq!(video.send_eof().expect("no fault"), Sent::Accepted);
    assert_eq!(
      drain(&mut video, &mut dst, 8).expect("no fault"),
      (1, Received::Ended),
      "the frame held before the reset is gone: one packet, not two",
    );

    let mut audio = FixedAac::new();
    let mut apacket = APacket::new(EMPTY, ());
    assert_eq!(
      audio.send_frame(&audio_frame(1024)).expect("no fault"),
      Sent::Accepted
    );
    assert_eq!(audio.flush().expect("no fault"), Flushed::Refused);
    assert_eq!(
      audio.receive_packet(&mut apacket).expect("no fault"),
      Received::Produced,
      "a refused flush discarded nothing",
    );
  }

  /// **`force_keyframe` marks the next packet, and only that one.**
  #[test]
  fn force_keyframe_marks_the_next_packet_only() {
    let mut encoder = Squeeze::new(0);
    let mut dst = packet();
    encoder.force_keyframe();
    assert_eq!(
      encoder.send_frame(&frame(64, 32, 0)).expect("no fault"),
      Sent::Accepted
    );
    assert!(
      encoder
        .receive_packet(&mut dst)
        .expect("no fault")
        .is_produced()
    );
    assert!(
      dst.flags().contains(PacketFlags::KEY),
      "the forced keyframe"
    );
    assert_eq!(
      encoder.send_frame(&frame(64, 32, 1)).expect("no fault"),
      Sent::Accepted
    );
    assert!(
      encoder
        .receive_packet(&mut dst)
        .expect("no fault")
        .is_produced()
    );
    assert!(
      !dst.flags().contains(PacketFlags::KEY),
      "not the frame after it"
    );
  }

  #[test]
  fn an_audio_frame_of_the_wrong_size_is_refused_by_name() {
    let mut encoder = FixedAac::new();
    assert_eq!(encoder.frame_size(), NonZeroU32::new(1024));
    assert!(matches!(
      encoder.send_frame(&audio_frame(960)),
      Err(EncodeError::Refused(EncodeRefusal::FrameSize {
        expected: 1024,
        offered: 960
      }))
    ));
    // The last frame may be short, never long.
    assert!(matches!(
      encoder.send_final_frame(&audio_frame(1500)),
      Err(EncodeError::Refused(EncodeRefusal::FrameSize {
        expected: 1024,
        offered: 1500
      }))
    ));
    assert_eq!(encoder.accepted, 0, "a refused frame is not consumed");
  }

  /// **The stream's last frame may be short, and it ends the stream.**
  /// `send_final_frame` takes 600 samples where `send_frame` takes only
  /// 1024. Under back pressure it consumes nothing and ends nothing; once
  /// accepted, the stream ends as `send_eof` ends it — the final packet,
  /// then `Ended`, and every later frame refused by name.
  #[test]
  fn the_final_frame_may_be_short_and_ends_the_stream() {
    let mut encoder = FixedAac::new();
    let mut dst = APacket::new(EMPTY, ());
    assert_eq!(
      encoder.send_frame(&audio_frame(1024)).expect("no fault"),
      Sent::Accepted
    );
    // The first packet waits to be drained: back pressure.
    assert_eq!(
      encoder
        .send_final_frame(&audio_frame(600))
        .expect("no fault"),
      Sent::MustDrain
    );
    assert_eq!(
      encoder.accepted, 1,
      "a back-pressured frame is not consumed"
    );
    assert_eq!(
      encoder.receive_packet(&mut dst).expect("no fault"),
      Received::Produced
    );
    assert_eq!(
      encoder.receive_packet(&mut dst).expect("no fault"),
      Received::NeedsInput,
      "a back-pressured final frame ends nothing",
    );
    // Offered again, accepted, and the stream is over.
    assert_eq!(
      encoder
        .send_final_frame(&audio_frame(600))
        .expect("no fault"),
      Sent::Accepted
    );
    assert_eq!(encoder.accepted, 2);
    assert_eq!(
      encoder.receive_packet(&mut dst).expect("no fault"),
      Received::Produced,
      "the final frame's packet",
    );
    assert_eq!(
      encoder.receive_packet(&mut dst).expect("no fault"),
      Received::Ended
    );
    assert_eq!(
      encoder.receive_packet(&mut dst).expect("no fault"),
      Received::Ended,
      "the end is sticky",
    );
    assert!(matches!(
      encoder.send_frame(&audio_frame(1024)),
      Err(EncodeError::Refused(EncodeRefusal::Ended))
    ));
    assert!(matches!(
      encoder.send_final_frame(&audio_frame(600)),
      Err(EncodeError::Refused(EncodeRefusal::Ended))
    ));
  }

  /// **`send_frame` has no last-frame exception.** A short frame offered
  /// through it is refused by name wherever it falls in the stream, and
  /// consumes nothing; the same frame through `send_final_frame` is taken.
  #[test]
  fn send_frame_takes_no_short_last_frame() {
    let mut encoder = FixedAac::new();
    let mut dst = APacket::new(EMPTY, ());
    assert_eq!(
      encoder.send_frame(&audio_frame(1024)).expect("no fault"),
      Sent::Accepted
    );
    assert_eq!(
      encoder.receive_packet(&mut dst).expect("no fault"),
      Received::Produced
    );
    assert!(matches!(
      encoder.send_frame(&audio_frame(600)),
      Err(EncodeError::Refused(EncodeRefusal::FrameSize {
        expected: 1024,
        offered: 600
      }))
    ));
    assert_eq!(encoder.accepted, 1, "a refused frame is not consumed");
    assert_eq!(
      encoder
        .send_final_frame(&audio_frame(600))
        .expect("no fault"),
      Sent::Accepted
    );
  }

  /// Compiles only where both arguments are one type.
  fn same<T>(_: PhantomData<T>, _: PhantomData<T>) {}

  /// **The rhythm is the vocabulary's own.** The encoder faces answer the
  /// very `Sent` and `Received` the decoder faces answer, so one loop
  /// drives both.
  #[test]
  fn the_encoders_answer_the_vocabularys_own_rhythm() {
    same(PhantomData::<crate::Sent>, PhantomData::<mediaspine::Sent>);
    same(
      PhantomData::<crate::Received>,
      PhantomData::<mediaspine::Received>,
    );
    fn answers<E: VideoEncoder>(
      e: &mut E,
      dst: &mut VideoPacket<<E::Adapter as VideoAdapter>::PacketExtra, E::Buffer>,
    ) -> Option<mediaspine::Received> {
      e.receive_packet(dst).ok()
    }
    assert_eq!(
      answers(&mut Squeeze::new(0), &mut packet()),
      Some(mediaspine::Received::NeedsInput)
    );
  }
}

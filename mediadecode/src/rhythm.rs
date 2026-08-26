//! The push rhythm's vocabulary — what a submission answered
//! ([`Sent`]) and what a drain answered ([`Received`]), shared by every
//! decoder session and by the resampler.
//!
//! *Rhythm* is [the crate's own word for this][rhythm]: packets go in
//! over time, frames come out over time, and the two are not in step.
//! Both halves of that mismatch have states, and both halves' states
//! live here.
//!
//! # Why the protocol states are not errors
//!
//! A push-style codec's two calls have exactly five answers between
//! them, and only two are failures. `send_packet` either took the
//! packet or wants the output drained first; `receive_frame` either
//! produced a frame, wants more input, or is over. Everything else is a
//! fault. Carrying the three non-fault answers in `Err` cost three
//! things at once:
//!
//! * **`?` stopped meaning what it says.** A function that propagated a
//!   drain error propagated end-of-stream as a failure, and one that
//!   propagated a send error propagated *back pressure* as a failure —
//!   so every correct caller had to *not* use `?` and hand-write a
//!   classifier instead.
//! * **The classifier could not be written generically.** The
//!   conditions lived in `Self::Error`, a type the traits leave
//!   entirely to the backend, so a consumer bounded on the trait alone
//!   had no way to ask. What survived on the receive side was
//!   `while d.receive_frame(&mut f).is_ok()`, which cannot tell
//!   *drained* from *broken*; what survived on the send side was worse,
//!   the **two-offer rule** — submit, and if that fails drain and
//!   submit again, because "drain me first" and "this packet is
//!   damaged" arrived as the same `Err` and only a second attempt could
//!   separate them.
//! * **Backends drifted apart.** With nothing named at this tier each
//!   one picked its own spelling, and two implementations of the same
//!   trait ended up with two observably different protocols — the exact
//!   thing the trait exists to prevent.
//!
//! [`Sent`] and [`Received`] move those answers into the `Ok` side,
//! where they are data. `Err` goes back to meaning **fault**, `?` goes
//! back to meaning *give up*, and an exhaustive `match` makes the
//! compiler ask every consumer what it does when the decoder is full or
//! the stream ends.
//!
//! # Why these two are exhaustive and the error types are not
//!
//! [`Sent`] and [`Received`] are deliberately **not**
//! `#[non_exhaustive]`; every backend error type in this family
//! deliberately **is**. The two decisions are the same decision seen
//! from its two ends.
//!
//! A status enum is a **closed protocol vocabulary**. Its arms are not
//! this crate's taxonomy — they are the state set of the substrate
//! every push decoder is built on: `avcodec_send_packet` takes the
//! packet or says `EAGAIN`; `avcodec_receive_frame` answers a frame,
//! `EAGAIN`, or `EOF`; WebCodecs answers a full queue, a frame, an
//! empty queue, or a resolved flush. There is no sixth answer to
//! discover later, so a sixth arm would be a *redesign* rather than an
//! addition — and paying `#[non_exhaustive]`'s permanent wildcard arm
//! to insure against a redesign would defeat the whole point of the
//! type, which is that the compiler names every state a consumer
//! forgot.
//!
//! An error type is an **open fault taxonomy**. New ways to fail really
//! are discovered — a new hardware backend, a new ceiling, a new
//! corruption a codec learns to report — and a consumer that meets one
//! it has never heard of should take its generic-fault path, which is
//! exactly what a wildcard arm is for. There, the wildcard is the
//! correct handling rather than dead weight.
//!
//! # The two-state sibling
//!
//! [`Demuxer::next_packet`](crate::demuxer::Demuxer::next_packet) is
//! the one rhythm-shaped surface in this crate that answers neither
//! enum, and the reason is that it has no
//! [`NeedsInput`](Received::NeedsInput) state: a demuxer is pulled, not
//! fed, so "not yet" cannot happen to it. It answers `Ok(None)` at end
//! of file — the same fact, in the two-state shape that fits it — and
//! carries its packet in the `Ok` arm rather than into a `dst` the way
//! the decoders do. Every surface here obeys the same law: **a protocol
//! state never travels in `Err`.**
//!
//! [rhythm]: crate::decoder#what-the-names-say

use derive_more::IsVariant;

/// What a submission answered.
///
/// Returned by every `send_packet` / `send_frame` / `send_eof` in this
/// crate — [`VideoStreamDecoder`](crate::decoder::VideoStreamDecoder),
/// [`AudioStreamDecoder`](crate::decoder::AudioStreamDecoder),
/// [`SubtitleDecoder`](crate::decoder::SubtitleDecoder),
/// [`AudioResampler`](crate::resampler::AudioResampler), and their
/// [`future`](crate::future) mirrors.
///
/// # The name
///
/// [`MustDrain`](Self::MustDrain) means nothing was sent, so the type's
/// name is read as *what the send call answered* rather than as a claim
/// that a send happened — exactly as [`Received::NeedsInput`] is read.
/// The pair is named for the two calls, not for the two outcomes,
/// because that is what a caller is holding when it reads one.
///
/// # The feeder loop
///
/// ```
/// use mediadecode::{Received, Sent, decoder::AudioStreamDecoder};
/// # use mediadecode::{adapter::AudioAdapter, frame::AudioFrame, packet::AudioPacket};
/// # type Frame<D> = AudioFrame<
/// #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::SampleFormat,
/// #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::ChannelLayout,
/// #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::FrameExtra,
/// #   <D as AudioStreamDecoder>::Buffer,
/// # >;
/// # type Packet<D> = AudioPacket<
/// #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::PacketExtra,
/// #   <D as AudioStreamDecoder>::Buffer,
/// # >;
/// /// Feeds one packet, draining as required, and delivers every frame
/// /// it makes ready. Answers `true` once the stream is over.
/// fn feed<D: AudioStreamDecoder>(
///   decoder: &mut D,
///   packet: &Packet<D>,
///   dst: &mut Frame<D>,
///   mut on_frame: impl FnMut(&Frame<D>),
/// ) -> Result<bool, D::Error> {
///   loop {
///     // `?` means what it says on both faces: only a fault leaves.
///     match decoder.send_packet(packet)? {
///       Sent::Accepted => break,
///       // Not a failed submission — a full decoder. Drain and
///       // re-offer *this same packet*; nothing consumed it.
///       Sent::MustDrain => loop {
///         match decoder.receive_frame(dst)? {
///           Received::Frame => on_frame(dst),
///           Received::NeedsInput | Received::Ended => break,
///         }
///       },
///     }
///   }
///   loop {
///     match decoder.receive_frame(dst)? {
///       Received::Frame => on_frame(dst),
///       Received::NeedsInput => return Ok(false),
///       Received::Ended => return Ok(true),
///     }
///   }
/// }
/// ```
///
/// That loop is the point. Under the older convention it could not be
/// written against the trait at all: "drain me first" and "this packet
/// is damaged" were both `Err`, indistinguishable to a generic
/// consumer, so the idiom that survived was to **offer the packet
/// twice** — submit, drain on any failure, submit again, and treat the
/// second failure as real. Twice, because once meant nothing.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, IsVariant)]
#[must_use = "a submission that was not accepted must be drained and re-offered"]
pub enum Sent {
  /// The session took it. A packet is consumed; an end-of-stream is
  /// recorded.
  Accepted,
  /// **Nothing was consumed.** The session cannot take more until its
  /// output is drained: call
  /// [`receive_frame`](crate::decoder::AudioStreamDecoder::receive_frame)
  /// until it stops producing, then offer the *same* packet again.
  ///
  /// This is back pressure, not refusal. The submission left no trace
  /// — the packet is still the caller's to re-send, and the session's
  /// state is exactly what it was before the call.
  MustDrain,
}

/// What a drain call answered.
///
/// Returned by every `receive_frame` in this crate —
/// [`VideoStreamDecoder`](crate::decoder::VideoStreamDecoder),
/// [`AudioStreamDecoder`](crate::decoder::AudioStreamDecoder),
/// [`SubtitleDecoder`](crate::decoder::SubtitleDecoder),
/// [`AudioResampler`](crate::resampler::AudioResampler), and their
/// [`future`](crate::future) mirrors — so one vocabulary covers every
/// backend and every generic consumer.
///
/// # The drain loop
///
/// ```
/// use mediadecode::{Received, decoder::AudioStreamDecoder};
/// # use mediadecode::{adapter::AudioAdapter, frame::AudioFrame};
/// # type Frame<D> = AudioFrame<
/// #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::SampleFormat,
/// #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::ChannelLayout,
/// #   <<D as AudioStreamDecoder>::Adapter as AudioAdapter>::FrameExtra,
/// #   <D as AudioStreamDecoder>::Buffer,
/// # >;
/// /// Drains everything ready right now. Answers `true` once the
/// /// stream is over, so the caller knows not to feed it again.
/// fn drain<D: AudioStreamDecoder>(
///   decoder: &mut D,
///   dst: &mut Frame<D>,
///   mut on_frame: impl FnMut(&Frame<D>),
/// ) -> Result<bool, D::Error> {
///   loop {
///     // `?` means what it says again: only a fault leaves here.
///     match decoder.receive_frame(dst)? {
///       Received::Frame => on_frame(dst),
///       Received::NeedsInput => return Ok(false),
///       Received::Ended => return Ok(true),
///     }
///   }
/// }
/// ```
///
/// The loop terminates by construction: the two non-frame arms both
/// return, and neither can be reached without the callee having decided
/// which one it is. Under the older convention — both conditions
/// arriving as unnamed `Err` variants — a caller that had already sent
/// end-of-stream could not tell "send me more" from "there is no more",
/// and a drain that answered the first forever was an ordinary,
/// silent infinite loop.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, IsVariant)]
#[must_use = "the drain's answer decides whether to feed, deliver, or stop"]
pub enum Received {
  /// A frame was written into `dst`. Take it and call again.
  Frame,
  /// Nothing is ready. The session is waiting on input: send another
  /// packet (or frame, for a resampler), or signal end-of-stream and
  /// drain the tail.
  ///
  /// **Never returned after the stream has ended** — that is
  /// [`Ended`](Self::Ended)'s job, and keeping the two apart is what
  /// makes a drain loop terminate.
  NeedsInput,
  /// The stream is over and every buffered frame has been delivered.
  /// Nothing but [`flush`](crate::decoder::AudioStreamDecoder::flush)
  /// changes this answer.
  Ended,
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{
    adapter::AudioAdapter,
    decoder::AudioStreamDecoder,
    frame::{AudioFrame, Plane},
    packet::AudioPacket,
  };

  struct ALoop;
  impl AudioAdapter for ALoop {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  /// **Faults only**, and with the two protocol states gone there is
  /// nothing this decoder can fail at — which is the shape the reform
  /// produces at the small end.
  #[derive(Debug, PartialEq, Eq)]
  struct Fault;

  /// A decoder with a **one-frame output queue**: the smallest thing
  /// that can answer [`Sent::MustDrain`] and mean it.
  struct Squeeze {
    queued: bool,
    eof: bool,
    /// Counts submissions the session actually consumed, so a test can
    /// prove a back-pressured packet was not silently eaten.
    accepted: u32,
  }

  impl AudioStreamDecoder for Squeeze {
    type Adapter = ALoop;
    type Buffer = &'static [u8];
    type Error = Fault;

    fn send_packet(&mut self, _: &AudioPacket<(), &'static [u8]>) -> Result<Sent, Fault> {
      // **The terminal state is checked first, and a fixture that
      // models the face has to model this too.** `MustDrain` promises
      // that draining makes the same offer acceptable; past the end it
      // never does, so answering it under a held frame would send a
      // caller round a loop that cannot finish — and the drained retry
      // would then be *accepted*, un-ending a stream that ended.
      if self.eof {
        return Err(Fault);
      }
      if self.queued {
        // Nothing consumed. The caller still owns this packet.
        return Ok(Sent::MustDrain);
      }
      self.queued = true;
      self.accepted += 1;
      Ok(Sent::Accepted)
    }

    fn receive_frame(
      &mut self,
      _: &mut AudioFrame<u32, u32, (), &'static [u8]>,
    ) -> Result<Received, Fault> {
      if self.queued {
        self.queued = false;
        return Ok(Received::Frame);
      }
      Ok(if self.eof {
        Received::Ended
      } else {
        Received::NeedsInput
      })
    }

    fn send_eof(&mut self) -> Result<Sent, Fault> {
      // A held frame is output too: the seat has to clear before the
      // end can be recorded, and saying so is the same back pressure.
      if self.queued {
        return Ok(Sent::MustDrain);
      }
      self.eof = true;
      Ok(Sent::Accepted)
    }

    fn flush(&mut self) -> Result<(), Fault> {
      self.queued = false;
      self.eof = false;
      Ok(())
    }
  }

  fn packet() -> AudioPacket<(), &'static [u8]> {
    AudioPacket::new(&b"compressed"[..], ())
  }

  fn frame() -> AudioFrame<u32, u32, (), &'static [u8]> {
    const EMPTY: &[u8] = &[];
    AudioFrame::new(48_000, 1024, 2, 0, 0, [Plane::new(EMPTY, 0); 8], 1, ())
  }

  /// **The two-offer rule, written once and generically.**
  ///
  /// Bounded on the trait alone — no backend type, no predicate, no
  /// error inspection. That is the property under test: before the
  /// reform this function could not exist, because "drain me first" and
  /// "this packet is damaged" were both `Err` on a `Self::Error` the
  /// bound says nothing about. The idiom that replaced it was to submit
  /// the packet **twice** and treat the second failure as real — a
  /// guess that is wrong whenever one drain is not enough.
  ///
  /// Here the loop offers until the answer is `Accepted`, and `?` is
  /// honest: only a fault leaves.
  fn feed<D: AudioStreamDecoder>(
    decoder: &mut D,
    packet: &AudioPacket<<D::Adapter as AudioAdapter>::PacketExtra, D::Buffer>,
    dst: &mut AudioFrame<
      <D::Adapter as AudioAdapter>::SampleFormat,
      <D::Adapter as AudioAdapter>::ChannelLayout,
      <D::Adapter as AudioAdapter>::FrameExtra,
      D::Buffer,
    >,
    frames: &mut u32,
    offers: &mut u32,
  ) -> Result<(), D::Error> {
    loop {
      *offers += 1;
      match decoder.send_packet(packet)? {
        Sent::Accepted => return Ok(()),
        Sent::MustDrain => {
          while let Received::Frame = decoder.receive_frame(dst)? {
            *frames += 1;
          }
        }
      }
    }
  }

  #[test]
  fn the_two_offer_rule_is_writable_against_the_trait_alone() {
    let mut decoder = Squeeze {
      queued: false,
      eof: false,
      accepted: 0,
    };
    let packet = packet();
    let mut dst = frame();
    let (mut frames, mut offers) = (0, 0);

    // First packet: the queue is empty, so it goes straight in.
    feed(&mut decoder, &packet, &mut dst, &mut frames, &mut offers).expect("no fault");
    assert_eq!((offers, frames), (1, 0));

    // Second packet: the seat is occupied. The loop is told so, drains
    // the one frame, and re-offers the SAME packet — which is only
    // sound because `MustDrain` promises nothing was consumed.
    feed(&mut decoder, &packet, &mut dst, &mut frames, &mut offers).expect("no fault");
    assert_eq!(
      (offers, frames),
      (3, 1),
      "one refused offer, one drained frame, one accepted offer",
    );
    assert_eq!(
      decoder.accepted, 2,
      "a back-pressured submission must not be counted as consumed",
    );
  }

  #[test]
  fn end_of_stream_is_back_pressured_the_same_way() {
    let mut decoder = Squeeze {
      queued: false,
      eof: false,
      accepted: 0,
    };
    let mut dst = frame();
    decoder
      .send_packet(&packet())
      .expect("no fault")
      .is_accepted()
      .then_some(())
      .expect("the empty seat takes it");

    // The seat is full, so the end cannot be recorded yet — and the
    // caller is told which of the two it is.
    assert_eq!(decoder.send_eof(), Ok(Sent::MustDrain));
    assert_eq!(decoder.receive_frame(&mut dst), Ok(Received::Frame));
    assert_eq!(decoder.send_eof(), Ok(Sent::Accepted));
    assert_eq!(decoder.receive_frame(&mut dst), Ok(Received::Ended));
  }
}

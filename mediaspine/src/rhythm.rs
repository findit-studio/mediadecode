//! The push rhythm's vocabulary — what a submission answered
//! ([`Sent`]) and what a drain answered ([`Received`]), shared by every
//! push session written against this vocabulary: the decoders, the
//! resampler, the encoders.
//!
//! *Rhythm* is the family's own word for this: packets go in over time,
//! frames come out over time, and the two are not in step. Both halves
//! of that mismatch have states, and both halves' states live here.
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
//! `Demuxer::next_packet` (the read face in `mediadecode`) is the one
//! rhythm-shaped surface in the family that answers neither enum, and
//! the reason is that it has no
//! [`NeedsInput`](Received::NeedsInput) state: a demuxer is pulled, not
//! fed, so "not yet" cannot happen to it. It answers `Ok(None)` at end
//! of file — the same fact, in the two-state shape that fits it — and
//! carries its packet in the `Ok` arm rather than into a `dst` the way
//! the decoders do. Every surface here obeys the same law: **a protocol
//! state never travels in `Err`.**

use derive_more::IsVariant;

/// What a submission answered.
///
/// Returned by every `send_packet` / `send_frame` / `send_eof` written
/// against this vocabulary — the decoder faces in `mediadecode`
/// (`VideoStreamDecoder`, `AudioStreamDecoder`, `SubtitleDecoder`, and
/// their `future` mirrors) and the
/// [`AudioResampler`](crate::resampler::AudioResampler) here.
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
/// Offer, drain on [`MustDrain`](Self::MustDrain), offer the *same*
/// packet again — the loop this answer makes writable against a trait
/// alone is written out, against `AudioStreamDecoder`, on
/// `mediadecode`'s `decoder` module.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, IsVariant)]
#[must_use = "a submission that was not accepted must be drained and re-offered"]
pub enum Sent {
  /// The session took it. A packet is consumed; an end-of-stream is
  /// recorded.
  Accepted,
  /// **Nothing was consumed.** The session cannot take more until its
  /// output is drained: call its `receive_frame` until it stops
  /// producing, then offer the *same* packet again.
  ///
  /// This is back pressure, not refusal. The submission left no trace
  /// — the packet is still the caller's to re-send, and the session's
  /// state is exactly what it was before the call.
  MustDrain,
}

/// What a drain call answered.
///
/// Returned by every `receive_frame` / `receive_packet` written against
/// this vocabulary — the decoder faces in `mediadecode` and their
/// `future` mirrors, the [`AudioResampler`](crate::resampler::AudioResampler)
/// here, and the encoders — so one vocabulary covers every backend and
/// every generic consumer.
///
/// # The drain loop
///
/// Drain until [`NeedsInput`](Self::NeedsInput) or
/// [`Ended`](Self::Ended) — a loop that terminates by construction — is
/// written out, against `AudioStreamDecoder`, on `mediadecode`'s
/// `decoder` module.
///
/// # The name
///
/// The answer that delivers output is [`Produced`](Self::Produced): a
/// decoder or a resampler produces a frame, an encoder a packet, and
/// the one vocabulary names what all of them do. It was `Frame` while
/// only decoders answered it, and the old name is gone rather than
/// kept as a second spelling of the same state:
///
/// ```compile_fail,E0599
/// let _ = mediaspine::Received::Frame;
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, IsVariant)]
#[must_use = "the drain's answer decides whether to feed, deliver, or stop"]
pub enum Received {
  /// An output was written into `dst` — a frame from a decoder or a
  /// resampler, a packet from an encoder. Take it and call again.
  Produced,
  /// Nothing is ready. The session is waiting on input: send another
  /// packet (or frame, for a resampler), or signal end-of-stream and
  /// drain the tail.
  ///
  /// **Never returned after the stream has ended** — that is
  /// [`Ended`](Self::Ended)'s job, and keeping the two apart is what
  /// makes a drain loop terminate.
  NeedsInput,
  /// The stream is over and every buffered output has been delivered.
  /// Nothing but the session's `flush` changes this answer.
  Ended,
}

#[cfg(test)]
mod tests {
  use super::*;

  /// **The rename's law: where `Frame` was, `Produced` is.** The answer
  /// that carries output is the first of the three, it is the only one
  /// that asks the caller to take something, and its predicate is
  /// spelled for what every push session does — a decoder, a resampler
  /// and an encoder alike.
  #[test]
  fn the_answer_that_carries_output_is_produced() {
    let answers = [Received::Produced, Received::NeedsInput, Received::Ended];
    assert!(answers[0].is_produced());
    assert_eq!(answers.iter().filter(|a| a.is_produced()).count(), 1);
    assert!(answers[1].is_needs_input() && answers[2].is_ended());
    assert_ne!(Received::Produced, Received::Ended);
  }
}

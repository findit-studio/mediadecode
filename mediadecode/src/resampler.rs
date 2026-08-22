//! The resample seam — sample-rate, sample-format and channel-layout
//! conversion for decoded audio.
//!
//! Every consumer downstream of a decoder wants audio in *its* shape,
//! and the shapes disagree: speech models want 16 kHz mono, audio-event
//! and music models want 48 kHz, a mixer wants whatever the graph runs
//! at. The decoder produces whatever the file holds. [`AudioResampler`]
//! is the seam between them.
//!
//! The face is the [`AudioStreamDecoder`] push pair, one tier along:
//! `send_frame` / `receive_frame` / `send_eof` / `flush`, with
//! "nothing ready yet" signalled the same way — a backend-specific
//! [`Error`](AudioResampler::Error) variant out of
//! [`receive_frame`](AudioResampler::receive_frame). One rhythm for the
//! whole chain: the caller pushes, drains, and schedules.
//!
//! No FFmpeg in the shape. `swresample` is merely the first
//! implementation; a pure-Rust polyphase resampler fits the same face.
//!
//! [`AudioStreamDecoder`]: crate::decoder::AudioStreamDecoder

use crate::{adapter::AudioAdapter, frame::AudioFrame};

/// The [`AudioFrame`] an [`AudioAdapter`] carries over buffer `B`.
pub type AdapterAudioFrame<A, B> = AudioFrame<
  <A as AudioAdapter>::SampleFormat,
  <A as AudioAdapter>::ChannelLayout,
  <A as AudioAdapter>::FrameExtra,
  B,
>;

/// Push-style audio resampler: rate, sample format, and channel layout
/// conversion.
///
/// # Both specs are explicit at construction
///
/// An implementation is built knowing **both** ends: the source spec —
/// the rate, sample format and channel layout the frames coming in will
/// carry, which the caller reads off the track's
/// [`TrackInfo`](crate::demuxer::TrackInfo) — and the target spec,
/// which is the caller's own. The target is never a constant baked into
/// the seam: one consumer wants 16 kHz mono and the next wants 48 kHz
/// stereo, from the same file, at the same time. It is options.
///
/// Construction is therefore not on this trait, for the same reason it
/// is not on the decoder traits: each backend takes its own spelling of
/// those two specs.
///
/// # A mid-stream format change is a named refusal
///
/// If a frame arrives whose rate, sample format or channel layout is
/// not the source spec the implementation was built with,
/// [`send_frame`](Self::send_frame) **fails with a named error**. It
/// does not silently reconfigure. Silent reconfiguration would resample
/// the two halves of a stream on different terms and hand the caller a
/// single unbroken timeline built out of them, with nothing in the
/// output saying so. Refusing hands the decision back up: the caller
/// rebuilds the resampler for the new source spec, and the seam in the
/// data has a seam in the code to match.
///
/// # EOF drains the conversion tail
///
/// A rate converter holds samples back: its filter needs future input
/// to produce present output, so at any moment some tens of
/// milliseconds are inside it and not yet out. [`send_eof`](Self::send_eof)
/// followed by [`receive_frame`](Self::receive_frame) drains them.
/// Skipping the drain loses the end of every file.
///
/// Output timestamps are kept by **delay-compensated accounting**: the
/// output timeline is anchored on the first input timestamp and then
/// advanced by the number of samples actually produced, so the frames
/// drained after EOF continue the same timeline rather than restarting
/// or repeating it.
pub trait AudioResampler {
  /// Backend vocabulary. The source and target specs are both
  /// expressed in it.
  type Adapter: AudioAdapter;
  /// Buffer type held by the frames this resampler accepts and
  /// produces.
  type Buffer: AsRef<[u8]>;
  /// Resampler-specific error type. Carries both the
  /// "nothing ready yet" signal out of [`receive_frame`](Self::receive_frame)
  /// and the mid-stream-change refusal out of
  /// [`send_frame`](Self::send_frame).
  type Error;

  /// Submits one decoded frame.
  ///
  /// Fails with a named error if the frame's rate, sample format or
  /// channel layout is not the source spec this resampler was built
  /// with.
  fn send_frame(
    &mut self,
    frame: &AdapterAudioFrame<Self::Adapter, Self::Buffer>,
  ) -> Result<(), Self::Error>;

  /// Drains one converted frame into `dst`. Implementations signal
  /// "no frame ready" via a backend-specific [`Error`](Self::Error)
  /// variant, exactly as
  /// [`AudioStreamDecoder::receive_frame`](crate::decoder::AudioStreamDecoder::receive_frame)
  /// does.
  fn receive_frame(
    &mut self,
    dst: &mut AdapterAudioFrame<Self::Adapter, Self::Buffer>,
  ) -> Result<(), Self::Error>;

  /// Signals end of stream. The conversion tail becomes drainable
  /// through [`receive_frame`](Self::receive_frame).
  fn send_eof(&mut self) -> Result<(), Self::Error>;

  /// Flushes internal state — buffered input, the undrained tail, and
  /// the output timestamp anchor — leaving the resampler ready for a
  /// fresh stream on the same two specs.
  fn flush(&mut self) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::frame::AudioFrame;

  struct ALoop;
  impl AudioAdapter for ALoop {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  #[derive(Debug, PartialEq, Eq)]
  enum LoopError {
    /// The "nothing ready yet" signal — the `AudioStreamDecoder`
    /// mechanism, one tier along.
    Again,
    /// The mid-stream refusal.
    SourceChanged,
  }

  /// Loopback resampler: accepts only 48 kHz, hands each frame straight
  /// back, and refuses anything else by name.
  struct LoopResampler {
    pending: bool,
  }

  impl AudioResampler for LoopResampler {
    type Adapter = ALoop;
    type Buffer = &'static [u8];
    type Error = LoopError;

    fn send_frame(
      &mut self,
      frame: &AudioFrame<u32, u32, (), &'static [u8]>,
    ) -> Result<(), LoopError> {
      if frame.sample_rate() != 48_000 {
        return Err(LoopError::SourceChanged);
      }
      self.pending = true;
      Ok(())
    }

    fn receive_frame(
      &mut self,
      _dst: &mut AudioFrame<u32, u32, (), &'static [u8]>,
    ) -> Result<(), LoopError> {
      if !self.pending {
        return Err(LoopError::Again);
      }
      self.pending = false;
      Ok(())
    }

    fn send_eof(&mut self) -> Result<(), LoopError> {
      Ok(())
    }

    fn flush(&mut self) -> Result<(), LoopError> {
      self.pending = false;
      Ok(())
    }
  }

  fn frame(rate: u32) -> AudioFrame<u32, u32, (), &'static [u8]> {
    const EMPTY: &[u8] = &[];
    AudioFrame::new(
      rate,
      1024,
      2,
      0,
      0,
      [crate::frame::Plane::new(EMPTY, 0); 8],
      1,
      (),
    )
  }

  #[test]
  fn the_face_is_implementable_and_signals_needs_more_by_error() {
    fn _accepts<R: AudioResampler>() {}
    _accepts::<LoopResampler>();

    let mut r = LoopResampler { pending: false };
    let mut dst = frame(48_000);
    assert_eq!(r.receive_frame(&mut dst), Err(LoopError::Again));
    r.send_frame(&frame(48_000)).expect("matching spec");
    assert_eq!(r.receive_frame(&mut dst), Ok(()));
    assert_eq!(r.receive_frame(&mut dst), Err(LoopError::Again));
  }

  #[test]
  fn a_mid_stream_change_is_refused_by_name() {
    let mut r = LoopResampler { pending: false };
    r.send_frame(&frame(48_000)).expect("matching spec");
    assert_eq!(
      r.send_frame(&frame(44_100)),
      Err(LoopError::SourceChanged),
      "the face never silently reconfigures",
    );
  }

  #[test]
  fn flush_drops_the_undrained_tail() {
    let mut r = LoopResampler { pending: false };
    r.send_frame(&frame(48_000)).expect("matching spec");
    r.flush().expect("flush");
    let mut dst = frame(48_000);
    assert_eq!(r.receive_frame(&mut dst), Err(LoopError::Again));
  }
}

//! Decoder traits — push-style streams (FFmpeg / WebCodecs / ProRes
//! RAW via VTDecompressionSession), pull-style frame sources
//! (R3D / BRAW / ARRIRAW / X-OCN / Canon RAW Light), and the one-shot
//! [`ImageDecoder`].
//!
//! # What the names say
//!
//! `Stream` in a name means the decoder has a *rhythm*: packets go in
//! over time, frames come out over time, and the two are not in step —
//! hence `send_packet` / `receive_frame` / `send_eof` / `flush`.
//! [`VideoStreamDecoder`] and [`AudioStreamDecoder`] carry it.
//! [`SubtitleDecoder`] and [`ImageDecoder`] do not, and their names say
//! so: a subtitle cue and a still image each come out of exactly the
//! packet that went in.
//!
//! All four are mirrored under [`crate::future`] with `async fn`
//! methods, in both the `!Send` and the `Send`-bounded variant.
//!
//! # The buffer seat
//!
//! Every trait here carries a `Buffer` associated type bounded only by
//! `AsRef<[u8]>`, and none of them names a concrete carrier. What a
//! backend may bind there is the
//! [D-seat amputation contract](crate::adapter#the-d-seat-amputation-contract):
//! owned, `Send + Sync`, clone-is-a-refcount-bump, with no
//! backend-internal lifetime crossing the seam.
//!
//! # Construction is not on these traits
//!
//! Opening a decoder is each backend's own business and each
//! backend's is different — codec parameters, a WebCodecs config
//! dictionary, a clip handle. Putting a constructor on the trait would
//! force one of those spellings onto all of them, which is the same
//! stance [`crate::demuxer::Demuxer`] takes for the same reason.

use crate::{
  Timebase, Timestamp,
  adapter::{AudioAdapter, ImageAdapter, SubtitleAdapter, VideoAdapter},
  demuxer::AttachmentPacket,
  frame::{AudioFrame, ImageFrame, SubtitleFrame, VideoFrame},
  packet::{AudioPacket, SubtitlePacket, VideoPacket},
};

/// Push-style video decoder. Caller submits compressed packets and
/// drains decoded frames.
///
/// Backends: FFmpeg, WebCodecs, ProRes RAW (VideoToolbox).
pub trait VideoStreamDecoder {
  /// Backend-specific vocabulary.
  type Adapter: VideoAdapter;
  /// Buffer type held by the packets and frames this decoder
  /// produces or accepts.
  type Buffer: AsRef<[u8]>;
  /// Decoder-specific error type.
  type Error;

  /// Submits one compressed packet.
  fn send_packet(
    &mut self,
    packet: &VideoPacket<<Self::Adapter as VideoAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<(), Self::Error>;

  /// Drains one decoded frame into `dst`. Backends signal "no
  /// frame ready" via a backend-specific `Error` variant.
  fn receive_frame(
    &mut self,
    dst: &mut VideoFrame<
      <Self::Adapter as VideoAdapter>::PixelFormat,
      <Self::Adapter as VideoAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<(), Self::Error>;

  /// Signals end-of-stream.
  fn send_eof(&mut self) -> Result<(), Self::Error>;

  /// Flushes internal state.
  fn flush(&mut self) -> Result<(), Self::Error>;
}

/// Pull-style video frame source. Caller requests frames by integer
/// index. Clip-level metadata accessible via `clip_meta()`.
///
/// Backends: R3D, BRAW, ARRIRAW, Sony X-OCN, Canon Cinema RAW Light.
pub trait VideoFrameSource {
  /// Backend-specific vocabulary.
  type Adapter: VideoAdapter;
  /// Buffer type for the produced frames.
  type Buffer: AsRef<[u8]>;
  /// Backend-specific clip-level metadata bag (e.g. `R3dClipMeta`,
  /// `ArriClipMeta`). Backends without clip metadata set this to `()`.
  type ClipMeta;
  /// Decoder-specific error type.
  type Error;

  /// Total frame count in the clip.
  fn frame_count(&self) -> u64;
  /// Video frame rate (frames per second as a `Timebase`).
  fn frame_rate(&self) -> Timebase;
  /// Total clip duration.
  fn duration(&self) -> Timestamp;
  /// Backend-specific clip-level metadata.
  fn clip_meta(&self) -> &Self::ClipMeta;

  /// Decodes one frame at `index` into `dst`.
  fn decode_frame(
    &mut self,
    index: u64,
    dst: &mut VideoFrame<
      <Self::Adapter as VideoAdapter>::PixelFormat,
      <Self::Adapter as VideoAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<(), Self::Error>;
}

/// Push-style audio decoder.
pub trait AudioStreamDecoder {
  /// Backend vocabulary.
  type Adapter: AudioAdapter;
  /// Buffer type.
  type Buffer: AsRef<[u8]>;
  /// Decoder-specific error.
  type Error;
  /// Submits a compressed audio packet.
  fn send_packet(
    &mut self,
    packet: &AudioPacket<<Self::Adapter as AudioAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<(), Self::Error>;
  /// Drains a decoded frame.
  fn receive_frame(
    &mut self,
    dst: &mut AudioFrame<
      <Self::Adapter as AudioAdapter>::SampleFormat,
      <Self::Adapter as AudioAdapter>::ChannelLayout,
      <Self::Adapter as AudioAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<(), Self::Error>;
  /// Signals EOF.
  fn send_eof(&mut self) -> Result<(), Self::Error>;
  /// Flushes internal state.
  fn flush(&mut self) -> Result<(), Self::Error>;
}

/// Pull-style audio frame source. Caller requests blocks by sample
/// offset.
///
/// Backends: R3D, BRAW (audio in companion track of the same clip).
pub trait AudioFrameSource {
  /// Backend vocabulary.
  type Adapter: AudioAdapter;
  /// Buffer type.
  type Buffer: AsRef<[u8]>;
  /// Backend-specific clip-level metadata.
  type ClipMeta;
  /// Decoder-specific error.
  type Error;
  /// Total sample count across all channels.
  fn sample_count(&self) -> u64;
  /// Sample rate (Hz).
  fn sample_rate(&self) -> u32;
  /// Channel count.
  fn channel_count(&self) -> u8;
  /// Backend-specific clip metadata.
  fn clip_meta(&self) -> &Self::ClipMeta;
  /// Decodes a block starting at `sample_offset`, of `sample_count` samples.
  fn decode_block(
    &mut self,
    sample_offset: u64,
    sample_count: u32,
    dst: &mut AudioFrame<
      <Self::Adapter as AudioAdapter>::SampleFormat,
      <Self::Adapter as AudioAdapter>::ChannelLayout,
      <Self::Adapter as AudioAdapter>::FrameExtra,
      Self::Buffer,
    >,
  ) -> Result<(), Self::Error>;
}

/// Push-style subtitle decoder. (No pull-style subtitle decoders
/// exist in the wild — subtitle streams are linear and small.)
pub trait SubtitleDecoder {
  /// Backend vocabulary.
  type Adapter: SubtitleAdapter;
  /// Buffer type.
  type Buffer: AsRef<[u8]>;
  /// Decoder-specific error.
  type Error;
  /// Submits a compressed subtitle packet.
  fn send_packet(
    &mut self,
    packet: &SubtitlePacket<<Self::Adapter as SubtitleAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<(), Self::Error>;
  /// Drains a decoded subtitle frame.
  fn receive_frame(
    &mut self,
    dst: &mut SubtitleFrame<<Self::Adapter as SubtitleAdapter>::FrameExtra, Self::Buffer>,
  ) -> Result<(), Self::Error>;
  /// Signals EOF.
  fn send_eof(&mut self) -> Result<(), Self::Error>;
  /// Flushes internal state.
  fn flush(&mut self) -> Result<(), Self::Error>;
}

/// One-shot still-image decoder — cover art, an embedded thumbnail, a
/// poster frame.
///
/// **No `Stream` in the name, and no rhythm to go with it.** An
/// attachment track delivers exactly one packet (see
/// [`Demuxer`](crate::demuxer::Demuxer)'s attachment contract), that
/// packet is a whole file, and decoding it produces exactly one
/// picture. There is nothing to queue, nothing to drain, and no
/// end-of-stream to signal — so [`decode`](Self::decode) takes the
/// packet and returns the frame, instead of the
/// `send_packet` / `receive_frame` split the two `*StreamDecoder`
/// traits need. [`SubtitleDecoder`] keeps that split only because
/// FFmpeg's own subtitle API is push-shaped; nothing forces it here.
///
/// **The frame has no timestamps**, because
/// [`ImageFrame`] has no seats for them.
///
/// # Where the input comes from
///
/// A container's still images arrive as
/// [`TrackKind::Attachment`](crate::demuxer::TrackKind::Attachment)
/// tracks: the track row says what codec the picture is in, and the
/// track's one [`AttachmentPacket`] carries its bytes. So the row
/// opens the decoder — off-trait, per the module docs — and the packet
/// is what `decode` is handed.
///
/// # `&mut self`
///
/// Decoding one image needs no state across calls; the exclusive
/// borrow is here because a backend's decoder handle is a mutable
/// resource (FFmpeg's `AVCodecContext` is), and because it lets a
/// backend reuse one open decoder across several attachments of the
/// same codec rather than reopening per picture.
pub trait ImageDecoder {
  /// Backend-specific vocabulary.
  type Adapter: ImageAdapter;
  /// Buffer type held by the packet this decoder accepts and the frame
  /// it produces. See the module docs for what may be bound here.
  type Buffer: AsRef<[u8]>;
  /// Decoder-specific error type.
  type Error;

  /// Decodes one attachment payload — a whole image file — into a
  /// still.
  ///
  /// Backends signal "these bytes are not a picture this decoder can
  /// read" through a backend-specific `Error` variant, never by
  /// returning an empty frame.
  fn decode(
    &mut self,
    packet: &AttachmentPacket<<Self::Adapter as ImageAdapter>::PacketExtra, Self::Buffer>,
  ) -> Result<
    ImageFrame<
      <Self::Adapter as ImageAdapter>::PixelFormat,
      <Self::Adapter as ImageAdapter>::FrameExtra,
      Self::Buffer,
    >,
    Self::Error,
  >;
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::Timebase;
  use core::num::NonZeroI32;

  pub(crate) struct VLoop;
  impl VideoAdapter for VLoop {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  /// Trivial loopback impl — confirms the trait can be implemented.
  pub(crate) struct LoopVideoStream;

  #[derive(Debug)]
  pub(crate) struct LoopError;

  impl VideoStreamDecoder for LoopVideoStream {
    type Adapter = VLoop;
    type Buffer = &'static [u8];
    type Error = LoopError;

    fn send_packet(&mut self, _: &VideoPacket<(), &'static [u8]>) -> Result<(), LoopError> {
      Ok(())
    }
    fn receive_frame(
      &mut self,
      _: &mut VideoFrame<u32, (), &'static [u8]>,
    ) -> Result<(), LoopError> {
      Err(LoopError)
    }
    fn send_eof(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
    fn flush(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
  }

  pub(crate) struct LoopVideoSource;

  impl VideoFrameSource for LoopVideoSource {
    type Adapter = VLoop;
    type Buffer = &'static [u8];
    type ClipMeta = ();
    type Error = LoopError;

    fn frame_count(&self) -> u64 {
      0
    }
    fn frame_rate(&self) -> Timebase {
      Timebase::new(30, NonZeroI32::new(1).unwrap())
    }
    fn duration(&self) -> Timestamp {
      Timestamp::new(0, self.frame_rate())
    }
    fn clip_meta(&self) -> &() {
      &()
    }
    fn decode_frame(
      &mut self,
      _: u64,
      _: &mut VideoFrame<u32, (), &'static [u8]>,
    ) -> Result<(), LoopError> {
      Err(LoopError)
    }
  }

  #[test]
  fn video_traits_are_implementable() {
    fn _stream<D: VideoStreamDecoder>() {}
    fn _source<D: VideoFrameSource>() {}
    _stream::<LoopVideoStream>();
    _source::<LoopVideoSource>();
  }

  pub(crate) struct ALoop;
  impl AudioAdapter for ALoop {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  pub(crate) struct LoopAudioStream;

  impl AudioStreamDecoder for LoopAudioStream {
    type Adapter = ALoop;
    type Buffer = &'static [u8];
    type Error = LoopError;
    fn send_packet(&mut self, _: &AudioPacket<(), &'static [u8]>) -> Result<(), LoopError> {
      Ok(())
    }
    fn receive_frame(
      &mut self,
      _: &mut AudioFrame<u32, u32, (), &'static [u8]>,
    ) -> Result<(), LoopError> {
      Err(LoopError)
    }
    fn send_eof(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
    fn flush(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
  }

  pub(crate) struct LoopAudioSource;

  impl AudioFrameSource for LoopAudioSource {
    type Adapter = ALoop;
    type Buffer = &'static [u8];
    type ClipMeta = ();
    type Error = LoopError;
    fn sample_count(&self) -> u64 {
      0
    }
    fn sample_rate(&self) -> u32 {
      48_000
    }
    fn channel_count(&self) -> u8 {
      2
    }
    fn clip_meta(&self) -> &() {
      &()
    }
    fn decode_block(
      &mut self,
      _: u64,
      _: u32,
      _: &mut AudioFrame<u32, u32, (), &'static [u8]>,
    ) -> Result<(), LoopError> {
      Err(LoopError)
    }
  }

  #[test]
  fn audio_traits_are_implementable() {
    fn _stream<D: AudioStreamDecoder>() {}
    fn _source<D: AudioFrameSource>() {}
    _stream::<LoopAudioStream>();
    _source::<LoopAudioSource>();
  }

  pub(crate) struct SLoop;
  impl SubtitleAdapter for SLoop {
    type CodecId = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  pub(crate) struct LoopSubtitleStream;

  impl SubtitleDecoder for LoopSubtitleStream {
    type Adapter = SLoop;
    type Buffer = &'static [u8];
    type Error = LoopError;
    fn send_packet(&mut self, _: &SubtitlePacket<(), &'static [u8]>) -> Result<(), LoopError> {
      Ok(())
    }
    fn receive_frame(&mut self, _: &mut SubtitleFrame<(), &'static [u8]>) -> Result<(), LoopError> {
      Err(LoopError)
    }
    fn send_eof(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
    fn flush(&mut self) -> Result<(), LoopError> {
      Ok(())
    }
  }

  #[test]
  fn subtitle_decoder_is_implementable() {
    fn _decoder<D: SubtitleDecoder>() {}
    _decoder::<LoopSubtitleStream>();
  }

  pub(crate) struct ILoop;
  impl ImageAdapter for ILoop {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  pub(crate) struct LoopImage;

  impl ImageDecoder for LoopImage {
    type Adapter = ILoop;
    type Buffer = &'static [u8];
    type Error = LoopError;

    fn decode(
      &mut self,
      _: &AttachmentPacket<(), &'static [u8]>,
    ) -> Result<ImageFrame<u32, (), &'static [u8]>, LoopError> {
      Err(LoopError)
    }
  }

  #[test]
  fn image_decoder_is_implementable() {
    fn _decoder<D: ImageDecoder>() {}
    _decoder::<LoopImage>();
  }

  #[test]
  fn the_one_shot_seam_takes_a_packet_and_answers_a_frame() {
    // The shape the register turns on: no `send_*`, no `receive_*`, no
    // `flush`. One call in, one picture out.
    let mut decoder = LoopImage;
    let packet: AttachmentPacket<(), &'static [u8]> = AttachmentPacket::new(&[][..], ());
    assert!(decoder.decode(&packet).is_err());
  }
}

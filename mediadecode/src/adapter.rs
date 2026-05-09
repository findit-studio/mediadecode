//! Adapter traits — the per-kind backend "vocabulary."
//!
//! A backend implements only the kinds it handles. R3D / BRAW /
//! ARRIRAW / X-OCN / Canon RAW Light implement only [`VideoAdapter`].
//! FFmpeg implements all three. The buffer type is **not** part of
//! these traits — it's a struct generic on `Packet` / `Frame` so the
//! same adapter can be used with different buffer types at different
//! call sites.

use core::fmt::Debug;

/// Backend vocabulary for compressed/decoded **video**.
pub trait VideoAdapter {
  /// Codec identifier (e.g. backend-specific newtype around
  /// FFmpeg `AVCodecID`, WebCodecs codec string, etc.).
  type CodecId: Copy + Eq + Debug;
  /// Pixel format identifier (e.g. backend-specific newtype around
  /// FFmpeg `AVPixelFormat`, WebCodecs `VideoPixelFormat`, RAW
  /// `VideoPixelType`, BRAW `BlackmagicRawResourceFormat`).
  type PixelFormat: Copy + Eq + Debug;
  /// Backend-specific extras carried on every `VideoPacket` (e.g.
  /// FFmpeg side-data, WebCodecs metadata).
  type PacketExtra;
  /// Backend-specific extras carried on every `VideoFrame` (e.g.
  /// HDR mastering display, RAW sensor metadata, picture type).
  type FrameExtra;
}

/// Backend vocabulary for compressed/decoded **audio**.
pub trait AudioAdapter {
  /// Codec identifier.
  type CodecId: Copy + Eq + Debug;
  /// Sample format identifier (e.g. FFmpeg `AVSampleFormat`,
  /// WebCodecs `AudioSampleFormat`).
  type SampleFormat: Copy + Eq + Debug;
  /// Channel layout identifier (FFmpeg `AVChannelLayout`,
  /// WebCodecs raw count, RAW SDK fixed layouts).
  type ChannelLayout: Clone + Eq + Debug;
  /// Backend-specific extras carried on every `AudioPacket`.
  type PacketExtra;
  /// Backend-specific extras carried on every `AudioFrame`.
  type FrameExtra;
}

/// Backend vocabulary for compressed/decoded **subtitles**.
pub trait SubtitleAdapter {
  /// Codec identifier.
  type CodecId: Copy + Eq + Debug;
  /// Backend-specific extras carried on every `SubtitlePacket`.
  type PacketExtra;
  /// Backend-specific extras carried on every `SubtitleFrame`.
  type FrameExtra;
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Zero-sized "loopback" adapter that implements all three traits
  /// with `()` extras. Proves the traits are object-safe-ish in the
  /// associated-type sense (i.e. they can be implemented).
  pub struct Loopback;

  impl VideoAdapter for Loopback {
    type CodecId = u32;
    type PixelFormat = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  impl AudioAdapter for Loopback {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  impl SubtitleAdapter for Loopback {
    type CodecId = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  #[test]
  fn loopback_compiles() {
    // The fact that this test compiles means the three traits
    // are implementable. No runtime assertions necessary.
    fn _video<A: VideoAdapter>() {}
    fn _audio<A: AudioAdapter>() {}
    fn _subtitle<A: SubtitleAdapter>() {}
    _video::<Loopback>();
    _audio::<Loopback>();
    _subtitle::<Loopback>();
  }
}

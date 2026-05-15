//! Per-packet / per-frame extras carried alongside the unified
//! `mediadecode` types.
//!
//! For now the extras are nearly empty — WebCodecs doesn't carry
//! the rich side-data FFmpeg does (HDR metadata is partially
//! exposed via `VideoColorSpaceInit`, but most fields land on the
//! `mediadecode` side already). The structs are reserved namespaces
//! so we can add fields later without breaking downstream code.
//!
//! All fields are private; access goes through getters /
//! `with_*` builders / `set_*` mutators. The API matches the
//! workspace-wide convention so future additional fields don't
//! force a breaking change on existing callers.

/// Backend-specific extras on a `VideoPacket`. Reserved for future
/// use (e.g. WebCodecs `EncodedVideoChunkType` metadata beyond
/// what `PacketFlags::KEY` already encodes).
#[derive(Debug, Default, Clone, Copy)]
pub struct VideoPacketExtra {
  /// `true` if the encoded chunk's `type` was `key`. Mirrored from
  /// `web_sys::EncodedVideoChunkType::Key` at packet construction.
  /// Also reflected in [`mediadecode::packet::PacketFlags::KEY`];
  /// keeping it here too lets boundary helpers round-trip without
  /// redundant flag inspection.
  key: bool,
}

impl VideoPacketExtra {
  /// Construct with the given key-flag. Default-equivalent
  /// constructor; `Self::default()` produces `new(false)`.
  pub const fn new(key: bool) -> Self {
    Self { key }
  }

  /// Whether the originating encoded chunk was a key chunk.
  pub const fn key(&self) -> bool {
    self.key
  }

  /// Builder-style setter — consumes and returns `self`.
  #[must_use]
  pub const fn with_key(mut self, key: bool) -> Self {
    self.key = key;
    self
  }

  /// In-place setter — returns `&mut self` for chaining.
  pub const fn set_key(&mut self, key: bool) -> &mut Self {
    self.key = key;
    self
  }
}

/// Backend-specific extras on a `VideoFrame`.
#[derive(Debug, Default, Clone, Copy)]
pub struct VideoFrameExtra {
  /// Whether this frame originated from a key-frame chunk. Useful
  /// for downstream pipelines that need to distinguish I-frames
  /// without re-parsing the bitstream.
  key: bool,
}

impl VideoFrameExtra {
  /// Construct with the given key-flag.
  pub const fn new(key: bool) -> Self {
    Self { key }
  }

  /// Whether this frame originated from a key-frame chunk.
  pub const fn key(&self) -> bool {
    self.key
  }

  /// Builder-style setter.
  #[must_use]
  pub const fn with_key(mut self, key: bool) -> Self {
    self.key = key;
    self
  }

  /// In-place setter.
  pub const fn set_key(&mut self, key: bool) -> &mut Self {
    self.key = key;
    self
  }
}

/// Backend-specific extras on an `AudioPacket`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AudioPacketExtra {
  /// `true` if the encoded chunk's `type` was `key`.
  key: bool,
}

impl AudioPacketExtra {
  /// Construct with the given key-flag.
  pub const fn new(key: bool) -> Self {
    Self { key }
  }

  /// Whether the originating encoded chunk was a key chunk.
  pub const fn key(&self) -> bool {
    self.key
  }

  /// Builder-style setter.
  #[must_use]
  pub const fn with_key(mut self, key: bool) -> Self {
    self.key = key;
    self
  }

  /// In-place setter.
  pub const fn set_key(&mut self, key: bool) -> &mut Self {
    self.key = key;
    self
  }
}

/// Backend-specific extras on an `AudioFrame`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AudioFrameExtra {
  /// Whether this frame originated from a key-frame chunk.
  key: bool,
}

impl AudioFrameExtra {
  /// Construct with the given key-flag.
  pub const fn new(key: bool) -> Self {
    Self { key }
  }

  /// Whether this frame originated from a key-frame chunk.
  pub const fn key(&self) -> bool {
    self.key
  }

  /// Builder-style setter.
  #[must_use]
  pub const fn with_key(mut self, key: bool) -> Self {
    self.key = key;
    self
  }

  /// In-place setter.
  pub const fn set_key(&mut self, key: bool) -> &mut Self {
    self.key = key;
    self
  }
}

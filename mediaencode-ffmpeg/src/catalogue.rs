//! The encoders this crate opens by name, and the two gates in front of
//! them.
//!
//! An encoder is named to this crate by its FFmpeg name (`"libx264"`,
//! `"h264_videotoolbox"`, `"aac"`) and passes two gates before anything
//! opens it:
//!
//! 1. **The catalogue and its license gate** ([`lookup`]): the name must
//!    be one [`CATALOGUE`] knows, and a [`License::Gpl`] encoder passes
//!    only when this crate is built with its `gpl` feature. This gate is
//!    pure: it asks FFmpeg nothing, so a default build never asks
//!    libavcodec for a GPL encoder, even when the linked FFmpeg carries
//!    one.
//! 2. **The linked libavcodec** ([`encoder_by_name`]): the FFmpeg this
//!    crate links must carry an encoder by that name — `libx264` exists
//!    only in an FFmpeg built with it, VideoToolbox only on Apple
//!    platforms. A name it lacks is refused by name, never replaced by
//!    another encoder.
//!
//! What passes both is an [`Admitted`] encoder, the only way to name one
//! to this crate's machines.
//!
//! # What the `gpl` feature does not decide
//!
//! The license of your binary. Linking an FFmpeg built with
//! `--enable-gpl` — Homebrew's is — makes the binary that links it GPL,
//! whatever this crate's features say. The feature decides only whether
//! this crate will ask for a GPL encoder by name; it is how a build that
//! links an LGPL FFmpeg, or that must not depend on GPL code, keeps every
//! road to one closed.

use mediaio_ffmpeg::MediaKind;
use smol_bytes::Utf8Bytes;

/// The license an encoder's code puts on a binary that links it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum License {
  /// The LGPL, or a license more permissive than it (`libopenh264` is
  /// BSD): linking the encoder keeps an LGPL FFmpeg's terms.
  Lgpl,
  /// The GPL: a binary that links the encoder is GPL.
  Gpl,
}

/// One encoder the catalogue knows by name.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
  name: &'static str,
  kind: MediaKind,
  license: License,
  hardware: bool,
}

impl Entry {
  const fn new(name: &'static str, kind: MediaKind, license: License, hardware: bool) -> Self {
    Self {
      name,
      kind,
      license,
      hardware,
    }
  }

  /// The encoder's FFmpeg name.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn name(&self) -> &'static str {
    self.name
  }
  /// What the encoder encodes: [`MediaKind::Video`] or
  /// [`MediaKind::Audio`].
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn kind(&self) -> MediaKind {
    self.kind
  }
  /// The license the encoder's code puts on a binary that links it.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn license(&self) -> License {
    self.license
  }
  /// Whether the encoder runs on a hardware engine, and so may be
  /// unavailable on a machine that has none even where it is linked.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn is_hardware(&self) -> bool {
    self.hardware
  }
}

/// Every encoder this crate opens by name. A name not here is refused,
/// whatever the linked FFmpeg carries: the license of an encoder this
/// list does not know cannot be vouched for.
pub const CATALOGUE: &[Entry] = &[
  Entry::new("h264_videotoolbox", MediaKind::Video, License::Lgpl, true),
  Entry::new("hevc_videotoolbox", MediaKind::Video, License::Lgpl, true),
  Entry::new("prores_videotoolbox", MediaKind::Video, License::Lgpl, true),
  Entry::new("prores_ks", MediaKind::Video, License::Lgpl, false),
  Entry::new("libopenh264", MediaKind::Video, License::Lgpl, false),
  Entry::new("libx264", MediaKind::Video, License::Gpl, false),
  Entry::new("libx264rgb", MediaKind::Video, License::Gpl, false),
  Entry::new("libx265", MediaKind::Video, License::Gpl, false),
  Entry::new("aac", MediaKind::Audio, License::Lgpl, false),
  Entry::new("aac_at", MediaKind::Audio, License::Lgpl, false),
];

/// Whether this build lets a GPL encoder through the license gate.
const GPL: bool = cfg!(feature = "gpl");

/// An encoder that passed both gates: the catalogue knows it, its
/// license is one this build admits, and the linked libavcodec carries
/// it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Admitted {
  entry: &'static Entry,
}

impl Admitted {
  /// The catalogue's entry for the encoder.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn entry(&self) -> &'static Entry {
    self.entry
  }
  /// The encoder's FFmpeg name.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn name(&self) -> &'static str {
    self.entry.name
  }
}

/// Why a name was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EncoderRefusal {
  /// The catalogue does not know the name.
  #[error("`{name}` is not an encoder this crate opens by name")]
  Unknown {
    /// The name as it was given.
    name: Utf8Bytes,
  },
  /// A GPL encoder, and this crate was built without its `gpl` feature.
  #[error("`{name}` is a GPL encoder, reachable only with this crate's `gpl` feature")]
  Gpl {
    /// The encoder's FFmpeg name.
    name: &'static str,
  },
  /// The linked libavcodec has no encoder by that name.
  #[error("the linked FFmpeg has no `{name}` encoder")]
  NotLinked {
    /// The encoder's FFmpeg name.
    name: &'static str,
  },
}

/// The first gate: finds `name` in [`CATALOGUE`] and refuses a GPL
/// encoder unless this crate was built with its `gpl` feature. Asks
/// FFmpeg nothing.
pub fn lookup(name: &str) -> Result<&'static Entry, EncoderRefusal> {
  let entry = CATALOGUE
    .iter()
    .find(|entry| entry.name == name)
    .ok_or_else(|| EncoderRefusal::Unknown {
      name: Utf8Bytes::from(name),
    })?;
  if entry.license == License::Gpl && !GPL {
    return Err(EncoderRefusal::Gpl { name: entry.name });
  }
  Ok(entry)
}

/// Both gates: [`lookup`], then the linked libavcodec.
///
/// libavcodec is asked by the catalogue entry's own name, so a caller's
/// string never reaches FFmpeg — only names this crate wrote.
pub fn encoder_by_name(name: &str) -> Result<Admitted, EncoderRefusal> {
  let entry = lookup(name)?;
  if ffmpeg_next::encoder::find_by_name(entry.name).is_none() {
    return Err(EncoderRefusal::NotLinked { name: entry.name });
  }
  Ok(Admitted { entry })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_catalogues_names_are_unique_and_plain() {
    for (i, entry) in CATALOGUE.iter().enumerate() {
      assert!(!entry.name().is_empty());
      assert!(
        entry.name().bytes().all(|b| b.is_ascii_graphic()),
        "{} must be a plain FFmpeg name",
        entry.name()
      );
      assert!(
        CATALOGUE[..i]
          .iter()
          .all(|other| other.name() != entry.name()),
        "{} is listed twice",
        entry.name()
      );
      assert!(matches!(entry.kind(), MediaKind::Video | MediaKind::Audio));
    }
  }

  /// A name the catalogue does not know is refused by that name — a
  /// non-free encoder, a typo, a name with a NUL in it alike — and none
  /// of them reaches FFmpeg.
  #[test]
  fn an_unknown_name_is_refused_by_name() {
    for name in ["libfdk_aac", "x264", "aac\0", ""] {
      match encoder_by_name(name) {
        Err(EncoderRefusal::Unknown { name: refused }) => assert_eq!(refused, name),
        other => panic!("{name:?} should be unknown, got {other:?}"),
      }
    }
  }

  #[test]
  fn every_lgpl_entry_passes_the_license_gate() {
    for entry in CATALOGUE.iter().filter(|e| e.license() == License::Lgpl) {
      assert_eq!(lookup(entry.name()), Ok(entry), "{}", entry.name());
    }
  }

  /// **The GPL refusal law.** Without the `gpl` feature every GPL entry
  /// is refused by its name at the license gate.
  #[cfg(not(feature = "gpl"))]
  #[test]
  fn every_gpl_entry_is_refused_by_name_without_the_feature() {
    let gpl: Vec<&Entry> = CATALOGUE
      .iter()
      .filter(|e| e.license() == License::Gpl)
      .collect();
    assert!(!gpl.is_empty());
    for entry in gpl {
      assert_eq!(
        lookup(entry.name()),
        Err(EncoderRefusal::Gpl { name: entry.name() })
      );
    }
  }

  /// With the feature, the same entries pass the license gate.
  #[cfg(feature = "gpl")]
  #[test]
  fn every_gpl_entry_passes_the_license_gate_with_the_feature() {
    for entry in CATALOGUE.iter().filter(|e| e.license() == License::Gpl) {
      assert_eq!(lookup(entry.name()), Ok(entry), "{}", entry.name());
    }
  }

  /// FFmpeg's own AAC encoder is in every libavcodec, so the second gate
  /// answers it: the linked library is asked, and says yes.
  #[test]
  fn aac_is_admitted_by_the_linked_libavcodec() {
    let admitted = encoder_by_name("aac").expect("every libavcodec carries aac");
    assert_eq!(admitted.name(), "aac");
    assert_eq!(admitted.entry().kind(), MediaKind::Audio);
  }

  /// **The refusal comes before libavcodec is asked.** A full FFmpeg
  /// build (Homebrew's) carries `libx264`, so had libavcodec been asked
  /// first the answer would be an admission; it is the license gate's
  /// refusal instead.
  #[cfg(not(feature = "gpl"))]
  #[test]
  fn a_gpl_name_is_refused_before_libavcodec_is_asked() {
    assert_eq!(
      encoder_by_name("libx264"),
      Err(EncoderRefusal::Gpl { name: "libx264" })
    );
  }

  /// With the feature, a GPL name reaches libavcodec, which answers
  /// whether it carries the encoder — never with the license refusal.
  #[cfg(feature = "gpl")]
  #[test]
  fn a_gpl_name_reaches_libavcodec_with_the_feature() {
    match encoder_by_name("libx264") {
      Ok(admitted) => assert_eq!(admitted.name(), "libx264"),
      Err(refusal) => assert_eq!(refusal, EncoderRefusal::NotLinked { name: "libx264" }),
    }
  }
}

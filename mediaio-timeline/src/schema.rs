//! The document's schema word.

use core::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Which shape of timeline document a value is.
///
/// A stored [`Timeline`](crate::Timeline) carries it as the number under
/// `schema`, written as the document's **first** field, so a reader meets it
/// before any field whose shape a later schema may change. A number this
/// reader does not know is refused by name — `unknown timeline schema 2` —
/// rather than read as the nearest shape it does know.
///
/// A word that changes what a document means arrives with a new schema:
/// time-warp (`speed`) is reserved for one, and is absent from schema 1.
///
/// Marked `#[non_exhaustive]`: a later schema joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Schema {
  /// Schema 1: explicit record ranges, gaps derived, no time-warp.
  V1,
}

impl Schema {
  /// The number a document carries for this schema.
  pub const fn number(self) -> u64 {
    match self {
      Self::V1 => 1,
    }
  }

  /// The schema `number` names, or `None` for one this reader does not know.
  pub const fn from_number(number: u64) -> Option<Self> {
    match number {
      1 => Some(Self::V1),
      _ => None,
    }
  }
}

/// Writes the schema's number: `1`.
impl fmt::Display for Schema {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.number())
  }
}

impl Serialize for Schema {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_u64(self.number())
  }
}

impl<'de> Deserialize<'de> for Schema {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let number = u64::deserialize(deserializer)?;
    Self::from_number(number).ok_or_else(|| {
      de::Error::custom(format_args!(
        "unknown timeline schema {number}: this reader knows schema 1"
      ))
    })
  }
}

//! A document's own notes.

use alloc::{
  collections::{BTreeMap, btree_map::Entry},
  string::String,
};
use core::fmt;

use serde::{
  Deserialize, Deserializer, Serialize, Serializer,
  de::{self, MapAccess, Visitor},
};

/// An ordered map from string words to string values: notes a document
/// carries for its own readers.
///
/// The model never interprets an entry. Entries iterate in key order, so a
/// map writes the same bytes however it was built. On the wire it is a plain
/// object of strings, and an object naming one key twice is refused by name
/// rather than letting the last value win.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Metadata(BTreeMap<String, String>);

impl Metadata {
  /// An empty map.
  pub const fn new() -> Self {
    Self(BTreeMap::new())
  }

  /// The value under `key`, if any.
  pub fn get(&self, key: &str) -> Option<&str> {
    self.0.get(key).map(String::as_str)
  }

  /// Sets `key` to `value`, returning the value it replaced.
  pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) -> Option<String> {
    self.0.insert(key.into(), value.into())
  }

  /// Sets `key` to `value` (consuming builder).
  #[must_use]
  pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
    self.insert(key, value);
    self
  }

  /// Removes `key`, returning its value.
  pub fn remove(&mut self, key: &str) -> Option<String> {
    self.0.remove(key)
  }

  /// The entries, in key order.
  pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
    self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
  }

  /// How many entries the map holds.
  pub fn len(&self) -> usize {
    self.0.len()
  }

  /// Whether the map holds no entry.
  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for Metadata {
  /// Collects the pairs; a later pair under a key already seen replaces it.
  fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
    Self(
      iter
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect(),
    )
  }
}

impl Serialize for Metadata {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    self.0.serialize(serializer)
  }
}

impl<'de> Deserialize<'de> for Metadata {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    deserializer.deserialize_map(MetadataVisitor)
  }
}

struct MetadataVisitor;

impl<'de> Visitor<'de> for MetadataVisitor {
  type Value = Metadata;

  fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("an object of string values")
  }

  fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Metadata, A::Error> {
    let mut entries = BTreeMap::new();
    while let Some((key, value)) = map.next_entry::<String, String>()? {
      match entries.entry(key) {
        Entry::Vacant(slot) => {
          slot.insert(value);
        }
        Entry::Occupied(slot) => {
          return Err(de::Error::custom(format_args!(
            "duplicate metadata key `{}`",
            slot.key()
          )));
        }
      }
    }
    Ok(Metadata(entries))
  }
}

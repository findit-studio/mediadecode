//! **This crate's self-granted [`ingraph`] citizenship** — the faces the
//! indexing framework reads its vocabulary types through, written here
//! because here is where those types live.
//!
//! Nothing in this module is reachable without the `ingraph` feature,
//! and nothing outside it changes when the feature is on.
//!
//! # Why the faces belong on this side
//!
//! Three things have to be attached to a type before a declaration can
//! hold a column of it — how the column is READ (its filter and the
//! interpretation it takes when a declaration names none), what it IS in
//! storage, and how a keyset cursor pages past it. Every one of those is
//! a foreign trait, so only two crates may write them: the framework, or
//! the crate that owns the type.
//!
//! For a while it was the framework, and downstream of it a consumer
//! kept a **mirror** — a local enum restating this crate's bits, with a
//! crossing in each direction and a drift pin over the pair. That works
//! and it costs a permanent restatement: one that has to be edited every
//! time the upstream grows a bit, in a crate that has no reason to know
//! the bit exists.
//!
//! Self-granting removes the restatement instead of maintaining it. The
//! rows below are ordinary trait work — this crate's own type, the
//! framework's traits, orphan-clean — and they leave the vocabulary
//! exactly where it was: nothing about [`PacketFlags`](crate::packet::PacketFlags)
//! changes shape, nothing about it is decided by `ingraph`, and a build
//! without the feature has never heard of it.
//!
//! # The roster is ONE type, and it is a census rather than a start
//!
//! [`PacketFlags`](crate::packet::PacketFlags) is the whole of it: it is
//! the only type of this crate's that a consumer mirrors today
//! (`mediagraph`'s `types::packet::PacketFlags`, declared
//! `#[ingraph::flags(u8, remote = "mediadecode::packet::PacketFlags")]`).
//! The rest of this crate's public surface is packets, frames and
//! sessions — values a graph moves *through* a node, not values a row
//! stores — and a citizenship for one of those would be surface nobody
//! asked for.
//!
//! # What a `flags` citizenship is, in rows
//!
//! | row | what asks for it |
//! |---|---|
//! | `FlagsValue` | every wire face — it carries the schema name and the field each bit publishes under |
//! | `FlagsFilterMarker` | the column's filter: the set-theoretic words a bit set answers |
//! | `DefaultMarker` / `DefaultVecMarker` | the inference, so a column of this type needs no word in the declaration to be read |
//! | `CursorValue` | the keyset cursor, which every persisted column is audited for |
//! | `ColumnKind` / `ColumnEq` | the column's width, and when two of these values are one column value |
//!
//! The bit TABLE is not among them: [`bitflags::Flags`] already carries
//! every bit with its word, and `FlagsValue` sits on that trait rather
//! than restating it. That is the property this whole module rests on —
//! a bit added to `PacketFlags` reaches the framework's faces with
//! nothing here to edit, which is exactly what a mirror could not do.
//!
//! # What it does NOT carry, and where that line is
//!
//! The **storage bind** (`sqlx`'s `Type`/`Encode`/`Decode` and the
//! per-dialect carrier) and the **wire seats** (`ToGraphqlOutput` /
//! `ToGraphqlInput`) are absent. Both ride features of `ingraph`'s that
//! name a backend or a wire library, and this crate takes `ingraph`
//! without them — see the workspace manifest's row, which also records
//! why the pin says `flags, runtime` where the rows below need only the
//! first. A media decoder that pulled a SQL driver and a GraphQL
//! runtime into its dependency graph to describe three bits would be
//! paying for a build it never runs.
//!
//! The rows are reachable when somebody wants them: `ingraph` publishes
//! `if_sqlite!`, `if_postgres!`, `if_mysql!` and `if_mongo!` precisely
//! so a per-backend half can be written beside a declaration and expand
//! only where that backend is compiled in. This module writes none of
//! them because no consumer has asked; a consumer that does can say so
//! on the ticket rather than discovering the gap.

use ingraph::{
  ColumnEq, ColumnKind, CursorValue, DefaultMarker, DefaultVecMarker, FlagsFilterMarker,
  FlagsMarker, FlagsValue, ListMarker,
};

use crate::packet::PacketFlags;

/// The schema name and the per-bit wire fields.
///
/// The two facts `bitflags` has no reason to carry, and the only two
/// this crate writes: the bit table itself is
/// [`bitflags::Flags::FLAGS`], which `PacketFlags` already has.
///
/// `FIELDS` is parallel to that table — same order, one entry per bit —
/// and the framework zips the two. The words are the constants'
/// (`KEY`, `CORRUPT`, `DISCARD`); the fields are those words under
/// GraphQL's own casing convention, which is the single difference
/// between the two lists and the reason there are two.
impl FlagsValue for PacketFlags {
  const GRAPHQL_NAME: &'static str = "PacketFlags";
  const FIELDS: &'static [&'static str] = &["key", "corrupt", "discard"];
}

/// The filter a column of these bits gets — the set-theoretic words,
/// which is the whole reason a flags column is not an enum column.
impl FlagsFilterMarker for PacketFlags {
  type Filter = ingraph::FlagsFilter<Self>;
}

/// How a column of this type is read when a declaration names no
/// interpretation: as flags, which is what it is.
impl DefaultMarker for PacketFlags {
  type Marker = FlagsMarker<Self>;
}

/// And how a *collection* of them is read — a list of flags values, each
/// element under the reading above.
impl DefaultVecMarker for PacketFlags {
  type Marker = ListMarker<Vec<Self>, FlagsMarker<Self>>;
}

/// The keyset cursor's byte form: the bit pattern, big-endian, width-
/// checked coming back.
///
/// A cursor is a string a **client** hands back, so the bytes are
/// arbitrary and the read checks its own width and its own domain
/// before answering. `from_bits` rather than `from_bits_retain`: a
/// pattern carrying a bit this type does not declare is refused, because
/// returning a plausible value for bytes this type never wrote is how a
/// forged cursor becomes a page.
///
/// The pattern and not a word, because a flags column stores the
/// pattern in every face — so there is no canonical spelling to choose
/// between, the way a vocabulary's cursor has to.
impl CursorValue for PacketFlags {
  fn write_cursor(&self, out: &mut Vec<u8>) {
    out.extend_from_slice(&self.bits().to_be_bytes());
  }

  fn read_cursor(bytes: &[u8]) -> Option<Self> {
    let [byte] = <[u8; 1]>::try_from(bytes).ok()?;
    Self::from_bits(u8::from_be_bytes([byte]))
  }
}

/// One column, whatever a dialect widens it to.
impl ColumnKind for PacketFlags {}

/// Two of these are one column value when their bits are — which is
/// what the `PartialEq` the type already derives says.
impl ColumnEq for PacketFlags {
  #[inline]
  fn column_eq(&self, other: &Self) -> bool {
    self == other
  }
}

#[cfg(test)]
mod tests;

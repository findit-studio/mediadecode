use super::*;

/// **One table, read twice.**
///
/// The word a bit is spelled with comes from `bitflags`' own table and
/// the field a client selects it by comes from [`FlagsValue::FIELDS`],
/// and the framework zips them. A pair that drifted — an added bit, a
/// renamed constant — would show up here as a mismatched word, which is
/// the one failure two parallel lists can have.
#[test]
fn every_bit_carries_its_word_and_its_wire_field() {
  let table: Vec<_> = ingraph::flags::bits::<PacketFlags>()
    .map(|bit| (bit.word, bit.field, bit.value))
    .collect();

  assert_eq!(
    table
      .iter()
      .map(|(word, field, _)| (*word, *field))
      .collect::<Vec<_>>(),
    [
      ("KEY", "key"),
      ("CORRUPT", "corrupt"),
      ("DISCARD", "discard"),
    ],
  );
  assert_eq!(
    table.iter().map(|(_, _, value)| *value).collect::<Vec<_>>(),
    [PacketFlags::KEY, PacketFlags::CORRUPT, PacketFlags::DISCARD],
  );
}

/// The field list is **exactly as long as the bit table**, which is what
/// makes the zip above total.
///
/// The framework zips rather than indexes, so a short list loses its
/// tail in silence — a bit that exists, is stored, and has no field for
/// a client to ask it by. This is the assertion that would fire the day
/// a bit is added here and the field list is not.
#[test]
fn the_field_list_names_every_declared_bit() {
  use bitflags::Flags as _;

  assert_eq!(
    <PacketFlags as FlagsValue>::FIELDS.len(),
    PacketFlags::FLAGS.len(),
    "a field per bit, or the framework's zip drops the tail",
  );
}

/// The schema publishes the type under its own Rust name — the operand
/// scalar and the filter input object are composed from this one string,
/// so it is the only name a client ever sees.
#[test]
fn the_schema_name_is_the_types_own() {
  assert_eq!(<PacketFlags as FlagsValue>::GRAPHQL_NAME, "PacketFlags");
}

/// A cursor round-trips every combination of the declared bits.
#[test]
fn the_cursor_round_trips_every_declared_pattern() {
  for bits in 0..=0b111u8 {
    let value = PacketFlags::from_bits(bits).expect("every pattern under the mask is declared");
    let mut out = Vec::new();
    value.write_cursor(&mut out);
    assert_eq!(out, [bits], "one big-endian byte, and nothing framing it");
    assert_eq!(PacketFlags::read_cursor(&out), Some(value));
  }
}

/// **A forged cursor is refused, twice over.**
///
/// A cursor is a string a client hands back, so its bytes are arbitrary.
/// The width is checked — an empty slice and a two-byte one are not this
/// type's encoding whatever they hold — and so is the domain: FFmpeg
/// carries `AV_PKT_FLAG_TRUSTED` (`0b0_1000`), a bit this table does not
/// declare, and a pattern holding it names no value this type can spell.
#[test]
fn a_cursor_this_type_did_not_write_is_refused() {
  assert_eq!(PacketFlags::read_cursor(&[]), None, "no bytes, no value");
  assert_eq!(
    PacketFlags::read_cursor(&[0, 1]),
    None,
    "two bytes are not this type's one",
  );
  assert_eq!(
    PacketFlags::read_cursor(&[0b0_1000]),
    None,
    "an undeclared bit is refused rather than retained — a value the declaration cannot \
     spell must not come back out of a client's string",
  );
}

/// Column equality is the bits', which is what the type's own `PartialEq`
/// already says. Stated as a test because the row delegates, and a
/// delegation that stopped agreeing with its source would be invisible.
#[test]
fn two_values_are_one_column_value_when_their_bits_are() {
  let key = PacketFlags::KEY;
  let also_key = PacketFlags::from_bits_truncate(0b001);
  let discard = PacketFlags::DISCARD;

  assert!(key.column_eq(&also_key));
  assert!(!key.column_eq(&discard));
  assert!(PacketFlags::empty().column_eq(&PacketFlags::empty()));
}

/// The citizenship's **resolution**, asked the way the framework asks
/// it: a column of this type infers the flags reading, and its filter is
/// the flags filter, with nothing written at the declaration.
///
/// Two associated types checked by construction — the functions never
/// run, and they fail to compile if either row goes missing or names
/// something else.
#[test]
fn a_column_of_these_bits_needs_no_word_to_be_read() {
  fn _infers_the_flags_reading(_: <PacketFlags as DefaultMarker>::Marker) {}
  fn _infers_the_flags_filter(_: <PacketFlags as FlagsFilterMarker>::Filter) {}
  fn _a_collection_infers_a_list(_: <PacketFlags as DefaultVecMarker>::Marker) {}

  // A `ColumnKind` with no `SEGMENTS` of its own is the scalar answer:
  // one column, whatever a dialect widens it to.
  assert!(<PacketFlags as ColumnKind>::SEGMENTS.is_empty());
}

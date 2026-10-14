//! Lossy text out of a container's bytes, priced before it is built.
//!
//! The metadata road and the channel-label road share these two: a
//! second copy of a lossy-decoding rule would be a second rule.

use std::collections::TryReserveError;

use smol_bytes::Utf8Bytes;

/// The length `String::from_utf8_lossy` would produce for `bytes`,
/// **without producing it**.
///
/// The charge has to be the decoded size rather than the raw one:
/// lossy decoding replaces each invalid sequence with `U+FFFD`, three
/// bytes, so a value of invalid single bytes triples on the way in. It
/// also has to be knowable before anything is allocated, which rules
/// out decoding first and measuring afterwards.
///
/// Exactness is not decorative — an approximation would either
/// under-charge the budget or refuse ordinary text — so agreement with
/// `from_utf8_lossy` is asserted directly in the unit lanes rather
/// than argued here.
pub fn lossy_len(bytes: &[u8]) -> usize {
  const REPLACEMENT: usize = char::REPLACEMENT_CHARACTER.len_utf8();

  let mut rest = bytes;
  let mut total = 0usize;
  loop {
    match std::str::from_utf8(rest) {
      Ok(valid) => return total + valid.len(),
      Err(fault) => {
        total += fault.valid_up_to() + REPLACEMENT;
        match fault.error_len() {
          // An invalid sequence of `skip` bytes becomes one `U+FFFD`.
          Some(skip) => rest = &rest[fault.valid_up_to() + skip..],
          // A truncated trailing sequence: one `U+FFFD`, and the end.
          None => return total,
        }
      }
    }
  }
}

/// Decodes measured bytes into owned text, through a buffer reserved
/// **fallibly**.
///
/// `decoded` is [`lossy_len`]'s answer for the same bytes, so the one
/// reservation here is exact and the pushes that follow cannot grow
/// it.
///
/// # Nothing is copied, and nothing allocates after the charge
///
/// The buffer is reserved once, fallibly, at the exact decoded size,
/// and then **moved** into the carrier: `Utf8Bytes::from(String)`
/// keeps a short value inline (`smol_bytes::INLINE_CAP`, no allocation
/// at all) and hands a longer one to `bytes::Bytes::from(Vec<u8>)`,
/// which takes the vector's own allocation over. The `Utf8Bytes` this
/// replaced copied into a fresh `Arc<str>` instead — a second
/// allocation, infallible, of an attacker-sized value, made while the
/// first was still live, so failing it aborted the process that the
/// budget above existed to keep alive.
///
/// **The one residue, stated exactly.** `Bytes::from(Vec<u8>)` moves
/// the buffer outright when the vector's length equals its capacity,
/// which `try_reserve_exact` followed by exactly `decoded` bytes is
/// what produces; should an allocator hand back more capacity than was
/// asked for, `bytes` allocates a fixed-size reference-count header —
/// thirty-two bytes, the same for a ten-byte title and a sixty-four
/// kibibyte one. What is gone is the part an attacker could scale.
pub fn lossy_text(bytes: &[u8], decoded: usize) -> Result<Utf8Bytes, TryReserveError> {
  let mut buffer = std::string::String::new();
  buffer.try_reserve_exact(decoded)?;

  let mut rest = bytes;
  loop {
    match std::str::from_utf8(rest) {
      Ok(valid) => {
        buffer.push_str(valid);
        break;
      }
      Err(fault) => {
        let (valid, after) = rest.split_at(fault.valid_up_to());
        buffer.push_str(
          std::str::from_utf8(valid).expect("valid_up_to bounds a valid prefix by definition"),
        );
        buffer.push(char::REPLACEMENT_CHARACTER);
        match fault.error_len() {
          Some(skip) => rest = &after[skip..],
          None => break,
        }
      }
    }
  }
  debug_assert_eq!(
    buffer.len(),
    decoded,
    "lossy_len must price exactly what lossy_text builds",
  );
  // The move. Nothing past this point copies the value.
  Ok(Utf8Bytes::from(buffer))
}

#[cfg(test)]
mod tests {
  use super::*;

  /// **`lossy_len` prices exactly what `from_utf8_lossy` builds.**
  ///
  /// The budget charge is made from this number *before* anything is
  /// decoded, so a disagreement would either under-charge the budget —
  /// the hostile case, where bytes that are not UTF-8 triple on the way
  /// through — or refuse ordinary text. Asserted against the real
  /// decoder rather than argued, over the shapes that differ: valid
  /// ASCII and multi-byte text, a lone invalid byte, a run of them, an
  /// invalid sequence between valid text, and a truncated trailing
  /// sequence (which `error_len() == None` reports and which becomes
  /// exactly one replacement).
  #[test]
  fn lossy_len_prices_exactly_what_lossy_text_builds() {
    let cases: [&[u8]; 9] = [
      b"",
      b"Opening",
      "héllo wörld".as_bytes(),
      b"\xff",
      b"\xff\xfe\xfd",
      b"before\xffafter",
      b"\xe2\x82",           // truncated three-byte sequence
      b"ok\xe2\x82",         // ... after valid text
      b"\xf0\x9f\x92\xa9ok", // a real four-byte sequence, untouched
    ];
    for raw in cases {
      let built = std::string::String::from_utf8_lossy(raw);
      assert_eq!(
        lossy_len(raw),
        built.len(),
        "{raw:?} must be priced at what from_utf8_lossy produces",
      );
      let text = lossy_text(raw, lossy_len(raw)).expect("a small reservation");
      assert_eq!(
        text.as_str(),
        built.as_ref(),
        "{raw:?} must decode identically"
      );
    }
  }
}

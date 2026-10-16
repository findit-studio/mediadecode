//! A small JSON value: written pretty for the export, read strictly for the
//! self-check.

use core::{cmp::Ordering, fmt::Write as _};
use smol_bytes::{Utf8Bytes, Utf8BytesMut};
use std::{collections::BTreeSet, vec::Vec};

use super::Syntax;

/// A JSON value. A number is kept as the text that spells it, so the writer
/// puts down exactly the digits it was handed and the reader keeps exactly
/// the digits it read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
  Null,
  Bool(bool),
  Number(Utf8Bytes),
  String(Utf8Bytes),
  Array(Vec<Value>),
  Object(Vec<(Utf8Bytes, Value)>),
}

impl Value {
  pub(crate) fn as_object(&self) -> Option<&[(Utf8Bytes, Value)]> {
    match self {
      Self::Object(members) => Some(members),
      _ => None,
    }
  }

  pub(crate) fn as_array(&self) -> Option<&[Value]> {
    match self {
      Self::Array(items) => Some(items),
      _ => None,
    }
  }

  pub(crate) fn as_str(&self) -> Option<&str> {
    match self {
      Self::String(text) => Some(text.as_str()),
      _ => None,
    }
  }

  /// The number's value, read through `f64`.
  pub(crate) fn as_f64(&self) -> Option<f64> {
    match self {
      Self::Number(text) => text.parse().ok(),
      _ => None,
    }
  }
}

/// Writes `value` with four spaces of indent per level and `": "` after a
/// key — the layout OpenTimelineIO's own writer uses — without a final
/// newline.
pub(crate) fn write_pretty(value: &Value, out: &mut Utf8BytesMut) {
  write_value(value, 0, out);
}

fn write_value(value: &Value, depth: usize, out: &mut Utf8BytesMut) {
  match value {
    Value::Null => out.push_str("null"),
    Value::Bool(true) => out.push_str("true"),
    Value::Bool(false) => out.push_str("false"),
    Value::Number(text) => out.push_str(text),
    Value::String(text) => write_string(text, out),
    Value::Array(items) if items.is_empty() => out.push_str("[]"),
    Value::Array(items) => {
      out.push('[');
      for (index, item) in items.iter().enumerate() {
        if index > 0 {
          out.push(',');
        }
        newline(depth + 1, out);
        write_value(item, depth + 1, out);
      }
      newline(depth, out);
      out.push(']');
    }
    Value::Object(members) if members.is_empty() => out.push_str("{}"),
    Value::Object(members) => {
      out.push('{');
      for (index, (key, item)) in members.iter().enumerate() {
        if index > 0 {
          out.push(',');
        }
        newline(depth + 1, out);
        write_string(key, out);
        out.push_str(": ");
        write_value(item, depth + 1, out);
      }
      newline(depth, out);
      out.push('}');
    }
  }
}

fn newline(depth: usize, out: &mut Utf8BytesMut) {
  out.push('\n');
  for _ in 0..depth {
    out.push_str("    ");
  }
}

/// Writes `text` as a JSON string: the quote, the backslash and the control
/// characters escaped, everything else as it is.
fn write_string(text: &str, out: &mut Utf8BytesMut) {
  out.push('"');
  for c in text.chars() {
    match c {
      '"' => out.push_str("\\\""),
      '\\' => out.push_str("\\\\"),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      '\u{8}' => out.push_str("\\b"),
      '\u{c}' => out.push_str("\\f"),
      c if u32::from(c) < 0x20 => {
        let _ = write!(out, "\\u{:04x}", u32::from(c));
      }
      c => out.push(c),
    }
  }
  out.push('"');
}

/// A JSON number's text as the exact decimal it spells: its sign, its
/// significant digits — none for a zero — and the power of ten the last of
/// them counts.
struct Decimal<'a> {
  negative: bool,
  integer: &'a str,
  fraction: &'a str,
  /// The leading zeros of the digits, which say nothing.
  lead: usize,
  /// How many digits are significant, after the leading zeros.
  significant: usize,
  /// The power of ten the last significant digit counts.
  scale: i128,
}

impl Decimal<'_> {
  /// The significant digits, the integer part's and the fraction's as one.
  fn digits(&self) -> impl Iterator<Item = u8> + '_ {
    self
      .integer
      .bytes()
      .chain(self.fraction.bytes())
      .skip(self.lead)
      .take(self.significant)
  }

  const fn is_zero(&self) -> bool {
    self.significant == 0
  }

  /// Whether the two spell the same number: any two zeros, else the same
  /// sign, digits and scale.
  fn same_value(&self, other: &Decimal<'_>) -> bool {
    if self.is_zero() || other.is_zero() {
      return self.is_zero() && other.is_zero();
    }
    self.negative == other.negative
      && self.scale == other.scale
      && self.significant == other.significant
      && self.digits().eq(other.digits())
  }
}

/// `text` — a number as the JSON grammar spells it — read as the exact
/// decimal it spells, or `None` for an exponent that is not one.
///
/// A mantissa of zeros alone is zero at any exponent, however long, and its
/// exponent is not read at all; for any other number an exponent of any
/// length is read ([`exponent`]).
fn decimal(text: &str) -> Option<Decimal<'_>> {
  let (negative, unsigned) = match text.strip_prefix('-') {
    Some(unsigned) => (true, unsigned),
    None => (false, text),
  };
  let (mantissa, power) = unsigned.split_once(['e', 'E']).unwrap_or((unsigned, "0"));
  let (integer, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
  let digits = || integer.bytes().chain(fraction.bytes());
  let all = integer.len() + fraction.len();
  // The trailing zeros each scale the digits before them by ten.
  let zeros = digits().rev().take_while(|&digit| digit == b'0').count();
  if zeros == all {
    return Some(Decimal {
      negative,
      integer,
      fraction,
      lead: all,
      significant: 0,
      scale: 0,
    });
  }
  let lead = digits().take_while(|&digit| digit == b'0').count();
  // Within ±2^100 and the two lengths within ±2^64: no overflow.
  let scale =
    exponent(power)? - i128::try_from(fraction.len()).ok()? + i128::try_from(zeros).ok()?;
  Some(Decimal {
    negative,
    integer,
    fraction,
    lead,
    significant: all - zeros - lead,
    scale,
  })
}

/// The whole number a JSON number's text spells, exactly — `86400.0`,
/// `86400`, `8.64e4` — or `None` for one with a fraction, or past what an
/// `i128` holds.
///
/// Read from the digits, not through an `f64`: past 2^53 an `f64` holds
/// only some whole numbers, and one between two of them would read as its
/// nearest. A zero is zero at any exponent, however long; for any other
/// number an exponent of any length is read ([`exponent`]), so one past a
/// machine integer makes a fraction or a number past an `i128`, by its
/// sign.
pub(crate) fn whole(text: &str) -> Option<i128> {
  let decimal = decimal(text)?;
  if decimal.is_zero() {
    return Some(0);
  }
  // A significant digit below the units is a fraction.
  if decimal.scale < 0 {
    return None;
  }
  let mut value: i128 = 0;
  for digit in decimal.digits() {
    value = value
      .checked_mul(10)?
      .checked_add(i128::from(digit - b'0'))?;
  }
  // `value` is at least 1, so past 38 places the product leaves an `i128`
  // and the loop ends.
  for _ in 0..decimal.scale {
    value = value.checked_mul(10)?;
  }
  Some(if decimal.negative { -value } else { value })
}

/// The `f64` a JSON number's text spells **exactly**, or `None`.
///
/// Exactly: the text's own decimal value is that of the `f64`'s canonical
/// decimal — the shortest that reads back as it, the one `{:?}` writes and
/// the export writes every `f64` in. So any spelling of that value reads —
/// `24`, `24.0`, `2.4e1` — and nothing else: a number that reads as its
/// nearest `f64` only rounded (`24.0000000000000001`), one past the largest
/// (which reads as an infinity) and one between zero and the smallest
/// (which reads as zero) are each `None`.
pub(crate) fn exact_f64(text: &str) -> Option<f64> {
  let value = text.parse::<f64>().ok().filter(|value| value.is_finite())?;
  spells(text, format_args!("{value:?}")).then_some(value)
}

/// The `f32` a JSON number's text spells **exactly**, or `None`: as
/// [`exact_f64`], against the `f32`'s canonical decimal — so `16777217`,
/// which reads as the `f32` 16777216, is `None`.
pub(crate) fn exact_f32(text: &str) -> Option<f32> {
  let value = text.parse::<f32>().ok().filter(|value| value.is_finite())?;
  spells(text, format_args!("{value:?}")).then_some(value)
}

/// Whether `text` spells the same decimal as `canonical`, written out.
fn spells(text: &str, canonical: core::fmt::Arguments<'_>) -> bool {
  let mut written = Spelled::default();
  if core::fmt::write(&mut written, canonical).is_err() {
    return false;
  }
  match (decimal(text), decimal(written.as_str())) {
    (Some(text), Some(canonical)) => text.same_value(&canonical),
    _ => false,
  }
}

/// A float's canonical decimal, written on the stack: the longest an `f64`
/// writes is 24 bytes (`-2.2250738585072014e-308`). Longer text is refused
/// as an error, which no float writes.
#[derive(Default)]
struct Spelled {
  bytes: [u8; 32],
  len: usize,
}

impl Spelled {
  fn as_str(&self) -> &str {
    // Only whole `&str`s are copied in.
    core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
  }
}

impl core::fmt::Write for Spelled {
  fn write_str(&mut self, text: &str) -> core::fmt::Result {
    let end = self.len + text.len();
    let slot = self.bytes.get_mut(self.len..end).ok_or(core::fmt::Error)?;
    slot.copy_from_slice(text.as_bytes());
    self.len = end;
    Ok(())
  }
}

/// A JSON exponent's value — a sign, if any, and its digits — however many
/// digits it has, saturated at ±2^100; `None` for text that is no exponent.
///
/// Saturating loses nothing [`whole`] reads: a number's text is shorter
/// than 2^64 bytes, so the places its digits have below the units, or its
/// trailing zeros above them, are fewer — and an exponent past ±2^100
/// leaves any digit but zero below the units, a fraction, or more than 38
/// places above them, past an `i128`, whichever its sign. Nor anything
/// [`exact_f64`] and [`exact_f32`] read: such a number reads as an infinity
/// or as zero, and a finite float's canonical decimal has an exponent within
/// ±400.
fn exponent(text: &str) -> Option<i128> {
  const SATURATED: i128 = 1 << 100;
  let (negative, digits) = match text.as_bytes().first() {
    Some(b'-') => (true, &text[1..]),
    Some(b'+') => (false, &text[1..]),
    _ => (false, text),
  };
  if digits.is_empty() || !digits.bytes().all(|digit| digit.is_ascii_digit()) {
    return None;
  }
  // Below 2^100, ten times the value and a digit stay within an `i128`.
  let magnitude = digits.bytes().fold(0, |value: i128, digit| {
    (value * 10 + i128::from(digit - b'0')).min(SATURATED)
  });
  Some(if negative { -magnitude } else { magnitude })
}

/// How deep arrays and objects may nest before the reader refuses: deep
/// enough for any timeline this crate writes, shallow enough that a hostile
/// document cannot exhaust the stack.
const MAX_DEPTH: usize = 128;

/// An object's key as the duplicate check orders it: by its text — and,
/// in the laws with `std`, counted each time two are compared.
#[derive(PartialEq, Eq)]
struct Key(Utf8Bytes);

impl PartialOrd for Key {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for Key {
  fn cmp(&self, other: &Self) -> Ordering {
    #[cfg(all(test, feature = "std"))]
    tests::compared();
    self.0.as_str().cmp(other.0.as_str())
  }
}

/// Reads `text` as one JSON value (RFC 8259), refusing what the grammar
/// does not admit, an object naming a key twice, and nesting past
/// [`MAX_DEPTH`].
pub(crate) fn parse(text: &str) -> Result<Value, Syntax> {
  let mut reader = Reader {
    text,
    bytes: text.as_bytes(),
    at: 0,
    depth: 0,
  };
  reader.skip_space();
  let value = reader.value()?;
  reader.skip_space();
  if reader.at != reader.bytes.len() {
    return Err(reader.refuse("text after the document"));
  }
  Ok(value)
}

struct Reader<'a> {
  text: &'a str,
  bytes: &'a [u8],
  at: usize,
  depth: usize,
}

impl Reader<'_> {
  fn refuse(&self, reason: &'static str) -> Syntax {
    Syntax {
      offset: self.at,
      reason,
    }
  }

  fn peek(&self) -> Option<u8> {
    self.bytes.get(self.at).copied()
  }

  fn skip_space(&mut self) {
    while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
      self.at += 1;
    }
  }

  fn expect(&mut self, byte: u8, reason: &'static str) -> Result<(), Syntax> {
    if self.peek() == Some(byte) {
      self.at += 1;
      Ok(())
    } else {
      Err(self.refuse(reason))
    }
  }

  fn value(&mut self) -> Result<Value, Syntax> {
    match self.peek() {
      Some(b'{') => self.nested(Self::object),
      Some(b'[') => self.nested(Self::array),
      Some(b'"') => self.string().map(Value::String),
      Some(b't') => self.literal("true", Value::Bool(true)),
      Some(b'f') => self.literal("false", Value::Bool(false)),
      Some(b'n') => self.literal("null", Value::Null),
      Some(b'-' | b'0'..=b'9') => self.number(),
      _ => Err(self.refuse("expected a value")),
    }
  }

  fn nested(&mut self, read: fn(&mut Self) -> Result<Value, Syntax>) -> Result<Value, Syntax> {
    if self.depth == MAX_DEPTH {
      return Err(self.refuse("nested too deeply"));
    }
    self.depth += 1;
    let value = read(self);
    self.depth -= 1;
    value
  }

  fn object(&mut self) -> Result<Value, Syntax> {
    self.at += 1;
    let mut members: Vec<(Utf8Bytes, Value)> = Vec::new();
    // The keys read so far, for the check that none is named twice: in a
    // set, so an object of n keys costs O(n log n) comparisons — a scan of
    // the members before each key would cost n²/2.
    let mut keys: BTreeSet<Key> = BTreeSet::new();
    self.skip_space();
    if self.peek() == Some(b'}') {
      self.at += 1;
      return Ok(Value::Object(members));
    }
    loop {
      self.skip_space();
      if self.peek() != Some(b'"') {
        return Err(self.refuse("expected a key"));
      }
      let key_at = self.at;
      let key = self.string()?;
      if !keys.insert(Key(key.clone())) {
        return Err(Syntax {
          offset: key_at,
          reason: "a key named twice in one object",
        });
      }
      self.skip_space();
      self.expect(b':', "expected `:` after a key")?;
      self.skip_space();
      let value = self.value()?;
      members.push((key, value));
      self.skip_space();
      match self.peek() {
        Some(b',') => self.at += 1,
        Some(b'}') => {
          self.at += 1;
          return Ok(Value::Object(members));
        }
        _ => return Err(self.refuse("expected `,` or `}`")),
      }
    }
  }

  fn array(&mut self) -> Result<Value, Syntax> {
    self.at += 1;
    let mut items = Vec::new();
    self.skip_space();
    if self.peek() == Some(b']') {
      self.at += 1;
      return Ok(Value::Array(items));
    }
    loop {
      self.skip_space();
      items.push(self.value()?);
      self.skip_space();
      match self.peek() {
        Some(b',') => self.at += 1,
        Some(b']') => {
          self.at += 1;
          return Ok(Value::Array(items));
        }
        _ => return Err(self.refuse("expected `,` or `]`")),
      }
    }
  }

  fn literal(&mut self, word: &'static str, value: Value) -> Result<Value, Syntax> {
    if self.bytes[self.at..].starts_with(word.as_bytes()) {
      self.at += word.len();
      Ok(value)
    } else {
      Err(self.refuse("expected `true`, `false` or `null`"))
    }
  }

  fn digits(&mut self) -> usize {
    let start = self.at;
    while let Some(b'0'..=b'9') = self.peek() {
      self.at += 1;
    }
    self.at - start
  }

  fn number(&mut self) -> Result<Value, Syntax> {
    let start = self.at;
    if self.peek() == Some(b'-') {
      self.at += 1;
    }
    match self.peek() {
      Some(b'0') => self.at += 1,
      Some(b'1'..=b'9') => {
        self.digits();
      }
      _ => return Err(self.refuse("expected a digit")),
    }
    if self.peek() == Some(b'.') {
      self.at += 1;
      if self.digits() == 0 {
        return Err(self.refuse("expected a digit after `.`"));
      }
    }
    if let Some(b'e' | b'E') = self.peek() {
      self.at += 1;
      if let Some(b'+' | b'-') = self.peek() {
        self.at += 1;
      }
      if self.digits() == 0 {
        return Err(self.refuse("expected a digit in the exponent"));
      }
    }
    Ok(Value::Number(Utf8Bytes::from(&self.text[start..self.at])))
  }

  fn string(&mut self) -> Result<Utf8Bytes, Syntax> {
    self.at += 1;
    let mut out = Utf8BytesMut::new();
    loop {
      let run = self.at;
      while let Some(b) = self.peek()
        && b != b'"'
        && b != b'\\'
        && b >= 0x20
      {
        self.at += 1;
      }
      // The run ends on an ASCII byte or at the end of the text, so it is a
      // whole number of UTF-8 sequences.
      out.push_str(&self.text[run..self.at]);
      match self.peek() {
        Some(b'"') => {
          self.at += 1;
          return Ok(out.freeze_shared());
        }
        Some(b'\\') => {
          self.at += 1;
          self.escape(&mut out)?;
        }
        Some(_) => return Err(self.refuse("a control character inside a string")),
        None => return Err(self.refuse("a string left open")),
      }
    }
  }

  fn escape(&mut self, out: &mut Utf8BytesMut) -> Result<(), Syntax> {
    let c = match self.peek() {
      Some(b'"') => '"',
      Some(b'\\') => '\\',
      Some(b'/') => '/',
      Some(b'b') => '\u{8}',
      Some(b'f') => '\u{c}',
      Some(b'n') => '\n',
      Some(b'r') => '\r',
      Some(b't') => '\t',
      Some(b'u') => {
        self.at += 1;
        let unit = self.hex4()?;
        let code = match unit {
          0xD800..=0xDBFF => {
            if !self.bytes[self.at..].starts_with(b"\\u") {
              return Err(self.refuse("a lone surrogate"));
            }
            self.at += 2;
            let low = self.hex4()?;
            if !(0xDC00..=0xDFFF).contains(&low) {
              return Err(self.refuse("a lone surrogate"));
            }
            0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00)
          }
          0xDC00..=0xDFFF => return Err(self.refuse("a lone surrogate")),
          unit => unit,
        };
        let c = char::from_u32(code).ok_or_else(|| self.refuse("not a character"))?;
        out.push(c);
        return Ok(());
      }
      _ => return Err(self.refuse("an unknown escape")),
    };
    self.at += 1;
    out.push(c);
    Ok(())
  }

  fn hex4(&mut self) -> Result<u32, Syntax> {
    let mut unit = 0;
    for _ in 0..4 {
      let digit = match self.peek() {
        Some(b @ b'0'..=b'9') => b - b'0',
        Some(b @ b'a'..=b'f') => b - b'a' + 10,
        Some(b @ b'A'..=b'F') => b - b'A' + 10,
        _ => return Err(self.refuse("expected four hex digits")),
      };
      unit = unit * 16 + u32::from(digit);
      self.at += 1;
    }
    Ok(unit)
  }
}

#[cfg(test)]
mod tests;

//! A small JSON value: written pretty for the export, read strictly for the
//! self-check.

use alloc::{string::String, vec::Vec};
use core::fmt::Write as _;

use super::Syntax;

/// A JSON value. A number is kept as the text that spells it, so the writer
/// puts down exactly the digits it was handed and the reader keeps exactly
/// the digits it read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
  Null,
  Bool(bool),
  Number(String),
  String(String),
  Array(Vec<Value>),
  Object(Vec<(String, Value)>),
}

impl Value {
  pub(crate) fn as_object(&self) -> Option<&[(String, Value)]> {
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
      Self::String(text) => Some(text),
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
pub(crate) fn write_pretty(value: &Value, out: &mut String) {
  write_value(value, 0, out);
}

fn write_value(value: &Value, depth: usize, out: &mut String) {
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

fn newline(depth: usize, out: &mut String) {
  out.push('\n');
  for _ in 0..depth {
    out.push_str("    ");
  }
}

/// Writes `text` as a JSON string: the quote, the backslash and the control
/// characters escaped, everything else as it is.
fn write_string(text: &str, out: &mut String) {
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

/// How deep arrays and objects may nest before the reader refuses: deep
/// enough for any timeline this crate writes, shallow enough that a hostile
/// document cannot exhaust the stack.
const MAX_DEPTH: usize = 128;

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
    let mut members: Vec<(String, Value)> = Vec::new();
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
      if members.iter().any(|(seen, _)| *seen == key) {
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
    Ok(Value::Number(String::from(&self.text[start..self.at])))
  }

  fn string(&mut self) -> Result<String, Syntax> {
    self.at += 1;
    let mut out = String::new();
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
          return Ok(out);
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

  fn escape(&mut self, out: &mut String) -> Result<(), Syntax> {
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

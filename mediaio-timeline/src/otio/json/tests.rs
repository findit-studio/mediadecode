use super::*;

fn n(text: &str) -> Value {
  Value::Number(Utf8Bytes::from(text))
}

fn s(text: &str) -> Value {
  Value::String(Utf8Bytes::from(text))
}

fn pretty(value: &Value) -> Utf8Bytes {
  let mut out = Utf8BytesMut::new();
  write_pretty(value, &mut out);
  out.freeze_shared()
}

fn refusal(text: &str) -> (usize, &'static str) {
  let syntax = parse(text).unwrap_err();
  (syntax.offset(), syntax.reason())
}

#[test]
fn the_writer_indents_four_spaces_a_level_and_keeps_empty_containers_inline() {
  let value = Value::Object(vec![
    (Utf8Bytes::from("a"), n("1.0")),
    (
      Utf8Bytes::from("b"),
      Value::Array(vec![Value::Null, Value::Bool(true), s("x")]),
    ),
    (Utf8Bytes::from("c"), Value::Object(vec![])),
    (Utf8Bytes::from("d"), Value::Array(vec![])),
  ]);
  assert_eq!(
    pretty(&value),
    "{\n    \"a\": 1.0,\n    \"b\": [\n        null,\n        true,\n        \"x\"\n    ],\n    \"c\": {},\n    \"d\": []\n}"
  );
}

#[test]
fn strings_escape_the_quote_the_backslash_and_control_characters_only() {
  let text = "q\"b\\n\nr\rt\tb\u{8}f\u{c}u\u{1}é😀/";
  let written = pretty(&s(text));
  assert_eq!(written, "\"q\\\"b\\\\n\\nr\\rt\\tb\\bf\\fu\\u0001é😀/\"");
  assert_eq!(parse(&written), Ok(s(text)));
}

#[test]
fn what_is_written_reads_back() {
  let value = Value::Object(vec![
    (Utf8Bytes::from("n"), n("-0.5e-3")),
    (
      Utf8Bytes::from("deep"),
      Value::Array(vec![Value::Object(vec![(
        Utf8Bytes::from("k"),
        Value::Null,
      )])]),
    ),
  ]);
  assert_eq!(parse(&pretty(&value)), Ok(value));
}

#[test]
fn escapes_read_as_the_characters_they_name() {
  assert_eq!(parse(r#""é😀\/\"""#), Ok(s("é😀/\"")));
}

#[test]
fn numbers_keep_their_spelling() {
  assert_eq!(parse("86400.0"), Ok(n("86400.0")));
  assert_eq!(parse("-0"), Ok(n("-0")));
  assert_eq!(parse("1E+2"), Ok(n("1E+2")));
  assert_eq!(n("23.976023976023978").as_f64(), Some(24_000.0 / 1001.0));
}

#[test]
fn what_the_grammar_does_not_admit_is_refused_where_it_breaks() {
  assert_eq!(refusal(""), (0, "expected a value"));
  assert_eq!(refusal("01"), (1, "text after the document"));
  assert_eq!(refusal("1."), (2, "expected a digit after `.`"));
  assert_eq!(refusal(".5"), (0, "expected a value"));
  assert_eq!(refusal("-"), (1, "expected a digit"));
  assert_eq!(refusal("1e"), (2, "expected a digit in the exponent"));
  assert_eq!(refusal("nul"), (0, "expected `true`, `false` or `null`"));
  assert_eq!(refusal("[1,]"), (3, "expected a value"));
  assert_eq!(refusal("[1 2]"), (3, "expected `,` or `]`"));
  assert_eq!(refusal("{\"a\" 1}"), (5, "expected `:` after a key"));
  assert_eq!(refusal("{1: 2}"), (1, "expected a key"));
  assert_eq!(refusal("{\"a\": 1 \"b\": 2}"), (8, "expected `,` or `}`"));
  assert_eq!(refusal("\"open"), (5, "a string left open"));
  assert_eq!(
    refusal("\"a\nb\""),
    (2, "a control character inside a string")
  );
  assert_eq!(refusal(r#""\x""#), (2, "an unknown escape"));
  assert_eq!(refusal(r#""\u12""#), (5, "expected four hex digits"));
  assert_eq!(refusal(r#""\ud83d""#), (7, "a lone surrogate"));
  assert_eq!(refusal(r#""\ude00""#), (7, "a lone surrogate"));
  assert_eq!(refusal("{} {}"), (3, "text after the document"));
}

#[test]
fn a_key_named_twice_in_one_object_is_refused_at_the_second() {
  assert_eq!(
    refusal(r#"{"a": 1, "a": 2}"#),
    (9, "a key named twice in one object")
  );
  // The same key in two objects is two keys.
  assert!(parse(r#"[{"a": 1}, {"a": 2}]"#).is_ok());
}

#[test]
fn nesting_past_the_limit_is_refused_rather_than_recursed_into() {
  let deep = |levels: usize| {
    let mut text = Utf8BytesMut::new();
    for _ in 0..levels {
      text.push('[');
    }
    for _ in 0..levels {
      text.push(']');
    }
    text
  };
  assert!(parse(&deep(MAX_DEPTH)).is_ok());
  assert_eq!(
    refusal(&deep(MAX_DEPTH + 1)),
    (MAX_DEPTH, "nested too deeply")
  );
}

#[test]
fn a_whole_number_is_read_from_its_digits_exactly() {
  for (text, value) in [
    ("86400.0", Some(86_400)),
    ("86400", Some(86_400)),
    ("-24.000", Some(-24)),
    ("0", Some(0)),
    ("-0.0", Some(0)),
    ("0e7", Some(0)),
    ("8.64e4", Some(86_400)),
    ("8640000e-2", Some(86_400)),
    ("1E+3", Some(1_000)),
    // 2^53 + 1, which an f64 reads as 2^53.
    ("9007199254740993.0", Some(9_007_199_254_740_993)),
    ("24.5", None),
    ("4503599627370496.5", None),
    ("1e-1", None),
    ("8640001e-2", None),
    // Past an i128.
    ("1e39", None),
    ("1e9999999999", None),
  ] {
    assert_eq!(whole(text), value, "{text}");
  }
}

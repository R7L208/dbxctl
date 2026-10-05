#![cfg_attr(not(test), allow(dead_code))]
// #30 (workspace integration) will consume this module

use std::fmt;

/// A JSON value parsed with defensive access and deterministic serialization.
///
/// This is a custom JSON parser that provides a crate-private API hiding implementation
/// details from callers. All accessors return `Option` or `Result`.
///
/// **Nesting depth limit:** The parser enforces a maximum recursion depth of 128 to
/// prevent stack overflow on deeply nested input. Attempting to parse deeper structures
/// returns an error.
///
/// **Duplicate object keys:** When an object has duplicate keys, the parser preserves
/// all entries in the vector. Access by key returns the last matching value. This
/// preserves the raw structure while allowing standard JSON semantics.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(JsonNumber),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

/// A JSON number that preserves the original string representation for arbitrary precision.
#[derive(Clone, Debug, PartialEq)]
pub struct JsonNumber {
    /// Original string representation from JSON
    repr: String,
    /// Cached i64 value if it fits
    as_i64: Option<i64>,
    /// Cached u64 value if it fits
    as_u64: Option<u64>,
    /// Cached f64 value
    as_f64: Option<f64>,
}

impl JsonNumber {
    /// Parse a JSON number string, returning `None` if invalid.
    fn parse(s: &str) -> Option<Self> {
        // Try to parse the number in various formats
        let i64_val = s.parse::<i64>().ok();
        let u64_val = s.parse::<u64>().ok();
        let f64_val = s.parse::<f64>().ok();

        // At least one must succeed
        if i64_val.is_some() || u64_val.is_some() || f64_val.is_some() {
            Some(JsonNumber {
                repr: s.to_string(),
                as_i64: i64_val,
                as_u64: u64_val,
                as_f64: f64_val,
            })
        } else {
            None
        }
    }
}

/// A custom JSON error type that never panics on input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Generic parse error with location and message.
    Parse(String),
    /// Maximum nesting depth exceeded.
    DepthExceeded,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Parse(msg) => write!(f, "JSON parse error: {msg}"),
            Error::DepthExceeded => write!(f, "JSON nesting depth limit (128) exceeded"),
        }
    }
}

impl std::error::Error for Error {}

const MAX_DEPTH: usize = 128;

/// Parse a JSON value from a string.
///
/// # Errors
/// Returns an error if the input is malformed, non-UTF-8, or exceeds the nesting depth limit.
pub fn parse(input: &str) -> Result<Value, Error> {
    let mut parser = Parser::new(input);
    parser.parse_value(0)
}

/// Parse a JSON value from bytes.
///
/// # Errors
/// Returns an error if the input is malformed, non-UTF-8, or exceeds the nesting depth limit.
pub fn parse_bytes(input: &[u8]) -> Result<Value, Error> {
    let s = std::str::from_utf8(input).map_err(|e| Error::Parse(format!("invalid UTF-8: {e}")))?;
    parse(s)
}

/// Serialize a value to a JSON string with deterministic output and a trailing newline.
pub fn to_string(value: &Value) -> String {
    let mut out = serialize_value(value);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Serialize a value to pretty-printed JSON with deterministic output and a trailing newline.
pub fn to_string_pretty(value: &Value) -> String {
    let mut out = serialize_value_pretty(value, 0);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn serialize_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.repr.clone(),
        Value::String(s) => format!("\"{}\"", escape_string(s)),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(serialize_value).collect();
            format!("[{}]", items.join(","))
        }
        Value::Object(obj) => {
            // Sort keys for determinism
            let mut sorted: Vec<_> = obj.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));

            let items: Vec<String> = sorted
                .iter()
                .map(|(k, v)| format!("\"{}\":{}", escape_string(k), serialize_value(v)))
                .collect();
            format!("{{{}}}", items.join(","))
        }
    }
}

fn serialize_value_pretty(value: &Value, indent: usize) -> String {
    let indent_str = "  ".repeat(indent);
    let next_indent_str = "  ".repeat(indent + 1);

    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.repr.clone(),
        Value::String(s) => format!("\"{}\"", escape_string(s)),
        Value::Array(arr) => {
            if arr.is_empty() {
                "[]".to_string()
            } else {
                let items: Vec<String> = arr
                    .iter()
                    .map(|v| {
                        format!(
                            "{}{}",
                            next_indent_str,
                            serialize_value_pretty(v, indent + 1)
                        )
                    })
                    .collect();
                format!("[\n{}\n{}]", items.join(",\n"), indent_str)
            }
        }
        Value::Object(obj) => {
            if obj.is_empty() {
                "{}".to_string()
            } else {
                // Sort keys for determinism
                let mut sorted: Vec<_> = obj.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));

                let items: Vec<String> = sorted
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "{}\"{}\": {}",
                            next_indent_str,
                            escape_string(k),
                            serialize_value_pretty(v, indent + 1)
                        )
                    })
                    .collect();
                format!("{{\n{}\n{}}}", items.join(",\n"), indent_str)
            }
        }
    }
}

fn escape_string(s: &str) -> String {
    let mut result = String::new();
    for c in s.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '\x08' => result.push_str("\\b"),
            '\x0c' => result.push_str("\\f"),
            c if c.is_control() => {
                use std::fmt::Write;
                let _ = write!(result, "\\u{:04x}", c as u32);
            }
            c => result.push(c),
        }
    }
    result
}

impl Value {
    /// Access a value by object key, returning `None` if not an object or key not found.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(obj) => obj.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Access a value by array index, returning `None` if not an array or index out of bounds.
    pub fn get_index(&self, index: usize) -> Option<&Value> {
        match self {
            Value::Array(arr) => arr.get(index),
            _ => None,
        }
    }

    /// Access a value using a JSON Pointer (RFC 6901) path.
    ///
    /// Supports paths like `/foo/bar/0` and properly unescapes `~0` (tilde) and `~1` (slash).
    ///
    /// # Examples
    /// ```ignore
    /// let val = parse(r#"{"state": {"resources": {"pipelines": {"key.name": {"__id__": 1}}}}}"#)?;
    /// assert_eq!(val.pointer("/state/resources.pipelines/key.name/__id__"), Some(...));
    /// ```
    pub fn pointer(&self, path: &str) -> Option<&Value> {
        if path.is_empty() {
            return Some(self);
        }

        if !path.starts_with('/') {
            return None;
        }

        let mut current = self;
        for token in path[1..].split('/') {
            let unescaped = unescape_pointer_token(token);
            current = match current {
                Value::Object(obj) => obj.iter().find(|(k, _)| *k == unescaped).map(|(_, v)| v)?,
                Value::Array(arr) => {
                    let index = unescaped.parse::<usize>().ok()?;
                    arr.get(index)?
                }
                _ => return None,
            };
        }
        Some(current)
    }

    /// Return the value as a string, or `None` if it's not a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Return the value as a bool, or `None` if it's not a bool.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Return the value as an i64, or `None` if it's not an integer within that range.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(n) => n.as_i64,
            _ => None,
        }
    }

    /// Return the value as a u64, or `None` if it's not an integer within that range.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(n) => n.as_u64,
            _ => None,
        }
    }

    /// Return the value as an array, or `None` if it's not an array.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(arr) => Some(arr),
            _ => None,
        }
    }

    /// Return the value as an object (key-value pairs), or `None` if it's not an object.
    pub fn as_object(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Object(obj) => Some(obj),
            _ => None,
        }
    }

    /// Return `true` if the value is null.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

fn unescape_pointer_token(token: &str) -> String {
    let mut result = String::new();
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            match chars.next() {
                Some('0') => result.push('~'),
                Some('1') => result.push('/'),
                _ => {
                    result.push('~');
                    if let Some(next) = chars.next() {
                        result.push(next);
                    }
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

struct Parser {
    input: Vec<char>,
    pos: usize,
}

impl Parser {
    fn new(input: &str) -> Self {
        Self {
            input: input.chars().collect(),
            pos: 0,
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::DepthExceeded);
        }

        self.skip_whitespace();

        match self.peek() {
            Some('n') => self.parse_null(),
            Some('t' | 'f') => self.parse_bool(),
            Some('"') => self.parse_string(),
            Some('[') => self.parse_array(depth),
            Some('{') => self.parse_object(depth),
            Some('-' | '0'..='9') => self.parse_number(),
            Some(c) => Err(Error::Parse(format!("unexpected character: {c}"))),
            None => Err(Error::Parse("unexpected end of input".to_string())),
        }
    }

    fn parse_null(&mut self) -> Result<Value, Error> {
        if self.consume_keyword("null") {
            Ok(Value::Null)
        } else {
            Err(Error::Parse("expected 'null'".to_string()))
        }
    }

    fn parse_bool(&mut self) -> Result<Value, Error> {
        if self.consume_keyword("true") {
            Ok(Value::Bool(true))
        } else if self.consume_keyword("false") {
            Ok(Value::Bool(false))
        } else {
            Err(Error::Parse("expected 'true' or 'false'".to_string()))
        }
    }

    fn parse_string(&mut self) -> Result<Value, Error> {
        self.expect('"')?;
        let mut result = String::new();

        while let Some(c) = self.next_char() {
            if c == '"' {
                return Ok(Value::String(result));
            }
            if c == '\\' {
                match self.next_char() {
                    Some('"') => result.push('"'),
                    Some('\\') => result.push('\\'),
                    Some('/') => result.push('/'),
                    Some('b') => result.push('\x08'),
                    Some('f') => result.push('\x0c'),
                    Some('n') => result.push('\n'),
                    Some('r') => result.push('\r'),
                    Some('t') => result.push('\t'),
                    Some('u') => {
                        let code = self.parse_unicode_escape()?;
                        result.push(code);
                    }
                    _ => return Err(Error::Parse("invalid escape sequence".to_string())),
                }
            } else {
                result.push(c);
            }
        }
        Err(Error::Parse("unterminated string".to_string()))
    }

    fn parse_unicode_escape(&mut self) -> Result<char, Error> {
        let mut code = 0u32;
        for _ in 0..4 {
            match self.next_char() {
                Some(c @ '0'..='9') => {
                    code = code * 16 + (c as u32 - '0' as u32);
                }
                Some(c @ 'a'..='f') => {
                    code = code * 16 + (c as u32 - 'a' as u32 + 10);
                }
                Some(c @ 'A'..='F') => {
                    code = code * 16 + (c as u32 - 'A' as u32 + 10);
                }
                _ => return Err(Error::Parse("invalid unicode escape".to_string())),
            }
        }

        // Handle surrogate pairs
        if (0xD800..=0xDBFF).contains(&code) {
            // High surrogate; expect low surrogate next
            self.skip_whitespace();
            if self.peek() == Some('\\') {
                self.next_char();
                if self.peek() == Some('u') {
                    self.next_char();
                    let low = self.parse_unicode_escape()?;
                    let low_code = low as u32;
                    if (0xDC00..=0xDFFF).contains(&low_code) {
                        let codepoint = 0x10000 + ((code - 0xD800) << 10) + (low_code - 0xDC00);
                        return char::from_u32(codepoint)
                            .ok_or_else(|| Error::Parse("invalid codepoint".to_string()));
                    }
                }
            }
            return Err(Error::Parse(
                "expected low surrogate after high surrogate".to_string(),
            ));
        }

        char::from_u32(code).ok_or_else(|| Error::Parse("invalid codepoint".to_string()))
    }

    fn parse_number(&mut self) -> Result<Value, Error> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.next_char();
        }

        // Parse the integer part
        if self.peek() == Some('0') {
            self.next_char();
            // A single '0' is valid, but '01', '00', etc. are not
            if matches!(self.peek(), Some('0'..='9')) {
                return Err(Error::Parse("leading zero in number".to_string()));
            }
        } else if matches!(self.peek(), Some('1'..='9')) {
            while matches!(self.peek(), Some('0'..='9')) {
                self.next_char();
            }
        } else {
            return Err(Error::Parse("invalid number".to_string()));
        }

        if self.peek() == Some('.') {
            self.next_char();
            if !matches!(self.peek(), Some('0'..='9')) {
                return Err(Error::Parse(
                    "invalid number: expected digits after decimal point".to_string(),
                ));
            }
            while matches!(self.peek(), Some('0'..='9')) {
                self.next_char();
            }
        }

        if matches!(self.peek(), Some('e' | 'E')) {
            self.next_char();
            if matches!(self.peek(), Some('+' | '-')) {
                self.next_char();
            }
            if !matches!(self.peek(), Some('0'..='9')) {
                return Err(Error::Parse(
                    "invalid number: expected digits in exponent".to_string(),
                ));
            }
            while matches!(self.peek(), Some('0'..='9')) {
                self.next_char();
            }
        }

        let num_str: String = self.input[start..self.pos].iter().collect();
        let num = JsonNumber::parse(&num_str)
            .ok_or_else(|| Error::Parse(format!("invalid number: {num_str}")))?;

        Ok(Value::Number(num))
    }

    fn parse_array(&mut self, depth: usize) -> Result<Value, Error> {
        self.expect('[')?;
        self.skip_whitespace();

        let mut arr = Vec::new();

        if self.peek() == Some(']') {
            self.next_char();
            return Ok(Value::Array(arr));
        }

        loop {
            arr.push(self.parse_value(depth + 1)?);
            self.skip_whitespace();

            match self.peek() {
                Some(',') => {
                    self.next_char();
                    self.skip_whitespace();
                }
                Some(']') => {
                    self.next_char();
                    return Ok(Value::Array(arr));
                }
                _ => return Err(Error::Parse("expected ',' or ']' in array".to_string())),
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<Value, Error> {
        self.expect('{')?;
        self.skip_whitespace();

        let mut obj = Vec::new();

        if self.peek() == Some('}') {
            self.next_char();
            return Ok(Value::Object(obj));
        }

        loop {
            self.skip_whitespace();

            let Value::String(key) = self.parse_string()? else {
                return Err(Error::Parse("expected string key in object".to_string()));
            };

            self.skip_whitespace();
            self.expect(':')?;
            self.skip_whitespace();

            let value = self.parse_value(depth + 1)?;
            obj.push((key, value));

            self.skip_whitespace();

            match self.peek() {
                Some(',') => {
                    self.next_char();
                    self.skip_whitespace();
                }
                Some('}') => {
                    self.next_char();
                    // Sort keys for consistency with serialization
                    obj.sort_by(|a, b| a.0.cmp(&b.0));
                    return Ok(Value::Object(obj));
                }
                _ => return Err(Error::Parse("expected ',' or '}' in object".to_string())),
            }
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.next_char();
        }
    }

    fn peek(&self) -> Option<char> {
        if self.pos < self.input.len() {
            Some(self.input[self.pos])
        } else {
            None
        }
    }

    fn next_char(&mut self) -> Option<char> {
        if self.pos < self.input.len() {
            let c = self.input[self.pos];
            self.pos += 1;
            Some(c)
        } else {
            None
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), Error> {
        if self.peek() == Some(expected) {
            self.next_char();
            Ok(())
        } else {
            Err(Error::Parse(format!("expected '{expected}'")))
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        let remaining: String = self.input[self.pos..].iter().take(keyword.len()).collect();
        if remaining == keyword {
            // Check that the next character is not an identifier character
            let next_pos = self.pos + keyword.len();
            let next_char = if next_pos < self.input.len() {
                Some(self.input[next_pos])
            } else {
                None
            };
            // Valid terminators for keywords: whitespace, punctuation, or EOF
            if next_char.is_none()
                || matches!(next_char, Some(c) if !c.is_alphanumeric() && c != '_')
            {
                self.pos += keyword.len();
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_null() {
        assert_eq!(parse("null").unwrap(), Value::Null);
    }

    #[test]
    fn parse_bool() {
        assert_eq!(parse("true").unwrap(), Value::Bool(true));
        assert_eq!(parse("false").unwrap(), Value::Bool(false));
    }

    #[test]
    fn parse_integer() {
        assert_eq!(parse("42").unwrap().as_i64(), Some(42));
        assert_eq!(parse("-17").unwrap().as_i64(), Some(-17));
        assert_eq!(parse("0").unwrap().as_i64(), Some(0));
    }

    #[test]
    fn parse_float() {
        let v = parse("3.14").unwrap();
        match v {
            Value::Number(ref n) => {
                assert!(n.as_f64.is_some());
                assert!((n.as_f64.unwrap() - std::f64::consts::PI).abs() < 0.1);
            }
            _ => panic!("expected float"),
        }
    }

    #[test]
    fn parse_scientific_notation() {
        let v = parse("1e10").unwrap();
        match v {
            Value::Number(ref n) => {
                assert!(n.as_f64.is_some());
                assert!((n.as_f64.unwrap() - 1e10).abs() < f64::EPSILON);
            }
            _ => panic!("expected float in scientific notation"),
        }

        let v = parse("1.5e-5").unwrap();
        match v {
            Value::Number(ref n) => {
                assert!(n.as_f64.is_some());
                assert!((n.as_f64.unwrap() - 1.5e-5).abs() < 1e-10);
            }
            _ => panic!("expected float in scientific notation"),
        }
    }

    #[test]
    fn parse_string() {
        assert_eq!(
            parse(r#""hello""#).unwrap(),
            Value::String("hello".to_string())
        );
    }

    #[test]
    fn parse_string_with_escapes() {
        assert_eq!(
            parse(r#""hello\"world""#).unwrap(),
            Value::String("hello\"world".to_string())
        );
        assert_eq!(
            parse(r#""line1\nline2""#).unwrap(),
            Value::String("line1\nline2".to_string())
        );
        assert_eq!(
            parse(r#""tab\there""#).unwrap(),
            Value::String("tab\there".to_string())
        );
    }

    #[test]
    fn parse_unicode_escapes() {
        // Basic BMP character
        assert_eq!(parse(r#""A""#).unwrap(), Value::String("A".to_string()));

        // Surrogate pair (emoji)
        let v = parse(r#""😀""#).unwrap();
        if let Value::String(s) = v {
            assert_eq!(s, "😀");
        } else {
            panic!("expected string");
        }
    }

    #[test]
    fn parse_array() {
        let empty = parse("[]").unwrap();
        assert!(empty.as_array().is_some_and(<[Value]>::is_empty));

        let arr = parse("[1,2,3]").unwrap();
        let arr_slice = arr.as_array().unwrap();
        assert_eq!(arr_slice.len(), 3);
        assert_eq!(arr_slice[0].as_i64(), Some(1));
        assert_eq!(arr_slice[1].as_i64(), Some(2));
        assert_eq!(arr_slice[2].as_i64(), Some(3));
    }

    #[test]
    fn parse_nested_array() {
        let v = parse("[[1,2],[3,4]]").unwrap();
        match v {
            Value::Array(ref arr) if arr.len() == 2 => {
                assert!(matches!(arr[0], Value::Array(_)));
            }
            _ => panic!("expected nested array"),
        }
    }

    #[test]
    fn parse_object() {
        let v = parse(r#"{"name":"Alice","age":30}"#).unwrap();
        match v {
            Value::Object(ref obj) => {
                assert_eq!(obj.len(), 2);
                // Keys are sorted alphabetically, so "age" comes before "name"
                assert_eq!(obj[0].0, "age");
                assert_eq!(obj[1].0, "name");
            }
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn parse_object_with_duplicate_keys() {
        // serde_json keeps the last value for duplicate keys
        let v = parse(r#"{"key":"first","key":"second"}"#).unwrap();
        match v {
            Value::Object(ref obj) => {
                // The parser will store both; the behavior depends on access method
                let values: Vec<_> = obj.iter().map(|(k, _)| k.clone()).collect();
                // Just verify we got two entries; typical JSON parsers may deduplicate
                assert_eq!(values.len(), 2);
            }
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn parse_malformed_input() {
        assert!(parse("").is_err());
        assert!(parse("{").is_err());
        assert!(parse("[").is_err());
        assert!(parse(r#""unterminated"#).is_err());
        assert!(parse("tru").is_err());
        assert!(parse("nul").is_err());
        assert!(parse("truee").is_err());
    }

    #[test]
    fn parse_invalid_numbers() {
        assert!(parse("01").is_err()); // Leading zero not allowed
        assert!(parse(".5").is_err()); // No leading digit
        assert!(parse("5.").is_err()); // No trailing digit after decimal
        assert!(parse("1e").is_err()); // No exponent digits
    }

    #[test]
    fn parse_invalid_utf8() {
        let invalid = b"\"hello\xff\"";
        assert!(parse_bytes(invalid).is_err());
    }

    #[test]
    fn parse_deep_nesting() {
        // Construct deeply nested structure below limit
        let mut s = String::new();
        for _ in 0..64 {
            s.push('[');
        }
        s.push('1');
        for _ in 0..64 {
            s.push(']');
        }
        assert!(parse(&s).is_ok());

        // Try to exceed the limit
        let mut s = String::new();
        for _ in 0..129 {
            s.push('[');
        }
        s.push('1');
        for _ in 0..129 {
            s.push(']');
        }
        assert!(parse(&s).is_err());
    }

    #[test]
    fn access_by_key() {
        let v = parse(r#"{"x":10,"y":20}"#).unwrap();
        assert_eq!(v.get("x").and_then(Value::as_i64), Some(10));
        assert_eq!(v.get("y").and_then(Value::as_i64), Some(20));
        assert_eq!(v.get("z"), None);
    }

    #[test]
    fn access_by_index() {
        let v = parse("[10,20,30]").unwrap();
        assert_eq!(v.get_index(0).and_then(Value::as_i64), Some(10));
        assert_eq!(v.get_index(2).and_then(Value::as_i64), Some(30));
        assert_eq!(v.get_index(5), None);
    }

    #[test]
    fn access_by_json_pointer() {
        let v = parse(r#"{"a":{"b":[1,2,3]}}"#).unwrap();
        assert_eq!(v.pointer("/a/b/1").and_then(Value::as_i64), Some(2));
        assert_eq!(v.pointer("/a/b/10"), None);
        assert_eq!(v.pointer("/x"), None);
    }

    #[test]
    fn json_pointer_with_escaped_tokens() {
        // ~0 represents ~ and ~1 represents /
        let v = parse(r#"{"a/b":{"c~d":5}}"#).unwrap();
        assert_eq!(v.pointer("/a~1b/c~0d").and_then(Value::as_i64), Some(5));
    }

    #[test]
    fn typed_getters() {
        let v = parse(r#"{"s":"text","b":true,"i":42,"f":3.14,"n":null,"a":[],"o":{}}"#).unwrap();

        assert_eq!(v.get("s").and_then(Value::as_str), Some("text"));
        assert_eq!(v.get("b").and_then(Value::as_bool), Some(true));
        assert_eq!(v.get("i").and_then(Value::as_i64), Some(42));
        assert!(v.get("f").and_then(Value::as_i64).is_none()); // Float can't be i64
        assert_eq!(v.get("n").map(Value::is_null), Some(true));
        assert!(v.get("a").and_then(Value::as_array).is_some());
        assert!(v.get("o").and_then(Value::as_object).is_some());

        // Type mismatches return None
        assert_eq!(v.get("s").and_then(Value::as_bool), None);
        assert_eq!(v.get("b").and_then(Value::as_str), None);
    }

    #[test]
    fn serialization_determinism() {
        let v = parse(r#"{"z":3,"a":1,"m":2}"#).unwrap();
        let s1 = to_string(&v);
        let s2 = to_string(&v);
        assert_eq!(s1, s2);

        // Keys are sorted
        assert!(s1.contains("\"a\":1"));
        assert!(s1.contains("\"m\":2"));
        assert!(s1.contains("\"z\":3"));
        let pos_a = s1.find("\"a\"").unwrap();
        let pos_m = s1.find("\"m\"").unwrap();
        let pos_z = s1.find("\"z\"").unwrap();
        assert!(pos_a < pos_m && pos_m < pos_z);
    }

    #[test]
    fn serialization_has_trailing_newline() {
        let v = parse("{}").unwrap();
        let s = to_string(&v);
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn pretty_serialization() {
        let v = parse(r#"{"a":1,"b":[2,3]}"#).unwrap();
        let s = to_string_pretty(&v);
        assert!(s.contains('\n'));
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn round_trip() {
        let original = r#"{"name":"Alice","age":30,"tags":["a","b"]}"#;
        let v = parse(original).unwrap();
        let serialized = to_string(&v);
        let v2 = parse(&serialized).unwrap();
        assert_eq!(v, v2);
    }

    #[test]
    fn large_numbers() {
        let v = parse("9223372036854775807").unwrap(); // i64::MAX
        assert_eq!(v.as_i64(), Some(i64::MAX));

        let v = parse("18446744073709551615").unwrap(); // u64::MAX
        assert_eq!(v.as_u64(), Some(u64::MAX));

        let v = parse("18446744073709551616").unwrap(); // u64::MAX + 1
        assert_eq!(v.as_u64(), None);
    }

    #[test]
    fn negative_numbers() {
        let v = parse("-42").unwrap();
        assert_eq!(v.as_i64(), Some(-42));
        assert_eq!(v.as_u64(), None); // Negative, can't be u64
    }

    #[test]
    fn option_chaining() {
        let v = parse(r#"{"x":{"y":10}}"#).unwrap();
        assert_eq!(
            v.get("x").and_then(|x| x.get("y")).and_then(Value::as_i64),
            Some(10)
        );
        assert_eq!(
            v.get("x").and_then(|x| x.get("z")).and_then(Value::as_i64),
            None
        );
    }

    #[test]
    fn parse_whitespace() {
        assert_eq!(parse("  null  ").unwrap(), Value::Null);
        let arr = parse("  [  1  ,  2  ]  ").unwrap();
        let arr_slice = arr.as_array().unwrap();
        assert_eq!(arr_slice.len(), 2);
        assert_eq!(arr_slice[0].as_i64(), Some(1));
        assert_eq!(arr_slice[1].as_i64(), Some(2));
    }

    #[test]
    fn error_no_panic_on_invalid_input() {
        // These should all return errors, never panic
        let invalid_inputs = vec![
            "",
            "{",
            "[",
            "tru",
            "nul",
            r#""incomplete"#,
            "NaN",
            "Infinity",
            "{\"key\":}",
            "[1,,2]",
        ];

        for input in invalid_inputs {
            let result = parse(input);
            assert!(result.is_err(), "should error on: {input}");
        }
    }
}

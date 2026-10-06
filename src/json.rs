#![cfg_attr(not(test), allow(dead_code))]
// #30 (workspace integration) will consume this module

use std::collections::BTreeMap;
use std::sync::Arc;

/// A JSON value with defensive access and deterministic serialization.
///
/// This module wraps `serde_json` to hide its types from callers and enforce
/// defensive access patterns. Accessors return borrowed references without cloning.
///
/// **Nesting depth limit:** `serde_json` accepts at most 127 nested arrays or
/// objects. Deeper input returns an error rather than overflowing the stack.
///
/// **Duplicate object keys:** When an object has duplicate keys, `serde_json` keeps
/// the last value (standard JSON behavior). Access methods return the value associated
/// with the final occurrence.
///
/// **Number representation:** Integers beyond the i64/u64 range are parsed as f64,
/// which may lose precision. Serialization outputs numbers using their parsed representation.
#[derive(Clone, Debug)]
pub struct Value(Arc<serde_json::Value>);

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        *self.0 == *other.0
    }
}

/// A JSON error wrapping `serde_json`'s error.
#[derive(Debug)]
pub struct Error(serde_json::error::Error);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON error: {}", self.0)
    }
}

impl std::error::Error for Error {}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_string() == other.0.to_string()
    }
}

/// Parse a JSON value from a string.
///
/// # Errors
/// Returns an error if the input is malformed or exceeds the nesting depth limit.
pub fn parse(input: &str) -> Result<Value, Error> {
    serde_json::from_str(input)
        .map(|v| Value(Arc::new(v)))
        .map_err(Error)
}

/// Parse a JSON value from bytes.
///
/// # Errors
/// Returns an error if the input is malformed, non-UTF-8, or exceeds the nesting
/// depth limit.
pub fn parse_bytes(input: &[u8]) -> Result<Value, Error> {
    serde_json::from_slice(input)
        .map(|v| Value(Arc::new(v)))
        .map_err(Error)
}

/// Serialize a value to a JSON string with deterministic output and a trailing newline.
pub fn to_string(value: &Value) -> Result<String, Error> {
    let sorted = sort_value(&value.0)?;
    let mut s = serde_json::to_string(&sorted).map_err(Error)?;
    if !s.ends_with('\n') {
        s.push('\n');
    }
    Ok(s)
}

/// Serialize a value to pretty-printed JSON with deterministic output and a trailing newline.
pub fn to_string_pretty(value: &Value) -> Result<String, Error> {
    let sorted = sort_value(&value.0)?;
    let mut s = serde_json::to_string_pretty(&sorted).map_err(Error)?;
    if !s.ends_with('\n') {
        s.push('\n');
    }
    Ok(s)
}

/// Sort object keys recursively for deterministic output.
fn sort_value(val: &serde_json::Value) -> Result<serde_json::Value, Error> {
    match val {
        serde_json::Value::Object(obj) => {
            let mut sorted = BTreeMap::new();
            for (k, v) in obj {
                sorted.insert(k.clone(), sort_value(v)?);
            }
            Ok(serde_json::Value::Object(
                sorted
                    .into_iter()
                    .collect::<serde_json::map::Map<String, serde_json::Value>>(),
            ))
        }
        serde_json::Value::Array(arr) => {
            let sorted_arr: Result<Vec<_>, _> = arr.iter().map(sort_value).collect();
            Ok(serde_json::Value::Array(sorted_arr?))
        }
        other => Ok(other.clone()),
    }
}

impl Value {
    /// Access a value by object key, returning `None` if not an object or key not found.
    /// Returns a new Value wrapping the value (Arc-backed, cheap clone).
    pub fn get(&self, key: &str) -> Option<Value> {
        self.0.get(key).map(|v| Value(Arc::new(v.clone())))
    }

    /// Access a value by array index, returning `None` if not an array or index out of bounds.
    /// Returns a new Value wrapping the value (Arc-backed, cheap clone).
    pub fn get_index(&self, index: usize) -> Option<Value> {
        self.0.get(index).map(|v| Value(Arc::new(v.clone())))
    }

    /// Access a value using a JSON Pointer (RFC 6901) path.
    ///
    /// Empty path returns the whole document. Path must start with `/` or be `""`.
    /// Invalid escapes and non-numeric array indices result in no match.
    /// Array indices with leading zeros (`01`) and `-` are rejected.
    ///
    /// Unescapes: `~0` → `~`, `~1` → `/`. Other escapes (like `~2`) yield no match.
    pub fn pointer(&self, path: &str) -> Option<Value> {
        if path.is_empty() {
            return Some(self.clone());
        }

        let rest = path.strip_prefix('/')?;

        let tokens: Vec<&str> = rest.split('/').collect();
        let mut current = &*self.0;

        for token in tokens {
            let unescaped = unescape_pointer_token(token)?;

            // Handle array index access
            if current.is_array() {
                // Reject `-` (means past-the-end)
                if unescaped == "-" {
                    return None;
                }
                // Reject leading zeros (like "01")
                if unescaped.starts_with('0') && unescaped.len() > 1 {
                    return None;
                }
                // Try to parse as index
                if let Ok(index) = unescaped.parse::<usize>() {
                    current = current.get(index)?;
                    continue;
                }
                // Non-numeric token on array = no match
                return None;
            }

            // Treat as object key
            current = current.get(&unescaped)?;
        }

        Some(Value(Arc::new(current.clone())))
    }

    /// Return the value as a string, or `None` if it's not a string.
    pub fn as_str(&self) -> Option<&str> {
        self.0.as_str()
    }

    /// Return the value as a bool, or `None` if it's not a bool.
    pub fn as_bool(&self) -> Option<bool> {
        self.0.as_bool()
    }

    /// Return the value as an i64, or `None` if it's not an integer within that range.
    pub fn as_i64(&self) -> Option<i64> {
        self.0.as_i64()
    }

    /// Return the value as a u64, or `None` if it's not an integer within that range.
    pub fn as_u64(&self) -> Option<u64> {
        self.0.as_u64()
    }

    /// Return the value as an f64, or `None` if it's not a number.
    pub fn as_f64(&self) -> Option<f64> {
        self.0.as_f64()
    }

    /// Return the value as an array slice, or `None` if it's not an array.
    pub fn as_array(&self) -> Option<&[serde_json::Value]> {
        self.0.as_array().map(Vec::as_slice)
    }

    /// Return the value as an object map, or `None` if it's not an object.
    pub fn as_object(&self) -> Option<&serde_json::Map<String, serde_json::Value>> {
        self.0.as_object()
    }

    /// Return `true` if the value is null.
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }
}

/// Unescape a JSON Pointer token per RFC 6901.
/// Returns None if the escape sequence is invalid (e.g., `~2`).
fn unescape_pointer_token(token: &str) -> Option<String> {
    let mut result = String::new();
    let mut chars = token.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '~' {
            match chars.next() {
                Some('0') => result.push('~'),
                Some('1') => result.push('/'),
                Some(_) | None => return None, // Invalid escape like ~2, or trailing ~
            }
        } else {
            result.push(c);
        }
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_null() {
        assert!(parse("null").unwrap().is_null());
    }

    #[test]
    fn parse_bool() {
        assert_eq!(parse("true").unwrap().as_bool(), Some(true));
        assert_eq!(parse("false").unwrap().as_bool(), Some(false));
    }

    #[test]
    fn parse_integer() {
        assert_eq!(parse("42").unwrap().as_i64(), Some(42));
        assert_eq!(parse("-17").unwrap().as_i64(), Some(-17));
        assert_eq!(parse("0").unwrap().as_i64(), Some(0));
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn parse_float() {
        let v = parse("3.14").unwrap();
        let f = v.as_f64().unwrap();
        let pi_approx = 3.14;
        assert!((f - pi_approx).abs() < 0.001);
    }

    #[test]
    fn parse_scientific_notation() {
        let v = parse("1e10").unwrap();
        let f = v.as_f64().unwrap();
        assert!((f - 1e10).abs() < f64::EPSILON);

        let v = parse("1.5e-5").unwrap();
        let f = v.as_f64().unwrap();
        assert!((f - 1.5e-5).abs() < 1e-10);
    }

    #[test]
    fn parse_string() {
        assert_eq!(parse(r#""hello""#).unwrap().as_str(), Some("hello"));
    }

    #[test]
    fn parse_string_with_escapes() {
        assert_eq!(
            parse(r#""hello\"world""#).unwrap().as_str(),
            Some("hello\"world")
        );
        assert_eq!(
            parse(r#""line1\nline2""#).unwrap().as_str(),
            Some("line1\nline2")
        );
    }

    #[test]
    fn parse_unicode_escapes() {
        assert_eq!(parse(r#""A""#).unwrap().as_str(), Some("A"));
        // Surrogate pair (emoji)
        let v = parse(r#""😀""#).unwrap();
        assert_eq!(v.as_str(), Some("😀"));
    }

    #[test]
    fn parse_array() {
        let empty = parse("[]").unwrap();
        assert!(empty.as_array().unwrap().is_empty());

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
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr[0].as_array().is_some());
        assert!(arr[1].as_array().is_some());
    }

    #[test]
    fn parse_object() {
        let v = parse(r#"{"name":"Alice","age":30}"#).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        // Keys are in the order serde_json provides them
        assert!(obj.iter().any(|(k, _)| k == "name"));
        assert!(obj.iter().any(|(k, _)| k == "age"));
    }

    #[test]
    fn parse_object_with_duplicate_keys() {
        // serde_json keeps the last value for duplicate keys
        let v = parse(r#"{"key":"first","key":"second"}"#).unwrap();
        if let Some(val) = v.get("key") {
            assert_eq!(val.as_str(), Some("second"));
        } else {
            panic!("key not found");
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
    }

    #[test]
    fn parse_invalid_utf8() {
        let invalid = b"\"hello\xff\"";
        assert!(parse_bytes(invalid).is_err());
    }

    #[test]
    fn access_by_key() {
        let v = parse(r#"{"x":10,"y":20}"#).unwrap();
        assert_eq!(v.get("x").and_then(|v| v.as_i64()), Some(10));
        assert_eq!(v.get("y").and_then(|v| v.as_i64()), Some(20));
        assert_eq!(v.get("z"), None);
    }

    #[test]
    fn access_by_index() {
        let v = parse("[10,20,30]").unwrap();
        assert_eq!(v.get_index(0).and_then(|v| v.as_i64()), Some(10));
        assert_eq!(v.get_index(2).and_then(|v| v.as_i64()), Some(30));
        assert_eq!(v.get_index(5), None);
    }

    #[test]
    fn access_by_json_pointer() {
        let v = parse(r#"{"a":{"b":[1,2,3]}}"#).unwrap();
        assert_eq!(v.pointer("/a/b/1").and_then(|v| v.as_i64()), Some(2));
        assert_eq!(v.pointer("/a/b/10"), None);
        assert_eq!(v.pointer("/x"), None);
    }

    #[test]
    fn json_pointer_with_escaped_tokens() {
        // ~0 represents ~ and ~1 represents /
        let v = parse(r#"{"a/b":{"c~d":5}}"#).unwrap();
        assert_eq!(v.pointer("/a~1b/c~0d").and_then(|v| v.as_i64()), Some(5));
    }

    #[test]
    fn typed_getters() {
        let v = parse(r#"{"s":"text","b":true,"i":42,"f":3.14,"n":null,"a":[],"o":{}}"#).unwrap();

        if let Some(val) = v.get("s") {
            assert_eq!(val.as_str(), Some("text"));
        } else {
            panic!("s not found");
        }
        if let Some(val) = v.get("b") {
            assert_eq!(val.as_bool(), Some(true));
        } else {
            panic!("b not found");
        }
        if let Some(val) = v.get("i") {
            assert_eq!(val.as_i64(), Some(42));
        } else {
            panic!("i not found");
        }
        if let Some(val) = v.get("f") {
            assert!(val.as_i64().is_none());
        } else {
            panic!("f not found");
        }
        assert_eq!(v.get("n").map(|v| v.is_null()), Some(true));
        if let Some(arr_val) = v.get("a") {
            assert!(arr_val.as_array().is_some());
        }
        if let Some(obj_val) = v.get("o") {
            assert!(obj_val.as_object().is_some());
        }

        // Type mismatches return None
        if let Some(val) = v.get("s") {
            assert_eq!(val.as_bool(), None);
        }
        if let Some(val) = v.get("b") {
            assert_eq!(val.as_str(), None);
        }
    }

    #[test]
    fn serialization_determinism() {
        let v = parse(r#"{"z":3,"a":1,"m":2}"#).unwrap();
        let s1 = to_string(&v).unwrap();
        let s2 = to_string(&v).unwrap();
        assert_eq!(s1, s2);

        // Keys are sorted in output
        assert!(s1.find("\"a\"").unwrap() < s1.find("\"m\"").unwrap());
        assert!(s1.find("\"m\"").unwrap() < s1.find("\"z\"").unwrap());
    }

    #[test]
    fn serialization_has_trailing_newline() {
        let v = parse("{}").unwrap();
        let s = to_string(&v).unwrap();
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn pretty_serialization() {
        let v = parse(r#"{"a":1,"b":[2,3]}"#).unwrap();
        let s = to_string_pretty(&v).unwrap();
        assert!(s.contains('\n'));
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn round_trip() {
        let original = r#"{"name":"Alice","age":30,"tags":["a","b"]}"#;
        let v = parse(original).unwrap();
        let serialized = to_string(&v).unwrap();
        let v2 = parse(&serialized).unwrap();
        // Compare the actual JSON structure
        assert_eq!(v.get("name"), v2.get("name"));
        assert_eq!(v.get("age"), v2.get("age"));
    }

    #[test]
    fn option_chaining() {
        let v = parse(r#"{"x":{"y":10}}"#).unwrap();
        assert_eq!(
            v.get("x").and_then(|x| x.get("y")).and_then(|y| y.as_i64()),
            Some(10)
        );
        assert_eq!(
            v.get("x").and_then(|x| x.get("z")).and_then(|z| z.as_i64()),
            None
        );
    }

    #[test]
    fn large_numbers() {
        // i64::MAX
        let v = parse("9223372036854775807").unwrap();
        assert_eq!(v.as_i64(), Some(i64::MAX));

        // u64::MAX - represented as f64 with potential precision loss
        let v = parse("18446744073709551615").unwrap();
        assert!(v.as_f64().is_some());
    }

    #[test]
    fn negative_numbers() {
        let v = parse("-42").unwrap();
        assert_eq!(v.as_i64(), Some(-42));
        assert_eq!(v.as_u64(), None); // Negative, can't be u64
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
            "{\"key\":}",
            "[1,,2]",
        ];

        for input in invalid_inputs {
            let result = parse(input);
            assert!(result.is_err(), "should error on: {input}");
        }
    }

    #[test]
    fn parse_whitespace() {
        assert!(parse("  null  ").unwrap().is_null());
        let arr = parse("  [  1  ,  2  ]  ").unwrap();
        let arr_slice = arr.as_array().unwrap();
        assert_eq!(arr_slice.len(), 2);
        assert_eq!(arr_slice[0].as_i64(), Some(1));
        assert_eq!(arr_slice[1].as_i64(), Some(2));
    }

    #[test]
    fn deep_nesting_64_levels_succeeds() {
        // Construct 64 nested arrays: [[[[...]]]]
        let mut s = String::new();
        for _ in 0..64 {
            s.push('[');
        }
        s.push('1');
        for _ in 0..64 {
            s.push(']');
        }
        assert!(parse(&s).is_ok(), "64 levels should parse successfully");
    }

    #[test]
    fn deep_nesting_beyond_the_recursion_limit_is_an_error() {
        // serde_json's recursion limit allows 127 nested containers around a
        // scalar; one more is an error rather than a stack overflow.
        let nested = |depth: usize| format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
        assert!(
            parse(&nested(127)).is_ok(),
            "127 levels are within the limit"
        );
        let error = parse(&nested(128)).expect_err("128 levels exceed the limit");
        assert!(error.to_string().contains("recursion limit"), "{error}");
        assert!(
            parse(&nested(100_000)).is_err(),
            "very deep input must not overflow the stack"
        );
    }

    #[test]
    fn rfc6901_pointer_empty_path_returns_root() {
        let v = parse(r#"{"x":1}"#).unwrap();
        assert_eq!(v.pointer("").unwrap().as_object().unwrap().len(), 1);
    }

    #[test]
    fn rfc6901_array_leading_zeros_rejected() {
        let v = parse(r#"{"a":[1,2,3]}"#).unwrap();
        assert!(
            v.pointer("/a/01").is_none(),
            "leading zero '01' should be rejected"
        );
    }

    #[test]
    fn rfc6901_array_past_end_rejected() {
        let v = parse(r#"{"a":[1,2,3]}"#).unwrap();
        assert!(
            v.pointer("/a/-").is_none(),
            "'-' means past-the-end and should be rejected"
        );
    }

    #[test]
    fn rfc6901_array_non_numeric_rejected() {
        let v = parse(r#"{"a":[1,2,3]}"#).unwrap();
        assert!(
            v.pointer("/a/foo").is_none(),
            "non-numeric key on array should be rejected"
        );
    }

    #[test]
    fn rfc6901_invalid_escapes_rejected() {
        let v = parse(r#"{"a":1}"#).unwrap();
        // Invalid escape ~2 should yield no match
        assert!(v.pointer("/a~2").is_none(), "invalid escape ~2 should fail");
        // Trailing ~ should yield no match
        assert!(v.pointer("/a~").is_none(), "trailing ~ should fail");
    }

    #[test]
    fn rfc6901_non_slash_start_rejected() {
        let v = parse(r#"{"a":1}"#).unwrap();
        // Path not starting with / (except "") should be None
        assert!(
            v.pointer("a").is_none(),
            "path not starting with / should be None"
        );
        assert!(v.pointer("foo").is_none());
    }
}

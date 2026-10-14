//! Crate-private JSON layer (DP0-1).
//!
//! `serde_json` does the parsing and serialization; no `serde_json` type
//! appears in this module's API. Callers parse into a [`Document`] and read it
//! through borrowed [`Node`] views, so navigating never copies a subtree.
//!
//! - **Determinism:** objects are stored and iterated in sorted key order
//!   (`serde_json` without `preserve_order` uses a `BTreeMap`), so the same
//!   input always serializes to the same bytes. Serialized output ends with
//!   exactly one newline.
//! - **Nesting:** `serde_json` accepts at most 127 nested arrays or objects.
//!   Deeper input is an error, not a stack overflow.
//! - **Duplicate keys:** the last occurrence wins.
//! - **Numbers:** integers that fit `i64` or `u64` are exact. Anything else,
//!   including integers beyond `u64::MAX`, is an `f64` and may lose precision.
//!   `-0` is the float negative zero. Typed getters never convert between
//!   integers and floats.
//! - **Size:** input size is not limited here; captured output is bounded by
//!   the transport (#31).

// The probe orchestration (#30) is the first consumer of this module.
#![cfg_attr(not(test), allow(dead_code))]

use std::fmt;

/// A parsed JSON document.
#[derive(Debug, PartialEq)]
pub(crate) struct Document(serde_json::Value);

/// A borrowed view of one value inside a [`Document`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Node<'a>(&'a serde_json::Value);

/// The JSON type of a [`Node`], for reporting unexpected shapes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

/// A JSON parse or serialization error.
#[derive(Debug)]
pub(crate) struct Error(serde_json::Error);

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid JSON: {}", self.0)
    }
}

impl std::error::Error for Error {}

impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Null => "null",
            Self::Bool => "boolean",
            Self::Number => "number",
            Self::String => "string",
            Self::Array => "array",
            Self::Object => "object",
        })
    }
}

/// Parses a JSON document from text.
pub(crate) fn parse(input: &str) -> Result<Document, Error> {
    serde_json::from_str(input).map(Document).map_err(Error)
}

/// Parses a JSON document from bytes, which must be UTF-8.
pub(crate) fn parse_bytes(input: &[u8]) -> Result<Document, Error> {
    serde_json::from_slice(input).map(Document).map_err(Error)
}

impl Document {
    /// The document's top-level value.
    pub(crate) fn root(&self) -> Node<'_> {
        Node(&self.0)
    }

    /// Compact serialization with sorted keys and a trailing newline.
    pub(crate) fn to_compact_string(&self) -> Result<String, Error> {
        self.root().to_compact_string()
    }

    /// Pretty serialization (two-space indent) with sorted keys and a trailing
    /// newline.
    pub(crate) fn to_pretty_string(&self) -> Result<String, Error> {
        self.root().to_pretty_string()
    }
}

impl<'a> Node<'a> {
    pub(crate) fn kind(self) -> Kind {
        match self.0 {
            serde_json::Value::Null => Kind::Null,
            serde_json::Value::Bool(_) => Kind::Bool,
            serde_json::Value::Number(_) => Kind::Number,
            serde_json::Value::String(_) => Kind::String,
            serde_json::Value::Array(_) => Kind::Array,
            serde_json::Value::Object(_) => Kind::Object,
        }
    }

    pub(crate) fn is_null(self) -> bool {
        self.0.is_null()
    }

    pub(crate) fn as_bool(self) -> Option<bool> {
        self.0.as_bool()
    }

    pub(crate) fn as_str(self) -> Option<&'a str> {
        self.0.as_str()
    }

    /// The value as an `i64`, only if it is an integer in range.
    pub(crate) fn as_i64(self) -> Option<i64> {
        self.0.as_i64()
    }

    /// The value as a `u64`, only if it is a non-negative integer in range.
    pub(crate) fn as_u64(self) -> Option<u64> {
        self.0.as_u64()
    }

    /// The value as an `f64`, only if it was not representable as an integer.
    pub(crate) fn as_f64(self) -> Option<f64> {
        match self.0 {
            serde_json::Value::Number(number) if number.is_f64() => number.as_f64(),
            _ => None,
        }
    }

    /// The member named `key`, if this is an object that has it.
    pub(crate) fn get(self, key: &str) -> Option<Node<'a>> {
        self.0.as_object()?.get(key).map(Node)
    }

    /// The element at `index`, if this is an array long enough.
    pub(crate) fn index(self, index: usize) -> Option<Node<'a>> {
        self.0.as_array()?.get(index).map(Node)
    }

    /// The elements, in order, if this is an array.
    pub(crate) fn elements(self) -> Option<impl ExactSizeIterator<Item = Node<'a>>> {
        self.0.as_array().map(|items| items.iter().map(Node))
    }

    /// The members in sorted key order, if this is an object.
    pub(crate) fn members(self) -> Option<impl ExactSizeIterator<Item = (&'a str, Node<'a>)>> {
        self.0
            .as_object()
            .map(|map| map.iter().map(|(key, value)| (key.as_str(), Node(value))))
    }

    /// Resolves an RFC 6901 JSON Pointer relative to this value.
    ///
    /// `""` is this value. Every other pointer starts with `/`. In each
    /// reference token `~1` means `/` and `~0` means `~`; any other `~` makes
    /// the pointer invalid. Array tokens must be `0` or a decimal number
    /// without leading zeros or a sign; `-` (past the end) never resolves.
    /// Invalid pointers resolve to `None`.
    pub(crate) fn pointer(self, pointer: &str) -> Option<Node<'a>> {
        if pointer.is_empty() {
            return Some(self);
        }
        let mut current = self.0;
        for token in pointer.strip_prefix('/')?.split('/') {
            let token = unescape(token)?;
            current = match current {
                serde_json::Value::Object(map) => map.get(token.as_str())?,
                serde_json::Value::Array(items) => items.get(array_index(&token)?)?,
                _ => return None,
            };
        }
        Some(Node(current))
    }

    pub(crate) fn to_compact_string(self) -> Result<String, Error> {
        serde_json::to_string(self.0)
            .map(with_newline)
            .map_err(Error)
    }

    pub(crate) fn to_pretty_string(self) -> Result<String, Error> {
        serde_json::to_string_pretty(self.0)
            .map(with_newline)
            .map_err(Error)
    }
}

fn unescape(token: &str) -> Option<String> {
    let mut unescaped = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(character) = chars.next() {
        if character == '~' {
            match chars.next() {
                Some('0') => unescaped.push('~'),
                Some('1') => unescaped.push('/'),
                _ => return None,
            }
        } else {
            unescaped.push(character);
        }
    }
    Some(unescaped)
}

fn array_index(token: &str) -> Option<usize> {
    let well_formed = token == "0"
        || (!token.is_empty()
            && !token.starts_with('0')
            && token.bytes().all(|byte| byte.is_ascii_digit()));
    if well_formed {
        token.parse().ok()
    } else {
        None
    }
}

fn with_newline(mut text: String) -> String {
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::{Kind, parse, parse_bytes};

    fn doc(text: &str) -> super::Document {
        parse(text).expect("valid JSON")
    }

    #[test]
    fn parses_every_kind() {
        let document = doc(r#"{"a":null,"b":true,"c":1,"d":"x","e":[],"f":{}}"#);
        let root = document.root();
        let kinds: Vec<_> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(|key| root.get(key).map(super::Node::kind))
            .collect();
        assert_eq!(
            kinds,
            [
                Some(Kind::Null),
                Some(Kind::Bool),
                Some(Kind::Number),
                Some(Kind::String),
                Some(Kind::Array),
                Some(Kind::Object),
            ]
        );
        assert_eq!(root.kind(), Kind::Object);
        assert_eq!(Kind::Array.to_string(), "array");
    }

    #[test]
    fn rejects_malformed_input_without_panicking() {
        for input in [
            "",
            "{",
            "[1,",
            r#"{"a":}"#,
            r#"{"a" 1}"#,
            "[1] trailing",
            r#""unterminated"#,
            r#""\x""#,
            r#""\u12""#,
            "01",
            "tru",
            "nul",
            "NaN",
            "[1,]",
        ] {
            let error = parse(input).expect_err(input);
            assert!(error.to_string().starts_with("invalid JSON:"), "{error}");
        }
    }

    #[test]
    fn rejects_invalid_utf8_bytes() {
        assert!(parse_bytes(b"\"\xff\"").is_err());
        assert_eq!(
            parse_bytes(br#"{"a":"b"}"#)
                .expect("valid UTF-8")
                .root()
                .pointer("/a")
                .and_then(super::Node::as_str),
            Some("b")
        );
    }

    #[test]
    fn decodes_escape_sequences() {
        let document = doc(r#"["\"\\\/\b\f\n\r\t", "\u00e9", "\ud83d\ude00"]"#);
        let strings: Vec<_> = document
            .root()
            .elements()
            .expect("array")
            .map(|node| node.as_str().expect("string"))
            .collect();
        assert_eq!(strings, ["\"\\/\u{8}\u{c}\n\r\t", "\u{e9}", "\u{1f600}"]);
        // A lone high surrogate is not valid JSON text.
        assert!(parse(r#""\ud83d""#).is_err());
    }

    #[test]
    fn integers_and_floats_are_never_coerced() {
        let document = doc("[1, -1, 1.0, 1e3, 18446744073709551615, 18446744073709551616, -0]");
        let root = document.root();
        let at = |index| root.index(index).expect("element");

        assert_eq!(
            (at(0).as_i64(), at(0).as_u64(), at(0).as_f64()),
            (Some(1), Some(1), None)
        );
        assert_eq!((at(1).as_i64(), at(1).as_u64()), (Some(-1), None));
        assert_eq!((at(2).as_i64(), at(2).as_f64()), (None, Some(1.0)));
        assert_eq!((at(3).as_i64(), at(3).as_f64()), (None, Some(1000.0)));
        // u64::MAX is exact; one more no longer fits and becomes a float.
        assert_eq!(at(4).as_u64(), Some(u64::MAX));
        assert_eq!(
            (at(5).as_u64(), at(5).as_f64()),
            (None, Some(18_446_744_073_709_551_616.0))
        );
        // `-0` parses as the float negative zero, not an integer.
        assert_eq!((at(6).as_i64(), at(6).as_f64()), (None, Some(-0.0)));
        assert_eq!(
            document.to_compact_string().expect("serialize"),
            "[1,-1,1.0,1000.0,18446744073709551615,1.8446744073709552e+19,-0.0]\n"
        );
    }

    #[test]
    fn typed_getters_reject_other_kinds() {
        let document = doc(r#"{"s":"1","n":1,"b":true,"z":null}"#);
        let root = document.root();
        let s = root.get("s").expect("s");
        assert_eq!((s.as_i64(), s.as_bool(), s.is_null()), (None, None, false));
        assert_eq!(root.get("n").and_then(super::Node::as_str), None);
        assert_eq!(root.get("b").and_then(super::Node::as_u64), None);
        assert!(root.get("z").expect("z").is_null());
        assert!(root.elements().is_none());
        assert!(root.get("s").expect("s").members().is_none());
        assert_eq!(root.get("s").and_then(|node| node.get("x")), None);
        assert_eq!(root.get("s").and_then(|node| node.index(0)), None);
    }

    #[test]
    fn duplicate_keys_keep_the_last_value() {
        let document = doc(r#"{"a":1,"a":2}"#);
        assert_eq!(
            document.root().get("a").and_then(super::Node::as_i64),
            Some(2)
        );
        assert_eq!(document.root().members().expect("object").len(), 1);
    }

    #[test]
    fn nesting_beyond_the_recursion_limit_is_an_error() {
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
    fn navigation_borrows_from_the_document() {
        let document = doc(r#"{"a":{"b":[10,20,30]}}"#);
        let b = document
            .root()
            .get("a")
            .and_then(|a| a.get("b"))
            .expect("b");
        assert_eq!(b.index(2).and_then(super::Node::as_i64), Some(30));
        assert_eq!(b.index(3), None);
        assert_eq!(b.elements().expect("array").len(), 3);
        // A node is a reference into the document, not a copy.
        assert!(std::ptr::eq(
            b.0,
            document.root().pointer("/a/b").expect("b").0
        ));
    }

    #[test]
    fn members_iterate_in_sorted_key_order() {
        let document = doc(r#"{"b":1,"a":2,"c":3}"#);
        let keys: Vec<_> = document
            .root()
            .members()
            .expect("object")
            .map(|(key, _)| key)
            .collect();
        assert_eq!(keys, ["a", "b", "c"]);
    }

    #[test]
    fn pointer_resolves_rfc_6901_paths() {
        let document = doc(
            r#"{"state":{"resources.pipelines.p":{"__id__":"abc"}},"a/b":{"c~d":5},"":{"":1},"list":[[1,2],[3]]}"#,
        );
        let root = document.root();
        let lookup = |pointer: &str| root.pointer(pointer);

        assert_eq!(lookup(""), Some(root));
        assert_eq!(
            lookup("/state/resources.pipelines.p/__id__").and_then(super::Node::as_str),
            Some("abc")
        );
        assert_eq!(lookup("/a~1b/c~0d").and_then(super::Node::as_i64), Some(5));
        assert_eq!(lookup("//").and_then(super::Node::as_i64), Some(1));
        assert_eq!(lookup("/list/0/1").and_then(super::Node::as_i64), Some(2));
        assert_eq!(lookup("/list/1/0").and_then(super::Node::as_i64), Some(3));
    }

    #[test]
    fn pointer_rejects_invalid_or_missing_paths() {
        let document = doc(r#"{"a":[1,2],"b":{"c":1}}"#);
        let root = document.root();
        for pointer in [
            "a", // missing leading slash
            "/missing", "/a/2",   // out of range
            "/a/-",   // past-the-end marker never resolves for reads
            "/a/01",  // leading zero
            "/a/+1",  // sign
            "/a/-1",  // negative
            "/a/1.0", // not an integer
            "/a/x",   // non-numeric token on an array
            "/a/",    // empty token on an array
            "/b/c/d", // descending into a scalar
            "/b/~2",  // invalid escape
            "/b/c~",  // trailing tilde
        ] {
            assert_eq!(root.pointer(pointer), None, "{pointer}");
        }
    }

    #[test]
    fn serialization_is_sorted_deterministic_and_newline_terminated() {
        let input = r#"{"z":{"y":1,"x":[{"b":1,"a":2}]},"a":"é"}"#;
        let first = doc(input).to_compact_string().expect("serialize");
        let second = doc(input).to_compact_string().expect("serialize");
        assert_eq!(first, second);
        assert_eq!(
            first,
            "{\"a\":\"\u{e9}\",\"z\":{\"x\":[{\"a\":2,\"b\":1}],\"y\":1}}\n"
        );

        let pretty = doc(input).to_pretty_string().expect("serialize");
        assert_eq!(
            pretty,
            "{\n  \"a\": \"\u{e9}\",\n  \"z\": {\n    \"x\": [\n      {\n        \"a\": 2,\n        \"b\": 1\n      }\n    ],\n    \"y\": 1\n  }\n}\n"
        );
        assert!(!pretty.ends_with("\n\n"));
    }

    #[test]
    fn serialization_round_trips() {
        let document = doc(r#"{"b":[1,2.5,"s",null,true],"a":{}}"#);
        let text = document.to_compact_string().expect("serialize");
        let reparsed = doc(&text);
        assert_eq!(reparsed, document);
        assert_eq!(reparsed.to_compact_string().expect("serialize"), text);
        assert_eq!(
            document
                .root()
                .get("b")
                .expect("b")
                .to_compact_string()
                .expect("serialize"),
            "[1,2.5,\"s\",null,true]\n"
        );
    }
}

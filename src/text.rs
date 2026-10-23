//! Quoting text into Databricks SQL (#31).
//!
//! Prefer statement parameters for values; these are for the places SQL
//! needs text spelled out, such as a `--scope-catalog` identifier.

/// `value` as a SQL string literal, or `None` if it holds a `'` or a `\`.
///
/// The literal must mean the same text whether or not
/// `spark.sql.parser.escapedStringLiterals` is on, and neither character
/// has a spelling that does: with the setting off a backslash escapes the
/// next character, so `\\` is one backslash, and with it on `\\` is two. A
/// quote can only be written as `\'`, which ends the literal under that
/// setting (`'a''b'` is two adjacent literals, not `a'b`). Every other
/// character stands for itself either way, so a value with either of these
/// must be a statement parameter instead.
pub(crate) fn string_literal(value: &str) -> Option<String> {
    if value.contains(['\'', '\\']) {
        return None;
    }
    Some(format!("'{value}'"))
}

/// `name` as a backtick-quoted identifier. Always quoted, so a reserved word
/// or any other text is read as a name; an embedded backtick is doubled.
pub(crate) fn identifier(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

#[cfg(test)]
mod tests {
    use super::{identifier, string_literal};
    use crate::safety::is_read_statement;

    #[test]
    fn quotes_text_and_refuses_quotes_and_backslashes() {
        let literal = |value| string_literal(value).expect(value);
        assert_eq!(literal("plain"), "'plain'");
        assert_eq!(literal(r#"say "hi""#), r#"'say "hi"'"#);
        assert_eq!(literal(""), "''");
        assert_eq!(literal("naïve\n☃"), "'naïve\n☃'");
        assert_eq!(string_literal("it's"), None);
        assert_eq!(string_literal(r"x\'; DROP TABLE t; --"), None);
        // `\\` reads as one backslash or two, depending on the setting.
        assert_eq!(string_literal(r"C:\dir"), None);
        assert_eq!(string_literal(r"a\b"), None);
    }

    #[test]
    fn always_quotes_identifiers() {
        assert_eq!(identifier("main"), "`main`");
        assert_eq!(identifier("select"), "`select`");
        assert_eq!(identifier("a`b"), "`a``b`");
        assert_eq!(identifier(""), "``");
    }

    #[test]
    fn quoted_text_cannot_end_a_statement() {
        for hostile in [
            "x\"; DROP TABLE t; --",
            "x` ; DROP TABLE t; --",
            "x -- ; DROP TABLE t",
            "x /* ; DROP TABLE t",
        ] {
            let literal = string_literal(hostile).expect(hostile);
            let sql = format!("SELECT {literal} FROM {}", identifier(hostile));
            assert!(is_read_statement(&sql), "{sql}");
        }
    }
}

//! Quoting text into Databricks SQL (#31).
//!
//! Prefer statement parameters for values; these are for the places SQL
//! needs text spelled out, such as a `--scope-catalog` identifier.

/// `value` as a SQL string literal, or `None` if it holds a `'`.
///
/// Databricks SQL reads a backslash in a literal as an escape, so each one
/// is doubled. A quote has no form that ends the literal in the same place
/// whether or not `spark.sql.parser.escapedStringLiterals` is on: `'a''b'` is
/// two adjacent literals, and `\'` ends the literal under that setting. The
/// read-only guard refuses `\'` for the same reason, so a value with a quote
/// must be a statement parameter instead.
pub(crate) fn string_literal(value: &str) -> Option<String> {
    if value.contains('\'') {
        return None;
    }
    let mut literal = String::with_capacity(value.len() + 2);
    literal.push('\'');
    for character in value.chars() {
        if character == '\\' {
            literal.push('\\');
        }
        literal.push(character);
    }
    literal.push('\'');
    Some(literal)
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
    fn escapes_backslashes_and_refuses_quotes() {
        let literal = |value| string_literal(value).expect(value);
        assert_eq!(literal("plain"), "'plain'");
        assert_eq!(literal(r"C:\dir\"), r"'C:\\dir\\'");
        assert_eq!(literal(r#"say "hi""#), r#"'say "hi"'"#);
        assert_eq!(literal(""), "''");
        assert_eq!(literal("naïve\n☃"), "'naïve\n☃'");
        assert_eq!(string_literal("it's"), None);
        assert_eq!(string_literal(r"x\'; DROP TABLE t; --"), None);
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
            r"x\; DROP TABLE t; --\",
            r"x\\",
            "x\"; DROP TABLE t; --",
            "x` ; DROP TABLE t; --",
        ] {
            let literal = string_literal(hostile).expect(hostile);
            let sql = format!("SELECT {literal} FROM {}", identifier(hostile));
            assert!(is_read_statement(&sql), "{sql}");
        }
    }
}

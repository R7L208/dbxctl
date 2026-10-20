//! Quoting text into Databricks SQL (#31).
//!
//! Prefer statement parameters for values; these are for the places SQL
//! needs text spelled out, such as a `--scope-catalog` identifier.

/// `value` as a SQL string literal.
///
/// Databricks SQL reads a backslash in a literal as an escape, and reads
/// `'a''b'` as two adjacent literals rather than an escaped quote, so both
/// the backslash and the quote are escaped with a backslash.
pub(crate) fn string_literal(value: &str) -> String {
    let mut literal = String::with_capacity(value.len() + 2);
    literal.push('\'');
    for character in value.chars() {
        if matches!(character, '\\' | '\'') {
            literal.push('\\');
        }
        literal.push(character);
    }
    literal.push('\'');
    literal
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
    fn escapes_quotes_and_backslashes() {
        assert_eq!(string_literal("plain"), "'plain'");
        assert_eq!(string_literal("it's"), r"'it\'s'");
        assert_eq!(string_literal(r"C:\dir\"), r"'C:\\dir\\'");
        assert_eq!(string_literal(""), "''");
        assert_eq!(string_literal("naïve\n☃"), "'naïve\n☃'");
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
            r"x\'; DROP TABLE t; --",
            "x'; DROP TABLE t; --",
            "x` ; DROP TABLE t; --",
        ] {
            let sql = format!(
                "SELECT {} FROM {}",
                string_literal(hostile),
                identifier(hostile)
            );
            assert!(is_read_statement(&sql), "{sql}");
        }
    }
}

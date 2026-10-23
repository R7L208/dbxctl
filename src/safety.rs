//! The read-only SQL guard (#31).
//!
//! [`is_read_statement`] decides whether a statement may be sent to a SQL
//! warehouse. The transport calls it before it starts any CLI process, so a
//! refused statement never reaches the workspace.

/// Words that may start a read-only statement.
const READ_STARTS: [&str; 5] = ["DESC", "DESCRIBE", "SELECT", "SHOW", "WITH"];
/// Words that start a statement that writes or changes state. One anywhere
/// outside a literal, quoted identifier, or comment refuses the statement,
/// which also catches a write after a common table expression such as
/// `WITH t AS (…) INSERT INTO …`. `INTO` is refused too, so no form of
/// `SELECT … INTO` can write. Quote an identifier with one of these names.
/// The one exception is `SHOW CREATE TABLE`, which reads a definition.
const WRITE_WORDS: [&str; 22] = [
    "ALTER", "ANALYZE", "CACHE", "CALL", "COPY", "CREATE", "DELETE", "DROP", "GRANT", "INSERT",
    "INTO", "LOAD", "MERGE", "MSCK", "OPTIMIZE", "REFRESH", "RESTORE", "REVOKE", "TRUNCATE",
    "UNCACHE", "UPDATE", "VACUUM",
];
/// Functions with an effect outside the query: `http_request` calls out
/// through an HTTP connection, `remote_query` runs SQL of its own on a
/// federated database, and `ai_query` invokes a serving endpoint, which may
/// be an agent with tools. A `SELECT` that names one is refused like a write.
/// A user-defined function can also have effects; the guard cannot see into
/// one, so dbxctl's statements call only built-in functions.
const SIDE_EFFECT_FUNCTIONS: [&str; 3] = ["AI_QUERY", "HTTP_REQUEST", "REMOTE_QUERY"];

/// Whether `sql` is a single statement that only reads: `SELECT`,
/// `WITH … SELECT`, `SHOW` (including `SHOW CREATE TABLE`), or
/// `DESCRIBE`/`DESC`, and that names no function with side effects.
///
/// Comments, string literals, and backtick-quoted identifiers are skipped
/// as Databricks SQL reads them: `--` and nested `/* */` comments, `'` and `"`
/// literals with backslash escapes, raw `r'…'` and `r"…"` literals without
/// them, and backtick identifiers with doubled backticks. Anything that
/// cannot be read, such as an unterminated literal, a `/*+` query hint, or a
/// backslash before a literal's own quote, is refused. One trailing
/// semicolon is allowed.
pub(crate) fn is_read_statement(sql: &str) -> bool {
    let Some(tokens) = tokenize(sql) else {
        return false;
    };
    let mut words = Vec::new();
    let mut ended = false;
    for token in tokens {
        match token {
            // Anything after a semicolon is a second statement.
            _ if ended => return false,
            Token::Semicolon => ended = true,
            Token::Word(word) => words.push(word.to_ascii_uppercase()),
            Token::Other => {}
        }
    }
    let Some(first) = words.first() else {
        return false;
    };
    // `SHOW CREATE` names the statement whose text it shows; a SHOW
    // statement cannot hold another statement.
    let show_create = first == "SHOW" && words.get(1).is_some_and(|word| word == "CREATE");
    READ_STARTS.contains(&first.as_str())
        && !words.iter().enumerate().any(|(at, word)| {
            (WRITE_WORDS.contains(&word.as_str()) && !(show_create && at == 1))
                || SIDE_EFFECT_FUNCTIONS.contains(&word.as_str())
        })
}

#[derive(Debug, PartialEq)]
enum Token<'a> {
    Word(&'a str),
    Semicolon,
    /// A literal, a quoted identifier, or punctuation.
    Other,
}

/// Splits `sql` into tokens, dropping comments and whitespace, or returns
/// `None` if a literal, identifier, or comment is not closed.
fn tokenize(sql: &str) -> Option<Vec<Token<'_>>> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let rest = &bytes[index..];
        if byte.is_ascii_whitespace() {
            index += 1;
        } else if rest.starts_with(b"--") {
            index = line_comment_end(bytes, index)?;
        } else if rest.starts_with(b"/*+") {
            // Spark reads a query hint as tokens, not a comment.
            return None;
        } else if rest.starts_with(b"/*") {
            index = block_comment_end(bytes, index)?;
        } else if byte == b'\'' || byte == b'"' {
            index = literal_end(bytes, index)?;
            tokens.push(Token::Other);
        } else if byte == b'`' {
            index = identifier_end(bytes, index)?;
            tokens.push(Token::Other);
        } else if byte == b';' {
            index += 1;
            tokens.push(Token::Semicolon);
        } else if is_word_byte(byte) {
            let start = index;
            while index < bytes.len() && is_word_byte(bytes[index]) {
                index += 1;
            }
            // Word bytes are ASCII, so these are character boundaries.
            let word = &sql[start..index];
            if word.eq_ignore_ascii_case("r") && matches!(bytes.get(index), Some(b'\'' | b'"')) {
                index = raw_literal_end(bytes, index)?;
                tokens.push(Token::Other);
            } else {
                tokens.push(Token::Word(word));
            }
        } else {
            // Punctuation or a non-ASCII byte outside any literal; step over
            // a whole character so `index` stays on a boundary.
            index += sql[index..].chars().next().map_or(1, char::len_utf8);
            tokens.push(Token::Other);
        }
    }
    Some(tokens)
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn find_byte(needle: u8, bytes: &[u8], from: usize) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|&byte| byte == needle)
        .map(|offset| from + offset)
}

/// The index of the line break that ends the `--` comment starting at
/// `start`, or the end of `sql`. As in Spark, `\r` ends it as well as `\n`.
/// Spark continues a comment past a backslash before `\n`; that is refused
/// rather than followed, so the guard never has to agree on where it ends.
fn line_comment_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 2;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if bytes.get(index + 1) == Some(&b'\n') => return None,
            b'\r' | b'\n' => return Some(index),
            _ => index += 1,
        }
    }
    Some(index)
}

/// The index after the comment starting at `start`. Comments nest. Spark
/// does not nest a `/*+` inside a comment, so the two would disagree on where
/// it ends; that is refused.
fn block_comment_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0_usize;
    let mut index = start;
    while index + 1 < bytes.len() {
        match &bytes[index..index + 2] {
            b"/*" if depth > 0 && bytes.get(index + 2) == Some(&b'+') => return None,
            b"/*" => {
                depth += 1;
                index += 2;
            }
            b"*/" => {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => index += 1,
        }
    }
    None
}

/// The index after the literal starting at `start`, whose quote is
/// `bytes[start]`. A backslash escapes the next byte.
///
/// With `spark.sql.parser.escapedStringLiterals` on, Spark reads backslashes
/// literally and ends a literal at its first quote. The two readings end it
/// in different places only when a backslash escapes the quote, so that is
/// refused and the guard holds under either setting.
fn literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if bytes.get(index + 1) == Some(&quote) => return None,
            b'\\' => index += 2,
            byte if byte == quote => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

/// The index after the raw literal whose quote is `bytes[start]`. A
/// backslash is an ordinary byte, so the first matching quote ends it.
fn raw_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    find_byte(bytes[start], bytes, start + 1).map(|end| end + 1)
}

/// The index after the backtick identifier starting at `start`. A doubled
/// backtick is a literal backtick.
fn identifier_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            if bytes.get(index + 1) == Some(&b'`') {
                index += 2;
            } else {
                return Some(index + 1);
            }
        } else {
            index += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::is_read_statement;

    #[test]
    fn accepts_reads() {
        for sql in [
            "SELECT 1",
            "select 1;",
            "  SELECT 1 ;  -- trailing comment",
            "-- one\r\n-- two\rSELECT 1 -- three \\ still one line",
            "WITH t AS (SELECT 1 AS x) SELECT x FROM t",
            "SHOW TABLES IN main.default",
            "DESCRIBE HISTORY main.default.t",
            "DESC EXTENDED t",
            "SHOW CREATE TABLE main.default.mv",
            "show create table `a`.`b`.`c` AS SERDE",
            "-- leading\n/* block /* nested */ still */ SELECT 1",
            "SELECT 'DROP TABLE t; -- not a comment' AS text",
            "SELECT \"it's\" AS text, 'say \"hi\"' AS other, 'C:\\\\' AS path",
            "SELECT 'tab\\there' AS text",
            "SELECT * FROM `update` JOIN `a``b` USING (id)",
            "SELECT `into` FROM t",
            "SELECT `http_request`, 'ai_query(x)' AS text FROM t",
            "SELECT 'naïve ☃' AS text, x·y FROM t",
            "SELECT r'C:\\temp\\' AS path, R\"\\d+\" AS pattern",
            "SELECT r FROM (SELECT 'a' AS r)",
            "SELECT * FROM system.lakeflow.pipelines WHERE name = :name",
        ] {
            assert!(is_read_statement(sql), "{sql}");
        }
    }

    #[test]
    fn refuses_writes_and_multiple_statements() {
        for sql in [
            "",
            "  ;",
            "-- only a comment",
            "INSERT INTO t VALUES (1)",
            "WITH t AS (SELECT 1) INSERT INTO u SELECT * FROM t",
            "WITH t AS (SELECT 1) MERGE INTO u USING t ON true WHEN MATCHED THEN DELETE",
            "SELECT 1; DROP TABLE t",
            "SELECT 1;;",
            "SELECT '--'; DROP TABLE t",
            "WITH t AS (SELECT 1) --\rINSERT INTO u SELECT * FROM t",
            "SELECT 1 --\r; DROP TABLE t",
            "SELECT 1 --\\\n'\n; DROP TABLE t\n--'",
            "SELECT 1 -- continued \\\n",
            "SELECT \"--\"; DROP TABLE t",
            "SELECT `--`; DROP TABLE t",
            "SELECT '/*'; DROP TABLE t; --*/",
            "/* SELECT */ DROP TABLE t",
            "SELECT * FROM t /* unterminated",
            "SELECT /* /* nested */ unterminated",
            "WITH x AS (SELECT /*+ `*/ 1) -- ` */ 1) INSERT INTO u SELECT 1",
            "WITH t AS (SELECT 1 /* /*+ */) INSERT INTO u SELECT 1 -- */",
            "SELECT /*+ BROADCAST(t) */ * FROM t",
            "SELECT 'unterminated",
            "SELECT 'escaped quote at end\\'",
            "SELECT '\\'; DROP TABLE t; --'",
            "SELECT \"it\\\"s\" AS text",
            "SELECT 'it\\'s' AS text",
            "SELECT 'C:\\\\\\'; DROP TABLE t; --'",
            "SELECT r'\\'; DROP TABLE t; --'",
            "WITH t AS (SELECT r'\\') INSERT INTO u SELECT 1 --'",
            "SELECT R\"\\\"; DROP TABLE t; --\"",
            "SELECT r'unterminated",
            "SELECT `unterminated",
            "REFRESH MATERIALIZED VIEW mv",
            "SET spark.x = 1",
            "USE CATALOG main",
            "EXPLAIN SELECT 1",
            "SELECT * FROM update",
            "SELECT * INTO u FROM t",
            "WITH t AS (SELECT 1 AS x) SELECT x INTO v FROM t",
            "OPTIMIZE t",
            "SHOW CREATE TABLE t; DROP TABLE t",
            "SHOW CREATE CREATE TABLE t",
            "SHOW TABLES CREATE TABLE t",
            "CREATE TABLE t AS SELECT 1",
            "SELECT http_request(conn => 'hook', method => 'POST', path => '/delete', json => '{}')",
            "SELECT HTTP_REQUEST('hook', 'POST', '/x')",
            "WITH t AS (SELECT 1) SELECT remote_query('pg', database => 'd', query => 'DELETE FROM x') FROM t",
            "SELECT ai_query('agent', 'drop every table') AS answer",
            "SELECT /* */ ai_query('agent', 'x')",
        ] {
            assert!(!is_read_statement(sql), "{sql}");
        }
    }
}

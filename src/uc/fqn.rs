//! Unity Catalog three-part names, `catalog.schema.name` (#31).

use std::fmt;

use crate::text;

/// A fully qualified Unity Catalog object name.
///
/// Parts keep the case they were written in; Unity Catalog compares names
/// case-insensitively, so compare with [`Fqn::matches`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fqn {
    pub(crate) catalog: String,
    pub(crate) schema: String,
    pub(crate) name: String,
}

impl Fqn {
    /// Parses `catalog.schema.name`. A part may be backtick-quoted, with a
    /// doubled backtick for a literal one, to contain `.` or other text; an
    /// unquoted part may not contain a backtick or whitespace. Every part
    /// must be non-empty.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut parts = Vec::with_capacity(3);
        let mut characters = text.chars().peekable();
        loop {
            let mut part = String::new();
            if characters.peek() == Some(&'`') {
                characters.next();
                loop {
                    match characters.next() {
                        Some('`') if characters.peek() == Some(&'`') => {
                            characters.next();
                            part.push('`');
                        }
                        Some('`') => break,
                        Some(character) => part.push(character),
                        None => return Err(format!("{text:?} has an unclosed backtick")),
                    }
                }
            } else {
                while let Some(&character) = characters.peek() {
                    if character == '.' {
                        break;
                    }
                    if character == '`' || character.is_whitespace() {
                        return Err(format!(
                            "{text:?} has {character:?} in an unquoted part; quote the part with backticks"
                        ));
                    }
                    part.push(character);
                    characters.next();
                }
            }
            if part.is_empty() {
                return Err(format!("{text:?} has an empty part"));
            }
            parts.push(part);
            match characters.next() {
                Some('.') => {}
                None => break,
                Some(character) => {
                    return Err(format!(
                        "{text:?} has {character:?} after a quoted part; expected `.`"
                    ));
                }
            }
        }
        let Ok([catalog, schema, name]) = <[String; 3]>::try_from(parts) else {
            return Err(format!("{text:?} is not a three-part catalog.schema.name"));
        };
        Ok(Self {
            catalog,
            schema,
            name,
        })
    }

    /// Whether `other` names the same object, comparing parts as Unity
    /// Catalog does: case-insensitively.
    pub(crate) fn matches(&self, other: &Self) -> bool {
        self.catalog.to_lowercase() == other.catalog.to_lowercase()
            && self.schema.to_lowercase() == other.schema.to_lowercase()
            && self.name.to_lowercase() == other.name.to_lowercase()
    }
}

/// Formats as SQL, with every part quoted, so the result can be placed in a
/// statement as is.
impl fmt::Display for Fqn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}.{}.{}",
            text::identifier(&self.catalog),
            text::identifier(&self.schema),
            text::identifier(&self.name)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Fqn;

    fn fqn(catalog: &str, schema: &str, name: &str) -> Fqn {
        Fqn {
            catalog: catalog.to_owned(),
            schema: schema.to_owned(),
            name: name.to_owned(),
        }
    }

    #[test]
    fn parses_plain_and_quoted_parts() {
        assert_eq!(
            Fqn::parse("main.default.t"),
            Ok(fqn("main", "default", "t"))
        );
        assert_eq!(
            Fqn::parse("`my catalog`.`a.b`.`x``y`"),
            Ok(fqn("my catalog", "a.b", "x`y"))
        );
        assert_eq!(Fqn::parse("Main.`s`.T"), Ok(fqn("Main", "s", "T")));
        assert_eq!(Fqn::parse("ünï.cödé.☃"), Ok(fqn("ünï", "cödé", "☃")));
    }

    #[test]
    fn rejects_malformed_names() {
        for (text, problem) in [
            ("main.default", "three-part"),
            ("a.b.c.d", "three-part"),
            ("a..c", "empty part"),
            (".b.c", "empty part"),
            ("a.b.", "empty part"),
            ("a.b.``", "empty part"),
            ("a.`b.c", "unclosed backtick"),
            ("a.b`x`.c", "unquoted part"),
            ("a.b c.d", "unquoted part"),
            ("a.`b`c.d", "after a quoted part"),
            ("", "empty part"),
        ] {
            let error = Fqn::parse(text).expect_err(text);
            assert!(error.contains(problem), "{text}: {error}");
        }
    }

    #[test]
    fn formats_quoted_sql_that_parses_back() {
        for name in [
            fqn("main", "default", "t"),
            fqn("my catalog", "a.b", "x`y"),
            fqn("select", "from", "where"),
        ] {
            let sql = name.to_string();
            assert_eq!(Fqn::parse(&sql), Ok(name), "{sql}");
        }
        assert_eq!(fqn("a", "b`", "c").to_string(), "`a`.`b```.`c`");
    }

    #[test]
    fn matches_case_insensitively() {
        assert!(fqn("Main", "Default", "T").matches(&fqn("main", "default", "t")));
        assert!(!fqn("main", "default", "t").matches(&fqn("main", "default", "u")));
    }
}

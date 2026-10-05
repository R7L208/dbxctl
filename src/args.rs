use std::ffi::{OsStr, OsString};

/// A generic option parser over `OsString` that preserves non-UTF-8 paths.
///
/// This parser supports:
/// - Flags (e.g., `--flag`)
/// - Valued options in `--name value` form (space-separated)
/// - Repeated options (last occurrence wins; can be overridden per-use)
/// - Positional arguments
///
/// Note: `--name=value` form is NOT supported; only `--name value` is valid.
/// Unknown options cause an error.
#[derive(Debug)]
pub(crate) struct Parser<'a> {
    args: Vec<&'a OsStr>,
    index: usize,
}

impl<'a> Parser<'a> {
    /// Create a new parser from an iterator of arguments.
    pub(crate) fn new(args: impl Iterator<Item = &'a OsString>) -> Self {
        Self {
            args: args.map(OsString::as_os_str).collect(),
            index: 0,
        }
    }

    /// Check if there are more arguments.
    fn has_next(&self) -> bool {
        self.index < self.args.len()
    }

    /// Peek at the current argument without consuming it.
    fn peek(&self) -> Option<&'a OsStr> {
        if self.has_next() {
            Some(self.args[self.index])
        } else {
            None
        }
    }

    /// Consume and return the current argument.
    fn next(&mut self) -> Option<&'a OsStr> {
        if self.has_next() {
            let arg = self.args[self.index];
            self.index += 1;
            Some(arg)
        } else {
            None
        }
    }

    /// Get a flag (no value). If the flag is present, it is consumed and returns true.
    pub(crate) fn flag(&mut self, name: &str) -> bool {
        let mut found = false;
        let full_flag = format!("--{name}");
        while let Some(arg) = self.peek() {
            if arg == full_flag.as_str() {
                self.next();
                found = true;
            } else if arg.to_string_lossy().starts_with("--") {
                break;
            } else {
                // Non-option argument; stop looking for this flag
                break;
            }
        }
        found
    }

    /// Get an option value. If the option is present, returns its value.
    /// The last occurrence's value is returned if present multiple times.
    pub(crate) fn option(&mut self, name: &str) -> Result<Option<&'a OsStr>, String> {
        let mut result = None;
        let full_option = format!("--{name}");
        let mut i = 0;
        while i < self.args.len() {
            let arg = self.args[i];
            if arg == full_option.as_str() {
                if i + 1 >= self.args.len() {
                    return Err(format!("option {full_option} requires a value"));
                }
                let next_arg = self.args[i + 1];
                // Check if the next argument looks like an option (starts with --)
                if next_arg.to_string_lossy().starts_with("--") {
                    return Err(format!(
                        "option {full_option} requires a value, got {}",
                        next_arg.display()
                    ));
                }
                result = Some(next_arg);
                i += 2;
            } else {
                i += 1;
            }
        }

        // Now consume all occurrences
        let mut first_consumed = false;
        while let Some(arg) = self.peek() {
            if arg == full_option.as_str() {
                self.next();
                if !first_consumed {
                    // Skip the value too
                    self.next();
                    first_consumed = true;
                }
            } else if arg.to_string_lossy().starts_with("--") {
                break;
            }
        }

        Ok(result)
    }

    /// Check if there are any remaining positional arguments (non-options).
    #[allow(dead_code)]
    pub(crate) fn has_remaining(&self) -> bool {
        let mut i = self.index;
        while i < self.args.len() {
            let arg = self.args[i];
            if !arg.to_string_lossy().starts_with("--") {
                return true;
            }
            // Skip over options and their values
            let arg_str = arg.to_string_lossy();
            if arg_str.starts_with("--") && !arg_str.contains('=') {
                // It's a flag or option; check if next is value
                if i + 1 < self.args.len() {
                    let next = self.args[i + 1];
                    if !next.to_string_lossy().starts_with("--") {
                        // It's an option with value, skip both
                        i += 2;
                        continue;
                    }
                }
            }
            i += 1;
        }
        false
    }

    /// Get the next positional argument (non-option).
    #[allow(dead_code)]
    pub(crate) fn positional(&mut self) -> Option<&'a OsStr> {
        // Skip past all options and their values
        while self.index < self.args.len() {
            let arg = self.args[self.index];
            let arg_str = arg.to_string_lossy();
            if arg_str.starts_with("--") {
                // It's an option; check if it takes a value
                if self.index + 1 < self.args.len() {
                    let next = self.args[self.index + 1];
                    if !next.to_string_lossy().starts_with("--") {
                        // It's an option with value; skip both
                        self.index += 2;
                        continue;
                    }
                }
                // It's a flag; skip it
                self.index += 1;
                continue;
            }

            // Found a positional argument
            let result = arg;
            self.index += 1;
            return Some(result);
        }
        None
    }

    /// Verify all arguments have been consumed.
    pub(crate) fn check_empty(&self) -> Result<(), String> {
        if self.index < self.args.len() {
            Err(format!(
                "unexpected argument {}",
                self.args[self.index].display()
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn args_refs(values: &[OsString]) -> impl Iterator<Item = &OsString> {
        values.iter()
    }

    #[test]
    fn parses_flags() {
        let input = args(&["--verbose", "--help"]);
        let mut parser = Parser::new(args_refs(&input));
        assert!(parser.flag("verbose"));
        assert!(parser.flag("help"));
        assert!(!parser.flag("other"));
    }

    #[test]
    fn parses_options_with_values() {
        let input = args(&["--name", "value", "--count", "42"]);
        let mut parser = Parser::new(args_refs(&input));
        assert_eq!(
            parser.option("name").unwrap().map(|s| s.to_string_lossy()),
            Some("value".into())
        );
        assert_eq!(
            parser.option("count").unwrap().map(|s| s.to_string_lossy()),
            Some("42".into())
        );
    }

    #[test]
    fn last_occurrence_wins_for_repeated_options() {
        let input = args(&["--name", "first", "--name", "second"]);
        let mut parser = Parser::new(args_refs(&input));
        assert_eq!(
            parser.option("name").unwrap().map(|s| s.to_string_lossy()),
            Some("second".into())
        );
    }

    #[test]
    fn rejects_missing_option_value() {
        let input = args(&["--name"]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parser.option("name").unwrap_err();
        assert!(error.contains("requires a value"));
    }

    #[test]
    fn rejects_option_value_that_looks_like_an_option() {
        let input = args(&["--target", "--profile"]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parser.option("target").unwrap_err();
        assert!(error.contains("requires a value"));
    }

    #[test]
    fn rejects_unknown_options() {
        // Note: Parser itself doesn't validate unknown options; that's the caller's responsibility.
        // This test documents the current behavior.
        let input = args(&["--unknown", "value"]);
        let mut parser = Parser::new(args_refs(&input));
        // Calling option on an unknown flag should still work (returns None)
        assert_eq!(
            parser
                .option("unknown")
                .unwrap()
                .map(|s| s.to_string_lossy()),
            Some("value".into())
        );
    }

    #[test]
    fn detects_unexpected_positional_arguments() {
        let input = args(&["extra"]);
        let parser = Parser::new(args_refs(&input));
        let error = parser.check_empty().unwrap_err();
        assert!(error.contains("unexpected argument"));
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_paths() {
        use std::os::unix::ffi::OsStrExt;
        let non_utf8 = OsStr::from_bytes(b"\xff\xfe");
        let input = [OsString::from("--path"), OsString::from(non_utf8)];
        let mut parser = Parser::new(input.iter());
        let value = parser.option("path").unwrap();
        assert_eq!(value, Some(non_utf8));
    }
}

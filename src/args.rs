use std::collections::HashSet;
use std::ffi::{OsStr, OsString};

/// A generic option parser over `OsString` that preserves non-UTF-8 paths.
///
/// This parser supports:
/// - Flags (e.g., `--flag`)
/// - Valued options in `--name value` form (space-separated)
/// - Options appearing in any order and interspersed
/// - Repeated options (last occurrence wins)
/// - Help flag (`--help` or `-h`) takes precedence and is recognized anywhere
///
/// Parsing rules:
/// - Format: `--name value` or `-h`, `--help` only (no `--name=value`)
/// - `--name=value` form is rejected with a clear error
/// - Options may appear in any order
/// - Repeated options: last occurrence wins
/// - Help flag anywhere exits help immediately
/// - All non-option arguments at the end are checked; extras cause an error
#[derive(Debug)]
pub(crate) struct Parser<'a> {
    args: Vec<&'a OsStr>,
    consumed: HashSet<usize>, // Track which indices have been consumed
}

impl<'a> Parser<'a> {
    /// Create a new parser from an iterator of arguments.
    pub(crate) fn new(args: impl Iterator<Item = &'a OsString>) -> Self {
        Self {
            args: args.map(OsString::as_os_str).collect(),
            consumed: HashSet::new(),
        }
    }

    /// Check if help flag is present anywhere in the arguments.
    /// Help takes precedence and is recognized at any position.
    pub(crate) fn help_requested(&self) -> bool {
        self.args
            .iter()
            .any(|arg| arg.to_string_lossy() == "--help" || arg.to_string_lossy() == "-h")
    }

    /// Get a flag (no value). If the flag is present, it is consumed and returns true.
    /// Flags may appear anywhere in the argument list.
    pub(crate) fn flag(&mut self, name: &str) -> bool {
        let full_flag = format!("--{name}");
        for (i, arg) in self.args.iter().enumerate() {
            if self.consumed.contains(&i) {
                continue;
            }
            if *arg == full_flag.as_str() {
                self.consumed.insert(i);
                return true;
            }
        }
        false
    }

    /// Get an option value. If the option is present, returns its value.
    /// The last occurrence's value is returned if present multiple times.
    /// Options may appear anywhere in the argument list.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn option(&mut self, name: &str) -> Result<Option<&'a OsStr>, String> {
        let full_option = format!("--{name}");
        let mut last_value: Option<(&'a OsStr, usize)> = None;
        let mut option_indices = Vec::new();

        // Find all occurrences of the option
        for (i, arg) in self.args.iter().enumerate() {
            if self.consumed.contains(&i) {
                continue;
            }

            // Check for --name=value form and reject it
            let arg_str = arg.to_string_lossy();
            if arg_str.starts_with("--") && arg_str.contains('=') {
                return Err(format!(
                    "option format --{name}=value is not supported; use --{name} value instead"
                ));
            }

            if *arg == full_option.as_str() {
                option_indices.push(i);
                // Check if there's a next argument that's the value
                if i + 1 < self.args.len() && !self.consumed.contains(&(i + 1)) {
                    let next_arg = self.args[i + 1];
                    let next_str = next_arg.to_string_lossy();

                    // Check if the next argument looks like an option (starts with --)
                    if next_str.starts_with("--") || next_str.starts_with('-') {
                        return Err(format!(
                            "option {} requires a value, got {}",
                            full_option,
                            next_arg.display()
                        ));
                    }

                    // Check for empty value
                    if next_str.is_empty() {
                        return Err(format!("option {full_option} requires a non-empty value"));
                    }

                    last_value = Some((next_arg, i + 1));
                } else if i + 1 >= self.args.len() || self.consumed.contains(&(i + 1)) {
                    return Err(format!("option {full_option} requires a value"));
                }
            }
        }

        // If we found the option, mark it and its value as consumed
        if let Some((value, value_idx)) = last_value {
            // Mark all occurrences of the option as consumed
            for idx in option_indices {
                self.consumed.insert(idx);
            }
            // Mark the value index as consumed
            self.consumed.insert(value_idx);
            Ok(Some(value))
        } else if !option_indices.is_empty() {
            // Option was found but we already returned an error above
            Ok(None)
        } else {
            Ok(None)
        }
    }

    /// Verify all arguments have been consumed.
    pub(crate) fn check_empty(&self) -> Result<(), String> {
        for (i, arg) in self.args.iter().enumerate() {
            if !self.consumed.contains(&i) {
                return Err(format!("unexpected argument {}", arg.display()));
            }
        }
        Ok(())
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
    fn parses_flags_in_any_order() {
        let input = args(&["--suite", "lineage", "--verbose", "--target", "dev"]);
        let mut parser = Parser::new(args_refs(&input));
        assert!(parser.flag("verbose"));
        parser.check_empty().ok(); // Reset for next test
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
    fn parses_options_in_any_order() {
        let input = args(&[
            "--target",
            "dev",
            "--bundle-root",
            "/tmp",
            "--suite",
            "lineage",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        // Should parse in any order
        assert_eq!(
            parser.option("suite").unwrap().map(|s| s.to_string_lossy()),
            Some("lineage".into())
        );
        assert_eq!(
            parser
                .option("bundle-root")
                .unwrap()
                .map(|s| s.to_string_lossy()),
            Some("/tmp".into())
        );
        assert_eq!(
            parser
                .option("target")
                .unwrap()
                .map(|s| s.to_string_lossy()),
            Some("dev".into())
        );
        parser.check_empty().unwrap();
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
    fn rejects_empty_option_value() {
        let input = args(&["--name", ""]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parser.option("name").unwrap_err();
        assert!(error.contains("non-empty value"));
    }

    #[test]
    fn rejects_option_value_that_looks_like_an_option() {
        let input = args(&["--target", "--profile"]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parser.option("target").unwrap_err();
        assert!(error.contains("requires a value"));
    }

    #[test]
    fn detects_unexpected_positional_arguments() {
        let input = args(&["extra"]);
        let parser = Parser::new(args_refs(&input));
        let error = parser.check_empty().unwrap_err();
        assert!(error.contains("unexpected argument"));
    }

    #[test]
    fn rejects_equals_form() {
        let input = args(&["--name=value"]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parser.option("name").unwrap_err();
        assert!(error.contains("not supported"));
        assert!(error.contains("--name value"));
    }

    #[test]
    fn detects_help_flag() {
        let input = args(&["--suite", "lineage", "--help"]);
        let parser = Parser::new(args_refs(&input));
        assert!(parser.help_requested());
    }

    #[test]
    fn detects_help_short_flag() {
        let input = args(&["--suite", "lineage", "-h"]);
        let parser = Parser::new(args_refs(&input));
        assert!(parser.help_requested());
    }

    #[test]
    fn help_not_requested_when_absent() {
        let input = args(&["--suite", "lineage"]);
        let parser = Parser::new(args_refs(&input));
        assert!(!parser.help_requested());
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

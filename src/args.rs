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
    /// `consumed[i]` is true once argument `i` has been matched.
    consumed: Vec<bool>,
}

impl<'a> Parser<'a> {
    /// Create a new parser from an iterator of arguments.
    pub(crate) fn new(args: impl Iterator<Item = &'a OsString>) -> Self {
        let args: Vec<_> = args.map(OsString::as_os_str).collect();
        let consumed = vec![false; args.len()];
        Self { args, consumed }
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
        let flag = format!("--{name}");
        let mut found = false;
        for (arg, consumed) in self.args.iter().zip(self.consumed.iter_mut()) {
            if !*consumed && *arg == flag.as_str() {
                *consumed = true;
                found = true;
            }
        }
        found
    }

    /// Get an option value. Every occurrence of `--name value` is consumed and
    /// the last value wins. Options may appear anywhere in the argument list.
    pub(crate) fn option(&mut self, name: &str) -> Result<Option<&'a OsStr>, String> {
        let option = format!("--{name}");
        let equals_form = format!("--{name}=");
        let mut value = None;
        let mut index = 0;
        while index < self.args.len() {
            if !self.consumed[index] && self.args[index].to_string_lossy().starts_with(&equals_form)
            {
                return Err(format!(
                    "option format {option}=value is not supported; use {option} value instead"
                ));
            }
            if self.consumed[index] || self.args[index] != option.as_str() {
                index += 1;
                continue;
            }
            let next = index + 1;
            let Some(candidate) = self.args.get(next).filter(|_| !self.consumed[next]) else {
                return Err(format!("option {option} requires a value"));
            };
            let text = candidate.to_string_lossy();
            if text.is_empty() {
                return Err(format!("option {option} requires a non-empty value"));
            }
            if text.starts_with('-') {
                return Err(format!(
                    "option {option} requires a value, got {}",
                    candidate.display()
                ));
            }
            self.consumed[index] = true;
            self.consumed[next] = true;
            value = Some(*candidate);
            index = next + 1;
        }
        Ok(value)
    }

    /// Verify all arguments have been consumed.
    pub(crate) fn check_empty(&self) -> Result<(), String> {
        let leftover = self
            .args
            .iter()
            .zip(&self.consumed)
            .find(|(_, consumed)| !**consumed);
        let Some((arg, _)) = leftover else {
            return Ok(());
        };
        let text = arg.to_string_lossy();
        if let Some((name, _)) = text
            .strip_prefix("--")
            .and_then(|rest| rest.split_once('='))
        {
            return Err(format!(
                "option format --{name}=value is not supported; use --{name} value instead"
            ));
        }
        Err(format!("unexpected argument {}", arg.display()))
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
        assert_eq!(
            parser
                .option("target")
                .unwrap()
                .map(|s| s.to_string_lossy()),
            Some("dev".into())
        );
        assert!(parser.flag("verbose"));
        assert_eq!(
            parser.option("suite").unwrap().map(|s| s.to_string_lossy()),
            Some("lineage".into())
        );
        parser.check_empty().unwrap();
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
        let input = args(&["--name", "first", "--other", "x", "--name", "second"]);
        let mut parser = Parser::new(args_refs(&input));
        assert_eq!(
            parser.option("name").unwrap().map(|s| s.to_string_lossy()),
            Some("second".into())
        );
        assert_eq!(
            parser.option("other").unwrap().map(|s| s.to_string_lossy()),
            Some("x".into())
        );
        // Earlier occurrences and their values are consumed too.
        parser.check_empty().unwrap();
    }

    #[test]
    fn repeated_flags_are_all_consumed() {
        let input = args(&["--verbose", "--verbose"]);
        let mut parser = Parser::new(args_refs(&input));
        assert!(parser.flag("verbose"));
        parser.check_empty().unwrap();
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
        let input = args(&["--target=dev"]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parser.option("target").unwrap_err();
        assert!(error.contains("--target=value is not supported"), "{error}");

        // An `=` form of an option nobody asked for is reported by check_empty,
        // naming the option that was actually given.
        let input = args(&["--suite", "lineage", "--name=value"]);
        let mut parser = Parser::new(args_refs(&input));
        assert!(parser.option("suite").unwrap().is_some());
        let error = parser.check_empty().unwrap_err();
        assert!(error.contains("--name=value is not supported"), "{error}");
        assert!(error.contains("--name value"), "{error}");
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

//! Fake-CLI scenario replay, shared by the integration tests and, through
//! `#[path]`, by the crate's unit tests.
//!
//! [`ScenarioCli`] links the fake Databricks CLI into its own directory next to
//! a scenario file. The fake answers each argv from that scenario and appends
//! it to an invocation log (see `tests/fixtures/fake_databricks.rs` for the
//! format), so a test can assert exactly which commands ran.

use std::fs;
use std::path::{Path, PathBuf};

const SCENARIO_FILE: &str = "fake-scenario.txt";
const INVOCATION_LOG: &str = "fake-invocations.log";

/// Command paths known to be read-only. Each later check that adds a CLI
/// surface extends this list; anything not listed counts as a mutation.
const READ_ONLY_COMMANDS: &[&[&str]] = &[
    &["version"],
    &["bundle", "validate"],
    &["bundle", "plan"],
    &["bundle", "schema"],
    &["api", "get"],
];

/// A fake Databricks CLI that replays one scenario.
pub struct ScenarioCli {
    binary: PathBuf,
    log: PathBuf,
}

impl ScenarioCli {
    /// Sets up `directory`, which must exist and be private to the test, with
    /// a link to `fake` and the `scenario` text.
    pub fn new(fake: &Path, directory: &Path, scenario: &str) -> Self {
        let binary = directory.join(format!("databricks{}", std::env::consts::EXE_SUFFIX));
        // On Unix, a symbolic link avoids writing a fresh executable, which
        // another thread's fork could inherit open and make busy (`ETXTBSY`
        // on Linux). Hard links are not used: macOS intermittently kills
        // processes started through them with `SIGKILL`.
        #[cfg(unix)]
        std::os::unix::fs::symlink(fake, &binary).expect("link fake Databricks CLI");
        #[cfg(not(unix))]
        fs::copy(fake, &binary).expect("copy fake Databricks CLI");
        fs::write(directory.join(SCENARIO_FILE), scenario).expect("write scenario");
        Self {
            binary,
            log: directory.join(INVOCATION_LOG),
        }
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Every argv the fake received, in order.
    pub fn invocations(&self) -> Vec<Vec<String>> {
        match fs::read_to_string(&self.log) {
            Ok(log) => parse_invocation_log(&log),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("read invocation log: {error}"),
        }
    }
}

/// Parses the fake's invocation log: one line per invocation, each argv
/// element preceded by a tab and escaped.
pub fn parse_invocation_log(log: &str) -> Vec<Vec<String>> {
    log.lines()
        .map(|line| line.split('\t').skip(1).map(unescape).collect())
        .collect()
}

fn unescape(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            text.push(character);
            continue;
        }
        match characters.next() {
            Some('\\') => text.push('\\'),
            Some('n') => text.push('\n'),
            Some('r') => text.push('\r'),
            Some('t') => text.push('\t'),
            other => panic!("unsupported escape \\{other:?} in invocation log"),
        }
    }
    text
}

/// Fails unless every invocation is `--version`, asks for help, or is a known
/// read-only command. The command path is the leading run of arguments that
/// are not flags, so an argv that starts with any other flag fails: its
/// command cannot be identified without knowing which flags take values.
pub fn assert_no_mutations(invocations: &[Vec<String>]) {
    for argv in invocations {
        if argv == &["--version"]
            || argv
                .iter()
                .any(|argument| argument == "--help" || argument == "-h")
        {
            continue;
        }
        let path: Vec<&str> = argv
            .iter()
            .map(String::as_str)
            .take_while(|argument| !argument.starts_with('-'))
            .collect();
        assert!(
            READ_ONLY_COMMANDS.contains(&path.as_slice()),
            "invocation {argv:?} is not a known read-only command"
        );
    }
}

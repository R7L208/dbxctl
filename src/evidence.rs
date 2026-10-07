//! Probe run directories and raw evidence (#30).
//!
//! A run lives at `<bundle-root>/.dbxctl/probe/<suite>/<run-id>/`. Before
//! anything is written below `.dbxctl/`, that directory gets a `.gitignore`
//! containing `*`, so raw evidence cannot be committed by accident.
//!
//! The run ID is the run's UTC start time (`20260102T030405Z`), with a `-2`,
//! `-3`, ... suffix when an earlier run already used that second. Time comes
//! from an injected [`Clock`]; with a fixed clock and the same inputs, every
//! written file is byte-identical.
//!
//! This module names checks only by their string IDs, so it stays independent
//! of the probe module that defines them.

use std::ffi::OsString;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::databricks::Captured;
use crate::json::{Document, Value};

const STATE_DIRECTORY: &str = ".dbxctl";
const GITIGNORE: &[u8] = b"*\n";
const EVIDENCE_DIRECTORY: &str = "evidence";
// More runs than this in one second indicates a stuck loop, not real use.
const MAX_RUNS_PER_SECOND: u32 = 1000;

/// A source of the current time, injected so tests are deterministic.
pub(crate) trait Clock {
    /// Whole seconds since the Unix epoch, in UTC.
    fn now(&self) -> u64;
}

/// The operating-system clock.
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        // A clock set before 1970 is reported as the epoch rather than failing.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs())
    }
}

/// A UTC calendar time with one-second resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UtcTime {
    year: u64,
    month: u64,
    day: u64,
    hour: u64,
    minute: u64,
    second: u64,
}

impl UtcTime {
    fn from_unix(seconds: u64) -> Self {
        // Howard Hinnant's `civil_from_days`, restricted to dates after 1970
        // so that every intermediate value is non-negative.
        let days = seconds / 86_400 + 719_468;
        let era = days / 146_097;
        let day_of_era = days % 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        let year = year_of_era + era * 400 + u64::from(month <= 2);
        let time_of_day = seconds % 86_400;
        Self {
            year,
            month,
            day,
            hour: time_of_day / 3600,
            minute: time_of_day % 3600 / 60,
            second: time_of_day % 60,
        }
    }

    /// RFC 3339, for example `2026-01-02T03:04:05Z`.
    fn rfc3339(self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// The basic ISO 8601 form used in run IDs, for example `20260102T030405Z`.
    fn compact(self) -> String {
        format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Formats Unix seconds as an RFC 3339 UTC timestamp.
pub(crate) fn timestamp(seconds: u64) -> String {
    UtcTime::from_unix(seconds).rfc3339()
}

/// A created run directory.
#[derive(Debug)]
pub(crate) struct RunDir {
    path: PathBuf,
    run_id: String,
}

impl RunDir {
    /// Creates a new run directory for `suite` below `bundle_root`, which must
    /// already exist, naming it from `started` (Unix seconds). Writes
    /// `.dbxctl/.gitignore` first if it is missing; an existing one is left as
    /// it is.
    pub(crate) fn create(bundle_root: &Path, suite: &str, started: u64) -> Result<Self, String> {
        check_name(suite)?;
        if !bundle_root.is_dir() {
            return Err(format!(
                "bundle root {} is not a directory",
                bundle_root.display()
            ));
        }
        let state = bundle_root.join(STATE_DIRECTORY);
        create_dir_all(&state)?;
        write_gitignore(&state.join(".gitignore"))?;
        let suite_root = state.join("probe").join(suite);
        create_dir_all(&suite_root)?;

        let base = UtcTime::from_unix(started).compact();
        for attempt in 1..=MAX_RUNS_PER_SECOND {
            let run_id = if attempt == 1 {
                base.clone()
            } else {
                format!("{base}-{attempt}")
            };
            let path = suite_root.join(&run_id);
            match private_dir_builder().create(&path) {
                Ok(()) => return Ok(Self { path, run_id }),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!(
                        "could not create run directory {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        Err(format!(
            "could not create a run directory in {}: {MAX_RUNS_PER_SECOND} runs already started at {base}",
            suite_root.display()
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Writes a top-level run file such as `run.json`.
    pub(crate) fn write_file(&self, name: &str, contents: &[u8]) -> Result<(), String> {
        check_name(name)?;
        write(&self.path.join(name), contents)
    }

    /// Writes a JSON document as a top-level run file.
    pub(crate) fn write_json(&self, name: &str, document: &Document) -> Result<(), String> {
        let text = document
            .to_pretty_string()
            .map_err(|error| format!("could not serialize {name}: {error}"))?;
        self.write_file(name, text.as_bytes())
    }

    /// Writes raw evidence for `check` and returns its run-relative reference,
    /// `evidence/<check>/<name>`.
    pub(crate) fn write_evidence(
        &self,
        check: &str,
        name: &str,
        contents: &[u8],
    ) -> Result<String, String> {
        check_name(check)?;
        check_name(name)?;
        let directory = self.path.join(EVIDENCE_DIRECTORY).join(check);
        create_dir_all(&directory)?;
        write(&directory.join(name), contents)?;
        Ok(format!("{EVIDENCE_DIRECTORY}/{check}/{name}"))
    }

    /// Records one Databricks CLI invocation as `<step>.json`, plus
    /// `<step>.stdout` and `<step>.stderr` when the process ran. Returns the
    /// reference to the JSON record.
    ///
    /// The record names the program `databricks` rather than the resolved
    /// path, so evidence does not depend on where the CLI is installed.
    pub(crate) fn record_invocation(
        &self,
        check: &str,
        step: &str,
        args: &[OsString],
        result: &Result<Captured, String>,
    ) -> Result<String, String> {
        let elements = args
            .iter()
            .map(|argument| Value::string(argument.to_string_lossy()))
            .collect();
        let mut record = vec![
            ("program", Value::string("databricks")),
            ("argv", Value::Array(elements)),
        ];
        match result {
            Ok(captured) => {
                let stdout_name = format!("{step}.stdout");
                let stderr_name = format!("{step}.stderr");
                self.write_evidence(check, &stdout_name, &captured.stdout)?;
                self.write_evidence(check, &stderr_name, &captured.stderr)?;
                let (code, signal) = status_parts(captured.status);
                record.extend([
                    ("error", Value::Null),
                    ("exit_code", code.map_or(Value::Null, Value::Integer)),
                    ("signal", signal.map_or(Value::Null, Value::Integer)),
                    ("stdout", Value::string(stdout_name)),
                    ("stderr", Value::string(stderr_name)),
                ]);
            }
            Err(problem) => record.extend([
                ("error", Value::string(problem.as_str())),
                ("exit_code", Value::Null),
                ("signal", Value::Null),
                ("stdout", Value::Null),
                ("stderr", Value::Null),
            ]),
        }
        let text = Document::from(Value::object(record))
            .to_pretty_string()
            .map_err(|error| format!("could not serialize {step} evidence: {error}"))?;
        self.write_evidence(check, &format!("{step}.json"), text.as_bytes())
    }
}

fn status_parts(status: ExitStatus) -> (Option<i64>, Option<i64>) {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(i64::from)
    };
    #[cfg(not(unix))]
    let signal = None;
    (status.code().map(i64::from), signal)
}

/// Accepts only names generated by dbxctl: lowercase ASCII letters, digits,
/// `.`, `_`, and `-`, not starting with `.`. This keeps every write inside the
/// run directory.
fn check_name(name: &str) -> Result<(), String> {
    let valid = name
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(format!("invalid evidence name {name:?}"))
    }
}

fn write_gitignore(path: &Path) -> Result<(), String> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file
            .write_all(GITIGNORE)
            .map_err(|error| format!("could not write {}: {error}", path.display())),
        Err(error) if error.kind() == ErrorKind::AlreadyExists && path.is_file() => Ok(()),
        Err(error) => Err(format!("could not create {}: {error}", path.display())),
    }
}

/// A builder for directories that may hold raw evidence. On Unix they are
/// private to the user, because later checks capture debug output.
fn private_dir_builder() -> DirBuilder {
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}

fn create_dir_all(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("could not create directory {}: {error}", path.display()))
}

fn write(path: &Path, contents: &[u8]) -> Result<(), String> {
    fs::write(path, contents)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::ffi::OsString;
    use std::fs;

    use super::{Clock, RunDir, SystemClock, UtcTime, check_name, timestamp};
    use crate::databricks::run_captured;
    use crate::test_support::{TempDir, fake_databricks};

    /// A clock that always reports the same instant.
    pub(crate) struct FixedClock(pub(crate) u64);

    impl Clock for FixedClock {
        fn now(&self) -> u64 {
            self.0
        }
    }

    // 2026-01-02T03:04:05Z.
    pub(crate) const FIXED_TIME: u64 = 1_767_323_045;

    #[test]
    fn formats_utc_calendar_times() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(timestamp(FIXED_TIME), "2026-01-02T03:04:05Z");
        // Leap day, end of a leap year, and the turn of a century.
        assert_eq!(timestamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(timestamp(1_735_689_599), "2024-12-31T23:59:59Z");
        assert_eq!(timestamp(4_107_542_400), "2100-03-01T00:00:00Z");
        assert_eq!(UtcTime::from_unix(FIXED_TIME).compact(), "20260102T030405Z");
    }

    #[test]
    fn system_clock_is_after_the_fixture_time() {
        assert!(SystemClock.now() > FIXED_TIME);
    }

    #[test]
    fn creates_the_run_directory_and_gitignore() {
        let temp = TempDir::new("evidence-create");
        let run = RunDir::create(temp.path(), "lineage", FIXED_TIME).expect("create");

        assert_eq!(run.run_id(), "20260102T030405Z");
        assert_eq!(
            run.path(),
            temp.path().join(".dbxctl/probe/lineage/20260102T030405Z")
        );
        assert!(run.path().is_dir());
        assert_eq!(
            fs::read(temp.path().join(".dbxctl/.gitignore")).expect("read gitignore"),
            b"*\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_directories_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new("evidence-private");
        let run = RunDir::create(temp.path(), "lineage", FIXED_TIME).expect("create");
        let mode = fs::metadata(run.path()).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn keeps_an_existing_gitignore() {
        let temp = TempDir::new("evidence-gitignore");
        fs::create_dir(temp.path().join(".dbxctl")).expect("create state dir");
        fs::write(temp.path().join(".dbxctl/.gitignore"), "custom\n").expect("write");
        RunDir::create(temp.path(), "lineage", FIXED_TIME).expect("create");
        assert_eq!(
            fs::read_to_string(temp.path().join(".dbxctl/.gitignore")).expect("read"),
            "custom\n"
        );
    }

    #[test]
    fn suffixes_run_ids_that_share_a_second() {
        let temp = TempDir::new("evidence-collide");
        let ids: Vec<_> = (0..3)
            .map(|_| {
                RunDir::create(temp.path(), "lineage", FIXED_TIME)
                    .expect("create")
                    .run_id()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            ids,
            [
                "20260102T030405Z",
                "20260102T030405Z-2",
                "20260102T030405Z-3"
            ]
        );
    }

    #[test]
    fn gives_up_after_too_many_runs_in_one_second() {
        // Every candidate name already exists as a plain file.
        let temp = TempDir::new("evidence-exhausted");
        let suite = temp.path().join(".dbxctl/probe/lineage");
        fs::create_dir_all(&suite).expect("create suite dir");
        fs::write(suite.join("20260102T030405Z"), "").expect("occupy");
        for attempt in 2..=super::MAX_RUNS_PER_SECOND {
            fs::write(suite.join(format!("20260102T030405Z-{attempt}")), "").expect("occupy");
        }
        let error =
            RunDir::create(temp.path(), "lineage", FIXED_TIME).expect_err("exhausted run IDs");
        assert!(error.contains("1000 runs already started"), "{error}");
    }

    #[test]
    fn rejects_a_missing_or_file_bundle_root() {
        let temp = TempDir::new("evidence-missing");
        let missing = temp.path().join("missing");
        let error = RunDir::create(&missing, "lineage", 0).expect_err("missing root");
        assert!(error.contains("is not a directory"), "{error}");
        assert!(
            !missing.exists(),
            "a missing bundle root must not be created"
        );

        let file = temp.path().join("file");
        fs::write(&file, "").expect("write file");
        let error = RunDir::create(&file, "lineage", 0).expect_err("file root");
        assert!(error.contains("is not a directory"), "{error}");
    }

    #[test]
    fn reports_unwritable_state_paths() {
        // A file where `.dbxctl` or the suite directory should be.
        let temp = TempDir::new("evidence-blocked-state");
        fs::write(temp.path().join(".dbxctl"), "").expect("block state dir");
        let error = RunDir::create(temp.path(), "lineage", 0).expect_err("blocked");
        assert!(error.contains("could not create directory"), "{error}");

        let temp = TempDir::new("evidence-blocked-gitignore");
        fs::create_dir_all(temp.path().join(".dbxctl/.gitignore")).expect("block gitignore");
        let error = RunDir::create(temp.path(), "lineage", 0).expect_err("blocked");
        assert!(error.contains("could not create"), "{error}");
    }

    #[test]
    fn rejects_names_that_could_escape_the_run_directory() {
        for name in ["", ".", "..", ".hidden", "a/b", "a\\b", "Upper", "sp ace"] {
            assert!(check_name(name).is_err(), "{name:?}");
        }
        for name in ["cli", "v10", "version.stdout", "a_b-c.json"] {
            assert!(check_name(name).is_ok(), "{name:?}");
        }

        let temp = TempDir::new("evidence-names");
        let error = RunDir::create(temp.path(), "../x", 0).expect_err("bad suite");
        assert!(error.contains("invalid evidence name"), "{error}");
        let run = RunDir::create(temp.path(), "lineage", 0).expect("create");
        assert!(run.write_file("../run.json", b"").is_err());
        assert!(run.write_evidence("cli", "../x", b"").is_err());
        assert!(run.write_evidence("../cli", "x", b"").is_err());
    }

    #[test]
    fn writes_evidence_and_run_files() {
        let temp = TempDir::new("evidence-write");
        let run = RunDir::create(temp.path(), "lineage", 0).expect("create");
        let reference = run
            .write_evidence("cli", "raw.txt", b"bytes")
            .expect("write evidence");
        assert_eq!(reference, "evidence/cli/raw.txt");
        assert_eq!(
            fs::read(run.path().join(&reference)).expect("read"),
            b"bytes"
        );

        run.write_file("notes.md", b"# x\n").expect("write file");
        assert_eq!(
            fs::read(run.path().join("notes.md")).expect("read"),
            b"# x\n"
        );

        // Writing into a path that is now a directory fails cleanly.
        fs::create_dir(run.path().join("run.json")).expect("block run.json");
        let error = run
            .write_json("run.json", &crate::json::parse("{}").expect("parse"))
            .expect_err("blocked run.json");
        assert!(error.contains("could not write"), "{error}");
    }

    #[test]
    fn records_captured_invocations() {
        let temp = TempDir::new("evidence-invocation");
        let run = RunDir::create(temp.path(), "lineage", 0).expect("create");
        let args = [OsString::from("jobs"), OsString::from("two words")];
        let captured = run_captured(
            fake_databricks().as_os_str(),
            &args,
            std::time::Duration::from_secs(5),
        );
        let reference = run
            .record_invocation("cli", "jobs", &args, &captured)
            .expect("record");
        assert_eq!(reference, "evidence/cli/jobs.json");
        assert_eq!(
            fs::read_to_string(run.path().join(&reference)).expect("read record"),
            "{\n  \"argv\": [\n    \"jobs\",\n    \"two words\"\n  ],\n  \"error\": null,\n  \"exit_code\": 0,\n  \"program\": \"databricks\",\n  \"signal\": null,\n  \"stderr\": \"jobs.stderr\",\n  \"stdout\": \"jobs.stdout\"\n}\n"
        );
        assert_eq!(
            fs::read(run.path().join("evidence/cli/jobs.stdout")).expect("read stdout"),
            b"<jobs>\n<two words>\n"
        );
        assert_eq!(
            fs::read(run.path().join("evidence/cli/jobs.stderr")).expect("read stderr"),
            b""
        );
    }

    #[test]
    fn records_failed_invocations() {
        let temp = TempDir::new("evidence-failed-invocation");
        let run = RunDir::create(temp.path(), "lineage", 0).expect("create");
        let reference = run
            .record_invocation(
                "cli",
                "version",
                &[OsString::from("--version")],
                &Err("could not execute databricks: not found".to_owned()),
            )
            .expect("record");
        let record = fs::read_to_string(run.path().join(reference)).expect("read record");
        assert!(
            record.contains("\"error\": \"could not execute databricks: not found\""),
            "{record}"
        );
        assert!(record.contains("\"stdout\": null"), "{record}");
        assert!(!run.path().join("evidence/cli/version.stdout").exists());
    }

    #[cfg(unix)]
    #[test]
    fn records_signal_termination() {
        use std::os::unix::process::ExitStatusExt;

        let temp = TempDir::new("evidence-signal");
        let run = RunDir::create(temp.path(), "lineage", 0).expect("create");
        let captured = crate::databricks::Captured {
            stdout: Vec::new(),
            stderr: Vec::new(),
            status: std::process::ExitStatus::from_raw(6),
        };
        let reference = run
            .record_invocation("cli", "abort", &[], &Ok(captured))
            .expect("record");
        let record = fs::read_to_string(run.path().join(reference)).expect("read record");
        assert!(record.contains("\"exit_code\": null"), "{record}");
        assert!(record.contains("\"signal\": 6"), "{record}");
    }
}

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const MINIMUM_DATABRICKS_VERSION: Version = Version::new(1, 13, 0);
// The newest release CI runs against: the `databricks_cli` version pinned in
// `.github/pins.json`. `scripts/discover-pin-versions.py` and its tests fail
// when the two differ. Newer versions are accepted with a warning.
const TESTED_DATABRICKS_VERSION: Version = Version::new(1, 13, 0);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
// How long to wait for the output pipes to close after the process is gone. A
// descendant that inherited them (for example Terraform under `databricks
// bundle`) can keep them open after the CLI exits or is killed.
const PIPE_CLOSE_GRACE: Duration = Duration::from_secs(1);
const VERSION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(crate) struct Captured {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) status: ExitStatus,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    pub(crate) const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    pub(crate) fn parse(output: &str) -> Option<Self> {
        let candidate = version_token(output)?.trim_start_matches('v');
        let mut parts = candidate.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch_segment = parts.next()?;
        let patch = patch_segment
            .split_once('-')
            .map_or(patch_segment, |(patch, _)| patch)
            .parse()
            .ok()?;
        Some(Self::new(major, minor, patch))
    }
}

fn version_token(output: &str) -> Option<&str> {
    let mut tokens = output.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "Databricks" && tokens.next() == Some("CLI") {
            return tokens.next();
        }
    }
    None
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

pub(crate) fn binary() -> PathBuf {
    resolve_binary(env::var_os("DATABRICKS_CLI_PATH"))
}

pub(crate) fn resolve_binary(explicit: Option<OsString>) -> PathBuf {
    explicit.map_or_else(|| PathBuf::from("databricks"), PathBuf::from)
}

pub(crate) fn run_captured(
    binary: &OsStr,
    args: &[OsString],
    timeout: Duration,
) -> Result<Captured, String> {
    let mut command = Command::new(binary);
    command.args(args);
    run_captured_command(&mut command, timeout)
}

fn run_captured_command(command: &mut Command, timeout: Duration) -> Result<Captured, String> {
    let display = command.get_program().display().to_string();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not execute {display}: {error}"))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        terminate(&mut child);
        format!("could not capture stdout from {display}")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        terminate(&mut child);
        format!("could not capture stderr from {display}")
    })?;

    // Drain both pipes concurrently. Waiting before reading can deadlock when
    // either pipe fills its operating-system buffer. The readers report through
    // a channel so that collecting their output can be bounded by a deadline.
    let (sender, output) = mpsc::channel();
    spawn_reader(Stream::Stdout, stdout, sender.clone());
    spawn_reader(Stream::Stderr, stderr, sender);
    let started = Instant::now();

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                terminate(&mut child);
                let captured = match collect(&output, Instant::now() + PIPE_CLOSE_GRACE) {
                    Ok((stdout, stderr)) => format!(
                        "captured {} stdout bytes and {} stderr bytes",
                        stdout.len(),
                        stderr.len()
                    ),
                    Err(problem) => problem,
                };
                return Err(format!(
                    "{display} timed out after {} ms ({captured})",
                    timeout.as_millis()
                ));
            }
            Ok(None) => thread::sleep(POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed()))),
            Err(error) => {
                terminate(&mut child);
                return Err(format!("could not wait for {display}: {error}"));
            }
        }
    };

    let deadline = (started + timeout).max(Instant::now()) + PIPE_CLOSE_GRACE;
    let (stdout, stderr) =
        collect(&output, deadline).map_err(|problem| format!("{display} exited, but {problem}"))?;
    Ok(Captured {
        stdout,
        stderr,
        status,
    })
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

type ReaderResult = (Stream, std::io::Result<Vec<u8>>);

fn spawn_reader(
    stream: Stream,
    mut pipe: impl Read + Send + 'static,
    sender: mpsc::Sender<ReaderResult>,
) {
    // The handle is dropped: a reader blocked on a pipe held open by a
    // descendant is abandoned at the deadline and ends when that pipe closes.
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send((stream, result));
    });
}

fn collect(
    output: &Receiver<ReaderResult>,
    deadline: Instant,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut stdout = None;
    let mut stderr = None;
    while stdout.is_none() || stderr.is_none() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (stream, result) = match output.recv_timeout(remaining) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => {
                return Err(format!(
                    "its output pipes stayed open for more than {} ms; a child process may still be holding them",
                    PIPE_CLOSE_GRACE.as_millis()
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err("an output capture thread stopped unexpectedly".to_owned());
            }
        };
        let (name, slot) = match stream {
            Stream::Stdout => ("stdout", &mut stdout),
            Stream::Stderr => ("stderr", &mut stderr),
        };
        *slot = Some(result.map_err(|error| format!("could not read captured {name}: {error}"))?);
    }
    Ok((stdout.unwrap_or_default(), stderr.unwrap_or_default()))
}

fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn installed_version(binary: &OsStr, timeout: Duration) -> Result<Version, String> {
    let Captured {
        stdout,
        stderr,
        status,
    } = run_captured(binary, &[OsString::from("version")], timeout)?;

    if !status.success() {
        return Err(format!(
            "{} version exited with {}",
            binary.display(),
            status
        ));
    }

    drop(stderr);
    let stdout = String::from_utf8(stdout)
        .map_err(|_| format!("{} version returned non-UTF-8 output", binary.display()))?;
    Version::parse(&stdout)
        .ok_or_else(|| format!("could not parse Databricks CLI version from {stdout:?}"))
}

/// A Databricks CLI version that meets the minimum, and whether it is within
/// the tested range (from the minimum through the tested version).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SupportedVersion {
    pub(crate) version: Version,
    pub(crate) tested: bool,
}

impl SupportedVersion {
    /// Rejects versions below the minimum. Newer-than-tested versions are
    /// supported but untested: callers warn and carry on.
    pub(crate) fn classify(version: Version) -> Result<Self, String> {
        if version < MINIMUM_DATABRICKS_VERSION {
            return Err(format!(
                "Databricks CLI {version} is unsupported; install {MINIMUM_DATABRICKS_VERSION} or newer"
            ));
        }
        Ok(Self {
            version,
            tested: version <= TESTED_DATABRICKS_VERSION,
        })
    }

    /// The warning for an untested version, without a `warning: ` prefix.
    pub(crate) fn untested_warning(self) -> Option<String> {
        (!self.tested).then(|| {
            format!(
                "Databricks CLI {} is newer than the tested version {TESTED_DATABRICKS_VERSION}; output shapes may differ",
                self.version
            )
        })
    }
}

fn validate(binary: &OsStr, timeout: Duration) -> Result<SupportedVersion, String> {
    SupportedVersion::classify(installed_version(binary, timeout)?)
}

pub(crate) fn run_doctor(binary: &OsStr) -> ExitCode {
    println!("dbxctl {}", env!("CARGO_PKG_VERSION"));
    match validate(binary, VERSION_TIMEOUT) {
        Ok(supported) => {
            println!(
                "Databricks CLI {} ({})",
                supported.version,
                binary.display()
            );
            // stderr keeps doctor's stdout identical for tested and untested
            // versions; an untested version never changes the exit status.
            if let Some(warning) = supported.untested_warning() {
                eprintln!("warning: {warning}");
            }
            ExitCode::SUCCESS
        }
        Err(problem) => {
            println!("Databricks CLI ({}): {problem}", binary.display());
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn run_passthrough(binary: &OsStr, args: Vec<OsString>) -> Result<ExitCode, String> {
    let status = Command::new(binary)
        .args(args)
        .status()
        .map_err(|error| format!("failed to run Databricks CLI: {error}"))?;
    std::process::exit(passthrough_exit_code(status));
}

#[cfg(unix)]
fn passthrough_exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;

    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

#[cfg(not(unix))]
fn passthrough_exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use super::{
        MINIMUM_DATABRICKS_VERSION, SupportedVersion, TESTED_DATABRICKS_VERSION, Version,
        run_captured, run_captured_command, validate,
    };

    // Generous bound for "returned promptly" on slow CI runners; every case
    // below would otherwise take at least five seconds.
    const PROMPT: Duration = Duration::from_secs(3);
    // The held-pipe cases use a one-second timeout so the fixture has time to
    // start its descendant, then wait one more second for the pipes to close.
    // Without the bound they would take the descendant's full ten seconds.
    const HELD_PIPES_BOUND: Duration = Duration::from_secs(5);

    #[test]
    fn minimum_version_is_1_13_0() {
        assert_eq!(MINIMUM_DATABRICKS_VERSION, Version::new(1, 13, 0));
        assert!(Version::new(1, 12, 99) < MINIMUM_DATABRICKS_VERSION);
        assert!(Version::new(0, 296, 0) < MINIMUM_DATABRICKS_VERSION);
        assert!(Version::new(1, 13, 0) >= MINIMUM_DATABRICKS_VERSION);
        assert!(Version::new(1, 19, 0) >= MINIMUM_DATABRICKS_VERSION);
    }

    #[test]
    fn tested_version_is_the_minimum() {
        // CI pins exactly the minimum, so the tested range is one release. If
        // the pin is raised past the minimum, assert `>=` here instead.
        assert_eq!(TESTED_DATABRICKS_VERSION, MINIMUM_DATABRICKS_VERSION);
    }

    #[test]
    fn classifies_versions_against_the_minimum_and_tested_range() {
        let error = SupportedVersion::classify(Version::new(1, 12, 99))
            .expect_err("older than the minimum must fail");
        assert_eq!(
            error,
            "Databricks CLI 1.12.99 is unsupported; install 1.13.0 or newer"
        );

        let tested = SupportedVersion::classify(TESTED_DATABRICKS_VERSION).expect("tested");
        assert!(tested.tested);
        assert_eq!(tested.untested_warning(), None);

        let newer = SupportedVersion::classify(Version::new(1, 13, 1)).expect("newer");
        assert!(!newer.tested);
        assert_eq!(
            newer.untested_warning().as_deref(),
            Some(
                "Databricks CLI 1.13.1 is newer than the tested version 1.13.0; output shapes may differ"
            )
        );
    }

    #[test]
    fn run_captured_collects_output() {
        let args = [OsString::from("jobs"), OsString::from("list")];
        let captured = run_captured(fake_databricks().as_os_str(), &args, Duration::from_secs(5))
            .expect("capture fake CLI");

        assert!(captured.status.success());
        assert_eq!(captured.stdout, b"<jobs>\n<list>\n");
        assert!(captured.stderr.is_empty());
    }

    #[test]
    fn captures_stdout_stderr_and_nonzero_status() {
        let mut command = Command::new(fake_databricks());
        command
            .args(["jobs", "list"])
            .env("STDERR_BYTES", "17")
            .env("FAKE_DATABRICKS_EXIT_CODE", "42");
        let captured =
            run_captured_command(&mut command, Duration::from_secs(5)).expect("capture fake CLI");

        assert_eq!(captured.status.code(), Some(42));
        assert_eq!(captured.stdout, b"<jobs>\n<list>\n");
        assert_eq!(captured.stderr, vec![b'e'; 17]);
    }

    #[test]
    fn times_out_and_kills_the_child() {
        let mut command = Command::new(fake_databricks());
        command.env("SLEEP_MS", "5000");
        let started = Instant::now();
        let error = run_captured_command(&mut command, Duration::from_millis(100))
            .expect_err("sleeping CLI must time out");
        assert!(error.contains("timed out after 100 ms"), "{error}");
        assert!(started.elapsed() < PROMPT, "took {:?}", started.elapsed());
    }

    #[test]
    fn timeout_is_bounded_when_a_descendant_holds_the_pipes() {
        let mut command = Command::new(fake_databricks());
        command
            .env("SLEEP_MS", "10000")
            .env("HOLD_PIPES_MS", "10000");
        let started = Instant::now();
        let error = run_captured_command(&mut command, Duration::from_secs(1))
            .expect_err("sleeping CLI must time out");
        assert!(error.contains("timed out after 1000 ms"), "{error}");
        assert!(error.contains("may still be holding them"), "{error}");
        assert!(
            started.elapsed() < HELD_PIPES_BOUND,
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn exit_is_bounded_when_a_descendant_holds_the_pipes() {
        let mut command = Command::new(fake_databricks());
        command.env("HOLD_PIPES_MS", "10000");
        let started = Instant::now();
        let error = run_captured_command(&mut command, Duration::from_secs(1))
            .expect_err("held pipes must not block past the deadline");
        assert!(error.contains("exited, but"), "{error}");
        assert!(error.contains("may still be holding them"), "{error}");
        assert!(
            started.elapsed() < HELD_PIPES_BOUND,
            "took {:?}",
            started.elapsed()
        );
    }

    // Checks only that the child reaches end-of-input immediately. This would
    // also pass if stdin were inherited from an already-empty test stdin (as in
    // CI); `doctor_does_not_share_its_stdin_with_databricks` in the wrapper
    // contract is the regression guard, because it gives dbxctl an open stdin.
    #[test]
    fn child_gets_an_empty_stdin() {
        let mut command = Command::new(fake_databricks());
        command.env("READ_STDIN", "1");
        let captured =
            run_captured_command(&mut command, Duration::from_secs(5)).expect("capture fake CLI");
        assert!(captured.status.success());
        assert_eq!(captured.stderr, b"stdin bytes: 0\n");
    }

    #[test]
    fn version_check_honors_its_timeout() {
        let directory = std::env::temp_dir().join(format!("dbxctl-slow-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create slow fixture directory");
        let slow = directory.join(format!(
            "fake-databricks-slow{}",
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::copy(fake_databricks(), &slow).expect("copy fixture");

        let started = Instant::now();
        let error = validate(slow.as_os_str(), Duration::from_millis(100))
            .expect_err("hung version command must time out");
        assert!(error.contains("timed out after 100 ms"), "{error}");
        assert!(started.elapsed() < PROMPT, "took {:?}", started.elapsed());
    }

    #[test]
    fn drains_large_stdout_and_stderr_without_deadlock() {
        const BYTES: usize = 256 * 1024;
        let mut command = Command::new(fake_databricks());
        command
            .env("STDOUT_BYTES", BYTES.to_string())
            .env("STDERR_BYTES", BYTES.to_string());
        let captured = run_captured_command(&mut command, Duration::from_secs(5))
            .expect("capture large output");

        assert!(captured.status.success());
        assert_eq!(captured.stdout, vec![b'o'; BYTES]);
        assert_eq!(captured.stderr, vec![b'e'; BYTES]);
    }

    fn fake_databricks() -> &'static Path {
        static BINARY: OnceLock<PathBuf> = OnceLock::new();
        BINARY.get_or_init(compile_fake).as_path()
    }

    fn compile_fake() -> PathBuf {
        let output_dir =
            std::env::temp_dir().join(format!("dbxctl-unit-fixture-{}", std::process::id()));
        std::fs::create_dir_all(&output_dir).expect("create fixture output directory");
        let binary = output_dir.join(format!("fake-databricks{}", std::env::consts::EXE_SUFFIX));
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_databricks.rs");
        let status = Command::new(OsStr::new("rustc"))
            .args([OsStr::new("--edition"), OsStr::new("2024")])
            .arg(source)
            .arg(OsStr::new("-o"))
            .arg(&binary)
            .status()
            .expect("execute rustc for unit-test fixture");
        assert!(status.success(), "compile Rust Databricks fixture");
        binary
    }
}

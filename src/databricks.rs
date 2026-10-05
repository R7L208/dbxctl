use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MINIMUM_DATABRICKS_VERSION: Version = Version::new(0, 200, 0);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not execute {display}: {error}"))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        let _ = child.kill();
        format!("could not capture stdout from {display}")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        let _ = child.kill();
        format!("could not capture stderr from {display}")
    })?;

    // Drain both pipes concurrently. Waiting before reading can deadlock when
    // either pipe fills its operating-system buffer.
    let stdout_reader = thread::spawn(move || read_all(stdout));
    let stderr_reader = thread::spawn(move || read_all(stderr));
    let started = Instant::now();

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                terminate(&mut child);
                let stdout = join_reader(stdout_reader, "stdout")?;
                let stderr = join_reader(stderr_reader, "stderr")?;
                return Err(format!(
                    "{display} timed out after {} ms (captured {} stdout bytes and {} stderr bytes)",
                    timeout.as_millis(),
                    stdout.len(),
                    stderr.len()
                ));
            }
            Ok(None) => thread::sleep(POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed()))),
            Err(error) => {
                terminate(&mut child);
                let _ = join_reader(stdout_reader, "stdout");
                let _ = join_reader(stderr_reader, "stderr");
                return Err(format!("could not wait for {display}: {error}"));
            }
        }
    };

    Ok(Captured {
        stdout: join_reader(stdout_reader, "stdout")?,
        stderr: join_reader(stderr_reader, "stderr")?,
        status,
    })
}

fn read_all(mut stream: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn join_reader(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stream: &str,
) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| format!("{stream} capture thread panicked"))?
        .map_err(|error| format!("could not read captured {stream}: {error}"))
}

fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn installed_version(binary: &OsStr) -> Result<Version, String> {
    let Captured {
        stdout,
        stderr,
        status,
    } = run_captured(binary, &[OsString::from("version")], VERSION_TIMEOUT)?;

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

fn validate(binary: &OsStr) -> Result<Version, String> {
    let version = installed_version(binary)?;
    if version < MINIMUM_DATABRICKS_VERSION {
        return Err(format!(
            "Databricks CLI {version} is unsupported; install {MINIMUM_DATABRICKS_VERSION} or newer"
        ));
    }
    Ok(version)
}

pub(crate) fn run_doctor(binary: &OsStr) -> ExitCode {
    println!("dbxctl {}", env!("CARGO_PKG_VERSION"));
    match validate(binary) {
        Ok(version) => {
            println!("Databricks CLI {version} ({})", binary.display());
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
    use std::time::Duration;

    use super::{run_captured, run_captured_command};

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
        command.env("SLEEP_MS", "500");
        let error = run_captured_command(&mut command, Duration::from_millis(25))
            .expect_err("sleeping CLI must time out");
        assert!(error.contains("timed out after 25 ms"), "{error}");
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

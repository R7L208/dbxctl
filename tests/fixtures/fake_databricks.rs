use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

// Scenario replay: when the directory this program was invoked from holds
// `fake-scenario.txt`, every invocation is appended to `fake-invocations.log`
// and answered from the scenario instead of the behaviors below. Tests link
// the fixture into a scratch directory, so no environment is needed and the
// fixture also works under in-process tests.
const SCENARIO_FILE: &str = "fake-scenario.txt";
const INVOCATION_LOG: &str = "fake-invocations.log";
// Exit code for an argv that no scenario case matches.
const NO_MATCHING_CASE: u8 = 99;

fn main() -> ExitCode {
    let mut argv = env::args_os();
    let program = argv.next().map(PathBuf::from);
    let args: Vec<_> = argv.collect();

    if let Some(directory) = program.as_deref().and_then(Path::parent) {
        let scenario = directory.join(SCENARIO_FILE);
        if scenario.is_file() {
            return replay(&scenario, &directory.join(INVOCATION_LOG), &args);
        }
    }

    if let Some(milliseconds) = env_usize("HOLD_PIPES_MS") {
        // Leave a descendant holding stdout and stderr open after this process
        // exits or is killed, as `databricks bundle` does with Terraform.
        Command::new(env::current_exe().expect("locate fake CLI"))
            .env_remove("HOLD_PIPES_MS")
            .env("SLEEP_MS", milliseconds.to_string())
            .spawn()
            .expect("spawn pipe-holding descendant");
    }
    if env::var_os("READ_STDIN").is_some() {
        // Blocks until EOF, so an inherited, still-open stdin hangs the CLI.
        let mut input = Vec::new();
        io::stdin().read_to_end(&mut input).expect("read stdin");
        eprintln!("stdin bytes: {}", input.len());
    }
    if let Some(milliseconds) = env_usize("SLEEP_MS") {
        thread::sleep(Duration::from_millis(milliseconds as u64));
    }
    if is_slow_copy() {
        // Tests that cannot set the environment (such as `doctor`, which builds
        // its own command) copy the fixture to a name ending in `-slow`.
        thread::sleep(Duration::from_secs(5));
    }
    if args.first().is_some_and(|argument| argument == "version") {
        return version_response();
    }

    if let Some(bytes) = env_usize("STDOUT_BYTES") {
        write_bytes(&mut io::stdout(), b'o', bytes, "stdout");
    }
    if let Some(bytes) = env_usize("STDERR_BYTES") {
        write_bytes(&mut io::stderr(), b'e', bytes, "stderr");
    }

    for argument in args {
        println!("<{}>", argument.to_string_lossy());
    }

    if env::var_os("FAKE_DATABRICKS_ABORT").is_some() {
        io::stdout().flush().expect("flush stdout before abort");
        // Terminates the process via SIGABRT on Unix, exercising the wrapper's
        // signal-termination exit-code path.
        std::process::abort();
    }

    env::var("FAKE_DATABRICKS_EXIT_CODE")
        .ok()
        .and_then(|code| code.parse::<u8>().ok())
        .map_or(ExitCode::SUCCESS, ExitCode::from)
}

/// One scenario case: the exact argv it answers and the response.
#[derive(Default)]
struct Case {
    args: Vec<String>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit: u8,
    abort: bool,
}

/// Answers `args` from a scenario file.
///
/// The format is line-based. Blank lines and lines starting with `#` are
/// ignored. `case` starts a case; within it, each `arg <value>` adds one argv
/// element, `stdout <text>` and `stderr <text>` append to the response, and
/// `exit <code>` sets its exit code (default 0), and `abort` ends the process
/// with `SIGABRT` (on Unix) after writing the response. Values support the escapes
/// `\\`, `\n`, `\r`, `\t`, and `\xHH`. The first case whose argv matches
/// exactly wins; an unmatched argv exits with `NO_MATCHING_CASE`.
fn replay(scenario: &Path, log: &Path, args: &[OsString]) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    let mut line = String::new();
    for argument in &args {
        line.push('\t');
        line.push_str(&escape(argument));
    }
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .expect("append to the invocation log");

    let text = fs::read_to_string(scenario).expect("read scenario");
    let cases = parse_scenario(&text);
    let Some(case) = cases.iter().find(|case| case.args == args) else {
        eprintln!("fake databricks: no scenario case for argv {args:?}");
        return ExitCode::from(NO_MATCHING_CASE);
    };
    io::stdout().write_all(&case.stdout).expect("write stdout");
    io::stderr().write_all(&case.stderr).expect("write stderr");
    if case.abort {
        io::stdout().flush().expect("flush stdout before abort");
        std::process::abort();
    }
    ExitCode::from(case.exit)
}

fn parse_scenario(text: &str) -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (keyword, value) = line.split_once(' ').unwrap_or((line, ""));
        if keyword == "case" {
            cases.push(Case::default());
            continue;
        }
        let case = cases
            .last_mut()
            .unwrap_or_else(|| panic!("scenario line {}: expected `case`", number + 1));
        match keyword {
            "arg" => case.args.push(String::from_utf8(unescape(value)).expect("UTF-8 arg")),
            "stdout" => case.stdout.extend(unescape(value)),
            "stderr" => case.stderr.extend(unescape(value)),
            "exit" => case.exit = value.parse().expect("exit code from 0 to 255"),
            "abort" => case.abort = true,
            _ => panic!("scenario line {}: unknown keyword {keyword:?}", number + 1),
        }
    }
    cases
}

fn unescape(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        match input.next() {
            Some(b'\\') => bytes.push(b'\\'),
            Some(b'n') => bytes.push(b'\n'),
            Some(b'r') => bytes.push(b'\r'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'x') => {
                let digits = [input.next(), input.next()];
                let hex: String = digits.iter().flatten().map(|&digit| char::from(digit)).collect();
                bytes.push(u8::from_str_radix(&hex, 16).expect("two hex digits after \\x"));
            }
            other => panic!("unsupported escape \\{other:?} in scenario"),
        }
    }
    bytes
}

/// Escapes one argv element for the tab-separated invocation log.
fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn is_slow_copy() -> bool {
    env::current_exe().ok().is_some_and(|path| {
        path.file_stem()
            .is_some_and(|stem| stem.to_string_lossy().ends_with("-slow"))
    })
}

fn env_usize(name: &str) -> Option<usize> {
    env::var(name).ok().and_then(|value| value.parse().ok())
}

fn write_bytes(stream: &mut impl Write, byte: u8, count: usize, name: &str) {
    let chunk = vec![byte; count.min(16 * 1024)];
    let mut remaining = count;
    while remaining > 0 {
        let length = remaining.min(chunk.len());
        stream
            .write_all(&chunk[..length])
            .unwrap_or_else(|error| panic!("write {name}: {error}"));
        remaining -= length;
    }
}

fn version_response() -> ExitCode {
    match env::var("FAKE_DATABRICKS_VERSION_MODE").as_deref() {
        Ok("old") => println!("Databricks CLI v1.12.0"),
        Ok("malformed") => println!("unexpected output"),
        Ok("non-utf8") => {
            io::stdout().write_all(&[0xff]).expect("write invalid UTF-8");
        }
        Ok("fail") => return ExitCode::from(9),
        _ => println!("Databricks CLI v1.13.0"),
    }
    ExitCode::SUCCESS
}

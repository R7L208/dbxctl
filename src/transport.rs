//! Read-only transport to a Databricks workspace (#31).
//!
//! Every workspace operation runs the resolved `databricks` executable (R6)
//! from the bundle root, with the bundle target and, when set, the profile,
//! so the CLI picks the workspace and credentials as `databricks bundle` does.
//!
//! - API reads run `databricks api get <path> --debug`. The CLI prints the
//!   response body on stdout and the HTTP exchange on stderr; the status comes
//!   from the last logged response's `< HTTP/<version> <code>` line and, on an
//!   error, the `error_code` and `message` from the JSON error body.
//! - Statements use the Statement Execution API: submit, poll until done,
//!   cancel at the deadline or when a poll fails, and read every result
//!   chunk; a result the API truncated is an error rather than partial rows.
//!   Only statements that pass [`safety::is_read_statement`] are submitted;
//!   anything else is refused before a CLI process starts.
//!
//! Every CLI run comes back as a [`Call`] for the evidence record, with
//! credentials scrubbed from its output; a statement response keeps only its
//! first [`STATEMENT_EVIDENCE_BYTES`]. Full redaction is #36.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::databricks::{Captured, run_captured_command};
use crate::json::{self, Document, Kind, Node, Value};
use crate::safety;

/// The longest the warehouse may hold the submit request before answering
/// with a pending statement. The API accepts 0 (answer at once) or 5 to 50
/// seconds.
const WAIT_TIMEOUT: Duration = Duration::from_secs(30);
/// The shortest wait the API accepts other than 0.
const MIN_WAIT: Duration = Duration::from_secs(5);
/// How much of a CLI run a submit leaves for the CLI itself, so the run is
/// not stopped before the warehouse answers with a statement ID.
const SUBMIT_MARGIN: Duration = Duration::from_secs(5);
const STATEMENTS: &str = "/api/2.0/sql/statements";
const REDACTED: &str = "[REDACTED]";
/// The CLI commands [`Runner::run`] may run, by their full command path.
/// Anything else is refused without starting a process.
const READ_ONLY_RUNS: &[&[&str]] = &[
    &["version"],
    &["bundle", "validate"],
    &["bundle", "plan"],
    &["bundle", "schema"],
];
/// How much of each statement response a [`Call`] keeps as evidence. The
/// rows are in the [`ResultSet`], so the evidence needs only the start.
const STATEMENT_EVIDENCE_BYTES: usize = 64 * 1024;

/// The time limits for CLI calls and statements.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// One CLI run. A submit asks the warehouse to answer within this less
    /// [`SUBMIT_MARGIN`], so it gets a statement ID to poll or cancel.
    pub(crate) call: Duration,
    /// From submitting a statement until it must have finished.
    pub(crate) deadline: Duration,
    /// Between polls of a running statement.
    pub(crate) poll: Duration,
}

impl Limits {
    /// How long the warehouse may hold a submit: [`WAIT_TIMEOUT`], cut to
    /// the deadline and to the call less [`SUBMIT_MARGIN`]. Below
    /// [`MIN_WAIT`] it answers at once and the statement is polled instead.
    fn submit_wait(&self) -> Duration {
        let wait = WAIT_TIMEOUT
            .min(self.deadline)
            .min(self.call.saturating_sub(SUBMIT_MARGIN));
        let wait = Duration::from_secs(wait.as_secs());
        if wait < MIN_WAIT {
            Duration::ZERO
        } else {
            wait
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            call: Duration::from_secs(60),
            deadline: Duration::from_secs(300),
            poll: Duration::from_secs(1),
        }
    }
}

/// One CLI run: its arguments and output, with credentials scrubbed.
#[derive(Debug)]
pub(crate) struct Call {
    pub(crate) args: Vec<OsString>,
    pub(crate) result: Result<Captured, String>,
}

/// What an operation returned, and every CLI run it took.
#[derive(Debug)]
pub(crate) struct Outcome<T> {
    pub(crate) calls: Vec<Call>,
    pub(crate) result: Result<T, Error>,
}

/// A successful API response.
#[derive(Debug)]
pub(crate) struct Response {
    /// The HTTP status from the debug log, if the CLI printed one.
    pub(crate) status: Option<u16>,
    /// The response body, or `None` when the CLI printed none.
    pub(crate) body: Option<Document>,
}

/// The rows of a finished statement.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ResultSet {
    pub(crate) statement_id: String,
    pub(crate) columns: Vec<String>,
    /// Each cell as the API renders it in `JSON_ARRAY` format: text, or
    /// `None` for SQL `NULL`.
    pub(crate) rows: Vec<Vec<Option<String>>>,
}

/// Why an operation failed.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Error {
    /// The statement is not read-only; no CLI process was started.
    Refused,
    /// The CLI could not be run to completion, or its output was not the
    /// expected response. The [`Call`] has the details.
    Transport(String),
    /// The API answered with an HTTP error.
    Http {
        status: u16,
        error_code: Option<String>,
        message: Option<String>,
    },
    /// The warehouse ran the statement and it ended `FAILED`, `CANCELED`, or
    /// `CLOSED`.
    Sql {
        state: String,
        error_code: Option<String>,
        message: Option<String>,
    },
    /// The statement had not finished at the deadline; a cancel was sent.
    Timeout { statement_id: String },
    /// The statement succeeded, but the API kept only part of its result,
    /// as it does past the 25 MiB inline limit.
    Truncated { statement_id: String },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused => formatter.write_str("refused a statement that is not read-only"),
            Self::Transport(problem) => formatter.write_str(problem),
            Self::Http {
                status, error_code, ..
            } => match error_code {
                Some(code) => write!(formatter, "HTTP {status} {code}"),
                None => write!(formatter, "HTTP {status}"),
            },
            Self::Sql {
                state, error_code, ..
            } => match error_code {
                Some(code) => write!(formatter, "statement {state}: {code}"),
                None => write!(formatter, "statement {state}"),
            },
            Self::Timeout { statement_id } => {
                write!(
                    formatter,
                    "statement {statement_id} timed out and was cancelled"
                )
            }
            Self::Truncated { statement_id } => {
                write!(
                    formatter,
                    "statement {statement_id} returned a truncated result"
                )
            }
        }
    }
}

/// How probes reach the workspace. Tests can supply their own.
pub(crate) trait Runner {
    /// Runs a read-only CLI command, such as `bundle validate`, and captures
    /// its output. A command not known to be read-only is refused.
    fn run(&self, args: &[&str], timeout: Duration) -> Call;

    /// Reads an API path, such as `/api/2.0/pipelines/<id>`.
    fn api_get(&self, path: &str) -> Outcome<Response>;

    /// Runs a read-only statement on a SQL warehouse and returns every row.
    fn statement(&self, sql: &str, warehouse_id: &str) -> Outcome<ResultSet>;
}

/// The [`Runner`] that runs the `databricks` CLI for one bundle target.
#[derive(Debug)]
pub(crate) struct CliRunner {
    binary: OsString,
    root: PathBuf,
    target: String,
    profile: Option<String>,
    limits: Limits,
}

impl CliRunner {
    pub(crate) fn for_bundle(
        binary: &OsStr,
        root: &Path,
        target: &str,
        profile: Option<&str>,
    ) -> Self {
        Self {
            binary: binary.to_os_string(),
            root: root.to_path_buf(),
            target: target.to_owned(),
            profile: profile.map(str::to_owned),
            limits: Limits::default(),
        }
    }

    pub(crate) fn with_limits(self, limits: Limits) -> Self {
        Self { limits, ..self }
    }

    /// The full argument list: `args`, then the target and profile.
    fn arguments(&self, args: &[&str]) -> Vec<OsString> {
        let mut full: Vec<OsString> = args.iter().map(OsString::from).collect();
        full.extend(["-t".into(), self.target.clone().into()]);
        if let Some(profile) = &self.profile {
            full.extend(["-p".into(), profile.clone().into()]);
        }
        full
    }

    fn command(&self, args: &[OsString]) -> Command {
        let mut command = Command::new(&self.binary);
        command.current_dir(&self.root).args(args);
        command
    }

    /// Runs the CLI and returns its full arguments and raw result. Parse the
    /// result first, then hand it to [`evidence`], which takes it.
    fn capture(
        &self,
        args: &[&str],
        timeout: Duration,
    ) -> (Vec<OsString>, Result<Captured, String>) {
        let args = self.arguments(args);
        let result = run_captured_command(&mut self.command(&args), timeout);
        (args, result)
    }

    /// One `databricks api <method>` call, stopped after `timeout`, whose
    /// [`Call`] keeps up to `kept` bytes of the response.
    fn api(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        timeout: Duration,
        kept: usize,
    ) -> (Call, Result<Response, Error>) {
        if let Err(problem) = check_path(path) {
            let call = Call {
                args: Vec::new(),
                result: Err(problem.clone()),
            };
            return (call, Err(Error::Transport(problem)));
        }
        let mut args = vec!["api", method, path];
        if let Some(body) = body {
            args.extend(["--json", body]);
        }
        args.push("--debug");
        let (args, result) = self.capture(&args, timeout);
        let response = response(method, &result);
        (evidence(args, result, kept), response)
    }

    fn statement_rows(
        &self,
        sql: &str,
        warehouse_id: &str,
        calls: &mut Vec<Call>,
    ) -> Result<ResultSet, Error> {
        let started = Instant::now();
        let submit = submit_body(sql, warehouse_id, self.limits.submit_wait());
        let body =
            self.statement_call("post", STATEMENTS, Some(&submit), self.limits.call, calls)?;
        let statement_id = body
            .root()
            .get("statement_id")
            .and_then(Node::as_str)
            .filter(|id| is_statement_id(id))
            .ok_or_else(|| {
                Error::Transport("the statement response has no valid `statement_id`".to_owned())
            })?
            .to_owned();
        let status_path = format!("{STATEMENTS}/{statement_id}");
        match self.wait(body, started, &statement_id, &status_path, calls) {
            Ok(body) => self.result_set(&body, statement_id, &status_path, calls),
            Err(error) => {
                // Unless it ended on its own, the statement may still be
                // running, so cancel it rather than leave it on the warehouse.
                if !matches!(error, Error::Sql { .. }) {
                    self.cancel(&status_path, calls);
                }
                Err(error)
            }
        }
    }

    /// Polls a submitted statement until it succeeds and returns the final
    /// response.
    fn wait(
        &self,
        mut body: Document,
        started: Instant,
        statement_id: &str,
        status_path: &str,
        calls: &mut Vec<Call>,
    ) -> Result<Document, Error> {
        let timeout = || Error::Timeout {
            statement_id: statement_id.to_owned(),
        };
        loop {
            let state = body
                .root()
                .pointer("/status/state")
                .and_then(Node::as_str)
                .unwrap_or_default()
                .to_owned();
            match state.as_str() {
                // A result that came back after the deadline is too late.
                "SUCCEEDED" if started.elapsed() > self.limits.deadline => {
                    return Err(timeout());
                }
                "SUCCEEDED" => return Ok(body),
                "PENDING" | "RUNNING" => {
                    // Neither the sleep nor the poll may outlast the deadline.
                    let remaining = || self.limits.deadline.saturating_sub(started.elapsed());
                    thread::sleep(self.limits.poll.min(remaining()));
                    if remaining().is_zero() {
                        return Err(timeout());
                    }
                    let limit = self.limits.call.min(remaining());
                    match self.statement_call("get", status_path, None, limit, calls) {
                        Ok(next) => body = next,
                        Err(_) if remaining().is_zero() => return Err(timeout()),
                        Err(error) => return Err(error),
                    }
                }
                "FAILED" | "CANCELED" | "CLOSED" => {
                    let error = body.root().pointer("/status/error");
                    let text = |key| {
                        error
                            .and_then(|error| error.get(key))
                            .and_then(Node::as_str)
                            .map(str::to_owned)
                    };
                    return Err(Error::Sql {
                        state,
                        error_code: text("error_code"),
                        message: text("message"),
                    });
                }
                "" => {
                    return Err(Error::Transport(
                        "the statement response has no `status.state`".to_owned(),
                    ));
                }
                other => {
                    return Err(Error::Transport(format!(
                        "the statement is in an unexpected state {other:?}"
                    )));
                }
            }
        }
    }

    /// The rows of a finished statement: those in `body`, then every further
    /// result chunk. Each fetched chunk must be the one asked for, and the
    /// rows and chunks read must match the manifest's totals when it has
    /// them.
    fn result_set(
        &self,
        body: &Document,
        statement_id: String,
        status_path: &str,
        calls: &mut Vec<Call>,
    ) -> Result<ResultSet, Error> {
        // Partial rows would read as a complete answer, so refuse them.
        if body
            .root()
            .pointer("/manifest/truncated")
            .and_then(Node::as_bool)
            == Some(true)
        {
            return Err(Error::Truncated { statement_id });
        }
        let columns = columns(body.root())?;
        let mut rows = Vec::new();
        let mut chunk = body.root().get("result");
        let mut chunks = u64::from(chunk.is_some());
        let mut owned: Document;
        // The inline chunk counts too, so it cannot point back at itself. It
        // is the first chunk when the response does not number it.
        let mut last_index = chunk.map(|result| {
            result
                .get("chunk_index")
                .and_then(Node::as_u64)
                .unwrap_or(0)
        });
        loop {
            let next = match chunk {
                Some(result) => {
                    append_rows(result, &mut rows)?;
                    result.get("next_chunk_index").and_then(Node::as_u64)
                }
                None => None,
            };
            let Some(index) = next else {
                break;
            };
            // Each chunk must move forward, so a bad response cannot loop.
            if last_index.is_some_and(|last| index <= last) {
                return Err(Error::Transport(format!(
                    "result chunk {index} does not follow chunk {}",
                    last_index.unwrap_or_default()
                )));
            }
            last_index = Some(index);
            let path = format!("{status_path}/result/chunks/{index}");
            owned = self.statement_call("get", &path, None, self.limits.call, calls)?;
            let returned = owned.root().get("chunk_index").and_then(Node::as_u64);
            if returned.is_some_and(|returned| returned != index) {
                return Err(Error::Transport(format!(
                    "result chunk {} was returned for chunk {index}",
                    returned.unwrap_or_default()
                )));
            }
            chunks += 1;
            chunk = Some(owned.root());
        }
        let total = |key: &str| {
            body.root()
                .get("manifest")
                .and_then(|manifest| manifest.get(key))
                .and_then(Node::as_u64)
        };
        let read = rows.len() as u64;
        for (key, read) in [("total_chunk_count", chunks), ("total_row_count", read)] {
            if let Some(total) = total(key).filter(|&total| total != read) {
                return Err(Error::Transport(format!(
                    "read {read} where `manifest.{key}` is {total}"
                )));
            }
        }
        Ok(ResultSet {
            statement_id,
            columns,
            rows,
        })
    }

    /// Sends a best-effort cancel for a statement. A failed cancel is
    /// recorded but does not change the outcome.
    fn cancel(&self, status_path: &str, calls: &mut Vec<Call>) {
        let cancel = format!("{status_path}/cancel");
        let (call, _) = self.api("post", &cancel, Some("{}"), self.limits.call, usize::MAX);
        calls.push(call);
    }

    /// One API call during a statement; its body must be present.
    fn statement_call(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        timeout: Duration,
        calls: &mut Vec<Call>,
    ) -> Result<Document, Error> {
        let (call, response) = self.api(method, path, body, timeout, STATEMENT_EVIDENCE_BYTES);
        calls.push(call);
        response?.body.ok_or_else(|| {
            Error::Transport(format!(
                "`databricks api {method} {path}` printed no response body"
            ))
        })
    }
}

impl Runner for CliRunner {
    fn run(&self, args: &[&str], timeout: Duration) -> Call {
        let path: Vec<&str> = args
            .iter()
            .copied()
            .take_while(|argument| !argument.starts_with('-'))
            .collect();
        if !READ_ONLY_RUNS.contains(&path.as_slice()) {
            return Call {
                args: self.arguments(args),
                result: Err(format!(
                    "refused `databricks {}`, which is not a known read-only command",
                    path.join(" ")
                )),
            };
        }
        let (args, result) = self.capture(args, timeout);
        evidence(args, result, usize::MAX)
    }

    fn api_get(&self, path: &str) -> Outcome<Response> {
        let (call, result) = self.api("get", path, None, self.limits.call, usize::MAX);
        Outcome {
            calls: vec![call],
            result,
        }
    }

    fn statement(&self, sql: &str, warehouse_id: &str) -> Outcome<ResultSet> {
        if !safety::is_read_statement(sql) {
            return Outcome {
                calls: Vec::new(),
                result: Err(Error::Refused),
            };
        }
        let mut calls = Vec::new();
        let result = self.statement_rows(sql, warehouse_id, &mut calls);
        Outcome { calls, result }
    }
}

/// The submit request: inline `JSON_ARRAY` results, and a pending statement
/// rather than a cancelled one if it outlasts `wait`.
fn submit_body(sql: &str, warehouse_id: &str, wait: Duration) -> String {
    let body = Value::object([
        ("disposition", Value::string("INLINE")),
        ("format", Value::string("JSON_ARRAY")),
        ("on_wait_timeout", Value::string("CONTINUE")),
        ("statement", Value::string(sql)),
        (
            "wait_timeout",
            Value::string(format!("{}s", wait.as_secs())),
        ),
        ("warehouse_id", Value::string(warehouse_id)),
    ]);
    Document::from(body)
        .to_compact_string()
        .map(|text| text.trim_end().to_owned())
        .unwrap_or_default()
}

/// An API path must be absolute and plain: no whitespace, control
/// characters, or `..` segments.
fn check_path(path: &str) -> Result<(), String> {
    let plain = path.starts_with("/api/")
        && !path.chars().any(|c| c.is_whitespace() || c.is_control())
        && !path.split(['/', '?']).any(|segment| segment == "..");
    if plain {
        Ok(())
    } else {
        Err(format!("refused API path {path:?}"))
    }
}

/// Statement IDs are UUID-like: letters, digits, and `-`.
fn is_statement_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Classifies one `databricks api` run.
fn response(method: &str, result: &Result<Captured, String>) -> Result<Response, Error> {
    let Ok(captured) = result else {
        return Err(Error::Transport(format!(
            "`databricks api {method}` could not be run to completion"
        )));
    };
    let stderr = String::from_utf8_lossy(&captured.stderr);
    let status = http_status(&stderr);
    if let Some(status) = status.filter(|&status| status >= 400) {
        let error = error_body(&stderr);
        let text = |key| {
            error
                .as_ref()
                .and_then(|error| error.root().get(key)?.as_str().map(str::to_owned))
        };
        return Err(Error::Http {
            status,
            error_code: text("error_code"),
            message: text("message"),
        });
    }
    if !captured.status.success() {
        let exit = captured
            .status
            .code()
            .map_or_else(|| "a signal".to_owned(), |code| format!("exit code {code}"));
        return Err(Error::Transport(format!(
            "`databricks api {method}` failed with {exit} and no HTTP error status"
        )));
    }
    if captured.stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(Response { status, body: None });
    }
    let body = json::parse_bytes(&captured.stdout).map_err(|_| {
        Error::Transport(format!(
            "`databricks api {method}` printed a response body that is not JSON"
        ))
    })?;
    Ok(Response {
        status,
        body: Some(body),
    })
}

/// The last response in a debug log: its status and the lines logged with
/// it, without their `<` prefix.
struct Logged<'a> {
    status: u16,
    lines: Vec<&'a str>,
}

/// The last response in a debug log.
///
/// The SDK logs each response as a `< HTTP/<version> <code>` line, after an
/// optional timestamp, followed by its headers and body on lines that also
/// start with `<`. Those lines belong to the response, so text in a body that
/// looks like a status line cannot be taken for one.
fn logged_response(stderr: &str) -> Option<Logged<'_>> {
    let mut last: Option<Logged<'_>> = None;
    let mut in_response = false;
    for line in stderr.lines() {
        let content = without_timestamp(line);
        if in_response && let Some(rest) = content.strip_prefix('<') {
            if let Some(last) = &mut last {
                last.lines.push(rest.strip_prefix(' ').unwrap_or(rest));
            }
            continue;
        }
        in_response = false;
        if let Some(status) = status_code(content) {
            last = Some(Logged {
                status,
                lines: Vec::new(),
            });
            in_response = true;
        }
    }
    last
}

/// The status of the last response in a debug log.
fn http_status(stderr: &str) -> Option<u16> {
    logged_response(stderr).map(|logged| logged.status)
}

/// `line` without a leading log timestamp such as `2024/10/23 10:30:45 `.
fn without_timestamp(line: &str) -> &str {
    line.trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '/' | ':' | ' '))
}

/// The code on a `< HTTP/<version> <code> <reason>` status line.
fn status_code(content: &str) -> Option<u16> {
    let rest = content.strip_prefix("< HTTP/")?;
    let code = rest.split_whitespace().nth(1)?;
    code.parse().ok().filter(|code| (100..600).contains(code))
}

/// The JSON error body of a failed request.
///
/// The CLI logs it with the last response, as `< {` through `< }` followed
/// by ` pid=… sdk=true`. Failing that, it is the first object that starts a
/// line of the log, ending on that line when it closes there or else on the
/// next line that starts with `}`. Output after it is ignored, and each line
/// is read once.
fn error_body(stderr: &str) -> Option<Document> {
    logged_response(stderr)
        .and_then(|logged| logged_object(&logged.lines))
        .or_else(|| first_object(stderr))
}

/// The JSON object in a logged response: from its first line that starts
/// with `{` to the last `}`, which drops the log fields after it.
fn logged_object(lines: &[&str]) -> Option<Document> {
    let start = lines.iter().position(|line| line.starts_with('{'))?;
    let text = lines[start..].join("\n");
    let end = text.rfind('}')?;
    json::parse(&text[..=end])
        .ok()
        .filter(|document| document.root().kind() == Kind::Object)
}

fn first_object(stderr: &str) -> Option<Document> {
    let mut lines = stderr.split_inclusive('\n');
    let mut offset = 0;
    while let Some(line) = lines.next() {
        let start = offset;
        offset += line.len();
        if !line.starts_with('{') {
            continue;
        }
        if !line.trim_end().ends_with('}') {
            for next in lines.by_ref() {
                offset += next.len();
                if next.starts_with('}') {
                    break;
                }
            }
        }
        if let Ok(document) = json::parse(&stderr[start..offset])
            && document.root().kind() == Kind::Object
        {
            return Some(document);
        }
    }
    None
}

/// Column names from `manifest.schema.columns`; absent for a statement with
/// no result set.
fn columns(body: Node<'_>) -> Result<Vec<String>, Error> {
    let Some(columns) = body.pointer("/manifest/schema/columns") else {
        return Ok(Vec::new());
    };
    columns
        .elements()
        .ok_or_else(|| Error::Transport("`manifest.schema.columns` is not an array".to_owned()))?
        .map(|column| {
            column
                .get("name")
                .and_then(Node::as_str)
                .map(str::to_owned)
                .ok_or_else(|| Error::Transport("a result column has no `name`".to_owned()))
        })
        .collect()
}

/// Appends the rows of one result chunk. A chunk without `data_array` has
/// no rows.
fn append_rows(chunk: Node<'_>, rows: &mut Vec<Vec<Option<String>>>) -> Result<(), Error> {
    let Some(data) = chunk.get("data_array") else {
        return Ok(());
    };
    let unexpected = || Error::Transport("`data_array` is not an array of rows of text".to_owned());
    for row in data.elements().ok_or_else(unexpected)? {
        let cells = row.elements().ok_or_else(unexpected)?;
        let row = cells
            .map(|cell| match cell.kind() {
                Kind::Null => Ok(None),
                Kind::String => Ok(cell.as_str().map(str::to_owned)),
                _ => Err(unexpected()),
            })
            .collect::<Result<_, _>>()?;
        rows.push(row);
    }
    Ok(())
}

/// The [`Call`] for a CLI run, keeping up to `kept` bytes of its stdout. It
/// takes the output, so scrubbing makes no copy when there is nothing to
/// scrub.
fn evidence(args: Vec<OsString>, result: Result<Captured, String>, kept: usize) -> Call {
    Call {
        args,
        result: result.map(|captured| scrubbed(captured, kept)),
    }
}

/// `captured` with credentials scrubbed and stdout cut to `kept` bytes.
/// A cut also drops a partial word at the end, so it cannot leave part of
/// a token that the scrubber no longer recognises.
fn scrubbed(captured: Captured, kept: usize) -> Captured {
    let Captured {
        stdout,
        stderr,
        status,
    } = captured;
    let stdout = if stdout.len() > kept {
        let end = stdout[..kept]
            .iter()
            .rposition(|&byte| !is_token_byte(byte))
            .map_or(0, |at| at + 1);
        let omitted = stdout.len() - end;
        let mut excerpt = scrub(stdout[..end].to_vec());
        excerpt.extend_from_slice(format!("\n[{omitted} more bytes not kept]\n").as_bytes());
        excerpt
    } else {
        scrub(stdout)
    };
    Captured {
        stdout,
        stderr: scrub(stderr),
        status,
    }
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
}

/// Replaces credentials in CLI output with `[REDACTED]`: the value of an
/// `Authorization` header, a bearer token, and a personal access token
/// (`dapi` and 32 hex digits). Output without any is returned as is.
fn scrub(output: Vec<u8>) -> Vec<u8> {
    let changed = {
        let text = String::from_utf8_lossy(&output);
        let mut scrubbed = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            let (content, ending) = match line.strip_suffix('\n') {
                Some(content) => (content, "\n"),
                None => (line, ""),
            };
            // Lowercased once: ASCII case changes keep every byte offset.
            let lower = content.to_ascii_lowercase();
            let content = match lower.find("authorization:") {
                Some(at) => format!("{} {REDACTED}", &content[..at + "authorization:".len()]),
                None => replace_bearer(content, &lower),
            };
            scrubbed.push_str(&replace_tokens(&content));
            scrubbed.push_str(ending);
        }
        (scrubbed != text).then_some(scrubbed)
    };
    changed.map_or(output, String::into_bytes)
}

/// `Bearer <token>`, with any run of whitespace after `Bearer`, becomes
/// `Bearer [REDACTED]`. `lower` is `line` in ASCII lowercase.
fn replace_bearer(line: &str, lower: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut start = 0;
    while let Some(found) = lower[start..].find("bearer") {
        let word_end = start + found + "bearer".len();
        let spaces = line[word_end..]
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(line.len() - word_end);
        let token_start = word_end + spaces;
        result.push_str(&line[start..token_start]);
        let after = &line[token_start..];
        let length = after
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(after.len());
        if spaces > 0 && length > 0 && &after[..length] != REDACTED {
            result.push_str(REDACTED);
        } else {
            result.push_str(&after[..length]);
        }
        start = token_start + length;
    }
    result.push_str(&line[start..]);
    result
}

/// Personal access tokens: `dapi` followed by 32 hex digits, with an
/// optional `-<digits>` suffix.
fn replace_tokens(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("dapi") {
        let (before, candidate) = rest.split_at(at);
        result.push_str(before);
        let hex = candidate[4..]
            .bytes()
            .take_while(u8::is_ascii_hexdigit)
            .count();
        if hex >= 32 {
            let mut end = 4 + hex;
            let suffix = &candidate[end..];
            if let Some(digits) = suffix.strip_prefix('-') {
                let count = digits.bytes().take_while(u8::is_ascii_digit).count();
                if count > 0 {
                    end += 1 + count;
                }
            }
            result.push_str(REDACTED);
            rest = &candidate[end..];
        } else {
            result.push_str("dapi");
            rest = &candidate[4..];
        }
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use std::process::ExitStatus;

    use super::{
        CliRunner, Error, Limits, ResultSet, Runner, STATEMENT_EVIDENCE_BYTES, STATEMENTS,
        WAIT_TIMEOUT, error_body, http_status, scrub, scrubbed, submit_body,
    };
    use crate::databricks::Captured;
    use crate::json::Node;
    use crate::test_support::{ScenarioCli, TempDir, assert_no_mutations, fake_databricks};

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cli-1.13.0");
    const SQL: &str = "SELECT 1 AS one, NULL AS nothing";
    const WAREHOUSE: &str = "abc123";
    const ID: &str = "01ef-0a1b";

    struct Fixture {
        _temp: TempDir,
        cli: ScenarioCli,
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str, scenario: &str) -> Self {
            let temp = TempDir::new(name);
            let cli_dir = temp.path().join("cli");
            let root = temp.path().join("bundle");
            fs::create_dir_all(&cli_dir).expect("create CLI dir");
            fs::create_dir_all(&root).expect("create bundle");
            let cli = ScenarioCli::new(fake_databricks(), &cli_dir, scenario);
            Self {
                _temp: temp,
                cli,
                root,
            }
        }

        fn runner(&self, deadline: Duration) -> CliRunner {
            CliRunner::for_bundle(self.cli.binary().as_os_str(), &self.root, "dev", None)
                .with_limits(Limits {
                    call: Duration::from_secs(10),
                    deadline,
                    poll: Duration::ZERO,
                })
        }

        fn invocations(&self) -> Vec<Vec<String>> {
            self.cli.invocations()
        }
    }

    /// A made-up personal access token, built at run time so the source holds
    /// nothing a secret scanner would take for a real one.
    fn fake_token() -> String {
        format!("dapi{}", "0123456789abcdef".repeat(2))
    }

    fn fixture(file: &str) -> String {
        fs::read_to_string(format!("{FIXTURES}/{file}")).expect("read fixture")
    }

    fn escape(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    }

    /// A scenario case for `args` plus the runner's `-t dev`.
    fn case(args: &[&str], stdout: &str, stderr: &str, exit: u8) -> String {
        let mut case = String::from("case\n");
        for arg in args.iter().chain(&["-t", "dev"]) {
            let _ = writeln!(case, "arg {}", escape(arg));
        }
        let _ = writeln!(
            case,
            "stdout {}\nstderr {}\nexit {exit}",
            escape(stdout),
            escape(stderr)
        );
        case
    }

    fn debug_log(method: &str, path: &str, status: &str) -> String {
        format!(
            "2026/10/20 10:00:00 {} {path} HTTP/1.1\n2026/10/20 10:00:00 \"Authorization: Bearer {}\"\n2026/10/20 10:00:00 < HTTP/1.1 {status}\n",
            method.to_uppercase(),
            fake_token()
        )
    }

    /// The wait [`Fixture::runner`]'s 10-second call leaves a submit.
    const TEST_WAIT: Duration = Duration::from_secs(5);

    /// The submit case for [`SQL`] with `wait`, answering with this output.
    fn submit_case_waiting(wait: Duration, stdout: &str, stderr: &str, exit: u8) -> String {
        let body = submit_body(SQL, WAREHOUSE, wait);
        case(
            &["api", "post", STATEMENTS, "--json", &body, "--debug"],
            stdout,
            stderr,
            exit,
        )
    }

    /// The submit case for [`TEST_WAIT`], answering with this output.
    fn submit_case(stdout: &str, stderr: &str, exit: u8) -> String {
        submit_case_waiting(TEST_WAIT, stdout, stderr, exit)
    }

    /// The submit case with `wait`, answering with `body`.
    fn submit_waiting(wait: Duration, body: &str) -> String {
        submit_case_waiting(wait, body, &debug_log("post", STATEMENTS, "200 OK"), 0)
    }

    /// The submit case for [`TEST_WAIT`], answering with `body`.
    fn submit(body: &str) -> String {
        submit_waiting(TEST_WAIT, body)
    }

    fn get(path: &str, body: &str) -> String {
        case(
            &["api", "get", path, "--debug"],
            body,
            &debug_log("get", path, "200 OK"),
            0,
        )
    }

    fn status_path() -> String {
        format!("{STATEMENTS}/{ID}")
    }

    fn cancel_path() -> String {
        format!("{}/cancel", status_path())
    }

    /// The cancel case, answering with success.
    fn cancel_case() -> String {
        let cancel = cancel_path();
        case(
            &["api", "post", &cancel, "--json", "{}", "--debug"],
            "{}",
            &debug_log("post", &cancel, "200 OK"),
            0,
        )
    }

    fn state(state: &str) -> String {
        format!(r#"{{"statement_id":"{ID}","status":{{"state":"{state}"}}}}"#)
    }

    fn succeeded(result: &str) -> String {
        format!(
            r#"{{"statement_id":"{ID}","status":{{"state":"SUCCEEDED"}},"manifest":{{"schema":{{"columns":[{{"name":"one"}},{{"name":"nothing"}}]}}}},"result":{result}}}"#
        )
    }

    fn rows(rows: &[&[Option<&str>]]) -> ResultSet {
        ResultSet {
            statement_id: ID.to_owned(),
            columns: vec!["one".to_owned(), "nothing".to_owned()],
            rows: rows
                .iter()
                .map(|row| row.iter().map(|cell| cell.map(str::to_owned)).collect())
                .collect(),
        }
    }

    const QUICK: Duration = Duration::from_secs(60);

    #[test]
    fn runs_from_the_bundle_root_with_target_and_profile() {
        let root = PathBuf::from("/bundle/root");
        for (profile, expected) in [
            (
                Some("prod"),
                &["bundle", "validate", "-t", "dev", "-p", "prod"][..],
            ),
            (None, &["bundle", "validate", "-t", "dev"]),
        ] {
            let runner = CliRunner::for_bundle("databricks".as_ref(), &root, "dev", profile);
            let args = runner.arguments(&["bundle", "validate"]);
            let command = runner.command(&args);
            assert_eq!(command.get_current_dir(), Some(root.as_path()));
            let actual: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_string_lossy())
                .collect();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn run_captures_output_with_credentials_scrubbed() {
        let token = fake_token();
        let scenario = case(
            &["bundle", "validate"],
            "{}",
            &format!("Authorization: Bearer {token}\nusing token {token}-2\n"),
            0,
        );
        let fixture = Fixture::new("transport-run", &scenario);
        let call = fixture.runner(QUICK).run(&["bundle", "validate"], QUICK);
        assert_eq!(call.args, ["bundle", "validate", "-t", "dev"]);
        let captured = call.result.expect("captured");
        let stderr = String::from_utf8(captured.stderr).expect("UTF-8");
        assert_eq!(
            stderr,
            "Authorization: [REDACTED]\nusing token [REDACTED]\n"
        );
        assert_eq!(fixture.invocations(), [["bundle", "validate", "-t", "dev"]]);
    }

    #[test]
    fn api_get_returns_the_body_and_status() {
        let path = "/api/2.0/pipelines/01a23b45c67d8901";
        let scenario = case(
            &["api", "get", path, "--debug"],
            &fixture("api-debug-200.stdout"),
            &fixture("api-debug-200.stderr"),
            0,
        );
        let fixture = Fixture::new("transport-get", &scenario);
        let outcome = fixture.runner(QUICK).api_get(path);
        let response = outcome.result.expect("response");
        assert_eq!(response.status, Some(200));
        let body = response.body.expect("body");
        assert_eq!(
            body.root().get("name").and_then(Node::as_str),
            Some("dlt_pipeline")
        );
        assert_eq!(outcome.calls.len(), 1);
        let invocations = fixture.invocations();
        assert_eq!(invocations, [["api", "get", path, "--debug", "-t", "dev"]]);
        assert_no_mutations(&invocations);
    }

    #[test]
    fn api_get_classifies_http_errors() {
        for (name, file, status, code, message) in [
            (
                "transport-404",
                "api-debug-404.stderr",
                404,
                "RESOURCE_NOT_FOUND",
                "Pipeline not found",
            ),
            (
                "transport-400",
                "api-debug-400.stderr",
                400,
                "INVALID_PARAMETER_VALUE",
                "The provided task is invalid. Reason: cluster_key_pair is required",
            ),
        ] {
            let scenario = case(&["api", "get", "/api/x", "--debug"], "", &fixture(file), 1);
            let fixture = Fixture::new(name, &scenario);
            let outcome = fixture.runner(QUICK).api_get("/api/x");
            assert_eq!(
                outcome.result.expect_err(name),
                Error::Http {
                    status,
                    error_code: Some(code.to_owned()),
                    message: Some(message.to_owned()),
                },
                "{name}"
            );
        }
    }

    #[test]
    fn api_get_reports_unusable_runs_as_transport_errors() {
        for (name, stdout, stderr, exit, problem) in [
            (
                "transport-exit",
                "",
                "Error: no profile\n",
                1,
                "no HTTP error status",
            ),
            (
                "transport-garbage",
                "not json",
                "< HTTP/1.1 200 OK\n",
                0,
                "not JSON",
            ),
        ] {
            let scenario = case(&["api", "get", "/api/x", "--debug"], stdout, stderr, exit);
            let fixture = Fixture::new(name, &scenario);
            let error = fixture
                .runner(QUICK)
                .api_get("/api/x")
                .result
                .expect_err(name);
            assert!(
                matches!(&error, Error::Transport(text) if text.contains(problem)),
                "{name}: {error:?}"
            );
        }

        let fixture = Fixture::new(
            "transport-empty",
            &case(&["api", "get", "/api/x", "--debug"], "", "", 0),
        );
        let response = fixture
            .runner(QUICK)
            .api_get("/api/x")
            .result
            .expect("response");
        assert!(response.body.is_none() && response.status.is_none());

        let temp = TempDir::new("transport-missing");
        let runner = CliRunner::for_bundle(
            temp.path().join("no-such-databricks").as_os_str(),
            temp.path(),
            "dev",
            None,
        );
        let outcome = runner.api_get("/api/x");
        assert!(
            matches!(outcome.result, Err(Error::Transport(ref text)) if text.contains("could not be run"))
        );
        assert!(outcome.calls[0].result.is_err());
    }

    #[test]
    fn refuses_unsafe_api_paths_without_running_the_cli() {
        let fixture = Fixture::new("transport-paths", "");
        let runner = fixture.runner(QUICK);
        for path in [
            "relative",
            "/api/../admin",
            "/api/x y",
            "/api/x\n",
            "/other",
        ] {
            let outcome = runner.api_get(path);
            assert!(
                matches!(outcome.result, Err(Error::Transport(ref text)) if text.contains("refused API path")),
                "{path:?}"
            );
        }
        assert!(fixture.invocations().is_empty());
    }

    #[test]
    fn refused_statements_start_no_cli_process() {
        let fixture = Fixture::new("transport-refused", "");
        let runner = fixture.runner(QUICK);
        for sql in [
            "DROP TABLE t",
            "SELECT 1; DROP TABLE t",
            "SELECT '--'; DROP TABLE t",
        ] {
            let outcome = runner.statement(sql, WAREHOUSE);
            assert_eq!(outcome.result, Err(Error::Refused), "{sql}");
            assert!(outcome.calls.is_empty());
        }
        assert!(fixture.invocations().is_empty());
    }

    #[test]
    fn submits_inline_json_array_results_that_outlast_the_wait() {
        // Spelled out rather than built with `submit_body`, so a change to
        // any field fails here.
        assert_eq!(
            submit_body("SELECT 'a\"b'", WAREHOUSE, WAIT_TIMEOUT),
            r#"{"disposition":"INLINE","format":"JSON_ARRAY","on_wait_timeout":"CONTINUE","statement":"SELECT 'a\"b'","wait_timeout":"30s","warehouse_id":"abc123"}"#
        );
    }

    #[test]
    fn a_submit_waits_no_longer_than_the_deadline_or_call_allows() {
        let secs = Duration::from_secs;
        for (call, deadline, wait) in [
            (secs(60), secs(300), secs(30)),
            (secs(10), secs(300), secs(5)),
            (secs(60), secs(7), secs(7)),
            (secs(60), Duration::from_millis(7_900), secs(7)),
            (secs(9), secs(300), Duration::ZERO),
            (secs(60), secs(4), Duration::ZERO),
            (secs(3), Duration::ZERO, Duration::ZERO),
        ] {
            let limits = Limits {
                call,
                deadline,
                poll: Duration::ZERO,
            };
            assert_eq!(limits.submit_wait(), wait, "{call:?} {deadline:?}");
        }
    }

    #[test]
    fn refuses_a_result_that_succeeded_after_the_deadline() {
        // The warehouse answers the submit late with a finished statement.
        let late = submit_waiting(Duration::ZERO, &succeeded(r#"{"data_array":[["1",null]]}"#))
            .replace("exit 0", "sleep 300\nexit 0");
        let scenario = [late, cancel_case()].concat();
        let fixture = Fixture::new("transport-late-success", &scenario);
        let runner = fixture.runner(QUICK).with_limits(Limits {
            call: Duration::from_secs(10),
            deadline: Duration::from_millis(100),
            poll: Duration::ZERO,
        });
        let outcome = runner.statement(SQL, WAREHOUSE);
        assert_eq!(
            outcome.result,
            Err(Error::Timeout {
                statement_id: ID.to_owned()
            })
        );
        assert_no_mutations(&fixture.invocations());
    }

    #[test]
    fn returns_rows_from_an_immediate_result() {
        let scenario = submit(&succeeded(r#"{"data_array":[["1",null]]}"#));
        let fixture = Fixture::new("transport-inline", &scenario);
        let outcome = fixture.runner(QUICK).statement(SQL, WAREHOUSE);
        assert_eq!(outcome.result, Ok(rows(&[&[Some("1"), None]])));
        assert_eq!(outcome.calls.len(), 1);
        let invocations = fixture.invocations();
        assert_eq!(invocations.len(), 1);
        assert_no_mutations(&invocations);
    }

    #[test]
    fn polls_a_pending_statement_until_it_succeeds() {
        let scenario = [
            submit(&state("PENDING")),
            get(&status_path(), &state("RUNNING")),
            get(&status_path(), &succeeded(r#"{"data_array":[["1",null]]}"#)),
        ]
        .concat();
        let fixture = Fixture::new("transport-poll", &scenario);
        let outcome = fixture.runner(QUICK).statement(SQL, WAREHOUSE);
        assert_eq!(outcome.result, Ok(rows(&[&[Some("1"), None]])));
        let invocations = fixture.invocations();
        assert_eq!(invocations.len(), 3);
        assert_eq!(invocations[1], invocations[2]);
        assert_no_mutations(&invocations);
    }

    #[test]
    fn reads_every_result_chunk_in_order() {
        let chunk = |index: u32| format!("{}/result/chunks/{index}", status_path());
        let scenario = [
            submit(&succeeded(
                r#"{"chunk_index":0,"data_array":[["1",null]],"next_chunk_index":1}"#,
            )),
            get(
                &chunk(1),
                r#"{"chunk_index":1,"data_array":[["2","b"]],"next_chunk_index":2}"#,
            ),
            get(&chunk(2), r#"{"chunk_index":2,"data_array":[["3",null]]}"#),
        ]
        .concat();
        let fixture = Fixture::new("transport-chunks", &scenario);
        let outcome = fixture.runner(QUICK).statement(SQL, WAREHOUSE);
        assert_eq!(
            outcome.result,
            Ok(rows(&[
                &[Some("1"), None],
                &[Some("2"), Some("b")],
                &[Some("3"), None]
            ]))
        );
        assert_eq!(fixture.invocations().len(), 3);
    }

    #[test]
    fn keeps_every_row_but_only_the_start_of_a_large_response() {
        let cell = "x".repeat(2 * STATEMENT_EVIDENCE_BYTES);
        let scenario = submit(&succeeded(&format!(
            r#"{{"data_array":[["{cell}",null]]}}"#
        )));
        let fixture = Fixture::new("transport-large", &scenario);
        let outcome = fixture.runner(QUICK).statement(SQL, WAREHOUSE);
        assert_eq!(outcome.result, Ok(rows(&[&[Some(&cell), None]])));
        let stdout = &outcome.calls[0].result.as_ref().expect("captured").stdout;
        assert!(
            stdout.len() < STATEMENT_EVIDENCE_BYTES + 64,
            "{}",
            stdout.len()
        );
        assert!(stdout.ends_with(b" more bytes not kept]\n"));
    }

    #[test]
    fn a_cut_response_drops_a_partial_token() {
        let token = fake_token();
        let captured = || Captured {
            stdout: format!("{{\"a\":\"{token}\"}}").into_bytes(),
            stderr: Vec::new(),
            status: ExitStatus::default(),
        };
        // Cut halfway through the token: none of it may remain.
        let kept = scrubbed(captured(), 6 + token.len() / 2);
        let text = String::from_utf8(kept.stdout).expect("UTF-8");
        assert!(text.starts_with("{\"a\":\"\n["), "{text}");
        assert!(!text.contains("dapi"), "{text}");
        assert_eq!(
            scrubbed(captured(), usize::MAX).stdout,
            br#"{"a":"[REDACTED]"}"#
        );
    }

    #[test]
    fn reports_failed_and_cancelled_statements_as_sql_errors() {
        let failed = format!(
            r#"{{"statement_id":"{ID}","status":{{"state":"FAILED","error":{{"error_code":"BAD_REQUEST","message":"[TABLE_OR_VIEW_NOT_FOUND] t"}}}}}}"#
        );
        for (name, body, expected) in [
            (
                "transport-failed",
                failed,
                Error::Sql {
                    state: "FAILED".to_owned(),
                    error_code: Some("BAD_REQUEST".to_owned()),
                    message: Some("[TABLE_OR_VIEW_NOT_FOUND] t".to_owned()),
                },
            ),
            (
                "transport-canceled",
                state("CANCELED"),
                Error::Sql {
                    state: "CANCELED".to_owned(),
                    error_code: None,
                    message: None,
                },
            ),
        ] {
            let fixture = Fixture::new(name, &submit(&body));
            let outcome = fixture.runner(QUICK).statement(SQL, WAREHOUSE);
            assert_eq!(outcome.result, Err(expected), "{name}");
        }
    }

    #[test]
    fn reports_a_rejected_submit_as_an_http_error() {
        let scenario = submit_case("", &fixture("api-debug-400.stderr"), 1);
        let fixture = Fixture::new("transport-submit-400", &scenario);
        let outcome = fixture.runner(QUICK).statement(SQL, WAREHOUSE);
        assert!(
            matches!(outcome.result, Err(Error::Http { status: 400, .. })),
            "{:?}",
            outcome.result
        );
    }

    #[test]
    fn cancels_a_statement_at_the_deadline() {
        let cancel = cancel_path();
        let scenario = [
            submit_waiting(Duration::ZERO, &state("PENDING")),
            cancel_case(),
        ]
        .concat();
        let fixture = Fixture::new("transport-timeout", &scenario);
        let outcome = fixture.runner(Duration::ZERO).statement(SQL, WAREHOUSE);
        assert_eq!(
            outcome.result,
            Err(Error::Timeout {
                statement_id: ID.to_owned()
            })
        );
        let invocations = fixture.invocations();
        assert_eq!(invocations.len(), 2);
        assert_eq!(invocations[1][2], cancel);
        assert_no_mutations(&invocations);
    }

    #[test]
    fn neither_the_poll_interval_nor_a_slow_poll_outlasts_the_deadline() {
        let cancel = cancel_path();
        let slow_poll =
            get(&status_path(), &state("RUNNING")).replace("exit 0", "sleep 20000\nexit 0");
        for (name, poll, scenario, polls) in [
            (
                "transport-long-interval",
                Duration::from_secs(20),
                [
                    submit_waiting(Duration::ZERO, &state("PENDING")),
                    cancel_case(),
                ]
                .concat(),
                0,
            ),
            (
                "transport-slow-poll",
                Duration::ZERO,
                [
                    submit_waiting(Duration::ZERO, &state("PENDING")),
                    slow_poll,
                    cancel_case(),
                ]
                .concat(),
                1,
            ),
        ] {
            let fixture = Fixture::new(name, &scenario);
            let runner = fixture.runner(QUICK).with_limits(Limits {
                call: Duration::from_secs(20),
                deadline: Duration::from_millis(500),
                poll,
            });
            let started = Instant::now();
            let outcome = runner.statement(SQL, WAREHOUSE);
            assert!(started.elapsed() < Duration::from_secs(10), "{name}");
            assert_eq!(
                outcome.result,
                Err(Error::Timeout {
                    statement_id: ID.to_owned()
                }),
                "{name}"
            );
            let invocations = fixture.invocations();
            assert_eq!(invocations.len(), 2 + polls, "{name}");
            assert_eq!(invocations.last().map(|args| &args[2]), Some(&cancel));
        }
    }

    #[test]
    fn cancels_a_statement_whose_poll_fails() {
        let unavailable = case(
            &["api", "get", &status_path(), "--debug"],
            "",
            &debug_log("get", &status_path(), "503 Service Unavailable"),
            1,
        );
        for (name, poll, expected) in [
            (
                "transport-poll-503",
                unavailable,
                Error::Http {
                    status: 503,
                    error_code: None,
                    message: None,
                },
            ),
            (
                "transport-poll-odd-state",
                get(&status_path(), &state("QUEUED")),
                Error::Transport(r#"the statement is in an unexpected state "QUEUED""#.to_owned()),
            ),
        ] {
            let scenario = [submit(&state("PENDING")), poll, cancel_case()].concat();
            let fixture = Fixture::new(name, &scenario);
            let outcome = fixture.runner(QUICK).statement(SQL, WAREHOUSE);
            assert_eq!(outcome.result, Err(expected), "{name}");
            let invocations = fixture.invocations();
            assert_eq!(invocations.len(), 3, "{name}");
            assert_eq!(invocations[2][2], cancel_path(), "{name}");
            assert_no_mutations(&invocations);
        }

        // A statement that ended on its own is not cancelled.
        let fixture = Fixture::new("transport-no-cancel", &submit(&state("FAILED")));
        let outcome = fixture.runner(QUICK).statement(SQL, WAREHOUSE);
        assert!(matches!(outcome.result, Err(Error::Sql { .. })));
        assert_eq!(fixture.invocations().len(), 1);
    }

    #[test]
    fn rejects_malformed_statement_responses() {
        let chunk1 = format!("{}/result/chunks/1", status_path());
        for (name, scenario, problem) in [
            (
                "transport-no-id",
                submit(r#"{"status":{"state":"SUCCEEDED"}}"#),
                "statement_id",
            ),
            (
                "transport-bad-id",
                submit(r#"{"statement_id":"../x","status":{"state":"SUCCEEDED"}}"#),
                "statement_id",
            ),
            (
                "transport-no-state",
                submit(&format!(r#"{{"statement_id":"{ID}"}}"#)),
                "status.state",
            ),
            (
                "transport-odd-state",
                submit(&state("QUEUED")),
                "unexpected state",
            ),
            (
                "transport-cell",
                submit(&succeeded(r#"{"data_array":[[1]]}"#)),
                "data_array",
            ),
            (
                "transport-columns",
                submit(&format!(
                    r#"{{"statement_id":"{ID}","status":{{"state":"SUCCEEDED"}},"manifest":{{"schema":{{"columns":{{}}}}}}}}"#
                )),
                "columns",
            ),
            (
                "transport-loop",
                [
                    submit(&succeeded(r#"{"data_array":[],"next_chunk_index":1}"#)),
                    get(&chunk1, r#"{"data_array":[],"next_chunk_index":1}"#),
                ]
                .concat(),
                "does not follow",
            ),
            (
                "transport-unnumbered-self-loop",
                submit(&succeeded(
                    r#"{"data_array":[["1",null]],"next_chunk_index":0}"#,
                )),
                "does not follow",
            ),
            (
                "transport-self-loop",
                submit(&succeeded(
                    r#"{"chunk_index":0,"data_array":[["1",null]],"next_chunk_index":0}"#,
                )),
                "does not follow",
            ),
            (
                "transport-no-body",
                [submit(&state("PENDING")), get(&status_path(), "")].concat(),
                "no response body",
            ),
        ] {
            let fixture = Fixture::new(name, &scenario);
            let error = fixture
                .runner(QUICK)
                .statement(SQL, WAREHOUSE)
                .result
                .expect_err(name);
            assert!(
                matches!(&error, Error::Transport(text) if text.contains(problem)),
                "{name}: {error:?}"
            );
        }
    }

    #[test]
    fn checks_chunks_against_the_request_and_the_manifest() {
        let chunk1 = format!("{}/result/chunks/1", status_path());
        let with_totals = |chunks: u64, rows: u64, result: &str| {
            succeeded(result).replace(
                r#""manifest":{"#,
                &format!(r#""manifest":{{"total_chunk_count":{chunks},"total_row_count":{rows},"#),
            )
        };
        let two_chunks = r#"{"chunk_index":0,"data_array":[["1",null]],"next_chunk_index":1}"#;
        let ok = [
            submit(&with_totals(2, 2, two_chunks)),
            get(&chunk1, r#"{"chunk_index":1,"data_array":[["2",null]]}"#),
        ]
        .concat();
        let fixture = Fixture::new("transport-totals", &ok);
        let outcome = fixture.runner(QUICK).statement(SQL, WAREHOUSE);
        assert_eq!(
            outcome.result,
            Ok(rows(&[&[Some("1"), None], &[Some("2"), None]]))
        );
        for (name, scenario, problem) in [
            (
                "transport-wrong-chunk",
                [
                    submit(&succeeded(two_chunks)),
                    get(&chunk1, r#"{"chunk_index":2,"data_array":[]}"#),
                ]
                .concat(),
                "chunk 2 was returned for chunk 1",
            ),
            (
                "transport-row-total",
                submit(&with_totals(1, 5, r#"{"data_array":[["1",null]]}"#)),
                "`manifest.total_row_count` is 5",
            ),
            (
                "transport-chunk-total",
                submit(&with_totals(3, 1, r#"{"data_array":[["1",null]]}"#)),
                "`manifest.total_chunk_count` is 3",
            ),
        ] {
            let fixture = Fixture::new(name, &scenario);
            let error = fixture
                .runner(QUICK)
                .statement(SQL, WAREHOUSE)
                .result
                .expect_err(name);
            assert!(
                matches!(&error, Error::Transport(text) if text.contains(problem)),
                "{name}: {error:?}"
            );
        }
    }

    #[test]
    fn run_refuses_commands_that_are_not_known_reads() {
        let fixture = Fixture::new("transport-run-refused", "");
        let runner = fixture.runner(QUICK);
        for args in [
            &["bundle", "deploy"][..],
            &["bundle", "validate", "extra"],
            &["--profile", "x", "bundle", "validate"],
            &["api", "get", "/api/x"],
        ] {
            let call = runner.run(args, QUICK);
            assert!(
                call.result
                    .as_ref()
                    .is_err_and(|problem| problem.contains("not a known read-only command")),
                "{args:?}"
            );
        }
        assert!(fixture.invocations().is_empty());
    }

    #[test]
    fn refuses_a_truncated_result() {
        let body = format!(
            r#"{{"statement_id":"{ID}","status":{{"state":"SUCCEEDED"}},"manifest":{{"truncated":true,"schema":{{"columns":[{{"name":"one"}}]}}}},"result":{{"data_array":[["1"]]}}}}"#
        );
        let fixture = Fixture::new("transport-truncated", &submit(&body));
        let result = fixture.runner(QUICK).statement(SQL, WAREHOUSE).result;
        assert_eq!(
            result,
            Err(Error::Truncated {
                statement_id: ID.to_owned()
            })
        );
        let complete = body.replace(r#""truncated":true"#, r#""truncated":false"#);
        let fixture = Fixture::new("transport-not-truncated", &submit(&complete));
        let result = fixture.runner(QUICK).statement(SQL, WAREHOUSE).result;
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn a_statement_without_a_result_set_has_no_rows() {
        let body = format!(r#"{{"statement_id":"{ID}","status":{{"state":"SUCCEEDED"}}}}"#);
        let fixture = Fixture::new("transport-no-result", &submit(&body));
        let result = fixture.runner(QUICK).statement(SQL, WAREHOUSE).result;
        assert_eq!(
            result,
            Ok(ResultSet {
                statement_id: ID.to_owned(),
                columns: Vec::new(),
                rows: Vec::new(),
            })
        );
    }

    #[test]
    fn scrubs_credentials_and_leaves_other_output_alone() {
        let token = fake_token();
        for (input, expected) in [
            (
                format!("> \"authorization: Bearer {token}\"\n"),
                "> \"authorization: [REDACTED]\n".to_owned(),
            ),
            (
                format!("token=Bearer {token} rest"),
                "token=Bearer [REDACTED] rest".to_owned(),
            ),
            (
                format!("{token}-12 and {token}"),
                "[REDACTED] and [REDACTED]".to_owned(),
            ),
            (
                "Bearer [REDACTED] stays".to_owned(),
                "Bearer [REDACTED] stays".to_owned(),
            ),
            (
                format!("a: Bearer  {token} b: BEARER\t\t{token}"),
                "a: Bearer  [REDACTED] b: BEARER\t\t[REDACTED]".to_owned(),
            ),
            (
                "bearers are people; trailing Bearer ".to_owned(),
                "bearers are people; trailing Bearer ".to_owned(),
            ),
            (
                "dapi is short: dapi1234".to_owned(),
                "dapi is short: dapi1234".to_owned(),
            ),
        ] {
            assert_eq!(
                String::from_utf8(scrub(input.into_bytes())).expect("UTF-8"),
                expected
            );
        }
        let binary = b"plain \xff output\n";
        assert_eq!(scrub(binary.to_vec()), binary);
        // Output with nothing to scrub is handed back, not copied.
        let plain = b"nothing to see\n".to_vec();
        let address = plain.as_ptr();
        let returned = scrub(plain);
        assert_eq!(returned.as_ptr(), address);
    }

    #[test]
    fn reads_the_error_body_before_any_later_output() {
        let code = |stderr: &str| {
            error_body(stderr)
                .and_then(|body| body.root().get("error_code")?.as_str().map(str::to_owned))
        };
        let pretty = "< HTTP/1.1 404 Not Found\n{\n  \"error_code\": \"NOT_FOUND\",\n  \"detail\": {\n    \"x\": 1\n  }\n}\nError: NOT_FOUND\n";
        assert_eq!(code(pretty).as_deref(), Some("NOT_FOUND"));
        let compact = "{not json}\n{\"error_code\":\"BAD\"}\nError: BAD\n";
        assert_eq!(code(compact).as_deref(), Some("BAD"));
        assert_eq!(code("{\n  \"unterminated\": 1\n"), None);
        assert_eq!(code("[1]\n"), None);
    }

    /// `databricks api get --debug` for a 404, as CLI 1.13.0 and 1.21.0 log
    /// it: the error body is part of the logged response.
    const LOGGED_404: &str = "09:56:51 Info: start pid=42698 version=1.13.0 args=\"databricks, api, get, /api/2.0/pipelines/missing, --debug\"\n09:56:51 Debug: GET /.well-known/databricks-config\n< HTTP/1.0 200 OK\n< {\n<   \"error_code\": \"NOT_THIS_ONE\"\n< } pid=42698 sdk=true\n09:56:51 Debug: GET /api/2.0/pipelines/missing\n< HTTP/1.0 404 Not Found\n< {\n<   \"error_code\": \"RESOURCE_NOT_FOUND\",\n<   \"message\": \"Pipeline not found\"\n< } pid=42698 sdk=true\n09:56:51 Debug: non-retriable error: Pipeline not found pid=42698 sdk=true\nError: Pipeline not found\n09:56:51 Info: failed execution pid=42698 exit_code=1 error=\"Pipeline not found\"\n";

    #[test]
    fn reads_the_error_body_the_cli_logs_with_the_response() {
        assert_eq!(http_status(LOGGED_404), Some(404));
        let body = error_body(LOGGED_404).expect("logged error body");
        assert_eq!(
            body.root().get("error_code").and_then(Node::as_str),
            Some("RESOURCE_NOT_FOUND")
        );
        let compact = "Debug: POST /api/x\n< HTTP/1.1 400 Bad Request\n< {\"error_code\":\"BAD\",\"message\":\"m\"} pid=1 sdk=true\nError: m\n";
        let code = error_body(compact)
            .and_then(|body| body.root().get("error_code")?.as_str().map(str::to_owned));
        assert_eq!(code.as_deref(), Some("BAD"));
        let text = "Debug: GET /api/x\n< HTTP/1.1 502 Bad Gateway\n< <html>bad gateway</html> pid=1 sdk=true\n";
        assert!(error_body(text).is_none());
    }

    #[test]
    fn api_get_reads_the_error_body_from_a_real_debug_log() {
        let scenario = case(&["api", "get", "/api/x", "--debug"], "", LOGGED_404, 1);
        let fixture = Fixture::new("transport-logged-404", &scenario);
        let outcome = fixture.runner(QUICK).api_get("/api/x");
        assert_eq!(
            outcome.result.expect_err("404"),
            Error::Http {
                status: 404,
                error_code: Some("RESOURCE_NOT_FOUND".to_owned()),
                message: Some("Pipeline not found".to_owned()),
            }
        );
    }

    #[test]
    fn reads_the_last_http_status_line() {
        // As the CLI logs it: each exchange starts with a request line.
        let redirected = "09:56:51 Debug: GET /api/x\n< HTTP/1.1 307 Temporary Redirect\n< Location: /api/y pid=1 sdk=true\n09:56:51 Debug: GET /api/y\n< HTTP/2.0 200 OK\n< {} pid=1 sdk=true\n";
        assert_eq!(http_status(redirected), Some(200));
        let timestamped = "2024/10/23 10:30:45 < HTTP/1.1 404 Not Found\n2024/10/23 10:30:45 < Content-Type: application/json\n";
        assert_eq!(http_status(timestamped), Some(404));
        assert_eq!(http_status("GET /x HTTP/1.1\n"), None);
        assert_eq!(http_status("< HTTP/1.1 abc\n"), None);
        assert_eq!(http_status("Debug: x < HTTP/1.1 500 x\n"), None);
    }

    #[test]
    fn a_body_line_that_looks_like_a_status_line_is_part_of_the_body() {
        // A text body as the SDK logs it: every body line starts with `< `.
        let stderr = "09:56:51 Debug: GET /api/2.0/x\n< HTTP/1.0 200 OK\n< first line\n< HTTP/1.1 500 x\n< last line pid=1 sdk=true\n09:56:51 Info: completed execution pid=1 exit_code=0\n";
        assert_eq!(http_status(stderr), Some(200));
    }

    #[test]
    fn describes_errors_without_details() {
        let http = Error::Http {
            status: 404,
            error_code: Some("RESOURCE_NOT_FOUND".to_owned()),
            message: Some("secret path".to_owned()),
        };
        assert_eq!(http.to_string(), "HTTP 404 RESOURCE_NOT_FOUND");
        let sql = Error::Sql {
            state: "FAILED".to_owned(),
            error_code: None,
            message: None,
        };
        assert_eq!(sql.to_string(), "statement FAILED");
        assert_eq!(
            Error::Timeout {
                statement_id: ID.to_owned()
            }
            .to_string(),
            format!("statement {ID} timed out and was cancelled")
        );
        assert_eq!(
            Error::Truncated {
                statement_id: ID.to_owned()
            }
            .to_string(),
            format!("statement {ID} returned a truncated result")
        );
        assert_eq!(
            Error::Refused.to_string(),
            "refused a statement that is not read-only"
        );
    }
}

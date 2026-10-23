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

/// A statement as one submit left it, before any poll.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Submitted {
    pub(crate) statement_id: String,
    /// `status.state` from the submit response, such as `SUCCEEDED` or
    /// `PENDING`.
    pub(crate) state: String,
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
    /// The statement had not finished, or its result had not been read, by
    /// the deadline. A statement still running was sent a cancel.
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

/// Error classes that name a missing table, view, schema, or catalog. A
/// failed statement carries its class in `error_code` or, as `[CLASS]`, at the
/// start of its message.
const NOT_FOUND_CLASSES: [&str; 4] = [
    "TABLE_OR_VIEW_NOT_FOUND",
    "SCHEMA_NOT_FOUND",
    "CATALOG_NOT_FOUND",
    "RESOURCE_DOES_NOT_EXIST",
];
/// Error classes that name missing privileges.
const DENIED_CLASSES: [&str; 2] = ["PERMISSION_DENIED", "INSUFFICIENT_PERMISSIONS"];

impl Error {
    /// Whether the workspace said that what was asked for does not exist: an
    /// HTTP 404, or an error whose class names a missing object.
    pub(crate) fn is_not_found(&self) -> bool {
        matches!(self, Self::Http { status: 404, .. }) || self.has_class(&NOT_FOUND_CLASSES)
    }

    /// Whether the workspace refused the caller: an HTTP 403, or an error
    /// whose class names missing privileges.
    pub(crate) fn is_permission_denied(&self) -> bool {
        matches!(self, Self::Http { status: 403, .. }) || self.has_class(&DENIED_CLASSES)
    }

    fn has_class(&self, classes: &[&str]) -> bool {
        let (Self::Http {
            error_code,
            message,
            ..
        }
        | Self::Sql {
            error_code,
            message,
            ..
        }) = self
        else {
            return false;
        };
        classes.iter().any(|class| {
            error_code.as_deref() == Some(*class)
                || message
                    .as_deref()
                    .is_some_and(|message| message.starts_with(&format!("[{class}]")))
        })
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

    /// Submits a read-only statement once with `wait` as its `wait_timeout`,
    /// and returns the statement as the submit left it, without polling or
    /// reading rows. One still pending or running is cancelled. A
    /// `wait_timeout` the API rejects comes back as an HTTP error.
    fn submit(&self, sql: &str, warehouse_id: &str, wait: Duration) -> Outcome<Submitted>;
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
        let mut args = vec!["api", method, path];
        if let Some(body) = body {
            args.extend(["--json", body]);
        }
        args.push("--debug");
        if let Err(problem) = check_path(path) {
            // Recorded as the run it would have been, without starting it.
            let call = Call {
                args: self.arguments(&args),
                result: Err(problem.clone()),
            };
            return (call, Err(Error::Transport(problem)));
        }
        let (args, result) = self.capture(&args, timeout);
        let response = response(Request { method, path }, &result);
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
        let statement_id = statement_id(&body)?;
        let status_path = format!("{STATEMENTS}/{statement_id}");
        match self.wait(body, started, &statement_id, &status_path, calls) {
            Ok(body) => self.result_set(&body, started, statement_id, &status_path, calls),
            Err(Stopped { error, running }) => {
                // A statement that may still be running is cancelled rather
                // than left on the warehouse; one that ended is not.
                if running {
                    self.cancel(&status_path, calls);
                }
                Err(error)
            }
        }
    }

    fn submit_once(
        &self,
        sql: &str,
        warehouse_id: &str,
        wait: Duration,
        calls: &mut Vec<Call>,
    ) -> Result<Submitted, Error> {
        let submit = submit_body(sql, warehouse_id, wait);
        // The CLI run must outlast the wait, so the warehouse can answer.
        let timeout = self.limits.call.max(wait + SUBMIT_MARGIN);
        let body = self.statement_call("post", STATEMENTS, Some(&submit), timeout, calls)?;
        let statement_id = statement_id(&body)?;
        let state = body
            .root()
            .pointer("/status/state")
            .and_then(Node::as_str)
            .unwrap_or_default()
            .to_owned();
        // Only the submit was asked for, so nothing is left running.
        if matches!(state.as_str(), "PENDING" | "RUNNING") {
            self.cancel(&format!("{STATEMENTS}/{statement_id}"), calls);
        }
        Ok(Submitted {
            statement_id,
            state,
        })
    }

    /// Polls a submitted statement until it succeeds and returns the final
    /// response. A success after the deadline is a timeout, because the
    /// deadline bounds the whole statement, but the statement has ended, so
    /// it is not cancelled.
    fn wait(
        &self,
        mut body: Document,
        started: Instant,
        statement_id: &str,
        status_path: &str,
        calls: &mut Vec<Call>,
    ) -> Result<Document, Stopped> {
        let ended = |error| Stopped {
            error,
            running: false,
        };
        let running = |error| Stopped {
            error,
            running: true,
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
                    return Err(ended(Error::Timeout {
                        statement_id: statement_id.to_owned(),
                    }));
                }
                "SUCCEEDED" => return Ok(body),
                "PENDING" | "RUNNING" => {
                    // Neither the sleep nor the poll may outlast the deadline.
                    thread::sleep(self.limits.poll.min(self.remaining(started)));
                    body = self
                        .bounded_get(status_path, started, statement_id, calls)
                        .map_err(running)?;
                }
                "FAILED" | "CANCELED" | "CLOSED" => {
                    let error = body.root().pointer("/status/error");
                    let text = |key| {
                        error
                            .and_then(|error| error.get(key))
                            .and_then(Node::as_str)
                            .map(str::to_owned)
                    };
                    return Err(ended(Error::Sql {
                        state,
                        error_code: text("error_code"),
                        message: text("message"),
                    }));
                }
                "" => {
                    return Err(running(Error::Transport(
                        "the statement response has no `status.state`".to_owned(),
                    )));
                }
                other => {
                    return Err(running(Error::Transport(format!(
                        "the statement is in an unexpected state {other:?}"
                    ))));
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
        started: Instant,
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
        // An inline result is a chunk when it is numbered or holds rows; an
        // empty result set comes back with an empty one and no chunks.
        let inline = chunk.filter(|result| {
            result.get("chunk_index").is_some()
                || result
                    .get("data_array")
                    .and_then(Node::elements)
                    .is_some_and(|mut rows| rows.next().is_some())
        });
        let mut chunks = u64::from(inline.is_some());
        let mut owned: Document;
        // The inline chunk counts too, so it cannot point back at itself. It
        // is the first chunk when the response does not number it.
        let mut last_index = inline.map(|result| {
            result
                .get("chunk_index")
                .and_then(Node::as_u64)
                .unwrap_or(0)
        });
        loop {
            let next = match chunk {
                Some(result) => {
                    append_rows(result, columns.len(), &mut rows)?;
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
            owned = self.bounded_get(&path, started, &statement_id, calls)?;
            // A fetched chunk must say which it is; only the inline one may not.
            match owned.root().get("chunk_index").and_then(Node::as_u64) {
                Some(returned) if returned == index => {}
                Some(returned) => {
                    return Err(Error::Transport(format!(
                        "result chunk {returned} was returned for chunk {index}"
                    )));
                }
                None => {
                    return Err(Error::Transport(format!(
                        "result chunk {index} came back without a `chunk_index`"
                    )));
                }
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

    /// The time left before the deadline of a statement submitted at
    /// `started`.
    fn remaining(&self, started: Instant) -> Duration {
        self.limits.deadline.saturating_sub(started.elapsed())
    }

    /// A GET during a statement, stopped at the deadline: a poll or a
    /// result chunk. It is a timeout when no time was left to start it or
    /// the run was stopped at the deadline; a run that finished keeps its
    /// own error, such as an HTTP 403, even past the deadline.
    fn bounded_get(
        &self,
        path: &str,
        started: Instant,
        statement_id: &str,
        calls: &mut Vec<Call>,
    ) -> Result<Document, Error> {
        let timeout = || Error::Timeout {
            statement_id: statement_id.to_owned(),
        };
        if self.remaining(started).is_zero() {
            return Err(timeout());
        }
        let limit = self.limits.call.min(self.remaining(started));
        match self.statement_call("get", path, None, limit, calls) {
            Err(_) if stopped_at_deadline(calls.last(), self.remaining(started)) => Err(timeout()),
            result => result,
        }
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
        // Anything but a known read-only command is refused without
        // starting a process.
        if !safety::READ_ONLY_COMMANDS.contains(&path.as_slice()) {
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

    fn submit(&self, sql: &str, warehouse_id: &str, wait: Duration) -> Outcome<Submitted> {
        if !safety::is_read_statement(sql) {
            return Outcome {
                calls: Vec::new(),
                result: Err(Error::Refused),
            };
        }
        let mut calls = Vec::new();
        let result = self.submit_once(sql, warehouse_id, wait, &mut calls);
        Outcome { calls, result }
    }
}

/// Why a statement stopped without a result, and whether it may still be
/// running on the warehouse.
struct Stopped {
    error: Error,
    running: bool,
}

/// Whether the last run was stopped at the deadline rather than finished.
fn stopped_at_deadline(call: Option<&Call>, remaining: Duration) -> bool {
    remaining.is_zero() && call.is_some_and(|call| call.result.is_err())
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

/// The statement ID in a submit response, which must be UUID-like.
fn statement_id(body: &Document) -> Result<String, Error> {
    body.root()
        .get("statement_id")
        .and_then(Node::as_str)
        .filter(|id| is_statement_id(id))
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::Transport("the statement response has no valid `statement_id`".to_owned())
        })
}

/// Statement IDs are UUID-like: letters, digits, and `-`.
fn is_statement_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Classifies one `databricks api` run of `request`.
fn response(request: Request<'_>, result: &Result<Captured, String>) -> Result<Response, Error> {
    let method = request.method;
    let Ok(captured) = result else {
        return Err(Error::Transport(format!(
            "`databricks api {method}` could not be run to completion"
        )));
    };
    let stderr = String::from_utf8_lossy(&captured.stderr);
    let logged = logged_response(&stderr, Some(request));
    let status = logged.as_ref().map(|logged| logged.status);
    if let Some(status) = status.filter(|&status| status >= 400) {
        let error = logged
            .as_ref()
            .and_then(|logged| logged_object(&logged.lines))
            .or_else(|| first_object(&stderr));
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

/// A logged response: its status and the lines logged with it, without
/// their `<` prefix.
struct Logged<'a> {
    status: u16,
    lines: Vec<&'a str>,
}

/// The request a `databricks api` run made.
#[derive(Clone, Copy)]
struct Request<'a> {
    method: &'a str,
    path: &'a str,
}

/// What a debug log records, in order.
enum Entry<'a> {
    Request(&'a str, &'a str),
    Response(Logged<'a>),
}

/// The response to `request` in a debug log, or to the last request when
/// `request` is `None` or the log names no request.
///
/// The SDK logs each request as a line such as `Debug: GET /api/2.0/x`, and
/// each response as a `< HTTP/<version> <code>` line, after an optional
/// timestamp, followed by its headers and body on lines that also start with
/// `<`. Those lines belong to the response, so text in a body that looks
/// like a status line cannot be taken for one. The response to `request` is
/// the last one after its last request line: a retry logs the request again,
/// and a redirect is followed to the request it leads to. A later request
/// for something else, such as a token refresh, is not this call's.
fn logged_response<'a>(stderr: &'a str, request: Option<Request<'_>>) -> Option<Logged<'a>> {
    let mut log = Vec::new();
    let mut in_response = false;
    for line in stderr.lines() {
        let content = without_timestamp(line);
        if in_response && let Some(rest) = content.strip_prefix('<') {
            if let Some(Entry::Response(last)) = log.last_mut() {
                last.lines.push(rest.strip_prefix(' ').unwrap_or(rest));
            }
            continue;
        }
        in_response = false;
        if let Some(status) = status_code(content) {
            log.push(Entry::Response(Logged {
                status,
                lines: Vec::new(),
            }));
            in_response = true;
        } else if let Some((method, path)) = request_line(content) {
            log.push(Entry::Request(method, path));
        }
    }
    let ours = request.and_then(|request| {
        let path = without_query(request.path);
        log.iter().rposition(|entry| match entry {
            Entry::Request(method, logged) => {
                method.eq_ignore_ascii_case(request.method) && *logged == path
            }
            Entry::Response(_) => false,
        })
    });
    let Some(start) = ours else {
        return log.into_iter().rev().find_map(|entry| match entry {
            Entry::Response(logged) => Some(logged),
            Entry::Request(..) => None,
        });
    };
    let mut answer: Option<Logged<'a>> = None;
    for entry in log.into_iter().skip(start + 1) {
        match entry {
            Entry::Response(logged) => answer = Some(logged),
            // A redirect leads to the next request; anything else is not
            // this call's.
            Entry::Request(..)
                if answer
                    .as_ref()
                    .is_some_and(|answer| (300..400).contains(&answer.status)) => {}
            Entry::Request(..) => break,
        }
    }
    answer
}

/// The method and path, without a query, of a request line such as
/// `Debug: GET /api/2.0/x` or `GET /api/2.0/x HTTP/1.1`.
fn request_line(content: &str) -> Option<(&str, &str)> {
    let content = content.strip_prefix("Debug: ").unwrap_or(content);
    let (method, rest) = content.split_once(' ')?;
    if !matches!(method, "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD") {
        return None;
    }
    let target = rest.split_whitespace().next()?;
    target
        .starts_with('/')
        .then(|| (method, without_query(target)))
}

fn without_query(path: &str) -> &str {
    path.split_once('?').map_or(path, |(path, _)| path)
}

/// The status of the last response in a debug log.
#[cfg(test)]
fn http_status(stderr: &str) -> Option<u16> {
    logged_response(stderr, None).map(|logged| logged.status)
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

/// The JSON error body of the last failed request in a debug log.
#[cfg(test)]
fn error_body(stderr: &str) -> Option<Document> {
    logged_response(stderr, None)
        .and_then(|logged| logged_object(&logged.lines))
        .or_else(|| first_object(stderr))
}

/// The JSON object in a logged response: from its first line that starts
/// with `{` to the last `}`, which drops the log fields after it. The CLI
/// logs an error body this way, as `< {` through `< }` followed by
/// ` pid=… sdk=true`.
fn logged_object(lines: &[&str]) -> Option<Document> {
    let start = lines.iter().position(|line| line.starts_with('{'))?;
    let text = lines[start..].join("\n");
    let end = text.rfind('}')?;
    json::parse(&text[..=end])
        .ok()
        .filter(|document| document.root().kind() == Kind::Object)
}

/// The first object that starts a line of the log, when no logged response
/// holds one: it ends on that line when it closes there, or else on the
/// next line that starts with `}`. Output after it is ignored, and each line
/// is read once.
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

/// Appends the rows of one result chunk, each of which must have one cell
/// per column. A chunk without `data_array` has no rows.
fn append_rows(
    chunk: Node<'_>,
    width: usize,
    rows: &mut Vec<Vec<Option<String>>>,
) -> Result<(), Error> {
    let Some(data) = chunk.get("data_array") else {
        return Ok(());
    };
    let unexpected = || Error::Transport("`data_array` is not an array of rows of text".to_owned());
    for row in data.elements().ok_or_else(unexpected)? {
        let cells = row.elements().ok_or_else(unexpected)?;
        if cells.len() != width {
            return Err(Error::Transport(format!(
                "a result row has {} cells for {width} columns",
                cells.len()
            )));
        }
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
/// `Authorization` header, a bearer token, the value of an OAuth
/// `access_token`, `refresh_token`, `id_token`, or `client_secret` field, a
/// JSON Web Token, and a personal access token (`dapi` and 32 hex digits).
/// Only the credential itself is replaced, so the rest of a line is kept.
/// Output without any is returned as is.
fn scrub(output: Vec<u8>) -> Vec<u8> {
    let changed = {
        let text = String::from_utf8_lossy(&output);
        let mut scrubbed = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            let mut kept = 0;
            for (start, end) in secret_spans(line) {
                scrubbed.push_str(&line[kept..start]);
                scrubbed.push_str(REDACTED);
                kept = end;
            }
            scrubbed.push_str(&line[kept..]);
        }
        (scrubbed != text).then_some(scrubbed)
    };
    changed.map_or(output, String::into_bytes)
}

/// The byte ranges of the credentials in `line`, sorted and merged. A range
/// that already reads `[REDACTED]` is left out.
fn secret_spans(line: &str) -> Vec<(usize, usize)> {
    // Lowercased once: ASCII case changes keep every byte offset.
    let lower = line.to_ascii_lowercase();
    let mut spans = Vec::new();
    header_values(line, &lower, &mut spans);
    bearer_tokens(line, &lower, &mut spans);
    secret_fields(line, &lower, &mut spans);
    access_tokens(line, &mut spans);
    web_tokens(line, &mut spans);
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged.retain(|&(start, end)| &line[start..end] != REDACTED);
    merged
}

/// Every place `needle`, which must be ASCII lowercase, occurs in `lower`.
fn occurrences<'a>(lower: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    lower.match_indices(needle).map(|(at, _)| at)
}

/// The value of each `Authorization:` header: from after the colon to the
/// end of the header, which a quote, comma, backslash, or line end closes.
fn header_values(line: &str, lower: &str, spans: &mut Vec<(usize, usize)>) {
    for at in occurrences(lower, "authorization:") {
        let after = at + "authorization:".len();
        let start =
            after + line[after..].len() - line[after..].trim_start_matches([' ', '\t']).len();
        let end = line[start..]
            .find(['"', '\'', ',', '\\', '\r', '\n'])
            .map_or(line.len(), |offset| start + offset);
        let end = start + line[start..end].trim_end().len();
        if end > start {
            spans.push((start, end));
        }
    }
}

/// The token after each `Bearer` followed by spaces or tabs, up to the next
/// whitespace or quote.
fn bearer_tokens(line: &str, lower: &str, spans: &mut Vec<(usize, usize)>) {
    for at in occurrences(lower, "bearer") {
        let after = at + "bearer".len();
        let rest = &line[after..];
        let start = after + rest.len() - rest.trim_start_matches([' ', '\t']).len();
        if start == after {
            continue;
        }
        let end = line[start..]
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .map_or(line.len(), |offset| start + offset);
        if end > start {
            spans.push((start, end));
        }
    }
}

/// The string value of each OAuth secret field, as in
/// `"access_token": "<value>"`.
fn secret_fields(line: &str, lower: &str, spans: &mut Vec<(usize, usize)>) {
    for name in [
        "\"access_token\"",
        "\"refresh_token\"",
        "\"id_token\"",
        "\"client_secret\"",
    ] {
        for at in occurrences(lower, name) {
            let rest = line[at + name.len()..].trim_start();
            let Some(rest) = rest.strip_prefix(':').map(str::trim_start) else {
                continue;
            };
            let Some(value) = rest.strip_prefix('"') else {
                continue;
            };
            let start = line.len() - value.len();
            let mut end = None;
            let mut escaped = false;
            for (offset, byte) in value.bytes().enumerate() {
                match byte {
                    _ if escaped => escaped = false,
                    b'\\' => escaped = true,
                    b'"' => {
                        end = Some(start + offset);
                        break;
                    }
                    _ => {}
                }
            }
            let end = end.unwrap_or(line.len());
            if end > start {
                spans.push((start, end));
            }
        }
    }
}

/// Personal access tokens: `dapi` followed by 32 hex digits, with an
/// optional `-<digits>` suffix.
fn access_tokens(line: &str, spans: &mut Vec<(usize, usize)>) {
    for (at, _) in line.match_indices("dapi") {
        let candidate = &line.as_bytes()[at + 4..];
        let hex = candidate
            .iter()
            .take_while(|byte| byte.is_ascii_hexdigit())
            .count();
        if hex < 32 {
            continue;
        }
        let mut end = at + 4 + hex;
        if line.as_bytes().get(end) == Some(&b'-') {
            let digits = line.as_bytes()[end + 1..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            if digits > 0 {
                end += 1 + digits;
            }
        }
        spans.push((at, end));
    }
}

/// JSON Web Tokens, such as OAuth access tokens: three or more base64url
/// segments joined by dots, the first starting `eyJ` (an encoded `{"`).
fn web_tokens(line: &str, spans: &mut Vec<(usize, usize)>) {
    let is_jwt_byte =
        |byte: &u8| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.');
    for (at, _) in line.match_indices("eyJ") {
        // Only where a token can start, not inside another word.
        if at > 0 && is_jwt_byte(&line.as_bytes()[at - 1]) {
            continue;
        }
        let length = line.as_bytes()[at..]
            .iter()
            .take_while(|byte| is_jwt_byte(byte))
            .count();
        let token = &line[at..at + length];
        let segments = token.split('.').collect::<Vec<_>>();
        if segments.len() >= 3 && segments[..3].iter().all(|segment| segment.len() >= 4) {
            spans.push((at, at + length));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use std::process::ExitStatus;

    use super::{
        Call, CliRunner, Error, Limits, Request, ResultSet, Runner, STATEMENT_EVIDENCE_BYTES,
        STATEMENTS, Submitted, WAIT_TIMEOUT, error_body, http_status, logged_object,
        logged_response, scrub, scrubbed, stopped_at_deadline, submit_body,
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
            // The evidence names what was refused.
            assert_eq!(
                outcome.calls[0].args,
                ["api", "get", path, "--debug", "-t", "dev"],
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
    fn submit_returns_the_statement_as_the_submit_left_it() {
        let scenario = submit_waiting(Duration::from_secs(50), &state("SUCCEEDED"));
        let fixture = Fixture::new("transport-submit", &scenario);
        let outcome = fixture
            .runner(QUICK)
            .submit(SQL, WAREHOUSE, Duration::from_secs(50));
        assert_eq!(
            outcome.result,
            Ok(Submitted {
                statement_id: ID.to_owned(),
                state: "SUCCEEDED".to_owned(),
            })
        );
        // A finished statement is neither polled nor cancelled.
        let invocations = fixture.invocations();
        assert_eq!(invocations.len(), 1);
        assert_no_mutations(&invocations);
    }

    #[test]
    fn submit_cancels_a_statement_it_leaves_running() {
        for pending in ["PENDING", "RUNNING"] {
            let scenario = [
                submit_waiting(Duration::ZERO, &state(pending)),
                cancel_case(),
            ]
            .concat();
            let fixture = Fixture::new(&format!("transport-submit-{pending}"), &scenario);
            let outcome = fixture.runner(QUICK).submit(SQL, WAREHOUSE, Duration::ZERO);
            assert_eq!(
                outcome.result.map(|submitted| submitted.state),
                Ok(pending.to_owned())
            );
            let invocations = fixture.invocations();
            assert_eq!(invocations.len(), 2, "{pending}");
            assert_eq!(invocations[1][2], cancel_path());
            assert_no_mutations(&invocations);
        }
    }

    #[test]
    fn submit_reports_a_rejected_wait_as_an_http_error() {
        let wait = Duration::from_secs(51);
        let rejected = submit_case_waiting(
            wait,
            "",
            &format!(
                "{}< {{\n<   \"error_code\": \"INVALID_PARAMETER_VALUE\"\n< }}\n",
                debug_log("post", STATEMENTS, "400 Bad Request")
            ),
            1,
        );
        let fixture = Fixture::new("transport-submit-rejected", &rejected);
        let outcome = fixture.runner(QUICK).submit(SQL, WAREHOUSE, wait);
        assert_eq!(
            outcome.result,
            Err(Error::Http {
                status: 400,
                error_code: Some("INVALID_PARAMETER_VALUE".to_owned()),
                message: None,
            })
        );
        assert_eq!(outcome.calls.len(), 1);
    }

    #[test]
    fn submit_refuses_statements_that_are_not_read_only() {
        let fixture = Fixture::new("transport-submit-refused", "");
        let outcome = fixture
            .runner(QUICK)
            .submit("DROP TABLE t", WAREHOUSE, Duration::ZERO);
        assert_eq!(outcome.result, Err(Error::Refused));
        assert!(outcome.calls.is_empty());
        assert!(fixture.invocations().is_empty());
    }

    #[test]
    fn submit_lets_the_cli_outlast_its_wait() {
        // A one-second call could not hold a five-second wait, so the run
        // gets the wait plus the submit margin; the fake answers after 1.5s.
        let wait = Duration::from_secs(5);
        let slow =
            submit_waiting(wait, &state("SUCCEEDED")).replace("exit 0", "sleep 1500\nexit 0");
        let fixture = Fixture::new("transport-submit-slow", &slow);
        let runner =
            CliRunner::for_bundle(fixture.cli.binary().as_os_str(), &fixture.root, "dev", None)
                .with_limits(Limits {
                    call: Duration::from_secs(1),
                    deadline: QUICK,
                    poll: Duration::ZERO,
                });
        let outcome = runner.submit(SQL, WAREHOUSE, wait);
        assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    }

    #[test]
    fn classifies_missing_objects_and_denied_access() {
        let http = |status, code: Option<&str>| Error::Http {
            status,
            error_code: code.map(str::to_owned),
            message: None,
        };
        let sql = |code: Option<&str>, message: &str| Error::Sql {
            state: "FAILED".to_owned(),
            error_code: code.map(str::to_owned),
            message: Some(message.to_owned()),
        };
        for missing in [
            http(404, None),
            http(400, Some("RESOURCE_DOES_NOT_EXIST")),
            sql(
                Some("BAD_REQUEST"),
                "[TABLE_OR_VIEW_NOT_FOUND] The table or view `system`.`x`.`y` cannot be found.",
            ),
            sql(Some("SCHEMA_NOT_FOUND"), "no such schema"),
            sql(None, "[CATALOG_NOT_FOUND] The catalog `c` was not found."),
        ] {
            assert!(missing.is_not_found(), "{missing:?}");
            assert!(!missing.is_permission_denied(), "{missing:?}");
        }
        for denied in [
            http(403, None),
            http(401, Some("PERMISSION_DENIED")),
            sql(
                None,
                "[INSUFFICIENT_PERMISSIONS] User does not have USE SCHEMA.",
            ),
        ] {
            assert!(denied.is_permission_denied(), "{denied:?}");
            assert!(!denied.is_not_found(), "{denied:?}");
        }
        for neither in [
            http(500, None),
            // A class only counts where the API puts it, not anywhere in the
            // message.
            sql(None, "near TABLE_OR_VIEW_NOT_FOUND"),
            Error::Transport("TABLE_OR_VIEW_NOT_FOUND".to_owned()),
            Error::Refused,
        ] {
            assert!(!neither.is_not_found(), "{neither:?}");
            assert!(!neither.is_permission_denied(), "{neither:?}");
        }
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
        let fixture = Fixture::new("transport-late-success", &late);
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
        // The statement has ended, so it is not cancelled.
        assert_eq!(fixture.invocations().len(), 1);
    }

    #[test]
    fn only_a_run_stopped_at_the_deadline_is_a_timeout() {
        let finished = Call {
            args: Vec::new(),
            result: Ok(Captured {
                stdout: Vec::new(),
                stderr: Vec::new(),
                status: ExitStatus::default(),
            }),
        };
        let stopped = Call {
            args: Vec::new(),
            result: Err("timed out".to_owned()),
        };
        // A run that finished, such as one answering 403, keeps its error.
        assert!(!stopped_at_deadline(Some(&finished), Duration::ZERO));
        assert!(stopped_at_deadline(Some(&stopped), Duration::ZERO));
        assert!(!stopped_at_deadline(Some(&stopped), Duration::from_secs(1)));
        assert!(!stopped_at_deadline(None, Duration::ZERO));
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
    fn a_slow_result_chunk_does_not_outlast_the_deadline() {
        let chunk1 = format!("{}/result/chunks/1", status_path());
        let scenario = [
            submit_waiting(
                Duration::ZERO,
                &succeeded(r#"{"chunk_index":0,"data_array":[["1",null]],"next_chunk_index":1}"#),
            ),
            get(&chunk1, r#"{"chunk_index":1,"data_array":[["2",null]]}"#)
                .replace("exit 0", "sleep 20000\nexit 0"),
        ]
        .concat();
        let fixture = Fixture::new("transport-slow-chunk", &scenario);
        let runner = fixture.runner(QUICK).with_limits(Limits {
            call: Duration::from_secs(20),
            deadline: Duration::from_millis(500),
            poll: Duration::ZERO,
        });
        let started = Instant::now();
        let outcome = runner.statement(SQL, WAREHOUSE);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(
            outcome.result,
            Err(Error::Timeout {
                statement_id: ID.to_owned()
            })
        );
        // The statement had finished, so nothing is cancelled.
        assert_eq!(fixture.invocations().len(), 2);
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
                submit(&succeeded(r#"{"data_array":[[1,null]]}"#)),
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
                    get(
                        &chunk1,
                        r#"{"chunk_index":1,"data_array":[],"next_chunk_index":1}"#,
                    ),
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
                "transport-unnumbered-chunk",
                [
                    submit(&succeeded(two_chunks)),
                    get(&chunk1, r#"{"data_array":[["2",null]]}"#),
                ]
                .concat(),
                "chunk 1 came back without a `chunk_index`",
            ),
            (
                "transport-text-chunk-index",
                [
                    submit(&succeeded(two_chunks)),
                    get(&chunk1, r#"{"chunk_index":"1","data_array":[["2",null]]}"#),
                ]
                .concat(),
                "chunk 1 came back without a `chunk_index`",
            ),
            (
                "transport-short-row",
                submit(&succeeded(r#"{"data_array":[["1"]]}"#)),
                "a result row has 1 cells for 2 columns",
            ),
            (
                "transport-long-row",
                [
                    submit(&succeeded(two_chunks)),
                    get(&chunk1, r#"{"chunk_index":1,"data_array":[["2","b","c"]]}"#),
                ]
                .concat(),
                "a result row has 3 cells for 2 columns",
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
    fn an_empty_result_set_has_no_chunks() {
        let empty = |result: &str| {
            format!(
                r#"{{"statement_id":"{ID}","status":{{"state":"SUCCEEDED"}},"manifest":{{"total_chunk_count":0,"total_row_count":0,"schema":{{"columns":[{{"name":"one"}},{{"name":"nothing"}}]}}}}{result}}}"#
            )
        };
        for (name, result) in [
            ("transport-empty-object", r#","result":{}"#),
            ("transport-empty-row-count", r#","result":{"row_count":0}"#),
            ("transport-empty-array", r#","result":{"data_array":[]}"#),
            ("transport-empty-absent", ""),
        ] {
            let fixture = Fixture::new(name, &submit(&empty(result)));
            let outcome = fixture.runner(QUICK).statement(SQL, WAREHOUSE);
            assert_eq!(outcome.result, Ok(rows(&[])), "{name}");
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
        // A made-up JSON Web Token, shaped like an OAuth access token.
        let jwt = format!(
            "eyJhbGciOiJSUzI1NiJ9.{}.{}",
            "eyJzdWIiOiJ4In0", "c2lnbmF0dXJl"
        );
        for (input, expected) in [
            (
                format!("> \"authorization: Bearer {token}\"\n"),
                "> \"authorization: [REDACTED]\"\n".to_owned(),
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
            (
                r#"[["authorization: pending","next cell"],["x"]]"#.to_owned(),
                r#"[["authorization: [REDACTED]","next cell"],["x"]]"#.to_owned(),
            ),
            (
                format!(
                    r#"{{"access_token": "{jwt}", "refresh_token":"r\"t", "token_type":"Bearer"}}"#
                ),
                r#"{"access_token": "[REDACTED]", "refresh_token":"[REDACTED]", "token_type":"Bearer"}"#
                    .to_owned(),
            ),
            (
                format!("token {jwt} end; heyJune.is.here eyJ.a.b"),
                "token [REDACTED] end; heyJune.is.here eyJ.a.b".to_owned(),
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
    fn takes_the_response_to_this_calls_own_request() {
        let ours = Request {
            method: "get",
            path: "/api/x?view=full",
        };
        let status = |stderr: &str| logged_response(stderr, Some(ours)).map(|logged| logged.status);
        // A later, unrelated request does not replace this call's response.
        let failed_then_telemetry = "Debug: GET /api/x\n< HTTP/1.1 404 Not Found\n< {\"error_code\":\"NOT_FOUND\"} pid=1 sdk=true\nDebug: POST /telemetry-ext\n< HTTP/1.1 200 OK\n< {} pid=1 sdk=true\n";
        assert_eq!(status(failed_then_telemetry), Some(404));
        let code = logged_response(failed_then_telemetry, Some(ours))
            .and_then(|logged| logged_object(&logged.lines))
            .and_then(|body| body.root().get("error_code")?.as_str().map(str::to_owned));
        assert_eq!(code.as_deref(), Some("NOT_FOUND"));
        let ok_then_refresh = "Debug: GET /api/x?view=full\n< HTTP/1.1 200 OK\n< {} pid=1 sdk=true\nDebug: POST /oidc/v1/token\n< HTTP/1.1 500 x\n";
        assert_eq!(status(ok_then_refresh), Some(200));
        // A request before this call's, like the CLI's config lookup, is
        // skipped; a retry and a redirect are followed.
        let missing = Request {
            method: "GET",
            path: "/api/2.0/pipelines/missing",
        };
        let logged = logged_response(LOGGED_404, Some(missing)).expect("response");
        assert_eq!(logged.status, 404);
        let retried = "Debug: GET /api/x\n< HTTP/1.1 503 x\nDebug: GET /api/x\n< HTTP/1.1 200 OK\n";
        assert_eq!(status(retried), Some(200));
        let redirected = "Debug: GET /api/x\n< HTTP/1.1 307 Temporary Redirect\nDebug: GET /api/y\n< HTTP/2.0 200 OK\n";
        assert_eq!(status(redirected), Some(200));
        // A request with no logged response has no status, and a log that
        // names no request falls back to its last response.
        assert_eq!(
            status("Debug: GET /api/x\nError: connection refused\n"),
            None
        );
        assert_eq!(status("< HTTP/1.1 403 Forbidden\n"), Some(403));
    }

    #[test]
    fn api_get_ignores_a_later_unrelated_request() {
        let log = "Debug: GET /api/x\n< HTTP/1.1 200 OK\n< {} pid=1 sdk=true\nDebug: POST /telemetry-ext\n< HTTP/1.1 500 Internal Server Error\n< {\"error_code\":\"INTERNAL\"} pid=1 sdk=true\n";
        let scenario = case(
            &["api", "get", "/api/x", "--debug"],
            "{\"ok\":true}",
            log,
            0,
        );
        let fixture = Fixture::new("transport-later-request", &scenario);
        let response = fixture.runner(QUICK).api_get("/api/x").result.expect("200");
        assert_eq!(response.status, Some(200));
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

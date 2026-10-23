//! Read-only transport to a Databricks workspace (#31).
//!
//! Every workspace operation runs the resolved `databricks` executable (R6)
//! from the bundle root, with the bundle target and, when set, the profile,
//! so the CLI picks the workspace and credentials as `databricks bundle` does.
//!
//! - API reads run `databricks api get <path> --debug`. The CLI prints the
//!   response body on stdout and the HTTP exchange on stderr; the status comes
//!   from the last `< HTTP/<version> <code>` line and, on an error, the
//!   `error_code` and `message` from the JSON error body.
//! - Statements use the Statement Execution API: submit, poll until done,
//!   cancel at the deadline, and read every result chunk. Only statements that
//!   pass [`safety::is_read_statement`] are submitted; anything else is
//!   refused before a CLI process starts.
//!
//! Every CLI run comes back as a [`Call`] for the evidence record, with
//! credentials scrubbed from its output. Full redaction is #36.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::databricks::{Captured, run_captured_command};
use crate::json::{self, Document, Kind, Node, Value};
use crate::safety;

/// How long the warehouse may hold the submit request before answering with
/// a pending statement. The API accepts 5 to 50 seconds.
const WAIT_TIMEOUT: &str = "30s";
const STATEMENTS: &str = "/api/2.0/sql/statements";
const REDACTED: &str = "[REDACTED]";

/// The time limits for CLI calls and statements.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// One CLI run. Longer than [`WAIT_TIMEOUT`], which a submit can take.
    pub(crate) call: Duration,
    /// From submitting a statement until it must have finished.
    pub(crate) deadline: Duration,
    /// Between polls of a running statement.
    pub(crate) poll: Duration,
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
        }
    }
}

/// How probes reach the workspace. Tests can supply their own.
pub(crate) trait Runner {
    /// Runs a read-only CLI command, such as `bundle validate`, and captures
    /// its output.
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

    /// Runs the CLI and returns the raw result, for parsing, with the
    /// scrubbed [`Call`] for evidence.
    fn capture(&self, args: &[&str], timeout: Duration) -> (Result<Captured, String>, Call) {
        let args = self.arguments(args);
        let result = run_captured_command(&mut self.command(&args), timeout);
        let call = Call {
            args,
            result: result.as_ref().map(scrubbed).map_err(Clone::clone),
        };
        (result, call)
    }

    /// One `databricks api <method>` call, stopped after `timeout`.
    fn api(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        timeout: Duration,
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
        let (result, call) = self.capture(&args, timeout);
        (call, response(method, &result))
    }

    fn statement_rows(
        &self,
        sql: &str,
        warehouse_id: &str,
        calls: &mut Vec<Call>,
    ) -> Result<ResultSet, Error> {
        let started = Instant::now();
        let submit = submit_body(sql, warehouse_id);
        let mut body =
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
        loop {
            let state = body
                .root()
                .pointer("/status/state")
                .and_then(Node::as_str)
                .unwrap_or_default()
                .to_owned();
            match state.as_str() {
                "SUCCEEDED" => break,
                "PENDING" | "RUNNING" => {
                    // Neither the sleep nor the poll may outlast the deadline.
                    let remaining = || self.limits.deadline.saturating_sub(started.elapsed());
                    thread::sleep(self.limits.poll.min(remaining()));
                    if remaining().is_zero() {
                        return Err(self.cancel(&status_path, statement_id, calls));
                    }
                    let timeout = self.limits.call.min(remaining());
                    match self.statement_call("get", &status_path, None, timeout, calls) {
                        Ok(next) => body = next,
                        Err(_) if remaining().is_zero() => {
                            return Err(self.cancel(&status_path, statement_id, calls));
                        }
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

        let columns = columns(body.root())?;
        let mut rows = Vec::new();
        let mut chunk = body.root().get("result");
        let mut owned: Document;
        let mut last_index = None;
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
            chunk = Some(owned.root());
        }
        Ok(ResultSet {
            statement_id,
            columns,
            rows,
        })
    }

    /// Sends a cancel for a statement that outlasted the deadline. A failed
    /// cancel is recorded but does not change the outcome.
    fn cancel(&self, status_path: &str, statement_id: String, calls: &mut Vec<Call>) -> Error {
        let cancel = format!("{status_path}/cancel");
        let (call, _) = self.api("post", &cancel, Some("{}"), self.limits.call);
        calls.push(call);
        Error::Timeout { statement_id }
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
        let (call, response) = self.api(method, path, body, timeout);
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
        self.capture(args, timeout).1
    }

    fn api_get(&self, path: &str) -> Outcome<Response> {
        let (call, result) = self.api("get", path, None, self.limits.call);
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
/// rather than a cancelled one if it outlasts [`WAIT_TIMEOUT`].
fn submit_body(sql: &str, warehouse_id: &str) -> String {
    let body = Value::object([
        ("disposition", Value::string("INLINE")),
        ("format", Value::string("JSON_ARRAY")),
        ("on_wait_timeout", Value::string("CONTINUE")),
        ("statement", Value::string(sql)),
        ("wait_timeout", Value::string(WAIT_TIMEOUT)),
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

/// The status from the last `< HTTP/<version> <code>` line of a debug log.
fn http_status(stderr: &str) -> Option<u16> {
    stderr.lines().rev().find_map(|line| {
        let (_, rest) = line.split_once("< HTTP/")?;
        let code = rest.split_whitespace().nth(1)?;
        code.parse().ok().filter(|code| (100..600).contains(code))
    })
}

/// The JSON error body the CLI prints after a failed request: the first
/// line that starts with `{` from which the rest of the log parses.
fn error_body(stderr: &str) -> Option<Document> {
    let mut offset = 0;
    for line in stderr.split_inclusive('\n') {
        if line.starts_with('{')
            && let Ok(document) = json::parse(&stderr[offset..])
            && document.root().kind() == Kind::Object
        {
            return Some(document);
        }
        offset += line.len();
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

fn scrubbed(captured: &Captured) -> Captured {
    Captured {
        stdout: scrub(&captured.stdout),
        stderr: scrub(&captured.stderr),
        status: captured.status,
    }
}

/// Replaces credentials in CLI output with `[REDACTED]`: the value of an
/// `Authorization` header, a bearer token, and a personal access token
/// (`dapi` and 32 hex digits). Output without any is returned unchanged.
fn scrub(output: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(output);
    let mut scrubbed = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (content, ending) = match line.strip_suffix('\n') {
            Some(content) => (content, "\n"),
            None => (line, ""),
        };
        let content = match find_ignore_case(content, "authorization:") {
            Some(at) => format!("{} {REDACTED}", &content[..at + "authorization:".len()]),
            None => replace_bearer(content),
        };
        scrubbed.push_str(&replace_tokens(&content));
        scrubbed.push_str(ending);
    }
    if scrubbed == text {
        output.to_vec()
    } else {
        scrubbed.into_bytes()
    }
}

fn find_ignore_case(text: &str, needle: &str) -> Option<usize> {
    text.to_ascii_lowercase().find(needle)
}

/// `Bearer <token>` becomes `Bearer [REDACTED]`.
fn replace_bearer(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = find_ignore_case(rest, "bearer ") {
        let (before, after) = rest.split_at(at + "bearer ".len());
        result.push_str(before);
        let token = after
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(after.len());
        if token > 0 && &after[..token] != REDACTED {
            result.push_str(REDACTED);
        } else {
            result.push_str(&after[..token]);
        }
        rest = &after[token..];
    }
    result.push_str(rest);
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

    use super::{
        CliRunner, Error, Limits, ResultSet, Runner, STATEMENTS, http_status, scrub, submit_body,
    };
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

    /// The submit case for [`SQL`], answering with this output.
    fn submit_case(stdout: &str, stderr: &str, exit: u8) -> String {
        let body = submit_body(SQL, WAREHOUSE);
        case(
            &["api", "post", STATEMENTS, "--json", &body, "--debug"],
            stdout,
            stderr,
            exit,
        )
    }

    /// The submit case, answering with `body`.
    fn submit(body: &str) -> String {
        submit_case(body, &debug_log("post", STATEMENTS, "200 OK"), 0)
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
        let cancel = format!("{}/cancel", status_path());
        let scenario = [
            submit(&state("PENDING")),
            case(
                &["api", "post", &cancel, "--json", "{}", "--debug"],
                "{}",
                &debug_log("post", &cancel, "200 OK"),
                0,
            ),
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
        let cancel = format!("{}/cancel", status_path());
        let cancel_case = case(
            &["api", "post", &cancel, "--json", "{}", "--debug"],
            "{}",
            &debug_log("post", &cancel, "200 OK"),
            0,
        );
        let slow_poll =
            get(&status_path(), &state("RUNNING")).replace("exit 0", "sleep 20000\nexit 0");
        for (name, poll, scenario, polls) in [
            (
                "transport-long-interval",
                Duration::from_secs(20),
                [submit(&state("PENDING")), cancel_case.clone()].concat(),
                0,
            ),
            (
                "transport-slow-poll",
                Duration::ZERO,
                [submit(&state("PENDING")), slow_poll, cancel_case.clone()].concat(),
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
    fn rejects_malformed_statement_responses() {
        let chunk0 = format!("{}/result/chunks/0", status_path());
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
                    submit(&succeeded(r#"{"data_array":[],"next_chunk_index":0}"#)),
                    get(&chunk0, r#"{"data_array":[],"next_chunk_index":0}"#),
                ]
                .concat(),
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
                "dapi is short: dapi1234".to_owned(),
                "dapi is short: dapi1234".to_owned(),
            ),
        ] {
            assert_eq!(
                String::from_utf8(scrub(input.as_bytes())).expect("UTF-8"),
                expected
            );
        }
        let binary = b"plain \xff output\n";
        assert_eq!(scrub(binary), binary);
    }

    #[test]
    fn reads_the_last_http_status_line() {
        assert_eq!(
            http_status("< HTTP/1.1 307 Temporary Redirect\n< HTTP/2.0 200 OK\n"),
            Some(200)
        );
        assert_eq!(http_status("GET /x HTTP/1.1\n"), None);
        assert_eq!(http_status("< HTTP/1.1 abc\n"), None);
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
            Error::Refused.to_string(),
            "refused a statement that is not read-only"
        );
    }
}

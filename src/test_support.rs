//! Helpers shared by the crate's unit tests.

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::OnceLock;
use std::time::Duration;

use crate::databricks::Captured;
use crate::transport::{Call, Error, Outcome, Response, ResultSet, Runner, Submitted};

#[path = "../tests/common/scenario.rs"]
mod scenario;

pub(crate) use scenario::{ScenarioCli, assert_no_mutations, stdout_line};
// For `scenario`, which reaches the guard and the JSON layer through its
// parent module.
use crate::{json, safety};

/// The fake Databricks CLI, compiled once per test process.
pub(crate) fn fake_databricks() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(compile_fake).as_path()
}

fn compile_fake() -> PathBuf {
    let output_dir =
        std::env::temp_dir().join(format!("dbxctl-unit-fixture-{}", std::process::id()));
    fs::create_dir_all(&output_dir).expect("create fixture output directory");
    let binary = output_dir.join(format!("fake-databricks{}", std::env::consts::EXE_SUFFIX));
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_databricks.rs");
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

/// A scratch directory, removed when dropped. `name` must be unique per test.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("dbxctl-test-{name}-{}", std::process::id()));
        // A directory left by an interrupted run of the same process ID.
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Reads every file below `root` as `(relative path, bytes)`, sorted by path,
/// with `/` separators on every platform.
pub(crate) fn read_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read directory") {
            let path = entry.expect("read directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("path below root")
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                files.push((relative, fs::read(&path).expect("read file")));
            }
        }
    }
    files.sort();
    files
}

/// A scripted answer: the JSON text of a body or result, or an error.
enum Answer {
    Body(Option<u16>, Option<String>),
    Rows(Vec<String>, Vec<Vec<Option<String>>>),
    Submitted(String),
    Error(Error),
}

/// A [`Runner`] that answers from a script, for checks that read the
/// workspace. The answer scripted last for a request wins, so a test can
/// override one answer of a shared script. A request nothing in the script
/// answers panics, so a test sees every request a check makes. Statements must pass the read-only guard,
/// as with the real transport.
///
/// Each answer comes with one CLI run that exited 0 with no output, or,
/// after [`FakeTransport::unrunnable`], one that could not be run.
#[derive(Default)]
pub(crate) struct FakeTransport {
    gets: Vec<(String, Answer)>,
    /// Matched by a fragment of the SQL.
    statements: Vec<(String, Answer)>,
    submits: Vec<(Duration, Answer)>,
    unrunnable: bool,
    asked: RefCell<Vec<String>>,
}

impl FakeTransport {
    /// A transport whose every request is a CLI that could not be run.
    pub(crate) fn unrunnable() -> Self {
        Self {
            unrunnable: true,
            ..Self::default()
        }
    }

    /// Answers a GET of `path` with HTTP 200 and the JSON `body`.
    pub(crate) fn get(self, path: &str, body: &str) -> Self {
        self.get_answer(path, Some(200), Some(body))
    }

    /// Answers a GET of `path` with `status`, as the debug log showed it,
    /// and `body`.
    pub(crate) fn get_answer(
        mut self,
        path: &str,
        status: Option<u16>,
        body: Option<&str>,
    ) -> Self {
        self.gets.push((
            path.to_owned(),
            Answer::Body(status, body.map(str::to_owned)),
        ));
        self
    }

    pub(crate) fn get_error(mut self, path: &str, error: Error) -> Self {
        self.gets.push((path.to_owned(), Answer::Error(error)));
        self
    }

    /// Answers a statement containing `fragment` with these rows.
    pub(crate) fn statement(
        mut self,
        fragment: &str,
        columns: &[&str],
        rows: &[&[Option<&str>]],
    ) -> Self {
        let rows = rows
            .iter()
            .map(|row| row.iter().map(|cell| cell.map(str::to_owned)).collect())
            .collect();
        let columns = columns.iter().map(|&column| column.to_owned()).collect();
        self.statements
            .push((fragment.to_owned(), Answer::Rows(columns, rows)));
        self
    }

    pub(crate) fn statement_error(mut self, fragment: &str, error: Error) -> Self {
        self.statements
            .push((fragment.to_owned(), Answer::Error(error)));
        self
    }

    /// Answers a submit with `wait` with a statement in `state`.
    pub(crate) fn submit(mut self, wait: Duration, state: &str) -> Self {
        self.submits
            .push((wait, Answer::Submitted(state.to_owned())));
        self
    }

    pub(crate) fn submit_error(mut self, wait: Duration, error: Error) -> Self {
        self.submits.push((wait, Answer::Error(error)));
        self
    }

    /// Every request so far: `GET <path>`, `SQL <statement>`, or
    /// `SUBMIT <seconds>s <statement>`.
    pub(crate) fn asked(&self) -> Vec<String> {
        self.asked.borrow().clone()
    }

    fn outcome<T>(
        &self,
        request: &str,
        answer: Option<&Answer>,
        map: impl FnOnce(&Answer) -> Result<T, Error>,
    ) -> Outcome<T> {
        self.asked.borrow_mut().push(request.to_owned());
        let args = vec![OsString::from("fake"), OsString::from(request)];
        if self.unrunnable {
            return Outcome {
                calls: vec![Call {
                    args,
                    result: Err("could not execute databricks: not found".to_owned()),
                }],
                result: Err(Error::Transport("could not be run".to_owned())),
            };
        }
        let answer = answer.unwrap_or_else(|| panic!("nothing answers {request}"));
        Outcome {
            calls: vec![Call {
                args,
                result: Ok(Captured {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    status: exit_success(),
                }),
            }],
            result: map(answer),
        }
    }
}

impl Runner for FakeTransport {
    fn run(&self, args: &[&str], _timeout: Duration) -> Call {
        panic!("FakeTransport does not run CLI commands: {args:?}")
    }

    fn api_get(&self, path: &str) -> Outcome<Response> {
        let answer = self
            .gets
            .iter()
            .rfind(|(scripted, _)| scripted == path)
            .map(|(_, answer)| answer);
        self.outcome(&format!("GET {path}"), answer, |answer| match answer {
            Answer::Body(status, body) => Ok(Response {
                status: *status,
                body: body
                    .as_deref()
                    .map(|body| crate::json::parse(body).expect("scripted JSON body")),
            }),
            other => Err(error_of(other)),
        })
    }

    fn statement(&self, sql: &str, _warehouse_id: &str) -> Outcome<ResultSet> {
        if !crate::safety::is_read_statement(sql) {
            self.asked.borrow_mut().push(format!("REFUSED {sql}"));
            return Outcome {
                calls: Vec::new(),
                result: Err(Error::Refused),
            };
        }
        let answer = self
            .statements
            .iter()
            .rfind(|(fragment, _)| sql.contains(fragment.as_str()))
            .map(|(_, answer)| answer);
        self.outcome(&format!("SQL {sql}"), answer, |answer| match answer {
            Answer::Rows(columns, rows) => Ok(ResultSet {
                statement_id: "01ef-fake".to_owned(),
                columns: columns.clone(),
                rows: rows.clone(),
            }),
            other => Err(error_of(other)),
        })
    }

    fn submit(&self, sql: &str, _warehouse_id: &str, wait: Duration) -> Outcome<Submitted> {
        if !crate::safety::is_read_statement(sql) {
            return Outcome {
                calls: Vec::new(),
                result: Err(Error::Refused),
            };
        }
        let answer = self
            .submits
            .iter()
            .rfind(|(scripted, _)| *scripted == wait)
            .map(|(_, answer)| answer);
        let request = format!("SUBMIT {}s {sql}", wait.as_secs());
        self.outcome(&request, answer, |answer| match answer {
            Answer::Submitted(state) => Ok(Submitted {
                statement_id: "01ef-fake".to_owned(),
                state: state.clone(),
            }),
            other => Err(error_of(other)),
        })
    }
}

/// The error a scripted answer holds; `Error` is not `Clone`, so each
/// request gets its own copy.
fn error_of(answer: &Answer) -> Error {
    let Answer::Error(error) = answer else {
        panic!("the scripted answer is for a different request");
    };
    match error {
        Error::Refused => Error::Refused,
        Error::Transport(problem) => Error::Transport(problem.clone()),
        Error::Http {
            status,
            error_code,
            message,
        } => Error::Http {
            status: *status,
            error_code: error_code.clone(),
            message: message.clone(),
        },
        Error::Sql {
            state,
            error_code,
            message,
        } => Error::Sql {
            state: state.clone(),
            error_code: error_code.clone(),
            message: message.clone(),
        },
        Error::Timeout { statement_id } => Error::Timeout {
            statement_id: statement_id.clone(),
        },
        Error::Truncated { statement_id } => Error::Truncated {
            statement_id: statement_id.clone(),
        },
    }
}

/// An exit status for a process that exited 0.
pub(crate) fn exit_success() -> ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }
}

/// A statement error of the class the API reports for a missing table.
pub(crate) fn missing_table() -> Error {
    Error::Sql {
        state: "FAILED".to_owned(),
        error_code: Some("BAD_REQUEST".to_owned()),
        message: Some("[TABLE_OR_VIEW_NOT_FOUND] The table or view cannot be found.".to_owned()),
    }
}

/// A statement error of the class the API reports for missing privileges.
pub(crate) fn denied() -> Error {
    Error::Sql {
        state: "FAILED".to_owned(),
        error_code: Some("PERMISSION_DENIED".to_owned()),
        message: None,
    }
}

#[cfg(test)]
mod tests {
    use std::panic::catch_unwind;

    use super::assert_no_mutations;

    fn argv(arguments: &[&str]) -> Vec<String> {
        arguments
            .iter()
            .map(|&argument| argument.to_owned())
            .collect()
    }

    #[test]
    fn accepts_help_only_after_a_command_path() {
        for allowed in [
            &["--help"][..],
            &["-h"],
            &["bundle", "deploy", "--help"],
            &["bundle", "plan", "-h"],
        ] {
            assert_no_mutations(&[argv(allowed)]);
        }
        for rejected in [
            &["bundle", "deploy", "--var", "-h"][..],
            &["bundle", "deploy", "--help", "--auto-approve"],
            &["bundle", "deploy", "-t", "dev", "--help"],
        ] {
            let invocations = [argv(rejected)];
            assert!(
                catch_unwind(|| assert_no_mutations(&invocations)).is_err(),
                "{rejected:?} must not count as a help request"
            );
        }
    }

    #[test]
    fn checks_command_paths_exactly_and_statements_with_the_guard() {
        let submit = |sql: &str| {
            let body = format!(
                r#"{{"disposition":"INLINE","statement":"{}","warehouse_id":"w"}}"#,
                sql.replace('\\', "\\\\").replace('"', "\\\"")
            );
            argv(&[
                "api",
                "post",
                "/api/2.0/sql/statements",
                "--json",
                &body,
                "--debug",
            ])
        };
        for allowed in [
            argv(&["bundle", "validate", "-t", "dev"]),
            argv(&["api", "get", "/api/2.0/pipelines/x", "--debug"]),
            argv(&[
                "api",
                "post",
                "/api/2.0/sql/statements/01ef/cancel",
                "--json",
                "{}",
            ]),
            submit("SELECT 1"),
            submit("SELECT 'C:\\temp', \"x\" AS q"),
        ] {
            assert_no_mutations(&[allowed]);
        }
        for rejected in [
            argv(&["bundle", "validate", "extra"]),
            argv(&["api", "get", "/a", "/b"]),
            argv(&["api", "get", "relative"]),
            submit("DROP TABLE t"),
            submit("SELECT 1; DROP TABLE t"),
            argv(&["api", "post", "/api/2.0/sql/statements", "--debug"]),
        ] {
            let invocations = [rejected];
            assert!(
                catch_unwind(|| assert_no_mutations(&invocations)).is_err(),
                "{:?} must count as a mutation",
                invocations[0]
            );
        }
    }
}

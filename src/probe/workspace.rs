//! Workspace steps for the API and SQL checks (#33): running one transport
//! operation, recording its CLI runs as evidence, and telling an answer from
//! a CLI that could not run.

use std::cell::RefCell;

use crate::probe::model::{CheckId, Finding};
use crate::probe::orchestrator::Context;
use crate::text;
use crate::transport::{Call, Error, Outcome, ResultSet, Runner};

/// What one workspace step of a check produced.
pub(crate) enum Reply<T> {
    /// The workspace answered.
    Answer(T),
    /// The workspace answered with an error, such as a missing table or a
    /// rejected request. The check decides what it means.
    Error(Error),
    /// The CLI could not be run, or the request was refused before it ran:
    /// the check's finding.
    Failed(Finding),
}

/// What a part of a check read: its value, the reason it is unknown, or
/// the finding when the CLI could not run.
pub(crate) enum Read<T> {
    Seen(T),
    Unknown(String),
    Failed(Finding),
}

/// Runs `operation` as `step` of `check`, records each CLI run it took in
/// `evidence`, and classifies the result. One run is recorded as
/// `<step>.json`; more, such as a statement's submit and polls, as
/// `<step>-1.json`, `<step>-2.json`, and so on.
pub(crate) fn step<T>(
    context: &Context<'_>,
    check: CheckId,
    step: &str,
    site: &'static str,
    evidence: &mut Vec<String>,
    operation: impl FnOnce(&dyn Runner) -> Outcome<T>,
) -> Result<Reply<T>, String> {
    let Outcome { calls, result } = operation(context.transport);
    for (number, call) in calls.iter().enumerate() {
        let name = if calls.len() == 1 {
            step.to_owned()
        } else {
            format!("{step}-{}", number + 1)
        };
        evidence.push(context.run.record_invocation(
            check.as_str(),
            &name,
            context.binary,
            &call.args,
            &call.result,
        )?);
    }
    Ok(match result {
        Ok(answer) => Reply::Answer(answer),
        Err(error) if could_not_run(&error, &calls) => Reply::Failed(Finding::failed(
            check,
            format!(
                "the {step} step {}; see the evidence record",
                describe(&error)
            ),
            evidence.clone(),
            site,
        )),
        Err(error) => Reply::Error(error),
    })
}

/// Whether `error` means the CLI could not run: a refused statement, or a
/// transport error that came from a run that did not complete, or from a
/// request refused before any run. The run that produced the error is the
/// last one that is not a cancel: after a failed poll, the transport sends a
/// best-effort cancel, which must not hide the poll's failure.
fn could_not_run(error: &Error, calls: &[Call]) -> bool {
    match error {
        Error::Refused => true,
        Error::Transport(_) => calls
            .iter()
            .rev()
            .find(|call| !is_cancel(call))
            .is_none_or(|call| call.result.is_err()),
        _ => false,
    }
}

/// Whether `call` is `databricks api post <statement>/cancel`.
fn is_cancel(call: &Call) -> bool {
    matches!(
        call.args.as_slice(),
        [api, post, path, ..] if api == "api"
            && post == "post"
            && path.to_str().is_some_and(|path| path.ends_with("/cancel"))
    )
}

/// `error` for a reason. Written by dbxctl: it names the kind of error and
/// its HTTP status or statement state, never text from the CLI's output.
pub(crate) fn describe(error: &Error) -> String {
    let kind = match error {
        Error::Refused => "was refused as not read-only".to_owned(),
        Error::Transport(_) => "returned output that is not the expected response".to_owned(),
        Error::Http { status, .. } => format!("failed with HTTP {status}"),
        // The transport only reports these three states as errors.
        Error::Sql { state, .. } => match state.as_str() {
            "FAILED" | "CANCELED" | "CLOSED" => format!("ended {state}"),
            _ => "ended unsuccessfully".to_owned(),
        },
        Error::Timeout { .. } => "timed out".to_owned(),
        Error::Truncated { .. } => "returned a truncated result".to_owned(),
    };
    if error.is_not_found() {
        format!("{kind} (not found)")
    } else if error.is_permission_denied() {
        format!("{kind} (permission denied)")
    } else {
        kind
    }
}

/// `text`, which comes from the workspace, as one word of a verdict: ASCII
/// letters, digits, and `_ . : / { } -` are kept, anything else becomes `?`,
/// and it is cut to 64 characters. A verdict is one line of dbxctl's
/// output, so it cannot carry arbitrary workspace text.
pub(crate) fn word(text: &str) -> String {
    text.chars()
        .take(64)
        .map(|character| {
            if character.is_ascii_alphanumeric() || "_.:/{}-".contains(character) {
                character
            } else {
                '?'
            }
        })
        .collect()
}

/// The non-null cells of `column` in `result`, as verdict words, or `None`
/// if the result has no such column.
pub(crate) fn words(result: &ResultSet, column: &str) -> Option<Vec<String>> {
    let at = result.columns.iter().position(|name| name == column)?;
    Some(
        result
            .rows
            .iter()
            .filter_map(|row| row.get(at)?.as_deref())
            .map(word)
            .collect(),
    )
}

/// Every cell of `column` in `result`, as verdict words. `Err` describes
/// the unexpected shape: no such column, or a null cell.
pub(crate) fn required_words(result: &ResultSet, column: &str) -> Result<Vec<String>, String> {
    let at = result
        .columns
        .iter()
        .position(|name| name == column)
        .ok_or_else(|| format!("has no `{column}` column"))?;
    result
        .rows
        .iter()
        .map(|row| {
            row.get(at)
                .and_then(Option::as_deref)
                .map(word)
                .ok_or_else(|| format!("has a null `{column}`"))
        })
        .collect()
}

/// The cell of `column` in `row` of `result`, if both exist and it is not
/// null.
pub(crate) fn cell<'a>(
    result: &'a ResultSet,
    row: &'a [Option<String>],
    column: &str,
) -> Option<&'a str> {
    let at = result.columns.iter().position(|name| name == column)?;
    row.get(at)?.as_deref()
}

/// The columns `DESCRIBE TABLE` lists, as verdict words: its `col_name`
/// cells up to the first empty or `#` one, which starts the partition and
/// detail sections. `None` if the result has no `col_name` column.
pub(crate) fn described_columns(result: &ResultSet) -> Option<Vec<String>> {
    let at = result.columns.iter().position(|name| name == "col_name")?;
    Some(
        result
            .rows
            .iter()
            .map(|row| row.get(at).and_then(Option::as_deref).unwrap_or_default())
            .take_while(|name| !name.is_empty() && !name.starts_with('#'))
            .map(word)
            .collect(),
    )
}

/// Steps more than one check reads, run once per probe run. The first
/// check to need one runs it and records its evidence; later checks reuse
/// the answer and refer to that evidence.
#[derive(Default)]
pub(crate) struct Shared {
    table_types: RefCell<Option<(Vec<String>, Scan)>>,
}

/// A shared step's answer, without the check it was run for.
#[derive(Clone)]
enum Scan {
    Seen(Vec<String>),
    Unknown(String),
    Failed(String),
}

/// The `table_type` values in `catalog`, which is always quoted, with how
/// many tables have each.
pub(crate) fn table_types_sql(catalog: &str) -> String {
    format!(
        "SELECT table_type, count(*) AS tables FROM {}.information_schema.tables GROUP BY table_type ORDER BY table_type",
        text::identifier(catalog)
    )
}

/// The `table_type` values in the scope catalog's
/// `information_schema.tables`, as verdict words: one scan per run, shared
/// by `v3` and `v11`.
pub(crate) fn scope_table_types(
    context: &Context<'_>,
    check: CheckId,
    site: &'static str,
    warehouse_id: &str,
    catalog: &str,
    evidence: &mut Vec<String>,
) -> Result<Read<Vec<String>>, String> {
    let cached = context.shared.table_types.borrow().clone();
    let (references, scan) = if let Some(cached) = cached {
        cached
    } else {
        let mut references = Vec::new();
        let sql = table_types_sql(catalog);
        let scan = match step(
            context,
            check,
            "table-types",
            site,
            &mut references,
            |transport| transport.statement(&sql, warehouse_id),
        )? {
            Reply::Answer(result) => match required_words(&result, "table_type") {
                Ok(types) => Scan::Seen(types),
                Err(shape) => Scan::Unknown(format!(
                    "the scope catalog's `information_schema.tables` {shape}"
                )),
            },
            Reply::Error(error) => Scan::Unknown(format!(
                "reading the scope catalog's `information_schema.tables` {}",
                describe(&error)
            )),
            Reply::Failed(finding) => Scan::Failed(finding.reason().unwrap_or_default().to_owned()),
        };
        *context.shared.table_types.borrow_mut() = Some((references.clone(), scan.clone()));
        (references, scan)
    };
    evidence.extend(references);
    Ok(match scan {
        Scan::Seen(types) => Read::Seen(types),
        Scan::Unknown(reason) => Read::Unknown(reason),
        Scan::Failed(reason) => {
            Read::Failed(Finding::failed(check, reason, evidence.clone(), site))
        }
    })
}

/// `items` joined with `, `, or `none` when there are none.
pub(crate) fn list_or_none<S: AsRef<str>>(items: &[S]) -> String {
    if items.is_empty() {
        return "none".to_owned();
    }
    items
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `--warehouse-id`, or a failed finding when it is missing. Option parsing
/// requires it for every implemented check that runs statements, so a
/// missing one is a bug rather than a usage error.
pub(crate) fn warehouse<'a>(
    context: &Context<'a>,
    check: CheckId,
    site: &'static str,
) -> Result<&'a str, Finding> {
    context
        .warehouse_id
        .ok_or_else(|| missing_option(check, "--warehouse-id", site))
}

/// `--scope-catalog`, or a failed finding when it is missing.
pub(crate) fn scope_catalog<'a>(
    context: &Context<'a>,
    check: CheckId,
    site: &'static str,
) -> Result<&'a str, Finding> {
    context
        .scope_catalog
        .ok_or_else(|| missing_option(check, "--scope-catalog", site))
}

fn missing_option(check: CheckId, option: &str, site: &'static str) -> Finding {
    Finding::failed(
        check,
        format!("{option} was not given, though {check} reads it"),
        Vec::new(),
        site,
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{
        Read, Shared, cell, could_not_run, describe, described_columns, list_or_none,
        required_words, scope_table_types, table_types_sql, word, words,
    };
    use crate::databricks::Captured;
    use crate::evidence::RunDir;
    use crate::probe::model::{CheckId, State};
    use crate::probe::orchestrator::Context;
    use crate::safety::is_read_statement;
    use crate::test_support::{FakeTransport, TempDir, exit_success};
    use crate::transport::{Call, Error, ResultSet};

    fn call(args: &[&str], ran: bool) -> Call {
        Call {
            args: args.iter().map(OsString::from).collect(),
            result: if ran {
                Ok(Captured {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    status: exit_success(),
                })
            } else {
                Err("could not execute databricks: timed out".to_owned())
            },
        }
    }

    #[test]
    fn classifies_by_the_call_that_produced_the_error() {
        let transport = Error::Transport("problem".to_owned());
        let submit = || call(&["api", "post", "/api/2.0/sql/statements"], true);
        let poll = |ran| call(&["api", "get", "/api/2.0/sql/statements/s"], ran);
        let cancel = |ran| call(&["api", "post", "/api/2.0/sql/statements/s/cancel"], ran);
        // A poll that could not run, then a cancel that did.
        assert!(could_not_run(
            &transport,
            &[submit(), poll(false), cancel(true)]
        ));
        // A poll that ran but returned junk, then a cancel that could not.
        assert!(!could_not_run(
            &transport,
            &[submit(), poll(true), cancel(false)]
        ));
        // A request refused before any run.
        assert!(could_not_run(&transport, &[]));
        assert!(could_not_run(&Error::Refused, &[]));
        // Only a transport error can mean the CLI did not run.
        let http = Error::Http {
            status: 500,
            error_code: None,
            message: None,
        };
        assert!(!could_not_run(&http, &[poll(false)]));
        assert!(!could_not_run(&transport, &[submit()]));
    }

    fn result(columns: &[&str], rows: &[&[Option<&str>]]) -> ResultSet {
        ResultSet {
            statement_id: "s".to_owned(),
            columns: columns.iter().map(|&name| name.to_owned()).collect(),
            rows: rows
                .iter()
                .map(|row| row.iter().map(|cell| cell.map(str::to_owned)).collect())
                .collect(),
        }
    }

    #[test]
    fn reads_columns_and_cells_by_name() {
        let rows = result(
            &["kind", "count"],
            &[
                &[Some("MANAGED"), Some("3")],
                &[None, Some("1")],
                &[Some("a b"), None],
            ],
        );
        assert_eq!(
            words(&rows, "kind"),
            Some(vec!["MANAGED".to_owned(), "a?b".to_owned()])
        );
        assert_eq!(words(&rows, "missing"), None);
        assert_eq!(cell(&rows, &rows.rows[0], "count"), Some("3"));
        assert_eq!(cell(&rows, &rows.rows[2], "count"), None);
        assert_eq!(cell(&rows, &rows.rows[0], "missing"), None);
    }

    #[test]
    fn lists_or_says_none() {
        assert_eq!(list_or_none::<&str>(&[]), "none");
        assert_eq!(list_or_none(&["a"]), "a");
        assert_eq!(list_or_none(&["a".to_owned(), "b".to_owned()]), "a, b");
    }

    #[test]
    fn quotes_the_scope_catalog_as_an_identifier() {
        assert_eq!(
            table_types_sql("main"),
            "SELECT table_type, count(*) AS tables FROM `main`.information_schema.tables GROUP BY table_type ORDER BY table_type"
        );
        for hostile in ["x`; DROP TABLE t; --", "it's", r"a\b", "x'.y"] {
            assert!(is_read_statement(&table_types_sql(hostile)), "{hostile}");
        }
    }

    #[test]
    fn scans_the_scope_catalog_once_per_run() {
        let temp = TempDir::new("workspace-shared-scan");
        let run_dir = RunDir::create(temp.path(), "lineage", 0).expect("create run");
        let transport = FakeTransport::default().statement(
            "information_schema.tables",
            &["table_type", "tables"],
            &[&[Some("MATERIALIZED_VIEW"), Some("1")]],
        );
        let shared = Shared::default();
        let context = Context {
            binary: "databricks".as_ref(),
            run: &run_dir,
            transport: &transport,
            warehouse_id: Some("w"),
            scope_catalog: Some("main"),
            shared: &shared,
        };
        let mut first = Vec::new();
        let mut second = Vec::new();
        for (check, evidence) in [(CheckId::V3, &mut first), (CheckId::V11, &mut second)] {
            let read =
                scope_table_types(&context, check, "site", "w", "main", evidence).expect("scan");
            assert!(
                matches!(read, Read::Seen(ref types) if types == &["MATERIALIZED_VIEW"]),
                "{check}"
            );
        }
        // One statement, recorded for the first check and referred to by both.
        assert_eq!(transport.asked().len(), 1);
        assert_eq!(first, ["evidence/v3/table-types.json"]);
        assert_eq!(second, first);
    }

    #[test]
    fn a_shared_failure_names_each_check() {
        let temp = TempDir::new("workspace-shared-failure");
        let run_dir = RunDir::create(temp.path(), "lineage", 0).expect("create run");
        let transport = FakeTransport::unrunnable();
        let shared = Shared::default();
        let context = Context {
            binary: "databricks".as_ref(),
            run: &run_dir,
            transport: &transport,
            warehouse_id: Some("w"),
            scope_catalog: Some("main"),
            shared: &shared,
        };
        for check in [CheckId::V3, CheckId::V11] {
            let read = scope_table_types(&context, check, "site", "w", "main", &mut Vec::new())
                .expect("scan");
            let Read::Failed(finding) = read else {
                panic!("{check} should fail");
            };
            assert_eq!(finding.check(), check);
            assert_eq!(finding.state(), State::Failed);
        }
        assert_eq!(transport.asked().len(), 1);
    }

    #[test]
    fn requires_a_column_with_no_null_cells() {
        let rows = result(&["kind"], &[&[Some("MANAGED")], &[Some("VIEW")]]);
        assert_eq!(
            required_words(&rows, "kind"),
            Ok(vec!["MANAGED".to_owned(), "VIEW".to_owned()])
        );
        assert_eq!(
            required_words(&rows, "type"),
            Err("has no `type` column".to_owned())
        );
        let nulls = result(&["kind"], &[&[Some("MANAGED")], &[None]]);
        assert_eq!(
            required_words(&nulls, "kind"),
            Err("has a null `kind`".to_owned())
        );
    }

    #[test]
    fn reads_described_columns_up_to_the_detail_sections() {
        let described = result(
            &["col_name", "data_type", "comment"],
            &[
                &[Some("pipeline_id"), Some("string"), None],
                &[Some("pipeline_type"), Some("string"), None],
                &[Some(""), None, None],
                &[Some("# Partition Information"), None, None],
                &[Some("pipeline_id"), Some("string"), None],
            ],
        );
        assert_eq!(
            described_columns(&described),
            Some(vec!["pipeline_id".to_owned(), "pipeline_type".to_owned()])
        );
        assert_eq!(described_columns(&result(&["name"], &[])), None);
    }

    #[test]
    fn describes_errors_without_their_output() {
        let http = Error::Http {
            status: 404,
            error_code: Some("RESOURCE_DOES_NOT_EXIST".to_owned()),
            message: Some("secret detail".to_owned()),
        };
        assert_eq!(describe(&http), "failed with HTTP 404 (not found)");
        let denied = Error::Sql {
            state: "FAILED".to_owned(),
            error_code: None,
            message: Some("[INSUFFICIENT_PERMISSIONS] secret detail".to_owned()),
        };
        assert_eq!(describe(&denied), "ended FAILED (permission denied)");
        let odd = Error::Sql {
            state: "\nINJECTED".to_owned(),
            error_code: None,
            message: None,
        };
        assert_eq!(describe(&odd), "ended unsuccessfully");
        for (error, text) in [
            (Error::Refused, "was refused as not read-only"),
            (
                Error::Transport("secret detail".to_owned()),
                "returned output that is not the expected response",
            ),
            (
                Error::Timeout {
                    statement_id: "s".to_owned(),
                },
                "timed out",
            ),
            (
                Error::Truncated {
                    statement_id: "s".to_owned(),
                },
                "returned a truncated result",
            ),
        ] {
            assert_eq!(describe(&error), text);
        }
    }

    #[test]
    fn words_keep_identifiers_and_drop_anything_else() {
        assert_eq!(word("MATERIALIZED_VIEW"), "MATERIALIZED_VIEW");
        assert_eq!(word("/api/2.0/pipelines/{id}"), "/api/2.0/pipelines/{id}");
        assert_eq!(word("a b\nc|d`e"), "a?b?c?d?e");
        assert_eq!(word("ü"), "?");
        assert_eq!(word(&"x".repeat(100)).len(), 64);
    }
}

//! Workspace steps for the API and SQL checks (#33): running one transport
//! operation, recording its CLI runs as evidence, and telling an answer from
//! a CLI that could not run.

use crate::probe::model::{CheckId, Finding};
use crate::probe::orchestrator::Context;
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

/// Whether `error` means the workspace was never asked: a refused
/// statement, or a transport error whose last run did not complete or that
/// was refused before any run.
fn could_not_run(error: &Error, calls: &[Call]) -> bool {
    match error {
        Error::Refused => true,
        Error::Transport(_) => calls.last().is_none_or(|call| call.result.is_err()),
        _ => false,
    }
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
    use super::{cell, describe, described_columns, word, words};
    use crate::transport::{Error, ResultSet};

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

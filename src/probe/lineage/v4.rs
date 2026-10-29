//! The `v4` check (#33): where refresh history is exposed.
//!
//! Describes each candidate system table, and reads the updates of one
//! pipeline the caller can see through the pipelines API. Each candidate is
//! found, with its shape, absent, unreadable, or, for the API when the
//! caller can see no pipeline, unobserved.

use std::collections::BTreeSet;

use crate::json::Node;
use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;
use crate::probe::workspace::{Reply, described_columns, list_or_none, step, warehouse, word};
use crate::transport::Error;

const CHECK: CheckId = CheckId::V4;
/// The design page that records where later phases read refresh history.
const SITE: &str = "docs/mv-lineage/phase0/04-sql-probes.md#v4";
/// System tables that may hold refresh history: pipeline updates, and the
/// job runs that trigger refreshes.
const TABLES: [&str; 2] = [
    "system.lakeflow.pipeline_update_timeline",
    "system.lakeflow.job_run_timeline",
];
/// One pipeline is enough to see the shape of its updates.
const PIPELINES: &str = "/api/2.0/pipelines?max_results=1";
/// How the verdict names the updates API, without a pipeline ID.
const UPDATES: &str = "/api/2.0/pipelines/{id}/updates";
const FALLBACK: &str = "Read refresh history per pipeline from `/api/2.0/pipelines/<id>/updates`, and per materialized view from its pipeline's event log.";

/// What one candidate showed.
enum Seen {
    Found(String),
    Absent,
    Unreadable,
    Unobserved,
}

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    let warehouse_id = match warehouse(context, CHECK, SITE) {
        Ok(warehouse_id) => warehouse_id,
        Err(finding) => return Ok(finding),
    };
    let mut evidence = Vec::new();
    let mut seen = Vec::new();
    for table in TABLES {
        let name = table.rsplit('.').next().unwrap_or(table);
        let sql = format!("DESCRIBE TABLE {table}");
        let shown = match step(context, CHECK, name, SITE, &mut evidence, |transport| {
            transport.statement(&sql, warehouse_id)
        })? {
            Reply::Answer(result) => match described_columns(&result) {
                Some(columns) if !columns.is_empty() => {
                    Seen::Found(format!("{table} ({})", columns.join(", ")))
                }
                _ => Seen::Unreadable,
            },
            Reply::Error(error) => unanswered(&error),
            Reply::Failed(finding) => return Ok(finding),
        };
        seen.push((table, shown));
    }
    let api = match updates_api(context, &mut evidence)? {
        Ok(api) => api,
        Err(finding) => return Ok(finding),
    };
    seen.push((UPDATES, api));

    let named = |wanted: fn(&Seen) -> bool| {
        let names: Vec<&str> = seen
            .iter()
            .filter(|(_, shown)| wanted(shown))
            .map(|(name, _)| *name)
            .collect();
        list_or_none(&names)
    };
    let found: Vec<&str> = seen
        .iter()
        .filter_map(|(_, shown)| match shown {
            Seen::Found(shape) => Some(shape.as_str()),
            _ => None,
        })
        .collect();
    let absent = named(|shown| matches!(shown, Seen::Absent));
    let unreadable = named(|shown| matches!(shown, Seen::Unreadable));
    let unobserved = named(|shown| matches!(shown, Seen::Unobserved));
    if found.is_empty() {
        return Ok(Finding::unknown(
            CHECK,
            format!(
                "no candidate showed refresh history: absent {absent}; unreadable {unreadable}; unobserved {unobserved}"
            ),
            FALLBACK,
            evidence,
            SITE,
        ));
    }
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(format!(
            "found: {}; absent: {absent}; unreadable: {unreadable}; unobserved: {unobserved}",
            found.join("; ")
        )),
        evidence,
        SITE,
    ))
}

/// What the pipelines API shows: the updates of the first pipeline listed,
/// or the finding when the CLI could not run.
fn updates_api(
    context: &Context<'_>,
    evidence: &mut Vec<String>,
) -> Result<Result<Seen, Finding>, String> {
    let listed = match step(context, CHECK, "pipelines", SITE, evidence, |transport| {
        transport.api_get(PIPELINES)
    })? {
        Reply::Answer(response) => response,
        Reply::Error(error) => return Ok(Ok(unanswered(&error))),
        Reply::Failed(finding) => return Ok(Err(finding)),
    };
    let id = match first_pipeline(listed.body.as_ref().map(|body| body.root())) {
        Listed::First(id) => id,
        Listed::Empty => return Ok(Ok(Seen::Unobserved)),
        Listed::Malformed => return Ok(Ok(Seen::Unreadable)),
    };
    let path = format!("/api/2.0/pipelines/{id}/updates?max_results=1");
    Ok(
        match step(context, CHECK, "updates", SITE, evidence, |transport| {
            transport.api_get(&path)
        })? {
            Reply::Answer(response) => Ok(match response.body.as_ref() {
                Some(body) => match shape(body.root()) {
                    Updates::Shape(shape) => Seen::Found(format!("{UPDATES} ({shape})")),
                    // A pipeline that has never been updated has no history
                    // to show.
                    Updates::None => Seen::Unobserved,
                    Updates::Malformed => Seen::Unreadable,
                },
                None => Seen::Unreadable,
            }),
            Reply::Error(error) => Ok(unanswered(&error)),
            Reply::Failed(finding) => Err(finding),
        },
    )
}

/// A missing table or path is absent; any other error leaves the candidate
/// unreadable.
fn unanswered(error: &Error) -> Seen {
    if error.is_not_found() {
        Seen::Absent
    } else {
        Seen::Unreadable
    }
}

/// What the pipelines list holds.
enum Listed {
    /// The ID of the first pipeline, which is safe to put in a path.
    First(String),
    /// A valid list with no pipeline: an empty `statuses` array, or an empty
    /// object, as the API omits an empty array.
    Empty,
    /// No `statuses` array in a non-empty response, or a first pipeline
    /// without a plain `pipeline_id`.
    Malformed,
}

fn first_pipeline(body: Option<Node<'_>>) -> Listed {
    let Some(body) = body else {
        return Listed::Malformed;
    };
    let Some(statuses) = body.get("statuses") else {
        let empty = body
            .members()
            .is_some_and(|mut members| members.next().is_none());
        return if empty {
            Listed::Empty
        } else {
            Listed::Malformed
        };
    };
    let Some(mut statuses) = statuses.elements() else {
        return Listed::Malformed;
    };
    let Some(first) = statuses.next() else {
        return Listed::Empty;
    };
    match first.get("pipeline_id").and_then(Node::as_str) {
        Some(id) if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => {
            Listed::First(id.to_owned())
        }
        _ => Listed::Malformed,
    }
}

/// What an updates response shows.
enum Updates {
    /// The keys of the response and of its first update, such as
    /// `next_page_token, updates[]; updates[]: cause, state, update_id`.
    Shape(String),
    /// No update: no `updates` member, or an empty array.
    None,
    /// The body is not an object, or `updates` is not an array.
    Malformed,
}

fn shape(body: Node<'_>) -> Updates {
    let keys = |node: Node<'_>| -> BTreeSet<String> {
        node.members()
            .map(|members| members.map(|(key, _)| word(key)).collect())
            .unwrap_or_default()
    };
    if body.members().is_none() {
        return Updates::Malformed;
    }
    let Some(updates) = body.get("updates") else {
        return Updates::None;
    };
    let Some(mut updates) = updates.elements() else {
        return Updates::Malformed;
    };
    let Some(first) = updates.next() else {
        return Updates::None;
    };
    let top: Vec<String> = keys(body)
        .into_iter()
        .map(|key| {
            if key == "updates" {
                "updates[]".to_owned()
            } else {
                key
            }
        })
        .collect();
    let first: Vec<String> = keys(first).into_iter().collect();
    Updates::Shape(format!(
        "{}; updates[]: {}",
        top.join(", "),
        first.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::{PIPELINES, TABLES, run};
    use crate::evidence::RunDir;
    use crate::probe::model::{Finding, State, Verdict};
    use crate::probe::orchestrator::Context;
    use crate::test_support::{FakeTransport, TempDir, denied, missing_table};
    use crate::transport::Error;

    const DESCRIBE: [&str; 3] = ["col_name", "data_type", "comment"];
    const LISTED: &str = r#"{"statuses":[{"pipeline_id":"01a23b45-c67d"}]}"#;
    const UPDATES_PATH: &str = "/api/2.0/pipelines/01a23b45-c67d/updates?max_results=1";
    const UPDATES_BODY: &str = r#"{"updates":[{"update_id":"u","state":"COMPLETED","cause":"API_CALL"}],"next_page_token":"t"}"#;

    fn check(name: &str, transport: &FakeTransport) -> Finding {
        let temp = TempDir::new(name);
        let run_dir = RunDir::create(temp.path(), "lineage", 0).expect("create run");
        run(&Context {
            binary: "databricks".as_ref(),
            run: &run_dir,
            bundle_root: std::path::Path::new("."),
            target: "dev",
            allow_mutations: false,
            validation: &crate::probe::validation::Shared::default(),
            transport,
            warehouse_id: Some("w"),
            scope_catalog: None,
            shared: &crate::probe::workspace::Shared::default(),
        })
        .expect("run check")
    }

    fn described(transport: FakeTransport, table: &str, columns: &[&str]) -> FakeTransport {
        let rows: Vec<[Option<&str>; 3]> = columns
            .iter()
            .map(|&column| [Some(column), Some("string"), None])
            .collect();
        let rows: Vec<&[Option<&str>]> = rows.iter().map(|row| &row[..]).collect();
        transport.statement(&format!("DESCRIBE TABLE {table}"), &DESCRIBE, &rows)
    }

    fn verdict(finding: &Finding) -> &str {
        finding.verdict().map(Verdict::as_str).unwrap_or_default()
    }

    #[test]
    fn resolves_where_refresh_history_is() {
        let transport = described(
            FakeTransport::default(),
            TABLES[0],
            &["pipeline_id", "update_id", "period_start_time"],
        )
        .statement_error(&format!("DESCRIBE TABLE {}", TABLES[1]), missing_table())
        .get(PIPELINES, LISTED)
        .get(UPDATES_PATH, UPDATES_BODY);
        let finding = check("v4-resolved", &transport);
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            verdict(&finding),
            "found: system.lakeflow.pipeline_update_timeline (pipeline_id, update_id, period_start_time); /api/2.0/pipelines/{id}/updates (next_page_token, updates[]; updates[]: cause, state, update_id); absent: system.lakeflow.job_run_timeline; unreadable: none; unobserved: none"
        );
        assert_eq!(transport.asked().len(), 4);
    }

    #[test]
    fn records_unreadable_and_unobserved_candidates() {
        let transport = described(FakeTransport::default(), TABLES[1], &["run_id"])
            .statement_error(&format!("DESCRIBE TABLE {}", TABLES[0]), denied())
            .get(PIPELINES, r#"{"statuses":[]}"#);
        let finding = check("v4-partial", &transport);
        assert_eq!(finding.state(), State::Resolved);
        assert_eq!(
            verdict(&finding),
            "found: system.lakeflow.job_run_timeline (run_id); absent: none; unreadable: system.lakeflow.pipeline_update_timeline; unobserved: /api/2.0/pipelines/{id}/updates"
        );
    }

    /// A transport on which neither candidate table exists.
    fn missing(transport: FakeTransport) -> FakeTransport {
        TABLES.iter().fold(transport, |transport, table| {
            transport.statement_error(&format!("DESCRIBE TABLE {table}"), missing_table())
        })
    }

    fn assert_unknown(cases: Vec<(&str, FakeTransport, &str)>) {
        for (name, transport, reason) in cases {
            let finding = check(name, &transport);
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
            assert!(finding.fallback().is_some(), "{name}");
        }
    }

    #[test]
    fn reports_no_history_anywhere_as_unknown() {
        assert_unknown(vec![
            (
                "v4-nothing",
                missing(FakeTransport::default()).get(PIPELINES, "{}"),
                "no candidate showed refresh history: absent system.lakeflow.pipeline_update_timeline, system.lakeflow.job_run_timeline; unreadable none; unobserved /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-empty-describe",
                TABLES
                    .iter()
                    .fold(FakeTransport::default(), |transport, table| {
                        transport.statement(&format!("DESCRIBE TABLE {table}"), &DESCRIBE, &[])
                    })
                    .get(PIPELINES, "{}"),
                "unreadable system.lakeflow.pipeline_update_timeline, system.lakeflow.job_run_timeline",
            ),
        ]);
    }

    #[test]
    fn tells_an_unobserved_api_from_an_unreadable_one() {
        assert_unknown(vec![
            (
                "v4-list-denied",
                missing(FakeTransport::default()).get_error(
                    PIPELINES,
                    Error::Http {
                        status: 403,
                        error_code: None,
                        message: None,
                    },
                ),
                "unreadable /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-updates-missing",
                missing(FakeTransport::default())
                    .get(PIPELINES, LISTED)
                    .get_error(
                        UPDATES_PATH,
                        Error::Http {
                            status: 404,
                            error_code: None,
                            message: None,
                        },
                    ),
                "absent system.lakeflow.pipeline_update_timeline, system.lakeflow.job_run_timeline, /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-no-history",
                missing(FakeTransport::default())
                    .get(PIPELINES, LISTED)
                    .get(UPDATES_PATH, "{}"),
                "unobserved /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-empty-history",
                missing(FakeTransport::default())
                    .get(PIPELINES, LISTED)
                    .get(UPDATES_PATH, r#"{"updates":[]}"#),
                "unobserved /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-updates-not-array",
                missing(FakeTransport::default())
                    .get(PIPELINES, LISTED)
                    .get(UPDATES_PATH, r#"{"updates":{}}"#),
                "unreadable /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-updates-not-object",
                missing(FakeTransport::default())
                    .get(PIPELINES, LISTED)
                    .get(UPDATES_PATH, "[]"),
                "unreadable /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-updates-no-body",
                missing(FakeTransport::default())
                    .get(PIPELINES, LISTED)
                    .get_answer(UPDATES_PATH, Some(200), None),
                "unreadable /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-empty-statuses",
                missing(FakeTransport::default()).get(PIPELINES, r#"{"statuses":[]}"#),
                "unobserved /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-no-statuses",
                missing(FakeTransport::default()).get(PIPELINES, r#"{"next_page_token":"t"}"#),
                "unreadable /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-statuses-not-array",
                missing(FakeTransport::default()).get(PIPELINES, r#"{"statuses":{}}"#),
                "unreadable /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-no-pipeline-id",
                missing(FakeTransport::default()).get(PIPELINES, r#"{"statuses":[{"name":"p"}]}"#),
                "unreadable /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-list-no-body",
                missing(FakeTransport::default()).get_answer(PIPELINES, Some(200), None),
                "unreadable /api/2.0/pipelines/{id}/updates",
            ),
            (
                "v4-unsafe-id",
                missing(FakeTransport::default())
                    .get(PIPELINES, r#"{"statuses":[{"pipeline_id":"../x"}]}"#),
                "unreadable /api/2.0/pipelines/{id}/updates",
            ),
        ]);
    }

    #[test]
    fn reports_missing_inputs_and_a_cli_that_cannot_run_as_failed() {
        let temp = TempDir::new("v4-no-warehouse");
        let run_dir = RunDir::create(temp.path(), "lineage", 0).expect("create run");
        let finding = run(&Context {
            binary: "databricks".as_ref(),
            run: &run_dir,
            bundle_root: std::path::Path::new("."),
            target: "dev",
            allow_mutations: false,
            validation: &crate::probe::validation::Shared::default(),
            transport: &FakeTransport::default(),
            warehouse_id: None,
            scope_catalog: None,
            shared: &crate::probe::workspace::Shared::default(),
        })
        .expect("run check");
        assert_eq!(finding.state(), State::Failed);
        let finding = check("v4-unrunnable", &FakeTransport::unrunnable());
        assert_eq!(finding.state(), State::Failed);
    }
}

//! The `v11` check (#33): which `table_type` metric views have, and how
//! shared catalogs are represented.
//!
//! Reads the distinct `table_type` values in the scope catalog's
//! `information_schema.tables`, and the `catalog_type` of every catalog the
//! caller can see from `/api/2.1/unity-catalog/catalogs`.

use std::collections::BTreeSet;

use crate::json::Node;
use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;
use crate::probe::workspace::{Reply, describe, scope_catalog, step, warehouse, word, words};
use crate::text;

const CHECK: CheckId = CheckId::V11;
/// The design page that records how later phases find metric views and
/// shared catalogs.
const SITE: &str = "docs/mv-lineage/phase0/04-sql-probes.md#v11";
/// Without `max_results`, the API returns every catalog in one response.
const CATALOGS: &str = "/api/2.1/unity-catalog/catalogs";
const FALLBACK: &str = "Treat `table_type` `METRIC_VIEW` as a metric view and `catalog_type` `DELTASHARING_CATALOG` as a shared catalog, as Databricks documents them.";

/// The distinct `table_type` values in `catalog`, which is always quoted.
fn table_types(catalog: &str) -> String {
    format!(
        "SELECT DISTINCT table_type FROM {}.information_schema.tables ORDER BY table_type",
        text::identifier(catalog)
    )
}

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    let (warehouse_id, catalog) = match (
        warehouse(context, CHECK, SITE),
        scope_catalog(context, CHECK, SITE),
    ) {
        (Ok(warehouse_id), Ok(catalog)) => (warehouse_id, catalog),
        (Err(finding), _) | (_, Err(finding)) => return Ok(finding),
    };
    let mut evidence = Vec::new();
    let unknown = |reason: String, evidence: &[String]| {
        Finding::unknown(CHECK, reason, FALLBACK, evidence.to_vec(), SITE)
    };

    let sql = table_types(catalog);
    let types = match step(
        context,
        CHECK,
        "table-types",
        SITE,
        &mut evidence,
        |transport| transport.statement(&sql, warehouse_id),
    )? {
        Reply::Answer(result) => words(&result, "table_type").unwrap_or_default(),
        Reply::Error(error) => {
            return Ok(unknown(
                format!(
                    "reading the scope catalog's `information_schema.tables` {}",
                    describe(&error)
                ),
                &evidence,
            ));
        }
        Reply::Failed(finding) => return Ok(finding),
    };
    let Some(metric) = types.iter().find(|kind| kind.contains("METRIC")) else {
        return Ok(unknown(
            "no table in the scope catalog has a metric-view `table_type`; choose a scope catalog with a metric view".to_owned(),
            &evidence,
        ));
    };

    let catalogs = match step(
        context,
        CHECK,
        "catalogs",
        SITE,
        &mut evidence,
        |transport| transport.api_get(CATALOGS),
    )? {
        Reply::Answer(response) => response.body,
        Reply::Error(error) => {
            return Ok(unknown(
                format!("listing catalogs from `{CATALOGS}` {}", describe(&error)),
                &evidence,
            ));
        }
        Reply::Failed(finding) => return Ok(finding),
    };
    let Some(listed) = catalogs
        .as_ref()
        .and_then(|body| body.root().get("catalogs"))
        .and_then(Node::elements)
    else {
        return Ok(unknown(
            format!("`{CATALOGS}` returned no `catalogs` array"),
            &evidence,
        ));
    };
    let mut catalog_types = BTreeSet::new();
    let mut scope_type = None;
    for listed in listed {
        let Some(kind) = listed.get("catalog_type").and_then(Node::as_str) else {
            continue;
        };
        let kind = word(kind);
        // Unity Catalog compares names case-insensitively.
        let is_scope = listed
            .get("name")
            .and_then(Node::as_str)
            .is_some_and(|name| name.eq_ignore_ascii_case(catalog));
        if is_scope {
            scope_type = Some(kind.clone());
        }
        catalog_types.insert(kind);
    }
    let Some(shared) = catalog_types.iter().find(|kind| kind.contains("SHARING")) else {
        return Ok(unknown(
            "no catalog the caller can see has a shared-catalog `catalog_type`".to_owned(),
            &evidence,
        ));
    };
    let catalog_types: Vec<&str> = catalog_types.iter().map(String::as_str).collect();
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(format!(
            "metric views: table_type {metric}; shared catalogs: catalog_type {shared}; scope catalog: {}; catalog_type values: {}",
            scope_type.as_deref().unwrap_or("not listed"),
            catalog_types.join(", ")
        )),
        evidence,
        SITE,
    ))
}

#[cfg(test)]
mod tests {
    use super::{CATALOGS, run, table_types};
    use crate::evidence::RunDir;
    use crate::probe::model::{Finding, State, Verdict};
    use crate::probe::orchestrator::Context;
    use crate::safety::is_read_statement;
    use crate::test_support::{FakeTransport, TempDir, missing_table};
    use crate::transport::Error;

    const LISTED: &str = r#"{"catalogs":[
        {"name":"MAIN","catalog_type":"MANAGED_CATALOG"},
        {"name":"partner","catalog_type":"DELTASHARING_CATALOG"},
        {"name":"system","catalog_type":"SYSTEM_CATALOG"},
        {"name":"untyped"}
    ]}"#;

    fn check(name: &str, transport: &FakeTransport) -> Finding {
        let temp = TempDir::new(name);
        let run_dir = RunDir::create(temp.path(), "lineage", 0).expect("create run");
        run(&Context {
            binary: "databricks".as_ref(),
            run: &run_dir,
            transport,
            warehouse_id: Some("w"),
            scope_catalog: Some("main"),
        })
        .expect("run check")
    }

    fn with_types(types: &[&str]) -> FakeTransport {
        let rows: Vec<[Option<&str>; 1]> = types.iter().map(|&kind| [Some(kind)]).collect();
        let rows: Vec<&[Option<&str>]> = rows.iter().map(|row| &row[..]).collect();
        FakeTransport::default().statement("information_schema.tables", &["table_type"], &rows)
    }

    #[test]
    fn quotes_the_scope_catalog_as_an_identifier() {
        assert_eq!(
            table_types("main"),
            "SELECT DISTINCT table_type FROM `main`.information_schema.tables ORDER BY table_type"
        );
        for hostile in ["x`; DROP TABLE t; --", "it's", r"a\b"] {
            assert!(is_read_statement(&table_types(hostile)), "{hostile}");
        }
    }

    #[test]
    fn resolves_metric_views_and_shared_catalogs() {
        let transport = with_types(&["MANAGED", "METRIC_VIEW", "VIEW"]).get(CATALOGS, LISTED);
        let finding = check("v11-resolved", &transport);
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some(
                "metric views: table_type METRIC_VIEW; shared catalogs: catalog_type DELTASHARING_CATALOG; scope catalog: MANAGED_CATALOG; catalog_type values: DELTASHARING_CATALOG, MANAGED_CATALOG, SYSTEM_CATALOG"
            )
        );
        assert_eq!(transport.asked()[1], "GET /api/2.1/unity-catalog/catalogs");
    }

    #[test]
    fn says_when_the_scope_catalog_is_not_listed() {
        let listed = r#"{"catalogs":[{"name":"partner","catalog_type":"DELTASHARING_CATALOG"}]}"#;
        let transport = with_types(&["METRIC_VIEW"]).get(CATALOGS, listed);
        let verdict = check("v11-unlisted", &transport)
            .verdict()
            .map(|verdict| verdict.as_str().to_owned())
            .unwrap_or_default();
        assert!(verdict.contains("scope catalog: not listed;"), "{verdict}");
    }

    #[test]
    fn reports_what_it_cannot_observe_as_unknown() {
        for (name, transport, reason) in [
            (
                "v11-no-catalog",
                FakeTransport::default()
                    .statement_error("information_schema.tables", missing_table()),
                "reading the scope catalog's `information_schema.tables` ended FAILED (not found)",
            ),
            (
                "v11-no-metric",
                with_types(&["MANAGED", "VIEW"]),
                "no table in the scope catalog has a metric-view `table_type`",
            ),
            (
                "v11-list-denied",
                with_types(&["METRIC_VIEW"]).get_error(
                    CATALOGS,
                    Error::Http {
                        status: 403,
                        error_code: None,
                        message: None,
                    },
                ),
                "listing catalogs from `/api/2.1/unity-catalog/catalogs` failed with HTTP 403 (permission denied)",
            ),
            (
                "v11-no-array",
                with_types(&["METRIC_VIEW"]).get(CATALOGS, "{}"),
                "returned no `catalogs` array",
            ),
            (
                "v11-no-body",
                with_types(&["METRIC_VIEW"]).get_answer(CATALOGS, Some(200), None),
                "returned no `catalogs` array",
            ),
            (
                "v11-no-share",
                with_types(&["METRIC_VIEW"]).get(
                    CATALOGS,
                    r#"{"catalogs":[{"name":"main","catalog_type":"MANAGED_CATALOG"}]}"#,
                ),
                "no catalog the caller can see has a shared-catalog `catalog_type`",
            ),
        ] {
            let finding = check(name, &transport);
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
            assert!(finding.fallback().is_some(), "{name}");
        }
    }

    #[test]
    fn reports_a_cli_that_cannot_run_as_failed() {
        let finding = check("v11-unrunnable", &FakeTransport::unrunnable());
        assert_eq!(finding.state(), State::Failed);
    }
}

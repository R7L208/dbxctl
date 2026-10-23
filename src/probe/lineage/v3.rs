//! The `v3` check (#33): how materialized views appear in Unity Catalog, and
//! the shape of `system.lakeflow.pipelines`, including `pipeline_type`.
//!
//! Reads the `table_type` values in the scope catalog's
//! `information_schema.tables`, the columns of `system.lakeflow.pipelines`,
//! and, when it has one, the values of its `pipeline_type` column.

use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;
use crate::probe::workspace::{
    Reply, describe, described_columns, scope_catalog, step, warehouse, words,
};
use crate::text;

const CHECK: CheckId = CheckId::V3;
/// The design page that records how later phases find materialized views.
const SITE: &str = "docs/mv-lineage/phase0/04-sql-probes.md#v3";
const PIPELINES: &str = "system.lakeflow.pipelines";
const DESCRIBE_PIPELINES: &str = "DESCRIBE TABLE system.lakeflow.pipelines";
const PIPELINE_TYPES: &str = "SELECT pipeline_type, count(*) AS pipelines FROM system.lakeflow.pipelines GROUP BY pipeline_type ORDER BY pipeline_type";
const FALLBACK: &str = "Identify materialized views by `table_type` `MATERIALIZED_VIEW` in `information_schema.tables`, and read pipeline metadata from `/api/2.0/pipelines/<id>` instead of `system.lakeflow.pipelines`.";

/// The `table_type` values in `catalog`, which is always quoted.
fn table_types(catalog: &str) -> String {
    format!(
        "SELECT table_type, count(*) AS tables FROM {}.information_schema.tables GROUP BY table_type ORDER BY table_type",
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
    let Some(materialized) = types.iter().find(|kind| kind.contains("MATERIALIZED")) else {
        return Ok(unknown(
            "no table in the scope catalog has a materialized-view `table_type`; choose a scope catalog with a materialized view".to_owned(),
            &evidence,
        ));
    };

    let columns = match step(
        context,
        CHECK,
        "pipelines-columns",
        SITE,
        &mut evidence,
        |transport| transport.statement(DESCRIBE_PIPELINES, warehouse_id),
    )? {
        Reply::Answer(result) => described_columns(&result).unwrap_or_default(),
        Reply::Error(error) => {
            return Ok(unknown(
                format!("describing `{PIPELINES}` {}", describe(&error)),
                &evidence,
            ));
        }
        Reply::Failed(finding) => return Ok(finding),
    };
    if columns.is_empty() {
        return Ok(unknown(
            format!("describing `{PIPELINES}` listed no columns"),
            &evidence,
        ));
    }

    let pipeline_types = if columns.iter().any(|column| column == "pipeline_type") {
        match step(
            context,
            CHECK,
            "pipeline-types",
            SITE,
            &mut evidence,
            |transport| transport.statement(PIPELINE_TYPES, warehouse_id),
        )? {
            Reply::Answer(result) => list(&words(&result, "pipeline_type").unwrap_or_default()),
            Reply::Error(error) => {
                return Ok(unknown(
                    format!(
                        "reading `pipeline_type` from `{PIPELINES}` {}",
                        describe(&error)
                    ),
                    &evidence,
                ));
            }
            Reply::Failed(finding) => return Ok(finding),
        }
    } else {
        "no such column".to_owned()
    };

    Ok(Finding::resolved(
        CHECK,
        Verdict::new(format!(
            "materialized views: table_type {materialized}; scope table_type values: {}; {PIPELINES} columns: {}; pipeline_type values: {pipeline_types}",
            list(&types),
            list(&columns),
        )),
        evidence,
        SITE,
    ))
}

fn list(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::{DESCRIBE_PIPELINES, PIPELINE_TYPES, run, table_types};
    use crate::evidence::RunDir;
    use crate::probe::model::{Finding, State, Verdict};
    use crate::probe::orchestrator::Context;
    use crate::safety::is_read_statement;
    use crate::test_support::{FakeTransport, TempDir, denied, missing_table};

    const DESCRIBE: [&str; 3] = ["col_name", "data_type", "comment"];

    fn check(name: &str, transport: &FakeTransport, catalog: Option<&str>) -> Finding {
        let temp = TempDir::new(name);
        let run_dir = RunDir::create(temp.path(), "lineage", 0).expect("create run");
        run(&Context {
            binary: "databricks".as_ref(),
            run: &run_dir,
            transport,
            warehouse_id: Some("w"),
            scope_catalog: catalog,
        })
        .expect("run check")
    }

    fn with_types(types: &[&str]) -> FakeTransport {
        let rows: Vec<[Option<&str>; 2]> =
            types.iter().map(|&kind| [Some(kind), Some("1")]).collect();
        let rows: Vec<&[Option<&str>]> = rows.iter().map(|row| &row[..]).collect();
        FakeTransport::default().statement(
            "information_schema.tables",
            &["table_type", "tables"],
            &rows,
        )
    }

    fn described<'a>(columns: &[&'a str]) -> Vec<[Option<&'a str>; 3]> {
        columns
            .iter()
            .map(|&column| [Some(column), Some("string"), None])
            .collect()
    }

    fn statement_rows<'a>(rows: &'a [[Option<&'a str>; 3]]) -> Vec<&'a [Option<&'a str>]> {
        rows.iter().map(|row| &row[..]).collect()
    }

    #[test]
    fn quotes_the_scope_catalog_as_an_identifier() {
        assert_eq!(
            table_types("main"),
            "SELECT table_type, count(*) AS tables FROM `main`.information_schema.tables GROUP BY table_type ORDER BY table_type"
        );
        for hostile in ["x`; DROP TABLE t; --", "it's", r"a\b", "x'.y"] {
            assert!(is_read_statement(&table_types(hostile)), "{hostile}");
        }
        assert!(is_read_statement(DESCRIBE_PIPELINES));
        assert!(is_read_statement(PIPELINE_TYPES));
    }

    #[test]
    fn resolves_the_table_types_and_pipeline_shape() {
        let columns = described(&["pipeline_id", "pipeline_type", "name"]);
        let transport = with_types(&["MANAGED", "MATERIALIZED_VIEW", "STREAMING_TABLE"])
            .statement(DESCRIBE_PIPELINES, &DESCRIBE, &statement_rows(&columns))
            .statement(
                "SELECT pipeline_type",
                &["pipeline_type", "pipelines"],
                &[&[Some("ETL_PIPELINE"), Some("2")], &[Some("MV"), Some("1")]],
            );
        let finding = check("v3-resolved", &transport, Some("main"));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some(
                "materialized views: table_type MATERIALIZED_VIEW; scope table_type values: MANAGED, MATERIALIZED_VIEW, STREAMING_TABLE; system.lakeflow.pipelines columns: pipeline_id, pipeline_type, name; pipeline_type values: ETL_PIPELINE, MV"
            )
        );
        assert_eq!(transport.asked().len(), 3);
        assert!(transport.asked()[0].contains("`main`.information_schema.tables"));
    }

    #[test]
    fn records_a_pipelines_table_without_pipeline_type() {
        let columns = described(&["pipeline_id", "name"]);
        let transport = with_types(&["MATERIALIZED_VIEW"]).statement(
            DESCRIBE_PIPELINES,
            &DESCRIBE,
            &statement_rows(&columns),
        );
        let finding = check("v3-no-type", &transport, Some("main"));
        assert_eq!(finding.state(), State::Resolved);
        assert!(
            finding
                .verdict()
                .map(Verdict::as_str)
                .unwrap_or_default()
                .ends_with("columns: pipeline_id, name; pipeline_type values: no such column"),
            "{finding:?}"
        );
        // The values are not asked for.
        assert_eq!(transport.asked().len(), 2);
    }

    #[test]
    fn reports_what_it_cannot_read_as_unknown() {
        let columns = described(&["pipeline_type"]);
        for (name, transport, reason) in [
            (
                "v3-no-catalog",
                FakeTransport::default()
                    .statement_error("information_schema.tables", missing_table()),
                "reading the scope catalog's `information_schema.tables` ended FAILED (not found)",
            ),
            (
                "v3-no-mv",
                with_types(&["MANAGED", "VIEW"]),
                "no table in the scope catalog has a materialized-view `table_type`",
            ),
            (
                "v3-no-pipelines",
                with_types(&["MATERIALIZED_VIEW"])
                    .statement_error(DESCRIBE_PIPELINES, missing_table()),
                "describing `system.lakeflow.pipelines` ended FAILED (not found)",
            ),
            (
                "v3-empty-describe",
                with_types(&["MATERIALIZED_VIEW"]).statement(DESCRIBE_PIPELINES, &DESCRIBE, &[]),
                "listed no columns",
            ),
            (
                "v3-types-denied",
                with_types(&["MATERIALIZED_VIEW"])
                    .statement(DESCRIBE_PIPELINES, &DESCRIBE, &statement_rows(&columns))
                    .statement_error("SELECT pipeline_type", denied()),
                "reading `pipeline_type` from `system.lakeflow.pipelines` ended FAILED (permission denied)",
            ),
        ] {
            let finding = check(name, &transport, Some("main"));
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
            assert!(finding.fallback().is_some(), "{name}");
        }
    }

    #[test]
    fn reports_missing_inputs_and_a_cli_that_cannot_run_as_failed() {
        let finding = check("v3-no-scope", &FakeTransport::default(), None);
        assert_eq!(finding.state(), State::Failed);
        assert_eq!(
            finding.reason(),
            Some("--scope-catalog was not given, though v3 reads it")
        );
        let finding = check("v3-unrunnable", &FakeTransport::unrunnable(), Some("main"));
        assert_eq!(finding.state(), State::Failed);
    }
}

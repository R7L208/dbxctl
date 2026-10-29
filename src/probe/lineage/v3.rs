//! The `v3` check (#33): how materialized views appear in Unity Catalog, and
//! the shape of `system.lakeflow.pipelines`, including `pipeline_type`.
//!
//! Reads the `table_type` values in the scope catalog's
//! `information_schema.tables`, the columns of `system.lakeflow.pipelines`,
//! and, when it has one, the values of its `pipeline_type` column.

use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;
use crate::probe::workspace::{
    Read, Reply, describe, described_columns, list_or_none, required_words, scope_catalog,
    scope_table_types, step, warehouse,
};

const CHECK: CheckId = CheckId::V3;
/// The design page that records how later phases find materialized views.
const SITE: &str = "docs/mv-lineage/phase0/04-sql-probes.md#v3";
const PIPELINES: &str = "system.lakeflow.pipelines";
const DESCRIBE_PIPELINES: &str = "DESCRIBE TABLE system.lakeflow.pipelines";
const PIPELINE_TYPES: &str = "SELECT pipeline_type, count(*) AS pipelines FROM system.lakeflow.pipelines GROUP BY pipeline_type ORDER BY pipeline_type";
const FALLBACK: &str = "Identify materialized views by `table_type` `MATERIALIZED_VIEW` in `information_schema.tables`, and read pipeline metadata from `/api/2.0/pipelines/<id>` instead of `system.lakeflow.pipelines`.";

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

    let types = match scope_table_types(context, CHECK, SITE, warehouse_id, catalog, &mut evidence)?
    {
        Read::Seen(types) => types,
        Read::Unknown(reason) => return Ok(unknown(reason, &evidence)),
        Read::Failed(finding) => return Ok(finding),
    };
    let Some(materialized) = types.iter().find(|kind| kind.contains("MATERIALIZED")) else {
        return Ok(unknown(
            "no table in the scope catalog has a materialized-view `table_type`; choose a scope catalog with a materialized view".to_owned(),
            &evidence,
        ));
    };

    let (columns, pipeline_types) = match pipelines(context, warehouse_id, &mut evidence)? {
        Read::Seen(shape) => shape,
        Read::Unknown(reason) => return Ok(unknown(reason, &evidence)),
        Read::Failed(finding) => return Ok(finding),
    };

    Ok(Finding::resolved(
        CHECK,
        Verdict::new(format!(
            "materialized views: table_type {materialized}; scope table_type values: {}; {PIPELINES} columns: {}; pipeline_type values: {pipeline_types}",
            list_or_none(&types),
            list_or_none(&columns),
        )),
        evidence,
        SITE,
    ))
}

/// The columns of `system.lakeflow.pipelines` and its `pipeline_type`
/// values, or `no such column`.
fn pipelines(
    context: &Context<'_>,
    warehouse_id: &str,
    evidence: &mut Vec<String>,
) -> Result<Read<(Vec<String>, String)>, String> {
    let columns = match step(
        context,
        CHECK,
        "pipelines-columns",
        SITE,
        evidence,
        |transport| transport.statement(DESCRIBE_PIPELINES, warehouse_id),
    )? {
        Reply::Answer(result) => described_columns(&result).unwrap_or_default(),
        Reply::Error(error) => {
            return Ok(Read::Unknown(format!(
                "describing `{PIPELINES}` {}",
                describe(&error)
            )));
        }
        Reply::Failed(finding) => return Ok(Read::Failed(finding)),
    };
    if columns.is_empty() {
        return Ok(Read::Unknown(format!(
            "describing `{PIPELINES}` listed no columns"
        )));
    }

    let pipeline_types = if columns.iter().any(|column| column == "pipeline_type") {
        match step(
            context,
            CHECK,
            "pipeline-types",
            SITE,
            evidence,
            |transport| transport.statement(PIPELINE_TYPES, warehouse_id),
        )? {
            Reply::Answer(result) => match required_words(&result, "pipeline_type") {
                Ok(types) => list_or_none(&types),
                Err(shape) => {
                    return Ok(Read::Unknown(format!("`{PIPELINES}` {shape}")));
                }
            },
            Reply::Error(error) => {
                return Ok(Read::Unknown(format!(
                    "reading `pipeline_type` from `{PIPELINES}` {}",
                    describe(&error)
                )));
            }
            Reply::Failed(finding) => return Ok(Read::Failed(finding)),
        }
    } else {
        "no such column".to_owned()
    };

    Ok(Read::Seen((columns, pipeline_types)))
}

#[cfg(test)]
mod tests {
    use super::{DESCRIBE_PIPELINES, PIPELINE_TYPES, run};
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
            shared: &crate::probe::workspace::Shared::default(),
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
    fn its_statements_are_read_only() {
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
    fn reports_an_unexpected_tables_shape_as_unknown() {
        for (case, transport, reason) in [
            (
                "v3-no-type-column",
                FakeTransport::default().statement(
                    "information_schema.tables",
                    &["kind"],
                    &[&[Some("MANAGED")]],
                ),
                "the scope catalog's `information_schema.tables` has no `table_type` column",
            ),
            (
                "v3-null-type",
                FakeTransport::default().statement(
                    "information_schema.tables",
                    &["table_type", "tables"],
                    &[&[Some("MANAGED"), Some("1")], &[None, Some("2")]],
                ),
                "the scope catalog's `information_schema.tables` has a null `table_type`",
            ),
        ] {
            let finding = check(case, &transport, Some("main"));
            assert_eq!(finding.state(), State::Unknown, "{case}");
            assert_eq!(finding.reason(), Some(reason), "{case}");
        }
    }

    #[test]
    fn reports_an_unexpected_pipeline_types_shape_as_unknown() {
        let columns = described(&["pipeline_type"]);
        let one: &[&[Option<&str>]] = &[&[Some("MV"), Some("1")]];
        let null: &[&[Option<&str>]] = &[&[Some("MV"), Some("1")], &[None, Some("2")]];
        for (case, result_columns, rows, reason) in [
            (
                "v3-no-pipeline-type-column",
                &["kind", "pipelines"],
                one,
                "`system.lakeflow.pipelines` has no `pipeline_type` column",
            ),
            (
                "v3-null-pipeline-type",
                &["pipeline_type", "pipelines"],
                null,
                "`system.lakeflow.pipelines` has a null `pipeline_type`",
            ),
        ] {
            let transport = with_types(&["MATERIALIZED_VIEW"])
                .statement(DESCRIBE_PIPELINES, &DESCRIBE, &statement_rows(&columns))
                .statement("SELECT pipeline_type", result_columns, rows);
            let finding = check(case, &transport, Some("main"));
            assert_eq!(finding.state(), State::Unknown, "{case}");
            assert_eq!(finding.reason(), Some(reason), "{case}");
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

//! The `v9` check (#33): does table lineage attribute a pipeline's writes to
//! the pipeline, by its ID?
//!
//! Checks that `system.access.table_lineage` has the columns the question
//! needs, then counts the last 30 days of writes by `entity_type`, and how
//! many of each carry an `entity_id`.

use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;
use crate::probe::workspace::{Reply, cell, describe, described_columns, step, warehouse, words};

const CHECK: CheckId = CheckId::V9;
/// The design page that records how later phases attribute writes.
const SITE: &str = "docs/mv-lineage/phase0/04-sql-probes.md#v9";
const LINEAGE: &str = "system.access.table_lineage";
const DESCRIBE_LINEAGE: &str = "DESCRIBE TABLE system.access.table_lineage";
/// The columns the writes query reads.
const NEEDED: [&str; 4] = [
    "entity_type",
    "entity_id",
    "target_table_full_name",
    "event_date",
];
/// A write has a target table. Thirty days bounds the scan.
const WRITES: &str = "SELECT entity_type, count(*) AS writes, count(entity_id) AS with_entity_id FROM system.access.table_lineage WHERE target_table_full_name IS NOT NULL AND event_date >= date_sub(current_date(), 30) GROUP BY entity_type ORDER BY entity_type";
/// The `entity_type` lineage records for a pipeline.
const PIPELINE: &str = "PIPELINE";
const FALLBACK: &str = "Attribute materialized-view writes to pipelines from the pipeline definitions, in the bundle configuration and `/api/2.0/pipelines/<id>`, not from lineage.";

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    let warehouse_id = match warehouse(context, CHECK, SITE) {
        Ok(warehouse_id) => warehouse_id,
        Err(finding) => return Ok(finding),
    };
    let mut evidence = Vec::new();
    let unknown = |reason: String, evidence: &[String]| {
        Finding::unknown(CHECK, reason, FALLBACK, evidence.to_vec(), SITE)
    };

    let columns = match step(
        context,
        CHECK,
        "columns",
        SITE,
        &mut evidence,
        |transport| transport.statement(DESCRIBE_LINEAGE, warehouse_id),
    )? {
        Reply::Answer(result) => described_columns(&result).unwrap_or_default(),
        Reply::Error(error) => {
            return Ok(unknown(
                format!("describing `{LINEAGE}` {}", describe(&error)),
                &evidence,
            ));
        }
        Reply::Failed(finding) => return Ok(finding),
    };
    if let Some(missing) = NEEDED
        .iter()
        .find(|needed| !columns.iter().any(|column| column == *needed))
    {
        return Ok(unknown(
            format!("`{LINEAGE}` has no `{missing}` column"),
            &evidence,
        ));
    }

    let writes = match step(context, CHECK, "writes", SITE, &mut evidence, |transport| {
        transport.statement(WRITES, warehouse_id)
    })? {
        Reply::Answer(result) => result,
        Reply::Error(error) => {
            return Ok(unknown(
                format!("counting writes in `{LINEAGE}` {}", describe(&error)),
                &evidence,
            ));
        }
        Reply::Failed(finding) => return Ok(finding),
    };
    let count = |row: &[Option<String>], column| {
        cell(&writes, row, column).and_then(|count| count.parse::<u64>().ok())
    };
    let pipeline = writes
        .rows
        .iter()
        .find(|row| cell(&writes, row, "entity_type") == Some(PIPELINE))
        .map(|row| (count(row, "writes"), count(row, "with_entity_id")));
    let (writes_count, with_id) = match pipeline {
        None | Some((Some(0), _)) => {
            return Ok(unknown(
                format!(
                    "no write in the last 30 days of `{LINEAGE}` has entity_type `{PIPELINE}`; run the check after a pipeline has refreshed"
                ),
                &evidence,
            ));
        }
        Some((Some(writes), Some(with_id))) if with_id <= writes => (writes, with_id),
        Some((Some(_), Some(_))) => {
            return Ok(unknown(
                "the `PIPELINE` row counts more writes with an `entity_id` than writes".to_owned(),
                &evidence,
            ));
        }
        Some(_) => {
            return Ok(unknown(
                "the `PIPELINE` row of the write counts is not a pair of counts".to_owned(),
                &evidence,
            ));
        }
    };
    let answer = if with_id == writes_count {
        "yes: every PIPELINE write in the last 30 days has an entity_id"
    } else if with_id == 0 {
        "no: no PIPELINE write in the last 30 days has an entity_id"
    } else {
        "partly: some PIPELINE writes in the last 30 days have no entity_id"
    };
    let entity_types = words(&writes, "entity_type").unwrap_or_default();
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(format!("{answer}; writers: {}", entity_types.join(", "))),
        evidence,
        SITE,
    ))
}

#[cfg(test)]
mod tests {
    use super::{DESCRIBE_LINEAGE, NEEDED, WRITES, run};
    use crate::evidence::RunDir;
    use crate::probe::model::{Finding, State, Verdict};
    use crate::probe::orchestrator::Context;
    use crate::safety::is_read_statement;
    use crate::test_support::{FakeTransport, TempDir, denied, missing_table};

    const DESCRIBE: [&str; 3] = ["col_name", "data_type", "comment"];
    const COUNTS: [&str; 3] = ["entity_type", "writes", "with_entity_id"];

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

    /// A lineage table with `columns`, answering the writes query with
    /// `counts`.
    fn lineage(columns: &[&str], counts: &[&[Option<&str>]]) -> FakeTransport {
        let described: Vec<[Option<&str>; 3]> = columns
            .iter()
            .map(|&column| [Some(column), Some("string"), None])
            .collect();
        let described: Vec<&[Option<&str>]> = described.iter().map(|row| &row[..]).collect();
        FakeTransport::default()
            .statement(DESCRIBE_LINEAGE, &DESCRIBE, &described)
            .statement("count(entity_id)", &COUNTS, counts)
    }

    fn verdict(finding: &Finding) -> &str {
        finding.verdict().map(Verdict::as_str).unwrap_or_default()
    }

    #[test]
    fn its_statements_are_read_only() {
        assert!(is_read_statement(DESCRIBE_LINEAGE));
        assert!(is_read_statement(WRITES));
    }

    #[test]
    fn resolves_whether_pipeline_writes_carry_an_id() {
        for (name, pipeline, answer) in [
            (
                "v9-yes",
                [Some("PIPELINE"), Some("4"), Some("4")],
                "yes: every PIPELINE write in the last 30 days has an entity_id; writers: JOB, PIPELINE",
            ),
            (
                "v9-no",
                [Some("PIPELINE"), Some("4"), Some("0")],
                "no: no PIPELINE write in the last 30 days has an entity_id; writers: JOB, PIPELINE",
            ),
            (
                "v9-partly",
                [Some("PIPELINE"), Some("4"), Some("1")],
                "partly: some PIPELINE writes in the last 30 days have no entity_id; writers: JOB, PIPELINE",
            ),
        ] {
            let transport = lineage(&NEEDED, &[&[Some("JOB"), Some("9"), Some("9")], &pipeline]);
            let finding = check(name, &transport);
            assert_eq!(finding.state(), State::Resolved, "{name}: {finding:?}");
            assert_eq!(verdict(&finding), answer, "{name}");
            assert_eq!(transport.asked().len(), 2);
        }
    }

    #[test]
    fn reports_what_it_cannot_observe_as_unknown() {
        for (name, transport, reason) in [
            (
                "v9-no-table",
                FakeTransport::default().statement_error(DESCRIBE_LINEAGE, missing_table()),
                "describing `system.access.table_lineage` ended FAILED (not found)",
            ),
            (
                "v9-no-column",
                lineage(
                    &["entity_type", "target_table_full_name", "event_date"],
                    &[],
                ),
                "`system.access.table_lineage` has no `entity_id` column",
            ),
            (
                "v9-denied",
                FakeTransport::default()
                    .statement(
                        DESCRIBE_LINEAGE,
                        &DESCRIBE,
                        &NEEDED
                            .map(|column| [Some(column), None, None])
                            .iter()
                            .map(|row| &row[..])
                            .collect::<Vec<_>>(),
                    )
                    .statement_error("count(entity_id)", denied()),
                "counting writes in `system.access.table_lineage` ended FAILED (permission denied)",
            ),
            (
                "v9-no-pipeline",
                lineage(&NEEDED, &[&[Some("JOB"), Some("9"), Some("9")]]),
                "no write in the last 30 days of `system.access.table_lineage` has entity_type `PIPELINE`",
            ),
            (
                "v9-bad-counts",
                lineage(&NEEDED, &[&[Some("PIPELINE"), Some("many"), Some("1")]]),
                "is not a pair of counts",
            ),
            (
                "v9-zero",
                lineage(&NEEDED, &[&[Some("PIPELINE"), Some("0"), Some("0")]]),
                "has entity_type `PIPELINE`",
            ),
            (
                "v9-more-ids",
                lineage(&NEEDED, &[&[Some("PIPELINE"), Some("1"), Some("2")]]),
                "more writes with an `entity_id` than writes",
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
    fn reports_missing_inputs_and_a_cli_that_cannot_run_as_failed() {
        let temp = TempDir::new("v9-no-warehouse");
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
        let finding = check("v9-unrunnable", &FakeTransport::unrunnable());
        assert_eq!(finding.state(), State::Failed);
    }
}

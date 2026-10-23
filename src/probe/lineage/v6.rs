//! The `v6` check, part A (#34): are table-update triggers job fields,
//! according to the bundle schema the CLI ships?
//!
//! Answered offline, with no bundle or workspace. Part B (#37) checks a live
//! job.

use std::collections::BTreeSet;
use std::ffi::OsString;

use crate::bundle::schema::{SCHEMA_COMMAND, SCHEMA_TIMEOUT, definitions_with_property};
use crate::databricks::run_captured;
use crate::json;
use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;

const CHECK: CheckId = CheckId::V6;
/// The expectation the answer confirms or corrects.
const SITE: &str = "docs/mv-lineage/phase0/01-requirements.md#r5-cli-baseline";
const PROPERTY: &str = "table_update";
/// SDK service packages are named without this prefix, so the SDK's jobs
/// package is `jobs`. Any other package keeps its full import path.
const SDK_SERVICE: &str = "github.com/databricks/databricks-sdk-go/service/";
/// The package R5 expects to declare the trigger: the SDK's jobs package.
const JOBS: &str = "jobs";
/// Starts a verdict in which no `jobs` definition declares the trigger.
const OUTSIDE_JOBS: &str = "not jobs: ";
const FALLBACK: &str = "Assume table-update triggers are job fields, as R5 expects, until v6 part B (#37) checks a live job.";

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    let args = SCHEMA_COMMAND.map(OsString::from);
    let result = run_captured(context.binary, &args, SCHEMA_TIMEOUT);
    let evidence = vec![context.run.record_invocation(
        CHECK.as_str(),
        "schema",
        context.binary,
        &args,
        &result,
    )?];
    let unknown = |reason: &str| Finding::unknown(CHECK, reason, FALLBACK, evidence.clone(), SITE);

    // The error text stays in the evidence, with the binary path removed.
    let Ok(captured) = result else {
        return Ok(Finding::failed(
            CHECK,
            "`databricks bundle schema` could not be run to completion; see the evidence record",
            evidence,
            SITE,
        ));
    };
    if !captured.status.success() {
        let status = captured
            .status
            .code()
            .map_or_else(|| "a signal".to_owned(), |code| format!("exit code {code}"));
        return Ok(unknown(&format!(
            "`databricks bundle schema` failed with {status}"
        )));
    }
    let Ok(schema) = json::parse_bytes(&captured.stdout) else {
        return Ok(unknown(
            "`databricks bundle schema` did not print valid JSON",
        ));
    };
    let Some(definitions) = definitions_with_property(schema.root(), PROPERTY) else {
        return Ok(unknown("the bundle schema has no `$defs` object"));
    };
    if definitions.is_empty() {
        return Ok(unknown(
            "no definition in the bundle schema has a `table_update` property",
        ));
    }
    let packages: BTreeSet<_> = definitions
        .iter()
        .map(|definition| {
            let package = definition.package.as_str();
            package.strip_prefix(SDK_SERVICE).unwrap_or(package)
        })
        .collect();
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(verdict(&packages)),
        evidence,
        SITE,
    ))
}

/// `jobs` alone answers yes, and other packages name where else the trigger
/// lives. Without `jobs` the answer is no, which contradicts R5, so the
/// verdict says so before naming the packages.
fn verdict(packages: &BTreeSet<&str>) -> String {
    let names = packages.iter().copied().collect::<Vec<_>>().join(", ");
    if packages.contains(JOBS) {
        names
    } else {
        format!("{OUTSIDE_JOBS}{names}")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::run;
    use crate::evidence::RunDir;
    use crate::probe::model::{Finding, State, Verdict};
    use crate::probe::orchestrator::Context;
    use crate::test_support::{ScenarioCli, TempDir, assert_no_mutations, fake_databricks};

    /// The real nesting of `jobs.TriggerSettings`, trimmed from CLI 1.21.0.
    const JOBS_SCHEMA: &str = include_str!("../../../tests/fixtures/bundle-schema-jobs.json");

    /// Runs the check against a scenario answering `bundle schema`.
    fn check(name: &str, response: &str) -> (Finding, Vec<Vec<String>>, TempDir) {
        let temp = TempDir::new(name);
        let cli_dir = temp.path().join("cli");
        let bundle = temp.path().join("bundle");
        fs::create_dir_all(&cli_dir).expect("create CLI dir");
        fs::create_dir_all(&bundle).expect("create bundle");
        let scenario = format!("case\narg bundle\narg schema\n{response}\n");
        let cli = ScenarioCli::new(fake_databricks(), &cli_dir, &scenario);
        let run_dir = RunDir::create(&bundle, "lineage", 0).expect("create run");
        let finding = run(&Context {
            binary: cli.binary().as_os_str(),
            run: &run_dir,
        })
        .expect("run check");
        (finding, cli.invocations(), temp)
    }

    /// A schema whose `definitions` sit in the SDK's service packages.
    fn sdk_schema(definitions: &str) -> String {
        format!(
            r#"{{"$defs": {{"github.com": {{"databricks": {{"databricks-sdk-go": {{"service": {{{definitions}}}}}}}}}}}}}"#
        )
    }

    fn schema_response(schema: &str) -> String {
        format!(
            "stdout {}",
            schema.replace('\\', "\\\\").replace('\n', "\\n")
        )
    }

    #[test]
    fn resolves_jobs_from_the_real_schema_shape() {
        let (finding, invocations, _temp) = check("v6-jobs", &schema_response(JOBS_SCHEMA));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(finding.verdict().map(Verdict::as_str), Some("jobs"));
        assert_eq!(invocations, [["bundle", "schema"]]);
        assert_no_mutations(&invocations);
    }

    #[test]
    fn names_every_package_that_defines_the_trigger() {
        let schema = sdk_schema(
            r#""pipelines.PipelineTrigger": {"type": "object", "properties": {"table_update": {}}},
            "jobs.TriggerSettings": {"oneOf": [{"properties": {"table_update": {}}}]}"#,
        );
        let (finding, _, _temp) = check("v6-elsewhere", &schema_response(&schema));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some("jobs, pipelines")
        );
    }

    #[test]
    fn says_when_no_jobs_definition_has_the_trigger() {
        let schema = sdk_schema(
            r#""pipelines.PipelineTrigger": {"type": "object", "properties": {"table_update": {}}},
            "jobs.TriggerSettings": {"type": "object", "properties": {"periodic": {}}}"#,
        );
        let (finding, _, _temp) = check("v6-not-jobs", &schema_response(&schema));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some("not jobs: pipelines")
        );
    }

    #[test]
    fn a_jobs_package_outside_the_sdk_is_not_jobs() {
        let schema = r#"{"$defs": {"github.com": {"databricks": {"cli": {"libs": {
            "jobs.Settings": {"type": "object", "properties": {"table_update": {}}}
        }}}}}}"#;
        let (finding, _, _temp) = check("v6-other-jobs", &schema_response(schema));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some("not jobs: github.com/databricks/cli/libs/jobs")
        );
    }

    #[test]
    fn reports_unusable_schemas_as_unknown() {
        for (name, response, reason) in [
            (
                "v6-absent",
                schema_response(r#"{"$defs": {"service": {"jobs.T": {"properties": {}}}}}"#),
                "no definition",
            ),
            ("v6-no-defs", schema_response("{}"), "no `$defs` object"),
            (
                "v6-malformed",
                schema_response("{\"$defs\": "),
                "valid JSON",
            ),
            ("v6-non-utf8", "stdout \\xff".to_owned(), "valid JSON"),
            ("v6-exit", "exit 3".to_owned(), "failed with exit code 3"),
        ] {
            let (finding, _, _temp) = check(name, &response);
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn reports_signal_termination_as_unknown() {
        let (finding, _, _temp) = check("v6-signal", "abort");
        assert_eq!(finding.state(), State::Unknown);
        assert!(
            finding.reason().unwrap_or_default().contains("a signal"),
            "{finding:?}"
        );
    }

    #[test]
    fn reports_a_missing_cli_as_failed() {
        let temp = TempDir::new("v6-missing");
        let run_dir = RunDir::create(temp.path(), "lineage", 0).expect("create run");
        let missing = temp.path().join("no-such-databricks");
        let finding = run(&Context {
            binary: missing.as_os_str(),
            run: &run_dir,
        })
        .expect("run check");
        assert_eq!(finding.state(), State::Failed);
        let reason = finding.reason().unwrap_or_default();
        assert!(
            !reason.contains("no-such-databricks"),
            "reasons must not copy raw error text: {reason}"
        );
        let record =
            fs::read_to_string(run_dir.path().join("evidence/v6/schema.json")).expect("read");
        assert!(
            record.contains("\"error\": \"could not execute databricks: "),
            "{record}"
        );
        assert!(!record.contains("no-such-databricks"), "{record}");
    }
}

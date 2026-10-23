//! The `v6` check, part A (#34): are table-update triggers job fields,
//! according to the bundle schema the CLI ships?
//!
//! Answered offline, with no bundle or workspace. Part B (#37) checks a live
//! job.

use std::borrow::Cow;
use std::collections::BTreeSet;

use crate::bundle::schema::{SCHEMA_COMMAND, SCHEMA_TIMEOUT, packages_with_property};
use crate::json;
use crate::probe::lineage::{Step, run_step};
use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;

const CHECK: CheckId = CheckId::V6;
/// The expectation the answer confirms or corrects.
const SITE: &str = "docs/mv-lineage/phase0/01-requirements.md#r5-cli-baseline";
const PROPERTY: &str = "table_update";
/// The SDK module. Its service packages are named by what follows `service/`,
/// after the major version and a `/` when there is one, such as `v2/jobs`, so
/// the SDK's jobs package is `jobs` or `vN/jobs`. Any other package keeps its
/// full import path.
const SDK_MODULE: &str = "github.com/databricks/databricks-sdk-go/";
/// The package R5 expects to declare the trigger: the SDK's jobs package.
const JOBS: &str = "jobs";
/// Starts a verdict in which no `jobs` definition declares the trigger.
const OUTSIDE_JOBS: &str = "not jobs: ";
/// Follows `jobs` in a verdict when other packages declare the trigger too.
const ALSO: &str = "; also: ";
const FALLBACK: &str = "Assume table-update triggers are job fields, as R5 expects, until v6 part B (#37) checks a live job.";

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    let (evidence, step) = run_step(
        context,
        CHECK,
        "schema",
        &SCHEMA_COMMAND,
        SCHEMA_TIMEOUT,
        SITE,
    )?;
    let unknown = |reason: &str| Finding::unknown(CHECK, reason, FALLBACK, evidence.clone(), SITE);
    let stdout = match step {
        Step::Succeeded(stdout) => stdout,
        Step::Unsuccessful(reason) => return Ok(unknown(&reason)),
        Step::Failed(finding) => return Ok(finding),
    };
    let Ok(schema) = json::parse_bytes(&stdout) else {
        return Ok(unknown(
            "`databricks bundle schema` did not print valid JSON",
        ));
    };
    let Some(packages) = packages_with_property(schema.root(), PROPERTY) else {
        return Ok(unknown("the bundle schema has no `$defs` object"));
    };
    if packages.is_empty() {
        return Ok(unknown(
            "no definition in the bundle schema has a `table_update` property",
        ));
    }
    let packages: BTreeSet<_> = packages.iter().map(|package| name(package)).collect();
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(verdict(&packages)),
        evidence,
        SITE,
    ))
}

/// How the verdict names `package`: an SDK service package by its own name,
/// a package directly under `$defs` as `$defs/` and its name, so that a
/// top-level `jobs` is not taken for the SDK's, and any other package by its
/// import path. Control characters are escaped, since the name comes from the
/// CLI's output and a verdict is one line.
fn name(package: &str) -> Cow<'_, str> {
    let name = match sdk_service(package) {
        Some((None, name)) => Cow::Borrowed(name),
        Some((Some(major), name)) => Cow::Owned(format!("{major}/{name}")),
        None if !package.contains('/') => Cow::Owned(format!("$defs/{package}")),
        None => Cow::Borrowed(package),
    };
    if !name.chars().any(char::is_control) {
        return name;
    }
    let mut escaped = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_control() {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    Cow::Owned(escaped)
}

/// The module's major version, if any, and the name of `package` if it is an
/// SDK service package, such as `v2` and `jobs` for
/// `github.com/databricks/databricks-sdk-go/v2/service/jobs`.
fn sdk_service(package: &str) -> Option<(Option<&str>, &str)> {
    let rest = package.strip_prefix(SDK_MODULE)?;
    let (major, rest) = match rest.split_once('/') {
        Some((major, after)) if is_major_version(major) => (Some(major), after),
        _ => (None, rest),
    };
    let name = rest
        .strip_prefix("service/")
        .filter(|name| !name.is_empty() && !name.contains('/'))?;
    Some((major, name))
}

/// Whether the verdict name `name` is the SDK's jobs package, in any major
/// version of the module.
fn is_jobs(name: &str) -> bool {
    name == JOBS
        || name
            .split_once('/')
            .is_some_and(|(major, name)| name == JOBS && is_major_version(major))
}

/// Whether `segment` is a Go major version suffix: `v` and a number above 1,
/// in ASCII digits without a sign or a leading zero.
fn is_major_version(segment: &str) -> bool {
    segment
        .strip_prefix('v')
        .filter(|number| number.bytes().all(|byte| byte.is_ascii_digit()))
        .filter(|number| !number.starts_with('0'))
        .and_then(|number| number.parse::<u32>().ok())
        .is_some_and(|number| number > 1)
}

/// The SDK's jobs packages alone answer yes, so the SDK's `jobs` alone is the
/// bare `jobs`, and each major version is named, such as `jobs, v2/jobs`. With
/// other packages too, the jobs packages come first and `; also: ` names where
/// else the trigger lives. Without a jobs package the answer is no, which
/// contradicts R5, so the verdict says so before naming the packages.
fn verdict(packages: &BTreeSet<Cow<'_, str>>) -> String {
    let (jobs, others): (Vec<&str>, Vec<&str>) = packages
        .iter()
        .map(AsRef::as_ref)
        .partition(|name| is_jobs(name));
    let (jobs, others) = (jobs.join(", "), others.join(", "));
    match (jobs.is_empty(), others.is_empty()) {
        (true, _) => format!("{OUTSIDE_JOBS}{others}"),
        (false, true) => jobs,
        (false, false) => format!("{jobs}{ALSO}{others}"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::run;
    use crate::evidence::RunDir;
    use crate::probe::model::{Finding, State, Verdict};
    use crate::probe::orchestrator::Context;
    use crate::test_support::{
        FakeTransport, ScenarioCli, TempDir, assert_no_mutations, fake_databricks, stdout_line,
    };

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
            transport: &FakeTransport::default(),
            warehouse_id: None,
            scope_catalog: None,
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

    #[test]
    fn resolves_jobs_from_the_real_schema_shape() {
        let (finding, invocations, _temp) = check("v6-jobs", &stdout_line(JOBS_SCHEMA));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(finding.verdict().map(Verdict::as_str), Some("jobs"));
        assert_eq!(invocations, [["bundle", "schema"]]);
        assert_no_mutations(&invocations);
    }

    #[test]
    fn names_every_package_that_defines_the_trigger() {
        // `jobs` comes first whether other names sort before or after it, so
        // only a clean yes reads as the bare `jobs`.
        for (name, other, verdict) in [
            ("v6-also-before", "compute", "jobs; also: compute"),
            ("v6-also-after", "pipelines", "jobs; also: pipelines"),
        ] {
            let schema = sdk_schema(&format!(
                r#""{other}.Trigger": {{"type": "object", "properties": {{"table_update": {{}}}}}},
                "jobs.TriggerSettings": {{"oneOf": [{{"properties": {{"table_update": {{}}}}}}]}}"#
            ));
            let (finding, _, _temp) = check(name, &stdout_line(&schema));
            assert_eq!(finding.state(), State::Resolved, "{finding:?}");
            assert_eq!(finding.verdict().map(Verdict::as_str), Some(verdict));
        }
        let schema = sdk_schema(
            r#""compute.T": {"type": "object", "properties": {"table_update": {}}},
            "pipelines.T": {"type": "object", "properties": {"table_update": {}}},
            "jobs.T": {"type": "object", "properties": {"table_update": {}}}"#,
        );
        let (finding, _, _temp) = check("v6-also-many", &stdout_line(&schema));
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some("jobs; also: compute, pipelines")
        );
    }

    #[test]
    fn says_when_no_jobs_definition_has_the_trigger() {
        let schema = sdk_schema(
            r#""pipelines.PipelineTrigger": {"type": "object", "properties": {"table_update": {}}},
            "jobs.TriggerSettings": {"type": "object", "properties": {"periodic": {}}}"#,
        );
        let (finding, _, _temp) = check("v6-not-jobs", &stdout_line(&schema));
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
        let (finding, _, _temp) = check("v6-other-jobs", &stdout_line(schema));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some("not jobs: github.com/databricks/cli/libs/jobs")
        );
    }

    #[test]
    fn a_top_level_jobs_package_is_not_the_sdks() {
        let schema = r#"{"$defs": {
            "jobs.Settings": {"type": "object", "properties": {"table_update": {}}}
        }}"#;
        let (finding, _, _temp) = check("v6-top-level-jobs", &stdout_line(schema));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some("not jobs: $defs/jobs")
        );
    }

    #[test]
    fn a_versioned_sdk_module_is_still_the_sdk() {
        let schema = r#"{"$defs": {"github.com": {"databricks": {"databricks-sdk-go": {
            "v2": {"service": {
                "jobs.TriggerSettings": {"type": "object", "properties": {"table_update": {}}}
            }},
            "v0": {"service": {
                "pipelines.T": {"type": "object", "properties": {"table_update": {}}}
            }}
        }}}}}"#;
        let (finding, _, _temp) = check("v6-versioned-sdk", &stdout_line(schema));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some("v2/jobs; also: github.com/databricks/databricks-sdk-go/v0/service/pipelines")
        );
    }

    #[test]
    fn escapes_control_characters_in_package_names() {
        // A key with a newline must not add a line to the run summary.
        let schema = r#"{"$defs": {
            "pkg\nv7        resolved  yes.T": {"type": "object", "properties": {"table_update": {}}}
        }}"#;
        let (finding, _, _temp) = check("v6-control", &stdout_line(schema));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some(r"not jobs: $defs/pkg\nv7        resolved  yes")
        );
    }

    #[test]
    fn recognizes_only_real_major_versions() {
        for segment in ["v2", "v10", "v123"] {
            assert!(super::is_major_version(segment), "{segment}");
        }
        for segment in [
            "v", "v0", "v1", "v02", "v+2", "v-2", "v 2", "v2a", "V2", "2",
        ] {
            assert!(!super::is_major_version(segment), "{segment}");
        }
    }

    #[test]
    fn keeps_sdk_major_versions_apart() {
        let schema = r#"{"$defs": {"github.com": {"databricks": {"databricks-sdk-go": {
            "service": {
                "jobs.TriggerSettings": {"type": "object", "properties": {"table_update": {}}}
            },
            "v2": {"service": {
                "jobs.TriggerSettings": {"type": "object", "properties": {"table_update": {}}}
            }}
        }}}}}"#;
        let (finding, _, _temp) = check("v6-both-sdks", &stdout_line(schema));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some("jobs, v2/jobs")
        );
    }

    #[test]
    fn reports_unusable_schemas_as_unknown() {
        for (name, response, reason) in [
            (
                "v6-absent",
                stdout_line(r#"{"$defs": {"service": {"jobs.T": {"properties": {}}}}}"#),
                "no definition",
            ),
            ("v6-no-defs", stdout_line("{}"), "no `$defs` object"),
            ("v6-malformed", stdout_line("{\"$defs\": "), "valid JSON"),
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
            transport: &FakeTransport::default(),
            warehouse_id: None,
            scope_catalog: None,
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

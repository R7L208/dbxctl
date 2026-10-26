//! The `validate` check (#32): what `bundle validate` reports about the
//! bundle for the target: its engine, workspace paths, declared resources,
//! and scripts.
//!
//! Validate runs once per probe run, behind a read-only check that it would
//! not create `workspace.file_path` (`src/probe/validation.rs`).

use crate::bundle::config::{Keys, engine, keys, resources, workspace_paths};
use crate::probe::lineage::{exit_text, one_line};
use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;
use crate::probe::validation::{Outcome, validation};

const CHECK: CheckId = CheckId::Validate;
/// The expectation the answer confirms or corrects.
const SITE: &str = "docs/mv-lineage/phase0/01-requirements.md#r5-cli-baseline";
const FALLBACK: &str = "Read the bundle's own configuration files instead, and assume the `direct` engine, workspace paths below `workspace.root_path`, and no scripts.";

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    let validation = validation(context, CHECK)?;
    let evidence = validation.evidence.clone();
    let unknown =
        |reason: String| Finding::unknown(CHECK, reason, FALLBACK, evidence.clone(), SITE);
    let (config, status) = match &validation.outcome {
        Outcome::Config { config, status } => (config, *status),
        Outcome::Unknown(reason) => return Ok(unknown(reason.clone())),
        Outcome::Failed(reason) => {
            return Ok(Finding::failed(CHECK, reason.clone(), evidence, SITE));
        }
    };
    let root = config.root();
    let Some(paths) = workspace_paths(root) else {
        return Ok(unknown(
            "the validated configuration has no `workspace` object".to_owned(),
        ));
    };
    let resources = match resources(root) {
        Ok(resources) => resources,
        Err(reason) => return Ok(unknown(reason)),
    };
    let mut parts = vec![
        format!(
            "engine {}",
            engine(root).map_or_else(|| "unset".to_owned(), one_line)
        ),
        format!("workspace paths: {}", list(&paths)),
        format!(
            "resources: {}",
            if resources.is_empty() {
                "none".to_owned()
            } else {
                resources
                    .iter()
                    .map(|(kind, entries)| format!("{} {}", one_line(kind), entries.len()))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
    ];
    for (label, pointer) in [
        ("experimental scripts", "/experimental/scripts"),
        ("scripts", "/scripts"),
    ] {
        match keys(root, pointer) {
            Keys::Absent => parts.push(format!("{label}: none")),
            Keys::Present(names) => parts.push(format!("{label}: {}", list(&names))),
            Keys::NotAnObject => {
                return Ok(unknown(format!(
                    "`{}` in the validated configuration is not an object",
                    &pointer[1..].replace('/', ".")
                )));
            }
        }
    }
    if !status.success() {
        parts.push(format!("validate exited with {}", exit_text(status)));
    }
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(parts.join("; ")),
        evidence,
        SITE,
    ))
}

/// The names, escaped and comma-separated, or `none`.
fn list(names: &[&str]) -> String {
    if names.is_empty() {
        return "none".to_owned();
    }
    names
        .iter()
        .map(|name| one_line(name))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::probe::lineage::testing::{
        Fixture, VALIDATE, bundle_case, gate_cases, get_status_path,
    };
    use crate::probe::model::{Finding, State, Verdict};
    use crate::test_support::{assert_no_mutations, stdout_line};

    const VALIDATED: &str = include_str!("../../../tests/fixtures/cli-1.13.0/validate.json");
    const SUMMARY: [&str; 4] = ["bundle", "summary", "-o", "json"];

    /// Runs the check with `file_path` answered with HTTP `status` and
    /// validate with `response`.
    fn check(name: &str, status: u16, response: &str) -> (Finding, Fixture) {
        let scenario = gate_cases(status) + &bundle_case(&VALIDATE, response);
        let fixture = Fixture::new(name, &scenario);
        (fixture.run(run), fixture)
    }

    /// The argv `args` with the target appended, as the bundle runner calls
    /// the CLI.
    fn argv(args: &[&str]) -> Vec<String> {
        args.iter()
            .chain(&["-t", "dev"])
            .map(|argument| (*argument).to_owned())
            .collect()
    }

    #[test]
    fn resolves_the_validated_fixture() {
        let (finding, fixture) = check("validate-resolved", 200, &stdout_line(VALIDATED));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some(
                "engine direct; workspace paths: artifact_path, file_path, resource_path, root_path, state_path; resources: jobs 2, pipelines 1; experimental scripts: none; scripts: none"
            )
        );
        let invocations = fixture.invocations();
        let status = get_status_path();
        assert_eq!(
            invocations,
            [
                argv(&SUMMARY),
                argv(&["api", "get", &status, "--debug"]),
                argv(&VALIDATE),
            ]
        );
        assert_no_mutations(&invocations);
        assert!(
            fixture
                .evidence("validate", "get-status.json")
                .contains("/api/2.0/workspace/get-status")
        );
    }

    #[test]
    fn does_not_validate_when_the_files_folder_is_missing() {
        let (finding, fixture) = check("validate-missing-folder", 404, &stdout_line(VALIDATED));
        assert_eq!(finding.state(), State::Unknown, "{finding:?}");
        let reason = finding.reason().unwrap_or_default();
        assert!(reason.contains("does not exist"), "{reason}");
        assert!(reason.contains("(R9)"), "{reason}");
        let invocations = fixture.invocations();
        assert_eq!(
            invocations.len(),
            2,
            "summary and get-status only: {invocations:?}"
        );
        assert!(
            invocations
                .iter()
                .all(|argv| argv[..2] != ["bundle", "validate"]),
            "{invocations:?}"
        );
        assert_no_mutations(&invocations);
    }

    #[test]
    fn validates_a_missing_folder_with_mutations_allowed() {
        let scenario = gate_cases(404) + &bundle_case(&VALIDATE, &stdout_line(VALIDATED));
        let fixture = Fixture::new("validate-allowed", &scenario);
        let finding = fixture.run_allowing_mutations(run);
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(fixture.invocations().len(), 3);
    }

    #[test]
    fn does_not_validate_when_the_folder_cannot_be_checked() {
        for (name, scenario, reason) in [
            (
                "validate-status-403",
                gate_cases(403),
                "get-status answered HTTP 403",
            ),
            (
                "validate-status-broken",
                gate_cases(200)
                    .replace("exit 0", "")
                    .replace("stdout {\"object_type\": \"DIRECTORY\"}", "stdout not json"),
                "no usable response",
            ),
            (
                "validate-summary-exit",
                bundle_case(&SUMMARY, "exit 1"),
                "`databricks bundle summary -o json` failed with exit code 1",
            ),
            (
                "validate-summary-path",
                bundle_case(&SUMMARY, r#"stdout {"workspace": {"file_path": "files"}}"#),
                "no absolute `workspace.file_path`",
            ),
        ] {
            let scenario = scenario + &bundle_case(&VALIDATE, &stdout_line(VALIDATED));
            let fixture = Fixture::new(name, &scenario);
            let finding = fixture.run(run);
            assert_eq!(finding.state(), State::Unknown, "{name}: {finding:?}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
            assert!(actual.contains("(R9)"), "{name}: {actual}");
            assert!(
                fixture
                    .invocations()
                    .iter()
                    .all(|argv| argv[..2] != ["bundle", "validate"]),
                "{name}: validate must not run"
            );
        }
    }

    #[test]
    fn accepts_json_when_validate_exits_1() {
        let stdout = include_str!("../../../tests/fixtures/cli-1.13.0/validate-exit-1.stdout");
        let (finding, _fixture) = check(
            "validate-exit-1",
            200,
            &format!("{}\nexit 1", stdout_line(stdout)),
        );
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        let verdict = finding.verdict().map(Verdict::as_str).unwrap_or_default();
        assert!(
            verdict.ends_with("; validate exited with exit code 1"),
            "{verdict}"
        );
    }

    #[test]
    fn reports_scripts_and_unset_engine() {
        let config = r#"{"workspace": {"file_path": "/w", "root_path": "/r", "current_user": {}},
            "resources": {}, "experimental": {"scripts": {"predeploy": "x", "pre\ndeploy": "y"}},
            "scripts": {"hello": {"content": "echo"}}}"#;
        let (finding, _fixture) = check("validate-scripts", 200, &stdout_line(config));
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some(
                "engine unset; workspace paths: file_path, root_path; resources: none; experimental scripts: pre\\ndeploy, predeploy; scripts: hello"
            )
        );
    }

    #[test]
    fn reports_unexpected_output_as_unknown() {
        for (name, response, reason) in [
            (
                "validate-exit",
                "exit 1",
                "failed with exit code 1 and printed no JSON object",
            ),
            (
                "validate-text",
                "stdout Validation OK!\\n",
                "did not print a JSON object",
            ),
            ("validate-array", "stdout []", "did not print a JSON object"),
            (
                "validate-no-workspace",
                "stdout {\"resources\": {}}",
                "no `workspace` object",
            ),
            (
                "validate-no-resources",
                "stdout {\"workspace\": {}}",
                "no `resources` object",
            ),
            (
                "validate-bad-scripts",
                "stdout {\"workspace\": {}, \"resources\": {}, \"scripts\": 1}",
                "`scripts` in the validated configuration is not an object",
            ),
            (
                "validate-bad-experimental",
                "stdout {\"workspace\": {}, \"resources\": {}, \"experimental\": {\"scripts\": []}}",
                "`experimental.scripts` in the validated configuration is not an object",
            ),
        ] {
            let (finding, _fixture) = check(name, 200, response);
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn reports_signal_termination_as_unknown() {
        let (finding, _fixture) = check("validate-signal", 200, "abort");
        assert_eq!(finding.state(), State::Unknown);
        assert!(finding.reason().unwrap_or_default().contains("a signal"));
    }
}

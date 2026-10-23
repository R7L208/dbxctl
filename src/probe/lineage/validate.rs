//! The `validate` check (#32): what `bundle validate` reports about the
//! bundle for the target: its engine, workspace paths, declared resources,
//! and scripts.

use std::process::ExitStatus;

use crate::bundle::config::{
    BUNDLE_TIMEOUT, Keys, VALIDATE_COMMAND, engine, keys, resources, workspace_paths,
};
use crate::json::{self, Document};
use crate::probe::lineage::{Ran, exit_text, one_line, run_bundle_step};
use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;

const CHECK: CheckId = CheckId::Validate;
/// The expectation the answer confirms or corrects.
const SITE: &str = "docs/mv-lineage/phase0/01-requirements.md#r5-cli-baseline";
const FALLBACK: &str = "Read the bundle's own configuration files instead, and assume the `direct` engine, workspace paths below `workspace.root_path`, and no scripts.";

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    let (evidence, validated) = validated(context, CHECK, "validate", &VALIDATE_COMMAND, SITE)?;
    let unknown =
        |reason: String| Finding::unknown(CHECK, reason, FALLBACK, evidence.clone(), SITE);
    let (config, status) = match validated {
        Validated::Config { config, status } => (config, status),
        Validated::Unknown(reason) => return Ok(unknown(reason)),
        Validated::Failed(finding) => return Ok(finding),
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

/// What a `bundle validate` step produced.
pub(crate) enum Validated {
    /// The configuration it printed, and how it exited.
    Config {
        config: Document,
        status: ExitStatus,
    },
    /// A reason for an unknown finding.
    Unknown(String),
    Failed(Finding),
}

/// Runs `args`, a `bundle validate -o json` command, as `step` of `check`
/// and reads the configuration it prints. Any exit with a JSON object on
/// stdout counts: CLI 1.13.0 prints the configuration even when it exits 1.
pub(crate) fn validated(
    context: &Context<'_>,
    check: CheckId,
    step: &str,
    args: &[&str],
    site: &'static str,
) -> Result<(Vec<String>, Validated), String> {
    let (evidence, ran) = run_bundle_step(context, check, step, args, BUNDLE_TIMEOUT, site)?;
    let command = format!("`databricks {}`", args.join(" "));
    let validated = match ran {
        Ran::Failed(finding) => Validated::Failed(finding),
        Ran::Completed { status, stdout } => match json::parse_bytes(&stdout) {
            Ok(config) if config.root().members().is_some() => Validated::Config { config, status },
            _ if status.success() => {
                Validated::Unknown(format!("{command} did not print a JSON object"))
            }
            _ => Validated::Unknown(format!(
                "{command} failed with {} and printed no JSON object",
                exit_text(status)
            )),
        },
    };
    Ok((evidence, validated))
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::probe::lineage::testing::{Fixture, bundle_case};
    use crate::probe::model::{State, Verdict};
    use crate::test_support::{assert_no_mutations, stdout_line};

    const VALIDATE: &str = include_str!("../../../tests/fixtures/cli-1.13.0/validate.json");
    const ARGS: [&str; 4] = ["bundle", "validate", "-o", "json"];

    fn check(name: &str, response: &str) -> (crate::probe::model::Finding, Fixture) {
        let fixture = Fixture::new(name, &bundle_case(&ARGS, response));
        (fixture.run(run), fixture)
    }

    #[test]
    fn resolves_the_validated_fixture() {
        let (finding, fixture) = check("validate-resolved", &stdout_line(VALIDATE));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some(
                "engine direct; workspace paths: artifact_path, file_path, resource_path, root_path, state_path; resources: jobs 2, pipelines 1; experimental scripts: none; scripts: none"
            )
        );
        let invocations = fixture.invocations();
        assert_eq!(
            invocations,
            [["bundle", "validate", "-o", "json", "-t", "dev"]]
        );
        assert_no_mutations(&invocations);
    }

    #[test]
    fn accepts_json_when_validate_exits_1() {
        let stdout = include_str!("../../../tests/fixtures/cli-1.13.0/validate-exit-1.stdout");
        let (finding, _fixture) = check(
            "validate-exit-1",
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
        let (finding, _fixture) = check("validate-scripts", &stdout_line(config));
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
            let (finding, _fixture) = check(name, response);
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn reports_signal_termination_as_unknown() {
        let (finding, _fixture) = check("validate-signal", "abort");
        assert_eq!(finding.state(), State::Unknown);
        assert!(finding.reason().unwrap_or_default().contains("a signal"));
    }
}

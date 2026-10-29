//! The `v1` check (#32): are unknown experimental script keys rejected,
//! according to the bundle schema the CLI ships?
//!
//! Answered offline, like `v6` part A. The schema describes
//! `experimental.scripts` as a map; if any key is allowed, a misspelled hook
//! such as `pre_deploy` is accepted and never runs. On CLI 1.13.0,
//! `bundle validate` also accepts such a key without a warning and keeps it
//! in its output.

use crate::bundle::schema::{SCHEMA_COMMAND, SCHEMA_TIMEOUT, object_schema};
use crate::json::{self, Node};
use crate::probe::lineage::{Step, one_line, run_step};
use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;

const CHECK: CheckId = CheckId::V1;
/// The expectation the answer confirms or corrects.
const SITE: &str = "docs/mv-lineage/phase0/01-requirements.md#r5-cli-baseline";
const FALLBACK: &str = "Assume unknown experimental script keys are not rejected, as R5 expects, so a misspelled hook is silently ignored.";

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
    let root = schema.root();
    let Some(scripts) = root
        .pointer("/properties/experimental")
        .and_then(|experimental| object_schema(root, experimental))
        .and_then(|experimental| experimental.pointer("/properties/scripts"))
        .and_then(|scripts| object_schema(root, scripts))
    else {
        return Ok(unknown(
            "the bundle schema does not describe `experimental.scripts` as an object",
        ));
    };
    if scripts.get("propertyNames").is_some() || scripts.get("patternProperties").is_some() {
        return Ok(unknown(
            "the bundle schema restricts `experimental.scripts` keys by pattern, which v1 does not read",
        ));
    }
    let verdict = match scripts.get("additionalProperties") {
        Some(rule) if rule.as_bool() == Some(false) => {
            let keys: Vec<_> = scripts
                .get("properties")
                .and_then(Node::members)
                .into_iter()
                .flatten()
                .map(|(key, _)| one_line(key))
                .collect();
            if keys.is_empty() {
                "rejected: experimental.scripts accepts no keys".to_owned()
            } else {
                format!(
                    "rejected: experimental.scripts accepts only {}",
                    keys.join(", ")
                )
            }
        }
        // Absent, `true`, or a schema for each value: any key is accepted.
        _ => "not rejected: experimental.scripts accepts any key".to_owned(),
    };
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(verdict),
        evidence,
        SITE,
    ))
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::probe::lineage::testing::{Fixture, case};
    use crate::probe::model::{Finding, State, Verdict};
    use crate::test_support::{assert_no_mutations, stdout_line};

    /// `experimental.scripts`, trimmed from the CLI 1.13.0 schema.
    const SCRIPTS_SCHEMA: &str = include_str!("../../../tests/fixtures/bundle-schema-scripts.json");

    fn check(name: &str, response: &str) -> (Finding, Fixture) {
        let fixture = Fixture::new(name, &case(&["bundle", "schema"], response));
        (fixture.run(run), fixture)
    }

    /// A schema whose `experimental.scripts` is `scripts`.
    fn schema(scripts: &str) -> String {
        format!(
            r#"{{"properties": {{"experimental": {{"type": "object", "properties": {{"scripts": {scripts}}}}}}}}}"#
        )
    }

    #[test]
    fn resolves_the_cli_schema() {
        let (finding, fixture) = check("v1-resolved", &stdout_line(SCRIPTS_SCHEMA));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some("not rejected: experimental.scripts accepts any key")
        );
        let invocations = fixture.invocations();
        assert_eq!(invocations, [["bundle", "schema"]]);
        assert_no_mutations(&invocations);
    }

    #[test]
    fn reports_closed_maps_as_rejected() {
        for (scripts, verdict) in [
            (
                r#"{"type": "object", "additionalProperties": false, "properties": {"predeploy": {}, "postdeploy": {}}}"#,
                "rejected: experimental.scripts accepts only postdeploy, predeploy",
            ),
            (
                r#"{"type": "object", "additionalProperties": false}"#,
                "rejected: experimental.scripts accepts no keys",
            ),
            (
                r#"{"type": "object"}"#,
                "not rejected: experimental.scripts accepts any key",
            ),
        ] {
            let (finding, _fixture) = check("v1-closed", &stdout_line(&schema(scripts)));
            assert_eq!(
                finding.verdict().map(Verdict::as_str),
                Some(verdict),
                "{scripts}"
            );
        }
    }

    #[test]
    fn reports_unexpected_output_as_unknown() {
        for (name, response, reason) in [
            ("v1-exit", "exit 2".to_owned(), "failed with exit code 2"),
            (
                "v1-text",
                "stdout nope".to_owned(),
                "did not print valid JSON",
            ),
            ("v1-missing", "stdout {}".to_owned(), "does not describe"),
            (
                "v1-string",
                stdout_line(&schema(r#"{"type": "string"}"#)),
                "does not describe",
            ),
            (
                "v1-pattern",
                stdout_line(&schema(
                    r#"{"type": "object", "propertyNames": {"pattern": "^pre"}}"#,
                )),
                "by pattern",
            ),
        ] {
            let (finding, _fixture) = check(name, &response);
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
        }
    }
}

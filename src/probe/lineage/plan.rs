//! The `plan` check (#32): the shape of `bundle plan`, its action
//! vocabulary, and where each action carries the configuration it would
//! deploy.
//!
//! A `skip` carries none, so when an entry has no configuration the check
//! looks for the resource in the probe run's `bundle validate`
//! (`src/probe/validation.rs`).

use std::collections::BTreeMap;

use crate::bundle::config::{BUNDLE_TIMEOUT, resources};
use crate::bundle::plan::{CONFIG_POINTER, Entry, PLAN_COMMAND, read};
use crate::json;
use crate::probe::lineage::{Ran, exit_text, one_line, run_bundle_step};
use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;
use crate::probe::validation::{Outcome, validation};

const CHECK: CheckId = CheckId::Plan;
/// The expectation the answer confirms or corrects.
const SITE: &str = "docs/mv-lineage/phase0/01-requirements.md#r5-cli-baseline";
const FALLBACK: &str = "Read each resource's configuration from `bundle validate` and treat every declared resource as changed.";

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    let (mut evidence, ran) =
        run_bundle_step(context, CHECK, "plan", &PLAN_COMMAND, BUNDLE_TIMEOUT)?;
    let unknown = |reason: String, evidence: &[String]| {
        Finding::unknown(CHECK, reason, FALLBACK, evidence.to_vec(), SITE)
    };
    let stdout = match ran {
        Ran::Completed { status, stdout } if status.success() => stdout,
        Ran::Completed { status, .. } => {
            return Ok(unknown(
                format!(
                    "`databricks bundle plan -o json` failed with {}",
                    exit_text(status)
                ),
                &evidence,
            ));
        }
        Ran::Failed(reason) => return Ok(Finding::failed(CHECK, reason, evidence, SITE)),
    };
    let Ok(document) = json::parse_bytes(&stdout) else {
        return Ok(unknown(
            "`databricks bundle plan -o json` did not print valid JSON".to_owned(),
            &evidence,
        ));
    };
    let plan = match read(document.root()) {
        Ok(plan) => plan,
        Err(reason) => return Ok(unknown(reason, &evidence)),
    };
    if plan.entries.is_empty() {
        return Ok(unknown(
            "the plan has no resources, so its actions cannot be observed".to_owned(),
            &evidence,
        ));
    }

    // Resources whose entries carry no configuration, looked up in the
    // validated configuration: whether each is declared there.
    let unconfigured: Vec<_> = plan
        .entries
        .iter()
        .filter(|entry| entry.config.is_none())
        .collect();
    let mut declared = BTreeMap::new();
    if !unconfigured.is_empty() {
        let validation = validation(context, CHECK)?;
        evidence.extend(validation.evidence.iter().cloned());
        // Without the validated configuration, where those entries' configuration
        // lives is not observed, so the plan is not resolved.
        let without = |why: &str| {
            format!(
                "plan entries without `new_state` need the validated configuration, which is unavailable: {why}"
            )
        };
        let resources = match &validation.outcome {
            Outcome::Config { config, .. } => match resources(config.root()) {
                Ok(resources) => resources,
                Err(reason) => return Ok(unknown(without(&reason), &evidence)),
            },
            Outcome::Unknown(reason) => return Ok(unknown(without(reason), &evidence)),
            Outcome::Failed(reason) => {
                return Ok(Finding::failed(CHECK, reason.clone(), evidence, SITE));
            }
        };
        for entry in &unconfigured {
            let found = resources
                .get(entry.kind)
                .is_some_and(|entries| entries.contains_key(entry.key));
            declared.insert((entry.kind, entry.key), found);
        }
    }

    let actions = actions(&plan.entries, &declared);
    let version = plan
        .version
        .map_or_else(|| "unset".to_owned(), |version| version.to_string());
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(format!("plan_version {version}; {}", actions.join("; "))),
        evidence,
        SITE,
    ))
}

/// Each action, sorted, and where its entries carry their configuration:
/// `new_state.value`, `validate` for an entry whose resource validate
/// declares, or `not in validate` for one it does not. `declared` says, for
/// each entry without configuration, whether validate declares its
/// resource.
fn actions(entries: &[Entry<'_>], declared: &BTreeMap<(&str, &str), bool>) -> Vec<String> {
    let mut locations: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for entry in entries {
        let location = if entry.config.is_some() {
            &CONFIG_POINTER[1..]
        } else {
            match declared.get(&(entry.kind, entry.key)) {
                Some(true) => "validate",
                _ => "not in validate",
            }
        };
        let seen = locations.entry(entry.action).or_default();
        if !seen.contains(&location) {
            seen.push(location);
        }
    }
    locations
        .iter_mut()
        .map(|(action, places)| {
            places.sort_unstable();
            format!(
                "{}: {}",
                one_line(action),
                places.join(" or ").replace('/', ".")
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::probe::lineage::testing::{Fixture, VALIDATE, bundle_case, gate_cases};
    use crate::probe::model::{Finding, State, Verdict};
    use crate::test_support::{assert_no_mutations, stdout_line};

    const PLAN: [&str; 4] = ["bundle", "plan", "-o", "json"];
    const VALIDATED: &str = include_str!("../../../tests/fixtures/cli-1.13.0/validate.json");

    fn plan_fixture(name: &str) -> String {
        let path = format!(
            "{}/tests/fixtures/cli-1.13.0/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(path).expect("read fixture")
    }

    fn check(name: &str, plan: &str, validate: &str) -> (Finding, Fixture) {
        let scenario =
            bundle_case(&PLAN, plan) + &gate_cases(200) + &bundle_case(&VALIDATE, validate);
        let fixture = Fixture::new(name, &scenario);
        (fixture.run(run), fixture)
    }

    fn verdict(finding: &Finding) -> &str {
        finding.verdict().map(Verdict::as_str).unwrap_or_default()
    }

    #[test]
    fn create_plans_carry_their_configuration() {
        let (finding, fixture) = check(
            "plan-create",
            &stdout_line(&plan_fixture("plan-create.json")),
            "exit 1",
        );
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(verdict(&finding), "plan_version 2; create: new_state.value");
        let invocations = fixture.invocations();
        assert_eq!(invocations, [["bundle", "plan", "-o", "json", "-t", "dev"]]);
        assert_no_mutations(&invocations);
    }

    #[test]
    fn skip_actions_read_validate() {
        let (finding, fixture) = check(
            "plan-mixed",
            &stdout_line(&plan_fixture("plan-mixed.json")),
            &stdout_line(VALIDATED),
        );
        assert_eq!(
            verdict(&finding),
            "plan_version 2; skip: validate; update: new_state.value"
        );
        let invocations = fixture.invocations();
        assert_eq!(invocations.len(), 4, "plan, then the gated validate");
        assert_no_mutations(&invocations);

        // The deleted job is gone from the configuration.
        let validated = r#"{"workspace": {}, "resources": {
            "jobs": {"refresh_silver": {}}, "pipelines": {"dlt_pipeline": {}}}}"#;
        let (finding, _fixture) = check(
            "plan-delete",
            &stdout_line(&plan_fixture("plan-delete.json")),
            &stdout_line(validated),
        );
        assert_eq!(
            verdict(&finding),
            "plan_version 2; delete: not in validate; skip: validate"
        );
    }

    #[test]
    fn skip_without_validate_is_unknown() {
        for (name, validate, reason) in [
            ("plan-skip-no-validate", "exit 1", "printed no JSON object"),
            (
                "plan-skip-no-resources",
                "stdout {\"workspace\": {}}",
                "no `resources` object",
            ),
        ] {
            let (finding, _fixture) = check(
                name,
                &stdout_line(&plan_fixture("plan-skip.json")),
                validate,
            );
            assert_eq!(finding.state(), State::Unknown, "{name}: {finding:?}");
            let actual = finding.reason().unwrap_or_default();
            assert!(
                actual.contains("need the validated configuration"),
                "{name}: {actual}"
            );
            assert!(actual.contains(reason), "{name}: {actual}");
        }
    }

    #[test]
    fn reports_mixed_locations_and_unset_version() {
        let plan = r#"{"plan": {
            "resources.jobs.a": {"action": "create", "new_state": {"value": {}}},
            "resources.jobs.b": {"action": "create"}
        }}"#;
        let validated = r#"{"workspace": {}, "resources": {"jobs": {"b": {}}}}"#;
        let (finding, _fixture) = check("plan-odd", &stdout_line(plan), &stdout_line(validated));
        assert_eq!(
            verdict(&finding),
            "plan_version unset; create: new_state.value or validate"
        );
    }

    #[test]
    fn reports_unexpected_output_as_unknown() {
        for (name, response, reason) in [
            ("plan-exit", "exit 1", "failed with exit code 1"),
            ("plan-text", "stdout nope", "did not print valid JSON"),
            ("plan-shape", "stdout {}", "no `plan` object"),
            ("plan-empty", "stdout {\"plan\": {}}", "has no resources"),
            (
                "plan-name",
                "stdout {\"plan\": {\"jobs.a\": {\"action\": \"create\"}}}",
                "not named",
            ),
            (
                "plan-config-kind",
                "stdout {\"plan\": {\"resources.jobs.a\": {\"action\": \"create\", \"new_state\": {\"value\": \"x\"}}}}",
                "`new_state.value` is not an object",
            ),
        ] {
            let (finding, _fixture) = check(name, response, "exit 1");
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
        }
    }

    #[test]
    fn reports_a_missing_cli_as_failed() {
        crate::probe::lineage::testing::assert_fails_without_a_cli(
            run,
            "plan-missing-cli",
            "`databricks bundle plan -o json` could not be run to completion; see the evidence record",
        );
    }
}

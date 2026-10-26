//! The `state` check (#32): the layout of the deployment state in
//! `resources.json`, and where it keeps each pipeline's ID.
//!
//! Reads the copy the CLI keeps in the bundle root,
//! `.databricks/bundle/<target>/resources.json`, so it runs no command and
//! contacts no workspace. The copy exists once the bundle has been deployed
//! to the target from this checkout. The deployed configuration in it can
//! hold secrets, so the evidence keeps only its layout
//! (`bundle::state::layout`), never the file.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;

use crate::bundle::state::{layout, local_state, read, terraform_state};
use crate::json::{self, Document};
use crate::probe::lineage::one_line;
use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;

const CHECK: CheckId = CheckId::State;
/// The expectation the answer confirms or corrects.
const SITE: &str = "docs/mv-lineage/phase0/01-requirements.md#r5-cli-baseline";
const FALLBACK: &str = "Assume pipeline IDs at `/state/resources.pipelines.<key>/__id__` in the deployed `resources.json`, as R5 expects.";
/// Where every entry keeps its ID.
const ID_PATH: &str = "/state/resources.<type>.<key>/__id__";
/// The largest state file read, the same bound as captured CLI output.
const MAX_STATE_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    let unknown = |reason: &str, evidence: Vec<String>| {
        Finding::unknown(CHECK, reason, FALLBACK, evidence, SITE)
    };
    let Some(path) = local_state(context.bundle_root, context.target) else {
        return Ok(unknown(
            "the target is not a plain directory name, so its local state cannot be found",
            Vec::new(),
        ));
    };
    let mut bytes = Vec::new();
    let read_result =
        File::open(&path).and_then(|file| file.take(MAX_STATE_BYTES + 1).read_to_end(&mut bytes));
    if read_result.is_err() {
        let terraform =
            terraform_state(context.bundle_root, context.target).is_some_and(|path| path.is_file());
        return Ok(unknown(
            if terraform {
                "the bundle keeps Terraform state for the target, not `resources.json`"
            } else {
                "there is no local `resources.json` for the target; deploy the bundle to it from this checkout and re-run"
            },
            Vec::new(),
        ));
    }
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Ok(unknown(
            "the local `resources.json` is larger than 64 MiB",
            Vec::new(),
        ));
    }
    let Ok(document) = json::parse_bytes(&bytes) else {
        return Ok(unknown(
            "the local `resources.json` is not valid JSON",
            Vec::new(),
        ));
    };
    let layout = Document::from(layout(document.root()))
        .to_pretty_string()
        .map_err(|error| format!("could not serialize the state layout: {error}"))?;
    let evidence = vec![context.run.write_evidence(
        CHECK.as_str(),
        "resources-layout.json",
        layout.as_bytes(),
    )?];
    let state = match read(document.root()) {
        Ok(state) => state,
        Err(reason) => return Ok(unknown(&reason, evidence)),
    };
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for entry in &state.entries {
        *counts.entry(entry.kind).or_default() += 1;
    }
    if !counts.contains_key("pipelines") {
        return Ok(unknown(
            "the state has no pipelines, so the pipeline-ID path cannot be observed",
            evidence,
        ));
    }
    let without_id = state
        .entries
        .iter()
        .filter(|entry| entry.id.is_none())
        .count();
    if without_id > 0 {
        return Ok(unknown(
            &format!("{without_id} state entries have no string `__id__`"),
            evidence,
        ));
    }
    let configured = state.entries.iter().all(|entry| entry.has_config);
    let version = state
        .version
        .map_or_else(|| "unset".to_owned(), |version| version.to_string());
    let resources: Vec<_> = counts
        .iter()
        .map(|(kind, count)| format!("{} {count}", one_line(kind)))
        .collect();
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(format!(
            "state_version {version}; ids at {ID_PATH}; config in {}; resources: {}",
            if configured {
                "state"
            } else {
                "state for some entries"
            },
            resources.join(", ")
        )),
        evidence,
        SITE,
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::run;
    use crate::probe::lineage::testing::Fixture;
    use crate::probe::model::{Finding, State, Verdict};

    const STATE: &str = include_str!("../../../tests/fixtures/cli-1.13.0/resources.json");

    /// Runs the check with `state` as the local copy, if any.
    fn check(name: &str, state: Option<&str>) -> (Finding, Fixture) {
        let fixture = Fixture::new(name, "");
        if let Some(state) = state {
            let dir = fixture.bundle().join(".databricks/bundle/dev");
            fs::create_dir_all(&dir).expect("create state dir");
            fs::write(dir.join("resources.json"), state).expect("write state");
        }
        (fixture.run(run), fixture)
    }

    #[test]
    fn resolves_the_state_fixture() {
        let (finding, fixture) = check("state-resolved", Some(STATE));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some(
                "state_version 2; ids at /state/resources.<type>.<key>/__id__; config in state; resources: jobs 2, pipelines 1"
            )
        );
        assert!(
            fixture.invocations().is_empty(),
            "the check runs no command"
        );
        // Only the layout is kept: names, never the deployed values.
        let layout = fixture.evidence("state", "resources-layout.json");
        assert!(
            layout.contains("\"resources.pipelines.dlt_pipeline\""),
            "{layout}"
        );
        assert!(layout.contains("\"has_id\": true"), "{layout}");
        assert!(layout.contains("\"storage\""), "{layout}");
        assert!(!layout.contains("01a23b45c67d8901"), "no IDs: {layout}");
        assert!(!layout.contains("/Workspace/"), "no values: {layout}");
        assert!(
            !fixture
                .run_dir
                .path()
                .join("evidence/state/resources.json")
                .exists(),
            "the state file itself is never copied"
        );
    }

    #[test]
    fn reports_missing_and_terraform_state_as_unknown() {
        let (finding, _fixture) = check("state-missing", None);
        assert_eq!(finding.state(), State::Unknown);
        assert!(
            finding
                .reason()
                .unwrap_or_default()
                .contains("no local `resources.json`")
        );

        let fixture = Fixture::new("state-terraform", "");
        let dir = fixture.bundle().join(".databricks/bundle/dev/terraform");
        fs::create_dir_all(&dir).expect("create terraform dir");
        fs::write(dir.join("terraform.tfstate"), "{}").expect("write tfstate");
        let finding = fixture.run(run);
        assert!(
            finding
                .reason()
                .unwrap_or_default()
                .contains("Terraform state")
        );
    }

    #[test]
    fn reports_unexpected_state_as_unknown() {
        for (name, state, reason) in [
            ("state-text", "nope", "not valid JSON"),
            ("state-shape", "{}", "no `state` object"),
            (
                "state-no-pipelines",
                r#"{"state": {"resources.jobs.a": {"__id__": "1", "state": {}}}}"#,
                "has no pipelines",
            ),
            (
                "state-no-id",
                r#"{"state": {"resources.pipelines.a": {"state": {}}, "resources.jobs.b": {"__id__": 2}}}"#,
                "2 state entries have no string `__id__`",
            ),
        ] {
            let (finding, _fixture) = check(name, Some(state));
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
        }
        let (finding, _fixture) = check(
            "state-partial-config",
            Some(r#"{"state": {"resources.pipelines.a": {"__id__": "p"}}}"#),
        );
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some(
                "state_version unset; ids at /state/resources.<type>.<key>/__id__; config in state for some entries; resources: pipelines 1"
            )
        );
    }
}

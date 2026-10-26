//! The `v8` check (#32): where the bundle's resources are defined, and what
//! form their source paths take once validated.
//!
//! Reads the probe run's `bundle validate` (`src/probe/validation.rs`),
//! which passes `--include-locations` to add the configuration files and
//! where in them each resource is defined. Each
//! source path is classified by [`crate::bundle::paths::form`], and a path
//! below `workspace.file_path` is matched with its file in the bundle root.

use std::collections::BTreeMap;

use crate::bundle::config::{Locations, resources, source_paths};
use crate::bundle::paths::{Form, Local, form, local};
use crate::json::Node;
use crate::probe::lineage::{exit_text, one_line};
use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;
use crate::probe::validation::{Outcome, validation};

const CHECK: CheckId = CheckId::V8;
/// The expectation the answer confirms or corrects.
const SITE: &str = "docs/mv-lineage/phase0/03-bundle-probes.md#v8";
const FALLBACK: &str = "Treat every resource as defined in `databricks.yml`, and each source path as `workspace.file_path` plus its path in the bundle root, with a notebook's extension dropped.";

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
    let Some(file_path) = root.pointer("/workspace/file_path").and_then(Node::as_str) else {
        return Ok(unknown(
            "the validated configuration has no string `workspace.file_path`".to_owned(),
        ));
    };
    let Some(locations) = Locations::read(root) else {
        return Ok(unknown(
            "validate did not report where resources are defined (`__locations`)".to_owned(),
        ));
    };
    let resources = match resources(root) {
        Ok(resources) => resources,
        Err(reason) => return Ok(unknown(reason)),
    };
    let paths = source_paths(&resources);
    if paths.is_empty() {
        return Ok(unknown(
            "the bundle's resources name no source paths".to_owned(),
        ));
    }

    let mut forms: BTreeMap<Form, usize> = BTreeMap::new();
    let mut locals: BTreeMap<&str, usize> = BTreeMap::new();
    for source in &paths {
        let (found, relative) = if source.git {
            (Form::Git, None)
        } else {
            form(source.path, file_path)
        };
        *forms.entry(found).or_default() += 1;
        if let Some(relative) = relative {
            let local = match local(context.bundle_root, relative) {
                Local::File => "files",
                Local::Notebook(_) => "notebooks without extension",
                Local::Directory => "directories",
                Local::Missing => "missing",
            };
            *locals.entry(local).or_default() += 1;
        }
    }
    let mut origins: BTreeMap<String, usize> = BTreeMap::new();
    for (kind, entries) in &resources {
        for key in entries.keys() {
            let origin = locations
                .origin(&format!("{kind}.{key}"))
                .map_or_else(|| "unlocated".to_owned(), one_line);
            *origins.entry(origin).or_default() += 1;
        }
    }

    let forms: Vec<_> = forms
        .iter()
        .map(|(form, count)| {
            if *form == Form::FilePath && !locals.is_empty() {
                let locals: Vec<_> = locals
                    .iter()
                    .map(|(local, count)| format!("{count} {local}"))
                    .collect();
                format!("{} {count} ({})", form.as_str(), locals.join(", "))
            } else {
                format!("{} {count}", form.as_str())
            }
        })
        .collect();
    let origins: Vec<_> = origins
        .iter()
        .map(|(origin, count)| format!("{origin} {count}"))
        .collect();
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(format!(
            "paths: {}; origins: {}{}",
            forms.join(", "),
            origins.join(", "),
            if status.success() {
                String::new()
            } else {
                format!("; validate exited with {}", exit_text(status))
            }
        )),
        evidence,
        SITE,
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::run;
    use crate::probe::lineage::testing::{Fixture, VALIDATE, bundle_case, gate_cases};
    use crate::probe::model::{Finding, State, Verdict};
    use crate::test_support::{assert_no_mutations, stdout_line};

    const VALIDATED: &str =
        include_str!("../../../tests/fixtures/cli-1.13.0/validate-locations.json");
    const BUNDLE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/cli-1.13.0/bundle"
    );

    /// Copies the fixture bundle's files into `root`.
    fn copy_bundle(from: &Path, root: &Path) {
        for entry in fs::read_dir(from).expect("read bundle") {
            let entry = entry.expect("entry");
            let target = root.join(entry.file_name());
            if entry.file_type().expect("type").is_dir() {
                fs::create_dir_all(&target).expect("create dir");
                copy_bundle(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).expect("copy file");
            }
        }
    }

    fn check(name: &str, response: &str, files: bool) -> (Finding, Fixture) {
        let scenario = gate_cases(200) + &bundle_case(&VALIDATE, response);
        let fixture = Fixture::new(name, &scenario);
        if files {
            copy_bundle(Path::new(BUNDLE), &fixture.bundle());
        }
        (fixture.run(run), fixture)
    }

    #[test]
    fn resolves_the_fixture_bundle() {
        let (finding, fixture) = check("v8-resolved", &stdout_line(VALIDATED), true);
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some(
                "paths: file_path 6 (4 files, 2 notebooks without extension); origins: databricks.yml 2, resources/extra.yml 1"
            )
        );
        // The summary and get-status gate, then validate.
        let invocations = fixture.invocations();
        assert_eq!(invocations.len(), 3, "{invocations:?}");
        assert_eq!(
            invocations[2],
            [
                "bundle",
                "validate",
                "-o",
                "json",
                "--include-locations",
                "-t",
                "dev"
            ]
        );
        assert_no_mutations(&invocations);
    }

    #[test]
    fn qualifies_a_verdict_after_a_failed_validate() {
        let (finding, _fixture) = check(
            "v8-exit-1",
            &format!("{}\nexit 1", stdout_line(VALIDATED)),
            true,
        );
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        let verdict = finding.verdict().map(Verdict::as_str).unwrap_or_default();
        assert!(
            verdict.ends_with("; origins: databricks.yml 2, resources/extra.yml 1; validate exited with exit code 1"),
            "{verdict}"
        );
    }

    #[test]
    fn counts_missing_files_and_other_forms() {
        let (finding, _fixture) = check("v8-missing", &stdout_line(VALIDATED), false);
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some(
                "paths: file_path 6 (6 missing); origins: databricks.yml 2, resources/extra.yml 1"
            )
        );

        let config = r#"{"workspace": {"file_path": "/w/files"},
            "__locations": {"files": ["a.yml"], "locations": {"resources.jobs.j": [[0, 1, 1]]}},
            "resources": {"jobs": {
                "j": {"tasks": [{"notebook_task": {"notebook_path": "/Volumes/c/s/v/n.py"}}]},
                "k": {"git_source": {}, "tasks": [{"notebook_task": {"notebook_path": "nb"}},
                    {"spark_python_task": {"python_file": "dbfs:/p.py", "source": "WORKSPACE"}},
                    {"sql_task": {"file": {"path": "/Workspace/Shared/q.sql", "source": "WORKSPACE"}}}]}
            }}}"#;
        let (finding, _fixture) = check("v8-forms", &stdout_line(config), false);
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some("paths: workspace 1, volume 1, dbfs 1, git 1; origins: a.yml 1, unlocated 1")
        );
    }

    #[test]
    fn reports_unexpected_output_as_unknown() {
        for (name, response, reason) in [
            ("v8-exit", "exit 1", "printed no JSON object"),
            (
                "v8-no-file-path",
                "stdout {}",
                "no string `workspace.file_path`",
            ),
            (
                "v8-no-locations",
                "stdout {\"workspace\": {\"file_path\": \"/w\"}}",
                "`__locations`",
            ),
            (
                "v8-no-resources",
                "stdout {\"workspace\": {\"file_path\": \"/w\"}, \"__locations\": {\"files\": [], \"locations\": {}}}",
                "no `resources` object",
            ),
            (
                "v8-no-paths",
                "stdout {\"workspace\": {\"file_path\": \"/w\"}, \"__locations\": {\"files\": [], \"locations\": {}}, \"resources\": {\"jobs\": {\"j\": {}}}}",
                "name no source paths",
            ),
        ] {
            let (finding, _fixture) = check(name, response, false);
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
        }
    }

    #[test]
    fn reports_a_missing_cli_as_failed() {
        crate::probe::lineage::testing::assert_fails_without_a_cli(
            run,
            "v8-missing-cli",
            "`databricks bundle summary -o json` could not be run to completion; see the evidence record",
        );
    }
}

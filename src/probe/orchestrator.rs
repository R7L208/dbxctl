//! Runs the selected lineage checks and writes the run directory (#30).
//!
//! A run writes, in order: `run.json` with status `running`; each check's raw
//! evidence; `findings.json` and the skeleton `findings.md`; and finally
//! `run.json` again with status `completed` and the exit code. An interrupted
//! run therefore still leaves metadata for `probe report` and `probe cleanup`.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::evidence::{Clock, RunDir, timestamp};
use crate::exit::Exit;
use crate::json::{Document, Value};
use crate::probe::lineage::{self, LineageOptions};
use crate::probe::model::{CheckId, Finding, State};

const SUITE: &str = "lineage";
/// Version of the `run.json` and `findings.json` layouts.
const SCHEMA_VERSION: i64 = 1;

/// What one check needs to run.
pub(crate) struct Context<'a> {
    /// The resolved Databricks CLI executable.
    pub(crate) binary: &'a OsStr,
    pub(crate) run: &'a RunDir,
}

/// A validated `probe run --suite lineage` request.
#[derive(Debug)]
pub(crate) struct Plan {
    bundle_root: PathBuf,
    target: String,
    profile: Option<String>,
    /// Selected checks in canonical order.
    checks: Vec<CheckId>,
}

impl Plan {
    pub(crate) fn from_options(options: &LineageOptions) -> Result<Self, String> {
        if options.promote_to.is_some() {
            return Err("--promote-to is not implemented yet (#36)".to_owned());
        }
        let bundle_root = options
            .bundle_root
            .as_deref()
            .ok_or("option --bundle-root is required")?;
        let target =
            utf8("--target", options.target.as_deref())?.ok_or("option --target is required")?;
        let profile = utf8("--profile", options.profile.as_deref())?;
        Ok(Self {
            bundle_root: PathBuf::from(bundle_root),
            target,
            profile,
            checks: options.selected_checks(),
        })
    }
}

/// `run.json` records these as strings, so they must be valid UTF-8.
fn utf8(option: &str, value: Option<&OsStr>) -> Result<Option<String>, String> {
    value
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{option} must be valid UTF-8"))
        })
        .transpose()
}

/// A completed run.
#[derive(Debug)]
pub(crate) struct Outcome {
    pub(crate) exit: Exit,
    pub(crate) run_dir: PathBuf,
    pub(crate) findings: Vec<Finding>,
}

/// Runs `plan` against `binary`. An `Err` means the run directory could not
/// be written; check problems are reported as findings instead. Once the run
/// directory exists, the error names it, so a partial run can be found.
pub(crate) fn run(plan: &Plan, binary: &OsStr, clock: &dyn Clock) -> Result<Outcome, String> {
    let started = clock.now();
    let run_dir = RunDir::create(&plan.bundle_root, SUITE, started)?;
    complete(plan, binary, clock, &run_dir, started).map_err(|error| {
        format!(
            "{error} (partial run directory: {})",
            run_dir.path().display()
        )
    })
}

fn complete(
    plan: &Plan,
    binary: &OsStr,
    clock: &dyn Clock,
    run_dir: &RunDir,
    started: u64,
) -> Result<Outcome, String> {
    run_dir.write_json(
        "run.json",
        &run_document(plan, run_dir, started, None).into(),
    )?;

    let context = Context {
        binary,
        run: run_dir,
    };
    let findings = plan
        .checks
        .iter()
        .map(|&check| run_check(check, &context))
        .collect::<Result<Vec<_>, _>>()?;
    let exit = if findings
        .iter()
        .all(|finding| finding.state() == State::Resolved)
    {
        Exit::Success
    } else {
        Exit::Unresolved
    };

    run_dir.write_json("findings.json", &findings_document(run_dir, &findings))?;
    run_dir.write_file(
        "findings.md",
        findings_markdown(run_dir, &findings).as_bytes(),
    )?;
    run_dir.write_json(
        "run.json",
        &run_document(plan, run_dir, started, Some((clock.now(), exit))).into(),
    )?;
    Ok(Outcome {
        exit,
        run_dir: run_dir.path().to_path_buf(),
        findings,
    })
}

fn run_check(check: CheckId, context: &Context<'_>) -> Result<Finding, String> {
    match check {
        CheckId::Cli => lineage::cli::run(context),
        _ => Ok(Finding::skipped(
            check,
            format!("not implemented yet; {} adds this check", check.issue()),
        )),
    }
}

fn run_document(
    plan: &Plan,
    run_dir: &RunDir,
    started: u64,
    finished: Option<(u64, Exit)>,
) -> Value {
    Value::object([
        ("schema_version", Value::Integer(SCHEMA_VERSION)),
        ("suite", Value::string(SUITE)),
        ("run_id", Value::string(run_dir.run_id())),
        ("dbxctl_version", Value::string(env!("CARGO_PKG_VERSION"))),
        ("target", Value::string(plan.target.as_str())),
        ("profile", Value::optional_string(plan.profile.as_deref())),
        (
            "checks",
            Value::Array(
                plan.checks
                    .iter()
                    .map(|check| Value::string(check.as_str()))
                    .collect(),
            ),
        ),
        ("started_at", Value::string(timestamp(started))),
        (
            "finished_at",
            Value::optional_string(finished.map(|(at, _)| timestamp(at))),
        ),
        (
            "status",
            Value::string(if finished.is_some() {
                "completed"
            } else {
                "running"
            }),
        ),
        (
            "exit_code",
            finished.map_or(Value::Null, |(_, exit)| {
                Value::Integer(i64::from(exit.code()))
            }),
        ),
    ])
}

fn findings_document(run_dir: &RunDir, findings: &[Finding]) -> Document {
    Value::object([
        ("schema_version", Value::Integer(SCHEMA_VERSION)),
        ("suite", Value::string(SUITE)),
        ("run_id", Value::string(run_dir.run_id())),
        (
            "findings",
            Value::Array(findings.iter().map(Finding::to_value).collect()),
        ),
    ])
    .into()
}

/// A minimal human-readable summary. `probe report` (#36) replaces it with
/// the full redacted report.
fn findings_markdown(run_dir: &RunDir, findings: &[Finding]) -> String {
    let mut text = format!(
        "# Lineage probe findings\n\nRun `{}`. This is a skeleton report; `dbxctl probe report` generates the full redacted report.\n\n| Check | State | Verdict | Reason |\n| --- | --- | --- | --- |\n",
        run_dir.run_id()
    );
    for finding in findings {
        let verdict = finding
            .verdict()
            .map_or(String::new(), |verdict| format!("`{}`", verdict.as_str()));
        let _ = writeln!(
            text,
            "| `{}` | {} | {} | {} |",
            finding.check(),
            finding.state().as_str(),
            verdict,
            table_cell(finding.reason().unwrap_or_default()),
        );
    }
    text
}

fn table_cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;

    use super::{Context, Plan, run, run_check, table_cell};
    use crate::args::Parser;
    use crate::evidence::RunDir;
    use crate::evidence::tests::{FIXED_TIME, FixedClock};
    use crate::exit::Exit;
    use crate::probe::lineage::parse_lineage_options;
    use crate::probe::model::{CheckId, Finding, State};
    use crate::test_support::{
        ScenarioCli, TempDir, assert_no_mutations, fake_databricks, read_tree,
    };

    const VERSION_SCENARIO: &str = include_str!("../../tests/fixtures/scenarios/cli-version.txt");

    fn plan(bundle_root: &Path, extra: &[&str]) -> Result<Plan, String> {
        let mut args = vec![
            OsString::from("--bundle-root"),
            bundle_root.as_os_str().to_os_string(),
            OsString::from("--target"),
            OsString::from("dev"),
        ];
        args.extend(extra.iter().map(OsString::from));
        let mut parser = Parser::new(args.iter());
        let options = parse_lineage_options(&mut parser)?;
        parser.check_empty()?;
        Plan::from_options(&options)
    }

    /// A bundle root and a scenario CLI below one scratch directory.
    fn setup(name: &str, scenario: &str) -> (TempDir, ScenarioCli) {
        let temp = TempDir::new(name);
        fs::create_dir_all(temp.path().join("cli")).expect("create CLI dir");
        fs::create_dir_all(temp.path().join("bundle")).expect("create bundle");
        let cli = ScenarioCli::new(fake_databricks(), &temp.path().join("cli"), scenario);
        (temp, cli)
    }

    #[test]
    fn runs_the_cli_check_end_to_end() {
        let (temp, cli) = setup("orchestrator-cli", VERSION_SCENARIO);
        let bundle = temp.path().join("bundle");
        let plan = plan(&bundle, &["--only", "cli"]).expect("plan");
        let outcome =
            run(&plan, cli.binary().as_os_str(), &FixedClock(FIXED_TIME)).expect("run probe");

        assert_eq!(outcome.exit, Exit::Success);
        assert_eq!(outcome.findings.len(), 1);
        assert_eq!(outcome.findings[0].state(), State::Resolved);
        assert_eq!(
            outcome.run_dir,
            bundle.join(".dbxctl/probe/lineage/20260102T030405Z")
        );
        let invocations = cli.invocations();
        assert_eq!(invocations, [["--version"]]);
        assert_no_mutations(&invocations);

        let files: Vec<_> = read_tree(&outcome.run_dir)
            .into_iter()
            .map(|(path, bytes)| (path, String::from_utf8(bytes).expect("UTF-8 output")))
            .collect();
        let names: Vec<_> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            names,
            [
                "evidence/cli/version.json",
                "evidence/cli/version.stderr",
                "evidence/cli/version.stdout",
                "findings.json",
                "findings.md",
                "run.json",
            ]
        );
        let file = |name: &str| {
            files
                .iter()
                .find(|(path, _)| path == name)
                .map(|(_, text)| text.as_str())
                .expect("file present")
        };
        assert_eq!(
            file("run.json"),
            r#"{
  "checks": [
    "cli"
  ],
  "dbxctl_version": "0.1.0",
  "exit_code": 0,
  "finished_at": "2026-01-02T03:04:05Z",
  "profile": null,
  "run_id": "20260102T030405Z",
  "schema_version": 1,
  "started_at": "2026-01-02T03:04:05Z",
  "status": "completed",
  "suite": "lineage",
  "target": "dev"
}
"#
        );
        assert_eq!(
            file("findings.json"),
            r#"{
  "findings": [
    {
      "check": "cli",
      "evidence": [
        "evidence/cli/version.json"
      ],
      "fallback": null,
      "reason": null,
      "site": "src/databricks.rs MINIMUM_DATABRICKS_VERSION",
      "state": "resolved",
      "verdict": "1.13.0"
    }
  ],
  "run_id": "20260102T030405Z",
  "schema_version": 1,
  "suite": "lineage"
}
"#
        );
        assert_eq!(
            file("findings.md"),
            "# Lineage probe findings\n\nRun `20260102T030405Z`. This is a skeleton report; `dbxctl probe report` generates the full redacted report.\n\n| Check | State | Verdict | Reason |\n| --- | --- | --- | --- |\n| `cli` | resolved | `1.13.0` |  |\n"
        );
        assert_eq!(
            file("evidence/cli/version.stdout"),
            "Databricks CLI v1.13.0\n"
        );
        assert_eq!(
            fs::read(bundle.join(".dbxctl/.gitignore")).expect("read gitignore"),
            b"*\n"
        );
    }

    #[test]
    fn identical_inputs_produce_identical_files() {
        let (temp, cli) = setup("orchestrator-deterministic", VERSION_SCENARIO);
        let mut trees = Vec::new();
        for name in ["first", "second"] {
            let bundle = temp.path().join(name);
            fs::create_dir_all(&bundle).expect("create bundle");
            let plan = plan(&bundle, &["--only", "cli,v6,plan", "--profile", "p"]).expect("plan");
            let outcome =
                run(&plan, cli.binary().as_os_str(), &FixedClock(FIXED_TIME)).expect("run probe");
            trees.push(read_tree(&bundle.join(".dbxctl")));
            // The selection is reported in canonical order, not `--only` order.
            let checks: Vec<_> = outcome.findings.iter().map(Finding::check).collect();
            assert_eq!(checks, [CheckId::Cli, CheckId::Plan, CheckId::V6]);
        }
        assert_eq!(trees[0], trees[1]);
        assert!(!trees[0].is_empty());
    }

    #[test]
    fn unimplemented_checks_are_skipped_and_unresolved() {
        let (temp, cli) = setup("orchestrator-skipped", VERSION_SCENARIO);
        let bundle = temp.path().join("bundle");
        let plan = plan(&bundle, &["--only", "cli,v7"]).expect("plan");
        let outcome =
            run(&plan, cli.binary().as_os_str(), &FixedClock(FIXED_TIME)).expect("run probe");
        assert_eq!(outcome.exit, Exit::Unresolved);
        let v7 = &outcome.findings[1];
        assert_eq!(v7.state(), State::Skipped);
        assert_eq!(
            v7.reason(),
            Some("not implemented yet; #37 adds this check")
        );
        let run_json = fs::read_to_string(outcome.run_dir.join("run.json")).expect("read");
        assert!(run_json.contains("\"exit_code\": 10"), "{run_json}");
        let report = fs::read_to_string(outcome.run_dir.join("findings.md")).expect("read");
        assert!(
            report.contains("| `v7` | skipped |  | not implemented yet; #37 adds this check |"),
            "{report}"
        );
        // Only the `cli` check runs a command so far.
        assert_eq!(cli.invocations(), [["--version"]]);
    }

    #[test]
    fn unresolved_cli_check_sets_the_unresolved_exit() {
        let (temp, cli) = setup("orchestrator-unknown", "case\narg --version\nexit 2\n");
        let bundle = temp.path().join("bundle");
        let plan = plan(&bundle, &["--only", "cli"]).expect("plan");
        let outcome = run(&plan, cli.binary().as_os_str(), &FixedClock(0)).expect("run probe");
        assert_eq!(outcome.exit, Exit::Unresolved);
        assert_eq!(outcome.findings[0].state(), State::Unknown);
    }

    #[test]
    fn missing_bundle_root_is_an_error_without_side_effects() {
        let temp = TempDir::new("orchestrator-missing-root");
        let bundle = temp.path().join("missing");
        let plan = plan(&bundle, &["--only", "cli"]).expect("plan");
        let error = run(&plan, fake_databricks().as_os_str(), &FixedClock(0))
            .expect_err("missing bundle root");
        assert!(error.contains("is not a directory"), "{error}");
        assert!(!bundle.exists());
    }

    #[test]
    fn evidence_write_failures_are_errors() {
        let (temp, cli) = setup("orchestrator-blocked-evidence", VERSION_SCENARIO);
        let run_dir = RunDir::create(&temp.path().join("bundle"), "lineage", 0).expect("create");
        // A file where the evidence directory belongs.
        fs::write(run_dir.path().join("evidence"), "").expect("block evidence");
        let error = run_check(
            CheckId::Cli,
            &Context {
                binary: cli.binary().as_os_str(),
                run: &run_dir,
            },
        )
        .expect_err("blocked evidence");
        assert!(error.contains("could not create directory"), "{error}");
    }

    #[test]
    fn report_cells_cannot_break_the_table() {
        assert_eq!(table_cell("a|b\nc"), "a\\|b c");
    }

    #[test]
    fn rejects_unsupported_or_non_utf8_options() {
        let temp = TempDir::new("orchestrator-options");
        let error =
            plan(temp.path(), &["--only", "cli", "--promote-to", "/x"]).expect_err("promote-to");
        assert!(
            error.contains("--promote-to is not implemented yet"),
            "{error}"
        );

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            let options = crate::probe::lineage::LineageOptions {
                bundle_root: Some(temp.path().as_os_str().to_os_string()),
                target: Some(OsString::from_vec(vec![0xff])),
                ..Default::default()
            };
            let error = Plan::from_options(&options).expect_err("non-UTF-8 target");
            assert_eq!(error, "--target must be valid UTF-8");
        }
        let error = Plan::from_options(&crate::probe::lineage::LineageOptions::default())
            .expect_err("no bundle root");
        assert!(error.contains("--bundle-root"), "{error}");
    }
}

//! The minimal `cli` check (#30): which Databricks CLI version is installed.
//!
//! #32 extends this check to verify the required command surfaces.

use crate::databricks::{MINIMUM_DATABRICKS_VERSION, VERSION_COMMAND, VERSION_TIMEOUT, Version};
use crate::probe::lineage::{Step, run_step};
use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;

const CHECK: CheckId = CheckId::Cli;
/// The code that depends on the answer: the supported-version gate.
const SITE: &str = "src/databricks.rs MINIMUM_DATABRICKS_VERSION";

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    // The same command and timeout `dbxctl doctor` uses for its gate.
    let (evidence, step) = run_step(
        context,
        CHECK,
        "version",
        &[VERSION_COMMAND],
        VERSION_TIMEOUT,
        SITE,
    )?;
    let unknown = |reason: String| {
        Finding::unknown(
            CHECK,
            reason,
            format!(
                "Assume only what `dbxctl doctor` enforces: Databricks CLI {MINIMUM_DATABRICKS_VERSION} or newer."
            ),
            evidence.clone(),
            SITE,
        )
    };
    let stdout = match step {
        Step::Succeeded(stdout) => stdout,
        Step::Unsuccessful(reason) => return Ok(unknown(reason)),
        Step::Failed(finding) => return Ok(finding),
    };
    let Some(version) = std::str::from_utf8(&stdout).ok().and_then(Version::parse) else {
        return Ok(unknown(
            "`databricks version` did not report a recognizable version".to_owned(),
        ));
    };
    if version < MINIMUM_DATABRICKS_VERSION {
        return Ok(Finding::unknown(
            CHECK,
            format!(
                "Databricks CLI {version} is older than the minimum supported {MINIMUM_DATABRICKS_VERSION}"
            ),
            format!(
                "Install Databricks CLI {MINIMUM_DATABRICKS_VERSION} or newer and re-run; evidence from older releases does not describe the supported baseline."
            ),
            evidence,
            SITE,
        ));
    }
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(version.to_string()),
        evidence,
        SITE,
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::run;
    use crate::evidence::RunDir;
    use crate::probe::model::{Finding, State, Verdict};
    use crate::probe::orchestrator::Context;
    use crate::test_support::{
        FakeTransport, ScenarioCli, TempDir, assert_no_mutations, fake_databricks,
    };

    /// Runs the check against a scenario answering `version`.
    fn check(name: &str, response: &str) -> (Finding, Vec<Vec<String>>, TempDir) {
        let temp = TempDir::new(name);
        let cli_dir = temp.path().join("cli");
        let bundle = temp.path().join("bundle");
        fs::create_dir_all(&cli_dir).expect("create CLI dir");
        fs::create_dir_all(&bundle).expect("create bundle");
        let cli = ScenarioCli::new(
            fake_databricks(),
            &cli_dir,
            &format!("case\narg version\n{response}\n"),
        );
        let run_dir = RunDir::create(&bundle, "lineage", 0).expect("create run");
        let finding = run(&Context {
            binary: cli.binary().as_os_str(),
            run: &run_dir,
            transport: &FakeTransport::default(),
            warehouse_id: None,
            scope_catalog: None,
            shared: &crate::probe::workspace::Shared::default(),
        })
        .expect("run check");
        (finding, cli.invocations(), temp)
    }

    #[test]
    fn resolves_the_installed_version() {
        let (finding, invocations, _temp) =
            check("cli-resolved", "stdout Databricks CLI v1.13.0\\n");
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(finding.verdict().map(Verdict::as_str), Some("1.13.0"));
        assert_eq!(invocations, [["version"]]);
        assert_no_mutations(&invocations);
    }

    #[test]
    fn reports_unusable_output_as_unknown() {
        for (name, response, reason) in [
            ("cli-exit", "exit 3", "failed with exit code 3"),
            ("cli-garbage", "stdout nonsense\\n", "recognizable version"),
            ("cli-non-utf8", "stdout \\xff\\n", "recognizable version"),
            ("cli-old", "stdout Databricks CLI v1.12.0\\n", "older than"),
        ] {
            let (finding, _, _temp) = check(name, response);
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn reports_signal_termination_as_unknown() {
        let (finding, _, _temp) = check("cli-signal", "abort");
        assert_eq!(finding.state(), State::Unknown);
        assert!(
            finding.reason().unwrap_or_default().contains("a signal"),
            "{finding:?}"
        );
    }

    #[test]
    fn reports_a_missing_cli_as_failed() {
        let temp = TempDir::new("cli-missing");
        let run_dir = RunDir::create(temp.path(), "lineage", 0).expect("create run");
        let missing = temp.path().join("no-such-databricks");
        let finding = run(&Context {
            binary: missing.as_os_str(),
            run: &run_dir,
            transport: &FakeTransport::default(),
            warehouse_id: None,
            scope_catalog: None,
            shared: &crate::probe::workspace::Shared::default(),
        })
        .expect("run check");
        assert_eq!(finding.state(), State::Failed);
        let reason = finding.reason().unwrap_or_default();
        assert!(
            !reason.contains("no-such-databricks"),
            "reasons must not copy raw error text: {reason}"
        );
        let record =
            fs::read_to_string(run_dir.path().join("evidence/cli/version.json")).expect("read");
        assert!(
            record.contains("\"error\": \"could not execute databricks: "),
            "{record}"
        );
        assert!(!record.contains("no-such-databricks"), "{record}");
    }
}

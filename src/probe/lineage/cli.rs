//! The `cli` check (#30, #32): which Databricks CLI version is installed, and
//! whether it offers every command the suite runs.

use crate::databricks::{MINIMUM_DATABRICKS_VERSION, VERSION_COMMAND, VERSION_TIMEOUT, Version};
use crate::probe::lineage::{Step, run_step};
use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;

const CHECK: CheckId = CheckId::Cli;
/// The code that depends on the answer: the supported-version gate.
const SITE: &str = "src/databricks.rs MINIMUM_DATABRICKS_VERSION";
/// The commands the lineage checks run, as their help names them.
pub(crate) const SURFACES: [&[&str]; 5] = [
    &["bundle", "validate"],
    &["bundle", "plan"],
    &["bundle", "schema"],
    &["api", "get"],
    &["api", "post"],
];

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    // The same command and timeout `dbxctl doctor` uses for its gate.
    let (mut evidence, step) = run_step(
        context,
        CHECK,
        "version",
        &[VERSION_COMMAND],
        VERSION_TIMEOUT,
        SITE,
    )?;
    let unknown = |reason: String, evidence: &[String]| {
        Finding::unknown(
            CHECK,
            reason,
            format!(
                "Assume only what `dbxctl doctor` enforces: Databricks CLI {MINIMUM_DATABRICKS_VERSION} or newer."
            ),
            evidence.to_vec(),
            SITE,
        )
    };
    let stdout = match step {
        Step::Succeeded(stdout) => stdout,
        Step::Unsuccessful(reason) => return Ok(unknown(reason, &evidence)),
        Step::Failed(finding) => return Ok(finding),
    };
    let Some(version) = std::str::from_utf8(&stdout).ok().and_then(Version::parse) else {
        return Ok(unknown(
            "`databricks version` did not report a recognizable version".to_owned(),
            &evidence,
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

    let mut missing = Vec::new();
    for surface in SURFACES {
        let mut args = surface.to_vec();
        args.push("--help");
        let (help, step) = run_step(
            context,
            CHECK,
            &format!("help-{}", surface.join("-")),
            &args,
            VERSION_TIMEOUT,
            SITE,
        )?;
        evidence.extend(help);
        match step {
            Step::Succeeded(stdout) if offers(&stdout, surface) => {}
            Step::Succeeded(_) | Step::Unsuccessful(_) => {
                missing.push(format!("`databricks {}`", surface.join(" ")));
            }
            Step::Failed(finding) => {
                return Ok(Finding::failed(
                    CHECK,
                    finding.reason().unwrap_or_default(),
                    evidence,
                    SITE,
                ));
            }
        }
    }
    if !missing.is_empty() {
        return Ok(Finding::unknown(
            CHECK,
            format!(
                "Databricks CLI {version} does not offer {}",
                missing.join(", ")
            ),
            "Treat the checks that run the missing commands as unknown; Phase 0 expects every command on the supported CLI.",
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

/// Whether `help` is the help for `surface` itself. For an unknown
/// subcommand the CLI prints its parent's help and exits 0, so the usage
/// line must name the whole command: `databricks bundle plan [flags]`, not
/// `databricks bundle [command]`.
fn offers(help: &[u8], surface: &[&str]) -> bool {
    let usage = format!("databricks {}", surface.join(" "));
    String::from_utf8_lossy(help).lines().any(|line| {
        line.trim()
            .strip_prefix(&usage)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;

    use super::{SURFACES, offers, run};
    use crate::evidence::RunDir;
    use crate::probe::lineage::testing::{Fixture, case, run_with};
    use crate::probe::model::{State, Verdict};
    use crate::test_support::{TempDir, assert_no_mutations};

    /// Scenario cases answering each surface's `--help` as CLI 1.13.0 does.
    pub(crate) fn help_cases() -> String {
        SURFACES
            .iter()
            .map(|surface| {
                let mut args = surface.to_vec();
                args.push("--help");
                case(
                    &args,
                    &format!(
                        "stdout Usage:\\n  databricks {} [flags]\\n",
                        surface.join(" ")
                    ),
                )
            })
            .collect()
    }

    fn version_case(response: &str) -> String {
        case(&["version"], response)
    }

    #[test]
    fn resolves_the_installed_version_and_surfaces() {
        let fixture = Fixture::new(
            "cli-resolved",
            &(version_case("stdout Databricks CLI v1.13.0\\n") + &help_cases()),
        );
        let finding = fixture.run(run);
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(finding.verdict().map(Verdict::as_str), Some("1.13.0"));
        let invocations = fixture.invocations();
        assert_eq!(invocations[0], ["version"]);
        assert_eq!(invocations[1], ["bundle", "validate", "--help"]);
        assert_eq!(invocations.len(), 1 + SURFACES.len());
        assert_no_mutations(&invocations);
        assert!(
            fixture
                .evidence("cli", "help-bundle-plan.json")
                .contains("\"bundle\"")
        );
    }

    #[test]
    fn reports_unusable_output_as_unknown() {
        for (name, response, reason) in [
            ("cli-exit", "exit 3", "failed with exit code 3"),
            ("cli-garbage", "stdout nonsense\\n", "recognizable version"),
            ("cli-non-utf8", "stdout \\xff\\n", "recognizable version"),
            ("cli-old", "stdout Databricks CLI v1.12.0\\n", "older than"),
        ] {
            let fixture = Fixture::new(name, &version_case(response));
            let finding = fixture.run(run);
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
            assert_eq!(
                fixture.invocations().len(),
                1,
                "{name}: no help after a bad version"
            );
        }
    }

    #[test]
    fn reports_missing_surfaces_as_unknown() {
        // `bundle plan --help` prints the parent's help, as an unknown
        // subcommand does, and `api post --help` fails.
        let cases = help_cases()
            .replace(
                "databricks bundle plan [flags]",
                "databricks bundle [command]",
            )
            .replace("stdout Usage:\\n  databricks api post [flags]\\n", "exit 1");
        let fixture = Fixture::new(
            "cli-missing-surface",
            &(version_case("stdout Databricks CLI v1.13.0\\n") + &cases),
        );
        let finding = fixture.run(run);
        assert_eq!(finding.state(), State::Unknown, "{finding:?}");
        assert_eq!(
            finding.reason(),
            Some(
                "Databricks CLI 1.13.0 does not offer `databricks bundle plan`, `databricks api post`"
            )
        );
    }

    #[test]
    fn matches_the_whole_usage_line() {
        let surface: &[&str] = &["bundle", "plan"];
        assert!(offers(
            b"Usage:\n  databricks bundle plan [flags]\n",
            surface
        ));
        assert!(offers(b"  databricks bundle plan\n", surface));
        assert!(!offers(b"Usage:\n  databricks bundle [command]\n", surface));
        assert!(!offers(b"  databricks bundle planner [flags]\n", surface));
    }

    #[cfg(unix)]
    #[test]
    fn reports_signal_termination_as_unknown() {
        let fixture = Fixture::new("cli-signal", &version_case("abort"));
        let finding = fixture.run(run);
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
        let finding = run_with(run, missing.as_os_str(), &run_dir, temp.path());
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

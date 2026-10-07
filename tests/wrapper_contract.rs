mod common;

use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use common::scenario::{ScenarioCli, assert_no_mutations};
use common::{TempDir, dbxctl, fake_databricks};

#[test]
fn forwards_arguments_exactly_and_preserves_exit_code() {
    let output = dbxctl()
        .env("DATABRICKS_CLI_PATH", fake_databricks())
        .env("FAKE_DATABRICKS_EXIT_CODE", "42")
        .args(["databricks", "jobs", "list", "two words", "--limit=10"])
        .output()
        .expect("run dbxctl");

    assert_eq!(output.status.code(), Some(42));
    assert_eq!(
        output.stdout,
        b"<jobs>\n<list>\n<two words>\n<--limit=10>\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn help_and_version_do_not_require_databricks() {
    for argument in ["help", "version"] {
        let output = dbxctl()
            .env("DATABRICKS_CLI_PATH", Path::new("definitely-missing"))
            .arg(argument)
            .output()
            .expect("run dbxctl");
        assert!(
            output.status.success(),
            "{argument} must not require Databricks"
        );
    }
}

#[test]
fn reports_missing_databricks_binary() {
    let missing =
        std::env::temp_dir().join(format!("dbxctl-definitely-missing-{}", std::process::id()));
    let output = dbxctl()
        .env("DATABRICKS_CLI_PATH", missing)
        .arg("doctor")
        .output()
        .expect("run dbxctl");
    assert!(!output.status.success());
    // `doctor` renders the problem as diagnostic output rather than aborting.
    assert!(String::from_utf8_lossy(&output.stdout).contains("dbxctl-definitely-missing"));
}

#[test]
fn reports_broken_version_command() {
    let output = dbxctl()
        .env("DATABRICKS_CLI_PATH", fake_databricks())
        .env("FAKE_DATABRICKS_VERSION_MODE", "fail")
        .arg("doctor")
        .output()
        .expect("run dbxctl");
    assert_eq!(output.status.code(), Some(1));
    // The upstream exit status renders as "exit status: 9" on Unix and
    // "exit code: 9" on Windows.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("exit status: 9") || stdout.contains("exit code: 9"),
        "unexpected doctor output: {stdout}"
    );
}

#[test]
fn rejects_malformed_and_non_utf8_version_output() {
    for (mode, expected) in [("malformed", "could not parse"), ("non-utf8", "non-UTF-8")] {
        let output = dbxctl()
            .env("DATABRICKS_CLI_PATH", fake_databricks())
            .env("FAKE_DATABRICKS_VERSION_MODE", mode)
            .arg("doctor")
            .output()
            .expect("run dbxctl");
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains(expected));
    }
}

#[test]
fn enforces_minimum_databricks_version() {
    let output = dbxctl()
        .env("DATABRICKS_CLI_PATH", fake_databricks())
        .env("FAKE_DATABRICKS_VERSION_MODE", "old")
        .arg("doctor")
        .output()
        .expect("run dbxctl");
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Databricks CLI 1.12.0 is unsupported; install 1.13.0 or newer"),
        "{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn propagates_signal_termination_as_128_plus_signal() {
    let output = dbxctl()
        .env("DATABRICKS_CLI_PATH", fake_databricks())
        .env("FAKE_DATABRICKS_ABORT", "1")
        .args(["databricks", "jobs", "list"])
        .output()
        .expect("run dbxctl");
    // SIGABRT is signal 6; the shell convention reports 128 + the signal.
    assert_eq!(output.status.code(), Some(134));
}

#[test]
fn reports_passthrough_spawn_failure() {
    // Passthrough to a binary that cannot be launched propagates the error out
    // through `main`, unlike `doctor`, which renders the problem itself.
    let missing =
        std::env::temp_dir().join(format!("dbxctl-missing-passthrough-{}", std::process::id()));
    let output = dbxctl()
        .env("DATABRICKS_CLI_PATH", missing)
        .args(["databricks", "jobs", "list"])
        .output()
        .expect("run dbxctl");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("failed to run Databricks CLI"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reports_unknown_command() {
    // An unrecognized wrapper command surfaces the parse error on stderr and a
    // failure exit code.
    let output = dbxctl().arg("bogus").output().expect("run dbxctl");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown command bogus"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn doctor_accepts_supported_databricks_cli() {
    let output = dbxctl()
        .env("DATABRICKS_CLI_PATH", fake_databricks())
        .arg("doctor")
        .output()
        .expect("run dbxctl");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Databricks CLI 1.13.0"));
}

#[test]
fn doctor_does_not_share_its_stdin_with_databricks() {
    // The fake CLI reads stdin to EOF. dbxctl's own stdin is an open pipe that
    // never closes, so the check only finishes if the CLI gets an empty stdin.
    let mut child = dbxctl()
        .env("DATABRICKS_CLI_PATH", fake_databricks())
        .env("READ_STDIN", "1")
        .arg("doctor")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run dbxctl");
    let _open_stdin = child.stdin.take();

    let started = Instant::now();
    while child.try_wait().expect("poll dbxctl").is_none() {
        if started.elapsed() > Duration::from_secs(10) {
            child.kill().expect("kill hung dbxctl");
            panic!("doctor waited on stdin shared with the Databricks CLI");
        }
        thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("collect dbxctl output");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Databricks CLI 1.13.0"));
}

#[test]
fn probe_help_succeeds() {
    let output = dbxctl()
        .arg("probe")
        .arg("--help")
        .output()
        .expect("run dbxctl probe --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("probe") && stdout.contains("lineage"));
}

#[test]
fn probe_run_help_succeeds() {
    let output = dbxctl()
        .args(["probe", "run", "--help"])
        .output()
        .expect("run dbxctl probe run --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--suite") && stdout.contains("--bundle-root"));
}

#[test]
fn probe_report_help_succeeds() {
    let output = dbxctl()
        .args(["probe", "report", "--help"])
        .output()
        .expect("run dbxctl probe report --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--from"));
}

#[test]
fn probe_cleanup_help_succeeds() {
    let output = dbxctl()
        .args(["probe", "cleanup", "--help"])
        .output()
        .expect("run dbxctl probe cleanup --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--from"));
}

#[test]
fn probe_run_requires_suite() {
    let output = dbxctl()
        .args(["probe", "run", "--bundle-root", "/tmp", "--target", "dev"])
        .output()
        .expect("run probe run without --suite");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--suite is required"));
}

#[test]
fn probe_run_rejects_unknown_suite() {
    let output = dbxctl()
        .args([
            "probe",
            "run",
            "--suite",
            "unknown",
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
        ])
        .output()
        .expect("run probe run with unknown suite");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown suite"));
}

#[test]
fn probe_run_requires_bundle_root() {
    let output = dbxctl()
        .args(["probe", "run", "--suite", "lineage", "--target", "dev"])
        .output()
        .expect("run probe run without --bundle-root");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--bundle-root is required"));
}

#[test]
fn probe_run_requires_target() {
    let output = dbxctl()
        .args([
            "probe",
            "run",
            "--suite",
            "lineage",
            "--bundle-root",
            "/tmp",
        ])
        .output()
        .expect("run probe run without --target");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--target is required"));
}

const CLI_VERSION_SCENARIO: &str = include_str!("fixtures/scenarios/cli-version.txt");

/// A bundle root and a scenario-backed fake CLI in one scratch directory.
struct ProbeFixture {
    temp: TempDir,
    cli: ScenarioCli,
}

impl ProbeFixture {
    fn new(name: &str, scenario: &str) -> Self {
        let temp = TempDir::new(name);
        std::fs::create_dir(temp.path().join("bundle")).expect("create bundle");
        std::fs::create_dir(temp.path().join("cli")).expect("create CLI dir");
        let cli = ScenarioCli::new(fake_databricks(), &temp.path().join("cli"), scenario);
        Self { temp, cli }
    }

    fn bundle(&self) -> std::path::PathBuf {
        self.temp.path().join("bundle")
    }

    fn run(&self, extra: &[&str]) -> std::process::Output {
        dbxctl()
            .env("DATABRICKS_CLI_PATH", self.cli.binary())
            .args(["probe", "run", "--suite", "lineage", "--bundle-root"])
            .arg(self.bundle())
            .args(["--target", "dev"])
            .args(extra)
            .output()
            .expect("run dbxctl probe run")
    }

    /// The only run directory created so far.
    fn run_dir(&self) -> std::path::PathBuf {
        // Join each component so the separators match what dbxctl prints on
        // Windows.
        let suite = self.bundle().join(".dbxctl").join("probe").join("lineage");
        let runs: Vec<_> = std::fs::read_dir(suite)
            .expect("read runs")
            .map(|entry| entry.expect("read run entry").path())
            .collect();
        assert_eq!(runs.len(), 1, "{runs:?}");
        runs.into_iter().next().expect("one run")
    }
}

#[test]
fn probe_run_cli_check_runs_end_to_end() {
    let fixture = ProbeFixture::new("probe-cli", CLI_VERSION_SCENARIO);
    let output = fixture.run(&["--only", "cli"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.starts_with("cli       resolved  1.13.0\n"),
        "{stdout}"
    );
    let run_dir = fixture.run_dir();
    assert!(
        stdout.ends_with(&format!("run directory: {}\n", run_dir.display())),
        "{stdout}"
    );
    assert!(output.stderr.is_empty());

    assert_eq!(
        std::fs::read(fixture.bundle().join(".dbxctl/.gitignore")).expect("read gitignore"),
        b"*\n"
    );
    let run_json = std::fs::read_to_string(run_dir.join("run.json")).expect("read run.json");
    assert!(run_json.contains("\"status\": \"completed\""), "{run_json}");
    assert!(run_json.contains("\"exit_code\": 0"), "{run_json}");
    for name in ["findings.json", "findings.md", "evidence/cli/version.json"] {
        assert!(run_dir.join(name).is_file(), "missing {name}");
    }

    let invocations = fixture.cli.invocations();
    assert_eq!(invocations, [["--version"]]);
    assert_no_mutations(&invocations);
}

#[test]
fn probe_run_directory_is_ignored_by_git() {
    let fixture = ProbeFixture::new("probe-git", CLI_VERSION_SCENARIO);
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(fixture.bundle())
            .args(args)
            .output()
    };
    if git(&["init", "--quiet"]).is_err() {
        eprintln!("git is not installed; skipping");
        return;
    }
    assert_eq!(fixture.run(&["--only", "cli"]).status.code(), Some(0));
    let status = git(&["status", "--porcelain", "--untracked-files=all"]).expect("git status");
    assert!(status.status.success());
    assert_eq!(String::from_utf8_lossy(&status.stdout), "");
}

#[test]
fn probe_run_with_unresolved_checks_exits_10() {
    let fixture = ProbeFixture::new("probe-unresolved", CLI_VERSION_SCENARIO);
    let output = fixture.run(&["--only", "v6,cli"]);
    assert_eq!(output.status.code(), Some(10));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with(
            "cli       resolved  1.13.0\nv6        skipped   not implemented yet; #34 adds this check\n"
        ),
        "{stdout}"
    );
}

#[test]
fn probe_run_with_a_missing_bundle_root_exits_1() {
    let fixture = ProbeFixture::new("probe-missing-root", CLI_VERSION_SCENARIO);
    std::fs::remove_dir(fixture.bundle()).expect("remove bundle");
    let output = fixture.run(&["--only", "cli"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("error: bundle root ") && stderr.ends_with(" is not a directory\n"),
        "{stderr}"
    );
    assert!(fixture.cli.invocations().is_empty());
}

#[test]
fn probe_report_requires_from() {
    let output = dbxctl()
        .args(["probe", "report"])
        .output()
        .expect("run probe report without --from");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--from is required"));
}

#[test]
fn probe_report_with_valid_options_prints_not_implemented() {
    let output = dbxctl()
        .args(["probe", "report", "--from", "/tmp/run"])
        .output()
        .expect("run probe report with valid options");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not implemented yet"));
}

#[test]
fn probe_cleanup_requires_from() {
    let output = dbxctl()
        .args(["probe", "cleanup"])
        .output()
        .expect("run probe cleanup without --from");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--from is required"));
}

#[test]
fn probe_cleanup_with_valid_options_prints_not_implemented() {
    let output = dbxctl()
        .args(["probe", "cleanup", "--from", "/tmp/run"])
        .output()
        .expect("run probe cleanup with valid options");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not implemented yet"));
}

#[test]
fn probe_run_rejects_unknown_check_id() {
    let output = dbxctl()
        .args([
            "probe",
            "run",
            "--suite",
            "lineage",
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--only",
            "unknown",
        ])
        .output()
        .expect("run probe run with unknown check ID");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown check ID"));
}

#[test]
fn probe_run_rejects_duplicate_check_ids() {
    let output = dbxctl()
        .args([
            "probe",
            "run",
            "--suite",
            "lineage",
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--only",
            "cli,plan,cli",
        ])
        .output()
        .expect("run probe run with duplicate check IDs");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("duplicate check ID"));
}

#[test]
fn probe_run_rejects_empty_check_id() {
    let output = dbxctl()
        .args([
            "probe",
            "run",
            "--suite",
            "lineage",
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--only",
            "cli,,plan",
        ])
        .output()
        .expect("run probe run with empty check ID");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("empty check ID"));
}

fn probe_run(extra: &[&str]) -> std::process::Output {
    dbxctl()
        .args([
            "probe",
            "run",
            "--suite",
            "lineage",
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
        ])
        .args(extra)
        .output()
        .expect("run dbxctl probe run")
}

#[test]
fn probe_run_without_only_requires_warehouse_for_all_checks() {
    // With no --only, every check is selected, so the warehouse requirement
    // lists every warehouse-backed check in the contract's canonical order.
    let output = probe_run(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: --warehouse-id is required by selected checks: v2, v3, v11, v9, v4\n"
    );
}

#[test]
fn probe_run_without_only_requires_scope_catalog_for_catalog_checks() {
    let output = probe_run(&["--warehouse-id", "w"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("error: --scope-catalog is required by selected checks: v3"),
        "{stderr}"
    );
}

#[test]
fn probe_run_with_all_required_options_passes_validation() {
    // Every check runs; all but `cli` are not implemented yet, so the run
    // completes with unresolved findings.
    let fixture = ProbeFixture::new("probe-all-checks", CLI_VERSION_SCENARIO);
    let output = fixture.run(&["--warehouse-id", "w", "--scope-catalog", "c"]);
    assert_eq!(output.status.code(), Some(10));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().count(), 16, "{stdout}");
}

#[test]
fn probe_run_accepts_repeated_options_with_last_value_winning() {
    let fixture = ProbeFixture::new("probe-repeated", CLI_VERSION_SCENARIO);
    let output = fixture.run(&["--target", "prod", "--only", "cli"]);
    assert_eq!(output.status.code(), Some(0));
    let run_json =
        std::fs::read_to_string(fixture.run_dir().join("run.json")).expect("read run.json");
    assert!(run_json.contains("\"target\": \"prod\""), "{run_json}");
}

mod common;

use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use common::{dbxctl, fake_databricks};

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

#[test]
fn probe_run_with_valid_options_prints_not_implemented() {
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
            "cli",
        ])
        .output()
        .expect("run probe run with valid options");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not implemented yet"));
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

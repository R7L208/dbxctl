mod common;

use std::path::Path;

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
    assert!(String::from_utf8_lossy(&output.stdout).contains("exit status: 9"));
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
    assert!(String::from_utf8_lossy(&output.stdout).contains("unsupported"));
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
fn doctor_accepts_supported_databricks_cli() {
    let output = dbxctl()
        .env("DATABRICKS_CLI_PATH", fake_databricks())
        .arg("doctor")
        .output()
        .expect("run dbxctl");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Databricks CLI 0.296.0"));
}

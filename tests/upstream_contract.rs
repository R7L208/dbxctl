mod common;

use std::env;
use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use common::{assert_same_process_result, dbxctl, run_direct, run_wrapped};

#[test]
#[ignore = "requires the pinned Databricks CLI binary"]
fn pinned_databricks_surface_matches_passthrough() {
    let binary = env::var_os("DATABRICKS_CLI_PATH")
        .expect("DATABRICKS_CLI_PATH must identify the pinned upstream binary");
    let binary = Path::new(&binary);

    for args in [
        &["version"][..],
        &["--help"],
        &["api", "--help"],
        &["auth", "--help"],
        &["bundle", "--help"],
        &["configure", "--help"],
        &["current-user", "--help"],
        &["fs", "--help"],
        &["jobs", "--help"],
        &["sync", "--help"],
        &["workspace", "--help"],
    ] {
        let direct = run_direct(binary, args);
        let wrapped = run_wrapped(binary, args);
        assert_same_process_result(&direct, &wrapped);
    }
}

#[test]
#[ignore = "requires the pinned Databricks CLI binary"]
fn doctor_accepts_the_pinned_databricks_cli() {
    let binary = env::var_os("DATABRICKS_CLI_PATH")
        .expect("DATABRICKS_CLI_PATH must identify the pinned upstream binary");

    // The version the real CLI reports, e.g. "Databricks CLI v0.296.0".
    let direct = run_direct(Path::new(&binary), &["version"]);
    let banner = String::from_utf8(direct.stdout).expect("UTF-8 version output");
    let version = banner
        .trim()
        .strip_prefix("Databricks CLI v")
        .unwrap_or_else(|| panic!("unexpected version banner {banner:?}"));

    // Run doctor through captured execution with an open, never-written stdin,
    // so a real CLI that waited on input would hang instead of passing.
    let mut child = dbxctl()
        .env("DATABRICKS_CLI_PATH", &binary)
        .arg("doctor")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run dbxctl doctor");
    let _open_stdin = child.stdin.take();
    let started = Instant::now();
    while child.try_wait().expect("poll dbxctl").is_none() {
        if started.elapsed() > Duration::from_secs(60) {
            child.kill().expect("kill hung dbxctl");
            panic!("dbxctl doctor did not finish against the pinned CLI");
        }
        thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().expect("collect dbxctl output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "doctor failed: {stdout}");
    assert!(
        stdout.contains(&format!("Databricks CLI {version} (")),
        "doctor did not report version {version}: {stdout}"
    );
}

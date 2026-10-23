mod common;

use std::env;
use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use common::{TempDir, assert_same_process_result, dbxctl, run_direct, run_wrapped};

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

    // The version the real CLI reports, e.g. "Databricks CLI v1.13.0".
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

#[test]
#[ignore = "requires the pinned Databricks CLI binary"]
fn bundle_schema_trigger_contract() {
    let binary = env::var_os("DATABRICKS_CLI_PATH")
        .expect("DATABRICKS_CLI_PATH must identify the pinned upstream binary");

    // `bundle schema` needs no bundle or workspace, so an empty root is enough.
    let temp = TempDir::new("contract-v6");
    let output = dbxctl()
        .env("DATABRICKS_CLI_PATH", &binary)
        .args(["probe", "run", "--suite", "lineage", "--bundle-root"])
        .arg(temp.path())
        .args(["--target", "contract", "--only", "v6"])
        .output()
        .expect("run dbxctl probe run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.starts_with("v6        resolved  jobs\n"),
        "the pinned CLI no longer defines table-update triggers only under jobs: {stdout}"
    );
}

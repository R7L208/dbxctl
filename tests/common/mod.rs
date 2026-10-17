#![allow(dead_code)]

pub mod scenario;

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

/// A scratch directory, removed when dropped. `name` must be unique per test.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        let path = env::temp_dir().join(format!("dbxctl-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn dbxctl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dbxctl"))
}

pub fn fake_databricks() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(compile_fake).as_path()
}

fn compile_fake() -> PathBuf {
    let output_dir = env::temp_dir().join(format!("dbxctl-rust-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&output_dir).expect("create fixture output directory");
    let binary = output_dir.join(format!("fake-databricks{}", env::consts::EXE_SUFFIX));
    let status = Command::new("rustc")
        .args([
            "--edition",
            "2024",
            "tests/fixtures/fake_databricks.rs",
            "-o",
        ])
        .arg(&binary)
        .status()
        .expect("execute rustc for test fixture");
    assert!(status.success(), "compile Rust Databricks fixture");
    binary
}

pub fn run_direct(binary: &Path, args: &[&str]) -> Output {
    Command::new(binary)
        .args(args)
        .output()
        .expect("run Databricks CLI directly")
}

pub fn run_wrapped(binary: &Path, args: &[&str]) -> Output {
    dbxctl()
        .env("DATABRICKS_CLI_PATH", binary)
        .arg("databricks")
        .args(args)
        .output()
        .expect("run Databricks CLI through dbxctl")
}

pub fn assert_same_process_result(direct: &Output, wrapped: &Output) {
    assert_eq!(
        wrapped.status.code(),
        direct.status.code(),
        "exit status differs"
    );
    assert_eq!(wrapped.stdout, direct.stdout, "stdout differs");
    assert_eq!(wrapped.stderr, direct.stderr, "stderr differs");
}

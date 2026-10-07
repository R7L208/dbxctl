//! Helpers shared by the crate's unit tests.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

#[path = "../tests/common/scenario.rs"]
mod scenario;

pub(crate) use scenario::{ScenarioCli, assert_no_mutations};

/// The fake Databricks CLI, compiled once per test process.
pub(crate) fn fake_databricks() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(compile_fake).as_path()
}

fn compile_fake() -> PathBuf {
    let output_dir =
        std::env::temp_dir().join(format!("dbxctl-unit-fixture-{}", std::process::id()));
    fs::create_dir_all(&output_dir).expect("create fixture output directory");
    let binary = output_dir.join(format!("fake-databricks{}", std::env::consts::EXE_SUFFIX));
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_databricks.rs");
    let status = Command::new(OsStr::new("rustc"))
        .args([OsStr::new("--edition"), OsStr::new("2024")])
        .arg(source)
        .arg(OsStr::new("-o"))
        .arg(&binary)
        .status()
        .expect("execute rustc for unit-test fixture");
    assert!(status.success(), "compile Rust Databricks fixture");
    binary
}

/// A scratch directory, removed when dropped. `name` must be unique per test.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("dbxctl-test-{name}-{}", std::process::id()));
        // A directory left by an interrupted run of the same process ID.
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Reads every file below `root` as `(relative path, bytes)`, sorted by path,
/// with `/` separators on every platform.
pub(crate) fn read_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read directory") {
            let path = entry.expect("read directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("path below root")
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                files.push((relative, fs::read(&path).expect("read file")));
            }
        }
    }
    files.sort();
    files
}

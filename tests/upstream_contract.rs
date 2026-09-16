mod common;

use std::env;
use std::path::Path;

use common::{assert_same_process_result, run_direct, run_wrapped};

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

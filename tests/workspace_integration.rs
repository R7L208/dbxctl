mod common;

use std::env;
use std::path::PathBuf;
use std::process::{Command, Output};

use common::{assert_same_process_result, run_direct, run_wrapped};

const PROBE_TEXT: &str = "dbxctl integration probe";
/// Base64 of `PROBE_TEXT` followed by a newline, as the import API requires.
const PROBE_TEXT_BASE64: &str = "ZGJ4Y3RsIGludGVncmF0aW9uIHByb2JlCg==";

/// Live-workspace settings. Credentials are never read here: the Databricks
/// CLI resolves them from its own environment or profile configuration.
struct Workspace {
    binary: PathBuf,
    prefix: String,
    catalog: String,
}

impl Workspace {
    fn from_env() -> Self {
        let binary = env::var_os("DATABRICKS_CLI_PATH")
            .expect("DATABRICKS_CLI_PATH must identify the pinned upstream binary")
            .into();
        let prefix = env::var("DBXCTL_IT_PREFIX")
            .expect("DBXCTL_IT_PREFIX must name this run's resources, such as dbxctl_it_local_1");
        assert!(
            is_test_prefix(&prefix),
            "DBXCTL_IT_PREFIX must match dbxctl_it_[a-z0-9_]+ so cleanup cannot select other resources"
        );
        let catalog = env::var("DBXCTL_IT_CATALOG").unwrap_or_else(|_| "workspace".to_owned());
        Self {
            binary,
            prefix,
            catalog,
        }
    }

    fn name(&self, suffix: &str) -> String {
        format!("{}_{suffix}", self.prefix)
    }

    fn wrapped(&self, args: &[&str]) -> Output {
        run_wrapped(&self.binary, args)
    }

    fn cleanup(&self, args: &[&str]) -> Cleanup {
        Cleanup {
            binary: self.binary.clone(),
            args: args.iter().map(|&arg| arg.to_owned()).collect(),
        }
    }

    fn home(&self) -> String {
        let output = self.wrapped(&["current-user", "me", "--output", "json"]);
        let stdout = expect_success(&output, "resolve the current workspace identity");
        let user =
            json_string_field(&stdout, "userName").expect("current-user output has userName");
        format!("/Workspace/Users/{user}")
    }
}

/// Deletes a workspace resource when dropped, including after a failed
/// assertion. Cleanup calls the upstream CLI directly so it does not depend on
/// the wrapper under test, and reports failures without panicking.
struct Cleanup {
    binary: PathBuf,
    args: Vec<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        match Command::new(&self.binary).args(&self.args).output() {
            Ok(output) if output.status.success() => {}
            Ok(output) => eprintln!(
                "cleanup `databricks {}` failed: {}",
                self.args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Err(error) => eprintln!(
                "cleanup `databricks {}` failed: {error}",
                self.args.join(" ")
            ),
        }
    }
}

fn is_test_prefix(prefix: &str) -> bool {
    prefix.strip_prefix("dbxctl_it_").is_some_and(|run| {
        !run.is_empty()
            && run
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

fn expect_success(output: &Output, action: &str) -> String {
    assert!(
        output.status.success(),
        "{action} failed with {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout.clone()).expect("Databricks CLI output is UTF-8")
}

/// Extracts a top-level string value without escape handling, which is
/// sufficient for identifiers such as user names.
fn json_string_field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let quoted_key = format!("\"{key}\"");
    let rest = &json[json.find(&quoted_key)? + quoted_key.len()..];
    let rest = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    rest.split('"').next()
}

#[test]
#[ignore = "requires a Databricks workspace"]
fn authenticated_identity_matches_passthrough() {
    let workspace = Workspace::from_env();
    let args = ["current-user", "me", "--output", "json"];

    let direct = run_direct(&workspace.binary, &args);
    let wrapped = workspace.wrapped(&args);

    expect_success(&direct, "query the current user directly");
    assert_same_process_result(&direct, &wrapped);
}

#[test]
#[ignore = "requires a Databricks workspace"]
fn schema_and_table_are_removed_after_use() {
    let workspace = Workspace::from_env();
    let warehouse_id = env::var("DBXCTL_IT_WAREHOUSE_ID")
        .expect("DBXCTL_IT_WAREHOUSE_ID must identify a SQL warehouse the test identity can use");
    let schema = workspace.name("schema");
    let full_schema = format!("{}.{schema}", workspace.catalog);
    let guard = workspace.cleanup(&["schemas", "delete", &full_schema, "--force"]);

    let created = workspace.wrapped(&["schemas", "create", &schema, &workspace.catalog]);
    expect_success(&created, "create the test schema");

    let statement = format!(
        r#"{{"warehouse_id":"{warehouse_id}","statement":"CREATE TABLE `{}`.`{schema}`.probe AS SELECT 1 AS id","wait_timeout":"50s","on_wait_timeout":"CANCEL"}}"#,
        workspace.catalog
    );
    let executed = workspace.wrapped(&[
        "api",
        "post",
        "/api/2.0/sql/statements",
        "--json",
        &statement,
    ]);
    let response = expect_success(&executed, "create the probe table");
    assert!(
        response.contains("\"SUCCEEDED\""),
        "probe table statement did not succeed: {response}"
    );

    let table = format!("{full_schema}.probe");
    expect_success(
        &workspace.wrapped(&["tables", "get", &table]),
        "read the probe table",
    );

    drop(guard);
    let missing = workspace.wrapped(&["schemas", "get", &full_schema]);
    assert!(
        !missing.status.success(),
        "test schema still exists after cleanup"
    );
}

#[test]
#[ignore = "requires a Databricks workspace"]
fn workspace_file_round_trips_and_is_removed() {
    let workspace = Workspace::from_env();
    let directory = format!("{}/dbxctl-it/{}", workspace.home(), workspace.name("files"));
    let file = format!("{directory}/probe.txt");
    let guard = workspace.cleanup(&["workspace", "delete", &directory, "--recursive"]);

    expect_success(
        &workspace.wrapped(&["workspace", "mkdirs", &directory]),
        "create the test directory",
    );
    let imported = workspace.wrapped(&[
        "workspace",
        "import",
        &file,
        "--format",
        "AUTO",
        "--content",
        PROBE_TEXT_BASE64,
    ]);
    expect_success(&imported, "import the probe file");

    let exported = workspace.wrapped(&["workspace", "export", &file, "--format", "AUTO"]);
    // Text output decodes the exported content.
    let content = expect_success(&exported, "export the probe file");
    assert_eq!(
        content.trim_end(),
        PROBE_TEXT,
        "exported probe file content differs"
    );

    drop(guard);
    let missing = workspace.wrapped(&["workspace", "get-status", &directory]);
    assert!(
        !missing.status.success(),
        "test directory still exists after cleanup"
    );
}

#[test]
fn test_prefix_rejects_names_outside_the_test_namespace() {
    assert!(is_test_prefix("dbxctl_it_123_1"));
    assert!(is_test_prefix("dbxctl_it_local_1760000000"));
    assert!(!is_test_prefix("dbxctl_it_"));
    assert!(!is_test_prefix("default"));
    assert!(!is_test_prefix("dbxctl_it_Run"));
    assert!(!is_test_prefix("dbxctl_it_1/../other"));
}

#[test]
fn json_string_field_reads_compact_and_pretty_output() {
    assert_eq!(
        json_string_field(r#"{"userName":"a@b.c"}"#, "userName"),
        Some("a@b.c")
    );
    assert_eq!(
        json_string_field(
            "{\n  \"id\": \"1\",\n  \"userName\": \"uuid\"\n}",
            "userName"
        ),
        Some("uuid")
    );
    assert_eq!(json_string_field(r#"{"id":"1"}"#, "userName"), None);
}

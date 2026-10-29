mod common;

use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use common::scenario::{ScenarioCli, assert_no_mutations, stdout_line};
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
fn propagates_signal_termination_as_the_signal() {
    use std::os::unix::process::ExitStatusExt;

    let output = dbxctl()
        .env("DATABRICKS_CLI_PATH", fake_databricks())
        .env("FAKE_DATABRICKS_ABORT", "1")
        .args(["databricks", "jobs", "list"])
        .output()
        .expect("run dbxctl");
    // dbxctl is replaced by the CLI, so the caller sees the CLI's SIGABRT
    // (signal 6) itself, exactly as if it had run the CLI directly. A shell
    // reports that as 128 + 6 = 134.
    assert_eq!(output.status.code(), None);
    assert_eq!(output.status.signal(), Some(6));
}

#[cfg(unix)]
#[test]
fn passthrough_replaces_the_dbxctl_process() {
    // The fake CLI prints its own PID. It equals the PID of the spawned
    // dbxctl only if dbxctl replaced itself with the CLI instead of starting
    // it as a child, which is what lets the CLI handle Ctrl-C on its own.
    let child = dbxctl()
        .env("DATABRICKS_CLI_PATH", fake_databricks())
        .env("PRINT_PID", "1")
        .args(["databricks", "jobs", "list"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("run dbxctl");
    let dbxctl_pid = child.id();
    let output = child.wait_with_output().expect("collect dbxctl output");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let cli_pid: u32 = stdout
        .lines()
        .find_map(|line| line.strip_prefix("pid: "))
        .unwrap_or_else(|| panic!("fake CLI did not print its PID: {stdout}"))
        .parse()
        .expect("parse fake CLI PID");
    assert_eq!(cli_pid, dbxctl_pid);
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
    // Requirements apply only to implemented checks, so the help says when.
    assert!(stdout.contains(
        "Warehouse ID for Statement Execution checks (required once a selected one is implemented)"
    ));
    assert!(stdout.contains(
        "Catalog for catalog-scoped checks (required once a selected one is implemented)"
    ));
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

/// What the `cli` check runs: `version`, then the help for each command the
/// suite runs.
fn cli_check_invocations() -> Vec<Vec<String>> {
    [
        &["version"][..],
        &["bundle", "validate", "--help"],
        &["bundle", "plan", "--help"],
        &["bundle", "schema", "--help"],
        &["api", "get", "--help"],
        &["api", "post", "--help"],
    ]
    .iter()
    .map(|argv| argv.iter().map(|argument| (*argument).to_owned()).collect())
    .collect()
}

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
    assert_eq!(invocations, cli_check_invocations());
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
    // An implemented check that comes back unknown, so the test does not
    // depend on which checks are still unimplemented.
    let scenario = CLI_VERSION_SCENARIO.replace("v1.13.0", "v1.12.0");
    let fixture = ProbeFixture::new("probe-unresolved", &scenario);
    let output = fixture.run(&["--only", "cli"]);
    assert_eq!(output.status.code(), Some(10));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("cli       unknown   Databricks CLI 1.12.0 is older than"),
        "{stdout}"
    );
}

#[test]
fn probe_run_with_skipped_checks_exits_10() {
    // Whichever checks are still unimplemented come back skipped, so this
    // checks every skipped line rather than naming one, and holds as checks
    // land. Once every check is implemented there is nothing left to skip.
    let fixture = ProbeFixture::new("probe-skipped", CLI_VERSION_SCENARIO);
    let output = fixture.run(&["--warehouse-id", "w", "--scope-catalog", "c"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let skipped: Vec<&str> = stdout
        .lines()
        .filter(|line| line.get(10..20) == Some("skipped   "))
        .collect();
    for line in &skipped {
        let reason = &line[20..];
        let issue = reason
            .strip_prefix("not implemented yet; #")
            .and_then(|rest| rest.strip_suffix(" adds this check"));
        assert!(
            issue.is_some_and(|issue| !issue.is_empty() && issue.bytes().all(|b| b.is_ascii_digit())),
            "{line}"
        );
    }
    if !skipped.is_empty() {
        assert_eq!(output.status.code(), Some(10), "{stdout}");
    }
}

#[test]
fn probe_run_answers_v6_offline() {
    let schema = include_str!("fixtures/bundle-schema-jobs.json");
    let scenario = format!(
        "{CLI_VERSION_SCENARIO}\ncase\narg bundle\narg schema\n{}\n",
        stdout_line(schema)
    );
    let fixture = ProbeFixture::new("probe-v6", &scenario);
    let output = fixture.run(&["--only", "cli,v6"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.starts_with("cli       resolved  1.13.0\nv6        resolved  jobs\n"),
        "{stdout}"
    );
    let invocations = fixture.cli.invocations();
    let mut expected = cli_check_invocations();
    expected.push(vec!["bundle".to_owned(), "schema".to_owned()]);
    assert_eq!(invocations, expected);
    assert_no_mutations(&invocations);
}

/// Copies the files of the fixture bundle at `from` into `to`.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    for entry in std::fs::read_dir(from).expect("read fixture bundle") {
        let entry = entry.expect("read entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            std::fs::create_dir_all(&target).expect("create directory");
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

#[test]
fn probe_run_answers_the_bundle_checks() {
    let fixture_file = |name: &str| {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/cli-1.13.0/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("read fixture")
    };
    let bundle_case = |args: &[&str], stdout: &str| {
        let mut case = String::from("case\n");
        for argument in args.iter().chain(&["-t", "dev"]) {
            case.push_str("arg ");
            case.push_str(argument);
            case.push('\n');
        }
        case + &stdout_line(stdout) + "\n"
    };
    // `bundle validate` runs once, after a read-only summary and get-status
    // find `workspace.file_path`, so validate cannot create it (R9).
    let file_path = "/Workspace/Users/user@example.test/projects/example-dev/files";
    let status_path = format!(
        "/api/2.0/workspace/get-status?path={}",
        file_path.replace('@', "%40")
    );
    let get_status = format!(
        "case\narg api\narg get\narg {status_path}\narg --debug\narg -t\narg dev\nstdout {{}}\nstderr GET {status_path} HTTP/1.1\\n< HTTP/1.1 200 OK\\n\n"
    );
    let validate = ["bundle", "validate", "-o", "json", "--include-locations"];
    let scenario = [
        CLI_VERSION_SCENARIO.to_owned(),
        bundle_case(
            &["bundle", "summary", "-o", "json"],
            &format!(r#"{{"workspace": {{"file_path": "{file_path}"}}}}"#),
        ),
        get_status,
        bundle_case(&validate, &fixture_file("validate-locations.json")),
        bundle_case(
            &["bundle", "plan", "-o", "json"],
            &fixture_file("plan-mixed.json"),
        ),
        format!(
            "case\narg bundle\narg schema\n{}\n",
            stdout_line(include_str!("fixtures/bundle-schema-scripts.json"))
        ),
    ]
    .concat();
    let fixture = ProbeFixture::new("probe-bundle-checks", &scenario);
    copy_tree(
        std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/cli-1.13.0/bundle"
        )),
        &fixture.bundle(),
    );
    let state = fixture.bundle().join(".databricks/bundle/dev");
    std::fs::create_dir_all(&state).expect("create state dir");
    std::fs::write(state.join("resources.json"), fixture_file("resources.json"))
        .expect("write state");

    let output = fixture.run(&["--only", "cli,validate,plan,state,v8,v1"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    let lines: Vec<_> = stdout.lines().take(6).collect();
    assert_eq!(
        lines,
        [
            "cli       resolved  1.13.0",
            "validate  resolved  engine direct; workspace paths: artifact_path, file_path, resource_path, root_path, state_path; resources: jobs 2, pipelines 1; experimental scripts: none; scripts: none",
            "plan      resolved  plan_version 2; skip: validate; update: new_state.value",
            "state     resolved  state_version 2; ids at /state/resources.<type>.<key>/__id__; config in state; resources: jobs 2, pipelines 1",
            "v8        resolved  paths: file_path 6 (4 files, 2 notebooks without extension); origins: databricks.yml 2, resources/extra.yml 1",
            "v1        resolved  not rejected: experimental.scripts accepts any key",
        ]
    );
    let invocations = fixture.cli.invocations();
    let validates = invocations
        .iter()
        .filter(|argv| {
            argv.get(..3)
                .is_some_and(|path| path == ["bundle", "validate", "-o"])
        })
        .count();
    assert_eq!(
        validates, 1,
        "validate runs once per probe run: {invocations:?}"
    );
    assert_no_mutations(&invocations);
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
fn probe_run_that_cannot_create_its_run_directory_exits_1() {
    // A file where `.dbxctl/` belongs stops the run before any check, so
    // the run fails with 1 rather than reporting unresolved checks with 10.
    let fixture = ProbeFixture::new("probe-unwritable-root", CLI_VERSION_SCENARIO);
    std::fs::write(fixture.bundle().join(".dbxctl"), b"").expect("block .dbxctl");
    let output = fixture.run(&["--only", "cli"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("error: could not create directory ")
            && stderr.ends_with(": it is not a directory\n"),
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

#[test]
fn probe_run_without_only_requires_the_sql_inputs() {
    // Every check is selected, and the implemented SQL checks read a
    // warehouse and a scope catalog, so a run without them is a usage error
    // that starts no CLI process.
    let fixture = ProbeFixture::new("probe-all-no-inputs", CLI_VERSION_SCENARIO);
    let output = fixture.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: --warehouse-id is required by selected checks: v2, v3, v11, v9, v4\n"
    );
    assert!(fixture.cli.invocations().is_empty());

    let output = fixture.run(&["--warehouse-id", "w"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: --scope-catalog is required by selected checks: v3, v11\n"
    );
}

#[test]
fn probe_run_without_only_runs_every_check() {
    // Every check is selected. The fake answers only `version`, so the API
    // and SQL checks come back unknown and the skipped checks unresolved.
    let fixture = ProbeFixture::new("probe-all-checks", CLI_VERSION_SCENARIO);
    let output = fixture.run(&["--warehouse-id", "w", "--scope-catalog", "c"]);
    assert_eq!(output.status.code(), Some(10));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().count(), 16, "{stdout}");
    // v7 is the last check to land (#37), so a skipped line stays here
    // until every check is implemented.
    assert!(
        stdout.contains("\nv7        skipped   not implemented yet; #37 adds this check\n"),
        "{stdout}"
    );
    assert_no_mutations(&fixture.cli.invocations());
}

/// A scenario case for `databricks api get <path> --debug -t dev`, answered
/// with `stdout`, a debug log of `status`, and `exit`.
fn api_get_case(path: &str, stdout: &str, status: &str, body: &str, exit: u8) -> String {
    let log = format!("09:56:51 Debug: GET {path}\n< HTTP/1.0 {status}\n{body}");
    format!(
        "case\narg api\narg get\narg {path}\narg --debug\narg -t\narg dev\n{}\n{}\nexit {exit}\n",
        stdout_line(stdout),
        stdout_line(&log).replacen("stdout ", "stderr ", 1),
    )
}

#[test]
fn probe_run_answers_api_through_the_cli() {
    let bad = "/api/2.0/pipelines/00000000-0000-0000-0000-000000000000";
    let scenario = [
        api_get_case(
            "/api/2.0/clusters/spark-versions",
            r#"{"versions":[]}"#,
            "200 OK",
            "< {} pid=1 sdk=true\n",
            0,
        ),
        api_get_case(
            bad,
            "",
            "404 Not Found",
            "< {\n<   \"error_code\": \"RESOURCE_DOES_NOT_EXIST\"\n< } pid=1 sdk=true\nError: not found\n",
            1,
        ),
    ]
    .concat();
    let fixture = ProbeFixture::new("probe-api", &scenario);
    let output = fixture.run(&["--only", "api"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.starts_with(
            "api       resolved  status and error_code: HTTP 200; HTTP 404 RESOURCE_DOES_NOT_EXIST\n"
        ),
        "{stdout}"
    );
    let invocations = fixture.cli.invocations();
    assert_eq!(invocations.len(), 2);
    assert_no_mutations(&invocations);
    let run_dir = fixture.run_dir();
    for name in ["good.json", "good.stderr", "bad.json", "bad.stderr"] {
        assert!(run_dir.join("evidence/api").join(name).is_file(), "{name}");
    }
}

#[test]
fn probe_run_sends_statements_to_the_given_warehouse() {
    // Nothing answers the submits, so v2 comes back unknown, but each
    // submit names the warehouse and passes the read-only guard.
    let fixture = ProbeFixture::new("probe-v2-warehouse", CLI_VERSION_SCENARIO);
    let output = fixture.run(&["--only", "v2", "--warehouse-id", "wh-0123"]);
    assert_eq!(output.status.code(), Some(10));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("v2        unknown   "), "{stdout}");
    let invocations = fixture.cli.invocations();
    assert_eq!(
        invocations.len(),
        1,
        "it stops at the first unanswered submit"
    );
    assert!(
        invocations[0]
            .iter()
            .any(|argument| argument.contains(r#""warehouse_id":"wh-0123""#)
                && argument.contains(r#""wait_timeout":"0s""#)),
        "{:?}",
        invocations[0]
    );
    assert_no_mutations(&invocations);
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

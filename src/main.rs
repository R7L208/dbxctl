mod args;
mod databricks;
mod evidence;
mod exit;
mod json;
mod probe;
#[cfg(test)]
mod test_support;

use std::env;
use std::ffi::{OsStr, OsString};
use std::process::ExitCode;

#[cfg(test)]
use databricks::{Version, resolve_binary};

#[derive(Debug)]
enum Cli {
    Doctor,
    Databricks(Vec<OsString>),
    Probe(Vec<OsString>),
    Help,
    Version,
}

fn parse_args(mut args: impl Iterator<Item = OsString>) -> Result<Cli, String> {
    let _program = args.next();
    match args.next().as_deref() {
        Some(command) if command == "doctor" => no_extra_args(args, Cli::Doctor),
        Some(command) if command == "databricks" => Ok(Cli::Databricks(args.collect())),
        Some(command) if command == "probe" => Ok(Cli::Probe(args.collect())),
        Some(command) if command == "help" || command == "--help" || command == "-h" => {
            no_extra_args(args, Cli::Help)
        }
        Some(command) if command == "version" || command == "--version" || command == "-V" => {
            no_extra_args(args, Cli::Version)
        }
        Some(command) => Err(format!(
            "unknown command {}; run `dbxctl help` for usage",
            command.display()
        )),
        None => Ok(Cli::Help),
    }
}

fn no_extra_args(mut args: impl Iterator<Item = OsString>, command: Cli) -> Result<Cli, String> {
    args.next().map_or(Ok(command), |argument| {
        Err(format!("unexpected argument {}", argument.display()))
    })
}

fn print_help() {
    println!(
        "dbxctl {}\n\nUSAGE:\n    dbxctl <COMMAND>\n\nCOMMANDS:\n    doctor                Show dependency status\n    databricks [ARGS]...  Run the Databricks CLI\n    probe [ARGS]...       Probe workspace for lineage discovery\n    help                  Print help\n    version               Print version",
        env!("CARGO_PKG_VERSION")
    );
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let binary = databricks::binary();
    run_with_binary(cli, binary.as_os_str())
}

fn run_with_binary(cli: Cli, binary: &OsStr) -> Result<ExitCode, String> {
    match cli {
        Cli::Help => {
            print_help();
            Ok(ExitCode::SUCCESS)
        }
        Cli::Version => {
            println!("dbxctl {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Cli::Doctor => Ok(databricks::run_doctor(binary)),
        Cli::Databricks(args) => databricks::run_passthrough(binary, args),
        Cli::Probe(args) => {
            // Top-level probe help: no subcommand, or only a help argument.
            let help_only = match args.as_slice() {
                [] => true,
                [argument] => matches!(argument.to_str(), Some("--help" | "-h" | "help")),
                _ => false,
            };
            if help_only {
                probe::print_probe_help();
                return Ok(ExitCode::SUCCESS);
            }
            probe::run_probe(probe::parse_probe(&args)?, binary)
        }
    }
}

fn main() -> ExitCode {
    let result = parse_args(env::args_os()).and_then(run);
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            exit::Exit::Usage.into()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{Cli, Version, parse_args, resolve_binary};

    fn args(values: &[&str]) -> impl Iterator<Item = OsString> {
        values.iter().map(OsString::from)
    }

    #[test]
    fn parses_current_cli_version() {
        assert_eq!(
            Version::parse("Databricks CLI v1.13.0\n"),
            Some(Version::new(1, 13, 0))
        );
    }

    #[test]
    fn parses_prerelease_version() {
        assert_eq!(
            Version::parse("Databricks CLI v1.2.3-preview\n"),
            Some(Version::new(1, 2, 3))
        );
    }

    #[test]
    fn rejects_unrelated_output() {
        assert_eq!(Version::parse("not installed"), None);
    }

    #[test]
    fn ignores_unrelated_numbers_before_the_banner() {
        assert_eq!(
            Version::parse("build 12345\nDatabricks CLI v1.13.0\n"),
            Some(Version::new(1, 13, 0))
        );
    }

    #[test]
    fn rejects_incomplete_and_invalid_versions() {
        assert_eq!(Version::parse("Databricks CLI v1.2"), None);
        assert_eq!(Version::parse("Databricks CLI v1.two.3"), None);
        assert_eq!(Version::parse("Databricks CLI v1.2.three"), None);
    }

    #[test]
    fn rejects_missing_minor_and_non_numeric_major() {
        // Only a major segment: the minor `?` short-circuits to None.
        assert_eq!(Version::parse("Databricks CLI v1"), None);
        // Non-numeric major: the `.ok()?` on the major segment short-circuits.
        assert_eq!(Version::parse("Databricks CLI vx.2.3"), None);
    }

    #[test]
    fn resolves_binary_from_override_or_defaults_to_path() {
        assert_eq!(
            resolve_binary(Some(OsString::from("/opt/databricks/bin/databricks"))),
            PathBuf::from("/opt/databricks/bin/databricks")
        );
        // No override: falls back to bare `databricks` for `PATH` resolution.
        assert_eq!(resolve_binary(None), PathBuf::from("databricks"));
    }

    #[test]
    fn parses_commands_and_aliases() {
        assert!(matches!(parse_args(args(&["dbxctl"])), Ok(Cli::Help)));
        assert!(matches!(
            parse_args(args(&["dbxctl", "doctor"])),
            Ok(Cli::Doctor)
        ));
        assert!(matches!(
            parse_args(args(&["dbxctl", "help"])),
            Ok(Cli::Help)
        ));
        assert!(matches!(
            parse_args(args(&["dbxctl", "--help"])),
            Ok(Cli::Help)
        ));
        assert!(matches!(parse_args(args(&["dbxctl", "-h"])), Ok(Cli::Help)));
        assert!(matches!(
            parse_args(args(&["dbxctl", "version"])),
            Ok(Cli::Version)
        ));
        assert!(matches!(
            parse_args(args(&["dbxctl", "--version"])),
            Ok(Cli::Version)
        ));
        assert!(matches!(
            parse_args(args(&["dbxctl", "-V"])),
            Ok(Cli::Version)
        ));
    }

    #[test]
    fn preserves_all_databricks_arguments() {
        let cli = parse_args(args(&[
            "dbxctl",
            "databricks",
            "jobs",
            "list",
            "--profile",
            "two words",
        ]))
        .expect("parse passthrough command");
        let Cli::Databricks(forwarded) = cli else {
            panic!("expected passthrough command");
        };
        assert_eq!(
            forwarded,
            ["jobs", "list", "--profile", "two words"].map(OsString::from)
        );
    }

    #[test]
    fn rejects_unknown_commands_and_extra_wrapper_arguments() {
        assert_eq!(
            parse_args(args(&["dbxctl", "unknown"])).expect_err("unknown command must fail"),
            "unknown command unknown; run `dbxctl help` for usage"
        );
        assert_eq!(
            parse_args(args(&["dbxctl", "doctor", "extra"])).expect_err("extra argument must fail"),
            "unexpected argument extra"
        );
    }

    #[test]
    fn parses_probe_command() {
        let cli = parse_args(args(&[
            "dbxctl",
            "probe",
            "run",
            "--suite",
            "lineage",
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
        ]))
        .expect("parse probe command");
        assert!(matches!(cli, Cli::Probe(_)));
    }

    #[test]
    fn preserves_all_probe_arguments() {
        let cli = parse_args(args(&[
            "dbxctl",
            "probe",
            "run",
            "--suite",
            "lineage",
            "--bundle-root",
            "/bundle",
            "--target",
            "mytarget",
            "--only",
            "cli,validate",
        ]))
        .expect("parse probe command");
        let Cli::Probe(args) = cli else {
            panic!("expected probe command");
        };
        assert!(!args.is_empty());
    }
}

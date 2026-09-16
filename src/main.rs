use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

const MINIMUM_DATABRICKS_VERSION: Version = Version::new(0, 200, 0);

#[derive(Debug)]
enum Cli {
    Doctor,
    Databricks(Vec<OsString>),
    Help,
    Version,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    fn parse(output: &str) -> Option<Self> {
        let candidate = output
            .split_whitespace()
            .find(|part| {
                part.trim_start_matches('v')
                    .starts_with(|c: char| c.is_ascii_digit())
            })?
            .trim_start_matches('v');
        let mut parts = candidate.split('.');
        Some(Self::new(
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.split_once('-').map_or_else(
                || candidate_patch(candidate),
                |(patch, _)| patch.parse().ok(),
            )?,
        ))
    }
}

fn candidate_patch(candidate: &str) -> Option<u64> {
    candidate.split('.').nth(2)?.parse().ok()
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn databricks_binary() -> PathBuf {
    env::var_os("DATABRICKS_CLI_PATH").map_or_else(|| PathBuf::from("databricks"), PathBuf::from)
}

fn parse_args(mut args: impl Iterator<Item = OsString>) -> Result<Cli, String> {
    let _program = args.next();
    match args.next().as_deref() {
        Some(command) if command == "doctor" => no_extra_args(args, Cli::Doctor),
        Some(command) if command == "databricks" => Ok(Cli::Databricks(args.collect())),
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
        "dbxctl {}\n\nUSAGE:\n    dbxctl <COMMAND>\n\nCOMMANDS:\n    doctor                Show dependency status\n    databricks [ARGS]...  Run the Databricks CLI\n    help                  Print help\n    version               Print version",
        env!("CARGO_PKG_VERSION")
    );
}

fn installed_databricks_version(binary: &OsStr) -> Result<Version, String> {
    let output = Command::new(binary)
        .arg("version")
        .output()
        .map_err(|error| format!("could not execute {}: {error}", binary.display()))?;

    if !output.status.success() {
        return Err(format!(
            "{} version exited with {}",
            binary.display(),
            output.status
        ));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| format!("{} version returned non-UTF-8 output", binary.display()))?;
    Version::parse(&stdout)
        .ok_or_else(|| format!("could not parse Databricks CLI version from {stdout:?}"))
}

fn validate_databricks(binary: &OsStr) -> Result<Version, String> {
    let version = installed_databricks_version(binary)?;
    if version < MINIMUM_DATABRICKS_VERSION {
        return Err(format!(
            "Databricks CLI {version} is unsupported; install {MINIMUM_DATABRICKS_VERSION} or newer"
        ));
    }
    Ok(version)
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let binary = databricks_binary();
    run_with_binary(cli, binary.as_os_str())
}

fn run_with_binary(cli: Cli, binary: &OsStr) -> Result<ExitCode, String> {
    if matches!(cli, Cli::Help | Cli::Version) {
        match cli {
            Cli::Help => print_help(),
            Cli::Version => println!("dbxctl {}", env!("CARGO_PKG_VERSION")),
            Cli::Doctor | Cli::Databricks(_) => unreachable!(),
        }
        return Ok(ExitCode::SUCCESS);
    }

    let version = validate_databricks(binary)?;

    match cli {
        Cli::Doctor => {
            println!("dbxctl {}", env!("CARGO_PKG_VERSION"));
            println!("Databricks CLI {version} ({})", binary.display());
            Ok(ExitCode::SUCCESS)
        }
        Cli::Databricks(args) => {
            let status = Command::new(binary)
                .args(args)
                .status()
                .map_err(|error| format!("failed to run Databricks CLI: {error}"))?;
            Ok(status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .map_or(ExitCode::FAILURE, ExitCode::from))
        }
        Cli::Help | Cli::Version => unreachable!(),
    }
}

fn main() -> ExitCode {
    let result = parse_args(env::args_os()).and_then(run);
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{Cli, Version, parse_args};

    fn args(values: &[&str]) -> impl Iterator<Item = OsString> {
        values.iter().map(OsString::from)
    }

    #[test]
    fn parses_current_cli_version() {
        assert_eq!(
            Version::parse("Databricks CLI v0.296.0\n"),
            Some(Version::new(0, 296, 0))
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
    fn rejects_incomplete_and_invalid_versions() {
        assert_eq!(Version::parse("Databricks CLI v1.2"), None);
        assert_eq!(Version::parse("Databricks CLI v1.two.3"), None);
        assert_eq!(Version::parse("Databricks CLI v1.2.three"), None);
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
}

mod lineage;
mod model;
mod orchestrator;

use std::ffi::{OsStr, OsString};
use std::process::ExitCode;

use crate::args::Parser;
use crate::evidence::SystemClock;
use crate::exit::Exit;

#[derive(Debug)]
pub(crate) enum ProbeCommand {
    Run(Box<RunOptions>),
    #[allow(dead_code)] // Read by `probe report` (#36)
    Report(ReportOptions),
    #[allow(dead_code)] // Read by `probe cleanup` (#37)
    Cleanup(CleanupOptions),
    Help,
}

#[derive(Debug)]
pub(crate) struct RunOptions {
    // Always `lineage`: `parse_run` rejects every other suite.
    #[allow(dead_code)]
    pub(crate) suite: String,
    pub(crate) lineage: lineage::LineageOptions,
}

#[derive(Debug)]
pub(crate) struct ReportOptions {
    #[allow(dead_code)] // Used by `probe report` (#36)
    pub(crate) from: OsString,
}

#[derive(Debug)]
pub(crate) struct CleanupOptions {
    #[allow(dead_code)] // Used by `probe cleanup` (#37)
    pub(crate) from: OsString,
}

pub(crate) fn parse_probe(args: &[OsString]) -> Result<ProbeCommand, String> {
    if args.is_empty() {
        return Err(
            "probe requires a subcommand (run, report, cleanup); run `dbxctl probe --help` for usage"
                .to_string(),
        );
    }

    let cmd = &args[0];
    let remaining = &args[1..];

    match cmd.to_string_lossy().as_ref() {
        "run" => parse_run(remaining),
        "report" => parse_report(remaining),
        "cleanup" => parse_cleanup(remaining),
        _ => Err(format!(
            "unknown probe subcommand {}; run `dbxctl probe --help` for usage",
            cmd.display()
        )),
    }
}

fn parse_run(args: &[OsString]) -> Result<ProbeCommand, String> {
    let mut parser = Parser::new(args.iter());

    // Check for help early (takes precedence, exits 0 without validating required options)
    if parser.help_requested() {
        print_run_help();
        return Ok(ProbeCommand::Help);
    }

    let suite = parser
        .option("suite")?
        .ok_or("option --suite is required")?
        .to_string_lossy()
        .to_string();

    if suite != "lineage" {
        return Err(format!(
            "unknown suite {suite}; only 'lineage' is supported"
        ));
    }

    let lineage = lineage::parse_lineage_options(&mut parser)?;
    parser.check_empty()?;

    Ok(ProbeCommand::Run(Box::new(RunOptions { suite, lineage })))
}

fn parse_report(args: &[OsString]) -> Result<ProbeCommand, String> {
    let mut parser = Parser::new(args.iter());

    if parser.help_requested() {
        print_report_help();
        return Ok(ProbeCommand::Help);
    }

    let from = parser
        .option("from")?
        .ok_or("option --from is required for probe report")?
        .to_os_string();

    parser.check_empty()?;

    Ok(ProbeCommand::Report(ReportOptions { from }))
}

fn parse_cleanup(args: &[OsString]) -> Result<ProbeCommand, String> {
    let mut parser = Parser::new(args.iter());

    if parser.help_requested() {
        print_cleanup_help();
        return Ok(ProbeCommand::Help);
    }

    let from = parser
        .option("from")?
        .ok_or("option --from is required for probe cleanup")?
        .to_os_string();

    parser.check_empty()?;

    Ok(ProbeCommand::Cleanup(CleanupOptions { from }))
}

/// Execute a parsed probe command with the resolved Databricks CLI `binary`.
///
/// `probe run` prints one line per finding and the run directory, and exits
/// 0 only when every selected check resolved (see `src/exit.rs`).
pub(crate) fn run_probe(command: ProbeCommand, binary: &OsStr) -> Result<ExitCode, String> {
    match command {
        ProbeCommand::Help => {
            // Help was already printed in parse phase; exit cleanly
            Ok(Exit::Success.into())
        }
        ProbeCommand::Run(options) => {
            let plan = orchestrator::Plan::from_options(&options.lineage)?;
            let outcome = orchestrator::run(&plan, binary, &SystemClock)?;
            for finding in &outcome.findings {
                let detail = finding
                    .verdict()
                    .map(model::Verdict::as_str)
                    .or(finding.reason())
                    .unwrap_or_default();
                println!(
                    "{:<9} {:<9} {detail}",
                    finding.check().as_str(),
                    finding.state().as_str()
                );
            }
            println!("run directory: {}", outcome.run_dir.display());
            Ok(outcome.exit.into())
        }
        ProbeCommand::Report(_) => {
            eprintln!("probe report: not implemented yet");
            Ok(Exit::Usage.into())
        }
        ProbeCommand::Cleanup(_) => {
            eprintln!("probe cleanup: not implemented yet");
            Ok(Exit::Usage.into())
        }
    }
}

pub(crate) fn print_probe_help() {
    println!(
        "dbxctl probe - Probe Databricks workspace for lineage discovery

USAGE:
    dbxctl probe <SUBCOMMAND> [OPTIONS]

SUBCOMMANDS:
    run      Execute lineage probes
    report   Generate a report from probe run results
    cleanup  Clean up probe run artifacts

OPTIONS:
    --help, -h    Print help

Use 'dbxctl probe <SUBCOMMAND> --help' for more information on a subcommand."
    );
}

fn print_run_help() {
    println!(
        "dbxctl probe run - Execute lineage probes

USAGE:
    dbxctl probe run --suite <SUITE> --bundle-root <PATH> --target <TARGET> [OPTIONS]

OPTIONS:
    --suite <SUITE>                 The probe suite to run (only 'lineage' is supported)
    --bundle-root <PATH>            Root directory of the Databricks bundle
    --target <TARGET>               Bundle target name
    --profile <PROFILE>             Databricks CLI profile name
    --warehouse-id <ID>             Warehouse ID (required for checks using Statement Execution)
    --scope-catalog <CATALOG>       Catalog name (required for checks using catalog-scoped probes)
    --only <IDS>                    Run only specific checks (comma-separated check IDs)
    --allow-mutations                Allow mutation capabilities (required to unlock mutation checks)
    --v6-table <TABLE>              Per-check mutation input for v6
    --v10-scratch-schema <SCHEMA>   Per-check mutation input for v10
    --v7-pipeline-id <ID>           Per-check mutation input for v7
    --promote-to <DIR>              Directory to copy approved redacted evidence to
    --help, -h                      Print help"
    );
}

fn print_report_help() {
    println!(
        "dbxctl probe report - Generate a report from probe run results

USAGE:
    dbxctl probe report --from <RUN-DIRECTORY>

OPTIONS:
    --from <RUN-DIRECTORY>  The probe run directory (contains run.json)
    --help, -h              Print help"
    );
}

fn print_cleanup_help() {
    println!(
        "dbxctl probe cleanup - Clean up probe run artifacts

USAGE:
    dbxctl probe cleanup --from <RUN-DIRECTORY>

OPTIONS:
    --from <RUN-DIRECTORY>  The probe run directory to clean up
    --help, -h              Print help"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn rejects_missing_run_suite() {
        let error = parse_run(&args(&[])).unwrap_err();
        assert!(error.contains("--suite is required"));
    }

    #[test]
    fn rejects_unknown_suite() {
        let error = parse_run(&args(&[
            "--suite",
            "unknown",
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
        ]))
        .unwrap_err();
        assert!(error.contains("unknown suite"));
    }

    #[test]
    fn accepts_lineage_suite() {
        let cmd = parse_run(&args(&[
            "--suite",
            "lineage",
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--only",
            "cli",
        ]))
        .unwrap();
        match cmd {
            ProbeCommand::Run(opts) => {
                assert_eq!(opts.suite, "lineage");
            }
            _ => panic!("expected Run command"),
        }
    }

    #[test]
    fn help_in_run() {
        let cmd = parse_run(&args(&["--help"])).unwrap();
        assert!(matches!(cmd, ProbeCommand::Help));
    }

    #[test]
    fn help_short_in_run() {
        let cmd = parse_run(&args(&["-h"])).unwrap();
        assert!(matches!(cmd, ProbeCommand::Help));
    }

    #[test]
    fn rejects_missing_report_from() {
        let error = parse_report(&args(&[])).unwrap_err();
        assert!(error.contains("--from is required"));
    }

    #[test]
    fn accepts_report_from() {
        let cmd = parse_report(&args(&["--from", "/tmp/run"])).unwrap();
        match cmd {
            ProbeCommand::Report(opts) => {
                assert_eq!(opts.from.to_string_lossy(), "/tmp/run");
            }
            _ => panic!("expected Report command"),
        }
    }

    #[test]
    fn help_in_report() {
        let cmd = parse_report(&args(&["--help"])).unwrap();
        assert!(matches!(cmd, ProbeCommand::Help));
    }

    #[test]
    fn rejects_missing_cleanup_from() {
        let error = parse_cleanup(&args(&[])).unwrap_err();
        assert!(error.contains("--from is required"));
    }

    #[test]
    fn accepts_cleanup_from() {
        let cmd = parse_cleanup(&args(&["--from", "/tmp/run"])).unwrap();
        match cmd {
            ProbeCommand::Cleanup(opts) => {
                assert_eq!(opts.from.to_string_lossy(), "/tmp/run");
            }
            _ => panic!("expected Cleanup command"),
        }
    }

    #[test]
    fn help_in_cleanup() {
        let cmd = parse_cleanup(&args(&["--help"])).unwrap();
        assert!(matches!(cmd, ProbeCommand::Help));
    }

    #[test]
    fn rejects_unknown_probe_subcommand() {
        let error = parse_probe(&args(&["unknown"])).unwrap_err();
        assert!(error.contains("unknown probe subcommand"));
    }

    #[test]
    fn rejects_empty_probe_subcommand() {
        let error = parse_probe(&args(&[])).unwrap_err();
        assert!(error.contains("probe requires a subcommand"));
    }

    #[test]
    fn options_in_any_order_run() {
        let cmd = parse_run(&args(&[
            "--target",
            "dev",
            "--bundle-root",
            "/tmp",
            "--suite",
            "lineage",
            "--only",
            "validate",
        ]))
        .unwrap();
        match cmd {
            ProbeCommand::Run(opts) => {
                assert_eq!(opts.suite, "lineage");
            }
            _ => panic!("expected Run command"),
        }
    }
}

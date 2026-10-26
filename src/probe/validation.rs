//! The probe run's one `bundle validate` (#32), shared by the `validate`,
//! `plan`, and `v8` checks.
//!
//! On CLI 1.13.0, `bundle validate` creates `workspace.file_path`
//! (`POST /api/2.0/workspace/mkdirs`) when that directory does not exist yet,
//! so it is not read-only ([R9]). Before running it, the probe reads the
//! path with `bundle summary -o json`, which resolves the same configuration
//! but only reads, and checks it with
//! `api get /api/2.0/workspace/get-status`. Validate runs only if the
//! directory exists, or if `--allow-mutations` lets it be created; otherwise
//! every check that needs it is unknown, naming R9.
//!
//! Validate runs once per probe run, with `--include-locations` so `v8` gets
//! where each resource is defined, and the first check that needs it records
//! the summary, get-status, and validate runs as its evidence. Later checks
//! reuse the result and refer to the same evidence.
//!
//! [R9]: ../../docs/mv-lineage/phase0/01-requirements.md#r9-read-only-by-default

use std::cell::OnceCell;
use std::fmt::Write as _;
use std::process::ExitStatus;

use crate::bundle::config::{BUNDLE_TIMEOUT, SUMMARY_COMMAND, VALIDATE_COMMAND};
use crate::json::{self, Document, Node};
use crate::probe::lineage::{Ran, exit_text, run_bundle_step};
use crate::probe::model::CheckId;
use crate::probe::orchestrator::Context;
use crate::transport::Error;

/// The API path that reports whether a workspace path exists.
const GET_STATUS: &str = "/api/2.0/workspace/get-status";

/// What the probe run's validate produced.
pub(crate) enum Outcome {
    /// The configuration validate printed, and how it exited: CLI 1.13.0
    /// prints it even when it exits 1.
    Config {
        config: Document,
        status: ExitStatus,
    },
    /// A reason for an unknown finding: validate was not run, or printed no
    /// configuration.
    Unknown(String),
    /// A reason for a failed finding: a command could not be run.
    Failed(String),
}

/// The shared result, and the evidence records behind it.
pub(crate) struct Validation {
    pub(crate) evidence: Vec<String>,
    pub(crate) outcome: Outcome,
}

/// Holds the probe run's validation once a check has asked for it.
#[derive(Default)]
pub(crate) struct Shared(OnceCell<Validation>);

/// The probe run's validation, run for `check` if no check has asked yet.
/// Returns an error if evidence cannot be written.
pub(crate) fn validation<'a>(
    context: &Context<'a>,
    check: CheckId,
) -> Result<&'a Validation, String> {
    let shared = context.shared;
    if let Some(validation) = shared.0.get() {
        return Ok(validation);
    }
    let validation = validate(context, check)?;
    Ok(shared.0.get_or_init(|| validation))
}

fn validate(context: &Context<'_>, check: CheckId) -> Result<Validation, String> {
    let mut evidence = Vec::new();
    let file_path = match file_path(context, check, &mut evidence)? {
        Ok(file_path) => file_path,
        Err(outcome) => return Ok(Validation { evidence, outcome }),
    };
    match exists(context, check, &file_path, &mut evidence)? {
        Ok(true) => {}
        Ok(false) if context.allow_mutations => {}
        Ok(false) => {
            return Ok(Validation {
                evidence,
                outcome: Outcome::Unknown(
                    "`workspace.file_path` does not exist, and `bundle validate` would create it, so it was not run without --allow-mutations (R9)".to_owned(),
                ),
            });
        }
        Err(outcome) => return Ok(Validation { evidence, outcome }),
    }
    let (records, ran) = run_bundle_step(
        context,
        check,
        "validate",
        &VALIDATE_COMMAND,
        BUNDLE_TIMEOUT,
    )?;
    evidence.extend(records);
    let command = format!("`databricks {}`", VALIDATE_COMMAND.join(" "));
    let outcome = match ran {
        Ran::Failed(reason) => Outcome::Failed(reason),
        Ran::Completed { status, stdout } => match json::parse_bytes(&stdout) {
            Ok(config) if config.root().members().is_some() => Outcome::Config { config, status },
            _ if status.success() => {
                Outcome::Unknown(format!("{command} did not print a JSON object"))
            }
            _ => Outcome::Unknown(format!(
                "{command} failed with {} and printed no JSON object",
                exit_text(status)
            )),
        },
    };
    Ok(Validation { evidence, outcome })
}

/// `workspace.file_path` from `bundle summary`, or the outcome that stops
/// validation.
fn file_path(
    context: &Context<'_>,
    check: CheckId,
    evidence: &mut Vec<String>,
) -> Result<Result<String, Outcome>, String> {
    let (records, ran) =
        run_bundle_step(context, check, "summary", &SUMMARY_COMMAND, BUNDLE_TIMEOUT)?;
    evidence.extend(records);
    let unresolved = |why: String| {
        Outcome::Unknown(format!(
            "`workspace.file_path` could not be read without running `bundle validate`, which may create it (R9): {why}"
        ))
    };
    let stdout = match ran {
        Ran::Completed { status, stdout } if status.success() => stdout,
        Ran::Completed { status, .. } => {
            return Ok(Err(unresolved(format!(
                "`databricks bundle summary -o json` failed with {}",
                exit_text(status)
            ))));
        }
        Ran::Failed(reason) => return Ok(Err(Outcome::Failed(reason))),
    };
    let summary = json::parse_bytes(&stdout).ok();
    let path = summary
        .as_ref()
        .and_then(|summary| summary.root().pointer("/workspace/file_path"))
        .and_then(Node::as_str)
        .filter(|path| path.starts_with('/'));
    Ok(path
        .map(str::to_owned)
        .ok_or_else(|| unresolved("the summary has no absolute `workspace.file_path`".to_owned())))
}

/// Whether the workspace has `file_path`, from a read-only get-status: a
/// 404 means no. Any other failure is the outcome that stops validation.
fn exists(
    context: &Context<'_>,
    check: CheckId,
    file_path: &str,
    evidence: &mut Vec<String>,
) -> Result<Result<bool, Outcome>, String> {
    let request = format!("{GET_STATUS}?path={}", encode(file_path));
    let outcome = context.cli.api_get(&request);
    for (index, call) in outcome.calls.iter().enumerate() {
        let step = if index == 0 {
            "get-status".to_owned()
        } else {
            format!("get-status-{index}")
        };
        evidence.push(context.run.record_invocation(
            check.as_str(),
            &step,
            context.binary,
            &call.args,
            &call.result,
        )?);
    }
    let unchecked = |why: String| {
        Outcome::Unknown(format!(
            "whether `workspace.file_path` exists could not be checked, so `bundle validate`, which would create it, was not run (R9): {why}"
        ))
    };
    Ok(match outcome.result {
        Ok(_) => Ok(true),
        Err(Error::Http { status: 404, .. }) => Ok(false),
        Err(Error::Http { status, .. }) => {
            Err(unchecked(format!("get-status answered HTTP {status}")))
        }
        Err(_) => Err(unchecked(
            "the get-status call returned no usable response; see the evidence record".to_owned(),
        )),
    })
}

/// `text` percent-encoded for a query value: everything but unreserved
/// characters and `/`.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::encode;
    use crate::probe::lineage::testing::{Fixture, VALIDATE, bundle_case, gate_cases};
    use crate::probe::lineage::{plan, v8, validate};
    use crate::probe::model::State;
    use crate::test_support::{assert_no_mutations, stdout_line};

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cli-1.13.0");

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{FIXTURES}/{name}")).expect("read fixture")
    }

    #[test]
    fn checks_share_one_validate() {
        let scenario = gate_cases(200)
            + &bundle_case(&VALIDATE, &stdout_line(&fixture("validate-locations.json")))
            + &bundle_case(
                &["bundle", "plan", "-o", "json"],
                &stdout_line(&fixture("plan-skip.json")),
            );
        let probe = Fixture::new("validation-shared", &scenario);
        let findings = probe.run_together(&[validate::run, plan::run, v8::run]);
        assert!(
            findings
                .iter()
                .all(|finding| finding.state() == State::Resolved),
            "{findings:?}"
        );
        let invocations = probe.invocations();
        let validates = invocations
            .iter()
            .filter(|argv| argv[..2] == ["bundle", "validate"])
            .count();
        assert_eq!(validates, 1, "{invocations:?}");
        assert_eq!(invocations.len(), 4, "summary, get-status, validate, plan");
        assert_no_mutations(&invocations);
        // The first check records the shared runs; the others refer to them.
        let record = probe.evidence("validate", "validate.json");
        assert!(record.contains("--include-locations"), "{record}");
    }

    #[test]
    fn a_withheld_validate_is_unknown_for_every_check() {
        let scenario = gate_cases(404)
            + &bundle_case(
                &["bundle", "plan", "-o", "json"],
                &stdout_line(&fixture("plan-skip.json")),
            );
        let probe = Fixture::new("validation-withheld", &scenario);
        let findings = probe.run_together(&[validate::run, plan::run, v8::run]);
        for finding in [&findings[0], &findings[2]] {
            assert_eq!(finding.state(), State::Unknown, "{finding:?}");
            assert!(finding.reason().unwrap_or_default().contains("(R9)"));
        }
        let invocations = probe.invocations();
        assert!(
            invocations
                .iter()
                .all(|argv| argv[..2] != ["bundle", "validate"]),
            "{invocations:?}"
        );
        assert_no_mutations(&invocations);
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(
            encode("/Workspace/Users/user@example.test/my project/files"),
            "/Workspace/Users/user%40example.test/my%20project/files"
        );
        assert_eq!(encode("a&b=c?d%"), "a%26b%3Dc%3Fd%25");
        assert_eq!(encode("é"), "%C3%A9");
    }
}

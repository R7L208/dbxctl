//! The `v2` check (#33): which `wait_timeout` values Statement Execution
//! accepts, probed with `SELECT 1`.
//!
//! Submits the statement once per value, on each side of both documented
//! edges. A statement the submit leaves running is cancelled. Each submit
//! runs for at most the transport's call limit, or its wait plus a margin
//! when longer, and never past the statement deadline, so with the default
//! limits the five take at most five minutes.

use std::time::Duration;

use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;
use crate::probe::workspace::{Reply, describe, list_or_none, step, warehouse};
use crate::transport::Error;

const CHECK: CheckId = CheckId::V2;
/// The code that depends on the answer: the submit wait the transport asks
/// for.
const SITE: &str = "src/transport.rs WAIT_TIMEOUT";
const PROBE: &str = "SELECT 1";
/// Each side of the documented range, 0 or 5 to 50 seconds.
const WAITS: [u64; 5] = [0, 4, 5, 50, 51];
/// The waits the API documents as valid. If it rejects all of them, it is
/// rejecting the request, such as an unknown warehouse, not the wait.
const DOCUMENTED: [u64; 3] = [0, 5, 50];
const FALLBACK: &str = "Assume the documented range, 0 or 5 to 50 seconds; the transport's 30-second submit wait is inside it.";

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    let warehouse_id = match warehouse(context, CHECK, SITE) {
        Ok(warehouse_id) => warehouse_id,
        Err(finding) => return Ok(finding),
    };
    let mut evidence = Vec::new();
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for seconds in WAITS {
        let wait = Duration::from_secs(seconds);
        let reply = step(
            context,
            CHECK,
            &format!("wait-{seconds}s"),
            SITE,
            &mut evidence,
            |transport| transport.submit(PROBE, warehouse_id, wait),
        )?;
        match reply {
            Reply::Answer(_) => accepted.push(seconds),
            Reply::Error(error) if rejects_wait(&error) => rejected.push(seconds),
            Reply::Error(error) => {
                return Ok(Finding::unknown(
                    CHECK,
                    format!(
                        "the submit of `{PROBE}` with `wait_timeout` {seconds}s {}",
                        describe(&error)
                    ),
                    FALLBACK,
                    evidence,
                    SITE,
                ));
            }
            Reply::Failed(finding) => return Ok(finding),
        }
    }
    if !DOCUMENTED.iter().any(|seconds| accepted.contains(seconds)) {
        return Ok(Finding::unknown(
            CHECK,
            format!(
                "none of the waits the API documents as valid ({}) was accepted, so the request itself was likely rejected, such as for an unknown warehouse",
                seconds_list(&DOCUMENTED)
            ),
            FALLBACK,
            evidence,
            SITE,
        ));
    }
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(format!(
            "accepted {}; rejected {}",
            seconds_list(&accepted),
            seconds_list(&rejected)
        )),
        evidence,
        SITE,
    ))
}

/// Whether `error` rejects the wait rather than the request: a bad request
/// whose `error_code` is `INVALID_PARAMETER_VALUE` or whose message names
/// `wait_timeout`. Any other error, such as an unknown warehouse with
/// another code, is not an answer about the wait.
fn rejects_wait(error: &Error) -> bool {
    let Error::Http {
        status: 400,
        error_code,
        message,
    } = error
    else {
        return false;
    };
    error_code.as_deref() == Some("INVALID_PARAMETER_VALUE")
        || message
            .as_deref()
            .is_some_and(|message| message.contains("wait_timeout"))
}

/// `values` as seconds, such as `0s, 5s`, or `none`.
fn seconds_list(values: &[u64]) -> String {
    let seconds: Vec<String> = values.iter().map(|value| format!("{value}s")).collect();
    list_or_none(&seconds)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{PROBE, WAITS, run};
    use crate::evidence::RunDir;
    use crate::probe::model::{Finding, State, Verdict};
    use crate::probe::orchestrator::Context;
    use crate::test_support::{FakeTransport, TempDir, denied};
    use crate::transport::Error;

    fn rejected() -> Error {
        Error::Http {
            status: 400,
            error_code: Some("INVALID_PARAMETER_VALUE".to_owned()),
            message: None,
        }
    }

    /// A transport that accepts the documented range.
    fn documented() -> FakeTransport {
        FakeTransport::default()
            .submit(secs(0), "PENDING")
            .submit_error(secs(4), rejected())
            .submit(secs(5), "SUCCEEDED")
            .submit(secs(50), "SUCCEEDED")
            .submit_error(secs(51), rejected())
    }

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    fn check(name: &str, transport: &FakeTransport, warehouse_id: Option<&str>) -> Finding {
        let temp = TempDir::new(name);
        let run_dir = RunDir::create(temp.path(), "lineage", 0).expect("create run");
        run(&Context {
            binary: "databricks".as_ref(),
            run: &run_dir,
            bundle_root: std::path::Path::new("."),
            target: "dev",
            allow_mutations: false,
            validation: &crate::probe::validation::Shared::default(),
            transport,
            warehouse_id,
            scope_catalog: None,
            shared: &crate::probe::workspace::Shared::default(),
        })
        .expect("run check")
    }

    #[test]
    fn resolves_which_waits_are_accepted() {
        let transport = documented();
        let finding = check("v2-resolved", &transport, Some("w"));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some("accepted 0s, 5s, 50s; rejected 4s, 51s")
        );
        let asked: Vec<_> = WAITS
            .iter()
            .map(|seconds| format!("SUBMIT {seconds}s {PROBE}"))
            .collect();
        assert_eq!(transport.asked(), asked);
        assert_eq!(finding.evidence().len(), WAITS.len());
    }

    #[test]
    fn records_a_range_that_differs_from_the_documentation() {
        let transport = WAITS
            .iter()
            .fold(FakeTransport::default(), |transport, &seconds| {
                transport.submit(secs(seconds), "SUCCEEDED")
            });
        let verdict = check("v2-all", &transport, Some("w"))
            .verdict()
            .map(|verdict| verdict.as_str().to_owned());
        assert_eq!(
            verdict.as_deref(),
            Some("accepted 0s, 4s, 5s, 50s, 51s; rejected none")
        );
        // A narrower range than documented still answers the question.
        let transport = FakeTransport::default()
            .submit_error(secs(0), rejected())
            .submit_error(secs(4), rejected())
            .submit(secs(5), "SUCCEEDED")
            .submit_error(secs(50), rejected())
            .submit_error(secs(51), rejected());
        let verdict = check("v2-narrow", &transport, Some("w"))
            .verdict()
            .map(|verdict| verdict.as_str().to_owned());
        assert_eq!(
            verdict.as_deref(),
            Some("accepted 5s; rejected 0s, 4s, 50s, 51s")
        );
    }

    #[test]
    fn counts_only_a_rejection_of_the_wait() {
        let bad_request = |code: Option<&str>, message: Option<&str>| Error::Http {
            status: 400,
            error_code: code.map(str::to_owned),
            message: message.map(str::to_owned),
        };
        // Named by its message, without the usual code.
        let transport = documented().submit_error(
            secs(4),
            bad_request(Some("BAD_REQUEST"), Some("wait_timeout must be 0 or 5-50s")),
        );
        let finding = check("v2-message", &transport, Some("w"));
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        // Any other bad request is not an answer about the wait.
        let transport =
            documented().submit_error(secs(4), bad_request(Some("BAD_REQUEST"), Some("bad JSON")));
        let finding = check("v2-other-400", &transport, Some("w"));
        assert_eq!(finding.state(), State::Unknown);
        assert_eq!(
            finding.reason(),
            Some("the submit of `SELECT 1` with `wait_timeout` 4s failed with HTTP 400")
        );
    }

    #[test]
    fn reports_a_rejected_request_as_unknown() {
        // An unknown warehouse is rejected with the same code on every
        // submit, including the waits that must be valid.
        let unknown_warehouse =
            WAITS
                .iter()
                .fold(FakeTransport::default(), |transport, &seconds| {
                    transport.submit_error(
                        secs(seconds),
                        Error::Http {
                            status: 400,
                            error_code: Some("INVALID_PARAMETER_VALUE".to_owned()),
                            message: Some("Warehouse wh-x does not exist".to_owned()),
                        },
                    )
                });
        let finding = check("v2-bad-warehouse", &unknown_warehouse, Some("wh-x"));
        assert_eq!(finding.state(), State::Unknown);
        assert_eq!(
            finding.reason(),
            Some(
                "none of the waits the API documents as valid (0s, 5s, 50s) was accepted, so the request itself was likely rejected, such as for an unknown warehouse"
            )
        );
        assert!(finding.fallback().is_some());
    }

    #[test]
    fn reports_other_errors_as_unknown() {
        let transport = FakeTransport::default()
            .submit(secs(0), "SUCCEEDED")
            .submit_error(
                secs(4),
                Error::Http {
                    status: 403,
                    error_code: None,
                    message: None,
                },
            );
        let finding = check("v2-denied", &transport, Some("w"));
        assert_eq!(finding.state(), State::Unknown);
        assert_eq!(
            finding.reason(),
            Some(
                "the submit of `SELECT 1` with `wait_timeout` 4s failed with HTTP 403 (permission denied)"
            )
        );
        assert!(finding.fallback().is_some());
        // It stops at the first error.
        assert_eq!(transport.asked().len(), 2);

        let transport = FakeTransport::default().submit_error(secs(0), denied());
        let finding = check("v2-sql-denied", &transport, Some("w"));
        assert_eq!(finding.state(), State::Unknown);

        // A `SELECT 1` that ended on the warehouse says nothing about the
        // wait it was submitted with.
        let transport = FakeTransport::default().submit_error(
            secs(0),
            Error::Sql {
                state: "CANCELED".to_owned(),
                error_code: None,
                message: None,
            },
        );
        let finding = check("v2-ended", &transport, Some("w"));
        assert_eq!(finding.state(), State::Unknown);
        assert_eq!(
            finding.reason(),
            Some("the submit of `SELECT 1` with `wait_timeout` 0s ended CANCELED")
        );
    }

    #[test]
    fn reports_a_cli_that_cannot_run_as_failed() {
        let finding = check("v2-unrunnable", &FakeTransport::unrunnable(), Some("w"));
        assert_eq!(finding.state(), State::Failed);
    }

    #[test]
    fn reports_a_missing_warehouse_as_failed() {
        let transport = FakeTransport::default();
        let finding = check("v2-no-warehouse", &transport, None);
        assert_eq!(finding.state(), State::Failed);
        assert_eq!(
            finding.reason(),
            Some("--warehouse-id was not given, though v2 reads it")
        );
        assert!(transport.asked().is_empty());
    }
}

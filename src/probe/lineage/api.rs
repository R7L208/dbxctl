//! The `api` check (#33): does `databricks api --debug` expose the HTTP
//! status and `error_code` the transport classifies errors by?
//!
//! Reads one path that any user may read and one that cannot exist, and
//! looks for the status of each and the `error_code` of the failure.

use crate::probe::model::{CheckId, Finding, Verdict};
use crate::probe::orchestrator::Context;
use crate::probe::workspace::{Reply, describe, step, word};
use crate::transport::Error;

const CHECK: CheckId = CheckId::Api;
/// The code that depends on the answer: classifying a `databricks api` run.
const SITE: &str = "src/transport.rs response";
/// Readable by any workspace user, and holds no user data.
const GOOD: &str = "/api/2.0/clusters/spark-versions";
/// No pipeline has the nil UUID as its ID.
const BAD: &str = "/api/2.0/pipelines/00000000-0000-0000-0000-000000000000";
const FALLBACK: &str = "Classify a failed `databricks api` call by its exit code alone, as an error with no HTTP status or `error_code`.";

pub(crate) fn run(context: &Context<'_>) -> Result<Finding, String> {
    let mut evidence = Vec::new();
    let good = step(context, CHECK, "good", SITE, &mut evidence, |transport| {
        transport.api_get(GOOD)
    })?;
    let unknown = |reason: String, evidence: &[String]| {
        Finding::unknown(CHECK, reason, FALLBACK, evidence.to_vec(), SITE)
    };
    let good = match good {
        Reply::Answer(response) => match response.status {
            Some(status) => status,
            None => {
                return Ok(unknown(
                    format!("the debug log of the successful GET of `{GOOD}` shows no HTTP status"),
                    &evidence,
                ));
            }
        },
        Reply::Error(error) => {
            return Ok(unknown(
                format!(
                    "the GET of `{GOOD}`, which any user may read, {}",
                    describe(&error)
                ),
                &evidence,
            ));
        }
        Reply::Failed(finding) => return Ok(finding),
    };
    let bad = step(context, CHECK, "bad", SITE, &mut evidence, |transport| {
        transport.api_get(BAD)
    })?;
    let (status, code) = match bad {
        Reply::Error(Error::Http {
            status,
            error_code: Some(code),
            ..
        }) => (status, code),
        Reply::Error(Error::Http { status, .. }) => {
            return Ok(unknown(
                format!("the failed GET of `{BAD}` logged HTTP {status} but no `error_code`"),
                &evidence,
            ));
        }
        Reply::Error(error) => {
            return Ok(unknown(
                format!(
                    "the GET of `{BAD}`, which cannot exist, {} with no HTTP error status",
                    describe(&error)
                ),
                &evidence,
            ));
        }
        Reply::Answer(_) => {
            return Ok(unknown(
                format!("the GET of `{BAD}`, which cannot exist, succeeded"),
                &evidence,
            ));
        }
        Reply::Failed(finding) => return Ok(finding),
    };
    Ok(Finding::resolved(
        CHECK,
        Verdict::new(format!(
            "status and error_code: HTTP {good}; HTTP {status} {}",
            word(&code)
        )),
        evidence,
        SITE,
    ))
}

#[cfg(test)]
mod tests {
    use super::{BAD, GOOD, run};
    use crate::evidence::RunDir;
    use crate::probe::model::{Finding, State, Verdict};
    use crate::probe::orchestrator::Context;
    use crate::test_support::{FakeTransport, TempDir};
    use crate::transport::Error;

    fn http(status: u16, code: Option<&str>) -> Error {
        Error::Http {
            status,
            error_code: code.map(str::to_owned),
            message: None,
        }
    }

    fn check(name: &str, transport: &FakeTransport) -> Finding {
        let temp = TempDir::new(name);
        let run_dir = RunDir::create(temp.path(), "lineage", 0).expect("create run");
        run(&Context {
            binary: "databricks".as_ref(),
            run: &run_dir,
            transport,
            warehouse_id: None,
            scope_catalog: None,
            shared: &crate::probe::workspace::Shared::default(),
        })
        .expect("run check")
    }

    #[test]
    fn resolves_the_status_and_error_code() {
        let transport = FakeTransport::default()
            .get(GOOD, r#"{"versions":[]}"#)
            .get_error(BAD, http(404, Some("RESOURCE_DOES_NOT_EXIST")));
        let finding = check("api-resolved", &transport);
        assert_eq!(finding.state(), State::Resolved, "{finding:?}");
        assert_eq!(
            finding.verdict().map(Verdict::as_str),
            Some("status and error_code: HTTP 200; HTTP 404 RESOURCE_DOES_NOT_EXIST")
        );
        assert_eq!(
            transport.asked(),
            [format!("GET {GOOD}"), format!("GET {BAD}")]
        );
        assert_eq!(finding.evidence().len(), 2);
    }

    #[test]
    fn keeps_the_error_code_to_one_word() {
        let transport = FakeTransport::default()
            .get(GOOD, "{}")
            .get_error(BAD, http(404, Some("NOT FOUND\nfake: line")));
        let verdict = check("api-word", &transport)
            .verdict()
            .map(|verdict| verdict.as_str().to_owned());
        assert_eq!(
            verdict.as_deref(),
            Some("status and error_code: HTTP 200; HTTP 404 NOT?FOUND?fake:?line")
        );
    }

    #[test]
    fn reports_missing_status_or_error_code_as_unknown() {
        for (name, transport, reason) in [
            (
                "api-no-status",
                FakeTransport::default()
                    .get_answer(GOOD, None, Some("{}"))
                    .get_error(BAD, http(404, Some("X"))),
                "shows no HTTP status",
            ),
            (
                "api-good-fails",
                FakeTransport::default()
                    .get_error(GOOD, http(403, None))
                    .get_error(BAD, http(404, Some("X"))),
                "which any user may read, failed with HTTP 403 (permission denied)",
            ),
            (
                "api-no-code",
                FakeTransport::default()
                    .get(GOOD, "{}")
                    .get_error(BAD, http(404, None)),
                "logged HTTP 404 but no `error_code`",
            ),
            (
                "api-no-error-status",
                FakeTransport::default()
                    .get(GOOD, "{}")
                    .get_error(BAD, Error::Transport("exit 1".to_owned())),
                "returned output that is not the expected response with no HTTP error status",
            ),
            (
                "api-bad-succeeds",
                FakeTransport::default().get(GOOD, "{}").get(BAD, "{}"),
                "which cannot exist, succeeded",
            ),
        ] {
            let finding = check(name, &transport);
            assert_eq!(finding.state(), State::Unknown, "{name}");
            let actual = finding.reason().unwrap_or_default();
            assert!(actual.contains(reason), "{name}: {actual}");
            assert!(finding.fallback().is_some(), "{name}");
        }
    }

    #[test]
    fn reports_a_cli_that_cannot_run_as_failed() {
        let finding = check("api-unrunnable", &FakeTransport::unrunnable());
        assert_eq!(finding.state(), State::Failed);
        assert_eq!(
            finding.reason(),
            Some(
                "the good step returned output that is not the expected response; see the evidence record"
            )
        );
    }
}

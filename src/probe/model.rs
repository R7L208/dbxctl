//! Check identifiers and findings for the lineage suite (#23).

use std::fmt;

use crate::json::Value;

/// A lineage check. Declaration order is the canonical run and report order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum CheckId {
    Cli,
    Validate,
    Plan,
    State,
    V8,
    V1,
    Api,
    V2,
    V3,
    V11,
    V9,
    V4,
    V6,
    V10,
    V7,
}

impl CheckId {
    /// Every check, in canonical order.
    pub(crate) const ALL: [Self; 15] = [
        Self::Cli,
        Self::Validate,
        Self::Plan,
        Self::State,
        Self::V8,
        Self::V1,
        Self::Api,
        Self::V2,
        Self::V3,
        Self::V11,
        Self::V9,
        Self::V4,
        Self::V6,
        Self::V10,
        Self::V7,
    ];

    /// The stable, command-facing identifier.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Validate => "validate",
            Self::Plan => "plan",
            Self::State => "state",
            Self::V8 => "v8",
            Self::V1 => "v1",
            Self::Api => "api",
            Self::V2 => "v2",
            Self::V3 => "v3",
            Self::V11 => "v11",
            Self::V9 => "v9",
            Self::V4 => "v4",
            Self::V6 => "v6",
            Self::V10 => "v10",
            Self::V7 => "v7",
        }
    }

    pub(crate) fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|check| check.as_str() == id)
    }

    /// The step issue that implements this check, from the #23 checks table.
    pub(crate) const fn issue(self) -> &'static str {
        match self {
            Self::Cli | Self::Validate | Self::Plan | Self::State | Self::V8 | Self::V1 => "#32",
            Self::Api | Self::V2 | Self::V3 | Self::V11 | Self::V9 | Self::V4 => "#33",
            Self::V6 => "#34",
            Self::V10 | Self::V7 => "#37",
        }
    }
}

impl fmt::Display for CheckId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Whether a check answered its question.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum State {
    /// The evidence answers the question; the verdict records the answer.
    Resolved,
    /// The evidence does not answer the question. Later phases use the
    /// recorded fallback.
    Unknown,
    /// The check did not run.
    Skipped,
    /// The check could not gather its evidence.
    Failed,
}

impl State {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Unknown => "unknown",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// A check's answer, such as a version or a field location. Its vocabulary
/// is defined by each check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Verdict(String);

impl Verdict {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// One check's outcome. The constructors enforce the contract: a resolved
/// finding has a verdict, an unknown one has a reason and a fallback, and
/// every finding except a skipped one names the code or design site that
/// depends on it.
///
/// Reasons and fallbacks are written by dbxctl, never copied from captured
/// output, so reports built from findings carry no raw evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Finding {
    check: CheckId,
    state: State,
    verdict: Option<Verdict>,
    evidence: Vec<String>,
    reason: Option<String>,
    fallback: Option<String>,
    site: Option<&'static str>,
}

impl Finding {
    pub(crate) fn resolved(
        check: CheckId,
        verdict: Verdict,
        evidence: Vec<String>,
        site: &'static str,
    ) -> Self {
        Self {
            check,
            state: State::Resolved,
            verdict: Some(verdict),
            evidence,
            reason: None,
            fallback: None,
            site: Some(site),
        }
    }

    pub(crate) fn unknown(
        check: CheckId,
        reason: impl Into<String>,
        fallback: impl Into<String>,
        evidence: Vec<String>,
        site: &'static str,
    ) -> Self {
        Self {
            check,
            state: State::Unknown,
            verdict: None,
            evidence,
            reason: Some(reason.into()),
            fallback: Some(fallback.into()),
            site: Some(site),
        }
    }

    pub(crate) fn skipped(check: CheckId, reason: impl Into<String>) -> Self {
        Self {
            check,
            state: State::Skipped,
            verdict: None,
            evidence: Vec::new(),
            reason: Some(reason.into()),
            fallback: None,
            site: None,
        }
    }

    pub(crate) fn failed(
        check: CheckId,
        reason: impl Into<String>,
        evidence: Vec<String>,
        site: &'static str,
    ) -> Self {
        Self {
            check,
            state: State::Failed,
            verdict: None,
            evidence,
            reason: Some(reason.into()),
            fallback: None,
            site: Some(site),
        }
    }

    pub(crate) fn check(&self) -> CheckId {
        self.check
    }

    pub(crate) fn state(&self) -> State {
        self.state
    }

    pub(crate) fn verdict(&self) -> Option<&Verdict> {
        self.verdict.as_ref()
    }

    pub(crate) fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// The finding as a JSON object. Absent fields are `null`, so every
    /// finding has the same shape.
    pub(crate) fn to_value(&self) -> Value {
        Value::object([
            ("check", Value::string(self.check.as_str())),
            ("state", Value::string(self.state.as_str())),
            (
                "verdict",
                Value::optional_string(self.verdict.as_ref().map(Verdict::as_str)),
            ),
            (
                "evidence",
                Value::Array(self.evidence.iter().map(Value::string).collect()),
            ),
            ("reason", Value::optional_string(self.reason.as_deref())),
            ("fallback", Value::optional_string(self.fallback.as_deref())),
            ("site", Value::optional_string(self.site)),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckId, Finding, State, Verdict};
    use crate::json::Document;

    #[test]
    fn check_ids_round_trip_in_canonical_order() {
        let ids: Vec<_> = CheckId::ALL.map(CheckId::as_str).to_vec();
        assert_eq!(
            ids,
            [
                "cli", "validate", "plan", "state", "v8", "v1", "api", "v2", "v3", "v11", "v9",
                "v4", "v6", "v10", "v7"
            ]
        );
        for check in CheckId::ALL {
            assert_eq!(CheckId::parse(check.as_str()), Some(check));
            assert_eq!(check.to_string(), check.as_str());
        }
        assert_eq!(CheckId::parse("V1"), None);
        assert_eq!(CheckId::parse(""), None);
        let mut sorted = CheckId::ALL;
        sorted.sort();
        assert_eq!(sorted, CheckId::ALL, "Ord must follow canonical order");
    }

    #[test]
    fn every_check_names_its_issue() {
        assert_eq!(CheckId::Validate.issue(), "#32");
        assert_eq!(CheckId::V4.issue(), "#33");
        assert_eq!(CheckId::V6.issue(), "#34");
        assert_eq!(CheckId::V7.issue(), "#37");
    }

    #[test]
    fn findings_serialize_with_a_uniform_shape() {
        let resolved = Finding::resolved(
            CheckId::Cli,
            Verdict::new("1.13.0"),
            vec!["evidence/cli/version.json".to_owned()],
            "src/databricks.rs",
        );
        assert_eq!(resolved.check(), CheckId::Cli);
        assert_eq!(resolved.state(), State::Resolved);
        assert_eq!(resolved.verdict().map(Verdict::as_str), Some("1.13.0"));
        assert_eq!(resolved.reason(), None);
        assert_eq!(
            Document::from(resolved.to_value())
                .to_compact_string()
                .expect("serialize"),
            "{\"check\":\"cli\",\"evidence\":[\"evidence/cli/version.json\"],\"fallback\":null,\"reason\":null,\"site\":\"src/databricks.rs\",\"state\":\"resolved\",\"verdict\":\"1.13.0\"}\n"
        );

        let unknown = Finding::unknown(CheckId::Plan, "why", "instead", Vec::new(), "site");
        assert_eq!(unknown.state(), State::Unknown);
        assert_eq!(unknown.reason(), Some("why"));
        assert_eq!(
            Document::from(unknown.to_value())
                .to_compact_string()
                .expect("serialize"),
            "{\"check\":\"plan\",\"evidence\":[],\"fallback\":\"instead\",\"reason\":\"why\",\"site\":\"site\",\"state\":\"unknown\",\"verdict\":null}\n"
        );

        let skipped = Finding::skipped(CheckId::V7, "later");
        assert_eq!(skipped.state(), State::Skipped);
        assert!(skipped.to_value() != unknown.to_value());

        let failed = Finding::failed(CheckId::Cli, "broken", Vec::new(), "site");
        assert_eq!(failed.state(), State::Failed);
        assert_eq!(State::Failed.as_str(), "failed");
        assert_eq!(State::Skipped.as_str(), "skipped");
    }
}

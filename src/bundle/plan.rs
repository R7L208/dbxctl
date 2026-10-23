//! Deployment plans, from `databricks bundle plan -o json` (#32).
//!
//! On CLI 1.13.0 a plan is an object with `plan_version` (2), `cli_version`,
//! and `plan`, which maps each resource, named `resources.<type>.<key>`, to
//! its `action`. A `create` or `update` carries the configuration it would
//! deploy in `new_state.value`, and the deployed copy in `remote_state` once
//! the resource exists. A `skip` or `delete` carries no `new_state`, so its
//! configuration has to come from `bundle validate`.

use std::collections::BTreeMap;

use crate::bundle::state::resource_name;
use crate::json::Node;

/// The CLI arguments that print the plan.
pub(crate) const PLAN_COMMAND: [&str; 4] = ["bundle", "plan", "-o", "json"];
/// Where an entry carries the configuration it would deploy.
pub(crate) const CONFIG_POINTER: &str = "/new_state/value";

/// One planned resource.
#[derive(Debug)]
pub(crate) struct Entry<'a> {
    /// The resource type, such as `jobs`.
    pub(crate) kind: &'a str,
    pub(crate) key: &'a str,
    pub(crate) action: &'a str,
    /// The configuration the plan would deploy, if the entry carries it.
    pub(crate) config: Option<Node<'a>>,
}

/// A parsed plan.
#[derive(Debug)]
pub(crate) struct Plan<'a> {
    /// `plan_version`, if it is a number.
    pub(crate) version: Option<i64>,
    /// The entries, sorted by resource name.
    pub(crate) entries: Vec<Entry<'a>>,
}

/// `plan` read as a CLI 1.13.0 plan, or why it cannot be: no `plan` object,
/// an entry not named `resources.<type>.<key>`, or an entry without a string
/// `action`.
pub(crate) fn read(plan: Node<'_>) -> Result<Plan<'_>, String> {
    let members = plan
        .get("plan")
        .and_then(Node::members)
        .ok_or("the plan has no `plan` object")?;
    let mut entries = BTreeMap::new();
    for (name, entry) in members {
        let (kind, key) =
            resource_name(name).ok_or("a plan entry is not named `resources.<type>.<key>`")?;
        let action = entry
            .get("action")
            .and_then(Node::as_str)
            .ok_or("a plan entry has no string `action`")?;
        let config = entry
            .pointer(CONFIG_POINTER)
            .filter(|config| config.members().is_some());
        entries.insert(
            name,
            Entry {
                kind,
                key,
                action,
                config,
            },
        );
    }
    Ok(Plan {
        version: plan.get("plan_version").and_then(Node::as_i64),
        entries: entries.into_values().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::read;
    use crate::json;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cli-1.13.0");

    fn actions(name: &str) -> Vec<(String, String, bool)> {
        let text = std::fs::read_to_string(format!("{FIXTURES}/{name}")).expect("read fixture");
        let plan = json::parse(&text).expect("parse fixture");
        let plan = read(plan.root()).expect("read plan");
        assert_eq!(plan.version, Some(2));
        plan.entries
            .iter()
            .map(|entry| {
                (
                    format!("{}.{}", entry.kind, entry.key),
                    entry.action.to_owned(),
                    entry.config.is_some(),
                )
            })
            .collect()
    }

    fn owned(rows: [(&str, &str, bool); 3]) -> Vec<(String, String, bool)> {
        rows.map(|(name, action, config)| (name.to_owned(), action.to_owned(), config))
            .to_vec()
    }

    #[test]
    fn reads_the_plan_fixtures() {
        assert_eq!(
            actions("plan-create.json"),
            owned([
                ("jobs.gold_job", "create", true),
                ("jobs.refresh_silver", "create", true),
                ("pipelines.dlt_pipeline", "create", true),
            ])
        );
        assert_eq!(
            actions("plan-mixed.json"),
            owned([
                ("jobs.gold_job", "skip", false),
                ("jobs.refresh_silver", "skip", false),
                ("pipelines.dlt_pipeline", "update", true),
            ])
        );
        assert_eq!(
            actions("plan-skip.json"),
            owned([
                ("jobs.gold_job", "skip", false),
                ("jobs.refresh_silver", "skip", false),
                ("pipelines.dlt_pipeline", "skip", false),
            ])
        );
        assert_eq!(
            actions("plan-delete.json"),
            owned([
                ("jobs.gold_job", "delete", false),
                ("jobs.refresh_silver", "skip", false),
                ("pipelines.dlt_pipeline", "skip", false),
            ])
        );
    }

    #[test]
    fn rejects_unexpected_plan_shapes() {
        for (text, reason) in [
            ("{}", "no `plan` object"),
            (r#"{"plan": []}"#, "no `plan` object"),
            (r#"{"plan": {"jobs.a": {"action": "create"}}}"#, "not named"),
            (
                r#"{"plan": {"resources.jobs": {"action": "create"}}}"#,
                "not named",
            ),
            (
                r#"{"plan": {"resources.jobs.a": {}}}"#,
                "no string `action`",
            ),
            (r#"{"plan": {"resources.jobs.a": 1}}"#, "no string `action`"),
        ] {
            let plan = json::parse(text).expect("parse");
            let error = read(plan.root()).unwrap_err();
            assert!(error.contains(reason), "{text}: {error}");
        }
    }

    #[test]
    fn config_must_be_an_object() {
        let plan = json::parse(
            r#"{"plan_version": "2", "plan": {"resources.jobs.a": {"action": "create", "new_state": {"value": 3}}}}"#,
        )
        .expect("parse");
        let plan = read(plan.root()).expect("read");
        assert_eq!(plan.version, None);
        assert!(plan.entries[0].config.is_none());
    }
}

//! Deployment state written by the `direct` engine (#32).
//!
//! On CLI 1.13.0 `bundle deploy` writes `resources.json` to
//! `workspace.state_path` and keeps a copy in the bundle root at
//! `.databricks/bundle/<target>/resources.json`. It is an object with
//! `state_version` (2), `cli_version`, `lineage`, `serial`, and `state`,
//! which maps each resource, named `resources.<type>.<key>`, to its `__id__`
//! and the deployed configuration in `state`. A pipeline's ID is therefore at
//! `/state/resources.pipelines.<key>/__id__`. The `terraform` engine keeps
//! `terraform/terraform.tfstate` there instead.

use std::path::{Component, Path, PathBuf};

use crate::json::Node;

/// The local copy of the state for `target`, or `None` if `target` is not a
/// plain directory name.
pub(crate) fn local_state(bundle_root: &Path, target: &str) -> Option<PathBuf> {
    target_dir(bundle_root, target).map(|dir| dir.join("resources.json"))
}

/// The local Terraform state for `target`, or `None` if `target` is not a
/// plain directory name.
pub(crate) fn terraform_state(bundle_root: &Path, target: &str) -> Option<PathBuf> {
    target_dir(bundle_root, target).map(|dir| dir.join("terraform").join("terraform.tfstate"))
}

fn target_dir(bundle_root: &Path, target: &str) -> Option<PathBuf> {
    let mut components = Path::new(target).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) if name == target => {
            Some(bundle_root.join(".databricks").join("bundle").join(name))
        }
        _ => None,
    }
}

/// The type and key of a resource named `resources.<type>.<key>`, as plan
/// and state entries are, such as `jobs` and `refresh_silver`.
pub(crate) fn resource_name(name: &str) -> Option<(&str, &str)> {
    let (kind, key) = name.strip_prefix("resources.")?.split_once('.')?;
    (!kind.is_empty() && !key.is_empty()).then_some((kind, key))
}

/// The JSON Pointer to the ID of resource `key` of type `kind`.
pub(crate) fn id_pointer(kind: &str, key: &str) -> String {
    let escape = |text: &str| text.replace('~', "~0").replace('/', "~1");
    format!("/state/resources.{}.{}/__id__", escape(kind), escape(key))
}

/// One deployed resource.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Entry<'a> {
    pub(crate) kind: &'a str,
    pub(crate) key: &'a str,
    /// The string at the entry's `__id__`, if any.
    pub(crate) id: Option<&'a str>,
    /// Whether the entry keeps its configuration in a `state` object.
    pub(crate) has_config: bool,
}

/// A parsed state file.
#[derive(Debug)]
pub(crate) struct State<'a> {
    /// `state_version`, if it is a number.
    pub(crate) version: Option<i64>,
    /// The entries, sorted by resource name.
    pub(crate) entries: Vec<Entry<'a>>,
}

/// `state` read as a `direct`-engine state file, or why it cannot be: no
/// `state` object, or an entry that is not an object named
/// `resources.<type>.<key>`.
pub(crate) fn read(state: Node<'_>) -> Result<State<'_>, String> {
    let members = state
        .get("state")
        .and_then(Node::members)
        .ok_or("the state has no `state` object")?;
    let mut entries = Vec::new();
    for (name, entry) in members {
        let (kind, key) =
            resource_name(name).ok_or("a state entry is not named `resources.<type>.<key>`")?;
        if entry.members().is_none() {
            return Err("a state entry is not an object".to_owned());
        }
        entries.push(Entry {
            kind,
            key,
            id: state.pointer(&id_pointer(kind, key)).and_then(Node::as_str),
            has_config: entry.get("state").and_then(Node::members).is_some(),
        });
    }
    Ok(State {
        version: state.get("state_version").and_then(Node::as_i64),
        entries,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Entry, id_pointer, local_state, read, resource_name, terraform_state};
    use crate::json;

    #[test]
    fn reads_the_state_fixture() {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/cli-1.13.0/resources.json"
        ))
        .expect("read fixture");
        let document = json::parse(&text).expect("parse fixture");
        let state = read(document.root()).expect("read state");
        assert_eq!(state.version, Some(2));
        let entry = |kind, key, id| Entry {
            kind,
            key,
            id: Some(id),
            has_config: true,
        };
        assert_eq!(
            state.entries,
            [
                entry("jobs", "gold_job", "124"),
                entry("jobs", "refresh_silver", "123"),
                entry("pipelines", "dlt_pipeline", "01a23b45c67d8901"),
            ]
        );
        assert_eq!(
            document
                .root()
                .pointer("/state/resources.pipelines.dlt_pipeline/__id__")
                .and_then(json::Node::as_str),
            Some("01a23b45c67d8901")
        );
    }

    #[test]
    fn rejects_unexpected_state_shapes() {
        for (text, reason) in [
            ("{}", "no `state` object"),
            (r#"{"state": 1}"#, "no `state` object"),
            (r#"{"state": {"jobs.a": {}}}"#, "not named"),
            (r#"{"state": {"resources.jobs.a": []}}"#, "not an object"),
        ] {
            let document = json::parse(text).expect("parse");
            let error = read(document.root()).unwrap_err();
            assert!(error.contains(reason), "{text}: {error}");
        }
        let document = json::parse(
            r#"{"state_version": "2", "state": {"resources.jobs.a": {"__id__": 5, "state": []}}}"#,
        )
        .expect("parse");
        let state = read(document.root()).expect("read");
        assert_eq!(state.version, None);
        assert_eq!(
            state.entries,
            [Entry {
                kind: "jobs",
                key: "a",
                id: None,
                has_config: false
            }]
        );
    }

    #[test]
    fn names_and_pointers() {
        assert_eq!(resource_name("resources.jobs.a.b"), Some(("jobs", "a.b")));
        for name in [
            "jobs.a",
            "resources.jobs",
            "resources..a",
            "resources.jobs.",
        ] {
            assert_eq!(resource_name(name), None, "{name}");
        }
        assert_eq!(
            id_pointer("jobs", "a/b~c"),
            "/state/resources.jobs.a~1b~0c/__id__"
        );
    }

    #[test]
    fn state_paths_stay_in_the_bundle() {
        let root = Path::new("bundle");
        assert_eq!(
            local_state(root, "dev"),
            Some(root.join(".databricks/bundle/dev/resources.json"))
        );
        assert_eq!(
            terraform_state(root, "dev"),
            Some(root.join(".databricks/bundle/dev/terraform/terraform.tfstate"))
        );
        for target in ["", "..", ".", "a/b", "/abs"] {
            assert_eq!(local_state(root, target), None, "{target}");
        }
    }
}

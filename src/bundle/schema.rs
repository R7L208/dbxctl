//! The bundle configuration schema the CLI ships, from `databricks bundle
//! schema` (#34).
//!
//! `$defs` nests definitions by Go import path, one object per path segment,
//! so the SDK's `jobs.TriggerSettings` lives at
//! `$defs/github.com/databricks/databricks-sdk-go/service/jobs.TriggerSettings`.
//! A path segment holds only objects, so an object is a definition if it is
//! empty, has a member that is not an object, or is the target of a `$ref`.
//! Any other object is a path segment.

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use crate::json::{Kind, Node};

/// The CLI arguments that print the schema. It needs no bundle or workspace.
pub(crate) const SCHEMA_COMMAND: [&str; 2] = ["bundle", "schema"];
/// The schema is about 1 MB, so it gets more time than `version`.
pub(crate) const SCHEMA_TIMEOUT: Duration = Duration::from_secs(60);

/// Starts a `$ref` to a definition in `$defs`.
const DEFS_REF: &str = "#/$defs/";
/// Subschemas whose `properties` count as the definition's own, at any depth.
const BRANCHES: [&str; 3] = ["allOf", "anyOf", "oneOf"];

/// The import path of every Go package with a definition that has a property
/// named `property`, such as `github.com/databricks/databricks-sdk-go/service/jobs`
/// for `jobs.TriggerSettings` at that path, or `None` when `schema` has no
/// `$defs` object.
pub(crate) fn packages_with_property(schema: Node<'_>, property: &str) -> Option<BTreeSet<String>> {
    let defs = schema
        .get("$defs")
        .filter(|defs| defs.kind() == Kind::Object)?;
    let mut targets = HashSet::new();
    ref_targets(schema, &mut targets);
    let mut found = BTreeSet::new();
    collect(
        schema,
        defs,
        &mut Vec::new(),
        &targets,
        property,
        &mut found,
    );
    Some(found)
}

/// Adds the `$defs` pointer of every `$ref` in `node` to `targets`, without
/// the `#/$defs/` prefix.
fn ref_targets<'a>(node: Node<'a>, targets: &mut HashSet<&'a str>) {
    if let Some(members) = node.members() {
        for (key, child) in members {
            match child.as_str().and_then(|text| text.strip_prefix(DEFS_REF)) {
                Some(target) if key == "$ref" => {
                    targets.insert(target);
                }
                _ => ref_targets(child, targets),
            }
        }
    } else if let Some(elements) = node.elements() {
        for element in elements {
            ref_targets(element, targets);
        }
    }
}

fn collect<'a>(
    root: Node<'a>,
    node: Node<'a>,
    path: &mut Vec<&'a str>,
    targets: &HashSet<&str>,
    property: &str,
    found: &mut BTreeSet<String>,
) {
    let Some(members) = node.members() else {
        return;
    };
    for (key, child) in members {
        if child.kind() != Kind::Object {
            continue;
        }
        path.push(key);
        if is_definition(child, path, targets) {
            if has_property(root, child, property, &mut HashSet::new()) {
                found.insert(package(path));
            }
        } else {
            collect(root, child, path, targets, property, found);
        }
        path.pop();
    }
}

fn is_definition(node: Node<'_>, path: &[&str], targets: &HashSet<&str>) -> bool {
    let mut members = node.members().into_iter().flatten().peekable();
    members.peek().is_none()
        || members.any(|(_, child)| child.kind() != Kind::Object)
        || targets.contains(pointer(path).as_str())
}

/// The JSON Pointer to `path` below `$defs`, with `~` and `/` escaped.
fn pointer(path: &[&str]) -> String {
    path.iter()
        .map(|segment| segment.replace('~', "~0").replace('/', "~1"))
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether `schema` declares `property` in its own `properties`, in a branch
/// at any depth, or in a schema it refers to with a `$ref` into `root`.
/// `followed` holds the refs already followed, so a cycle ends.
fn has_property<'a>(
    root: Node<'a>,
    schema: Node<'a>,
    property: &str,
    followed: &mut HashSet<&'a str>,
) -> bool {
    if schema
        .get("properties")
        .is_some_and(|properties| properties.get(property).is_some())
    {
        return true;
    }
    let target = schema
        .get("$ref")
        .and_then(Node::as_str)
        .filter(|target| followed.insert(target))
        .and_then(|target| root.pointer(target.strip_prefix('#')?));
    if target.is_some_and(|target| has_property(root, target, property, followed)) {
        return true;
    }
    BRANCHES.iter().any(|branch| {
        schema
            .get(branch)
            .and_then(Node::elements)
            .is_some_and(|mut schemas| {
                schemas.any(|schema| has_property(root, schema, property, followed))
            })
    })
}

/// The segments above the definition joined with the text before the first
/// `.` of its name: `…/service/jobs` for `…/service/jobs.TriggerSettings`. A
/// name without a package is its own.
fn package(path: &[&str]) -> String {
    let Some((name, parents)) = path.split_last() else {
        return String::new();
    };
    let own = name.split_once('.').map_or(*name, |(package, _)| package);
    parents
        .iter()
        .copied()
        .chain([own])
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::packages_with_property;
    use crate::json;

    fn found(schema: &str) -> Option<Vec<String>> {
        let document = json::parse(schema).expect("valid schema");
        packages_with_property(document.root(), "table_update")
            .map(|packages| packages.into_iter().collect())
    }

    const SDK: &str = "github.com/databricks/databricks-sdk-go/service";

    #[test]
    fn finds_properties_in_nested_definitions_and_branches() {
        let schema = r##"{"$defs": {
            "bool": {"oneOf": [{"type": "boolean"}]},
            "github.com": {"databricks": {"databricks-sdk-go": {"service": {
                "jobs.TriggerSettings": {"oneOf": [
                    {"type": "object", "properties": {"table_update": {"$ref": "#/x"}}},
                    {"type": "string"}
                ]},
                "compute.Direct": {"type": "object", "properties": {"table_update": {}}},
                "jobs.Other": {"type": "object", "properties": {"periodic": {}}},
                "pipelines.PipelineTrigger": {"allOf": [{"properties": {"table_update": {}}}]}
            }}}}
        }}"##;
        assert_eq!(
            found(schema),
            Some(vec![
                format!("{SDK}/compute"),
                format!("{SDK}/jobs"),
                format!("{SDK}/pipelines"),
            ])
        );
    }

    #[test]
    fn ignores_properties_of_properties() {
        // `table_update` here describes a nested object, not the definition.
        let schema = r#"{"$defs": {"x": {"jobs.T": {"type": "object", "properties": {
            "settings": {"type": "object", "properties": {"table_update": {}}}
        }}}}}"#;
        assert_eq!(found(schema), Some(Vec::new()));
    }

    #[test]
    fn keeps_the_import_path_of_same_named_packages() {
        let schema = r#"{"$defs": {"github.com": {"databricks": {"cli": {"libs": {
            "jobs.Settings": {"type": "object", "properties": {"table_update": {}}}
        }}}}}}"#;
        assert_eq!(
            found(schema),
            Some(vec!["github.com/databricks/cli/libs/jobs".to_owned()])
        );
    }

    #[test]
    fn a_definition_without_a_package_is_its_own() {
        let schema =
            r#"{"$defs": {"Trigger": {"type": "object", "properties": {"table_update": {}}}}}"#;
        assert_eq!(found(schema), Some(vec!["Trigger".to_owned()]));
    }

    #[test]
    fn segments_may_share_names_with_keywords() {
        // `items` and `type` are directories here, not schema keywords.
        let schema = r#"{"$defs": {"libs": {"items": {"type": {
            "jobs.T": {"type": "object", "properties": {"table_update": {}}}
        }}}}}"#;
        assert_eq!(found(schema), Some(vec!["libs/items/type/jobs".to_owned()]));
    }

    #[test]
    fn definitions_need_no_listed_keyword() {
        // `required` is not an object, so this is a definition, and its `not`
        // subschema is not walked as a path segment.
        let schema = r#"{"$defs": {"x": {
            "jobs.T": {"required": ["a"], "not": {"properties": {"table_update": {}}}},
            "pipelines.U": {"title": "U", "properties": {"table_update": {}}}
        }}}"#;
        assert_eq!(found(schema), Some(vec!["x/pipelines".to_owned()]));
    }

    #[test]
    fn a_ref_target_is_a_definition() {
        // Only objects inside, but a `$ref` names it, so it is not a segment.
        let schema = r##"{"properties": {"t": {"$ref": "#/$defs/x/jobs.T"}},
            "$defs": {"x": {"jobs.T": {"properties": {"table_update": {}}}}}}"##;
        assert_eq!(found(schema), Some(vec!["x/jobs".to_owned()]));
    }

    #[test]
    fn follows_refs_and_nested_branches() {
        let schema = r##"{"$defs": {"x": {
            "nested.T": {"allOf": [{"oneOf": [{"properties": {"table_update": {}}}]}]},
            "alias.T": {"$ref": "#/$defs/x/shared.T"},
            "shared.T": {"type": "object", "properties": {"table_update": {}}},
            "cycle.T": {"oneOf": [{"$ref": "#/$defs/x/loop.T"}]},
            "loop.T": {"oneOf": [{"$ref": "#/$defs/x/cycle.T"}]},
            "dangling.T": {"$ref": "#/$defs/x/missing"}
        }}}"##;
        assert_eq!(
            found(schema),
            Some(vec![
                "x/alias".to_owned(),
                "x/nested".to_owned(),
                "x/shared".to_owned(),
            ])
        );
    }

    #[test]
    fn needs_a_defs_object() {
        assert_eq!(found("{}"), None);
        assert_eq!(found(r#"{"$defs": []}"#), None);
        assert_eq!(found(r#"{"$defs": {"x": 1, "y": [{}]}}"#), Some(Vec::new()));
    }
}

//! The bundle configuration schema the CLI ships, from `databricks bundle
//! schema` (#34).
//!
//! `$defs` nests definitions by Go import path, one object per path segment,
//! so the SDK's `jobs.TriggerSettings` lives at
//! `$defs/github.com/databricks/databricks-sdk-go/service/jobs.TriggerSettings`.
//! An object with a JSON Schema keyword is a definition; any other object is a
//! path segment.

use std::time::Duration;

use crate::json::{Kind, Node};

/// The CLI arguments that print the schema. It needs no bundle or workspace.
pub(crate) const SCHEMA_COMMAND: [&str; 2] = ["bundle", "schema"];
/// The schema is about 1 MB, so it gets more time than `version`.
pub(crate) const SCHEMA_TIMEOUT: Duration = Duration::from_secs(60);

/// Keys that only appear in a definition, never in a path segment.
const KEYWORDS: [&str; 11] = [
    "$ref",
    "additionalProperties",
    "allOf",
    "anyOf",
    "const",
    "description",
    "enum",
    "items",
    "oneOf",
    "properties",
    "type",
];
/// Subschemas whose `properties` count as the definition's own.
const BRANCHES: [&str; 3] = ["allOf", "anyOf", "oneOf"];

/// A definition in `$defs`.
#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Definition {
    /// The slash-separated path below `$defs`.
    pub(crate) path: String,
    /// The import path of the Go package that declares it:
    /// `github.com/databricks/databricks-sdk-go/service/jobs` for
    /// `jobs.TriggerSettings` at that path.
    pub(crate) package: String,
}

/// Every definition with a property named `property`, sorted by path, or
/// `None` when `schema` has no `$defs` object.
pub(crate) fn definitions_with_property(
    schema: Node<'_>,
    property: &str,
) -> Option<Vec<Definition>> {
    let defs = schema
        .get("$defs")
        .filter(|defs| defs.kind() == Kind::Object)?;
    let mut found = Vec::new();
    collect(defs, &mut Vec::new(), property, &mut found);
    found.sort();
    Some(found)
}

fn collect<'a>(
    node: Node<'a>,
    path: &mut Vec<&'a str>,
    property: &str,
    found: &mut Vec<Definition>,
) {
    let Some(members) = node.members() else {
        return;
    };
    for (key, child) in members {
        if child.kind() != Kind::Object {
            continue;
        }
        path.push(key);
        if is_definition(child) {
            if has_property(child, property) {
                found.push(Definition {
                    path: path.join("/"),
                    package: package(path),
                });
            }
        } else {
            collect(child, path, property, found);
        }
        path.pop();
    }
}

fn is_definition(node: Node<'_>) -> bool {
    KEYWORDS.iter().any(|keyword| node.get(keyword).is_some())
}

fn has_property(definition: Node<'_>, property: &str) -> bool {
    let own = |schema: Node<'_>| {
        schema
            .get("properties")
            .is_some_and(|properties| properties.get(property).is_some())
    };
    own(definition)
        || BRANCHES.iter().any(|branch| {
            definition
                .get(branch)
                .and_then(Node::elements)
                .is_some_and(|mut schemas| schemas.any(own))
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
    use super::{Definition, definitions_with_property};
    use crate::json;

    fn found(schema: &str) -> Option<Vec<(String, String)>> {
        let document = json::parse(schema).expect("valid schema");
        definitions_with_property(document.root(), "table_update").map(|definitions| {
            definitions
                .into_iter()
                .map(|Definition { path, package }| (path, package))
                .collect()
        })
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
                "jobs.Direct": {"type": "object", "properties": {"table_update": {}}},
                "jobs.Other": {"type": "object", "properties": {"periodic": {}}},
                "pipelines.PipelineTrigger": {"allOf": [{"properties": {"table_update": {}}}]}
            }}}}
        }}"##;
        assert_eq!(
            found(schema),
            Some(vec![
                (format!("{SDK}/jobs.Direct"), format!("{SDK}/jobs")),
                (format!("{SDK}/jobs.TriggerSettings"), format!("{SDK}/jobs")),
                (
                    format!("{SDK}/pipelines.PipelineTrigger"),
                    format!("{SDK}/pipelines")
                ),
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
            Some(vec![(
                "github.com/databricks/cli/libs/jobs.Settings".to_owned(),
                "github.com/databricks/cli/libs/jobs".to_owned()
            )])
        );
    }

    #[test]
    fn a_definition_without_a_package_is_its_own() {
        let schema = r#"{"$defs": {"Trigger": {"properties": {"table_update": {}}}}}"#;
        assert_eq!(
            found(schema),
            Some(vec![("Trigger".to_owned(), "Trigger".to_owned())])
        );
    }

    #[test]
    fn needs_a_defs_object() {
        assert_eq!(found("{}"), None);
        assert_eq!(found(r#"{"$defs": []}"#), None);
        assert_eq!(found(r#"{"$defs": {"x": 1, "y": [{}]}}"#), Some(Vec::new()));
    }
}

//! The bundle configuration schema the CLI ships, from `databricks bundle
//! schema` (#34).
//!
//! `$defs` nests definitions by Go import path, one object per path segment,
//! so the SDK's `jobs.TriggerSettings` lives at
//! `$defs/github.com/databricks/databricks-sdk-go/service/jobs.TriggerSettings`.
//! A path segment holds only objects, so an object is a definition if it is
//! empty, has a member that is not an object, is the target of a `$ref`, or
//! reads as a schema: every object member is a schema keyword, such as
//! `properties` or `items`, holding what that keyword holds. Any other object
//! is a path segment. A segment that happens to read as a schema, such as one
//! named `properties` holding only definitions, is taken for a definition.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use crate::json::{Kind, Node, unescape_token};

/// The CLI arguments that print the schema. It needs no bundle or workspace.
pub(crate) const SCHEMA_COMMAND: [&str; 2] = ["bundle", "schema"];
/// The schema is about 1 MB, so it gets more time than `version`.
pub(crate) const SCHEMA_TIMEOUT: Duration = Duration::from_secs(60);

/// Starts the decoded pointer of a `$ref` to a definition in `$defs`.
const DEFS_POINTER: &str = "/$defs/";
/// Subschemas whose `properties` count as the definition's own, at any depth.
const BRANCHES: [&str; 3] = ["allOf", "anyOf", "oneOf"];
/// Keywords whose object value is one schema.
const SCHEMA_KEYWORDS: [&str; 12] = [
    "additionalItems",
    "additionalProperties",
    "contains",
    "contentSchema",
    "else",
    "if",
    "items",
    "not",
    "propertyNames",
    "then",
    "unevaluatedItems",
    "unevaluatedProperties",
];
/// Keywords whose object value maps names to schemas.
const MAP_KEYWORDS: [&str; 5] = [
    "$defs",
    "definitions",
    "dependentSchemas",
    "patternProperties",
    "properties",
];
/// Keywords whose object value may be any JSON.
const VALUE_KEYWORDS: [&str; 2] = ["const", "default"];

/// The import path of every Go package with a definition that has a property
/// named `property`, such as `github.com/databricks/databricks-sdk-go/service/jobs`
/// for `jobs.TriggerSettings` at that path, or `None` when `schema` has no
/// `$defs` object.
pub(crate) fn packages_with_property(schema: Node<'_>, property: &str) -> Option<BTreeSet<String>> {
    search(schema, property).map(|(found, _)| found)
}

/// [`packages_with_property`], and how many times a `$ref` target was walked.
fn search(schema: Node<'_>, property: &str) -> Option<(BTreeSet<String>, usize)> {
    let defs = schema
        .get("$defs")
        .filter(|defs| defs.kind() == Kind::Object)?;
    let mut targets = Targets::default();
    ref_targets(schema, &mut targets);
    let mut search = Search {
        root: schema,
        property,
        known: HashMap::new(),
        seen: HashSet::new(),
        walked: 0,
    };
    let mut found = BTreeSet::new();
    collect(
        defs,
        Some(&targets),
        &mut Vec::new(),
        &mut search,
        &mut found,
    );
    Some((found, search.walked))
}

/// The decoded `$defs` pointers that `$ref`s name, as a tree of path
/// segments, so the walk can tell a ref target without building its pointer.
#[derive(Default)]
struct Targets {
    /// Whether a `$ref` names the path that ends here.
    named: bool,
    below: HashMap<String, Targets>,
}

impl Targets {
    fn insert(&mut self, pointer: &str) {
        let mut node = self;
        for token in pointer.split('/') {
            let Some(segment) = unescape_token(token) else {
                return;
            };
            node = node.below.entry(segment).or_default();
        }
        node.named = true;
    }
}

/// Adds the decoded `$defs` pointer of every `$ref` in `node` to `targets`,
/// without the `/$defs/` prefix.
fn ref_targets(node: Node<'_>, targets: &mut Targets) {
    if let Some(members) = node.members() {
        for (key, child) in members {
            let pointer = child
                .as_str()
                .filter(|_| key == "$ref")
                .and_then(ref_pointer);
            match pointer
                .as_deref()
                .and_then(|pointer| pointer.strip_prefix(DEFS_POINTER))
            {
                Some(target) => targets.insert(target),
                None => ref_targets(child, targets),
            }
        }
    } else if let Some(elements) = node.elements() {
        for element in elements {
            ref_targets(element, targets);
        }
    }
}

fn collect<'a>(
    node: Node<'a>,
    targets: Option<&Targets>,
    path: &mut Vec<&'a str>,
    search: &mut Search<'a, '_>,
    found: &mut BTreeSet<String>,
) {
    let Some(members) = node.members() else {
        return;
    };
    for (key, child) in members {
        if child.kind() != Kind::Object {
            continue;
        }
        let below = targets.and_then(|targets| targets.below.get(key));
        path.push(key);
        if is_definition(child, below.is_some_and(|below| below.named)) {
            if search.declares(child) {
                found.insert(package(path));
            }
        } else {
            collect(child, below, path, search, found);
        }
        path.pop();
    }
}

/// `named` says whether a `$ref` names `node`.
fn is_definition(node: Node<'_>, named: bool) -> bool {
    let mut members = node.members().into_iter().flatten().peekable();
    named
        || members.peek().is_none()
        || members.any(|(_, child)| child.kind() != Kind::Object)
        || is_schema(node)
}

/// Whether `node` reads as a schema: a boolean, or an object whose object
/// members are all keywords holding what that keyword holds. Members that are
/// not objects are annotations or constraints, which a segment never has.
fn is_schema(node: Node<'_>) -> bool {
    if node.kind() == Kind::Bool {
        return true;
    }
    let Some(mut members) = node.members() else {
        return false;
    };
    members.all(|(key, child)| {
        child.kind() != Kind::Object
            || VALUE_KEYWORDS.contains(&key)
            || (SCHEMA_KEYWORDS.contains(&key) && is_schema(child))
            || (MAP_KEYWORDS.contains(&key)
                && child
                    .members()
                    .is_some_and(|mut schemas| schemas.all(|(_, schema)| is_schema(schema))))
    })
}

/// The search for a property, which remembers what each `$ref` leads to so
/// a ref that many definitions share is walked once.
struct Search<'a, 'p> {
    root: Node<'a>,
    property: &'p str,
    /// Whether the schema at each pointer declares the property, once known.
    known: HashMap<String, bool>,
    /// The pointers walked in the current definition's search.
    seen: HashSet<String>,
    /// How many times a `$ref` target was walked, over the whole search.
    walked: usize,
}

impl<'a> Search<'a, '_> {
    /// Whether the definition `schema` declares the property. Every ref is
    /// walked at most once per definition. A ref seen again adds nothing: if
    /// anything it leads to declares the property, the first walk finds it.
    /// So a "no" holds for every ref walked, and is remembered for them all.
    fn declares(&mut self, schema: Node<'a>) -> bool {
        self.seen.clear();
        let found = self.has_property(schema);
        if !found {
            for pointer in self.seen.drain() {
                self.known.insert(pointer, false);
            }
        }
        found
    }

    /// Whether `schema` declares the property in its own `properties`, in a
    /// branch at any depth, or in a schema it names with a `$ref`.
    fn has_property(&mut self, schema: Node<'a>) -> bool {
        if schema
            .get("properties")
            .is_some_and(|properties| properties.get(self.property).is_some())
        {
            return true;
        }
        let pointer = schema
            .get("$ref")
            .and_then(Node::as_str)
            .and_then(ref_pointer);
        if pointer.is_some_and(|pointer| self.follow(&pointer)) {
            return true;
        }
        BRANCHES.iter().any(|branch| {
            schema
                .get(branch)
                .and_then(Node::elements)
                .into_iter()
                .flatten()
                .any(|schema| self.has_property(schema))
        })
    }

    /// [`Self::has_property`] for the schema at `pointer`. A "yes" holds
    /// wherever the ref is reached, so it is remembered at once.
    fn follow(&mut self, pointer: &str) -> bool {
        if let Some(&found) = self.known.get(pointer) {
            return found;
        }
        if !self.seen.insert(pointer.to_owned()) {
            return false;
        }
        let Some(schema) = self.root.pointer(pointer) else {
            return false;
        };
        self.walked += 1;
        let found = self.has_property(schema);
        if found {
            self.known.insert(pointer.to_owned(), true);
        }
        found
    }
}

/// The JSON Pointer in a `$ref` to this document: its fragment after `#`,
/// percent-decoded as RFC 6901 section 6 requires, or `None` for a ref to
/// another document or a bad escape.
fn ref_pointer(reference: &str) -> Option<Cow<'_, str>> {
    let fragment = reference.strip_prefix('#')?;
    if !fragment.contains('%') {
        return Some(Cow::Borrowed(fragment));
    }
    let bytes = fragment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let digit = |byte: u8| char::from(byte).to_digit(16);
            let (high, low) = (digit(hex[0])?, digit(hex[1])?);
            decoded.push(u8::try_from(high * 16 + low).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok().map(Cow::Owned)
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
    use super::{packages_with_property, search};
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
    fn a_definition_of_keywords_alone_is_not_a_segment() {
        // Only object members and no `$ref` to it, but each member is a
        // keyword holding schemas, so this is a definition, not a segment.
        let schema = r#"{"$defs": {"x": {
            "jobs.T": {"properties": {"table_update": {}}},
            "pipelines.U": {"properties": {
                "table_update": {"items": {}},
                "settings": {"type": "object", "properties": {"table_update": {}}}
            }},
            "compute.V": {"properties": {
                "settings": {"type": "object", "properties": {"table_update": {}}}
            }}
        }}}"#;
        assert_eq!(
            found(schema),
            Some(vec!["x/jobs".to_owned(), "x/pipelines".to_owned()])
        );
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
    fn a_ref_seen_inside_a_cycle_is_not_remembered_as_no() {
        // Following `d.T` reaches `q.T` while `p.T` is still open, so `q.T`
        // looks like a "no" there. `p.T` then finds the property, and `e.T`,
        // which reaches it only through `q.T`, must find it too.
        let schema = r##"{"$defs": {"x": {
            "d.T": {"$ref": "#/$defs/x/p.T"},
            "e.T": {"$ref": "#/$defs/x/q.T"},
            "p.T": {"oneOf": [{"$ref": "#/$defs/x/q.T"}, {"properties": {"table_update": {}}}]},
            "q.T": {"$ref": "#/$defs/x/p.T"}
        }}}"##;
        assert_eq!(
            found(schema),
            Some(vec![
                "x/d".to_owned(),
                "x/e".to_owned(),
                "x/p".to_owned(),
                "x/q".to_owned(),
            ])
        );
    }

    #[test]
    fn walks_each_ref_in_a_dense_cycle_once() {
        // Every definition refs every other and none declares the property,
        // so a walk that forgot each "no" would visit every path: about 12!.
        const COUNT: usize = 12;
        let definitions: Vec<String> = (0..COUNT)
            .map(|from| {
                let refs: Vec<String> = (0..COUNT)
                    .filter(|&to| to != from)
                    .map(|to| format!(r##"{{"$ref": "#/$defs/x/d{to}.T"}}"##))
                    .collect();
                format!(r#""d{from}.T": {{"oneOf": [{}]}}"#, refs.join(", "))
            })
            .collect();
        let schema = format!(r#"{{"$defs": {{"x": {{{}}}}}}}"#, definitions.join(", "));
        let document = json::parse(&schema).expect("valid schema");
        let (found, walked) = search(document.root(), "table_update").expect("has $defs");
        assert!(found.is_empty(), "{found:?}");
        assert!(
            walked <= COUNT,
            "walked {walked} refs for {COUNT} definitions"
        );
    }

    #[test]
    fn decodes_percent_encoded_refs() {
        // `%20%7B%7D` is ` {}` and `%24` is `$`; the bad escape is ignored.
        // Only its decoded `$ref` makes `shared {}` a definition, and only
        // following that ref finds the property for `jobs.T`.
        let schema = r##"{"properties": {
            "a": {"$ref": "#/%24defs/x/jobs.T"},
            "b": {"$ref": "#/$defs/x/bad%2"}
        }, "$defs": {"x": {
            "jobs.T": {"properties": {"settings": {}}, "allOf": [{"$ref": "#/$defs/x/shared%20%7B%7D"}]},
            "shared {}": {"x-meta": {}, "properties": {"table_update": {}}}
        }}}"##;
        assert_eq!(
            found(schema),
            Some(vec!["x/jobs".to_owned(), "x/shared {}".to_owned()])
        );
    }

    #[test]
    fn needs_a_defs_object() {
        assert_eq!(found("{}"), None);
        assert_eq!(found(r#"{"$defs": []}"#), None);
        assert_eq!(found(r#"{"$defs": {"x": 1, "y": [{}]}}"#), Some(Vec::new()));
    }
}

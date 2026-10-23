//! Validated bundle configuration, from `databricks bundle validate -o json`
//! (#32).
//!
//! Validation prints the bundle's configuration for one target with
//! variables resolved and source paths rewritten to workspace paths. On CLI
//! 1.13.0 it prints that JSON even when it exits 1, and it contacts the
//! workspace: it reads the current user and checks, and if need be creates,
//! the directory at `workspace.file_path`.
//!
//! Every reader returns `None` or an error for a missing or unexpected
//! field, so a caller can make it an explicit unknown rather than an empty
//! result.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::json::{Kind, Node};

/// The CLI arguments that validate the bundle and print its configuration.
pub(crate) const VALIDATE_COMMAND: [&str; 4] = ["bundle", "validate", "-o", "json"];
/// Adds `__locations`: the configuration files, and where in them each
/// resource is defined. Accepted by CLI 1.13.0, though not in its help.
pub(crate) const LOCATIONS_FLAG: &str = "--include-locations";
/// Validation and planning contact the workspace, so they get more time than
/// an offline command.
pub(crate) const BUNDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// The declared resources: each resource type, such as `jobs`, and its
/// resources by key. Both levels iterate in sorted order.
pub(crate) type Resources<'a> = BTreeMap<&'a str, BTreeMap<&'a str, Node<'a>>>;

/// The resources in `config`, or why they cannot be read: `resources` is
/// missing or is not an object of objects of objects.
pub(crate) fn resources(config: Node<'_>) -> Result<Resources<'_>, String> {
    let resources = config
        .get("resources")
        .ok_or("the configuration has no `resources` object")?;
    let types = resources
        .members()
        .ok_or("the configuration's `resources` is not an object")?;
    let mut found = Resources::new();
    for (kind, entries) in types {
        let entries = entries
            .members()
            .ok_or_else(|| format!("`resources.{kind}` is not an object"))?;
        let mut by_key = BTreeMap::new();
        for (key, resource) in entries {
            if resource.kind() != Kind::Object {
                return Err(format!("`resources.{kind}.{key}` is not an object"));
            }
            by_key.insert(key, resource);
        }
        found.insert(kind, by_key);
    }
    Ok(found)
}

/// The deployment engine in `bundle.engine`, if set.
pub(crate) fn engine(config: Node<'_>) -> Option<&str> {
    config.get("bundle")?.get("engine")?.as_str()
}

/// The names of the string-valued `workspace` fields ending in `_path`,
/// such as `file_path` and `state_path`, or `None` without a `workspace`
/// object.
pub(crate) fn workspace_paths(config: Node<'_>) -> Option<Vec<&str>> {
    let members = config.get("workspace")?.members()?;
    Some(
        members
            .filter(|(name, value)| name.ends_with("_path") && value.as_str().is_some())
            .map(|(name, _)| name)
            .collect(),
    )
}

/// The keys of the object at `pointer`, such as `/experimental/scripts`.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Keys<'a> {
    /// There is nothing at `pointer`.
    Absent,
    /// The object's keys, sorted.
    Present(Vec<&'a str>),
    /// Something other than an object is there.
    NotAnObject,
}

pub(crate) fn keys<'a>(config: Node<'a>, pointer: &str) -> Keys<'a> {
    match config.pointer(pointer) {
        None => Keys::Absent,
        Some(node) => node.members().map_or(Keys::NotAnObject, |members| {
            Keys::Present(members.map(|(key, _)| key).collect())
        }),
    }
}

/// One source path a resource names, such as a task's notebook.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct SourcePath<'a> {
    /// The resource, such as `jobs.refresh_silver`.
    pub(crate) resource: String,
    pub(crate) path: &'a str,
    /// Whether the path is relative to the job's Git repository.
    pub(crate) git: bool,
}

/// Source paths of a job task, by task type: the task field, the path in
/// it, and where it may name its `source`, `GIT` or `WORKSPACE`.
const TASK_PATHS: [(&str, &str, &str); 4] = [
    ("notebook_task", "/notebook_path", "/source"),
    ("spark_python_task", "/python_file", "/source"),
    ("sql_task", "/file/path", "/file/source"),
    ("dbt_task", "/project_directory", "/source"),
];
/// Source paths of a pipeline library, by library type.
const LIBRARY_PATHS: [&str; 3] = ["/file/path", "/notebook/path", "/glob/include"];
/// Single source paths of other resource types.
const RESOURCE_PATHS: [(&str, &str); 3] = [
    ("pipelines", "/root_path"),
    ("dashboards", "/file_path"),
    ("apps", "/source_code_path"),
];

/// Every source path the resources name, by resource and then in field
/// order: job task files and notebooks (including a `for_each_task`'s task),
/// pipeline libraries and `root_path`, dashboard files, and app sources.
pub(crate) fn source_paths<'a>(resources: &Resources<'a>) -> Vec<SourcePath<'a>> {
    let mut found = Vec::new();
    for (&kind, entries) in resources {
        for (&key, &resource) in entries {
            let resource_name = format!("{kind}.{key}");
            let mut push = |path: &'a str, git: bool| {
                found.push(SourcePath {
                    resource: resource_name.clone(),
                    path,
                    git,
                });
            };
            if kind == "jobs" {
                let git_source = resource.get("git_source").is_some();
                for task in elements(resource, "tasks") {
                    let nested = task.pointer("/for_each_task/task");
                    for task in [Some(task), nested].into_iter().flatten() {
                        for (field, pointer, source) in TASK_PATHS {
                            let Some(part) = task.get(field) else {
                                continue;
                            };
                            if let Some(path) = part.pointer(pointer).and_then(Node::as_str) {
                                push(path, is_git(part.pointer(source), git_source));
                            }
                        }
                    }
                }
            }
            if kind == "pipelines" {
                for library in elements(resource, "libraries") {
                    for pointer in LIBRARY_PATHS {
                        if let Some(path) = library.pointer(pointer).and_then(Node::as_str) {
                            push(path, false);
                        }
                    }
                }
            }
            for (path_kind, pointer) in RESOURCE_PATHS {
                if kind == path_kind
                    && let Some(path) = resource.pointer(pointer).and_then(Node::as_str)
                {
                    push(path, false);
                }
            }
        }
    }
    found
}

fn elements<'a>(node: Node<'a>, key: &str) -> impl Iterator<Item = Node<'a>> {
    node.get(key).and_then(Node::elements).into_iter().flatten()
}

/// A task's path is in Git when its `source` is `GIT`, or when it names no
/// source and the job has a `git_source`.
fn is_git(source: Option<Node<'_>>, git_source: bool) -> bool {
    match source.and_then(Node::as_str) {
        Some(source) => source == "GIT",
        None => git_source,
    }
}

/// Where each resource is defined, from `__locations`.
#[derive(Debug)]
pub(crate) struct Locations<'a> {
    files: Vec<&'a str>,
    locations: Node<'a>,
}

impl<'a> Locations<'a> {
    /// The `__locations` of `config`: its `files`, all strings, and its
    /// `locations` object. `None` if either is missing or malformed.
    pub(crate) fn read(config: Node<'a>) -> Option<Self> {
        let locations = config.get("__locations")?;
        let files = locations
            .get("files")?
            .elements()?
            .map(Node::as_str)
            .collect::<Option<Vec<_>>>()?;
        let locations = locations.get("locations")?;
        (locations.kind() == Kind::Object).then_some(Self { files, locations })
    }

    /// The file that defines `resource`, such as `jobs.refresh_silver`: the
    /// file of its first location.
    pub(crate) fn origin(&self, resource: &str) -> Option<&'a str> {
        let first = self
            .locations
            .get(&format!("resources.{resource}"))?
            .index(0)?;
        let file = usize::try_from(first.index(0)?.as_u64()?).ok()?;
        self.files.get(file).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Keys, Locations, SourcePath, engine, keys, resources, source_paths, workspace_paths,
    };
    use crate::json;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cli-1.13.0");

    fn fixture(name: &str) -> json::Document {
        let text = std::fs::read_to_string(format!("{FIXTURES}/{name}")).expect("read fixture");
        json::parse(&text).expect("parse fixture")
    }

    #[test]
    fn reads_the_validated_fixture() {
        let config = fixture("validate.json");
        let root = config.root();
        assert_eq!(engine(root), Some("direct"));
        assert_eq!(
            workspace_paths(root),
            Some(vec![
                "artifact_path",
                "file_path",
                "resource_path",
                "root_path",
                "state_path"
            ])
        );
        let resources = resources(root).expect("resources");
        let declared: Vec<_> = resources
            .iter()
            .map(|(kind, entries)| (*kind, entries.keys().copied().collect::<Vec<_>>()))
            .collect();
        assert_eq!(
            declared,
            [
                ("jobs", vec!["gold_job", "refresh_silver"]),
                ("pipelines", vec!["dlt_pipeline"])
            ]
        );
        assert_eq!(keys(root, "/experimental/scripts"), Keys::Absent);
        assert_eq!(
            keys(root, "/bundle"),
            Keys::Present(vec!["engine", "environment", "git", "name", "target"])
        );
        assert_eq!(keys(root, "/bundle/name"), Keys::NotAnObject);
    }

    #[test]
    fn rejects_unexpected_resource_shapes() {
        for (text, reason) in [
            ("{}", "no `resources` object"),
            (r#"{"resources": []}"#, "`resources` is not an object"),
            (
                r#"{"resources": {"jobs": 1}}"#,
                "`resources.jobs` is not an object",
            ),
            (
                r#"{"resources": {"jobs": {"a": "x"}}}"#,
                "`resources.jobs.a` is not an object",
            ),
        ] {
            let config = json::parse(text).expect("parse");
            let error = resources(config.root()).unwrap_err();
            assert!(error.contains(reason), "{text}: {error}");
        }
        let empty = json::parse(r#"{"resources": {}}"#).expect("parse");
        assert!(resources(empty.root()).expect("empty").is_empty());
        let none = json::parse(r#"{"bundle": 1, "workspace": []}"#).expect("parse");
        assert_eq!(engine(none.root()), None);
        assert_eq!(workspace_paths(none.root()), None);
    }

    #[test]
    fn lists_source_paths_with_their_forms_of_origin() {
        let config = fixture("validate.json");
        let resources = resources(config.root()).expect("resources");
        let found: Vec<_> = source_paths(&resources)
            .into_iter()
            .map(
                |SourcePath {
                     resource,
                     path,
                     git,
                 }| {
                    let file = path.rsplit_once("/files/").map_or(path, |(_, file)| file);
                    (resource, file.to_owned(), git)
                },
            )
            .collect();
        let expected = [
            ("jobs.gold_job", "notebooks/silver"),
            ("jobs.gold_job", "src/gold/g.sql"),
            ("jobs.refresh_silver", "notebooks/silver"),
            ("pipelines.dlt_pipeline", "src/silver.sql"),
            ("pipelines.dlt_pipeline", "src/q.sql"),
            ("pipelines.dlt_pipeline", "src/gold/g.sql"),
        ]
        .map(|(resource, file)| (resource.to_owned(), file.to_owned(), false));
        assert_eq!(found, expected);
    }

    #[test]
    fn marks_git_sources_and_reads_every_path_field() {
        let config = json::parse(
            r#"{"resources": {
                "apps": {"a": {"source_code_path": "/w/files/app"}},
                "dashboards": {"d": {"file_path": "/w/files/d.lvdash.json"}},
                "jobs": {
                    "git": {"git_source": {"git_url": "u"}, "tasks": [
                        {"notebook_task": {"notebook_path": "nb"}},
                        {"spark_python_task": {"python_file": "/w/files/p.py", "source": "WORKSPACE"}},
                        {"for_each_task": {"task": {"dbt_task": {"project_directory": "dbt", "source": "GIT"}}}}
                    ]},
                    "plain": {"tasks": [{"sql_task": {"file": {"path": "/w/files/q.sql"}}}, 7]}
                },
                "pipelines": {"p": {"root_path": "/w/files/p", "libraries": [
                    {"notebook": {"path": "/w/files/n"}}, {"glob": {"include": "/w/files/t/**"}}, {"jar": "x"}
                ]}}
            }}"#,
        )
        .expect("parse");
        let resources = resources(config.root()).expect("resources");
        let found: Vec<_> = source_paths(&resources)
            .into_iter()
            .map(|source| (source.resource, source.path, source.git))
            .collect();
        assert_eq!(
            found,
            [
                ("apps.a".to_owned(), "/w/files/app", false),
                ("dashboards.d".to_owned(), "/w/files/d.lvdash.json", false),
                ("jobs.git".to_owned(), "nb", true),
                ("jobs.git".to_owned(), "/w/files/p.py", false),
                ("jobs.git".to_owned(), "dbt", true),
                ("jobs.plain".to_owned(), "/w/files/q.sql", false),
                ("pipelines.p".to_owned(), "/w/files/n", false),
                ("pipelines.p".to_owned(), "/w/files/t/**", false),
                ("pipelines.p".to_owned(), "/w/files/p", false),
            ]
        );
    }

    #[test]
    fn reads_resource_origins() {
        let config = fixture("validate-locations.json");
        let locations = Locations::read(config.root()).expect("locations");
        assert_eq!(
            locations.origin("jobs.refresh_silver"),
            Some("databricks.yml")
        );
        assert_eq!(
            locations.origin("jobs.gold_job"),
            Some("resources/extra.yml")
        );
        assert_eq!(
            locations.origin("pipelines.dlt_pipeline"),
            Some("databricks.yml")
        );
        assert_eq!(locations.origin("jobs.missing"), None);

        for text in [
            "{}",
            r#"{"__locations": {"files": ["a"]}}"#,
            r#"{"__locations": {"files": [1], "locations": {}}}"#,
            r#"{"__locations": {"files": [], "locations": []}}"#,
        ] {
            let config = json::parse(text).expect("parse");
            assert!(Locations::read(config.root()).is_none(), "{text}");
        }
        let odd = json::parse(
            r#"{"__locations": {"files": ["a"], "locations": {"resources.jobs.x": [[5, 1, 1]], "resources.jobs.y": [["0"]], "resources.jobs.z": []}}}"#,
        )
        .expect("parse");
        let locations = Locations::read(odd.root()).expect("locations");
        for resource in ["jobs.x", "jobs.y", "jobs.z"] {
            assert_eq!(locations.origin(resource), None, "{resource}");
        }
    }
}

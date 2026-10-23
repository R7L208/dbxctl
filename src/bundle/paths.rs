//! Source path forms in validated bundle configuration, and their local files
//! (#32).
//!
//! `bundle validate` rewrites a relative source path, such as
//! `./notebooks/silver.py`, to an absolute workspace path below
//! `workspace.file_path`, where the CLI syncs the bundle's files. A notebook
//! loses its extension on the way; any other file keeps it. Paths the bundle
//! does not sync, such as a Unity Catalog volume, keep their own form.

use std::path::{Component, Path, PathBuf};

/// Extensions a synced notebook drops, in the order they are tried.
const NOTEBOOK_EXTENSIONS: [&str; 5] = ["py", "sql", "scala", "r", "ipynb"];

/// The form of one source path in validated configuration.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Form {
    /// Below `workspace.file_path`: a bundle file synced to the workspace.
    FilePath,
    /// Another absolute workspace path, outside the synced files.
    Workspace,
    /// A Unity Catalog volume path, `/Volumes/…`.
    Volume,
    /// A DBFS path, `dbfs:/…`.
    Dbfs,
    /// Relative to a Git repository named by the job's `git_source`.
    Git,
    /// A relative path that validation did not rewrite.
    Relative,
    /// Anything else, such as a URL.
    Other,
}

impl Form {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::FilePath => "file_path",
            Self::Workspace => "workspace",
            Self::Volume => "volume",
            Self::Dbfs => "dbfs",
            Self::Git => "git",
            Self::Relative => "relative",
            Self::Other => "other",
        }
    }
}

/// The form of `path`, and for a path below `file_path`, the part after it,
/// such as `notebooks/silver` for `<file_path>/notebooks/silver`.
pub(crate) fn form<'a>(path: &'a str, file_path: &str) -> (Form, Option<&'a str>) {
    let file_path = file_path.trim_end_matches('/');
    if let Some(relative) = path
        .strip_prefix(file_path)
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|relative| !file_path.is_empty() && !relative.is_empty())
    {
        return (Form::FilePath, Some(relative));
    }
    let form = if path.starts_with("/Volumes/") {
        Form::Volume
    } else if path.starts_with("dbfs:/") {
        Form::Dbfs
    } else if path.starts_with('/') {
        Form::Workspace
    } else if path.contains(':') {
        Form::Other
    } else {
        Form::Relative
    };
    (form, None)
}

/// What a path below `workspace.file_path` is in the bundle root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Local {
    /// A file at the same relative path.
    File,
    /// A notebook whose extension the sync dropped: the file has this
    /// extension.
    Notebook(&'static str),
    /// A directory, such as a dbt project or an app's source.
    Directory,
    /// Nothing at that path, with or without a notebook extension.
    Missing,
}

/// The local counterpart of `relative`, a path below `workspace.file_path`,
/// in `bundle_root`. A path that is not plainly relative, such as one with
/// `..` or a root, is [`Local::Missing`]: it cannot name a bundle file.
pub(crate) fn local(bundle_root: &Path, relative: &str) -> Local {
    let Some(path) = join_relative(bundle_root, relative) else {
        return Local::Missing;
    };
    if path.is_file() {
        return Local::File;
    }
    if path.is_dir() {
        return Local::Directory;
    }
    for extension in NOTEBOOK_EXTENSIONS {
        let mut candidate = path.clone().into_os_string();
        candidate.push(".");
        candidate.push(extension);
        if Path::new(&candidate).is_file() {
            return Local::Notebook(extension);
        }
    }
    Local::Missing
}

/// `root` joined with each `/`-separated segment of `relative`, or `None` if
/// a segment is empty, `.`, `..`, or not a plain name on this platform.
fn join_relative(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for segment in relative.split('/') {
        let mut components = Path::new(segment).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(name)), None) if name == segment => path.push(name),
            _ => return None,
        }
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{Form, Local, form, local};
    use crate::test_support::TempDir;

    const FILE_PATH: &str = "/Workspace/Users/user@example.test/projects/example-dev/files";

    #[test]
    fn classifies_path_forms() {
        let synced = format!("{FILE_PATH}/src/silver.sql");
        assert_eq!(
            form(&synced, FILE_PATH),
            (Form::FilePath, Some("src/silver.sql"))
        );
        assert_eq!(
            form(&synced, &format!("{FILE_PATH}/")),
            (Form::FilePath, Some("src/silver.sql")),
            "a trailing slash on file_path is ignored"
        );
        for (path, expected) in [
            ("/Workspace/Shared/lib/util", Form::Workspace),
            (FILE_PATH, Form::Workspace),
            (
                "/Workspace/Users/user@example.test/projects/example-dev/filesystem/x",
                Form::Workspace,
            ),
            ("/Volumes/main/raw/files/a.py", Form::Volume),
            ("dbfs:/FileStore/a.py", Form::Dbfs),
            ("notebooks/silver", Form::Relative),
            ("https://example.test/a.py", Form::Other),
        ] {
            assert_eq!(form(path, FILE_PATH), (expected, None), "{path}");
        }
        assert_eq!(form("/a/b", ""), (Form::Workspace, None));
    }

    #[test]
    fn form_names_are_stable() {
        let names: Vec<_> = [
            Form::FilePath,
            Form::Workspace,
            Form::Volume,
            Form::Dbfs,
            Form::Git,
            Form::Relative,
            Form::Other,
        ]
        .map(Form::as_str)
        .to_vec();
        assert_eq!(
            names,
            [
                "file_path",
                "workspace",
                "volume",
                "dbfs",
                "git",
                "relative",
                "other"
            ]
        );
    }

    #[test]
    fn finds_local_files_and_notebooks_without_extensions() {
        let temp = TempDir::new("bundle-paths");
        let root = temp.path();
        fs::create_dir_all(root.join("notebooks")).expect("create notebooks");
        fs::create_dir_all(root.join("src/gold")).expect("create src");
        fs::write(root.join("notebooks/silver.py"), "# notebook").expect("write");
        fs::write(root.join("src/gold/g.sql"), "SELECT 1").expect("write");

        assert_eq!(local(root, "src/gold/g.sql"), Local::File);
        assert_eq!(local(root, "notebooks/silver"), Local::Notebook("py"));
        assert_eq!(local(root, "src"), Local::Directory);
        assert_eq!(local(root, "src/missing.sql"), Local::Missing);
        for unsafe_path in [
            "../outside",
            "src/../src/gold/g.sql",
            "./src",
            "src//gold",
            "",
        ] {
            assert_eq!(local(root, unsafe_path), Local::Missing, "{unsafe_path}");
        }
    }
}

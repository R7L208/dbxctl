//! Enforces the #23 layering rule: reusable crate-level modules never depend
//! on the probe module. No source file outside `src/probe/` may contain
//! `use crate::probe` or name `CheckId`.

use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN: &[&str] = &["use crate::probe", "CheckId"];

fn rust_sources(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("read source directory") {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            rust_sources(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn only_the_probe_module_names_probe_items() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let probe = src.join("probe");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    files.sort();

    let (inside, outside): (Vec<_>, Vec<_>) =
        files.iter().partition(|path| path.starts_with(&probe));
    // Guards against a scan that silently finds nothing.
    assert!(outside.iter().any(|path| path.ends_with("main.rs")));
    assert!(outside.iter().any(|path| path.ends_with("evidence.rs")));
    assert!(
        inside
            .iter()
            .any(|path| fs::read_to_string(path).is_ok_and(|text| text.contains("CheckId"))),
        "expected CheckId inside src/probe/"
    );

    let violations: Vec<String> = outside
        .iter()
        .flat_map(|path| {
            let text = fs::read_to_string(path).expect("read source file");
            FORBIDDEN
                .iter()
                .filter(|pattern| text.contains(*pattern))
                .map(|pattern| format!("{}: {pattern}", path.display()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        violations.is_empty(),
        "layering violations: {violations:#?}"
    );
}

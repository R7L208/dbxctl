use std::collections::HashSet;
use std::ffi::{OsStr, OsString};

use crate::args::Parser;

/// All valid check IDs for the lineage suite.
const VALID_CHECK_IDS: &[&str] = &[
    "cli", "validate", "plan", "state", "v8", "v1", "api", "v2", "v3", "v11", "v9", "v4", "v6",
    "v10", "v7",
];

#[derive(Debug, Default)]
pub(crate) struct LineageOptions {
    #[allow(dead_code)]
    pub(crate) bundle_root: Option<OsString>,
    #[allow(dead_code)]
    pub(crate) target: Option<OsString>,
    #[allow(dead_code)]
    pub(crate) profile: Option<OsString>,
    #[allow(dead_code)]
    pub(crate) warehouse_id: Option<OsString>,
    #[allow(dead_code)]
    pub(crate) scope_catalog: Option<OsString>,
    #[allow(dead_code)]
    pub(crate) only: Option<Vec<String>>,
    #[allow(dead_code)]
    pub(crate) allow_mutations: bool,
    #[allow(dead_code)]
    pub(crate) v6_table: Option<OsString>,
    #[allow(dead_code)]
    pub(crate) v10_scratch_schema: Option<OsString>,
    #[allow(dead_code)]
    pub(crate) v7_pipeline_id: Option<OsString>,
    #[allow(dead_code)]
    pub(crate) promote_to: Option<OsString>,
}

#[allow(clippy::field_reassign_with_default)]
pub(crate) fn parse_lineage_options(parser: &mut Parser) -> Result<LineageOptions, String> {
    let mut opts = LineageOptions::default();

    opts.bundle_root = parser
        .option("bundle-root")?
        .ok_or("option --bundle-root is required")?
        .to_os_string()
        .into();

    opts.target = parser
        .option("target")?
        .ok_or("option --target is required")?
        .to_os_string()
        .into();

    opts.profile = parser.option("profile")?.map(OsStr::to_os_string);

    opts.warehouse_id = parser.option("warehouse-id")?.map(OsStr::to_os_string);

    opts.scope_catalog = parser.option("scope-catalog")?.map(OsStr::to_os_string);

    // Parse --only with validation
    if let Some(only_str) = parser.option("only")? {
        let check_ids: Vec<String> = only_str
            .to_string_lossy()
            .split(',')
            .map(str::to_string)
            .collect();

        // Validate each check ID
        let mut seen = HashSet::new();
        for id in &check_ids {
            if id.is_empty() {
                return Err("--only: empty check ID (consecutive commas?)".to_string());
            }
            if !VALID_CHECK_IDS.contains(&id.as_str()) {
                return Err(format!(
                    "--only: unknown check ID '{id}' (valid: {})",
                    VALID_CHECK_IDS.join(", ")
                ));
            }
            if !seen.insert(id.clone()) {
                return Err(format!("--only: duplicate check ID '{id}'"));
            }
        }

        opts.only = Some(check_ids);
    }

    opts.allow_mutations = parser.flag("allow-mutations");

    opts.v6_table = parser.option("v6-table")?.map(OsStr::to_os_string);
    opts.v10_scratch_schema = parser
        .option("v10-scratch-schema")?
        .map(OsStr::to_os_string);
    opts.v7_pipeline_id = parser.option("v7-pipeline-id")?.map(OsStr::to_os_string);

    opts.promote_to = parser.option("promote-to")?.map(OsStr::to_os_string);

    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn args_refs(values: &[OsString]) -> impl Iterator<Item = &OsString> {
        values.iter()
    }

    #[test]
    fn requires_bundle_root() {
        let input = args(&["--target", "dev"]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parse_lineage_options(&mut parser).unwrap_err();
        assert!(error.contains("--bundle-root is required"));
    }

    #[test]
    fn requires_target() {
        let input = args(&["--bundle-root", "/tmp"]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parse_lineage_options(&mut parser).unwrap_err();
        assert!(error.contains("--target is required"));
    }

    #[test]
    fn accepts_required_options() {
        let input = args(&["--bundle-root", "/tmp", "--target", "dev"]);
        let mut parser = Parser::new(args_refs(&input));
        let opts = parse_lineage_options(&mut parser).unwrap();
        assert_eq!(opts.bundle_root.as_ref().unwrap().to_string_lossy(), "/tmp");
        assert_eq!(opts.target.as_ref().unwrap().to_string_lossy(), "dev");
    }

    #[test]
    fn accepts_all_optional_options() {
        let input = args(&[
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--profile",
            "myprofile",
            "--warehouse-id",
            "abc123",
            "--scope-catalog",
            "mycat",
            "--allow-mutations",
            "--v6-table",
            "t1",
            "--v10-scratch-schema",
            "s1",
            "--v7-pipeline-id",
            "p1",
            "--promote-to",
            "/approve",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        let opts = parse_lineage_options(&mut parser).unwrap();
        assert_eq!(
            opts.profile.as_ref().unwrap().to_string_lossy(),
            "myprofile"
        );
        assert_eq!(
            opts.warehouse_id.as_ref().unwrap().to_string_lossy(),
            "abc123"
        );
        assert_eq!(
            opts.scope_catalog.as_ref().unwrap().to_string_lossy(),
            "mycat"
        );
        assert!(opts.allow_mutations);
        assert_eq!(opts.v6_table.as_ref().unwrap().to_string_lossy(), "t1");
        assert_eq!(
            opts.v10_scratch_schema.as_ref().unwrap().to_string_lossy(),
            "s1"
        );
        assert_eq!(
            opts.v7_pipeline_id.as_ref().unwrap().to_string_lossy(),
            "p1"
        );
        assert_eq!(
            opts.promote_to.as_ref().unwrap().to_string_lossy(),
            "/approve"
        );
    }

    #[test]
    fn parses_only_with_single_check() {
        let input = args(&["--bundle-root", "/tmp", "--target", "dev", "--only", "cli"]);
        let mut parser = Parser::new(args_refs(&input));
        let opts = parse_lineage_options(&mut parser).unwrap();
        assert_eq!(opts.only.as_ref().unwrap(), &vec!["cli".to_string()]);
    }

    #[test]
    fn parses_only_with_multiple_checks() {
        let input = args(&[
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--only",
            "cli,validate,plan",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        let opts = parse_lineage_options(&mut parser).unwrap();
        let expected = vec![
            "cli".to_string(),
            "validate".to_string(),
            "plan".to_string(),
        ];
        assert_eq!(opts.only.as_ref().unwrap(), &expected);
    }

    #[test]
    fn rejects_unknown_check_id() {
        let input = args(&[
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--only",
            "unknown",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parse_lineage_options(&mut parser).unwrap_err();
        assert!(error.contains("unknown check ID"));
        assert!(error.contains("unknown"));
    }

    #[test]
    fn rejects_duplicate_check_ids() {
        let input = args(&[
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--only",
            "cli,plan,cli",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parse_lineage_options(&mut parser).unwrap_err();
        assert!(error.contains("duplicate check ID"));
    }

    #[test]
    fn rejects_empty_check_id() {
        let input = args(&[
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--only",
            "cli,,plan",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parse_lineage_options(&mut parser).unwrap_err();
        assert!(error.contains("empty check ID"));
    }

    #[test]
    fn validates_all_check_ids() {
        let input = args(&[
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--only",
            "cli,validate,plan,state,v8,v1,api,v2,v3,v11,v9,v4,v6,v10,v7",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        let opts = parse_lineage_options(&mut parser).unwrap();
        assert_eq!(opts.only.as_ref().unwrap().len(), 15);
    }

    #[test]
    fn only_flag_is_optional() {
        let input = args(&["--bundle-root", "/tmp", "--target", "dev"]);
        let mut parser = Parser::new(args_refs(&input));
        let opts = parse_lineage_options(&mut parser).unwrap();
        assert_eq!(opts.only, None);
    }

    #[test]
    fn allow_mutations_flag_is_optional() {
        let input = args(&["--bundle-root", "/tmp", "--target", "dev"]);
        let mut parser = Parser::new(args_refs(&input));
        let opts = parse_lineage_options(&mut parser).unwrap();
        assert!(!opts.allow_mutations);
    }

    #[test]
    fn all_mutation_inputs_are_optional() {
        let input = args(&["--bundle-root", "/tmp", "--target", "dev"]);
        let mut parser = Parser::new(args_refs(&input));
        let opts = parse_lineage_options(&mut parser).unwrap();
        assert_eq!(opts.v6_table, None);
        assert_eq!(opts.v10_scratch_schema, None);
        assert_eq!(opts.v7_pipeline_id, None);
    }
}

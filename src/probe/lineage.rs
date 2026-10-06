use std::collections::HashSet;
use std::ffi::{OsStr, OsString};

use crate::args::Parser;

/// All valid check IDs for the lineage suite (from #23 contract).
const VALID_CHECK_IDS: &[&str] = &[
    "cli", "validate", "plan", "state", "v8", "v1", "api", "v2", "v3", "v11", "v9", "v4", "v6",
    "v10", "v7",
];

/// Check requirements: whether each check needs warehouse-id and/or scope-catalog.
/// Derived from #23 checks table: checks using Statement Execution need warehouse-id;
/// checks using catalog-scoped probes need scope-catalog.
struct CheckRequirements {
    needs_warehouse: bool,
    needs_catalog: bool,
}

fn check_requirements(id: &str) -> CheckRequirements {
    match id {
        // Offline/bundle-only checks (no warehouse or catalog needed)
        "cli" | "validate" | "plan" | "state" | "v8" | "v1" | "api" | "v6" | "v10" | "v7" => {
            CheckRequirements {
                needs_warehouse: false,
                needs_catalog: false,
            }
        }
        // Checks requiring warehouse (Statement Execution)
        "v2" | "v9" | "v4" => CheckRequirements {
            needs_warehouse: true,
            needs_catalog: false,
        }, // v2: wait_timeout with SELECT 1; v9: Lineage via API; v4: Refresh history via API
        // Checks requiring both warehouse and catalog
        "v3" | "v11" => CheckRequirements {
            needs_warehouse: true,
            needs_catalog: true,
        }, // MV/metric-view checks
        _ => CheckRequirements {
            needs_warehouse: false,
            needs_catalog: false,
        },
    }
}

#[derive(Debug, Default)]
pub(crate) struct LineageOptions {
    #[allow(dead_code)] // Used by #30 (orchestration)
    pub(crate) bundle_root: Option<OsString>,
    #[allow(dead_code)] // Used by #30
    pub(crate) target: Option<OsString>,
    #[allow(dead_code)] // Used by #30
    pub(crate) profile: Option<OsString>,
    #[allow(dead_code)] // Used by #30
    pub(crate) warehouse_id: Option<OsString>,
    #[allow(dead_code)] // Used by #30
    pub(crate) scope_catalog: Option<OsString>,
    #[allow(dead_code)] // Used by #30
    pub(crate) only: Option<Vec<String>>,
    #[allow(dead_code)] // Used by #37
    pub(crate) allow_mutations: bool,
    #[allow(dead_code)] // Used by #37
    pub(crate) v6_table: Option<OsString>,
    #[allow(dead_code)] // Used by #37
    pub(crate) v10_scratch_schema: Option<OsString>,
    #[allow(dead_code)] // Used by #37
    pub(crate) v7_pipeline_id: Option<OsString>,
    #[allow(dead_code)] // Used by #30
    pub(crate) promote_to: Option<OsString>,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn parse_lineage_options(parser: &mut Parser) -> Result<LineageOptions, String> {
    let bundle_root = parser
        .option("bundle-root")?
        .ok_or("option --bundle-root is required")?
        .to_os_string();

    let target = parser
        .option("target")?
        .ok_or("option --target is required")?
        .to_os_string();

    let profile = parser.option("profile")?.map(OsStr::to_os_string);

    let warehouse_id = parser.option("warehouse-id")?.map(OsStr::to_os_string);

    let scope_catalog = parser.option("scope-catalog")?.map(OsStr::to_os_string);

    // Parse --only with validation
    let only = if let Some(only_str) = parser.option("only")? {
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

        Some(check_ids)
    } else {
        None
    };

    let allow_mutations = parser.flag("allow-mutations");

    // Reject mutation inputs if --allow-mutations is not set
    if !allow_mutations {
        if parser.option("v6-table")?.is_some() {
            return Err("--v6-table requires --allow-mutations flag".to_string());
        }
        if parser.option("v10-scratch-schema")?.is_some() {
            return Err("--v10-scratch-schema requires --allow-mutations flag".to_string());
        }
        if parser.option("v7-pipeline-id")?.is_some() {
            return Err("--v7-pipeline-id requires --allow-mutations flag".to_string());
        }
    }

    let v6_table = parser.option("v6-table")?.map(OsStr::to_os_string);
    let v10_scratch_schema = parser
        .option("v10-scratch-schema")?
        .map(OsStr::to_os_string);
    let v7_pipeline_id = parser.option("v7-pipeline-id")?.map(OsStr::to_os_string);

    let promote_to = parser.option("promote-to")?.map(OsStr::to_os_string);

    // Determine which checks are selected (default: all)
    let selected_checks: HashSet<&str> = if let Some(ref checks) = only {
        checks.iter().map(String::as_str).collect()
    } else {
        VALID_CHECK_IDS.iter().copied().collect()
    };

    // Check if required options are satisfied based on selected checks
    let mut needs_warehouse = false;
    let mut needs_catalog = false;
    for check_id in &selected_checks {
        let reqs = check_requirements(check_id);
        if reqs.needs_warehouse {
            needs_warehouse = true;
        }
        if reqs.needs_catalog {
            needs_catalog = true;
        }
    }

    // Collect list of checks that need each resource for error messages
    let warehouse_checks: Vec<&str> = selected_checks
        .iter()
        .filter(|id| check_requirements(id).needs_warehouse)
        .copied()
        .collect();
    let catalog_checks: Vec<&str> = selected_checks
        .iter()
        .filter(|id| check_requirements(id).needs_catalog)
        .copied()
        .collect();

    if needs_warehouse && warehouse_id.is_none() {
        return Err(format!(
            "--warehouse-id is required by selected checks: {}",
            warehouse_checks.join(", ")
        ));
    }

    if needs_catalog && scope_catalog.is_none() {
        return Err(format!(
            "--scope-catalog is required by selected checks: {}",
            catalog_checks.join(", ")
        ));
    }

    Ok(LineageOptions {
        bundle_root: Some(bundle_root),
        target: Some(target),
        profile,
        warehouse_id,
        scope_catalog,
        only,
        allow_mutations,
        v6_table,
        v10_scratch_schema,
        v7_pipeline_id,
        promote_to,
    })
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
        // Use offline-only checks to avoid warehouse requirement
        let input = args(&[
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--only",
            "cli,validate",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        let opts = parse_lineage_options(&mut parser).unwrap();
        assert_eq!(opts.bundle_root.as_ref().unwrap().to_string_lossy(), "/tmp");
        assert_eq!(opts.target.as_ref().unwrap().to_string_lossy(), "dev");
    }

    #[test]
    fn all_checks_online_dont_require_warehouse_by_default() {
        // With --only containing only offline checks, warehouse shouldn't be required
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
        assert!(opts.warehouse_id.is_none());
        assert!(opts.scope_catalog.is_none());
    }

    #[test]
    fn warehouse_required_when_v2_selected() {
        let input = args(&["--bundle-root", "/tmp", "--target", "dev", "--only", "v2"]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parse_lineage_options(&mut parser).unwrap_err();
        assert!(error.contains("--warehouse-id is required"));
        assert!(error.contains("v2"));
    }

    #[test]
    fn catalog_required_when_v3_selected() {
        let input = args(&[
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--only",
            "v3",
            "--warehouse-id",
            "w1",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parse_lineage_options(&mut parser).unwrap_err();
        assert!(error.contains("--scope-catalog is required"));
        assert!(error.contains("v3"));
    }

    #[test]
    fn rejects_mutation_input_without_flag() {
        let input = args(&[
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--v6-table",
            "t1",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        let error = parse_lineage_options(&mut parser).unwrap_err();
        assert!(error.contains("--allow-mutations"));
    }

    #[test]
    fn accepts_mutation_input_with_flag() {
        // Use offline-only checks to avoid warehouse requirement
        let input = args(&[
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--allow-mutations",
            "--v6-table",
            "t1",
            "--only",
            "cli",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        let opts = parse_lineage_options(&mut parser).unwrap();
        assert!(opts.allow_mutations);
        assert_eq!(opts.v6_table.as_ref().unwrap().to_string_lossy(), "t1");
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
            "--warehouse-id",
            "w1",
            "--scope-catalog",
            "c1",
            "--only",
            "cli,validate,plan,state,v8,v1,api,v2,v3,v11,v9,v4,v6,v10,v7",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        let opts = parse_lineage_options(&mut parser).unwrap();
        assert_eq!(opts.only.as_ref().unwrap().len(), 15);
    }
}

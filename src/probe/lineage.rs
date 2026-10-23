pub(crate) mod cli;

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};

use crate::args::Parser;
use crate::probe::model::{CheckId, Finding};
use crate::probe::orchestrator::Context;

/// Runs one check and returns its finding.
pub(crate) type Runner = fn(&Context<'_>) -> Result<Finding, String>;

/// The runner for `check`, or `None` until its step issue implements it. This
/// is the only list of implemented checks: the orchestrator runs these and
/// reports the rest as skipped, and only these require their inputs. Add a
/// check here when its step issue lands.
pub(crate) fn runner(check: CheckId) -> Option<Runner> {
    match check {
        CheckId::Cli => Some(cli::run),
        _ => None,
    }
}

fn is_implemented(check: CheckId) -> bool {
    runner(check).is_some()
}

/// Check requirements: whether each check needs warehouse-id and/or scope-catalog.
/// Derived from #23 checks table: checks using Statement Execution need warehouse-id;
/// checks using catalog-scoped probes need scope-catalog.
struct CheckRequirements {
    needs_warehouse: bool,
    needs_catalog: bool,
}

fn check_requirements(id: CheckId) -> CheckRequirements {
    match id {
        // Offline/bundle-only checks (no warehouse or catalog needed)
        CheckId::Cli
        | CheckId::Validate
        | CheckId::Plan
        | CheckId::State
        | CheckId::V8
        | CheckId::V1
        | CheckId::Api
        | CheckId::V6
        | CheckId::V10
        | CheckId::V7 => CheckRequirements {
            needs_warehouse: false,
            needs_catalog: false,
        },
        // Checks requiring warehouse (Statement Execution)
        CheckId::V2 | CheckId::V9 | CheckId::V4 => CheckRequirements {
            needs_warehouse: true,
            needs_catalog: false,
        }, // v2: wait_timeout with SELECT 1; v9: Lineage via API; v4: Refresh history via API
        // Checks requiring both warehouse and catalog
        CheckId::V3 | CheckId::V11 => CheckRequirements {
            needs_warehouse: true,
            needs_catalog: true,
        }, // MV/metric-view checks
    }
}

fn valid_check_ids() -> String {
    CheckId::ALL.map(CheckId::as_str).join(", ")
}

/// Parse and validate the --only option. Returns a `HashSet` for efficient duplicate detection.
fn parse_only_option(parser: &mut Parser) -> Result<Option<Vec<String>>, String> {
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
            if CheckId::parse(id).is_none() {
                return Err(format!(
                    "--only: unknown check ID '{id}' (valid: {})",
                    valid_check_ids()
                ));
            }
            if !seen.insert(id.clone()) {
                return Err(format!("--only: duplicate check ID '{id}'"));
            }
        }

        Ok(Some(check_ids))
    } else {
        Ok(None)
    }
}

/// Validate mutation input options. Returns error if mutation inputs are provided without --allow-mutations.
fn validate_mutation_inputs(parser: &mut Parser, allow_mutations: bool) -> Result<(), String> {
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
    Ok(())
}

/// Determine selected checks in canonical order based on --only option.
fn determine_selected_checks(only: Option<&Vec<String>>) -> Vec<CheckId> {
    let only_set: HashSet<&str> = only
        .map(|checks| checks.iter().map(String::as_str).collect())
        .unwrap_or_default();

    if only_set.is_empty() {
        CheckId::ALL.to_vec()
    } else {
        CheckId::ALL
            .into_iter()
            .filter(|id| only_set.contains(id.as_str()))
            .collect()
    }
}

/// The checks in `checks` that need a warehouse and a scope catalog, in
/// canonical order.
fn check_resource_requirements(checks: &[CheckId]) -> (Vec<&'static str>, Vec<&'static str>) {
    let needing = |needs: fn(&CheckRequirements) -> bool| {
        checks
            .iter()
            .copied()
            .filter(|&id| needs(&check_requirements(id)))
            .map(CheckId::as_str)
            .collect()
    };
    (
        needing(|reqs| reqs.needs_warehouse),
        needing(|reqs| reqs.needs_catalog),
    )
}

#[derive(Debug, Default)]
pub(crate) struct LineageOptions {
    pub(crate) bundle_root: Option<OsString>,
    pub(crate) target: Option<OsString>,
    pub(crate) profile: Option<OsString>,
    #[allow(dead_code)] // Used by the SQL probes (#33)
    pub(crate) warehouse_id: Option<OsString>,
    #[allow(dead_code)] // Used by the catalog-scoped probes (#33)
    pub(crate) scope_catalog: Option<OsString>,
    pub(crate) only: Option<Vec<String>>,
    #[allow(dead_code)] // Used by #37
    pub(crate) allow_mutations: bool,
    #[allow(dead_code)] // Used by #37
    pub(crate) v6_table: Option<OsString>,
    #[allow(dead_code)] // Used by #37
    pub(crate) v10_scratch_schema: Option<OsString>,
    #[allow(dead_code)] // Used by #37
    pub(crate) v7_pipeline_id: Option<OsString>,
    pub(crate) promote_to: Option<OsString>,
}

impl LineageOptions {
    /// The checks selected by `--only` (all checks without it), in canonical
    /// order.
    pub(crate) fn selected_checks(&self) -> Vec<CheckId> {
        determine_selected_checks(self.only.as_ref())
    }
}

pub(crate) fn parse_lineage_options(parser: &mut Parser) -> Result<LineageOptions, String> {
    parse_with(parser, is_implemented)
}

/// Parses the options, requiring a warehouse and catalog only for the
/// selected checks for which `implemented` holds. Tests pass `|_| true` to
/// exercise requirements that no implemented check has yet.
fn parse_with(
    parser: &mut Parser,
    implemented: fn(CheckId) -> bool,
) -> Result<LineageOptions, String> {
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

    // Parse and validate --only
    let only = parse_only_option(parser)?;

    let allow_mutations = parser.flag("allow-mutations");

    // Validate mutation inputs
    validate_mutation_inputs(parser, allow_mutations)?;

    let v6_table = parser.option("v6-table")?.map(OsStr::to_os_string);
    let v10_scratch_schema = parser
        .option("v10-scratch-schema")?
        .map(OsStr::to_os_string);
    let v7_pipeline_id = parser.option("v7-pipeline-id")?.map(OsStr::to_os_string);

    let promote_to = parser.option("promote-to")?.map(OsStr::to_os_string);

    // Only implemented checks read their inputs; the rest are skipped.
    let mut running_checks = determine_selected_checks(only.as_ref());
    running_checks.retain(|&id| implemented(id));
    let (warehouse_checks, catalog_checks) = check_resource_requirements(&running_checks);

    if !warehouse_checks.is_empty() && warehouse_id.is_none() {
        return Err(format!(
            "--warehouse-id is required by selected checks: {}",
            warehouse_checks.join(", ")
        ));
    }

    if !catalog_checks.is_empty() && scope_catalog.is_none() {
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
    fn unimplemented_checks_need_no_warehouse_or_catalog() {
        // v2 and v3 read the warehouse and catalog once #33 implements them;
        // until then they are skipped, so neither option is required.
        let input = args(&[
            "--bundle-root",
            "/tmp",
            "--target",
            "dev",
            "--only",
            "v2,v3",
        ]);
        let mut parser = Parser::new(args_refs(&input));
        let opts = parse_lineage_options(&mut parser).unwrap();
        assert!(opts.warehouse_id.is_none());
        assert!(opts.scope_catalog.is_none());
    }

    #[test]
    fn requirements_follow_the_check_table() {
        let (warehouse, catalog) = check_resource_requirements(&CheckId::ALL);
        assert_eq!(warehouse, ["v2", "v3", "v11", "v9", "v4"]);
        assert_eq!(catalog, ["v3", "v11"]);

        let (warehouse, catalog) = check_resource_requirements(&[CheckId::Cli, CheckId::V6]);
        assert!(warehouse.is_empty() && catalog.is_empty());
    }

    #[test]
    fn only_the_cli_check_is_implemented() {
        let implemented: Vec<_> = CheckId::ALL
            .into_iter()
            .filter(|&id| is_implemented(id))
            .collect();
        assert_eq!(implemented, [CheckId::Cli]);
    }

    /// Parses `extra` after the required options as if `implemented` named
    /// the implemented checks.
    fn parse_implementing(
        implemented: fn(CheckId) -> bool,
        extra: &[&str],
    ) -> Result<LineageOptions, String> {
        let mut values = vec!["--bundle-root", "/tmp", "--target", "dev"];
        values.extend_from_slice(extra);
        let input = args(&values);
        let mut parser = Parser::new(args_refs(&input));
        parse_with(&mut parser, implemented)
    }

    /// Parses `extra` as if every check were implemented, so the
    /// requirement errors run before #33 lands.
    fn parse_all_implemented(extra: &[&str]) -> Result<LineageOptions, String> {
        parse_implementing(|_| true, extra)
    }

    /// Parses `extra` as if only `v2` were implemented, as partway
    /// through #33.
    fn parse_only_v2_implemented(extra: &[&str]) -> Result<LineageOptions, String> {
        parse_implementing(|id| id == CheckId::V2, extra)
    }

    #[test]
    fn partial_implementation_names_only_implemented_checks() {
        // v3, v11, v9, and v4 also read the warehouse once implemented, but
        // only v2 is reported until then.
        assert_eq!(
            parse_only_v2_implemented(&[]).unwrap_err(),
            "--warehouse-id is required by selected checks: v2"
        );
    }

    #[test]
    fn partial_implementation_defers_the_catalog_requirement() {
        // v3 and v11 need --scope-catalog, but neither is implemented yet.
        let opts = parse_only_v2_implemented(&["--warehouse-id", "w"]).unwrap();
        assert_eq!(opts.warehouse_id.unwrap().to_string_lossy(), "w");
        assert!(opts.scope_catalog.is_none());
    }

    #[test]
    fn full_selection_requires_warehouse_for_every_sql_check() {
        assert_eq!(
            parse_all_implemented(&[]).unwrap_err(),
            "--warehouse-id is required by selected checks: v2, v3, v11, v9, v4"
        );
    }

    #[test]
    fn warehouse_is_required_before_catalog() {
        // v3 needs both; the missing warehouse is reported first.
        assert_eq!(
            parse_all_implemented(&["--only", "v3"]).unwrap_err(),
            "--warehouse-id is required by selected checks: v3"
        );
    }

    #[test]
    fn catalog_required_once_warehouse_is_given() {
        assert_eq!(
            parse_all_implemented(&["--warehouse-id", "w"]).unwrap_err(),
            "--scope-catalog is required by selected checks: v3, v11"
        );
    }

    #[test]
    fn requirements_list_selected_checks_in_canonical_order() {
        assert_eq!(
            parse_all_implemented(&["--only", "v4,v2"]).unwrap_err(),
            "--warehouse-id is required by selected checks: v2, v4"
        );
    }

    #[test]
    fn both_options_satisfy_every_requirement() {
        let opts = parse_all_implemented(&["--warehouse-id", "w", "--scope-catalog", "c"]).unwrap();
        assert_eq!(opts.warehouse_id.unwrap().to_string_lossy(), "w");
        assert_eq!(opts.scope_catalog.unwrap().to_string_lossy(), "c");
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

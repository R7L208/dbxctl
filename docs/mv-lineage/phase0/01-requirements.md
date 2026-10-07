# Phase 0 Requirements

This document mirrors the Phase 0 contract in tracking issue #23, which remains
the source of truth until #38 finalizes this page. If the two disagree, #23
wins and this page must be corrected. Requirements are numbered `R1`, `R2`, …
as defined in the [glossary](../../design-nomenclature-and-glossary.md);
headings are stable so code sites can link to them.

> **Status:** None of the `probe` behavior below is implemented yet. Each
> requirement names the issue that implements it.

## Goal

Add a `dbxctl probe` command that runs 15 checks against a Databricks Asset
Bundle and workspace, keeps the raw evidence, and writes a deterministic,
redacted report. Every check ends resolved, or explicitly unknown with a reason
and a fallback that later phases will use.

## R1: Commands

```text
dbxctl probe run --suite lineage ...
dbxctl probe report --from <run-directory>
dbxctl probe cleanup --from <run-directory>
```

`probe report` and `probe cleanup` take only `--from <run-directory>` and read
suite and run metadata from `run.json`. Implemented by #29 (parsing), #30
(`run`), #36 (`report`), and #37 (`cleanup`).

## R2: Run Options

| Option | Required | Notes |
| --- | --- | --- |
| `--suite lineage` | yes | The only suite in Phase 0 |
| `--bundle-root <path>` | yes | All CLI calls run from here |
| `--target <name>` | yes | Forwarded as `-t <target>` |
| `--profile <name>` | no | Forwarded as `-p <profile>` to every CLI call |
| `--warehouse-id <id>` | conditional | Required only when selected checks use Statement Execution |
| `--scope-catalog <catalog>` | conditional | Required only when selected checks use catalog-scoped probes |
| `--only <a,b,...>` | no | Subset of check IDs |
| `--allow-mutations` | no | Required before any mutation capability is granted |
| `--v6-table`, `--v10-scratch-schema`, `--v7-pipeline-id` | no | Per-check mutation inputs |
| `--promote-to <fixture-dir>` | no | Copies approved redacted evidence |

Input validation depends on the checks selected by `--only`. Offline and
bundle-only runs do not require SQL or catalog inputs. Paths are parsed as
`OsString` so non-UTF-8 paths survive. Missing values, unknown options, and
unknown suites are usage errors (exit 1). Implemented by #29.

## R3: Exit Codes

Centralized in `src/exit.rs` (#30).

| Code | Meaning |
| --- | --- |
| 0 | Run completed and every selected check resolved |
| 1 | Invocation or usage failure |
| 10 | One or more selected checks not resolved (unknown, skipped, or failed) |
| 11 | Cleanup failure |

The #23 contract words exit 10 as "unknown or blocked". The finding states in
[R12](#r12-findings) have no "blocked" state, so this page states the mapping
explicitly: any selected check that is not resolved yields exit 10.

## R4: Checks

| ID | Question it answers | Mode | Issue |
| --- | --- | --- | --- |
| `cli` | CLI version and required command surfaces | Read | #30 (minimal), #32 (full) |
| `validate` | Validated bundle config, engine, workspace paths, scripts | Read | #32 |
| `plan` | Plan shape, action vocabulary, per-action config location | Read | #32 |
| `state` | `resources.json` layout and pipeline-ID paths | Read | #32 |
| `v8` | Source origins and path forms | Read | #32 |
| `v1` | Experimental script-key spelling behavior | Read | #32 |
| `api` | `databricks api --debug` exposes HTTP status and `error_code` | Read | #33 |
| `v2` | Statement Execution `wait_timeout` range, using `SELECT 1` | Read | #33 |
| `v3` | MV naming, `pipeline_type`, `system.lakeflow.pipelines` shape | Read | #33 |
| `v11` | Metric-view `table_type` and shared-catalog representation | Read | #33 |
| `v9` | Whether lineage attributes pipeline writes by pipeline ID | Read | #33 |
| `v4` | Where refresh history is exposed | Read | #33 |
| `v6` | Whether table-update triggers are job fields | Read + opt-in | #34 (A), #37 (B) |
| `v10` | Unqualified-name resolution in a SQL file task | Read + opt-in | #37 |
| `v7` | Whether validate-only updates emit `dataset_definition` | Opt-in | #37 |

Lowercase `v` checks gather evidence for the correspondingly numbered `V`
questions; see the [glossary](../../design-nomenclature-and-glossary.md#identifier-rules).

## R5: CLI Baseline

Phase 0 targets Databricks CLI **1.13.0**, which is also the minimum version
`dbxctl doctor` accepts (`MINIMUM_DATABRICKS_VERSION` in `src/databricks.rs`)
and the version CI pins. The facts below were observed on CLI 0.296.0. Each is
an expectation to confirm on 1.13.0, not a given:

- The required CLI surfaces exist.
- `bundle validate` can emit JSON on stdout even when it exits 1.
- State pipeline IDs appear below `/state/resources.pipelines.<key>/__id__`.
- Rewritten plan actions contain source configuration; `skip` may require
  validate output.
- Unknown experimental script keys are not rejected.
- Offline `bundle schema` places table-update triggers under jobs.

Any unobserved or changed shape must become an explicit finding, not a panic or
a silent guess. The synthetic fixtures in `tests/fixtures/cli-1.13.0/` (#25)
encode these expectations; several shapes are marked unverified there.

## R6: Transport Through the Databricks CLI

Every workspace operation goes through the resolved `databricks` executable.
There is no direct HTTP client, separate authentication implementation, shell
invocation, or async runtime. See
[transport boundary](02-architecture.md#transport-boundary).

## R7: Module Layering

Reusable modules live at crate level: arguments, Databricks process execution,
transport, JSON, exit codes, evidence, safety, UC identifiers, and bundle
readers. Only the 15 lineage checks belong under `src/probe/lineage/`. Nothing
outside `src/probe/` may import from `probe` or name `CheckId`. Shared items
stay `pub(crate)` unless stronger visibility is required. See
[layering rule](02-architecture.md#layering-rule).

## R8: Safe Rust and Test Environment

`unsafe` stays forbidden (`unsafe_code = "forbid"` under `[lints.rust]` in
`Cargo.toml`). Tests must not call `std::env::set_var`; they
pass environment values to child processes instead.

## R9: Read-Only by Default

Probes are read-only unless mutation is explicitly authorized. SQL that is not
a read statement is refused before any CLI process starts (#31).

## R10: Mutation Capability and Cleanup

Mutation-capable code requires a private capability token obtainable only after
`--allow-mutations` **and** the check's specific input are validated. Created
resources are logged before any subsequent operation, and cleanup is
idempotent. Implemented by #37, which may be deferred; see
[R14](#r14-phase-0-exit-criteria).

## R11: Evidence Layout

Each run creates `<bundle-root>/.dbxctl/probe/lineage/<run-id>/` containing
`run.json`, raw per-check evidence, `findings.json`, `findings.md`, and a
created-resource log when applicable. `.dbxctl/` must be git-ignored
automatically. Implemented by #30, #36, and #37.

## R12: Findings

Each finding records the check ID, verdict, state (resolved, unknown, skipped,
or failed), evidence reference, reason and fallback for unknowns, and the
affected code or design site. Implemented by #30 (model) and #36 (sites and
report).

## R13: Determinism and Redaction

Output ordering and serialization are deterministic. Reports and promoted
fixtures redact credentials, tokens, hosts, user-specific absolute paths, and
sensitive workspace identifiers. Implemented by #36; minimal credential
scrubbing of stored debug output lands earlier in #31.

## R14: Phase 0 Exit Criteria

- Every check is either resolved, or explicitly unknown with a reason and a
  fallback that later phases will use.
- `plan`, `state`, and `v8` are resolved against a real bundle.
- One real read-only run has been reviewed and its redacted report kept.
- The mutation probes (#37) may be deferred. If they are, v6 part B, v7, and
  v10 are reported as unknown with fallbacks, and #37 moves out of the
  milestone.
- All CI lanes are green, with line coverage of at least 90%.

## R15: Standard Checks

Every step issue must pass:

```console
cargo fmt --check
cargo clippy --locked --all-targets --all-features
cargo test --locked --all-features
cargo llvm-cov --locked --all-features --workspace \
  --ignore-filename-regex '(/rustc/|/tests/)' --fail-under-lines 90
scripts/markdown.sh check
```

Pre-existing tests must keep passing unmodified unless an issue says otherwise.
No test may contact a real workspace.

## Open Items

- Phase 1–4 gates: defined by #36 in
  [06-findings-and-handoff.md](06-findings-and-handoff.md).
- Whether #37 ships in milestone 0.0.1 or is deferred: recorded before #38
  starts.
- Workspace and authentication context recording in run metadata: proposed in
  #55, which has no milestone and is not part of the contract.

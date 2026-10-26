# Phase 0 Architecture

This page describes how the Phase 0 probe suite fits into the `dbxctl` crate:
which module owns what, the layering rule that keeps probe code contained, and
the boundary through which every workspace operation passes. It implements
[R6](01-requirements.md#r6-transport-through-the-databricks-cli) and
[R7](01-requirements.md#r7-module-layering).

> **Status:** Modules marked *implemented* exist in `src/` today. Modules marked
> *planned* are named by their step issue and do not exist yet; their paths
> and responsibilities may change when they are built. #38 updates this page
> to match the final code.

## Module Ownership

### Implemented Modules

| Module | Owns | Issue |
| --- | --- | --- |
| `src/main.rs` | Top-level argument dispatch (`doctor`, `databricks`, `probe`, `help`, `version`) and process exit. | #29 |
| `src/databricks.rs` | Resolving the `databricks` executable, captured execution with a timeout, passthrough, the minimum-version check, and `doctor`. | #27, #48 |
| `src/json.rs` | The crate-private JSON layer from DP0-1: parsing into `Document`, borrowed `Node` views, JSON-pointer lookup, and deterministic serialization. No `serde_json` type appears in its API. | #24, #28 |
| `src/args.rs` | A small reusable option parser over `OsString`. | #29 |
| `src/exit.rs` | The exit-code table from [R3](01-requirements.md#r3-exit-codes). | #30 |
| `src/evidence.rs` | Run-ID generation, the run directory, raw evidence writes, `run.json`, and the `.dbxctl/.gitignore`. | #30 |
| `src/transport.rs` | The `Runner` trait and `CliRunner::for_bundle(binary, root, target, profile)`: captured runs of known read-only CLI commands, API GETs through `databricks api get --debug` with the HTTP status and `error_code` of the logged response, and Statement Execution with a submit wait, polls, and result-chunk reads all bounded by the deadline, cancellation of a still-running statement at the deadline or after a failed poll, result chunks checked against the manifest, and error classification. `submit` sends one statement with a chosen wait and cancels it if it is left running, and `is_not_found` and `is_permission_denied` classify an error by HTTP status or error class. | #31, #33 |
| `src/safety.rs` | The read-only CLI commands, as offline commands and commands that contact the workspace, and `is_read_only_command`. `is_read_statement`, the read-only SQL guard. It refuses anything Spark could read differently, such as query hints or a backslash before a literal's quote, and calls to functions with side effects (`http_request`, `remote_query`, `ai_query`). | #31 |
| `src/uc/fqn.rs` | Parsing and formatting Unity Catalog three-part names, including backtick quoting. | #31 |
| `src/text.rs` | SQL string-literal and identifier quoting. A value with a `'` or `\` has no literal that means the same text under every `escapedStringLiterals` setting, so it must be a statement parameter. | #31 |
| `src/bundle/config.rs` | Reading `bundle validate -o json` output: the engine, workspace paths, the declared resources, script keys, the source paths each resource names, and resource locations from `--include-locations`. | #32 |
| `src/bundle/paths.rs` | The form of a validated source path (below `workspace.file_path`, another workspace path, a volume, DBFS, Git, relative, or other) and the local file a `file_path` path names in the bundle root. | #32 |
| `src/bundle/plan.rs` | Reading `bundle plan -o json` output: each `resources.<type>.<key>` entry's action and the configuration in `new_state.value`. | #32 |
| `src/bundle/state.rs` | Where the local copy of `resources.json` lives, and reading its entries and their `__id__`s. | #32 |
| `src/bundle/schema.rs` | The `databricks bundle schema` command and timeout, and finding the packages whose definitions declare a property, such as `table_update`: telling definitions from path segments, resolving `$ref`s, and walking each ref once per definition. | #34 |

### Planned Crate-Level Modules

| Module | Will own | Issue |
| --- | --- | --- |
| `src/safety.rs` additions | If #37 ships, the `Mutate` capability and `safety::grant`. | #37 |

### Implemented Probe Modules

`probe run` runs the bundle checks, the SQL and API checks, and `v6` part A so far; every other selected check
is recorded as skipped and names the issue that adds it.

| Module | Owns | Issue |
| --- | --- | --- |
| `src/probe.rs` | The `probe` command group, its argument parsing, and help. | #29 |
| `src/probe/lineage.rs` | Lineage suite options from [R2](01-requirements.md#r2-run-options), including which checks need a warehouse or catalog; `run_step`, which runs one offline CLI step of a check, records it as evidence, and classifies its failure; and `run_bundle_step`, which does the same for a bundle command through the transport, keeping the output of a non-zero exit. | #29, #32, #34 |
| `src/probe/model.rs` | `CheckId`, verdict and state types, and the finding struct. | #30 |
| `src/probe/validation.rs` | The probe run's one `bundle validate`, shared by `validate`, `plan`, and `v8`: it reads `workspace.file_path` with `bundle summary`, checks it with a read-only `get-status`, and runs validate only if the directory exists or `--allow-mutations` was given ([R9](01-requirements.md#r9-read-only-by-default)). | #32 |
| `src/probe/orchestrator.rs` | Running selected checks in a fixed order and writing the run directory: `run.json`, raw evidence, `findings.json`, and the skeleton `findings.md`. | #30 |
| `src/probe/lineage/cli.rs` | The `cli` check: which Databricks CLI version is installed, and whether its help offers every command the suite runs. | #30, #32 |
| `src/probe/lineage/validate.rs` | The `validate` check, from the shared validate. | #32 |
| `src/probe/lineage/plan.rs` | The `plan` check: the plan's actions and where each carries its configuration, looking up `skip` and `delete` entries in validate output. | #32 |
| `src/probe/lineage/state.rs` | The `state` check: the layout of the local `resources.json` and its ID paths. | #32 |
| `src/probe/lineage/v8.rs` | The `v8` check: source path forms, their local files, and where each resource is defined. | #32 |
| `src/probe/lineage/v1.rs` | The `v1` check: whether the bundle schema accepts any `experimental.scripts` key. | #32 |
| `src/probe/workspace.rs` | Workspace steps for the API and SQL checks: running one transport operation, recording each of its CLI runs as evidence, telling a CLI that could not run from a workspace that answered with an error, keeping workspace values in verdicts to plain words, and the scope-catalog `table_type` scan that `v3` and `v11` share, run once per run. | #33 |
| `src/probe/lineage/api.rs` | `api`: whether `databricks api --debug` logs the HTTP status and `error_code`, from one readable and one missing path. | #33 |
| `src/probe/lineage/v2.rs` | `v2`: which `wait_timeout` values Statement Execution accepts, from one `SELECT 1` submit per value. | #33 |
| `src/probe/lineage/v3.rs` | `v3`: materialized-view `table_type` in the scope catalog, and the shape and `pipeline_type` values of `system.lakeflow.pipelines`. | #33 |
| `src/probe/lineage/v11.rs` | `v11`: metric-view `table_type` in the scope catalog, and the `catalog_type` of shared catalogs. | #33 |
| `src/probe/lineage/v9.rs` | `v9`: whether `system.access.table_lineage` attributes pipeline writes by `entity_id`. | #33 |
| `src/probe/lineage/v4.rs` | `v4`: which candidate system tables and pipeline APIs expose refresh history, and their shape. | #33 |
| `src/probe/lineage/v6.rs` | `v6` part A: running `databricks bundle schema`, naming each package that declares table-update triggers, and building the verdict: `jobs`, the jobs packages and `; also: ` the others, or `not jobs: `. | #34 |

### Planned Probe Modules

| Module | Will own | Issue |
| --- | --- | --- |
| `src/probe/lineage/` | The remaining 2 checks from [R4](01-requirements.md#r4-checks), and nothing else. | #37 |
| `src/probe/lineage/sites.rs` | The mapping from each `CheckId` to the code or design sites its answer affects. | #36 |
| `src/probe/report.rs` | `findings.json` and `findings.md` generation and redaction. | #36 |

## Layering Rule

- Reusable modules live at crate level. Only the 15 lineage checks belong under
  `src/probe/lineage/`.
- Nothing outside `src/probe/` may import from `probe` or name `CheckId`. #30
  adds a source-scanning test that enforces this.
- Shared items stay `pub(crate)` unless stronger visibility is required. The
  existing modules already follow this.
- Dependencies point inward toward crate-level modules: probe code may use
  `databricks`, `json`, transport, and the other shared modules, never the
  reverse.

## Transport Boundary

Every workspace operation goes through the resolved `databricks` executable.
There is no direct HTTP client, separate authentication implementation, shell
invocation, or async runtime.

What exists today in `src/databricks.rs` and `src/transport.rs`:

- The executable is `DATABRICKS_CLI_PATH` when set, otherwise `databricks`
  resolved from `PATH`.
- `run_captured` starts the process with arguments passed directly (no shell),
  stdin connected to null, and stdout and stderr captured. Both pipes are
  drained concurrently, and the process is killed if it exceeds the caller's
  timeout.
- Captured output is limited to 64 MiB per stream. Larger output is drained
  and discarded so the process can finish, and the run fails.
- Passthrough (`dbxctl databricks ...`) inherits stdio and returns the upstream
  exit code.
- `CliRunner` (#31) runs every probe call with the bundle root as its working
  directory, then `-t <target>`, and `-p <profile>` when set.
- Statements that `is_read_statement` does not accept are refused before any
  process starts ([R9](01-requirements.md#r9-read-only-by-default)). It
  allows one `SELECT`, `WITH … SELECT`, `SHOW`, or `DESCRIBE` statement, reads
  literals, quoted identifiers, and nested comments as Databricks SQL does,
  and refuses any write keyword outside them.
- Statement Execution is a POST even for a read. Only the submit and cancel
  paths are posted, and only for statements the guard accepted.
- `Authorization` headers, bearer tokens, and `dapi` tokens are scrubbed from
  captured output before it is returned for the evidence record. Full
  redaction is #36.

What is planned:

- Mutating transport methods, if #37 ships, require a `&Mutate` capability
  ([R10](01-requirements.md#r10-mutation-capability-and-cleanup)).

## JSON Boundary

CLI output is untrusted input. All parsing and serialization goes through
`src/json.rs` (DP0-1), which keeps `serde_json` types out of the rest of the
crate. Objects iterate in sorted key order, and serialized output ends with
exactly one newline, which supports
[R13](01-requirements.md#r13-determinism-and-redaction). Lookups and typed
getters return `Option`, so a missing or wrongly typed field is visible to the
caller, which can turn it into an explicit unknown instead of an empty value.

## Lineage Graph

Phase 0 does not build a lineage graph. Phase 1 will use `petgraph` with stable
external node IDs and deterministic ordering, as recorded in
[DP1-1](../design/dp1-1-graph-construction.md). Phase 0 evidence and findings
should not depend on graph types.

## Open Items

- Where workspace and authentication context is recorded (#55, outside the
  Phase 0 contract). The CLI's precedence and the planned checks are in
  [09-auth-context.md](09-auth-context.md).

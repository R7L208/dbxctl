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
| `src/main.rs` | Top-level argument dispatch (`doctor`, `databricks`, `help`, `version`) and process exit. | — |
| `src/databricks.rs` | Resolving the `databricks` executable, captured execution with a timeout, passthrough, the minimum-version check, and `doctor`. | #27, #48 |
| `src/json.rs` | The crate-private JSON layer from DP0-1: parsing into `Document`, borrowed `Node` views, JSON-pointer lookup, and deterministic serialization. No `serde_json` type appears in its API. | #24, #28 |

### Planned Crate-Level Modules

| Module | Will own | Issue |
| --- | --- | --- |
| `src/args.rs` | A small reusable option parser over `OsString`. | #29 |
| `src/exit.rs` | The exit-code table from [R3](01-requirements.md#r3-exit-codes). | #30 |
| `src/evidence.rs` | Run-ID generation, the run directory, raw evidence writes, `run.json`, and the `.dbxctl/.gitignore`. | #30 |
| Transport (path not yet decided) | The `Runner` trait and `CliRunner::for_bundle(root, target, profile)`, captured API GETs, and Statement Execution with polling, pagination, and error classification. | #31 |
| `src/safety.rs` | `is_read_statement` and, if #37 ships, the `Mutate` capability and `safety::grant`. | #31, #37 |
| `src/uc/fqn.rs` | Parsing and formatting Unity Catalog three-part names, including backtick quoting. | #31 |
| `src/text.rs` | SQL string-literal and identifier quoting. | #31 |
| `src/bundle/paths.rs` | Local-path resolution relative to the bundle root and workspace-path forms. | #32 |
| `src/bundle/schema.rs` | Running `databricks bundle schema` and locating table-update trigger definitions. | #34 |

### Planned Probe Modules

| Module | Will own | Issue |
| --- | --- | --- |
| `src/probe.rs` | The `probe` command group and suite dispatch. | #29 |
| `src/probe/lineage.rs` | Lineage suite options from [R2](01-requirements.md#r2-run-options). | #29 |
| `src/probe/model.rs` | `CheckId`, verdict and state types, the finding struct, and the orchestrator that runs selected checks in a fixed order. | #30 |
| `src/probe/lineage/` | The 15 checks from [R4](01-requirements.md#r4-checks), and nothing else. | #30, #32, #33, #34, #37 |
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

What exists today in `src/databricks.rs`:

- The executable is `DATABRICKS_CLI_PATH` when set, otherwise `databricks`
  resolved from `PATH`.
- `run_captured` starts the process with arguments passed directly (no shell),
  stdin connected to null, and stdout and stderr captured. Both pipes are
  drained concurrently, and the process is killed if it exceeds the caller's
  timeout.
- Passthrough (`dbxctl databricks ...`) inherits stdio and returns the upstream
  exit code.

What is planned:

- #31 builds the read-only transport on `run_captured`. Every probe call runs
  with the bundle root as its working directory, `-t <target>`, and
  `-p <profile>` when set.
- Write SQL is refused before any process starts
  ([R9](01-requirements.md#r9-read-only-by-default)).
- Credentials are scrubbed from captured debug output before it is stored.
- Captured output size is not yet bounded; `src/json.rs` documents that #31
  owns this limit.
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

- The transport module's path and exact API (#31).
- Where workspace and authentication context is recorded (#55, outside the
  Phase 0 contract).

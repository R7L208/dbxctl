# MV Lineage

This directory documents the materialized-view (MV) lineage work in `dbxctl`:
what it must do, how it is built, and what has been learned about the
Databricks CLI and workspace along the way. Terminology and identifier
families (`R`, `T`, `V`, `v`, `DP`) are defined in the
[design nomenclature and glossary](../design-nomenclature-and-glossary.md).

> **Status:** Phase 0 is in progress under milestone 0.0.1. The `dbxctl probe`
> command described here is not implemented yet. Pages marked as stubs are
> completed by the issue they name. Tracking issue: #23.

## Purpose

Later phases will build MV lineage features on top of facts that only the
Databricks CLI and workspace can supply: bundle plan and state file shapes,
how Unity Catalog lineage attributes pipeline writes, where refresh history is
exposed, and similar questions. Phase 0 answers those questions with
evidence before any dependent code is written. Anything it cannot confirm is
recorded as an explicit unknown with a stated fallback, never a guess.

## Phases

| Phase | Scope | Status |
| --- | --- | --- |
| 0 | Discovery and de-risking: the `dbxctl probe` lineage suite, its evidence, and a redacted findings report. | In progress (#23) |
| 1 | Build the lineage graph. Graph construction is decided in [DP1-1](design/dp1-1-graph-construction.md). | Not started; remaining scope open |
| 2 | Open: scope not yet defined. | Not started |
| 3 | Open: scope not yet defined. | Not started |
| 4 | Open: scope not yet defined. | Not started |

Phase 0 gates for Phases 1–4 are recorded in
[06-findings-and-handoff.md](phase0/06-findings-and-handoff.md) once #36
defines them.

## Phase 0 Documents

| Document | Content | Completed by |
| --- | --- | --- |
| [00-kit.md](phase0/00-kit.md) | How to run Phase 0 and what you need | #38 |
| [01-requirements.md](phase0/01-requirements.md) | The Phase 0 contract from #23 | #38 |
| [02-architecture.md](phase0/02-architecture.md) | Module ownership, layering rule, transport boundary | #38 |
| [03-bundle-probes.md](phase0/03-bundle-probes.md) | Bundle probes (stub) | #32, #34 |
| [04-sql-probes.md](phase0/04-sql-probes.md) | SQL and API probes (stub) | #33 |
| [05-workspace-probes.md](phase0/05-workspace-probes.md) | Opt-in mutation probes (stub) | #37 |
| [06-findings-and-handoff.md](phase0/06-findings-and-handoff.md) | Report schema, redaction, Phase 1–4 gates (stub) | #36 |
| [07-tests-and-task-plan.md](phase0/07-tests-and-task-plan.md) | Test strategy and work order | #38 |
| [08-cli-findings.md](phase0/08-cli-findings.md) | Live results (stub) | #39 |

## Design Decisions

| ID | Decision | Record |
| --- | --- | --- |
| DP0-1 | Parse and write JSON with `serde_json` behind the crate-private `src/json.rs` API. | [docs/security.md](../security.md#dp0-1-json-handling) |
| DP1-1 | Build the Phase 1 lineage graph in memory with `petgraph`; added when Phase 1 starts, not in Phase 0. | [design/dp1-1-graph-construction.md](design/dp1-1-graph-construction.md) |

## Related Documents

- [Development and testing](../development.md)
- [Security and supply chain](../security.md)
- [CLI 1.13.0 test fixtures](../../tests/fixtures/cli-1.13.0/README.md)

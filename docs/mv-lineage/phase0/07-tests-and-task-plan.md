# Phase 0 Tests and Task Plan

This page describes how Phase 0 is tested and the order its work is done in.
Tasks are the step issues of tracking issue #23. #38 replaces the planned test
list with the actual test inventory.

## Test Strategy

### Principles

- No test contacts a real workspace. Live behavior is checked only by the
  human-run steps (#35, #39).
- Tests must not call `std::env::set_var`; environment values are passed to
  child processes.
- Pre-existing tests keep passing unmodified unless an issue says otherwise.
- Production line coverage stays at or above 90%.

### Existing Test Layers

These exist today and are described in
[Development and testing](../../development.md#test-layers):

- Unit tests in `src/` for argument parsing, Databricks version parsing,
  captured execution, and the JSON layer.
- Wrapper integration tests (`tests/wrapper_contract.rs`) that compile
  `tests/fixtures/fake_databricks.rs` as a native executable and exercise the
  real process boundary.
- The ignored upstream contract test (`tests/upstream_contract.rs`), run
  against the pinned Databricks CLI.
- Synthetic CLI 1.13.0 fixtures in `tests/fixtures/cli-1.13.0/` (#25).

### Planned Test Additions

| Test | Purpose | Issue |
| --- | --- | --- |
| `args.rs` and lineage option unit tests; dispatch, help, and usage-error wrapper tests | Option parsing and exit code 1 | #29 |
| Fake-CLI scenario replay and invocation log | Map argv to stdout, stderr, and exit status without a workspace | #30 |
| `assert_no_mutations(invocation_log)` | Prove read-only runs issue no mutating calls | #30 |
| Layering source scan | No file outside `src/probe/` uses `crate::probe` or `CheckId` | #30 |
| Deterministic clock and run ID | Byte-identical output from identical inputs | #30 |
| SQL safety tests | Refused statements cause zero CLI invocations | #31 |
| FQN and literal quoting tests | UC names and SQL literals round-trip safely | #31 |
| Fixture-driven bundle probe cases | Resolved, unresolved, skipped, and unexpected-shape outcomes per check | #32 |
| `bundle schema` fake-CLI cases and ignored `bundle_schema_trigger_contract` | `v6` part A outcomes, plus the pinned-CLI contract | #34 |
| `sites.rs` heading test | Every code-site entry resolves to a heading in `docs/mv-lineage/` | #36 |
| Determinism, redaction corpus, regeneration, promotion allowlist and refusal | Report safety and stability | #36 |
| `safety::grant` caller scan and mutation scenarios | Unauthorized, happy path, mid-run failure with cleanup, cleanup twice | #37 |

### Standard Checks

Every step runs the checks in
[R15](01-requirements.md#r15-standard-checks).

## Task Plan

| Step | Issue | Runs in parallel with | Blocked by |
| --- | --- | --- | --- |
| 1 | #24 Design decisions (JSON, graph) | #25, #27 | — |
| 2 | #25 CLI 1.13.0 fixtures | #24, #27 | — |
| 3 | #26 Documentation scaffold | #27, #28 | #24 |
| 4 | #27 Databricks process layer | #24, #25, #28 | — |
| 5 | #28 Shared JSON layer | #27 | #24 |
| 6 | #29 `probe` command and arguments | — | #27 |
| 7 | #30 Evidence model, orchestration, minimal `cli` check | — | #25, #28, #29 |
| 8 | #31 Read-only transport, SQL safety, UC identifiers | #34 | #30 |
| 9 | #32 Bundle probes | #33, #34 | #31 |
| 10 | #35 Local live smoke test | #33, #34 | #32 |
| 11 | #33 SQL and API probes | #32, #34 | #31 |
| 12 | #34 Offline bundle-schema check (v6, part A) | #31, #32, #33 | #30 |
| 13 | #36 Reports, redaction, fixture promotion | — | #32, #33, #34 |
| 14 | #37 Opt-in mutation probes (deferrable) | #38 documentation drafting | #36 |
| 15 | #38 Finalize documentation | #37 implementation, if retained | #36; closure also requires #37 or formal deferral |
| 16 | #39 Live validation and Phase 0 close-out | — | #38 |

Step numbers are presentation order, not a substitute for the dependency
columns:

- Start #35 as soon as #32 closes, even if #33 or #34 is still underway.
- #38 documentation may overlap #37, but #38 cannot close until #37 is
  completed or formally deferred.
- Before #38 starts, record the #37 keep-or-defer decision. If deferred, move
  #37 out of milestone 0.0.1 and keep v6 part B, v7, and v10 as explicit
  unknowns with fallbacks.

Outside milestone 0.0.1: #40 (Rust mock workspace server, optional), #41
(workflow security scanning), and #42 (durable release assets).


# Project Status

Last updated: 2026-09-16

## Completed

- Initialized the Rust 2024 CLI with Rust 1.89.0 pinned.
- Added `help`, `version`, `doctor`, and Databricks passthrough commands.
- Added Databricks CLI discovery through `PATH` or `DATABRICKS_CLI_PATH`.
- Added a `doctor` check that enforces Databricks CLI 0.200.0 or newer and
  reports dependency status instead of aborting on the first problem.
- Preserved upstream arguments and exact process exit codes without a shell,
  including signal termination and codes above 255.
- Kept the Rust runtime dependency graph empty.
- Added portable Rust unit, wrapper-contract, and upstream-contract tests.
- Added native Linux, macOS, and Windows CI lanes.
- Added formatting, strict Clippy, compilation analysis, tests, and a 90% line
  coverage gate.
- Added RustSec CVE auditing and cargo-deny policy checks.
- Pinned GitHub Actions by commit SHA and Databricks/tool downloads by SHA-256.
- Pinned Linux CI jobs to a platform-specific OCI image digest.
- Isolated Cargo and downloaded tools in ephemeral CI storage.
- Fixed a workflow startup failure: the workflow-level `env` referenced the
  `runner` context, which is unavailable there, so every run failed at startup
  before this fix. `CARGO_HOME` isolation now runs as a per-job step.

## Verified

Verified locally on the pinned 1.89.0 toolchain. GitHub Actions CI did not run
to completion before the workflow startup fix above, so these results were not
confirmed in CI:

- Linux pinned-container formatting, Clippy, tests, analysis, and coverage pass.
- macOS formatting, Clippy, tests, and analysis pass.
- The Databricks 0.296.0 upstream passthrough contract passes.
- Production coverage is 96.79% lines, 92.70% regions, and 90.62% functions.
- The current lockfile has no known RustSec vulnerability.
- Dependency advisory, ban, license, and source policies pass.
- Every current GitHub Action reference uses a full commit SHA.

## Pending

1. Obtain the first successful GitHub Actions run on all lanes (Linux, macOS,
   and Windows) now that the workflow startup failure is fixed.
2. Verify or eliminate runtime rustup component downloads.
3. Add workflow security scanning and enforce Action SHA policy.
4. Add an SBOM and signed build provenance.
5. Define signed, reproducible release packaging for Linux, macOS, and Windows.
6. Document and automate reviewed updates for tool hashes, the Rust image
   digest, and the Databricks CLI dependency.

No release artifact has been published and no compatibility guarantee has been
declared yet.

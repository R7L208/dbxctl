# Project Status

Last updated: 2026-10-01

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
- Replaced runtime `rustup` component resolution with checksum-pinned Clippy,
  Rustfmt, and LLVM component archives from the dated Rust 1.89.0 release.
- Replaced runtime `rustup` toolchain resolution on macOS and Windows with
  checksum-pinned Cargo, compiler, and standard-library archives for each
  supported runner architecture.
- Added reproducible Linux release packaging with an SPDX JSON SBOM,
  SHA-256 checksum, signed SLSA build provenance, and a signed SBOM attestation.
- Fixed a workflow startup failure: the workflow-level `env` referenced the
  `runner` context, which is unavailable there, so every run failed at startup
  before this fix. `CARGO_HOME` isolation now runs as a per-job step.

## Verified

GitHub Actions CI now runs to completion and passes on every lane. The most
recent `main` run (workflow run `35242658129`, commit `5419917`) reported
success for all four jobs: Linux quality, Linux supply-chain, macOS, and
Windows. The workflow startup fix above cleared the failure that previously
prevented any run from completing.

- Linux pinned-container formatting, Clippy, tests, analysis, and coverage pass
  in CI.
- macOS and Windows native tests and analysis pass in CI.
- The Databricks 0.296.0 upstream passthrough contract passes.
- Production coverage is 96.79% lines, 92.70% regions, and 90.62% functions.
- The current lockfile has no known RustSec vulnerability.
- Dependency advisory, ban, license, and source policies pass.
- Every current GitHub Action reference uses a full commit SHA.

## Pending

1. Add workflow security scanning and enforce Action SHA policy.
2. Extend reproducible release packaging and attestations to macOS and Windows.
3. Publish durable GitHub Release assets and decide whether release binaries
   also require direct signatures in addition to signed digest attestations.
4. Document and automate reviewed updates for tool hashes, the Rust image
   digest, and the Databricks CLI dependency.

No release artifact has been published and no compatibility guarantee has been
declared yet.

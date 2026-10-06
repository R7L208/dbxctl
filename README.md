# dbxctl

`dbxctl` is a small Rust CLI that delegates commands to the official
[Databricks CLI](https://docs.databricks.com/dev-tools/cli/). It keeps
Databricks API behavior in the upstream Go binary while providing a controlled
place for additional workflows and policy. Use `dbxctl doctor` to check that a
supported Databricks CLI is installed; passthrough commands run the upstream
binary directly without a redundant version probe on the hot path.

Phase 0 adds `serde_json` as the first approved third-party Rust runtime
dependency, wrapped by a crate-private JSON layer (`src/json.rs`) for defensive
access and deterministic output; see the recorded
[JSON design decision](docs/security.md#design-decisions). Phase 1 will use
`petgraph` for its in-memory lineage graph. See the
[MV lineage documentation](docs/mv-lineage/README.md) for the Phase 0
requirements, architecture, and plan.

## Requirements

- Databricks CLI 1.13.0 or newer
- Rust 1.89.0 for source builds
- Databricks authentication configured for commands that access a workspace

The pinned Rust toolchain is selected automatically through
`rust-toolchain.toml`.

## Build

```console
cargo build --locked
```

The debug binary is written to `target/debug/dbxctl`. For an optimized binary:

```console
cargo build --release --locked
```

## Usage

Check the installed dependency:

```console
dbxctl doctor
```

Pass a command to the Databricks CLI:

```console
dbxctl databricks current-user me
dbxctl databricks workspace list /
dbxctl databricks jobs list --profile production
```

Arguments are passed directly to the Databricks process without invoking a
shell. The upstream process exit code is returned to the caller.

By default, `dbxctl` resolves `databricks` from `PATH`. Set an explicit binary
when the dependency is installed elsewhere:

```console
DATABRICKS_CLI_PATH=/opt/databricks/bin/databricks dbxctl doctor
```

On PowerShell:

```powershell
$env:DATABRICKS_CLI_PATH = "C:\Tools\databricks.exe"
dbxctl doctor
```

## Supported Platforms

The implementation and Rust test fixture are platform-neutral. CI runs native
tests on Linux, macOS, and Windows. Linux quality and security jobs execute in
an OCI digest-pinned Rust container.

Linux and macOS have been exercised locally. Linux, macOS, and Windows native
test lanes have also completed successfully in GitHub Actions.

## Development

The primary local checks are:

```console
scripts/markdown.sh check
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo check --locked --all-targets --all-features
```

Run `scripts/markdown.sh format` to format every tracked or unignored Markdown
file. The script creates an ignored virtual environment under `target/tools`,
installs the hash-locked formatter dependencies, and reuses that environment
on later runs. Set `PYTHON` to select a Python 3.11 or newer interpreter.

Production line coverage is currently 96.79% on Linux, with a CI minimum of
90%. See [Development and testing](docs/development.md) for coverage and
upstream contract commands.

## Security

CI uses full commit SHAs for GitHub Actions, checksum-pinned analysis tools,
an OCI digest-pinned Linux build image, an ephemeral Cargo home, RustSec CVE
analysis, and `cargo-deny` dependency policy enforcement. Version tags also
produce normalized Linux, macOS, and Windows archives with SPDX JSON SBOMs and
GitHub/Sigstore-signed build provenance and SBOM attestations.

These packages are currently stored as GitHub Actions workflow artifacts,
which expire after 30 days. They are not yet published as durable assets on the
repository's GitHub Releases page.

See [Security and supply chain](docs/security.md) for the trust model and known
gaps, [Release runbook](docs/releases.md) for the operator procedure, and
[Project status](docs/status.md) for completed work and next steps.

## License

Apache-2.0.

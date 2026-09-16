# dbxctl

`dbxctl` is a small Rust CLI that validates and delegates commands to the
official [Databricks CLI](https://docs.databricks.com/dev-tools/cli/). It keeps
Databricks API behavior in the upstream Go binary while providing a controlled
place for additional workflows and policy.

The project currently has no third-party Rust dependencies.

## Requirements

- Databricks CLI 0.200.0 or newer
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

Linux and macOS have been exercised locally. The Windows job is configured but
still requires its first successful GitHub Actions run before Windows support
is considered verified.

## Development

The primary local checks are:

```console
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo check --locked --all-targets --all-features
```

Production line coverage is currently 96.79% on Linux, with a CI minimum of
90%. See [Development and testing](docs/development.md) for coverage and
upstream contract commands.

## Security

CI uses full commit SHAs for GitHub Actions, checksum-pinned analysis tools,
an OCI digest-pinned Linux build image, an ephemeral Cargo home, RustSec CVE
analysis, and `cargo-deny` dependency policy enforcement.

See [Security and supply chain](docs/security.md) for the trust model and known
gaps. See [Project status](docs/status.md) for completed work and next steps.

## License

Apache-2.0. Release packaging and signed artifacts have not been implemented.

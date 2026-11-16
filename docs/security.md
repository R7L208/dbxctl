# Security and Supply Chain

## Current Controls

The Rust application's first and primary third-party runtime dependency is `serde_json`
(version 1.0, approved under DP0-1 below) with transitive dependencies on `itoa`, `memchr`,
`serde`, `serde_core`, and `zmij`. `Cargo.lock` is committed, CI uses `--locked`, and unsafe Rust is forbidden.

The Linux quality and supply-chain jobs run inside this platform-specific OCI
manifest:

```text
rust:1.100.0-bookworm@sha256:198cd28b4ba92b20fe849d5d8b119ace1bd28423d9e82e9c4de5ee1300d453a0
```

This pins the Linux userspace, compiler, Cargo, and base utilities. GitHub
Actions use full commit SHAs, checkout does not persist credentials in build
and release jobs, and jobs use timeouts and concurrency cancellation. Most CI
jobs have read-only tokens. Release jobs additionally receive narrowly scoped
OIDC and attestation permissions. The reviewed pin-update workflow has
`contents: write` and `pull-requests: write` so it can propose, but not merge,
an update. All tool, archive, and image pins live in `.github/pins.json`, which
a `pins` job validates and passes to the other jobs. Because the updater only
edits that file and documentation, its token never needs permission to change
workflow files.

Downloaded executables are treated as inert until their committed SHA-256 is
verified. Current pinned tools are:

| Tool | Version | SHA-256 |
| --- | --- | --- |
| cargo-llvm-cov | 0.9.1 | `b3f68e625481fed9b16444174f3fa5ebcdbde4a1878803a35eabe2dcefcdc41a` |
| cargo-audit | 0.23.0, musl | `4f01c74ed84b2dffd0b1d37012a62cdf2a92c96e25bdf8eec7fcc0d4d3eb7811` |
| cargo-deny | 0.21.0, musl | `a2170b854ff697e79e39c4c538981c9c1bdd803addbae6719a4cb66b4585a903` |
| Databricks CLI | 1.13.0, Linux amd64 | `0a94deffe3c9f1109020c91ac744a25bf45dc833ac302f8192892779e25b3df7` |

The Databricks CLI pin equals the minimum supported version and is not raised
by the automated updater; see [Reviewed Pin Updates](development.md#reviewed-pin-updates).

Markdown formatting uses mdformat 1.0.0 with exact transitive versions and
artifact hashes in `requirements/mdformat.txt`. Installation requires hashes,
accepts binary distributions only, disables transitive resolution and the pip
download cache, and runs inside an ignored local or ephemeral CI virtual
environment. CI pins Python 3.13.7 and the `actions/setup-python` commit.
Dependabot proposes reviewed updates to the formatter lock; it cannot merge
them automatically. Developers on networks that block pypi.org can select a
local mirror (see [Package Index](development.md#package-index)); hashes are
enforced regardless of the index, and no private index URL is committed.

The Linux quality job also downloads the Rust 1.100.0 Clippy, LLVM tools, and
Rustfmt component archives directly from the dated Rust distribution path.
Their SHA-256 values are pinned in `.github/pins.json` and verified before
their installers run. This avoids allowing `rustup component add` to resolve and
download components dynamically at job runtime. The job also selects the
already-installed, fully qualified Rust toolchain through `RUSTUP_TOOLCHAIN`,
so the repository's toolchain file cannot trigger implicit component setup.

| Rust component | Archive SHA-256 |
| --- | --- |
| Clippy | `0d18df3bff6c852ef8243727fed075ecb33dbf93b8ab8c138c2f45e3f270102a` |
| LLVM tools | `2c84cada1aef73339b1a2d101a35cd78d71072c8f6a3921bf1abb852c3f42ad3` |
| Rustfmt | `4c9389e4722b16499e5d47d89837ec84a4ef755f105ed2a84c038156064e01ab` |

The macOS and Windows portability jobs similarly install Cargo, `rustc`, and
the target standard library from dated Rust 1.100.0 standalone archives rather
than asking `rustup` to resolve the minimal toolchain. `.github/pins.json`
holds the archive SHA-256 values for macOS Arm64, macOS x86-64, and Windows
x86-64, and the workflows reject runner architectures without an explicit
checksum set. Each job
also asserts the installed release and host target before running project
commands.

| Target | Cargo SHA-256 | rustc SHA-256 | standard library SHA-256 |
| --- | --- | --- | --- |
| `aarch64-apple-darwin` | `7e18265d2d2cda8dec6cac4d0faa3bbafe927b401e5185350abc7d669f9adafb` | `3fcd7f1709f62dda58bf4db2dcfd630a451e750aadaaa551a21cc4a627a37b4c` | `9345645b6f56eea28bf8cc2c0f628a61d507a688f91c1bec5ae6bfad216a16c7` |
| `x86_64-apple-darwin` | `b597b59d15b19469b8ee19d2800217d8946511e4a8d930ff839bf7688b15a7c6` | `d788934c1fdca7f59217bc4de90641b80cc20e4764b449aaa2dda96392378c63` | `1cc179e8e58af18b6fcfc095065a75fb6ed9afb9e33dc7fd596551cf755efe9f` |
| `x86_64-pc-windows-msvc` | `efb63c35df751dbd58b7aa73d6a43d75b00bd356bfdb925901e85379d0de108a` | `4f1f70145ab94e1036b6a9fb0a1fa1393352999701699aedab4dd2724047149e` | `24ad2f28791e08a573b9c7df5efa3ace884eecd1fa7e7f5b62c65f4b49bb4532` |

Tools and Cargo state are installed under the job's ephemeral `RUNNER_TEMP`.
They are not restored from shared caches or written into the repository.
`RUSTUP_DIST_SERVER` points to a reserved, non-resolving domain throughout the
workflow so an accidental future `rustup` operation fails closed instead of
downloading unpinned content. Both Linux container jobs explicitly select the
toolchain already present in the digest-pinned image.

## Release Evidence

Pushing a version tag such as `v0.1.0` builds locked native packages for Linux
x86-64, macOS Arm64 or x86-64 (matching the hosted runner), and Windows x86-64.
The tag must exactly match the version in `Cargo.toml`. Each job packages the
binary with its license and Cargo manifests, records the archive's SHA-256
digest, and generates an SPDX JSON SBOM with checksum-pinned Syft 1.57.2.

All platforms use the same package layout and normalized `tar.gz` format.
Archive entry order, timestamps, ownership, user names, and gzip metadata are
fixed. Builds disable Cargo incremental compilation and remap the checkout
path; macOS disables the random Mach-O UUID and Windows enables the linker's
reproducible-build mode. CI packages the same native debug binary twice and
rejects different archive bytes. This tests deterministic packaging for a
fixed binary; the project does not yet compare binaries from independent
builds or claim bit-for-bit reproducible native builds.

GitHub Artifact Attestations create two Sigstore-signed, tamper-evident records:

- SLSA build provenance binding the workflow identity and source revision to
  the release archive digest; and
- an SBOM attestation binding the SPDX document to that same archive digest.

After downloading an archive, verify both records with GitHub CLI:

```console
gh attestation verify dbxctl-0.1.0-x86_64-unknown-linux-gnu.tar.gz \
  --repo R7L208/dbxctl
gh attestation verify dbxctl-0.1.0-x86_64-unknown-linux-gnu.tar.gz \
  --repo R7L208/dbxctl --predicate-type https://spdx.dev/Document/v2.3
sha256sum --check dbxctl-0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

The workflow uploads each archive, checksum, and standalone SBOM only as a
GitHub Actions workflow artifact. Those artifacts expire after 30 days and do
not appear as durable downloads on the repository's GitHub Releases page.
Publishing durable GitHub Release assets remains a separate release-management
task. The macOS and Windows system linkers and hosted runner images remain
mutable trust boundaries even though the Rust compiler, standard library, and
Cargo archives are checksum-pinned.

See the [Release runbook](releases.md) for version preparation, tag creation,
workflow monitoring, cross-platform verification, and failure recovery.

## Analysis

`cargo-audit` checks the committed lockfile against the live RustSec advisory
database. Keeping the database live prioritizes timely CVE detection over
pinning advisory data to a stale commit.

`cargo-deny` rejects unknown registries, Git dependencies, wildcard versions,
unapproved licenses, and known advisories. Clippy and the compiler treat code
warnings as errors.

The most recent container validation loaded 1,246 RustSec advisories, found no
vulnerabilities, and passed the advisory, ban, license, and source policies.

## Design Decisions

### DP0-1: JSON handling

Phase 0 will use `serde_json`, wrapped by a crate-private `json.rs` API. CLI
output is untrusted input, and using the mature parser avoids owning a custom
implementation of Unicode escapes, number parsing, nesting limits, malformed
input handling, and deterministic serialization. Callers must not depend on
serde types directly; the wrapper remains the boundary for defensive access
and stable output ordering.

This is the first approved third-party Rust runtime dependency. It must be
version-locked in `Cargo.lock`, come from the allowed crates.io registry, pass
`cargo audit` and `cargo deny check`, and use an approved license. The rejected
alternative was a standard-library-only parser: it preserved the empty
dependency graph but created substantially more parser and maintenance risk.
Revisit this choice if the dependency cannot satisfy the project's audit,
license, source, or supported-Rust-version policies.

### Passthrough Interrupts

`dbxctl databricks ...` must let the Databricks CLI handle Ctrl-C itself. A
command such as `bundle deploy` may need to release a deployment lock after an
interrupt, and the caller should get the CLI's result, not the wrapper's.

On Unix, passthrough replaces the `dbxctl` process with the CLI through the
standard library's `CommandExt::exec`, which is safe Rust and adds no
dependency. The CLI keeps the PID, process group, terminal, and inherited
streams, so it is the only process to receive `SIGINT`, and its exit code or
terminating signal is the caller's result. If the CLI cannot be started,
`exec` returns and `dbxctl` reports the error and exits non-zero. `doctor` and
captured execution still run the CLI as a child, because they must inspect its
output after it exits.

Windows has no `exec`, and its console delivers Ctrl-C to every attached
process. There, `dbxctl` still runs the CLI as a child and exits with its exit
code. The accepted limitation is that on Ctrl-C `dbxctl` may exit, and return
the prompt, before the CLI finishes its own cleanup. The rejected alternative
was ignoring Ctrl-C in `dbxctl` while the child runs. That requires a console
control handler, which means either `unsafe` FFI, which the crate forbids, or
a new runtime dependency such as `ctrlc`, which needs its own reviewed design
decision. Neither cost is justified for an interrupt-timing limitation that
does not change normal exit codes. Revisit this choice if Windows users depend
on interrupt-safe passthrough.

## Remaining Trust Boundaries

- GitHub's `ubuntu-24.04` runner remains the Docker host.
- Native macOS and Windows runner images and system linkers are mutable. The
  `macos-15` and `windows-2025` labels select an OS generation, but GitHub does
  not expose digest-pinned hosted images; removing this boundary would require
  controlled self-hosted runners.
- The live RustSec database is trusted as security data.
- The Databricks CLI alone decides which workspace and credentials a probe
  uses. Probes pass a bundle target, so the target's host
  silently overrides `DATABRICKS_HOST`, the CLI does not report where the token
  came from, and it can pick a host-matched profile without saying which.
  Without a target or profile, a `[DEFAULT]` profile is used silently. A
  profile named with `-p` also silently overrides `DATABRICKS_HOST` and
  `DATABRICKS_TOKEN`, and `auth describe` exits 0 even when authentication
  fails. `probe run` does not yet record or check the resolved context (#55);
  see [Workspace and Auth Context](mv-lineage/phase0/09-auth-context.md).
- There is no automated workflow security scanner or policy preventing a
  future unpinned Action.
- Release archives and SBOMs are not yet published as durable GitHub Release
  assets; workflow artifacts expire after 30 days.
- Release binaries are not signed in their platform-native formats. Each
  package digest is covered by signed provenance, which requires an
  attestation-aware verifier.
- The pin-update workflow downloads new upstream artifacts and calculates their
  hashes before opening a review PR. Reviewers must validate the upstream
  release identity and CI results; a checksum calculated from a compromised
  upstream artifact does not independently establish its trustworthiness.

These gaps should be addressed individually so each trust decision and update
mechanism can be reviewed independently.

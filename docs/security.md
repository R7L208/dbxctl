# Security and Supply Chain

## Current Controls

The Rust application's first and primary third-party runtime dependency is `serde_json`
(version 1.0, approved under DP0-1 below) with transitive dependencies on `itoa`, `memchr`,
`serde_core`, and `zmij`. `Cargo.lock` is committed, CI uses `--locked`, and unsafe Rust is forbidden.

The Linux quality and supply-chain jobs run inside this platform-specific OCI
manifest:

```text
rust:1.99.0-bookworm@sha256:b36c246742b4d323472588f789601be34eb4af053255e2af53969078ee8dcec7
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
| cargo-audit | 0.22.2, musl | `7fb9497f8594b389e5fce5ef9b92db08432996895b2e0c5a0167a69ed445c428` |
| cargo-deny | 0.20.2, musl | `9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f` |
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

The Linux quality job also downloads the Rust 1.99.0 Clippy, LLVM tools, and
Rustfmt component archives directly from the dated Rust distribution path.
Their SHA-256 values are pinned in `.github/pins.json` and verified before
their installers run. This avoids allowing `rustup component add` to resolve and
download components dynamically at job runtime. The job also selects the
already-installed, fully qualified Rust toolchain through `RUSTUP_TOOLCHAIN`,
so the repository's toolchain file cannot trigger implicit component setup.

| Rust component | Archive SHA-256 |
| --- | --- |
| Clippy | `982442e32ad8dd3f0bb3a30db74038da5f71fd9bf1a865cc71da8ea5d4ec71ec` |
| LLVM tools | `91a6db5668c08ee68e330e5b0fda27c11b79b90592627e89f7c5db12af2ff0ba` |
| Rustfmt | `b22c09ab9e258ec5571da170d88bd1624a4ea602e6d70d95720c5b47494cadce` |

The macOS and Windows portability jobs similarly install Cargo, `rustc`, and
the target standard library from dated Rust 1.99.0 standalone archives rather
than asking `rustup` to resolve the minimal toolchain. `.github/pins.json`
holds the archive SHA-256 values for macOS Arm64, macOS x86-64, and Windows
x86-64, and the workflows reject runner architectures without an explicit
checksum set. Each job
also asserts the installed release and host target before running project
commands.

| Target | Cargo SHA-256 | rustc SHA-256 | standard library SHA-256 |
| --- | --- | --- | --- |
| `aarch64-apple-darwin` | `76abff0ad79a10a3152dff640ea0d8b3c37637d9300bf55da41b95c3ddc069c1` | `334a66714ca316d71bbe5efe44f71762d0b276cea73be2987374693a837f7450` | `a7c6de8aa21e7c31163a7656295b321bb85f5dce55ed8ea95ac9ac561202bb5b` |
| `x86_64-apple-darwin` | `939b71952afd63ae73bc00f3517c19f0dd7ac54884b5fac16a813ee50985d14d` | `7460910254f059b49b99ee3ddbcf7197d16b9505b59c166e14a1465ee323a81d` | `18ee81961a73c3ae966ecb1e8f6096438c90aeefece8856201a6ecc17f5bc850` |
| `x86_64-pc-windows-msvc` | `6201b8dcea8cc55ccfa12e842adf1b2c2aa96490a6cb4f129bf03f2df08c80c9` | `20fed82e629c1ed145f9e9bdee9aad65e790d4237a49bcfc61221eda69e35b14` | `adadeafff137a7610884696912b853d4de259c4299ab035b66661e2e0f923ff0` |

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
digest, and generates an SPDX JSON SBOM with checksum-pinned Syft 1.54.1.

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

## Remaining Trust Boundaries

- GitHub's `ubuntu-24.04` runner remains the Docker host.
- Native macOS and Windows runner images and system linkers are mutable. The
  `macos-15` and `windows-2025` labels select an OS generation, but GitHub does
  not expose digest-pinned hosted images; removing this boundary would require
  controlled self-hosted runners.
- The live RustSec database is trusted as security data.
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

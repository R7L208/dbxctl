# Security and Supply Chain

## Current Controls

The Rust application has no third-party crate dependencies. `Cargo.lock` is
committed, CI uses `--locked`, and unsafe Rust is forbidden.

The Linux quality and supply-chain jobs run inside this platform-specific OCI
manifest:

```text
rust:1.89.0-bookworm@sha256:c9ac3fa8945b61dede1e4500d25028aa8fd8a8fe46365fcf9c0422f8d999b9b0
```

This pins the Linux userspace, compiler, Cargo, and base utilities. GitHub
Actions use full commit SHAs, checkout does not persist credentials in build
and release jobs, and jobs use timeouts and concurrency cancellation. Most CI
jobs have read-only tokens. Release jobs additionally receive narrowly scoped
OIDC and attestation permissions. The reviewed pin-update workflow has
`contents: write` and `pull-requests: write` so it can propose, but not merge,
an update.

Downloaded executables are treated as inert until their committed SHA-256 is
verified. Current pinned tools are:

| Tool | Version | SHA-256 |
| --- | --- | --- |
| cargo-llvm-cov | 0.9.1 | `b3f68e625481fed9b16444174f3fa5ebcdbde4a1878803a35eabe2dcefcdc41a` |
| cargo-audit | 0.22.2, musl | `7fb9497f8594b389e5fce5ef9b92db08432996895b2e0c5a0167a69ed445c428` |
| cargo-deny | 0.20.2, musl | `9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f` |
| Databricks CLI | 0.296.0, Linux amd64 | `cd9fa9748878f35d3c1cdf6b99ac285ce3124117b8839b653a9513b2e87820e6` |

Markdown formatting uses mdformat 1.0.0 with exact transitive versions and
artifact hashes in `requirements/mdformat.txt`. Installation requires hashes,
accepts binary distributions only, disables transitive resolution and the pip
download cache, and runs inside an ignored local or ephemeral CI virtual
environment. CI pins Python 3.13.7 and the `actions/setup-python` commit.
Dependabot proposes reviewed updates to the formatter lock; it cannot merge
them automatically.

The Linux quality job also downloads the Rust 1.89.0 Clippy, LLVM tools, and
Rustfmt component archives directly from the dated Rust distribution path.
Their SHA-256 values are pinned in the workflow and verified before their
installers run. This avoids allowing `rustup component add` to resolve and
download components dynamically at job runtime. The job also selects the
already-installed, fully qualified Rust toolchain through `RUSTUP_TOOLCHAIN`,
so the repository's toolchain file cannot trigger implicit component setup.

| Rust component | Archive SHA-256 |
| --- | --- |
| Clippy | `c6c362c6cd74567022e9ba0c16f6676f8c2b73d955adcf1f6f4c51cf15e57ce8` |
| LLVM tools | `bb0ced899fd1ac628f26e375adabc970b415c516ec82fb7cf9ac3c63cb4d0fee` |
| Rustfmt | `540eb7adf43e37b22936f981c630b10c63915f64f3c227d981a8b592ece33430` |

The macOS and Windows portability jobs similarly install Cargo, `rustc`, and
the target standard library from dated Rust 1.89.0 standalone archives rather
than asking `rustup` to resolve the minimal toolchain. The workflow contains
the archive SHA-256 values for macOS Arm64, macOS x86-64, and Windows x86-64
and rejects runner architectures without an explicit checksum set. Each job
also asserts the installed release and host target before running project
commands.

| Target | Cargo SHA-256 | rustc SHA-256 | standard library SHA-256 |
| --- | --- | --- | --- |
| `aarch64-apple-darwin` | `545517d16ac76789aa6ce801cbc3eeecc9acaf43f3ccb63148c3577f2bb4b8d3` | `6d2cf6164bef00ff3d2c37ca0a0658ffb7c9c3178882a72d78e35abeba888860` | `1f729f8ba21725618ab894f14cc38f01470f1d15ea76a81fac2da63291bed75c` |
| `x86_64-apple-darwin` | `81fabf0d783af844c7dd74dfe10d0302dd063775789a914f29b33e3d46ee1cf0` | `04f3acf7ddfb998fa2713226fd8528e6157b9030f9a6ac6678133d82d5c099f9` | `09780642e83b12085500ea78dcb46112a546467352cc4a4dd229f22e03d4a5f0` |
| `x86_64-pc-windows-msvc` | `8c0a40a5411746ff6600d93acd16652fef56702196bb25b940ff399d7e107f40` | `76f70bbd3dc8681ee189931bc5e270cd9524ff3d17738d87f609bba4980c466c` | `ba81500406fdf8a3a81078df2129bd6e4793b579c9e1a498563670bccad92611` |

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
digest, and generates an SPDX JSON SBOM with checksum-pinned Syft 1.52.0.

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

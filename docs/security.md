# Security and Supply Chain

## Current Controls

The Rust application's first and primary third-party runtime dependency is `serde_json`
(version 1.0, approved under DP0-1 below) with transitive dependencies on `itoa`, `memchr`,
`serde`, `serde_core`, and `zmij`. `Cargo.lock` is committed, CI uses `--locked`, and unsafe Rust is forbidden.

The Linux quality and supply-chain jobs run inside this platform-specific OCI
manifest:

```text
rust:1.99.1-bookworm@sha256:8bf7188ae46ad73b801c8d405eedc13ce63a7852b562885743a56890723549a8
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

The Linux quality job also downloads the Rust 1.99.1 Clippy, LLVM tools, and
Rustfmt component archives directly from the dated Rust distribution path.
Their SHA-256 values are pinned in `.github/pins.json` and verified before
their installers run. This avoids allowing `rustup component add` to resolve and
download components dynamically at job runtime. The job also selects the
already-installed, fully qualified Rust toolchain through `RUSTUP_TOOLCHAIN`,
so the repository's toolchain file cannot trigger implicit component setup.

| Rust component | Archive SHA-256 |
| --- | --- |
| Clippy | `458339712a058e52cc4522ff9ef8b59cd1ea1340312216cd94194c464a3f6dc5` |
| LLVM tools | `8429c5117debfb45c22cf2f97712ef00e0e94976c96cf92f661974a458ca7e84` |
| Rustfmt | `e3b36c4c7ec3486c059d8067ff48787676612007123aad7a4e8ec12c48ee9ba2` |

The macOS and Windows portability jobs similarly install Cargo, `rustc`, and
the target standard library from dated Rust 1.99.1 standalone archives rather
than asking `rustup` to resolve the minimal toolchain. `.github/pins.json`
holds the archive SHA-256 values for macOS Arm64, macOS x86-64, and Windows
x86-64, and the workflows reject runner architectures without an explicit
checksum set. Each job
also asserts the installed release and host target before running project
commands.

| Target | Cargo SHA-256 | rustc SHA-256 | standard library SHA-256 |
| --- | --- | --- | --- |
| `aarch64-apple-darwin` | `685e563d94db1f003ffa5f5831439eec02569b9950b69867953c8213269c1eaf` | `23948a0a9c162fd2952defb3fb0462c44e8fcfd1a4ab8a274a1e1bdb2cf810a3` | `fe79d6ab5a0321de26463999d8c112b8ad3eafbcbcf822d66a2300b45dde1031` |
| `x86_64-apple-darwin` | `6636dabcec164909d1c6eb9e2c596148787c84f7d3a02ff68a141569077f80b5` | `203feef2c2165d7dda1f30ccf75590f72a6abc52ce259ed1c418de84a6887698` | `484d5b88b424ddd29df196e1eb9420c8335bb5fac90a3c37140253345a8c65c4` |
| `x86_64-pc-windows-msvc` | `4adb46c3707dee790682f7769d86983841148111695f561c843c33daa2af0c9b` | `7da83c2af1f209606b0160f25174d2867c7c7b73667c32c9e5fbcef4f5f45348` | `4fe09430ded40b8b3092b49f0a651178eb1f8154ef4d1f1a73f2a3595b2d956f` |

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
digest, and generates an SPDX JSON SBOM with checksum-pinned Syft 1.55.0.

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

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
Actions use full commit SHAs, token permissions are read-only, checkout does
not persist credentials, and jobs use timeouts and concurrency cancellation.

Downloaded executables are treated as inert until their committed SHA-256 is
verified. Current pinned tools are:

| Tool | Version | SHA-256 |
| --- | --- | --- |
| cargo-llvm-cov | 0.9.1 | `b3f68e625481fed9b16444174f3fa5ebcdbde4a1878803a35eabe2dcefcdc41a` |
| cargo-audit | 0.22.2, musl | `7fb9497f8594b389e5fce5ef9b92db08432996895b2e0c5a0167a69ed445c428` |
| cargo-deny | 0.20.2, musl | `9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f` |
| Databricks CLI | 0.296.0, Linux amd64 | `cd9fa9748878f35d3c1cdf6b99ac285ce3124117b8839b653a9513b2e87820e6` |

Tools and Cargo state are installed under the job's ephemeral `RUNNER_TEMP`.
They are not restored from shared caches or written into the repository.

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
- Native macOS and Windows runner images are mutable.
- Rustfmt, Clippy, and LLVM components are downloaded by rustup at job runtime.
- The live RustSec database is trusted as security data.
- There is no automated workflow security scanner or policy preventing a
  future unpinned Action.
- Releases do not yet include an SBOM, signed provenance, attestations, or
  signed binaries.
- No release workflow or controlled dependency-update procedure exists yet.

These gaps should be addressed individually so each trust decision and update
mechanism can be reviewed independently.


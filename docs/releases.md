# Release Runbook

This project currently produces signed release evidence and downloadable
GitHub Actions workflow artifacts. These files are attached to an individual
workflow run, expire after 30 days, and do not appear on the repository's
GitHub Releases page. The project does not yet publish durable GitHub Release
assets or platform-native signed binaries.

## Prerequisites

- Write access to the repository and permission to create version tags.
- A clean checkout of the latest `main` branch.
- A successful CI run for the commit that will be tagged.
- GitHub CLI for downloading and verifying artifacts.

Repository administrators should protect version tags matching `v*` so only
authorized release maintainers can create them. GitHub Actions must be allowed
to issue artifact attestations through the workflow's `id-token: write` and
`attestations: write` permissions.

## Prepare the Version

Choose a semantic version without the leading `v`, such as `0.2.0`. On a
feature branch, update the `version` field in `Cargo.toml`, then refresh and
verify the lockfile:

```console
cargo check
cargo check --locked --all-targets --all-features
cargo test --locked --all-features
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*.py' -v
```

Commit both `Cargo.toml` and `Cargo.lock` if the lockfile changed. Open a pull
request, obtain review, and wait for every required CI job to pass before
merging. Do not tag the feature branch or an unreviewed commit.

## Create the Release Tag

Start from a clean, current `main` branch and confirm the package version:

```console
git switch main
git pull --ff-only origin main
git status --short
cargo metadata --no-deps --format-version 1
```

Create one annotated tag whose version exactly matches `Cargo.toml`, then push
only that tag. For version `0.2.0`:

```console
git tag -a v0.2.0 -m "Release v0.2.0"
git push origin v0.2.0
```

The tag push starts the `Release artifacts` workflow. It builds Linux x86-64,
the native architecture of the `macos-15` runner, and Windows x86-64. Each job
uploads an archive, SHA-256 file, and SPDX JSON SBOM, and records signed build
provenance and an SBOM attestation for the archive digest.

## Monitor and Download

Find the workflow run and wait for all three platform jobs to succeed:

```console
gh run list --workflow release.yml --limit 10
gh run watch RUN_ID --exit-status
gh run download RUN_ID --dir release-artifacts
```

Do not distribute artifacts from a partially successful run. The downloaded
files are Actions artifacts and will expire; retaining them as durable GitHub
Release assets is not automated yet.

## Verify Release Evidence

For every `.tar.gz` archive, first verify its adjacent checksum from the
directory containing both files.

On Linux:

```console
sha256sum --check dbxctl-0.2.0-x86_64-unknown-linux-gnu.tar.gz.sha256
```

On macOS:

```console
shasum -a 256 --check dbxctl-0.2.0-aarch64-apple-darwin.tar.gz.sha256
```

On PowerShell:

```powershell
$line = Get-Content .\dbxctl-0.2.0-x86_64-pc-windows-msvc.tar.gz.sha256
$expected = ($line -split '\s+')[0].ToLowerInvariant()
$actual = (Get-FileHash .\dbxctl-0.2.0-x86_64-pc-windows-msvc.tar.gz -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actual -ne $expected) { throw "SHA-256 mismatch" }
```

Then verify both GitHub/Sigstore attestations for each archive:

```console
gh attestation verify ARCHIVE.tar.gz --repo R7L208/dbxctl
gh attestation verify ARCHIVE.tar.gz --repo R7L208/dbxctl \
  --predicate-type https://spdx.dev/Document/v2.3
```

Confirm that the verified subject digest matches the archive and that the
attestation identifies this repository, the expected tag commit, and
`.github/workflows/release.yml`. Retain the standalone SPDX file with its
corresponding archive.

## Failure and Recovery

- If a job fails, preserve its logs and fix the cause through a reviewed pull
  request.
- Never move or overwrite a published version tag. Tag movement breaks the
  relationship between source identity, provenance, and user expectations.
- After fixing a tagged build, increment the package version and create a new
  tag rather than reusing the failed tag.
- If a tag was created from the wrong commit, do not distribute its artifacts.
  Record the mistake and issue a new version from the correct commit.
- A successful workflow proves the recorded archive was produced by the
  identified GitHub workflow. It does not make mutable hosted runner images or
  native system linkers independently reproducible.

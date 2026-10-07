# Development and Testing

## Toolchain

Rust 1.89.0 is pinned in `rust-toolchain.toml` with Clippy, Rustfmt, and LLVM
coverage tools. Cargo builds must use the committed lockfile:

```console
cargo build --locked
```

The crate forbids unsafe Rust and enables the Clippy `all` and `pedantic` lint
groups at deny level.

## Markdown

Markdown is formatted with mdformat 1.0.0. The local formatting loop installs
the exact transitive dependency set from `requirements/mdformat.txt` into an
ignored virtual environment:

```console
scripts/markdown.sh format
scripts/markdown.sh check
```

The first invocation requires Python 3.11 or newer and network access to the
configured Python package index. Installs require hashes and binary wheels;
subsequent runs reuse `target/tools/mdformat`. CI uses Python 3.13.7 and checks
all tracked Markdown files without modifying them. Local runs also include
unignored, untracked Markdown so new documentation is covered before staging.

### Package Index

The formatter installs from pypi.org by default. Where pypi.org is blocked, use
one of these, in order of precedence:

1. `PIP_INDEX_URL`, which pip honors directly. When it is set, the script uses
   it unconditionally and skips the fallback below.
1. `DBXCTL_PYPI_PROXY`, an index URL ending in `/simple/`, set in the
   environment.
1. The same `DBXCTL_PYPI_PROXY=<url>` line in a git-ignored `.dbxctl.local`
   file at the repository root. Only that key is read; the file is not
   sourced.

```text
# .dbxctl.local (not committed)
DBXCTL_PYPI_PROXY=https://<your-internal-pypi-index>/simple/
```

A configured proxy is a fallback, not an override: the script checks pypi.org
with a three-second timeout and uses the proxy only if pypi.org is unreachable.
Without a proxy, no check runs and behavior is unchanged, which keeps CI on the
public index. The proxy URL is never printed, so it may contain credentials.

Whichever index is used, every file must match a hash in
`requirements/mdformat.txt` (or `requirements/zizmor.txt` for the workflow
scanner), so a mirror cannot substitute different packages.

## Workflow Scanning

GitHub Actions workflows and the Dependabot configuration are scanned with
zizmor 1.30.1, installed from the hash-locked wheels in
`requirements/zizmor.txt` into `target/tools/zizmor`:

```console
scripts/workflows.sh check
```

Like the Markdown script, it requires Python 3.11 or newer, uses the same
[package index](#package-index) selection, and exits nonzero on any finding.
Without `GH_TOKEN` it runs with `--offline`, which skips the audits that query
the GitHub API, such as `impostor-commit`, `known-vulnerable-actions`, and
`ref-confusion`. To run the full CI scan locally, export a token:

```console
GH_TOKEN="$(gh auth token)" scripts/workflows.sh check
```

The `Workflow security` CI workflow runs the same script on any change to
`.github/`, `requirements/zizmor.txt`, or `scripts/workflows.sh`. Every
`uses:` reference must be pinned to a full commit SHA with a version comment,
for example `actions/checkout@<40-hex SHA> # v7.0.1`; a tag or branch fails the
scan. Pass upstream values (step outputs, event fields) to `run:` scripts
through `env:` rather than `${{ ... }}` in the script body. If a finding must be
accepted, add `# zizmor: ignore[<audit>]` with a one-line reason on the flagged
line; see [Workflow Scanning](security.md#workflow-scanning) for the policy.

## Test Layers

Unit tests cover argument parsing and Databricks version parsing. Wrapper
integration tests compile `tests/fixtures/fake_databricks.rs` as a native Rust
executable and exercise the real process boundary. They cover:

- exact argument forwarding, including spaces and option values;
- upstream exit-code preservation;
- missing and failing executables;
- malformed and non-UTF-8 version output;
- minimum supported Databricks CLI enforcement; and
- wrapper commands that do not require Databricks.

Run them with:

```console
cargo test --locked --all-features
```

The upstream contract test compares output, stderr, and exit status from the
pinned Databricks CLI with the same command passed through `dbxctl`. It covers
version and help surfaces for API, authentication, bundles, configuration,
identity, filesystem, jobs, sync, and workspace commands.

```console
DATABRICKS_CLI_PATH=/path/to/pinned/databricks \
  cargo test --locked --test upstream_contract -- --ignored
```

A fake-binary failure normally identifies a wrapper regression. A difference
between direct and wrapped execution identifies a passthrough regression. A
matching behavior change after updating the pinned dependency identifies an
upstream change.

## Coverage

CI measures production Rust code and excludes test harness sources:

```console
cargo llvm-cov --locked --all-features --workspace \
  --ignore-filename-regex '(/rustc/|/tests/)' \
  --fail-under-lines 90
```

The most recent Linux measurement was:

| Metric | Coverage |
| --- | ---: |
| Lines | 96.79% |
| Regions | 92.70% |
| Functions | 90.62% |

Coverage is evidence that code executed, not proof that all behavior is
correct. Process-contract assertions remain the primary compatibility signal.

The supply-chain pin updater has a separate standard-library-only Python test
suite. CI uses the pinned Python 3.13.7 interpreter and requires at least 95%
line coverage of `scripts/update-pins.py`:

```console
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*.py' -v
```

## Reviewed Pin Updates

The `Update supply-chain pins` workflow runs every Monday and supports manual
dispatch. It discovers stable upstream releases, calculates artifact hashes and
the Linux x86-64 Rust image digest, runs the updater tests, and opens or refreshes
one reviewable pull request. It never merges an update automatically.

Every pinned version, download checksum, and the Rust image digest live in
`.github/pins.json`. A `pins` job in the CI and release workflows validates the
file (`scripts/update-pins.py --emit-github-output`) and passes the values to
the other jobs, which still verify each download against its checksum. The
updater edits only `pins.json`, `rust-toolchain.toml`, and the documentation
tables, never `.github/workflows/`. The update workflow fails if a workflow
file changes, and a test rejects any pinned value or checksum written into a
workflow file. Action references (`uses: owner/action@<sha>`) must stay in the
workflows; Dependabot updates them.

The Databricks CLI is the exception: its pin is held at 1.13.0, which is also
the minimum version `dbxctl doctor` accepts, so CI tests exactly the oldest
supported release. Discovery reports the version already pinned in
`.github/pins.json` instead of the latest release, and the updater only
re-verifies its checksum. To raise the supported version, change these together
in one reviewed pull request:

1. `MINIMUM_DATABRICKS_VERSION` in `src/databricks.rs` and its boundary test.
1. The version and SHA-256 under `databricks_cli` in `.github/pins.json`, and
   the tool table in `docs/security.md` (`scripts/update-pins.py --databricks <version>` rewrites both).
1. The fake CLI's reported versions in `tests/fixtures/fake_databricks.rs`, the
   version assertions in the tests, and the requirement in `README.md`.

Configure a fine-grained `PIN_UPDATE_TOKEN` Actions secret with repository
Contents and Pull requests read/write permissions. It needs no Workflows
permission, because updates never touch workflow files. Pull requests created
with that bot token trigger normal CI. Without the secret, the workflow falls back
to `GITHUB_TOKEN`; GitHub permits the PR but suppresses workflows triggered by
that token, so a maintainer must manually trigger CI before merging.

Version discovery and update generation remain independently runnable:

```console
scripts/discover-pin-versions.py
scripts/update-pins.py --help
```

## CI Platforms

- Linux: Rust and Markdown formatting, Clippy, tests, coverage, analysis,
  upstream contract, RustSec audit, dependency policy, and workflow scanning
- macOS: native tests and analysis
- Windows: native tests and analysis

All three platform lanes have completed successfully in GitHub Actions. Native
macOS and Windows builds still inherit their hosted runner images and system
linkers as mutable trust boundaries.

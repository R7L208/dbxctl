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
`requirements/mdformat.txt`, so a mirror cannot substitute different packages.

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

### Workspace Integration Tests

The workspace integration tests run the pinned Databricks CLI through
`dbxctl` against a live workspace. They check that authenticated passthrough
matches direct execution, create and remove a Unity Catalog schema and table,
and round-trip a workspace file.

Every resource is named from `DBXCTL_IT_PREFIX`, which must match
`dbxctl_it_[a-z0-9_]+`. Schemas are created in `DBXCTL_IT_CATALOG` (default
`workspace`) and files under `/Workspace/Users/<identity>/dbxctl-it/`. Each
test deletes its own resources even when an assertion fails, and then confirms
they are gone.

Credentials are never passed to the tests. The Databricks CLI resolves them
from its environment or profile. Locally, use an OAuth login so no token is
stored in a configuration file:

```console
databricks auth login --host https://<workspace-host> --profile dbxctl-it
DATABRICKS_CONFIG_PROFILE=dbxctl-it \
  DATABRICKS_CLI_PATH="$(command -v databricks)" \
  DBXCTL_IT_PREFIX="dbxctl_it_local_$(date +%s)" \
  DBXCTL_IT_WAREHOUSE_ID=<warehouse-id> \
  cargo test --locked --test workspace_integration -- --ignored
```

The `Integration` workflow runs on pushes to `main` and on manual dispatch,
one run at a time. It reads credentials from the `databricks-free` GitHub
Environment, which only protected branches can use:

| Name | Kind | Value |
| --- | --- | --- |
| `DATABRICKS_HOST` | Variable | Workspace URL |
| `DBXCTL_IT_WAREHOUSE_ID` | Variable | SQL warehouse the test identity can use |
| `DATABRICKS_CLIENT_ID` | Secret | Test service principal application ID |
| `DATABRICKS_CLIENT_SECRET` | Secret | Test service principal OAuth secret |

The service principal needs `USE CATALOG` and `CREATE SCHEMA` on the test
catalog and `CAN USE` on the warehouse. It should not be a workspace admin.

After the tests, a separate job deletes anything left with the run's prefix.
The `Integration janitor` workflow runs every six hours and deletes test
resources older than six hours. Both use the same script, which can also be
run locally:

```console
scripts/integration-janitor.sh --prefix dbxctl_it_local_1760000000
scripts/integration-janitor.sh --older-than-hours 6
```

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

Configure a fine-grained `PIN_UPDATE_TOKEN` Actions secret with repository
Contents and Pull requests read/write permissions. Pull requests created with
that bot token trigger normal CI. Without the secret, the workflow falls back
to `GITHUB_TOKEN`; GitHub permits the PR but suppresses workflows triggered by
that token, so a maintainer must manually trigger CI before merging.

Version discovery and update generation remain independently runnable:

```console
scripts/discover-pin-versions.py
scripts/update-pins.py --help
```

## CI Platforms

- Linux: Rust and Markdown formatting, Clippy, tests, coverage, analysis,
  upstream contract, RustSec audit, and dependency policy
- macOS: native tests and analysis
- Windows: native tests and analysis

All three platform lanes have completed successfully in GitHub Actions. Native
macOS and Windows builds still inherit their hosted runner images and system
linkers as mutable trust boundaries.

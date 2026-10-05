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

### PyPI Index Configuration

By default, pip installs from pypi.org. For environments that cannot reach the
public index (corporate networks, sandboxes), three override options exist in
precedence order:

1. Explicit `PIP_INDEX_URL` environment variable (if set, the script skips all
   fallback logic; pip respects this standard variable unconditionally).
2. `DBXCTL_PYPI_PROXY` environment variable (an index URL ending in `/simple/`).
3. `.dbxctl.local` local file in the repository root (a `KEY=value` file; git
   will ignore it). Only the `DBXCTL_PYPI_PROXY=` key is parsed; the file is
   never sourced.

When neither option 1 nor option 2 is set, the script probes pypi.org with a
3-second timeout. If unreachable and option 3 provides a proxy, it falls back to
that index for the install only and prints a notice to stderr. If pypi.org is
reachable, or neither option is available, the script proceeds with the default
index unchanged.

Regardless of the index used, pip requires all installed packages to match
hashes from `requirements/mdformat.txt`, preventing substitution of different
artifacts even if the index is compromised or mirrors untrusted content.

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

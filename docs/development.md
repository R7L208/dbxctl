# Development and Testing

## Toolchain

Rust 1.89.0 is pinned in `rust-toolchain.toml` with Clippy, Rustfmt, and LLVM
coverage tools. Cargo builds must use the committed lockfile:

```console
cargo build --locked
```

The crate forbids unsafe Rust and enables the Clippy `all` and `pedantic` lint
groups at deny level.

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

## CI Platforms

- Linux: formatting, Clippy, tests, coverage, analysis, upstream contract,
  RustSec audit, and dependency policy
- macOS: native tests and analysis
- Windows: native tests and analysis

The Windows lane must complete successfully in GitHub Actions before the
platform is considered verified.


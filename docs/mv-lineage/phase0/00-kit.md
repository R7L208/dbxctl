# Phase 0 Kit

What you need to run the Phase 0 lineage probes, and how to run them. The
command shapes come from the contract in
[01-requirements.md](01-requirements.md).

> **Status:** `dbxctl probe run` works, but only the `cli` check resolves so
> far; every other selected check is reported as skipped, so a full run exits
> with code 10. `probe report` and `probe cleanup` are not implemented yet
> (#36, #37). #38 checks every command against the real
> `dbxctl probe ... --help` output.

## Prerequisites

- Databricks CLI 1.13.0 or newer, checked with `dbxctl doctor`. Set
  `DATABRICKS_CLI_PATH` when the CLI is not on `PATH`.
- Databricks authentication configured for the target and, optionally, a
  profile. `dbxctl` uses the CLI's own authentication and never handles
  credentials itself.
- A Databricks Asset Bundle checked out locally, with a non-production target.
- For SQL checks (`v2`, `v3`, `v11`, `v9`, `v4`): a SQL warehouse ID.
- For catalog-scoped checks: a Unity Catalog catalog to scope queries to.
- A `dbxctl` build from this repository (`cargo build --locked`).

`v2`, `v9`, and `v4` need `--warehouse-id`; `v3` and `v11` need both
`--warehouse-id` and `--scope-catalog`; every other check needs neither. See
the check table in [R4](01-requirements.md#r4-checks).

## Read-Only Run

The `cli` check, the only one implemented so far, exits 0 when it resolves:

```console
dbxctl probe run --suite lineage --bundle-root <path> --target <non-prod> \
  --only cli
```

Bundle-only checks, which need no warehouse or catalog. Until #32 implements
them, every check but `cli` is skipped and the run exits 10:

```console
dbxctl probe run --suite lineage --bundle-root <path> --target <non-prod> \
  --only cli,validate,plan,state,v8,v1
```

Full read-only suite:

```console
dbxctl probe run --suite lineage --bundle-root <path> --target <non-prod> \
  --warehouse-id <id> --scope-catalog <catalog>
```

Never pass `--allow-mutations` for a read-only run.

## Regenerating a Report

```console
dbxctl probe report --from <run-directory>
```

Run directories live at `<bundle-root>/.dbxctl/probe/lineage/<run-id>/`.
`.dbxctl/` is git-ignored automatically. The raw evidence in that directory is
not redacted and must not be shared; only the redacted `findings.md` may be
attached to issues, after the checks in #39.

## Mutation Run

Opt-in mutation probes (`v6` part B, `v10`, `v7`) are planned in #37, which may
be deferred. If they ship, they require `--allow-mutations` plus the check's
own input (`--v6-table`, `--v10-scratch-schema`, or `--v7-pipeline-id`), and
created resources are removed with:

```console
dbxctl probe cleanup --from <run-directory>
```

See [05-workspace-probes.md](05-workspace-probes.md).

## Interpreting Results

| Exit code | Meaning |
| --- | --- |
| 0 | Run completed and every selected check resolved |
| 1 | Invocation or usage failure |
| 10 | One or more selected checks not resolved (unknown, skipped, or failed) |
| 11 | Cleanup failure |

An unknown is a valid outcome: it carries a reason and the fallback later
phases will use. The report format is documented in
[06-findings-and-handoff.md](06-findings-and-handoff.md).

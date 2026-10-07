# Phase 0 Kit

What you need to run the Phase 0 lineage probes, and how to run them. The
command shapes come from the contract in
[01-requirements.md](01-requirements.md).

> **Status:** `dbxctl probe` is not implemented yet. The commands below are the
> planned interface from #23. #38 checks every command against the real
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

Which checks need a warehouse or catalog is defined per check by #29; until
then, see the check table in
[R4](01-requirements.md#r4-checks).

## Read-Only Run

Bundle-only checks, which need no warehouse or catalog:

```console
dbxctl probe run --suite lineage --bundle-root <path> --target <non-prod> \
  --only cli,validate,plan,state,v8,v1
```

Full read-only suite:

```console
dbxctl probe run --suite lineage --bundle-root <path> --target <target> \
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
| 0 | Every selected check resolved |
| 1 | Invocation or usage failure |
| 10 | One or more selected checks not resolved (unknown, skipped, or failed) |
| 11 | Cleanup failure |

An unknown is a valid outcome: it carries a reason and the fallback later
phases will use. The report format is documented in
[06-findings-and-handoff.md](06-findings-and-handoff.md).

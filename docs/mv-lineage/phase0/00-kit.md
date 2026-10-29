# Phase 0 Kit

What you need to run the Phase 0 lineage probes, and how to run them. The
command shapes come from the contract in
[01-requirements.md](01-requirements.md).

> **Status:** `dbxctl probe run` runs the bundle checks, the SQL and API
> checks, and `v6` part A; the mutation checks (#37) are reported as skipped,
> so a full run exits with code 10. `probe report` and `probe cleanup` are not
> implemented yet (#36, #37). #38 checks every command against the real
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
`--warehouse-id` and `--scope-catalog`; every other check needs neither. A run
that selects any of them without its option, including a run without `--only`,
exits 1 with a usage error naming the checks that need it. Only implemented
checks require an option, so a check added later starts requiring its option
when it lands. See the check table in [R4](01-requirements.md#r4-checks) and
what each SQL check reads in [04-sql-probes.md](04-sql-probes.md).

## Read-Only Run

The offline checks need no bundle or workspace: `cli` reads the CLI's
version and help, and `v1` and `v6` part A read its bundle schema. They exit
0 when they resolve:

```console
dbxctl probe run --suite lineage --bundle-root <path> --target <non-prod> \
  --only cli,v1,v6
```

Bundle-only checks, which need no warehouse or catalog. `validate`, `plan`,
and `v8` contact the workspace through the CLI. They share one
`bundle validate`, which runs only if `workspace.file_path` already exists,
because validate would create it; otherwise those checks are unknown unless
you pass `--allow-mutations`. `state` reads the local
copy of the deployment state, so deploy the bundle to the target from this
checkout first. They exit 0 when every check resolves:

```console
dbxctl probe run --suite lineage --bundle-root <path> --target <non-prod> \
  --only cli,validate,plan,state,v8,v1
```

Every check with a read step, which leaves out the mutation-only `v7`. `v6`
and `v10` run only their read steps, because no mutation step runs without
`--allow-mutations`. The SQL checks read both options. Until #37 implements
the remaining checks, they are skipped and the run exits 10:

```console
dbxctl probe run --suite lineage --bundle-root <path> --target <non-prod> \
  --warehouse-id <id> --scope-catalog <catalog> \
  --only cli,validate,plan,state,v8,v1,api,v2,v3,v11,v9,v4,v6,v10
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

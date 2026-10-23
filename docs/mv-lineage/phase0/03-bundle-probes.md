# Phase 0 Bundle Probes

> **Stub.** Completed by #32 (`cli`, `validate`, `plan`, `state`, `v8`, `v1`)
> and #34 (`v6` part A). Describe only implemented behavior here.

This page will document the bundle probes: what each check runs, the evidence
it stores, how each outcome is classified, and the fallback for unknowns. The
questions each check answers are listed in
[R4](01-requirements.md#r4-checks), and the expected CLI shapes in
[R5](01-requirements.md#r5-cli-baseline).

## cli

Stub (#30 minimal, #32 full).

## validate

Stub (#32).

## plan

Stub (#32).

## state

Stub (#32).

## v8

Stub (#32).

## v1

Stub (#32).

## v6

Part A (#34) answers offline whether table-update triggers are job fields.
Part B, which checks a live job, is in
[05-workspace-probes.md](05-workspace-probes.md).

**Runs:** `databricks bundle schema`, with a 60 second timeout. It needs no
bundle or workspace.

**Reads:** `$defs` nests definitions by Go import path, one object per path
segment, so the trigger definition is at
`$defs/github.com/databricks/databricks-sdk-go/service/jobs.TriggerSettings`.
A path segment holds only objects, so an object is a definition if it is
empty, has a member that is not an object, is the target of a `$ref`, or
reads as a schema: every object member is a keyword holding what that
keyword holds, such as `properties` mapping names to schemas. Any other
object is a path segment. A definition declares `table_update` if its
own `properties` contain it, or if a `oneOf`, `anyOf`, or `allOf` branch at
any depth, or a schema it names with `$ref`, declares it. Each `$ref` is
followed once, so a cycle ends. Each definition's package is its full import path. Verdicts name
SDK service packages without the `github.com/databricks/databricks-sdk-go/service/`
prefix, so only the SDK's package is `jobs`; any other package keeps its full
path.

**Evidence:** `evidence/v6/schema.json` records the invocation, with the
schema itself in `schema.stdout`.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| Only `jobs` definitions declare `table_update` | resolved | `jobs` |
| Definitions in other packages declare it too | resolved | Every package, sorted, such as `jobs, pipelines` |
| Only definitions outside `jobs` declare it, contradicting R5 | resolved | `not jobs: ` and every package, such as `not jobs: pipelines` |
| No definition declares it, or there is no `$defs` object | unknown | The reason names which |
| Output is not valid JSON, or the CLI exits non-zero or on a signal | unknown | The reason names which |
| The CLI cannot be run to completion | failed | See the evidence record |

**Fallback for unknown:** assume table-update triggers are job fields, as
[R5](01-requirements.md#r5-cli-baseline) expects, until part B checks a live
job.

The pinned CLI 1.13.0 and CLI 1.21.0 both resolve `jobs`. The ignored
`bundle_schema_trigger_contract` test runs v6 against the CLI at
`DATABRICKS_CLI_PATH`; CI runs it against the pinned 1.13.0, and it passes on
both versions. The fixture `tests/fixtures/bundle-schema-jobs.json` is a
trimmed excerpt of the 1.21.0 schema, which nests `jobs.TriggerSettings` as
1.13.0 does.

# Phase 0 Bundle Probes

This page documents the bundle probes: what each check runs, the evidence it
stores, how each outcome is classified, and the fallback for unknowns. The
questions each check answers are listed in
[R4](01-requirements.md#r4-checks), and the expected CLI shapes in
[R5](01-requirements.md#r5-cli-baseline).

Every check below records each CLI run under `evidence/<check>/` as
`<step>.json`, with the step's stdout and stderr beside it. Bundle commands run
from the bundle root with `-t <target>`, and `-p <profile>` when set, and
their output has credentials scrubbed. A check that cannot run its command to
completion is *failed*; a non-zero exit, a signal, or output of an unexpected
shape is *unknown*, with the reason naming which. Names taken from CLI output,
such as resource types or script keys, have control characters escaped so a
verdict stays on one line.

The shapes below were observed on CLI 1.13.0 against a local stub workspace;
the fixtures in `tests/fixtures/cli-1.13.0/` were captured that way. #35
checks them against a real workspace.

## cli

**Runs:** `databricks version`, then `databricks <command> --help` for each
command the suite runs: `bundle validate`, `bundle plan`, `bundle schema`,
`api get`, and `api post`. None needs a bundle or workspace.

**Reads:** the version, which must be 1.13.0 or newer, and each help's usage
line, which must name the whole command, such as
`databricks bundle plan [flags]`. For an unknown subcommand the CLI prints its
parent's help, `databricks bundle [command]`, and exits 0, so the exit code
alone proves nothing.

**Evidence:** `version.json` and `help-<command>.json`, such as
`help-bundle-plan.json`.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| Supported version, every command offered | resolved | The version, such as `1.13.0` |
| A help names another command, or exits non-zero | unknown | The commands not offered |
| No recognizable version, or older than 1.13.0 | unknown | The reason names which |

**Fallback for unknown:** assume only what `dbxctl doctor` enforces, and treat
the checks that run a missing command as unknown.

## The Shared Validate

`validate`, `plan`, and `v8` read one `bundle validate` per probe run.
Validation contacts the workspace: on CLI 1.13.0 it reads the current user
and checks `workspace.file_path`, and if that directory does not exist,
**creates it** (`POST /api/2.0/workspace/mkdirs`), so it is not read-only
([R9](01-requirements.md#r9-read-only-by-default)). Before running it, the
probe:

1. Runs `databricks bundle summary -o json`, which prints the same resolved
   workspace paths but only reads, and takes `workspace.file_path` from it.
1. Runs `databricks api get /api/2.0/workspace/get-status?path=<file_path>`.
   HTTP 200 means the directory exists, and 404 that it does not.
1. Runs `databricks bundle validate -o json --include-locations` only if the
   directory exists, or if `--allow-mutations` was given.

If the directory does not exist and `--allow-mutations` was not given, or the
summary or get-status gives no answer, validate does not run and each check
that needs it is unknown, with a reason naming R9 and its own fallback. The
first check that needs validate records `summary.json`, `get-status.json`,
and `validate.json` in its evidence directory; the others refer to the same
records. Validation and planning also write the CLI's local cache under
`.databricks/` in the bundle root, and every CLI command posts the CLI's own
telemetry; neither changes a workspace object.

## validate

**Runs:** the shared validate.

**Reads:** the JSON on stdout, whatever the exit code: CLI 1.13.0 prints the
validated configuration even when it exits 1, such as when it cannot create
the files directory. From it, `bundle.engine`, the string `workspace` fields
ending in `_path`, the resource types and how many of each, and the keys of
`experimental.scripts` and `scripts`. The declared resource set is read
defensively: a missing `resources` object, or a type or resource that is not
an object, is unknown rather than an empty set.

**Evidence:** the shared validate's records.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| A configuration with `workspace` and `resources` | resolved | `engine direct; workspace paths: artifact_path, file_path, …; resources: jobs 2, pipelines 1; experimental scripts: none; scripts: none` |
| The same, after a non-zero exit | resolved | The verdict ends `; validate exited with exit code 1` |
| Validate not run (R9) | unknown | The reason says why |
| No JSON object on stdout | unknown | The reason gives the exit code |
| No `workspace` or `resources`, or a malformed resource or script map | unknown | The reason names the field |

**Fallback for unknown:** read the bundle's own configuration files, and
assume the `direct` engine, workspace paths below `workspace.root_path`, and no
scripts.

## plan

**Runs:** `databricks bundle plan -o json`, which reads the deployed state but
deploys nothing and, on CLI 1.13.0, creates no directory. If an entry carries
no configuration, also the shared validate.

**Reads:** on CLI 1.13.0 the plan is
`{"plan_version": 2, "cli_version": …, "plan": {…}}`. Each entry of `plan` is
named `resources.<type>.<key>` and has an `action`: `create`, `update`,
`skip`, or `delete` were observed. A `create` or `update` carries the
configuration it would deploy in `new_state.value`, and the deployed copy in
`remote_state` once the resource exists. A `skip` or `delete` carries no
`new_state`, so for each such entry the check looks for the resource in the
validated configuration.

**Evidence:** `plan.json`, and the shared validate's records when it was
needed.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| A plan with entries | resolved | `plan_version 2`, then each action and where its configuration is: `new_state.value`, `validate`, or `not in validate` (a deleted resource), such as `plan_version 2; skip: validate; update: new_state.value` |
| Entries without configuration, and the shared validate gave none | unknown | The reason carries validate's, such as R9 |
| A non-zero exit, or output that is not JSON | unknown | The reason names which |
| No `plan` object, an entry not named `resources.<type>.<key>`, one without a string `action`, or a `new_state` without an object `value` | unknown | The reason names which |
| An empty plan | unknown | The actions cannot be observed |

**Fallback for unknown:** read each resource's configuration from
`bundle validate`, and treat every declared resource as changed.

## state

**Runs:** nothing. The check reads the copy of the deployment state the CLI
keeps in the bundle root, `.databricks/bundle/<target>/resources.json`, which
exists once the bundle has been deployed to the target from this checkout.

**Reads:** on CLI 1.13.0 the file is
`{"state_version": 2, "cli_version": …, "lineage": …, "serial": …, "state": {…}}`.
Each entry of `state` is named `resources.<type>.<key>` and has the resource's
`__id__` and its deployed configuration in `state`, so a pipeline's ID is at
`/state/resources.pipelines.<key>/__id__`. The copy matched the file deployed
to `workspace.state_path`.

**Evidence:** `resources-layout.json`: the file's top-level keys and, for each
entry, its name, whether it has an `__id__`, and the keys of its `state`. The
deployed configuration can hold secrets in plain fields, so the file itself
and its values, IDs included, are never copied.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| Every entry has a string `__id__`, and there are pipelines | resolved | `state_version 2; ids at /state/resources.<type>.<key>/__id__; config in state; resources: jobs 2, pipelines 1` |
| No local `resources.json`, or Terraform state instead | unknown | The reason names which |
| Not JSON, no `state` object, a malformed entry, or an entry without a string `__id__` | unknown | The reason names which |
| No pipeline entries | unknown | The pipeline-ID path cannot be observed |

**Fallback for unknown:** assume pipeline IDs at
`/state/resources.pipelines.<key>/__id__` in the deployed `resources.json`, as
R5 expects.

## v8

**Runs:** the shared validate, whose `--include-locations` CLI 1.13.0 accepts
though its help does not list it. It adds `__locations`, with the
configuration `files` and, for each resource, the file, line, and column where
it is defined.

**Reads:** the source paths each resource names: job tasks' notebooks, Python
files, SQL files, and dbt projects (including a `for_each_task`'s task),
pipeline libraries (`file`, `notebook`, and `glob`) and `root_path`, dashboard
files, and app sources. Validation rewrites a relative path to an absolute one
below `workspace.file_path`, relative to the file that names it, and a
notebook loses its extension, so `../notebooks/silver.py` in
`resources/extra.yml` becomes `<file_path>/notebooks/silver`. Each path takes
one form: `file_path` (below `workspace.file_path`), `workspace` (another
absolute path), `volume` (`/Volumes/…`), `dbfs` (`dbfs:/…`), `git` (a task
whose `source` is `GIT`, or that names none in a job with `git_source`),
`relative`, or `other`. A `file_path` path is matched with its file in the
bundle root: the file itself, a notebook with one of the extensions `py`,
`sql`, `scala`, `r`, or `ipynb`, a directory, or missing. A path with `..` or
an empty segment matches nothing. Each resource's origin is the file of its
first location.

**Evidence:** the shared validate's records.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| Source paths and locations | resolved | Each form's count, with the local matches of `file_path`, then each origin's resource count, such as `paths: file_path 6 (4 files, 2 notebooks without extension); origins: databricks.yml 2, resources/extra.yml 1` |
| A resource with no location | resolved | It counts as `unlocated` |
| Validate not run (R9) | unknown | The reason says why |
| No JSON object, no `workspace.file_path`, no `__locations`, or no `resources` | unknown | The reason names which |
| No source paths | unknown | The forms cannot be observed |

**Fallback for unknown:** treat every resource as defined in
`databricks.yml`, and each source path as `workspace.file_path` plus its path
in the bundle root, with a notebook's extension dropped.

## v1

**Runs:** `databricks bundle schema`, like `v6` part A. It needs no bundle or
workspace.

**Reads:** the schema of `experimental.scripts`, reached from
`properties.experimental` by following `$ref`s and taking the object branch of
the CLI's `oneOf` wrappers, which pair an object with a `${var…}` string. On
CLI 1.13.0 it is a map whose `additionalProperties` is a command, so any key
is accepted. A misspelled hook, such as `pre_deploy`, is accepted and never
runs; `bundle validate` on 1.13.0 also accepts it without a warning and keeps
it in its output.

**Evidence:** `schema.json`, with the schema in `schema.stdout`.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| `additionalProperties` is absent, `true`, or a schema | resolved | `not rejected: experimental.scripts accepts any key` |
| `additionalProperties` is `false` | resolved | `rejected: experimental.scripts accepts only` and the keys |
| Keys restricted by `propertyNames` or `patternProperties` | unknown | v1 does not read patterns |
| No object schema for `experimental.scripts`, or output that is not JSON | unknown | The reason names which |

**Fallback for unknown:** assume unknown experimental script keys are not
rejected, as R5 expects, so a misspelled hook is silently ignored.

The ignored `offline_bundle_checks_contract` test runs `cli` and `v1` against
the CLI at `DATABRICKS_CLI_PATH`; CI runs it against the pinned 1.13.0.

## v6

Part A (#34) answers offline whether table-update triggers are job fields.
Part B, which checks a live job, is in
[05-workspace-probes.md](05-workspace-probes.md).

**Runs:** `databricks bundle schema`, with a 60 second timeout. It needs no
bundle or workspace.

**Reads:** `$defs` nests definitions by Go import path, one object per path
segment, so the trigger definition is at
`$defs/github.com/databricks/databricks-sdk-go/service/jobs.TriggerSettings`.

- **Definitions and segments.** A path segment holds only objects and
  boolean-schema definitions, such as `"interface.Any": true`. An object is a
  definition if it is empty, is the target of a `$ref`, has a member that is
  neither an object nor a boolean under a name that is not a keyword, or reads
  as a schema: every object member is a keyword holding what that keyword
  holds, such as `properties` mapping names to schemas. Any other object is a
  path segment.
- **Refs.** A `$ref` is percent-decoded and must point into this document. A
  `$ref` inside data, under `default`, `const`, `enum`, or `examples`, names
  nothing.
- **Declaring.** A definition declares `table_update` if its own `properties`
  contain it, or if a `oneOf`, `anyOf`, or `allOf` branch at any depth, or a
  schema it names with `$ref`, declares it. Each `$ref` is walked at most once
  per definition and a "no" is reused for later definitions, so a cycle ends
  and a densely connected one stays linear.
- **Packages.** Each definition's package is its full import path. Verdicts
  name SDK service packages without the
  `github.com/databricks/databricks-sdk-go/service/` prefix, so only the SDK's
  package is `jobs`. A major version of the module keeps its version, so
  `.../databricks-sdk-go/v2/service/jobs` is `v2/jobs`; both count as the
  SDK's jobs package. A package directly under `$defs` is named `$defs/` and
  its name, such as `$defs/jobs`; any other package keeps its full path.
  Control characters in a name are escaped, so a verdict is one line.

**Limits:** a property added only under `if`, `then`, `else`, or
`dependentSchemas` is not the definition's own, and a definition that no
`$ref` names and whose object members are all names that are not keywords,
such as only `x-meta`, is walked as a path segment. A segment that reads as a
schema, such as one named `properties` holding only definitions, is taken for
a definition. Tests pin each of these.

**Evidence:** `evidence/v6/schema.json` records the invocation, with the
schema itself in `schema.stdout`.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| Only the SDK's `jobs` definitions declare `table_update` | resolved | `jobs` |
| Jobs definitions in several SDK major versions declare it | resolved | Each, sorted, such as `jobs, v2/jobs` |
| Definitions in other packages declare it too | resolved | The jobs packages, `; also: `, and the others, sorted, such as `jobs; also: pipelines` |
| Only definitions outside the SDK's jobs package declare it, contradicting R5 | resolved | `not jobs: ` and every package, sorted, such as `not jobs: pipelines` |
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

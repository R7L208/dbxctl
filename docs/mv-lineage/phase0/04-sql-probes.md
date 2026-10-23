# Phase 0 SQL and API Probes

This page documents the SQL and API probes (#33): the statements and API calls
each check makes, how missing system tables and permission errors become
unknowns, and the fallback for each. The questions are listed in
[R4](01-requirements.md#r4-checks).

Every check reaches the workspace through the transport (`src/transport.rs`):
the Databricks CLI, run from the bundle root with `-t <target>` and, when
given, `-p <profile>`. Every statement passes the read-only guard
(`safety::is_read_statement`) before any CLI process starts. The scope catalog
appears in SQL only as an identifier quoted with `text::identifier`; no value
is spelled into SQL as a literal.

`--warehouse-id` is required by `v2`, `v3`, `v11`, `v9`, and `v4`, and
`--scope-catalog` by `v3` and `v11`. `api` needs neither.

## How Outcomes Are Classified

- **Resolved:** the workspace answered every step the question needs.
- **Unknown:** the workspace answered with an error, such as a missing table
  (`TABLE_OR_VIEW_NOT_FOUND`, an HTTP 404) or missing privileges
  (`PERMISSION_DENIED`, `INSUFFICIENT_PERMISSIONS`, an HTTP 403), or the
  answer does not settle the question. The reason names the step and the kind
  of error, such as `ended FAILED (not found)`, and never copies text from the
  CLI's output.
- **Failed:** the CLI could not be run to completion, or a statement was
  refused as not read-only.

Values that come from the workspace, such as `table_type` values and column
names, enter a verdict only as words of ASCII letters, digits, and
`_ . : / { } -`; anything else becomes `?`, and each is cut to 64 characters.

**Evidence:** each step's CLI runs are recorded under `evidence/<check>/` as
`<step>.json`, with `.stdout` and `.stderr`, or as `<step>-1.json`,
`<step>-2.json`, and so on when a statement took several runs, such as a submit
and its polls. A statement response keeps only its first 64 KiB.

## api

**Runs:** `databricks api get <path> --debug` for
`/api/2.0/clusters/spark-versions`, which any user may read and which holds no
user data (step `good`), then for
`/api/2.0/pipelines/00000000-0000-0000-0000-000000000000`, which cannot exist
(step `bad`).

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| The good GET logs a status and the bad GET logs an HTTP error with an `error_code` | resolved | `status and error_code: HTTP 200; HTTP 404 RESOURCE_DOES_NOT_EXIST`, with the observed values |
| The good GET fails, or logs no status | unknown | The reason names which |
| The bad GET succeeds, logs no HTTP error status, or logs no `error_code` | unknown | The reason names which |
| The CLI cannot be run to completion | failed | See the evidence record |

**Fallback for unknown:** classify a failed `databricks api` call by its exit
code alone, as an error with no HTTP status or `error_code`.

## v2

**Runs:** `SELECT 1`, submitted once for each `wait_timeout` of 0, 4, 5, 50,
and 51 seconds (steps `wait-0s` through `wait-51s`): each side of the documented
range, 0 or 5 to 50 seconds. A statement the submit leaves pending or running
is cancelled. The CLI run is allowed the wait plus five seconds.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| Each submit is accepted or rejected with HTTP 400 | resolved | `accepted 0s, 5s, 50s; rejected 4s, 51s`, with the observed lists, or `none` |
| A submit fails any other way | unknown | The reason names the wait and the kind of error; the check stops there |
| The CLI cannot be run to completion | failed | See the evidence record |

**Fallback for unknown:** assume the documented range, 0 or 5 to 50 seconds;
the transport's 30-second submit wait is inside it.

## v3

**Runs:**

1. `table-types`: `SELECT table_type, count(*) AS tables FROM <scope>.information_schema.tables GROUP BY table_type ORDER BY table_type`.
1. `pipelines-columns`: `DESCRIBE TABLE system.lakeflow.pipelines`. Columns
   are the `col_name` values up to the first empty or `#` row.
1. `pipeline-types`, only when that table has a `pipeline_type` column:
   `SELECT pipeline_type, count(*) AS pipelines FROM system.lakeflow.pipelines GROUP BY pipeline_type ORDER BY pipeline_type`.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| A `table_type` in the scope catalog names a materialized view, and the pipelines table is described | resolved | `materialized views: table_type MATERIALIZED_VIEW; scope table_type values: …; system.lakeflow.pipelines columns: …; pipeline_type values: …`, where the last is `no such column` when there is none |
| No `table_type` in the scope catalog names a materialized view | unknown | Choose a scope catalog with a materialized view |
| A step fails, such as a missing catalog or table, or the describe lists no columns | unknown | The reason names the step and the kind of error |
| The CLI cannot be run to completion | failed | See the evidence record |

**Fallback for unknown:** identify materialized views by `table_type`
`MATERIALIZED_VIEW` in `information_schema.tables`, and read pipeline metadata
from `/api/2.0/pipelines/<id>` instead of `system.lakeflow.pipelines`.

## v11

**Runs:**

1. `table-types`: `SELECT DISTINCT table_type FROM <scope>.information_schema.tables ORDER BY table_type`.
1. `catalogs`: `databricks api get /api/2.1/unity-catalog/catalogs --debug`.
   Without `max_results`, the API returns every catalog the caller can see in
   one response.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| A `table_type` in the scope catalog names a metric view, and a listed catalog has a sharing `catalog_type` | resolved | `metric views: table_type METRIC_VIEW; shared catalogs: catalog_type DELTASHARING_CATALOG; scope catalog: …; catalog_type values: …`, sorted; the scope catalog is matched case-insensitively, or `not listed` |
| No metric-view `table_type` in the scope catalog | unknown | Choose a scope catalog with a metric view |
| No visible catalog has a sharing `catalog_type` | unknown | The reason says so |
| A step fails, or the response has no `catalogs` array | unknown | The reason names which |
| The CLI cannot be run to completion | failed | See the evidence record |

**Fallback for unknown:** treat `table_type` `METRIC_VIEW` as a metric view and
`catalog_type` `DELTASHARING_CATALOG` as a shared catalog, as Databricks
documents them.

## v9

**Runs:**

1. `columns`: `DESCRIBE TABLE system.access.table_lineage`, which must list
   `entity_type`, `entity_id`, `target_table_full_name`, and `event_date`.
1. `writes`: `SELECT entity_type, count(*) AS writes, count(entity_id) AS with_entity_id FROM system.access.table_lineage WHERE target_table_full_name IS NOT NULL AND event_date >= date_sub(current_date(), 30) GROUP BY entity_type ORDER BY entity_type`. A
   write has a target table; 30 days bounds the scan.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| Every `PIPELINE` write has an `entity_id` | resolved | `yes: every PIPELINE write in the last 30 days has an entity_id; writers: …` |
| No `PIPELINE` write has one | resolved | `no: …; writers: …` |
| Some do and some do not | resolved | `partly: …; writers: …` |
| No `PIPELINE` write in 30 days | unknown | Run the check after a pipeline has refreshed |
| The table lacks a needed column, a step fails, or the counts are not counts | unknown | The reason names which |
| The CLI cannot be run to completion | failed | See the evidence record |

`writers` lists every `entity_type` that wrote in the window, in order.

**Fallback for unknown:** attribute materialized-view writes to pipelines from
the pipeline definitions, in the bundle configuration and
`/api/2.0/pipelines/<id>`, not from lineage.

## v4

**Runs:**

1. One step per candidate system table, named by its last part:
   `DESCRIBE TABLE system.lakeflow.pipeline_update_timeline` and
   `DESCRIBE TABLE system.lakeflow.job_run_timeline`.
1. `pipelines`: `databricks api get /api/2.0/pipelines?max_results=1 --debug`.
1. `updates`, when a pipeline is listed and its ID holds only letters, digits,
   and `-`: `databricks api get /api/2.0/pipelines/<id>/updates?max_results=1 --debug`.

Each candidate is **found**, with its columns or, for the API, the keys of the
response and of its first update; **absent**, when the workspace says it does
not exist; **unreadable**, after any other error or an answer without a shape;
or, for the API when no pipeline is listed, **unobserved**. The verdict names
the API as `/api/2.0/pipelines/{id}/updates`, never with an ID.

| Outcome | State | Verdict or reason |
| --- | --- | --- |
| At least one candidate is found | resolved | `found: …; absent: …; unreadable: …; unobserved: …`, each `none` when empty |
| No candidate is found | unknown | The reason lists the absent, unreadable, and unobserved candidates |
| The CLI cannot be run to completion | failed | See the evidence record |

**Fallback for unknown:** read refresh history per pipeline from
`/api/2.0/pipelines/<id>/updates`, and per materialized view from its
pipeline's event log.

## Open Questions for #35 and #39

The fixtures and the fake CLI encode the documented shapes; a live run should
confirm:

- That `/api/2.0/clusters/spark-versions` is readable by the run's identity,
  and the `error_code` a missing pipeline returns.
- Whether the API rejects a `wait_timeout` outside the range with HTTP 400, as
  `v2` assumes, or another status.
- How a failed statement reports a missing table: as `[TABLE_OR_VIEW_NOT_FOUND]`
  at the start of `status.error.message`, or in `error_code`.
- The `table_type` of materialized and metric views, the `catalog_type` of a
  shared catalog, and which of the candidate system tables exist in the
  workspace.

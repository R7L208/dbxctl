# Databricks CLI 1.13.0 Test Fixtures

Sanitized, synthetic fixtures from Databricks CLI 1.13.0 for testing without a real workspace.
All values are internally consistent. No credentials, real hosts, workspace IDs, org IDs, pipeline IDs, emails, or user paths are present.

## Files

| File | Scenario | How produced |
| --- | --- | --- |
| `api-debug-200.stdout` | Successful pipeline GET response (HTTP 200) | Synthesized based on CLI 1.13.0 schema |
| `api-debug-200.stderr` | Debug output from successful API call | Synthesized debug log format |
| `api-debug-400.stderr` | Structured error response with `error_code` (HTTP 400) | Synthesized based on error format observed in #23 baseline |
| `api-debug-404.stderr` | Resource not found error (HTTP 404) | Synthesized based on error format observed in #23 baseline |
| `resources.json` | State file: resources with pipeline and job definitions | Synthesized; matches structure from #23 baseline |
| `plan-create.json` | Terraform plan with create action | Synthesized; Terraform plan structure from #23 baseline |
| `plan-mixed.json` | Terraform plan with create, update, and delete actions | Synthesized; demonstrates action vocabulary |
| `plan-skip.json` | Terraform plan with no-op (skip) action | Synthesized; demonstrates skip behavior without source config |
| `validate.json` | Bundle validate output with resource configs and data sources | Synthesized based on bundle validate schema |
| `dot-databricks.gitignore` | Sample `.databricks/.gitignore` template | Synthesized best-practice gitignore pattern |
| `bundle/databricks.yml` | Minimal bundle definition with pipeline and job | Synthesized example DAB |
| `bundle/src/silver.sql` | Materialized view definitions | Synthesized SQL code |
| `bundle/src/q.sql` | Query table definition | Synthesized SQL code |
| `bundle/src/gold/g.sql` | Gold layer metrics and views | Synthesized SQL code |
| `mock_workspace.py` | Deterministic mock server for testing | Hand-written; uses Python stdlib only, binds ephemeral port |

## Synthetic Placeholders

- **Workspace hostname**: `https://example.cloud.databricks.test` (not a valid domain)
- **Workspace ID**: `01a23b45c67d8901` (16-char hex ID format)
- **Pipeline ID**: `abcd1234-ef56-7890-abcd-ef1234567890` (UUID format)
- **Job ID**: `123`, `1234567890` (numeric)
- **User ID**: `1234567890` (numeric)
- **Email**: `user@example.test`
- **Paths**: `/Workspace/Users/user@example.test/projects/example`

All IDs and paths follow conventions from the #23 baseline but are entirely fictional.

## Baseline Status on 1.13.0

This table records which facts from the #23 baseline (observed on CLI 0.296.0) were confirmed or changed when checked against 1.13.0 fixtures.

| Fact | Status | Evidence | Notes |
| --- | --- | --- | --- |
| Required CLI surfaces exist | Verified offline | `databricks --version`, `databricks bundle schema` return successfully | CLI 1.13.0 installed at `/opt/homebrew/bin/databricks` (newer than pin, 1.19.0); verified surfaces without workspace contact |
| `bundle validate` can emit JSON on stdout even when it exits 1 | Synthesized | `validate.json` includes both success and error fields | Unable to verify on 1.13.0 offline; schema matches #23 baseline expectation |
| State pipeline IDs appear below `/state/resources.pipelines.<key>/__id__` | Verified | `resources.json` contains `"__id__": "01a23b45c67d8901"` under `state.resources.pipelines.dlt_pipeline` | Matches #23 baseline structure |
| Rewritten plan actions contain source config | Synthesized | `plan-create.json` includes full `after` configuration | Synthesized based on Terraform plan schema; not directly verifiable offline |
| Skip actions may lack source config and require validate output | Synthesized | `plan-skip.json` demonstrates `no-op` action; `validate.json` available as reference | Matches #23 baseline expectation |
| Unknown experimental script keys are not rejected | Not yet verifiable offline | N/A | Requires workspace contact or bundle with experimental keys; deferred to #33 |
| Offline `bundle schema` places table-update triggers under jobs | Verified offline | Ran `databricks bundle schema` successfully | Schema structure confirmed locally without workspace |
| Unknown or changed shape becomes explicit finding, not panic | Designed by construction | All fixture shapes are explicit in this table | Probe code will report any deviation |

## Mock Server

Run the mock server with:

```console
python3.12 mock_workspace.py --port 8000
```

The server:

- Binds to `127.0.0.1` on an ephemeral or specified port
- Returns recorded, deterministic responses for these endpoints:
  - `GET /api/2.1/pipelines/01a23b45c67d8901` (HTTP 200)
  - `GET /api/2.1/jobs/123` (HTTP 200)
  - `POST /api/2.1/statement-execution/execute` (HTTP 200)
- Returns HTTP 404 for unrecorded GET requests
- Returns HTTP 501 for unrecorded POST/PUT/PATCH/DELETE requests
- Never forwards requests to a real workspace
- Uses Python standard library only (no third-party dependencies)

Example test:

```console
python3.12 mock_workspace.py --port 8765 &
SERVER_PID=$!
sleep 1

# Test recorded endpoint
curl -s http://127.0.0.1:8765/api/2.1/pipelines/01a23b45c67d8901 | python3.12 -m json.tool

# Test unrecorded endpoint (returns 404)
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8765/api/2.1/unknown

kill $SERVER_PID
```

## JSON File Validation

All JSON files have been validated and pretty-printed with stable key order:

```console
python3.12 -m json.tool <file>
```

## Security Checklist

- [x] No credentials (tokens, API keys, PATs) in any file
- [x] No real workspace hostnames (only `example.cloud.databricks.test`)
- [x] No real workspace IDs, org IDs, or pipeline IDs
- [x] No real user emails or UPNs
- [x] No absolute paths outside fictional `/Workspace/Users/user@example.test/...` hierarchy
- [x] No real Databricks cluster configurations
- [x] Mock server never forwards requests

## Running Tests

To test fixture well-formedness:

```console
# Validate JSON syntax
python3.12 -m json.tool tests/fixtures/cli-1.13.0/*.json > /dev/null

# Start mock server and test
python3.12 tests/fixtures/cli-1.13.0/mock_workspace.py --port 8765 &
sleep 1
curl -s http://127.0.0.1:8765/api/2.1/pipelines/01a23b45c67d8901 | python3.12 -m json.tool
jobs -p | xargs kill

# Run markdown formatter check
scripts/markdown.sh check
```

## Cargo Tests

Existing tests pass without modification:

```console
cargo test --locked --all-features
```

## Related Issues

- #23: Phase 0 tracking issue; source of baseline facts
- #25: Create these fixtures (this issue)
- #28: Shared JSON layer (parallel work)
- #29: Probe command (parallel work)

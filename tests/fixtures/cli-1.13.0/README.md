# Databricks CLI 1.13.0 Test Fixtures

Synthetic, internally consistent test fixtures designed for Databricks CLI 1.13.0 (the minimum
supported version). These enable offline testing without a real workspace, credentials, or
network access.

All values are placeholders: no real credentials, hosts, workspace IDs, org IDs, pipeline IDs,
emails, or user paths are present.

## Files

| File | Scenario | How produced |
| --- | --- | --- |
| `api-debug-200.stdout` | Successful pipeline GET response (HTTP 200) | Synthesized based on API response schema |
| `api-debug-200.stderr` | Debug output from successful API call | Synthesized debug log format |
| `api-debug-400.stderr` | Structured error response with `error_code` (HTTP 400) | Synthesized based on error format from #23 baseline |
| `api-debug-404.stderr` | Resource not found error (HTTP 404) | Synthesized based on error format from #23 baseline |
| `resources.json` | State file: resources with pipeline and job definitions. Flat-key structure: `resources.pipelines.dlt_pipeline/__id__` resolves via JSON pointer. | Synthesized; structure matches #23 baseline |
| `plan-create.json` | CLI native plan output with create action. Keyed by resource (`jobs.refresh_silver`), with `action` and `config` fields. | Synthesized from CLI vocabulary; exact shape unverified on 1.13.0 |
| `plan-mixed.json` | CLI native plan with create and update actions. | Synthesized from CLI vocabulary; exact shape unverified on 1.13.0 |
| `plan-skip.json` | CLI native plan with skip action *without* source configuration. Probes must handle skip actions and consult `validate.json`. | Synthesized from CLI vocabulary; demonstrates baseline scenario |
| `validate.json` | Bundle validate resolved output: expanded bundle configuration for the `dev` target with all variables resolved. | Synthesized; exact shape unverified on 1.13.0 |
| `dot-databricks.gitignore` | Sample `.databricks/.gitignore` template | Synthesized best-practice pattern |
| `bundle/databricks.yml` | Minimal bundle definition with pipeline and job | Synthesized example DAB |
| `bundle/src/silver.sql` | Materialized view definitions | Synthesized SQL code |
| `bundle/src/q.sql` | Query table definition | Synthesized SQL code |
| `bundle/src/gold/g.sql` | Gold layer metrics and views | Synthesized SQL code |
| `mock_workspace.py` | Deterministic mock server for testing | Hand-written; uses Python stdlib only, binds ephemeral port |

## Synthetic Placeholders

Each placeholder is distinct and used consistently across all fixtures.

- **Workspace hostname**: `https://example.cloud.databricks.test` (not a valid domain)
- **Workspace ID**: `aabbccddeeff0011` (16-char hex)
- **Pipeline ID**: `01a23b45c67d8901` (16-char hex)
- **Job ID**: `123` (numeric)
- **User ID**: `9876543210` (numeric)
- **Pipeline configuration ID**: `abcd1234-ef56-7890-abcd-ef1234567890` (UUID)
- **Email**: `user@example.test`
- **Paths**: `/Workspace/Users/user@example.test/projects/example`

All follow conventions from the #23 baseline but are entirely fictional.

## Path Resolution (Synthesized)

The bundle source (`databricks.yml`) uses relative paths (e.g., `./notebooks/silver`). When
`bundle validate` runs, it resolves these paths using the target's workspace root and the
bundle's file path prefix. For the `dev` target:

- Bundle source relative path: `./notebooks/silver`
- Resolved workspace path: `/Workspace/Users/user@example.test/projects/example-dev/files/notebooks/silver`

Similarly, pipeline library files:

- Bundle relative path: `./src/silver.sql`
- Resolved workspace path: `/Workspace/Users/user@example.test/projects/example-dev/files/src/silver.sql`

This resolution is **synthesized** in `validate.json` and `plan-*.json` based on the bundle
schema and target definitions. The exact path resolution mechanism will be verified during
Phase 0 against a real 1.13.0 environment.

## Baseline Status (target 1.13.0)

This table records observations made during fixture creation. Cells marked "Verified offline" were
tested on CLI 1.19.0 (newer than the 1.13.0 pin, not yet confirmed on 1.13.0). Cells marked
"Synthesized" represent unverified shapes. All entries will be re-verified during Phase 0 execution
against a real 1.13.0 environment.

| Fact | Status | Observed on | Evidence |
| --- | --- | --- | --- |
| Required CLI surfaces exist | Verified offline | 1.19.0 | `databricks --version` and `databricks bundle schema` work without workspace contact |
| `bundle validate` can emit JSON on stdout even when it exits 1 | Synthesized | (unverified) | `validate.json` structure; needs workspace test for error case on 1.13.0 |
| State pipeline IDs at `/state/resources.pipelines.<key>/__id__` | Synthesized | (unverified) | `resources.json` structure matches #23 baseline expectations; needs verification on 1.13.0 |
| CLI plan output keyed by resource with action and config fields | Synthesized | (unverified) | `plan-*.json` structure inferred from baseline; exact shape needs verification on 1.13.0 |
| Skip actions may lack source configuration | Synthesized | (unverified) | `plan-skip.json` demonstrates; needs verification on 1.13.0 |
| Offline `bundle schema` structure | Verified offline | 1.19.0 | `databricks bundle schema` runs successfully and produces schema with expected fields |
| Unknown experimental script keys are not rejected | Deferred | — | Requires workspace contact or bundle with experimental keys; scheduled for #32 bundle probes |

## Mock Server

Run the mock server with:

```console
python3.12 mock_workspace.py --port 8000
```

The server:

- Binds to `127.0.0.1` on an ephemeral or specified port (binds once; no racing)
- Returns recorded, deterministic responses for these endpoints:
  - `GET /api/2.0/pipelines/01a23b45c67d8901` (HTTP 200, JSON with pipeline info)
  - `GET /api/2.1/jobs/get?job_id=123` (HTTP 200, JSON with job info)
  - `POST /api/2.0/sql/statements` (HTTP 200, Statement Execution response with `statement_id`, `status.state`, `manifest.format`, `manifest.schema.columns[]` with `name`, `type_text`, `type_name`, `position`, and `result.data_array`)
- Returns HTTP 404 for unrecorded GET requests
- Returns HTTP 501 for unrecorded POST/PUT/PATCH/DELETE requests
- Parses query parameters and reads POST JSON bodies
- Never forwards requests to a real workspace
- Uses Python standard library only (no third-party dependencies)

Example test:

```console
python3.12 mock_workspace.py --port 8765 &
SERVER_PID=$!
sleep 1

# Test recorded endpoint
curl -s http://127.0.0.1:8765/api/2.0/pipelines/01a23b45c67d8901 | python3.12 -m json.tool

# Test recorded job endpoint with query parameter
curl -s 'http://127.0.0.1:8765/api/2.1/jobs/get?job_id=123' | python3.12 -m json.tool

# Test unrecorded endpoint (returns 404)
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8765/api/2.1/unknown

kill $SERVER_PID
```

## JSON File Validation

All JSON files have been validated and pretty-printed with stable key order:

```console
python3.12 -m json.tool <file>
```

Baseline paths in `resources.json` have been verified to resolve correctly via JSON pointer:

```python
import json
with open('resources.json') as f:
    data = json.load(f)
# /state/resources.pipelines.dlt_pipeline/__id__ resolves to: 01a23b45c67d8901
# /state/resources.jobs.refresh_silver/__id__ resolves to: 123
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
for f in tests/fixtures/cli-1.13.0/*.json; do
  python3.12 -m json.tool "$f" > /dev/null || exit 1
done

# Start mock server and test
python3.12 tests/fixtures/cli-1.13.0/mock_workspace.py --port 8765 &
sleep 1
curl -s http://127.0.0.1:8765/api/2.0/pipelines/01a23b45c67d8901 | python3.12 -m json.tool
jobs -p | xargs kill

# Run markdown formatter check
scripts/markdown.sh check

# Run Rust tests
cargo test --locked --all-features
```

## Related Issues

- #23: Phase 0 tracking issue; source of baseline facts
- #25: Create these fixtures (this issue)
- #28: Shared JSON layer (parallel work)
- #29: Probe command (parallel work)
- #30: Evidence model and minimal CLI check
- #32: Bundle probes (will verify these fixtures on 1.13.0)

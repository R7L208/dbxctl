#!/usr/bin/env bash
# Delete dbxctl integration-test resources from a Databricks workspace.
#
# Only schemas and workspace directories named dbxctl_it_* are eligible.
# Authentication is resolved by the Databricks CLI from its environment or
# profile; this script never reads or prints credentials.
set -euo pipefail

usage() {
  cat <<'EOF'
usage: scripts/integration-janitor.sh --prefix PREFIX
       scripts/integration-janitor.sh --older-than-hours HOURS

  --prefix PREFIX          delete resources created by one run (PREFIX or PREFIX_*)
  --older-than-hours N     delete every test resource created more than N hours ago

Environment:
  DATABRICKS_CLI_PATH  Databricks CLI binary (default: databricks on PATH)
  DBXCTL_IT_CATALOG    Unity Catalog catalog holding test schemas (default: workspace)
EOF
}

prefix=""
hours=""
case "${1:-}" in
  --prefix) prefix="${2:-}" ;;
  --older-than-hours) hours="${2:-}" ;;
  *) usage >&2; exit 2 ;;
esac
if [[ $# -ne 2 ]]; then
  usage >&2
  exit 2
fi
if [[ -n "${prefix}" && ! "${prefix}" =~ ^dbxctl_it_[a-z0-9_]+$ ]]; then
  echo "error: prefix must match dbxctl_it_[a-z0-9_]+" >&2
  exit 2
fi
if [[ -n "${hours}" && ! "${hours}" =~ ^[0-9]+$ ]]; then
  echo "error: hours must be a non-negative integer" >&2
  exit 2
fi

databricks="${DATABRICKS_CLI_PATH:-databricks}"
catalog="${DBXCTL_IT_CATALOG:-workspace}"
cutoff_ms=$(( ($(date +%s) - ${hours:-0} * 3600) * 1000 ))

# Reads a JSON array of objects with "name" and optional "created_at" fields
# and prints the names selected for deletion. Objects without a creation time
# are kept in age mode.
select_names() {
  jq --raw-output --arg prefix "${prefix}" --argjson cutoff "${cutoff_ms}" '
    (. // [])[]
    | select(.name | test("^dbxctl_it_[a-z0-9_]+$"))
    | select(
        if $prefix != "" then .name == $prefix or (.name | startswith($prefix + "_"))
        else (.created_at // $cutoff) < $cutoff
        end)
    | .name'
}

failures=0
delete() {
  echo "deleting $*"
  if ! "${databricks}" "$@" >/dev/null; then
    failures=$((failures + 1))
  fi
}

schemas="$("${databricks}" schemas list "${catalog}" --output json | select_names)"
while IFS= read -r schema; do
  if [[ -n "${schema}" ]]; then
    delete schemas delete "${catalog}.${schema}" --force
  fi
done <<< "${schemas}"

user="$("${databricks}" current-user me --output json | jq --raw-output '.userName')"
root="/Workspace/Users/${user}/dbxctl-it"
"${databricks}" workspace mkdirs "${root}"
directories="$("${databricks}" workspace list "${root}" --output json \
  | jq '[(. // [])[] | select(.object_type == "DIRECTORY") | .name = (.path | split("/") | last)]' \
  | select_names)"
while IFS= read -r directory; do
  if [[ -n "${directory}" ]]; then
    delete workspace delete "${root}/${directory}" --recursive
  fi
done <<< "${directories}"

if [[ "${failures}" -gt 0 ]]; then
  echo "error: ${failures} deletion(s) failed" >&2
  exit 1
fi

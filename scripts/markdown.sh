#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
requirements="${root}/requirements/mdformat.txt"
tool_root="${DBXCTL_TOOL_DIR:-${root}/target/tools}/mdformat"
python="${PYTHON:-python3}"

usage() {
  echo "usage: scripts/markdown.sh <format|check>" >&2
  exit 2
}

[[ $# -eq 1 ]] || usage
case "$1" in
  format | check) mode="$1" ;;
  *) usage ;;
esac

if [[ ! -x "${tool_root}/bin/python" ]]; then
  "${python}" -c 'import sys; raise SystemExit(sys.version_info < (3, 11))'
  "${python}" -m venv "${tool_root}"
fi

export PIP_DISABLE_PIP_VERSION_CHECK=1
export PIP_NO_CACHE_DIR=1
export PIP_REQUIRE_VIRTUALENV=1
"${tool_root}/bin/python" -m pip install \
  --no-input --no-deps --require-hashes --only-binary=:all: \
  --requirement "${requirements}"
"${tool_root}/bin/python" -m pip check
[[ "$("${tool_root}/bin/python" -m mdformat --version)" == "mdformat 1.0.0" ]]

cd "${root}"
if [[ "${mode}" == check ]]; then
  git ls-files --cached --others --exclude-standard -z -- '*.md' \
    | xargs -0 "${tool_root}/bin/python" -m mdformat --check --
else
  git ls-files --cached --others --exclude-standard -z -- '*.md' \
    | xargs -0 "${tool_root}/bin/python" -m mdformat --
fi

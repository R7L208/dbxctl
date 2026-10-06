#!/usr/bin/env bash
# Scan GitHub Actions workflows, composite actions, and Dependabot
# configuration with zizmor. Exits nonzero on any unaccepted finding.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
requirements="${root}/requirements/zizmor.txt"
tool_root="${DBXCTL_TOOL_DIR:-${root}/target/tools}/zizmor"
python="${PYTHON:-python3}"
version="1.30.1"

usage() {
  echo "usage: scripts/workflows.sh check" >&2
  exit 2
}

[[ $# -eq 1 && "$1" == check ]] || usage

if [[ ! -x "${tool_root}/bin/python" ]]; then
  "${python}" -c 'import sys; raise SystemExit(sys.version_info < (3, 11))'
  "${python}" -m venv "${tool_root}"
fi

export PIP_DISABLE_PIP_VERSION_CHECK=1
export PIP_NO_CACHE_DIR=1
export PIP_REQUIRE_VIRTUALENV=1

# Same index selection as scripts/markdown.sh: an explicit PIP_INDEX_URL always
# wins; otherwise a configured DBXCTL_PYPI_PROXY is used only while pypi.org is
# unreachable. The URL is never printed because it may contain credentials.
if [[ -z "${PIP_INDEX_URL:-}" ]]; then
  proxy="${DBXCTL_PYPI_PROXY:-}"
  if [[ -z "${proxy}" && -f "${root}/.dbxctl.local" ]]; then
    proxy="$(sed -n 's/^DBXCTL_PYPI_PROXY=//p' "${root}/.dbxctl.local" | tail -n 1 | tr -d '\r')"
  fi
  if [[ -n "${proxy}" ]] && ! "${tool_root}/bin/python" -c \
    'import urllib.request; urllib.request.urlopen("https://pypi.org/simple/", timeout=3)' \
    2>/dev/null; then
    export PIP_INDEX_URL="${proxy}"
    echo "workflows.sh: pypi.org is unreachable; using DBXCTL_PYPI_PROXY." >&2
  fi
fi

"${tool_root}/bin/python" -m pip install \
  --no-input --no-deps --require-hashes --only-binary=:all: \
  --requirement "${requirements}"
"${tool_root}/bin/python" -m pip check

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) zizmor="${tool_root}/Scripts/zizmor.exe" ;;
  *) zizmor="${tool_root}/bin/zizmor" ;;
esac
[[ "$("${zizmor}" --version)" == "zizmor ${version}" ]]

# The impostor-commit, known-vulnerable-actions, ref-confusion, and other
# online audits need a GitHub token. Without one, run offline and say so, so a
# local pass is not mistaken for the full CI scan.
online=(--offline)
if [[ -n "${GH_TOKEN:-}" ]]; then
  online=()
else
  echo "workflows.sh: GH_TOKEN is unset; skipping online audits (CI runs them)." >&2
fi

cd "${root}"
# The empty-array expansion form keeps Bash 3.2 (macOS) happy under `set -u`.
"${zizmor}" --config .github/zizmor.yml --no-progress ${online[@]+"${online[@]}"} .

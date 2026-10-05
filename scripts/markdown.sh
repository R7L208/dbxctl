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

# PyPI proxy fallback: respect explicit PIP_INDEX_URL, probe pypi.org,
# fall back to DBXCTL_PYPI_PROXY if configured.
if [[ -z "${PIP_INDEX_URL:-}" ]]; then
  # Read DBXCTL_PYPI_PROXY from environment or .dbxctl.local
  dbxctl_proxy="${DBXCTL_PYPI_PROXY:-}"
  if [[ -z "${dbxctl_proxy}" ]] && [[ -f "${root}/.dbxctl.local" ]]; then
    dbxctl_proxy="$(grep -E '^DBXCTL_PYPI_PROXY=' "${root}/.dbxctl.local" | cut -d= -f2- || true)"
  fi

  # Probe pypi.org reachability with 3s timeout
  pypi_reachable=true
  if command -v curl &>/dev/null; then
    curl -fsS -m 3 -o /dev/null https://pypi.org/simple/ || pypi_reachable=false
  else
    # Fallback to Python urllib probe
    "${python}" -c "import urllib.request; urllib.request.urlopen('https://pypi.org/simple/', timeout=3)" 2>/dev/null || pypi_reachable=false
  fi

  # Use proxy if pypi.org is unreachable and proxy is configured
  if [[ "${pypi_reachable}" == false ]] && [[ -n "${dbxctl_proxy}" ]]; then
    export PIP_INDEX_URL="${dbxctl_proxy}"
    echo "PyPI index unreachable; using configured proxy." >&2
  fi
fi

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

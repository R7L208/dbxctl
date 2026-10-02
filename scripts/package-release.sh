#!/usr/bin/env bash

set -euo pipefail

if [[ "$#" -ne 3 ]]; then
  echo "usage: $0 VERSION TARGET BINARY" >&2
  exit 2
fi

version="$1"
target="$2"
binary="$3"

: "${SOURCE_DATE_EPOCH:?SOURCE_DATE_EPOCH must be set}"

case "${binary}" in
  *.exe) installed_binary=dbxctl.exe ;;
  *) installed_binary=dbxctl ;;
esac

archive_root="dbxctl-${version}-${target}"
package_root="dist/${archive_root}"
archive="dist/${archive_root}.tar.gz"

install -d -m 0755 "${package_root}"
install -m 0755 "${binary}" "${package_root}/${installed_binary}"
install -m 0644 LICENSE README.md Cargo.toml Cargo.lock "${package_root}/"

# Normalize filesystem timestamps before archiving. GNU and BSD touch use
# different date conversion flags, so select the supported form explicitly.
if touch -d "@${SOURCE_DATE_EPOCH}" "${package_root}" 2>/dev/null; then
  find "${package_root}" -exec touch -d "@${SOURCE_DATE_EPOCH}" {} +
else
  archive_timestamp="$(date -u -r "${SOURCE_DATE_EPOCH}" +%Y%m%d%H%M.%S)"
  find "${package_root}" -exec touch -t "${archive_timestamp}" {} +
fi

if tar --version 2>/dev/null | grep --quiet 'GNU tar'; then
  tar --create --file - --format=ustar --sort=name \
    --mtime="@${SOURCE_DATE_EPOCH}" --owner=0 --group=0 --numeric-owner \
    --directory dist "${archive_root}" | gzip -n > "${archive}"
else
  # macOS bsdtar would otherwise include AppleDouble metadata and local user
  # names. Entries are stable because the staging tree is created in a fixed
  # order and all timestamps were normalized above.
  export COPYFILE_DISABLE=1
  tar --create --file - --format=ustar --uid 0 --gid 0 --uname root --gname root \
    --directory dist "${archive_root}" | gzip -n > "${archive}"
fi

(
  cd dist
  archive_name="${archive_root}.tar.gz"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${archive_name}" > "${archive_name}.sha256"
  else
    shasum -a 256 "${archive_name}" > "${archive_name}.sha256"
  fi
)

echo "${archive}"

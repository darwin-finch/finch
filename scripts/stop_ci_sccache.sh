#!/usr/bin/env bash
# Stop sccache and prove its local cache is within the 256 MiB save bound.

set -euo pipefail

if [[ -z "${GITHUB_OUTPUT:-}" ]]; then
  echo "GITHUB_OUTPUT is required for the compiler-cache save decision" >&2
  exit 1
fi

echo "save-ready=false" >>"${GITHUB_OUTPUT}"
if [[ -z "${SCCACHE_PATH:-}" || ! -x "${SCCACHE_PATH}" || -z "${SCCACHE_DIR:-}" ]]; then
  echo "sccache was not enabled; skipping compiler-cache save"
  exit 0
fi
if ! "${SCCACHE_PATH}" --show-stats; then
  echo "sccache statistics were unavailable; skipping compiler-cache save" >&2
  exit 0
fi
if ! "${SCCACHE_PATH}" --stop-server; then
  echo "sccache did not stop cleanly; skipping compiler-cache save" >&2
  exit 0
fi

size_kib=$(du -sk "${SCCACHE_DIR}" | awk '{print $1}')
if [[ ! "${size_kib}" =~ ^[0-9]+$ || "${size_kib}" -gt 262144 ]]; then
  echo "local sccache is ${size_kib:-unknown} KiB, above the 256 MiB save bound; skipping save" >&2
  exit 0
fi

echo "cache-size-kib=${size_kib}" >>"${GITHUB_OUTPUT}"
echo "save-ready=true" >>"${GITHUB_OUTPUT}"
echo "local sccache stopped at ${size_kib} KiB and is safe to save"

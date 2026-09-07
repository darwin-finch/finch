#!/usr/bin/env bash
# Configure Finch's optional GitHub Actions compiler-object cache.
#
# The setup action exports SCCACHE_PATH only after it has downloaded and
# checksum-verified the pinned binary. A cache/setup outage must leave Cargo
# usable, so this script enables RUSTC_WRAPPER only when that binary exists.

set -euo pipefail

if [[ -z "${SCCACHE_PATH:-}" || ! -x "${SCCACHE_PATH}" ]]; then
  echo "sccache is unavailable; continuing with ordinary rustc compilation"
  exit 0
fi

if [[ -z "${GITHUB_ENV:-}" ]]; then
  echo "GITHUB_ENV is required to configure sccache for later workflow steps" >&2
  exit 1
fi

cache_mode=READ_ONLY
if [[ "${GITHUB_EVENT_NAME:-}" == "push" && \
      "${GITHUB_REF:-}" == "refs/heads/main" && \
      "${FINCH_SCCACHE_WRITER:-false}" == "true" ]]; then
  cache_mode=READ_WRITE
fi

{
  echo "RUSTC_WRAPPER=${SCCACHE_PATH}"
  echo "CARGO_INCREMENTAL=0"
  echo "SCCACHE_GHA_ENABLED=true"
  echo "SCCACHE_GHA_VERSION=finch-v1-sccache-0.17.0-rust-1.98.0"
  echo "SCCACHE_GHA_RW_MODE=${cache_mode}"
} >>"${GITHUB_ENV}"

echo "configured sccache GitHub backend in ${cache_mode} mode"

#!/usr/bin/env bash
# Cache reusable Rust objects, but never final or generated executables.

set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "rustc cache wrapper requires the real rustc path" >&2
  exit 1
fi

for argument in "$@"; do
  case "${argument}" in
    --test|build_script_build|build-script-build)
      exec "$@"
      ;;
  esac
done

expect_crate_type=false
for argument in "$@"; do
  if ${expect_crate_type}; then
    if [[ ",${argument}," == *,bin,* ]]; then
      exec "$@"
    fi
    expect_crate_type=false
  elif [[ "${argument}" == "--crate-type" ]]; then
    expect_crate_type=true
  elif [[ "${argument}" == --crate-type=*bin* ]]; then
    exec "$@"
  fi
done

if [[ -z "${SCCACHE_PATH:-}" || ! -x "${SCCACHE_PATH}" ]]; then
  echo "SCCACHE_PATH must name the reviewed executable" >&2
  exit 1
fi
exec "${SCCACHE_PATH}" "$@"

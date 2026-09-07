#!/usr/bin/env bash
# Give canonical CI Cargo jobs a fresh, explicitly scoped download home.

set -euo pipefail

if [[ -z "${RUNNER_TEMP:-}" || -z "${GITHUB_ENV:-}" || -z "${GITHUB_PATH:-}" ]]; then
  echo "RUNNER_TEMP, GITHUB_ENV, and GITHUB_PATH are required for isolated Cargo downloads" >&2
  exit 1
fi

cargo_home="${RUNNER_TEMP}/finch-cargo-home"
mkdir -p "${cargo_home}/bin"
echo "CARGO_HOME=${cargo_home}" >>"${GITHUB_ENV}"
echo "${cargo_home}/bin" >>"${GITHUB_PATH}"
echo "configured isolated Cargo home at ${cargo_home}"

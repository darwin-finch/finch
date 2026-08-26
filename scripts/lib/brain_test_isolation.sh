#!/usr/bin/env bash

# Shared fail-closed boundary for tests and smokes that can construct a Brain.
# Call brain_test_isolation_run with the command to execute. The command gets a
# disposable HOME, while the caller's real Brain-store manifest is guarded.

brain_store_manifest() {
  local root="$1"

  if [[ ! -d "$root" ]]; then
    printf '%s\n' '<missing>'
    return
  fi

  (
    cd "$root"
    find . -type f -print0 | LC_ALL=C sort -z | while IFS= read -r -d '' file; do
      printf 'file\t%s\t' "$file"
      shasum -a 256 "$file" | awk '{print $1}'
    done
    find . -type l -print0 | LC_ALL=C sort -z | while IFS= read -r -d '' link; do
      printf 'link\t%s\t%s\n' "$link" "$(readlink "$link")"
    done
    find . -type d -print | LC_ALL=C sort | sed 's/^/dir\t/'
  )
}

brain_test_isolation_run() (
  set -euo pipefail

  if [[ "$#" -eq 0 ]]; then
    echo 'brain_test_isolation_run requires a command' >&2
    exit 64
  fi

  local real_home="${FINCH_TEST_REAL_HOME:-${HOME:-}}"
  if [[ -z "$real_home" || "$real_home" != /* ]]; then
    echo 'Brain test isolation requires an absolute real HOME' >&2
    exit 64
  fi

  local real_store="$real_home/.finch/brains"
  local before_manifest
  local after_manifest
  local isolated_home
  before_manifest="$(mktemp "${TMPDIR:-/tmp}/finch-brain-manifest-before.XXXXXX")"
  after_manifest="$(mktemp "${TMPDIR:-/tmp}/finch-brain-manifest-after.XXXXXX")"
  isolated_home="$(mktemp -d "${FINCH_TEST_TMP_PARENT:-${TMPDIR:-/tmp}}/finch-brain-test-home.XXXXXX")"

  cleanup() {
    local command_status="$1"
    local guard_status=0

    brain_store_manifest "$real_store" >"$after_manifest"
    if ! cmp -s "$before_manifest" "$after_manifest"; then
      echo "ERROR: real Brain store changed while isolated command ran: $real_store" >&2
      diff -u "$before_manifest" "$after_manifest" >&2 || true
      guard_status=70
    fi

    # mktemp produced this exact path; reject a broad or production-home target.
    if [[ "$isolated_home" == "$real_home" || "$isolated_home" != */finch-brain-test-home.* ]]; then
      echo "ERROR: refusing unsafe Brain test cleanup target: $isolated_home" >&2
      guard_status=70
    else
      rm -rf -- "$isolated_home"
    fi
    rm -f -- "$before_manifest" "$after_manifest"

    if [[ "$guard_status" -ne 0 ]]; then
      exit "$guard_status"
    fi
    exit "$command_status"
  }
  trap 'cleanup $?' EXIT

  brain_store_manifest "$real_store" >"$before_manifest"

  if [[ "$isolated_home" == "$real_home" || "$isolated_home/.finch/brains" == "$real_store" ]]; then
    echo 'Brain test isolation resolved to the production home; refusing to run' >&2
    exit 64
  fi

  mkdir -p "$isolated_home/.finch/brains"
  # Rustup and Cargo default beneath HOME. Pin their existing locations before
  # replacing HOME; neither is Finch state and hiding them makes the wrapper
  # unusable on otherwise correctly configured developer machines.
  if [[ -z "${RUSTUP_HOME:-}" && -d "$real_home/.rustup" ]]; then
    export RUSTUP_HOME="$real_home/.rustup"
  fi
  if [[ -z "${CARGO_HOME:-}" && -d "$real_home/.cargo" ]]; then
    export CARGO_HOME="$real_home/.cargo"
  fi
  export HOME="$isolated_home"
  export FINCH_BRAIN_TEST_HOME="$isolated_home"
  export FINCH_BRAIN_TEST_ROOT="$isolated_home/.finch/brains"

  "$@"
)

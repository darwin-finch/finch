#!/usr/bin/env bash

brain_isolation_canonical_dir() {
  local label="$1" path="$2" canonical original
  if [[ -z "$path" || "$path" != /* || ! -d "$path" || -L "$path" ]]; then
    echo "Brain test isolation requires $label to be an existing, absolute, non-symlink directory" >&2; return 64
  fi
  original="$path"
  canonical="$(cd "$path" 2>/dev/null && pwd -P)" || { echo "Brain test isolation cannot resolve $label" >&2; return 64; }
  if [[ "$original" != "$canonical" || "$original" == *'/../'* || "$original" == *'/./'* ]]; then
    echo "Brain test isolation rejects non-canonical $label" >&2; return 64
  fi
  printf '%s\n' "$canonical"
}

brain_isolation_path_contains() { [[ "$2" == "$1" || "$2" == "$1"/* ]]; }

brain_store_manifest() {
  local root="$1"
  if [[ ! -e "$root" ]]; then printf '%s\n' '<missing>'; return; fi
  [[ -d "$root" && ! -L "$root" ]] || return 1
  (
    set -euo pipefail
    cd "$root" || exit 1
    find . -type f -print0 | LC_ALL=C sort -z | while IFS= read -r -d '' file; do
      local_hash="$(shasum -a 256 "$file")" || exit 1
      printf 'f\0%s\0%s\n' "$file" "${local_hash%% *}"
    done || exit 1
    find . -type l -print0 | LC_ALL=C sort -z | while IFS= read -r -d '' link; do
      local_target="$(readlink "$link")" || exit 1
      printf 'l\0%s\0%s\0' "$link" "$local_target"
    done || exit 1
    find . -type d -print0 | LC_ALL=C sort -z | while IFS= read -r -d '' directory; do printf 'd\0%s\0' "$directory"; done || exit 1
  )
}

brain_manifest_summary() {
  local manifest="$1" bytes digest
  bytes="$(wc -c <"$manifest" 2>/dev/null | tr -d ' ')" || bytes='unavailable'
  digest="$(shasum -a 256 "$manifest" 2>/dev/null | awk '{print $1}')" || digest='unavailable'
  printf 'bytes=%s sha256=%s' "$bytes" "$digest"
}

brain_test_isolation_run() (
  set -uo pipefail
  [[ "$#" -gt 0 ]] || { echo 'brain_test_isolation_run requires a command' >&2; exit 64; }

  local requested_real_home
  if [[ -n "${FINCH_TEST_REAL_HOME+x}" ]]; then requested_real_home="$FINCH_TEST_REAL_HOME"; else requested_real_home="${HOME:-}"; fi
  local real_home
  real_home="$(brain_isolation_canonical_dir 'the real HOME' "$requested_real_home")" || exit $?
  [[ "$real_home" != / && "$real_home" != /tmp && "$real_home" != /var && "$real_home" != /private ]] || { echo 'Brain test isolation rejects a broad real HOME' >&2; exit 64; }

  local requested_temp_parent temp_parent
  if [[ -n "${FINCH_TEST_TMP_PARENT+x}" ]]; then
    requested_temp_parent="$FINCH_TEST_TMP_PARENT"
  else
    requested_temp_parent="$(cd "${TMPDIR:-/tmp}" 2>/dev/null && pwd -P)" || { echo 'Brain test isolation cannot resolve the default temporary directory' >&2; exit 64; }
  fi
  temp_parent="$(brain_isolation_canonical_dir 'the temporary-home parent' "$requested_temp_parent")" || exit $?
  [[ "$temp_parent" != / && "$temp_parent" != /var && "$temp_parent" != /private ]] || { echo 'Brain test isolation rejects a broad temporary-home parent' >&2; exit 64; }

  local real_store="$real_home/.finch/brains"
  if brain_isolation_path_contains "$temp_parent" "$real_store" || brain_isolation_path_contains "$real_store" "$temp_parent"; then
    echo 'Brain test isolation rejects a temporary parent related to the real Brain store' >&2; exit 64
  fi

  local before_manifest after_manifest isolated_home child_pid='' pending_signal=''
  before_manifest="$(mktemp "$temp_parent/finch-brain-manifest-before.XXXXXX")" || exit 74
  after_manifest="$(mktemp "$temp_parent/finch-brain-manifest-after.XXXXXX")" || { rm -f -- "$before_manifest"; exit 74; }
  isolated_home="$(mktemp -d "$temp_parent/finch-brain-test-home.XXXXXX")" || { rm -f -- "$before_manifest" "$after_manifest"; exit 74; }

  forward_signal() {
    pending_signal="$1"
    if [[ -n "$child_pid" ]]; then kill -"$1" -- "-$child_pid" 2>/dev/null || kill -"$1" "$child_pid" 2>/dev/null || true; fi
  }
  trap 'forward_signal INT' INT; trap 'forward_signal TERM' TERM; trap 'forward_signal HUP' HUP
  if [[ -n "${FINCH_TEST_WRAPPER_PID_FILE:-}" ]]; then sh -c 'echo "$PPID"' >"$FINCH_TEST_WRAPPER_PID_FILE"; fi

  if ! brain_store_manifest "$real_store" >"$before_manifest"; then
    rm -rf -- "$isolated_home"; rm -f -- "$before_manifest" "$after_manifest"
    echo 'Brain test isolation could not snapshot the real Brain store' >&2; exit 74
  fi

  mkdir -p "$isolated_home/.finch/brains" || {
    rm -rf -- "$isolated_home"; rm -f -- "$before_manifest" "$after_manifest"; exit 74
  }
  if [[ -z "${RUSTUP_HOME:-}" && -d "$real_home/.rustup" ]]; then export RUSTUP_HOME="$real_home/.rustup"; fi
  if [[ -z "${CARGO_HOME:-}" && -d "$real_home/.cargo" ]]; then export CARGO_HOME="$real_home/.cargo"; fi
  export HOME="$isolated_home" FINCH_BRAIN_TEST_HOME="$isolated_home" FINCH_BRAIN_TEST_ROOT="$isolated_home/.finch/brains"
  export FINCH_BRAIN_TEST_ISOLATED=1 FINCH_TEST_REAL_HOME="$real_home"

  set -m; "$@" & child_pid=$!; set +m
  local command_status=0
  wait "$child_pid" || command_status=$?
  if [[ -n "$pending_signal" ]]; then
    local attempts=0
    while kill -0 -- "-$child_pid" 2>/dev/null && [[ "$attempts" -lt 20 ]]; do sleep 0.1; attempts=$((attempts + 1)); done
    kill -KILL -- "-$child_pid" 2>/dev/null || true
    case "$pending_signal" in INT) command_status=130 ;; TERM) command_status=143 ;; HUP) command_status=129 ;; esac
  fi

  trap - INT TERM HUP
  local guard_status=0
  if ! brain_store_manifest "$real_store" >"$after_manifest"; then
    echo 'ERROR: could not snapshot the real Brain store after the isolated command' >&2; guard_status=74
  elif ! cmp -s "$before_manifest" "$after_manifest"; then
    echo "ERROR: real Brain store manifest changed ($(brain_manifest_summary "$before_manifest") -> $(brain_manifest_summary "$after_manifest"))" >&2; guard_status=70
  fi
  if [[ "$isolated_home" == "$real_home" || "$isolated_home" != "$temp_parent"/finch-brain-test-home.* ]]; then
    echo 'ERROR: refusing unsafe Brain test cleanup target' >&2; guard_status=70
  else
    rm -rf -- "$isolated_home" || guard_status=74
  fi
  rm -f -- "$before_manifest" "$after_manifest" || guard_status=74
  [[ "$guard_status" -eq 0 ]] || exit "$guard_status"
  exit "$command_status"
)

brain_test_isolation_reexec_launcher() {
  local launcher="$1" repo_root; shift
  [[ "${FINCH_BRAIN_TEST_ISOLATED:-}" == 1 ]] && return 0
  repo_root="$(cd "$(dirname "$launcher")/.." && pwd -P)" || exit 64
  exec "$repo_root/scripts/test_brains.sh" "$launcher" "$@"
}

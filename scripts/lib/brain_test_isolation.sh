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

brain_isolation_resolve_store() {
  local real_home="$1" finch_dir="$1/.finch" store
  if [[ -L "$finch_dir" ]]; then echo 'Brain test isolation rejects a symlinked real Finch directory' >&2; return 64; fi
  if [[ -e "$finch_dir" ]]; then
    [[ -d "$finch_dir" ]] || { echo 'Brain test isolation rejects a non-directory real Finch path' >&2; return 64; }
    finch_dir="$(brain_isolation_canonical_dir 'the real Finch directory' "$finch_dir")" || return $?
  fi
  store="$finch_dir/brains"
  if [[ -L "$store" ]]; then echo 'Brain test isolation rejects a symlinked real Brain store' >&2; return 64; fi
  if [[ -e "$store" ]]; then
    [[ -d "$store" ]] || { echo 'Brain test isolation rejects a non-directory real Brain store' >&2; return 64; }
    store="$(brain_isolation_canonical_dir 'the real Brain store' "$store")" || return $?
  fi
  printf '%s\n' "$store"
}

brain_store_manifest() {
  local root="$1"
  if [[ -L "$root" ]]; then
    local dangling_target
    dangling_target="$(readlink "$root")" || return 1
    printf 'root-link\0%s\0' "$dangling_target"
    return
  fi
  if [[ ! -e "$root" ]]; then printf '%s\n' '<missing>'; return; fi
  [[ -d "$root" && ! -L "$root" ]] || return 1
  (
    set -euo pipefail
    cd "$root" || exit 1
    find . -print0 | LC_ALL=C sort -z | while IFS= read -r -d '' entry; do
      if stat -f '%p:%u:%g:%l:%i' "$entry" >/dev/null 2>&1; then
        local_metadata="$(stat -f '%p:%u:%g:%l:%i' "$entry")" || exit 1
      else
        local_metadata="$(stat -c '%f:%u:%g:%h:%i' "$entry")" || exit 1
      fi
      if [[ -L "$entry" ]]; then
        local_target="$(readlink "$entry")" || exit 1; printf 'l\0%s\0%s\0%s\0' "$entry" "$local_metadata" "$local_target"
      elif [[ -f "$entry" ]]; then
        local_hash="$(shasum -a 256 "$entry")" || exit 1; printf 'f\0%s\0%s\0%s\n' "$entry" "$local_metadata" "${local_hash%% *}"
      elif [[ -d "$entry" ]]; then printf 'd\0%s\0%s\0' "$entry" "$local_metadata"
      elif [[ -p "$entry" ]]; then printf 'p\0%s\0%s\0' "$entry" "$local_metadata"
      elif [[ -S "$entry" ]]; then printf 's\0%s\0%s\0' "$entry" "$local_metadata"
      elif [[ -b "$entry" ]]; then printf 'b\0%s\0%s\0' "$entry" "$local_metadata"
      elif [[ -c "$entry" ]]; then printf 'c\0%s\0%s\0' "$entry" "$local_metadata"
      else printf 'o\0%s\0%s\0' "$entry" "$local_metadata"
      fi
    done || exit 1
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

  local real_store
  real_store="$(brain_isolation_resolve_store "$real_home")" || exit $?
  if brain_isolation_path_contains "$temp_parent" "$real_store" || brain_isolation_path_contains "$real_store" "$temp_parent"; then
    echo 'Brain test isolation rejects a temporary parent related to the real Brain store' >&2; exit 64
  fi

  local before_manifest after_manifest isolated_home child_pid='' pending_signal=''
  before_manifest="$(mktemp "$temp_parent/finch-brain-manifest-before.XXXXXX")" || exit 74
  early_cleanup() {
    local status="$1"
    trap - EXIT INT TERM HUP
    [[ -z "${isolated_home:-}" ]] || rm -rf -- "$isolated_home"
    [[ -z "${before_manifest:-}" ]] || rm -f -- "$before_manifest"
    [[ -z "${after_manifest:-}" ]] || rm -f -- "$after_manifest"
    exit "$status"
  }
  trap 'early_cleanup $?' EXIT
  trap 'early_cleanup 130' INT
  trap 'early_cleanup 143' TERM
  trap 'early_cleanup 129' HUP
  after_manifest="$(mktemp "$temp_parent/finch-brain-manifest-after.XXXXXX")" || { rm -f -- "$before_manifest"; exit 74; }
  isolated_home="$(mktemp -d "$temp_parent/finch-brain-test-home.XXXXXX")" || { rm -f -- "$before_manifest" "$after_manifest"; exit 74; }

  forward_signal() {
    pending_signal="$1"
    if [[ -n "$child_pid" ]]; then kill -"$1" -- "-$child_pid" 2>/dev/null || kill -"$1" "$child_pid" 2>/dev/null || true; fi
  }
  trap - EXIT
  trap 'forward_signal INT' INT; trap 'forward_signal TERM' TERM; trap 'forward_signal HUP' HUP

  if ! brain_store_manifest "$real_store" >"$before_manifest"; then
    rm -rf -- "$isolated_home"; rm -f -- "$before_manifest" "$after_manifest"
    echo 'Brain test isolation could not snapshot the real Brain store' >&2; exit 74
  fi

  mkdir -p "$isolated_home/.finch/brains" || {
    rm -rf -- "$isolated_home"; rm -f -- "$before_manifest" "$after_manifest"; exit 74
  }
  local isolation_token="${RANDOM:-0}:$$:$(basename "$isolated_home")"
  if [[ -z "${RUSTUP_HOME:-}" && -d "$real_home/.rustup" ]]; then export RUSTUP_HOME="$real_home/.rustup"; fi
  if [[ -z "${CARGO_HOME:-}" && -d "$real_home/.cargo" ]]; then export CARGO_HOME="$real_home/.cargo"; fi
  export HOME="$isolated_home" FINCH_BRAIN_TEST_HOME="$isolated_home" FINCH_BRAIN_TEST_ROOT="$isolated_home/.finch/brains"
  export FINCH_BRAIN_TEST_ISOLATED=1 FINCH_BRAIN_TEST_TOKEN="$isolation_token" FINCH_BRAIN_TEST_PROOF_FD=9 FINCH_TEST_TMP_PARENT="$temp_parent" FINCH_TEST_REAL_HOME="$real_home"
  exec 9< <(printf '%s\n' "$isolation_token")

  set -m; "$@" & child_pid=$!; set +m
  local command_status=0
  wait "$child_pid" || command_status=$?
  exec 9<&-
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

brain_test_isolation_is_active() {
  [[ "${FINCH_BRAIN_TEST_ISOLATED:-}" == 1 ]] || return 1
  [[ -n "${HOME:-}" && "$HOME" == "${FINCH_BRAIN_TEST_HOME:-}" ]] || return 1
  [[ "${FINCH_BRAIN_TEST_ROOT:-}" == "$HOME/.finch/brains" ]] || return 1
  [[ "$HOME" == "${FINCH_TEST_TMP_PARENT:-}"/finch-brain-test-home.* ]] || return 1
  [[ -d "$FINCH_BRAIN_TEST_ROOT" && ! -L "$FINCH_BRAIN_TEST_ROOT" ]] || return 1
  [[ "$(brain_isolation_resolve_store "$HOME" 2>/dev/null)" == "$FINCH_BRAIN_TEST_ROOT" ]] || return 1
  [[ "${FINCH_BRAIN_TEST_PROOF_FD:-}" == 9 ]] || return 1
  local proof
  IFS= read -r proof <&9 2>/dev/null || return 1
  [[ "$proof" == "${FINCH_BRAIN_TEST_TOKEN:-}" ]] || return 1
  [[ "$(cd "$HOME" 2>/dev/null && pwd -P)" == "$HOME" ]] || return 1
}

brain_test_isolation_reexec_launcher() {
  local launcher="$1" repo_root; shift
  if brain_test_isolation_is_active; then
    if [[ -n "${FINCH_TEST_LAUNCHER_PROBE_FILE:-}" ]]; then printf '%s\n' "$HOME" >"$FINCH_TEST_LAUNCHER_PROBE_FILE"; fi
    return 0
  fi
  repo_root="$(cd "$(dirname "$launcher")/.." && pwd -P)" || exit 64
  exec "$repo_root/scripts/test_brains.sh" "$launcher" "$@"
}

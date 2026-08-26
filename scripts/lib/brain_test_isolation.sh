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

brain_isolation_record_process() {
  local registry="$1" pid="$2" pgid="$3"
  [[ "$pid" =~ ^[0-9]+$ && "$pgid" =~ ^[0-9]+$ ]] || return 0
  printf '%s %s\n' "$pid" "$pgid" >>"$registry"
}

brain_test_isolation_register_owned_pid() {
  local pid="$1" pgid
  [[ -n "${FINCH_TEST_PROCESS_REGISTRY:-}" ]] || return 0
  pgid="$(ps -o pgid= -p "$pid" 2>/dev/null | tr -d ' ')"
  [[ -n "$pgid" ]] || return 0
  brain_isolation_record_process "$FINCH_TEST_PROCESS_REGISTRY" "$pid" "$pgid"
}

brain_isolation_record_descendants() {
  local registry="$1" root_pid="$2"
  ps -eo pid=,ppid=,pgid= 2>/dev/null | awk -v root="$root_pid" '
    { pid[NR]=$1; parent[NR]=$2; group[NR]=$3 }
    END {
      owned[root]=1
      changed=1
      while (changed) {
        changed=0
        for (i=1; i<=NR; i++) {
          if (owned[parent[i]] && !owned[pid[i]]) {
            owned[pid[i]]=1
            changed=1
          }
        }
      }
      for (i=1; i<=NR; i++) if (owned[pid[i]]) print pid[i], group[i]
    }
  ' >>"$registry"
}

brain_isolation_monitor_descendants() {
  local registry="$1" root_pid="$2" stop_file="$3"
  while [[ ! -e "$stop_file" ]]; do
    brain_isolation_record_descendants "$registry" "$root_pid"
    sleep 0.02
  done
  brain_isolation_record_descendants "$registry" "$root_pid"
}

brain_isolation_terminate_owned() {
  local registry="$1" own_pgid pid pgid attempt
  [[ -f "$registry" ]] || return 0
  own_pgid="$(ps -o pgid= -p $$ 2>/dev/null | tr -d ' ')"

  # Signal exact recorded groups first so descendants that changed parent remain
  # covered. Never signal the wrapper's own group.
  while read -r pgid; do
    [[ -n "$pgid" && "$pgid" != "$own_pgid" ]] || continue
    kill -TERM -- "-$pgid" 2>/dev/null || true
  done < <(awk '{print $2}' "$registry" | LC_ALL=C sort -u)
  while read -r pid; do
    [[ -n "$pid" && "$pid" != "$$" ]] || continue
    kill -TERM -- "$pid" 2>/dev/null || true
  done < <(awk '{print $1}' "$registry" | LC_ALL=C sort -u)

  attempt=0
  while [[ "$attempt" -lt 20 ]]; do
    local alive=0
    while read -r pid; do
      [[ -n "$pid" && "$pid" != "$$" ]] || continue
      if kill -0 -- "$pid" 2>/dev/null; then alive=1; break; fi
    done < <(awk '{print $1}' "$registry" | LC_ALL=C sort -u)
    [[ "$alive" -eq 1 ]] || break
    sleep 0.05
    attempt=$((attempt + 1))
  done

  while read -r pgid; do
    [[ -n "$pgid" && "$pgid" != "$own_pgid" ]] || continue
    kill -KILL -- "-$pgid" 2>/dev/null || true
  done < <(awk '{print $2}' "$registry" | LC_ALL=C sort -u)
  while read -r pid; do
    [[ -n "$pid" && "$pid" != "$$" ]] || continue
    kill -KILL -- "$pid" 2>/dev/null || true
  done < <(awk '{print $1}' "$registry" | LC_ALL=C sort -u)
}

brain_test_isolation_run() (
  set +e
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

  local before_manifest after_manifest isolated_home child_pid='' child_pgid=''
  local monitor_pid='' process_registry='' monitor_stop='' pending_signal=''
  before_manifest="$(mktemp "$temp_parent/finch-brain-manifest-before.XXXXXX")" || exit 74
  early_cleanup() {
    local status="$1"
    trap '' INT TERM HUP
    [[ -z "${monitor_stop:-}" ]] || : >"$monitor_stop"
    [[ -z "${monitor_pid:-}" ]] || wait "$monitor_pid" 2>/dev/null || true
    [[ -z "${process_registry:-}" ]] || brain_isolation_terminate_owned "$process_registry"
    [[ -z "${isolated_home:-}" ]] || rm -rf -- "$isolated_home"
    [[ -z "${before_manifest:-}" ]] || rm -f -- "$before_manifest"
    [[ -z "${after_manifest:-}" ]] || rm -f -- "$after_manifest"
    trap - EXIT INT TERM HUP
    exit "$status"
  }
  trap 'early_cleanup $?' EXIT
  trap 'early_cleanup 130' INT
  trap 'early_cleanup 143' TERM
  trap 'early_cleanup 129' HUP
  after_manifest="$(mktemp "$temp_parent/finch-brain-manifest-after.XXXXXX")" || { rm -f -- "$before_manifest"; exit 74; }
  isolated_home="$(mktemp -d "$temp_parent/finch-brain-test-home.XXXXXX")" || { rm -f -- "$before_manifest" "$after_manifest"; exit 74; }
  process_registry="$isolated_home/.finch/owned-processes"
  monitor_stop="$isolated_home/.finch/process-monitor.stop"

  forward_signal() {
    pending_signal="$1"
    if [[ -n "$child_pid" ]]; then kill -"$1" -- "-$child_pid" 2>/dev/null || kill -"$1" "$child_pid" 2>/dev/null || true; fi
  }
  trap 'forward_signal INT' INT; trap 'forward_signal TERM' TERM; trap 'forward_signal HUP' HUP

  if ! brain_store_manifest "$real_store" >"$before_manifest"; then
    rm -rf -- "$isolated_home"; rm -f -- "$before_manifest" "$after_manifest"
    echo 'Brain test isolation could not snapshot the real Brain store' >&2; exit 74
  fi

  mkdir -p "$isolated_home/.finch/brains" || {
    rm -rf -- "$isolated_home"; rm -f -- "$before_manifest" "$after_manifest"; exit 74
  }
  : >"$process_registry" || exit 74
  local isolation_token="${RANDOM:-0}:$$:$(basename "$isolated_home")"
  if [[ -z "${RUSTUP_HOME:-}" && -d "$real_home/.rustup" ]]; then export RUSTUP_HOME="$real_home/.rustup"; fi
  if [[ -z "${CARGO_HOME:-}" && -d "$real_home/.cargo" ]]; then export CARGO_HOME="$real_home/.cargo"; fi
  export HOME="$isolated_home" FINCH_BRAIN_TEST_HOME="$isolated_home" FINCH_BRAIN_TEST_ROOT="$isolated_home/.finch/brains"
  export FINCH_TEST_IPC_SOCKET="$isolated_home/.finch/daemon.sock"
  export FINCH_BRAIN_TEST_ISOLATED=1 FINCH_BRAIN_TEST_TOKEN="$isolation_token" FINCH_BRAIN_TEST_PROOF_FD=9 FINCH_TEST_TMP_PARENT="$temp_parent" FINCH_TEST_REAL_HOME="$real_home"
  export FINCH_TEST_PROCESS_REGISTRY="$process_registry"
  exec 9< <(printf '%s\n' "$isolation_token")

  set -m; "$@" & child_pid=$!; set +m
  child_pgid="$(ps -o pgid= -p "$child_pid" 2>/dev/null | tr -d ' ')"
  [[ -z "$child_pgid" ]] || brain_isolation_record_process "$process_registry" "$child_pid" "$child_pgid"
  brain_isolation_monitor_descendants "$process_registry" "$child_pid" "$monitor_stop" & monitor_pid=$!
  local command_status=0
  wait "$child_pid" || command_status=$?
  : >"$monitor_stop"
  wait "$monitor_pid" 2>/dev/null || true
  monitor_pid=''
  brain_isolation_terminate_owned "$process_registry"
  exec 9<&-
  if [[ -n "$pending_signal" ]]; then
    local attempts=0
    while kill -0 -- "-$child_pid" 2>/dev/null && [[ "$attempts" -lt 20 ]]; do sleep 0.1; attempts=$((attempts + 1)); done
    kill -KILL -- "-$child_pid" 2>/dev/null || true
    case "$pending_signal" in INT) command_status=130 ;; TERM) command_status=143 ;; HUP) command_status=129 ;; esac
  fi

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
  trap '' INT TERM HUP
  trap - EXIT
  [[ "$guard_status" -eq 0 ]] || exit "$guard_status"
  case "$pending_signal" in INT) command_status=130 ;; TERM) command_status=143 ;; HUP) command_status=129 ;; esac
  trap - INT TERM HUP
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
    if [[ -n "${FINCH_TEST_LAUNCHER_PROBE_FILE:-}" ]]; then
      printf '%s\n' "$HOME" >"$FINCH_TEST_LAUNCHER_PROBE_FILE"
      [[ "${FINCH_TEST_LAUNCHER_PROBE_ONLY:-}" != 1 ]] || exit 0
    fi
    return 0
  fi
  repo_root="$(cd "$(dirname "$launcher")/.." && pwd -P)" || exit 64
  exec "$repo_root/scripts/test_brains.sh" "$launcher" "$@"
}

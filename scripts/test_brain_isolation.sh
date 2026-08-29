#!/usr/bin/env bash
set -Eeuo pipefail

phase=setup
trap 'status=$?; echo "Brain isolation regression failed in phase: $phase (line $LINENO, status $status)" >&2; exit "$status"' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source "$repo_root/scripts/lib/brain_test_isolation.sh"
default_supervisor="$repo_root/target/debug/finch-test-supervisor-pinned"
[[ -x "$default_supervisor" ]] || default_supervisor="$repo_root/target/debug/finch-test-supervisor"
supervisor="${FINCH_TEST_SUPERVISOR_BIN:-$default_supervisor}"
[[ -x "$supervisor" ]] || { echo 'build finch-test-supervisor before running isolation regressions' >&2; exit 69; }

scratch="$(mktemp -d "$(cd "${TMPDIR:-/tmp}" && pwd -P)/finch-brain-isolation-regression.XXXXXX")"
sentinel_pid=''
signaler_pid=''
substitution_pid=''
supervisor_backup=''
substitution_restored=''
cleanup_regression() {
  if [[ -n "$signaler_pid" ]]; then wait "$signaler_pid" 2>/dev/null || true; fi
  if [[ -n "$sentinel_pid" ]]; then printf '\n' >&7 2>/dev/null || true; wait "$sentinel_pid" 2>/dev/null || true; fi
  if [[ -n "$supervisor_backup" && -e "$supervisor_backup" ]]; then
    mv -f -- "$supervisor_backup" "$supervisor"
  fi
  if [[ -n "$substitution_restored" ]]; then : >"$substitution_restored"; fi
  if [[ -n "$substitution_pid" ]]; then
    kill "$substitution_pid" 2>/dev/null || true
    wait "$substitution_pid" 2>/dev/null || true
  fi
  exec 7>&- 2>/dev/null || true
  rm -rf -- "$scratch"
}
trap cleanup_regression EXIT

fake_home="$scratch/real-home"
temp_parent="$scratch/temp-homes"
mkdir -p "$fake_home/.finch/brains/existing" "$temp_parent"
printf 'keep me\n' >"$fake_home/.finch/brains/existing/events.jsonl"
printf 'keep node\n' >"$fake_home/.finch/node_id"

run_isolated() {
  FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$temp_parent" "$supervisor" "$@"
}

expect_supervisor_70() {
  if "$@" >/dev/null 2>&1; then
    echo 'expected supervisor validation failure' >&2
    exit 1
  else
    test "$?" -eq 70
  fi
}

# Canonical path validation fails before a child is launched or a disposable
# HOME is allocated.
phase=canonical-path-rejection
phase=canonical-missing-home
expect_supervisor_70 env -u HOME -u FINCH_TEST_REAL_HOME \
  FINCH_TEST_TMP_PARENT="$temp_parent" "$supervisor" true
phase=canonical-trailing-separator
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$fake_home/" \
  FINCH_TEST_TMP_PARENT="$temp_parent" "$supervisor" true
ln -s "$fake_home" "$scratch/home-link"
phase=canonical-symlink-home
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$scratch/home-link" \
  FINCH_TEST_TMP_PARENT="$temp_parent" "$supervisor" true
phase=canonical-relative-temp-parent
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$fake_home" \
  FINCH_TEST_TMP_PARENT=relative "$supervisor" true
ln -s "$temp_parent" "$scratch/temp-link"
phase=canonical-symlink-temp-parent
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$fake_home" \
  FINCH_TEST_TMP_PARENT="$scratch/temp-link" "$supervisor" true
phase=canonical-overlapping-scratch
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$fake_home" \
  FINCH_TEST_TMP_PARENT="$scratch" "$supervisor" true
phase=canonical-overlapping-home
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$fake_home" \
  FINCH_TEST_TMP_PARENT="$fake_home" "$supervisor" true
phase=canonical-overlapping-store
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$fake_home" \
  FINCH_TEST_TMP_PARENT="$fake_home/.finch/brains" "$supervisor" true
phase=canonical-rejection-clean
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# An absent parent uses the canonical platform temporary directory. Caller
# input remains strict: the explicit symlink case above must still fail.
phase=supervisor-canonicalizes-default-temp-parent
env -u FINCH_TEST_TMP_PARENT FINCH_TEST_REAL_HOME="$fake_home" TMPDIR="$temp_parent/" \
  "$supervisor" true
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# Cargo may relink its ordinary target/debug output while it builds a later
# integration test. CI therefore runs from a separately staged executable
# whose pathname Cargo does not own. Replacing that staged pathname while its
# process is live must invalidate the inherited proof, and restoring the
# original inode must leave the chosen artifact unchanged for later tests.
case "$supervisor" in
  "$repo_root"/target/debug/finch-test-supervisor-pinned|\
  "$repo_root"/target/release/finch-test-supervisor-pinned)
    phase=supervisor-substitution-rejected
    substitution_ready="$scratch/substitution.ready"
    substitution_continue="$scratch/substitution.continue"
    substitution_rejected="$scratch/substitution.rejected"
    substitution_restored="$scratch/substitution.restored"
    supervisor_backup="$scratch/supervisor.original"
    supervisor_identity="$(brain_isolation_file_identity "$supervisor")"
    FINCH_SUBSTITUTION_READY="$substitution_ready" \
    FINCH_SUBSTITUTION_CONTINUE="$substitution_continue" \
    FINCH_SUBSTITUTION_REJECTED="$substitution_rejected" \
    FINCH_SUBSTITUTION_RESTORED="$substitution_restored" \
      run_isolated bash -c '
        : >"$FINCH_SUBSTITUTION_READY"
        for _ in {1..400}; do
          [[ -e "$FINCH_SUBSTITUTION_CONTINUE" ]] && break
          sleep 0.01
        done
        [[ -e "$FINCH_SUBSTITUTION_CONTINUE" ]]
        if "$FINCH_TEST_SUPERVISOR_BIN" --verify-inherited-proof >/dev/null 2>&1; then
          exit 1
        fi
        : >"$FINCH_SUBSTITUTION_REJECTED"
        for _ in {1..400}; do
          [[ -e "$FINCH_SUBSTITUTION_RESTORED" ]] && exit 0
          sleep 0.01
        done
        exit 1
      ' & substitution_pid=$!
    for _ in {1..400}; do
      [[ -e "$substitution_ready" ]] && break
      sleep 0.01
    done
    test -e "$substitution_ready"
    mv -- "$supervisor" "$supervisor_backup"
    install -m 0555 "$supervisor_backup" "$supervisor"
    test "$(brain_isolation_file_identity "$supervisor")" != "$supervisor_identity"
    : >"$substitution_continue"
    for _ in {1..400}; do
      [[ -e "$substitution_rejected" ]] && break
      sleep 0.01
    done
    test -e "$substitution_rejected"
    mv -f -- "$supervisor_backup" "$supervisor"
    supervisor_backup=''
    test "$(brain_isolation_file_identity "$supervisor")" = "$supervisor_identity"
    : >"$substitution_restored"
    wait "$substitution_pid"
    substitution_pid=''
    substitution_restored=''
    test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"
    ;;
esac

# The public launcher applies the same canonical-default contract before it
# delegates to the supervisor.
phase=launcher-canonicalizes-default-temp-parent
env -u FINCH_TEST_TMP_PARENT FINCH_TEST_REAL_HOME="$fake_home" TMPDIR="$temp_parent/" \
  "$repo_root/scripts/test_brains.sh" true
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

hostile_home="$scratch/hostile-home"
hostile_target="$scratch/effective-production-finch"
mkdir -p "$hostile_home" "$hostile_target/brains"
printf 'production sentinel\n' >"$hostile_target/brains/sentinel"
ln -s "$hostile_target" "$hostile_home/.finch"
phase=canonical-symlinked-real-store
expect_supervisor_70 env FINCH_TEST_REAL_HOME="$hostile_home" \
  FINCH_TEST_TMP_PARENT="$temp_parent" "$supervisor" true
test "$(cat "$hostile_target/brains/sentinel")" = 'production sentinel'
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

created="$scratch/created-home"
phase=sealed-proof-and-endpoints
FINCH_TEST_BRAIN_ADDR=127.0.0.1:11436 FINCH_TEST_DAEMON_ADDR=127.0.0.1:11435 \
FINCH_TEST_BRAIN_PASSWORD=ambient-password FINCH_CREATED_HOME="$created" \
FINCH_PROOF_HELPER="$repo_root/scripts/lib/brain_test_isolation.sh" run_isolated bash -c '
  source "$FINCH_PROOF_HELPER"
  brain_test_isolation_is_active
  test "$FINCH_BRAIN_TEST_ROOT" = "$HOME/.finch/brains"
  test "$FINCH_BRAIN_TEST_NO_AUTO_SPAWN" = 1
  test "$FINCH_BRAIN_TEST_PROOF_BACKUP_FD" = 108
  test "$FINCH_TEST_BRAIN_ADDR" != 127.0.0.1:11436
  test "$FINCH_TEST_DAEMON_ADDR" != 127.0.0.1:11435
  test "$FINCH_TEST_BRAIN_PASSWORD" != ambient-password
  [[ "$FINCH_TEST_BRAIN_PASSWORD" =~ ^test-[0-9a-f]{32}$ ]]
  ! printf attacker >&9
  ! printf attacker >&108
  ! sh -c ": >/dev/fd/9"
  ! sh -c ": >/dev/fd/108"
  printf "%s\n" "$HOME" >"$FINCH_CREATED_HOME"
  printf test >"$FINCH_BRAIN_TEST_ROOT/test-created"
'
isolated_home="$(cat "$created")"
test ! -e "$isolated_home"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"
test "$(cat "$fake_home/.finch/brains/existing/events.jsonl")" = 'keep me'

# Ambient XDG/model-cache/temp and corpus paths are not allowed to redirect
# isolated reads or writes back into caller-owned state.
hostile_state="$scratch/hostile-state"
mkdir -p "$hostile_state"/{config,cache,data,state,hf,hub,transformers,tmp}
printf 'unchanged\n' >"$hostile_state/sentinel"
phase=hostile-state-environment-sealed
XDG_CONFIG_HOME="$hostile_state/config" XDG_CACHE_HOME="$hostile_state/cache" \
XDG_DATA_HOME="$hostile_state/data" XDG_STATE_HOME="$hostile_state/state" \
HF_HOME="$hostile_state/hf" HUGGINGFACE_HUB_CACHE="$hostile_state/hub" \
TRANSFORMERS_CACHE="$hostile_state/transformers" TMPDIR="$hostile_state/tmp" \
FINCH_WIRE_CORPUS_PATH="$hostile_state/corpus.jsonl" run_isolated bash -c '
  for variable in XDG_CONFIG_HOME XDG_CACHE_HOME XDG_DATA_HOME XDG_STATE_HOME \
    HF_HOME HUGGINGFACE_HUB_CACHE TRANSFORMERS_CACHE TMPDIR; do
    value="${!variable}"
    case "$value" in "$HOME"/*) ;; *) exit 1 ;; esac
    printf test >"$value/isolation-probe"
  done
  test -z "${FINCH_WIRE_CORPUS_PATH:-}"
  mkdir -p "$HOME/.finch/metrics"
  printf test >"$HOME/.finch/config.toml"
  printf test >"$HOME/.finch/feedback.jsonl"
  printf test >"$HOME/.finch/metrics/isolation.jsonl"
'
test "$(cat "$hostile_state/sentinel")" = unchanged
test -z "$(find "$hostile_state" -mindepth 2 -print -quit)"
test ! -e "$hostile_state/corpus.jsonl"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# Concurrent ordinary invocations receive disjoint process groups, state
# roots, sockets, listener addresses, and credentials without mutating global
# parent-shell environment.
parallel_a="$scratch/parallel-a"
parallel_b="$scratch/parallel-b"
phase=parallel-supervisor-isolation
FINCH_PARALLEL_RECORD="$parallel_a" run_isolated bash -c '
  printf "%s|%s|%s|%s|%s|%s|%s|%s|%s\n" \
    "$HOME" "$FINCH_BRAIN_TEST_ROOT" "$FINCH_TEST_SOCKET_ROOT" \
    "$FINCH_TEST_IPC_SOCKET" "$FINCH_TEST_BRAIN_ADDR" \
    "$FINCH_TEST_DAEMON_ADDR" "$FINCH_TEST_BRAIN_PASSWORD" \
    "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" >"$FINCH_PARALLEL_RECORD"
  sleep 0.2
' & parallel_pid_a=$!
FINCH_PARALLEL_RECORD="$parallel_b" run_isolated bash -c '
  printf "%s|%s|%s|%s|%s|%s|%s|%s|%s\n" \
    "$HOME" "$FINCH_BRAIN_TEST_ROOT" "$FINCH_TEST_SOCKET_ROOT" \
    "$FINCH_TEST_IPC_SOCKET" "$FINCH_TEST_BRAIN_ADDR" \
    "$FINCH_TEST_DAEMON_ADDR" "$FINCH_TEST_BRAIN_PASSWORD" \
    "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" >"$FINCH_PARALLEL_RECORD"
  sleep 0.2
' & parallel_pid_b=$!
wait "$parallel_pid_a"
wait "$parallel_pid_b"
test "$(awk -F '|' '{ for (field = 1; field <= NF; field++) print field "|" $field }' \
  "$parallel_a" "$parallel_b" | sort -u | wc -l | tr -d ' ')" -eq 18
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# Environment strings plus a caller-created descriptor are not wrapper
# authority. This linked, caller-owned proof must fail before any launcher can
# treat the process as isolated.
forged_proof="$scratch/forged-proof"
phase=forged-shell-proof-rejection
printf '%s\n' forged "$fake_home" "$fake_home/.finch/brains" 0:0 0:0 \
  127.0.0.1:1 127.0.0.1:2 forged "$fake_home/.finch/daemon.sock" \
  "$fake_home/.finch" 0:0 "$$" /bin/sh 0:0 >"$forged_proof"
(
  exec 9<"$forged_proof"
  FINCH_BRAIN_TEST_ISOLATED=1 FINCH_BRAIN_TEST_PROOF_FD=9 \
    FINCH_BRAIN_TEST_TOKEN=forged FINCH_TEST_SUPERVISOR_PID="$$" \
    FINCH_PROOF_HELPER="$repo_root/scripts/lib/brain_test_isolation.sh" \
    bash -c 'source "$FINCH_PROOF_HELPER"; ! brain_test_isolation_is_active'
)

phase=ordinary-nonzero-cleanup
if run_isolated bash -c 'exit 23'; then exit 1; else test "$?" -eq 23; fi
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# A Rust panic after spawning a TERM-resistant descendant still leaves both
# processes inside the outer supervisor's group. The group must be quiescent
# before its disposable HOME is removed.
panic_pid_file="$scratch/panic-descendant.pid"
panic_home_file="$scratch/panic-home"
phase=rust-panic-descendant-cleanup
panic_status=0
FINCH_TEST_PANIC_DESCENDANT_PID_FILE="$panic_pid_file" \
FINCH_TEST_PANIC_HOME_FILE="$panic_home_file" \
  run_isolated "$supervisor" --child-panic-probe >/dev/null 2>&1 || panic_status=$?
test "$panic_status" -ne 0
panic_pid="$(cat "$panic_pid_file")"
! /bin/ps -p "$panic_pid" -o pid= 2>/dev/null | grep -q '[0-9]'
test ! -e "$(cat "$panic_home_file")"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# Model an external test timeout while both the leader and a TERM-resistant
# descendant are live. The watchdog targets only this supervisor instance.
timeout_target_file="$scratch/timeout-target"
timeout_descendant_pid_file="$scratch/timeout-descendant.pid"
timeout_home_file="$scratch/timeout-home"
phase=timeout-descendant-cleanup
(
  for _ in {1..400}; do
    if [[ -s "$timeout_target_file" ]]; then
      kill -TERM "$(cat "$timeout_target_file")"
      exit 0
    fi
    sleep 0.005
  done
  exit 91
) & signaler_pid=$!
timeout_status=0
FINCH_TIMEOUT_TARGET_FILE="$timeout_target_file" \
FINCH_TIMEOUT_DESCENDANT_PID_FILE="$timeout_descendant_pid_file" \
FINCH_TIMEOUT_HOME_FILE="$timeout_home_file" run_isolated bash -c '
  (trap "" TERM HUP INT; printf "%s\n" "$BASHPID" >"$FINCH_TIMEOUT_DESCENDANT_PID_FILE"; sleep 30) &
  printf "%s\n" "$HOME" >"$FINCH_TIMEOUT_HOME_FILE"
  printf "%s\n" "$FINCH_TEST_SUPERVISOR_PID" >"$FINCH_TIMEOUT_TARGET_FILE"
  sleep 30
' || timeout_status=$?
signaler_status=0
wait "$signaler_pid" || signaler_status=$?
signaler_pid=''
test "$signaler_status" -eq 0
test "$timeout_status" -eq 143
timeout_descendant_pid="$(cat "$timeout_descendant_pid_file")"
! /bin/ps -p "$timeout_descendant_pid" -o pid= 2>/dev/null | grep -q '[0-9]'
test ! -e "$(cat "$timeout_home_file")"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# A caller-controlled PATH cannot hide a live group member from the
# supervisor. The external observer proves HOME exists for the descendant's
# entire lifetime; cleanup happens only after the trusted /bin/ps inspection
# sees the group quiesce.
inspection_bin="$scratch/inspection-bin"
inspection_called="$scratch/shadow-ps-called"
inspection_pid="$scratch/inspection-descendant.pid"
inspection_home="$scratch/inspection-home"
inspection_observer="$scratch/inspection-observer"
phase=successful-empty-shadow-ps-cannot-hide-descendant
mkdir "$inspection_bin"
printf '%s\n' '#!/bin/sh' ': >"$FINCH_SHADOW_PS_CALLED"' 'exit 0' >"$inspection_bin/ps"
chmod +x "$inspection_bin/ps"
(
  while [[ ! -s "$inspection_pid" || ! -s "$inspection_home" ]]; do sleep 0.01; done
  observed_pid="$(cat "$inspection_pid")"
  observed_home="$(cat "$inspection_home")"
  while /bin/ps -p "$observed_pid" -o pid= 2>/dev/null | grep -q '[0-9]'; do
    test -d "$observed_home" || exit 1
    sleep 0.01
  done
  printf survived >"$inspection_observer"
) &
inspection_observer_pid=$!
if FINCH_DESCENDANT_PID_FILE="$inspection_pid" FINCH_OBSERVED_HOME="$inspection_home" \
  FINCH_SHADOW_PS_CALLED="$inspection_called" PATH="$inspection_bin:$PATH" \
  run_isolated bash -c '
    printf "%s\n" "$HOME" >"$FINCH_OBSERVED_HOME"
    (trap "" TERM HUP INT; printf "%s\n" "$BASHPID" >"$FINCH_DESCENDANT_PID_FILE"; sleep 30) &
    exit 31
  '; then
  exit 1
else
  test "$?" -eq 31
fi
wait "$inspection_observer_pid"
test "$(cat "$inspection_observer")" = survived
test ! -e "$inspection_called"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# An actual inspection error still fails closed and preserves both disposable
# roots for diagnosis. This is distinct from PATH shadowing, which cannot
# influence the trusted platform inspection at all.
phase=inspection-failure-preserves-home
if FINCH_TEST_FORCE_GROUP_INSPECTION_FAILURE=1 \
  run_isolated true 2>"$scratch/inspection.err"; then
  exit 1
else
  test "$?" -eq 70
fi
preserved_home="$(find "$temp_parent" -type d -name 'finch-brain-test-home.*' -print -quit)"
test -n "$preserved_home" && test -d "$preserved_home"
rg -q 'process group was not quiescent' "$scratch/inspection.err"
preserved_socket_root="$(sed -n 's/.*socket root at \([^ ]*\) because.*/\1/p' "$scratch/inspection.err")"
[[ "$preserved_socket_root" == /private/tmp/ft.* || "$preserved_socket_root" == /tmp/ft.* ]]
test -d "$preserved_socket_root"
rm -rf -- "$preserved_home"
rm -rf -- "$preserved_socket_root"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# A normally exiting leader cannot strand a TERM-ignoring member of its owned
# process group. The leader remains unreaped until that group is quiescent.
normal_descendant_pid_file="$scratch/normal-descendant.pid"
phase=normal-exit-term-resistant-descendant
if FINCH_DESCENDANT_PID_FILE="$normal_descendant_pid_file" run_isolated bash -c '
  (trap "" TERM HUP INT; echo "$BASHPID" >"$FINCH_DESCENDANT_PID_FILE"; sleep 30) &
  exit 29
'; then
  exit 1
else
  test "$?" -eq 29
fi
normal_descendant_pid="$(cat "$normal_descendant_pid_file")"
! /bin/ps -p "$normal_descendant_pid" -o pid= 2>/dev/null | grep -q '[0-9]'
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

phase=real-store-manifest-guard
if FINCH_REAL_STORE="$fake_home/.finch/brains/existing" \
  run_isolated bash -c 'printf changed >"$FINCH_REAL_STORE/events.jsonl"'; then
  exit 1
else
  test "$?" -eq 70
fi
printf 'keep me\n' >"$fake_home/.finch/brains/existing/events.jsonl"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

phase=real-node-id-manifest-guard
if FINCH_REAL_NODE_ID="$fake_home/.finch/node_id" \
  run_isolated bash -c 'printf changed >"$FINCH_REAL_NODE_ID"'; then
  exit 1
else
  test "$?" -eq 70
fi
printf 'keep node\n' >"$fake_home/.finch/node_id"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

phase=real-node-id-missing-stays-missing
rm "$fake_home/.finch/node_id"
run_isolated true
test ! -e "$fake_home/.finch/node_id"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

phase=real-node-id-symlink-stays-unchanged
printf 'symlink target\n' >"$scratch/node-id-target"
ln -s "$scratch/node-id-target" "$fake_home/.finch/node_id"
node_link_before="$(readlink "$fake_home/.finch/node_id")"
node_target_before="$(shasum -a 256 "$scratch/node-id-target" | awk '{print $1}')"
run_isolated true
test -L "$fake_home/.finch/node_id"
test "$(readlink "$fake_home/.finch/node_id")" = "$node_link_before"
test "$(shasum -a 256 "$scratch/node-id-target" | awk '{print $1}')" = "$node_target_before"
rm "$fake_home/.finch/node_id"
printf 'keep node\n' >"$fake_home/.finch/node_id"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

# Manifest failures disclose only the parent-held digests, never path names or
# symlink targets from the real store.
diagnostic="$scratch/manifest-diagnostic"
phase=manifest-directory-status
directory_status=0
FINCH_REAL_STORE="$fake_home/.finch/brains" \
  run_isolated bash -c 'mkdir "$FINCH_REAL_STORE/secret-directory"' \
  2>"$diagnostic" || directory_status=$?
[[ "$directory_status" == 70 ]] || {
  echo "directory manifest adversary returned $directory_status, expected 70" >&2
  exit 1
}
phase=manifest-directory-redaction
if rg -q 'secret-directory|existing|events.jsonl' "$diagnostic"; then
  echo 'directory manifest diagnostic disclosed a protected path' >&2
  exit 1
fi
rmdir "$fake_home/.finch/brains/secret-directory"

phase=manifest-symlink-status
symlink_status=0
FINCH_REAL_STORE="$fake_home/.finch/brains" \
  run_isolated bash -c \
  'ln -s /private/secret-target "$FINCH_REAL_STORE/secret-link"' \
  2>"$diagnostic" || symlink_status=$?
[[ "$symlink_status" == 70 ]] || {
  echo "symlink manifest adversary returned $symlink_status, expected 70" >&2
  exit 1
}
phase=manifest-symlink-redaction
if rg -q 'secret-link|secret-target|existing|events.jsonl' "$diagnostic"; then
  echo 'symlink manifest diagnostic disclosed a protected path' >&2
  exit 1
fi
rm "$fake_home/.finch/brains/secret-link"

case "$(uname -s)" in
  Darwin) original_mode="$(stat -f '%Lp' "$fake_home/.finch/brains/existing/events.jsonl")" ;;
  Linux) original_mode="$(stat -c '%a' "$fake_home/.finch/brains/existing/events.jsonl")" ;;
  *) exit 1 ;;
esac
phase=manifest-mode-status
mode_status=0
run_isolated chmod 600 "$fake_home/.finch/brains/existing/events.jsonl" \
  2>"$diagnostic" || mode_status=$?
[[ "$mode_status" == 70 ]] || {
  echo "mode manifest adversary returned $mode_status, expected 70" >&2
  exit 1
}
chmod "$original_mode" "$fake_home/.finch/brains/existing/events.jsonl"

phase=manifest-fifo-status
fifo_status=0
FINCH_REAL_STORE="$fake_home/.finch/brains" \
  run_isolated bash -c 'mkfifo "$FINCH_REAL_STORE/secret-fifo"' \
  2>"$diagnostic" || fifo_status=$?
[[ "$fifo_status" == 70 ]] || {
  echo "FIFO manifest adversary returned $fifo_status, expected 70" >&2
  exit 1
}
phase=manifest-fifo-redaction
if rg -q 'secret-fifo|existing|events.jsonl' "$diagnostic"; then
  echo 'FIFO manifest diagnostic disclosed a protected path' >&2
  exit 1
fi
rm "$fake_home/.finch/brains/secret-fifo"

# Force the metadata/open race window: after the supervisor has classified a
# regular file, replace its name with a FIFO. Descriptor-relative O_NOFOLLOW +
# O_NONBLOCK traversal must reject the identity change without hanging.
phase=manifest-swap-to-fifo-status
race_name=manifest-race-node
race_path="$fake_home/.finch/brains/$race_name"
(
  for _ in {1..400}; do
    race_ready="$(find "$temp_parent" -maxdepth 2 -name .manifest-race-ready -print -quit)"
    [[ -n "$race_ready" ]] && break
    sleep 0.005
  done
  [[ -n "$race_ready" && -e "$race_ready" ]]
  rm -f -- "$race_path"
  mkfifo "$race_path"
  : >"$(dirname "$race_ready")/.manifest-race-continue"
) & race_swapper_pid=$!
race_status=0
FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$temp_parent" \
  FINCH_TEST_MANIFEST_RACE_NAME="$race_name" FINCH_REAL_STORE="$fake_home/.finch/brains" \
  "$supervisor" bash -c 'printf regular >"$FINCH_REAL_STORE/manifest-race-node"' \
  2>"$diagnostic" || race_status=$?
wait "$race_swapper_pid"
[[ "$race_status" == 70 ]] || {
  echo "manifest swap adversary returned $race_status, expected 70" >&2
  exit 1
}
phase=manifest-swap-to-fifo-redaction
if rg -q "$race_name|existing|events.jsonl" "$diagnostic"; then
  echo 'manifest swap diagnostic disclosed a protected path' >&2
  exit 1
fi
rm -f -- "$race_path"

socket_path="$fake_home/.finch/brains/secret-socket"
if [[ "${#socket_path}" -lt 100 ]]; then
  phase=manifest-socket-status
  socket_status=0
  FINCH_REAL_STORE="$fake_home/.finch/brains" \
    run_isolated "$supervisor" --child-socket-manifest-probe \
    2>"$diagnostic" || socket_status=$?
  [[ "$socket_status" == 70 ]] || {
    echo "socket manifest adversary returned $socket_status, expected 70" >&2
    exit 1
  }
  phase=manifest-socket-redaction
  if rg -q 'secret-socket|existing|events.jsonl' "$diagnostic"; then
    echo 'socket manifest diagnostic disclosed a protected path' >&2
    exit 1
  fi
  rm "$socket_path"
fi

# A first, external signal arriving only after the leader is a retained zombie
# and a TERM-resistant descendant has entered teardown must still determine the
# final status. Sampling only before teardown would miss this signal.
stubborn_pid_file="$scratch/stubborn.pid"
stubborn_ready_file="$scratch/stubborn.ready"
stubborn_term_file="$scratch/stubborn.term"
stubborn_target_file="$scratch/stubborn.target"
stubborn_home_file="$scratch/stubborn.home"
late_signal_file="$scratch/late-signal.observed"
phase=signal-during-teardown
(
  for _ in {1..400}; do
    if [[ -s "$stubborn_target_file" && -s "$stubborn_term_file" ]]; then
      read -r supervisor_pid leader_pid <"$stubborn_target_file"
      printf '%s\n' "$leader_pid" >"$late_signal_file"
      kill -TERM "$supervisor_pid"
      exit 0
    fi
    sleep 0.005
  done
  exit 91
) & signaler_pid=$!
signal_status=0
FINCH_STUBBORN_PID_FILE="$stubborn_pid_file" FINCH_STUBBORN_READY_FILE="$stubborn_ready_file" \
FINCH_STUBBORN_TERM_FILE="$stubborn_term_file" FINCH_STUBBORN_TARGET_FILE="$stubborn_target_file" \
FINCH_STUBBORN_HOME_FILE="$stubborn_home_file" run_isolated bash -c '
  leader_pid=$BASHPID
  "$FINCH_TEST_SUPERVISOR_BIN" --child-stubborn-probe &
  while [[ ! -s "$FINCH_STUBBORN_READY_FILE" ]]; do sleep 0.005; done
  printf "%s %s\n" "$FINCH_TEST_SUPERVISOR_PID" "$leader_pid" >"$FINCH_STUBBORN_TARGET_FILE"
  printf "%s\n" "$HOME" >"$FINCH_STUBBORN_HOME_FILE"
  printf "%s\n" "$leader_pid" >"$FINCH_STUBBORN_PID_FILE"
  exit 0
' || signal_status=$?
signaler_status=0
wait "$signaler_pid" || signaler_status=$?
signaler_pid=''
test "$signaler_status" -eq 0
test "$signal_status" -eq 143
test -s "$late_signal_file" && test -s "$stubborn_term_file"
stubborn_group="$(cat "$stubborn_pid_file")"
! kill -0 -- "-$stubborn_group" 2>/dev/null
test ! -e "$(cat "$stubborn_home_file")"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

launchers=(demo_boot.sh smoke_vm_wire_provider.sh stress_test.sh test_persistence.sh test_server.sh test_tool_passthrough.sh test_tui_debug.sh)
phase=launcher-probe-closure
for launcher in "${launchers[@]}"; do
  phase="launcher-probe-closure:$launcher"
  launcher_probe="$scratch/probe-$launcher"
  FINCH_TEST_PROOF_DIAGNOSTICS=1 FINCH_TEST_LAUNCHER_PROBE_FILE="$launcher_probe" \
    FINCH_TEST_LAUNCHER_PROBE_ONLY=1 \
    run_isolated "$repo_root/scripts/$launcher"
  [[ "$(cat "$launcher_probe")" == "$temp_parent"/finch-brain-test-home.* ]]
done

# Keep the executable/integration inventory closed. Any newly added script or
# integration entrypoint that mentions Brain, daemon, IPC, Finch binaries, or
# Cargo test execution must be classified in tests/BRAIN_TEST_INVENTORY.md and
# added to the supervised closure before this gate can pass.
phase=brain-entrypoint-inventory
script_inventory="$(
  cd "$repo_root"
  rg -l -i 'brain|finch daemon|target/(debug|release)/finch|cargo test' \
    scripts --glob '*.sh' | sort
)"
expected_script_inventory="$(cat <<'EOF'
scripts/demo_boot.sh
scripts/lib/brain_test_isolation.sh
scripts/smoke_vm_wire_provider.sh
scripts/stress_test.sh
scripts/test_brain_isolation.sh
scripts/test_brains.sh
scripts/test_persistence.sh
scripts/test_server.sh
scripts/test_tool_passthrough.sh
scripts/test_tui_debug.sh
EOF
)"
[[ "$script_inventory" == "$expected_script_inventory" ]]

integration_inventory="$(
  cd "$repo_root"
  rg -l -i 'brain|daemon|IpcClient' tests --glob '*.rs' | sort
)"
expected_integration_inventory="$(cat <<'EOF'
tests/daemon_integration_test.rs
tests/daemon_upgrade_preflight_test.rs
tests/live.rs
tests/live/impcpd.rs
tests/live/parity.rs
tests/live/providers.rs
tests/no_external_provider_binary_test.rs
tests/service_discovery_test.rs
tests/worker_integration_test.rs
EOF
)"
[[ "$integration_inventory" == "$expected_integration_inventory" ]]

# Exercise the maintained HTTP launchers beyond their re-exec probe. The
# synthetic Finch consumes inherited FD 11, records the exact sealed bind
# argument, and serves the endpoints each launcher requires.
phase=real-wrapped-server-launchers
mock_debug_dir="$scratch/target/debug"
mock_release_dir="$scratch/target/release"
mock_finch="$mock_debug_dir/finch"
mock_bind_log="$scratch/mock-bind.log"
mkdir -p "$mock_debug_dir" "$mock_release_dir"
# Keep the synthetic Finch entrypoint attached to the exact immutable
# supervisor inode. Copying it into a fabricated target tree changes what
# current_exe() is allowed to attest and correctly makes proof verification
# fail when the copied executable has no pinned supervisor sibling.
ln -s "$supervisor" "$mock_finch"
ln -s "$supervisor" "$mock_release_dir/finch"
FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$temp_parent" \
  FINCH_TEST_HTTP_FIXTURE=1 FINCH_BIN="$mock_finch" FINCH_MOCK_BIND_LOG="$mock_bind_log" \
  "$supervisor" "$repo_root/scripts/test_server.sh" >/dev/null
FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$temp_parent" \
  FINCH_TEST_HTTP_FIXTURE=1 FINCH_BIN="$mock_finch" FINCH_MOCK_BIND_LOG="$mock_bind_log" \
  ANTHROPIC_API_KEY=synthetic "$supervisor" "$repo_root/scripts/test_tool_passthrough.sh" >/dev/null
test "$(wc -l <"$mock_bind_log" | tr -d ' ')" -eq 2
awk -F '|' '$1 != $2 || $1 ~ /:0$/ { exit 1 }' "$mock_bind_log"

phase=debug-release-profile-mismatch-rejected
profile_status=0
FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$temp_parent" \
  FINCH_TEST_HTTP_FIXTURE=1 FINCH_BIN="$mock_release_dir/finch" FINCH_MOCK_BIND_LOG="$mock_bind_log" \
  "$supervisor" "$repo_root/scripts/test_server.sh" >/dev/null 2>&1 || profile_status=$?
test "$profile_status" -eq 64

# CI builds the release supervisor before this harness, making this a real
# matched release-profile proof. Developer runs without that artifact retain
# the debug and mismatch coverage above.
release_supervisor="$repo_root/target/release/finch-test-supervisor-pinned"
[[ -x "$release_supervisor" ]] || release_supervisor="$repo_root/target/release/finch-test-supervisor"
if [[ -x "$release_supervisor" ]]; then
  phase=real-wrapped-release-profile
  rm "$mock_release_dir/finch"
  ln -s "$release_supervisor" "$mock_release_dir/finch"
  FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$temp_parent" \
    FINCH_TEST_SUPERVISOR_BIN="$release_supervisor" FINCH_BIN="$mock_release_dir/finch" \
    FINCH_TEST_HTTP_FIXTURE=1 FINCH_MOCK_BIND_LOG="$mock_bind_log" "$release_supervisor" \
    "$repo_root/scripts/test_server.sh" >/dev/null
  test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"
fi

# An unrelated same-name process outside the supervisor's group survives.
sentinel_fifo="$scratch/sentinel.control"
phase=unrelated-finch-sentinel
mkfifo "$sentinel_fifo"
exec 7<>"$sentinel_fifo"
bash -c 'exec -a finch bash -c "read -r _"' <&7 & sentinel_pid=$!
mock_finch="$scratch/mock-finch"
printf '%s\n' '#!/bin/bash' 'while :; do sleep 1; done' >"$mock_finch"
chmod +x "$mock_finch"
FINCH_BIN="$mock_finch" run_isolated "$repo_root/scripts/test_tui_debug.sh" >/dev/null
jobs -pr | awk -v pid="$sentinel_pid" '$1 == pid { found=1 } END { exit !found }'
printf '\n' >&7
wait "$sentinel_pid"
sentinel_pid=''

! rg -n 'pkill|killall|127\.0\.0\.1:[1-9][0-9]*|(^|[;&|[:space:]])kill[[:space:]]+-' \
  "$repo_root/docs/AUTOMATIC_TRAINING.md" "$repo_root/docs/DEVELOPMENT.md" "$repo_root/tests/README.md"

# Scan the full executable/test closure. Only the supervisor's group creation,
# the production ambient-daemon detach (which the isolated gate denies first),
# and the deliberate outside-peer adversary may use an escape API.
escape_uses="$(
  phase=escape-api-allowlist
  cd "$repo_root"
  rg --no-heading --no-line-number \
    --glob '*.rs' --glob '*.sh' --glob '!scripts/test_brain_isolation.sh' \
    '(^|[^[:alnum:]_])(setsid|setpgid|process_group\(|set[[:space:]]+-m)' \
    scripts src tests | sed 's/:[[:space:]]*/:/' | sort
)"
expected_escape_uses="$(cat <<'EOF'
src/bin/finch-test-supervisor.rs:if libc::setpgid(0, 0) == -1 {
src/brain/mod.rs:.process_group(0)
src/brain/mod.rs:if nix::libc::setpgid(0, 0) == -1 {
src/daemon/spawn.rs:.process_group(0)
tests/no_external_provider_binary_test.rs:.process_group(0);
EOF
)"
[[ "$escape_uses" == "$expected_escape_uses" ]]

echo 'Brain test isolation regression checks passed.'

#!/usr/bin/env bash
set -Eeuo pipefail

phase=setup
trap 'status=$?; echo "Brain isolation regression failed in phase: $phase (line $LINENO, status $status)" >&2; exit "$status"' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source "$repo_root/scripts/lib/brain_test_isolation.sh"
supervisor="${FINCH_TEST_SUPERVISOR_BIN:-$repo_root/target/debug/finch-test-supervisor}"
[[ -x "$supervisor" ]] || { echo 'build finch-test-supervisor before running isolation regressions' >&2; exit 69; }

scratch="$(mktemp -d "$(cd "${TMPDIR:-/tmp}" && pwd -P)/finch-brain-isolation-regression.XXXXXX")"
sentinel_pid=''
cleanup_regression() {
  if [[ -n "$sentinel_pid" ]]; then printf '\n' >&7 2>/dev/null || true; wait "$sentinel_pid" 2>/dev/null || true; fi
  exec 7>&- 2>/dev/null || true
  rm -rf -- "$scratch"
}
trap cleanup_regression EXIT

fake_home="$scratch/real-home"
temp_parent="$scratch/temp-homes"
mkdir -p "$fake_home/.finch/brains/existing" "$temp_parent"
printf 'keep me\n' >"$fake_home/.finch/brains/existing/events.jsonl"

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
  test "$FINCH_TEST_BRAIN_ADDR" != 127.0.0.1:11436
  test "$FINCH_TEST_DAEMON_ADDR" != 127.0.0.1:11435
  test "$FINCH_TEST_BRAIN_PASSWORD" != ambient-password
  [[ "$FINCH_TEST_BRAIN_PASSWORD" =~ ^test-[0-9a-f]{32}$ ]]
  ! printf attacker >&9
  ! sh -c ": >/dev/fd/9"
  printf "%s\n" "$HOME" >"$FINCH_CREATED_HOME"
  printf test >"$FINCH_BRAIN_TEST_ROOT/test-created"
'
isolated_home="$(cat "$created")"
test ! -e "$isolated_home"
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"
test "$(cat "$fake_home/.finch/brains/existing/events.jsonl")" = 'keep me'

# Environment strings plus a caller-created descriptor are not wrapper
# authority. This linked, caller-owned proof must fail before any launcher can
# treat the process as isolated.
forged_proof="$scratch/forged-proof"
phase=forged-shell-proof-rejection
printf '%s\n' forged "$fake_home" "$fake_home/.finch/brains" 0:0 0:0 \
  127.0.0.1:1 127.0.0.1:2 forged "$fake_home/.finch/daemon.sock" \
  "$$" /bin/sh 0:0 >"$forged_proof"
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

# If the supervisor cannot prove group quiescence, it fails closed and leaves
# HOME intact for diagnosis instead of reaping the PGID leader and cleaning.
inspection_bin="$scratch/inspection-bin"
phase=inspection-failure-preserves-home
mkdir "$inspection_bin"
printf '%s\n' '#!/bin/sh' 'exit 91' >"$inspection_bin/ps"
chmod +x "$inspection_bin/ps"
if PATH="$inspection_bin:$PATH" run_isolated true 2>"$scratch/inspection.err"; then
  exit 1
else
  test "$?" -eq 70
fi
preserved_home="$(find "$temp_parent" -type d -name 'finch-brain-test-home.*' -print -quit)"
test -n "$preserved_home" && test -d "$preserved_home"
rg -q 'process group was not quiescent' "$scratch/inspection.err"
rm -rf -- "$preserved_home"
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
! ps -p "$normal_descendant_pid" -o pid= 2>/dev/null | grep -q '[0-9]'
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

socket_path="$fake_home/.finch/brains/secret-socket"
if command -v python3 >/dev/null 2>&1 && [[ "${#socket_path}" -lt 100 ]]; then
  phase=manifest-socket-status
  socket_status=0
  FINCH_REAL_STORE="$fake_home/.finch/brains" \
    run_isolated python3 -c \
    'import os,socket; p=os.environ["FINCH_REAL_STORE"]+"/secret-socket"; s=socket.socket(socket.AF_UNIX); s.bind(p); s.close()' \
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

# The supervisor owns a real process group. TERM-resistant members are killed
# within its bounded escalation window before HOME removal or return.
stubborn_pid_file="$scratch/stubborn.pid"
phase=signal-during-teardown
if FINCH_STUBBORN_PID_FILE="$stubborn_pid_file" run_isolated bash -c '
  trap "kill -TERM \"$PPID\"" TERM
  trap "" HUP INT
  echo "$BASHPID" >"$FINCH_STUBBORN_PID_FILE"
  sleep 30 &
  kill -TERM "$PPID"
  wait
'; then
  exit 1
else
  test "$?" -eq 143
fi
stubborn_group="$(cat "$stubborn_pid_file")"
! kill -0 -- "-$stubborn_group" 2>/dev/null
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

launchers=(demo_boot.sh smoke_vm_wire_provider.sh stress_test.sh test_persistence.sh test_server.sh test_tool_passthrough.sh test_tui_debug.sh)
phase=launcher-probe-closure
for launcher in "${launchers[@]}"; do
  launcher_probe="$scratch/probe-$launcher"
  FINCH_TEST_LAUNCHER_PROBE_FILE="$launcher_probe" FINCH_TEST_LAUNCHER_PROBE_ONLY=1 \
    run_isolated "$repo_root/scripts/$launcher"
  [[ "$(cat "$launcher_probe")" == "$temp_parent"/finch-brain-test-home.* ]]
done

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
src/daemon/spawn.rs:.process_group(0)
EOF
)"
[[ "$escape_uses" == "$expected_escape_uses" ]]

echo 'Brain test isolation regression checks passed.'

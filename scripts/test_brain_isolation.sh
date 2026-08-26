#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source "$repo_root/scripts/lib/brain_test_isolation.sh"

scratch="$(mktemp -d "$(cd "${TMPDIR:-/tmp}" && pwd -P)/finch-brain-isolation-regression.XXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT
fake_home="$scratch/real-home"
temp_parent="$scratch/temp-homes"
mkdir -p "$fake_home/.finch/brains/existing" "$temp_parent"
printf 'keep me\n' >"$fake_home/.finch/brains/existing/events.jsonl"

run_isolated() {
  FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$temp_parent" brain_test_isolation_run "$@"
}

expect_64() {
  if "$@" >/dev/null 2>&1; then echo 'expected isolation validation failure' >&2; exit 1; else test "$?" -eq 64; fi
}

run_isolated bash -c 'test "$HOME" = "$FINCH_BRAIN_TEST_HOME"; test "$FINCH_BRAIN_TEST_ROOT" = "$HOME/.finch/brains"; printf test >"$FINCH_BRAIN_TEST_ROOT/test-created"'
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

if run_isolated bash -c 'exit 23'; then exit 1; else test "$?" -eq 23; fi
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

expect_64 env -u HOME -u FINCH_TEST_REAL_HOME bash -c 'source "$1"; brain_test_isolation_run true' _ "$repo_root/scripts/lib/brain_test_isolation.sh"
expect_64 env FINCH_TEST_REAL_HOME= FINCH_TEST_TMP_PARENT="$temp_parent" bash -c 'source "$1"; brain_test_isolation_run true' _ "$repo_root/scripts/lib/brain_test_isolation.sh"
expect_64 env FINCH_TEST_REAL_HOME="$fake_home/" FINCH_TEST_TMP_PARENT="$temp_parent" bash -c 'source "$1"; brain_test_isolation_run true' _ "$repo_root/scripts/lib/brain_test_isolation.sh"
ln -s "$fake_home" "$scratch/home-link"
expect_64 env FINCH_TEST_REAL_HOME="$scratch/home-link" FINCH_TEST_TMP_PARENT="$temp_parent" bash -c 'source "$1"; brain_test_isolation_run true' _ "$repo_root/scripts/lib/brain_test_isolation.sh"
expect_64 env FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT=relative bash -c 'source "$1"; brain_test_isolation_run true' _ "$repo_root/scripts/lib/brain_test_isolation.sh"
ln -s "$temp_parent" "$scratch/temp-link"
expect_64 env FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$scratch/temp-link" bash -c 'source "$1"; brain_test_isolation_run true' _ "$repo_root/scripts/lib/brain_test_isolation.sh"
expect_64 env FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$scratch" bash -c 'source "$1"; brain_test_isolation_run true' _ "$repo_root/scripts/lib/brain_test_isolation.sh"
expect_64 env FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$fake_home" bash -c 'source "$1"; brain_test_isolation_run true' _ "$repo_root/scripts/lib/brain_test_isolation.sh"
expect_64 env FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$fake_home/.finch/brains" bash -c 'source "$1"; brain_test_isolation_run true' _ "$repo_root/scripts/lib/brain_test_isolation.sh"

hostile_home="$scratch/hostile-home"
hostile_target="$scratch/effective-production-finch"
mkdir -p "$hostile_home" "$hostile_target/brains" "$scratch/hostile-temp"
printf 'production sentinel\n' >"$hostile_target/brains/sentinel"
ln -s "$hostile_target" "$hostile_home/.finch"
expect_64 env FINCH_TEST_REAL_HOME="$hostile_home" FINCH_TEST_TMP_PARENT="$hostile_target/brains" bash -c 'source "$1"; brain_test_isolation_run true' _ "$repo_root/scripts/lib/brain_test_isolation.sh"
test "$(cat "$hostile_target/brains/sentinel")" = 'production sentinel'
test -z "$(find "$scratch/hostile-temp" -mindepth 1 -print -quit)"

FINCH_BRAIN_TEST_ISOLATED=1 FINCH_BRAIN_TEST_HOME="$fake_home" FINCH_BRAIN_TEST_ROOT="$fake_home/.finch/brains" \
  FINCH_BRAIN_TEST_TOKEN=forged FINCH_BRAIN_TEST_PROOF_FD=9 FINCH_TEST_TMP_PARENT="$scratch" HOME="$fake_home" \
  bash -c 'source "$1"; ! brain_test_isolation_is_active' _ "$repo_root/scripts/lib/brain_test_isolation.sh"
launcher_probe="$scratch/launcher-home"
if FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$temp_parent" FINCH_TEST_LAUNCHER_PROBE_FILE="$launcher_probe" \
  FINCH_BRAIN_TEST_ISOLATED=1 FINCH_BRAIN_TEST_HOME="$fake_home" FINCH_BRAIN_TEST_ROOT="$fake_home/.finch/brains" \
  FINCH_BRAIN_TEST_TOKEN=forged FINCH_BRAIN_TEST_PROOF_FD=9 HOME="$fake_home" FINCH_BIN="$scratch/missing-finch" \
  "$repo_root/scripts/smoke_vm_wire_provider.sh" >/dev/null 2>&1; then
  exit 1
fi
probed_home="$(cat "$launcher_probe")"
[[ "$probed_home" == "$temp_parent"/finch-brain-test-home.* && "$probed_home" != "$fake_home" ]]
test ! -e "$probed_home"

diagnostic="$scratch/diagnostic"
if run_isolated bash -c 'mkdir "$FINCH_TEST_REAL_HOME/.finch/brains/secret-directory"' 2>"$diagnostic"; then exit 1; else test "$?" -eq 70; fi
! rg -q 'secret-directory|existing|events.jsonl' "$diagnostic"
rmdir "$fake_home/.finch/brains/secret-directory"

if run_isolated bash -c 'ln -s /private/secret-target "$FINCH_TEST_REAL_HOME/.finch/brains/secret-link"' 2>"$diagnostic"; then exit 1; else test "$?" -eq 70; fi
! rg -q 'secret-link|secret-target|existing|events.jsonl' "$diagnostic"
rm "$fake_home/.finch/brains/secret-link"

if stat -f '%Lp' "$fake_home/.finch/brains/existing/events.jsonl" >/dev/null 2>&1; then
  original_mode="$(stat -f '%Lp' "$fake_home/.finch/brains/existing/events.jsonl")"
else
  original_mode="$(stat -c '%a' "$fake_home/.finch/brains/existing/events.jsonl")"
fi
if run_isolated chmod 600 "$fake_home/.finch/brains/existing/events.jsonl" 2>"$diagnostic"; then exit 1; else test "$?" -eq 70; fi
chmod "$original_mode" "$fake_home/.finch/brains/existing/events.jsonl"

if run_isolated bash -c 'mkfifo "$FINCH_TEST_REAL_HOME/.finch/brains/secret-fifo"' 2>"$diagnostic"; then exit 1; else test "$?" -eq 70; fi
! rg -q 'secret-fifo|existing|events.jsonl' "$diagnostic"
rm "$fake_home/.finch/brains/secret-fifo"

socket_path="$fake_home/.finch/brains/secret-socket"
if command -v python3 >/dev/null 2>&1 && [[ "${#socket_path}" -lt 100 ]]; then
  if run_isolated python3 -c 'import os,socket; p=os.environ["FINCH_TEST_REAL_HOME"]+"/.finch/brains/secret-socket"; s=socket.socket(socket.AF_UNIX); s.bind(p); s.close()' 2>"$diagnostic"; then exit 1; else test "$?" -eq 70; fi
  ! rg -q 'secret-socket|existing|events.jsonl' "$diagnostic"
  rm "$fake_home/.finch/brains/secret-socket"
fi

fake_bin="$scratch/bin"; mkdir "$fake_bin"
printf '%s\n' '#!/bin/bash' 'if [[ -e "$FINCH_MANIFEST_FAIL_MARKER" ]]; then exit 9; fi' 'exec /usr/bin/shasum "$@"' >"$fake_bin/shasum"
chmod +x "$fake_bin/shasum"
if PATH="$fake_bin:$PATH" FINCH_MANIFEST_FAIL_MARKER="$scratch/fail-manifest" run_isolated bash -c 'touch "$FINCH_MANIFEST_FAIL_MARKER"; exit 31' 2>"$scratch/manifest-failure.err"; then exit 1; else test "$?" -eq 74; fi
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"
rm "$scratch/fail-manifest"

child_pid_file="$scratch/child.pid"
if FINCH_SIGNAL_PID_FILE="$child_pid_file" run_isolated bash -c '
  sleep 30 & echo $! >"$FINCH_SIGNAL_PID_FILE"
  (sleep 0.2; kill -TERM "$PPID") &
  wait
' 2>"$scratch/signal.err"; then
  exit 1
else
  test "$?" -eq 143
fi
child_pid="$(cat "$child_pid_file")"
! kill -0 "$child_pid" 2>/dev/null
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

hostile_pid_file="$fake_home/.finch/brains/pid-file-sentinel"
printf 'unchanged\n' >"$hostile_pid_file"
FINCH_TEST_WRAPPER_PID_FILE="$hostile_pid_file" run_isolated true
test "$(cat "$hostile_pid_file")" = unchanged
rm "$hostile_pid_file"

dangling_root="$scratch/dangling-store"
ln -s "$scratch/no-such-store" "$dangling_root"
dangling_manifest="$(brain_store_manifest "$dangling_root")"
[[ "$dangling_manifest" == root-link* && "$dangling_manifest" != '<missing>' ]]

allocation_bin="$scratch/allocation-bin"; mkdir "$allocation_bin"
real_mktemp="$(command -v mktemp)"
printf '%s\n' '#!/bin/bash' \
  'count=0; [[ ! -f "$FINCH_ALLOCATION_COUNT" ]] || count=$(cat "$FINCH_ALLOCATION_COUNT")' \
  'count=$((count + 1)); printf "%s\n" "$count" >"$FINCH_ALLOCATION_COUNT"' \
  'if [[ "$count" -eq 2 ]]; then kill -TERM "$PPID"; sleep 0.2; fi' \
  'exec "$FINCH_REAL_MKTEMP" "$@"' >"$allocation_bin/mktemp"
chmod +x "$allocation_bin/mktemp"
if PATH="$allocation_bin:$PATH" FINCH_ALLOCATION_COUNT="$scratch/allocation-count" FINCH_REAL_MKTEMP="$real_mktemp" run_isolated true 2>"$scratch/allocation-signal.err"; then
  exit 1
else
  test "$?" -eq 143
fi
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

launchers=(demo_boot.sh smoke_vm_wire_provider.sh stress_test.sh test_persistence.sh test_server.sh test_tui_debug.sh)
for launcher in "${launchers[@]}"; do
  test -x "$repo_root/scripts/$launcher"
  rg -q 'brain_test_isolation_reexec_launcher' "$repo_root/scripts/$launcher"
done

echo 'Brain test isolation regression checks passed.'

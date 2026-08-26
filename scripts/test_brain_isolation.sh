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

diagnostic="$scratch/diagnostic"
if run_isolated bash -c 'mkdir "$FINCH_TEST_REAL_HOME/.finch/brains/secret-directory"' 2>"$diagnostic"; then exit 1; else test "$?" -eq 70; fi
! rg -q 'secret-directory|existing|events.jsonl' "$diagnostic"
rmdir "$fake_home/.finch/brains/secret-directory"

if run_isolated bash -c 'ln -s /private/secret-target "$FINCH_TEST_REAL_HOME/.finch/brains/secret-link"' 2>"$diagnostic"; then exit 1; else test "$?" -eq 70; fi
! rg -q 'secret-link|secret-target|existing|events.jsonl' "$diagnostic"
rm "$fake_home/.finch/brains/secret-link"

fake_bin="$scratch/bin"; mkdir "$fake_bin"
printf '%s\n' '#!/bin/bash' 'if [[ -e "$FINCH_MANIFEST_FAIL_MARKER" ]]; then exit 9; fi' 'exec /usr/bin/shasum "$@"' >"$fake_bin/shasum"
chmod +x "$fake_bin/shasum"
if PATH="$fake_bin:$PATH" FINCH_MANIFEST_FAIL_MARKER="$scratch/fail-manifest" run_isolated bash -c 'touch "$FINCH_MANIFEST_FAIL_MARKER"; exit 31' 2>"$scratch/manifest-failure.err"; then exit 1; else test "$?" -eq 74; fi
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"
rm "$scratch/fail-manifest"

child_pid_file="$scratch/child.pid"
wrapper_pid_file="$scratch/wrapper.pid"
if FINCH_TEST_WRAPPER_PID_FILE="$wrapper_pid_file" FINCH_SIGNAL_PID_FILE="$child_pid_file" run_isolated bash -c '
  sleep 30 & echo $! >"$FINCH_SIGNAL_PID_FILE"
  (sleep 0.2; kill -TERM "$(cat "$FINCH_TEST_WRAPPER_PID_FILE")") &
  wait
' 2>"$scratch/signal.err"; then
  exit 1
else
  test "$?" -eq 143
fi
child_pid="$(cat "$child_pid_file")"
! kill -0 "$child_pid" 2>/dev/null
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

launchers=(demo_boot.sh smoke_vm_wire_provider.sh stress_test.sh test_persistence.sh test_server.sh test_tui_debug.sh)
for launcher in "${launchers[@]}"; do
  test -x "$repo_root/scripts/$launcher"
  rg -q 'brain_test_isolation_reexec_launcher' "$repo_root/scripts/$launcher"
done

echo 'Brain test isolation regression checks passed.'

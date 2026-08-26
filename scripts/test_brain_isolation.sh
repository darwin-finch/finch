#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/brain_test_isolation.sh
source "$repo_root/scripts/lib/brain_test_isolation.sh"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/finch-brain-isolation-regression.XXXXXX")"
trap 'rm -rf -- "$scratch"' EXIT
fake_home="$scratch/real-home"
temp_parent="$scratch/temp-homes"
mkdir -p "$fake_home/.finch/brains/existing" "$temp_parent"
printf 'keep me\n' >"$fake_home/.finch/brains/existing/events.jsonl"

run_isolated() {
  FINCH_TEST_REAL_HOME="$fake_home" FINCH_TEST_TMP_PARENT="$temp_parent" \
    brain_test_isolation_run "$@"
}

run_isolated bash -c '
  test "$HOME" = "$FINCH_BRAIN_TEST_HOME"
  test "$FINCH_BRAIN_TEST_ROOT" = "$HOME/.finch/brains"
  printf test >"$FINCH_BRAIN_TEST_ROOT/test-created"
'
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

if run_isolated bash -c 'exit 23'; then
  echo 'expected wrapped command failure' >&2
  exit 1
else
  status="$?"
  test "$status" -eq 23
fi
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

if FINCH_TEST_REAL_HOME=relative/path brain_test_isolation_run true 2>/dev/null; then
  echo 'expected relative production-home fallback to fail closed' >&2
  exit 1
fi

if run_isolated bash -c 'printf changed >>"$FINCH_TEST_REAL_HOME/.finch/brains/existing/events.jsonl"' 2>/dev/null; then
  echo 'expected real-store manifest guard to fail' >&2
  exit 1
else
  status="$?"
  test "$status" -eq 70
fi
test -z "$(find "$temp_parent" -mindepth 1 -print -quit)"

echo 'Brain test isolation regression checks passed.'

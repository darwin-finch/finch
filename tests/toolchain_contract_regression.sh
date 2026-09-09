#!/usr/bin/env bash
set -euo pipefail

repository_root=$(git rev-parse --show-toplevel)
cd "$repository_root"

contract="tests/toolchain_contract.sh"

fail() {
  echo "toolchain contract regression failed: $*" >&2
  exit 1
}

capture_failure() {
  local output_name="$1"
  local status_name="$2"
  shift 2

  set +e
  local captured_output
  captured_output=$("$@" 2>&1)
  local captured_status=$?
  set -e

  printf -v "$output_name" '%s' "$captured_output"
  printf -v "$status_name" '%s' "$captured_status"
}

invalid_output=""
invalid_status=""
capture_failure invalid_output invalid_status "$contract" --definitely-invalid
if [[ "$invalid_status" -ne 2 || "$invalid_output" != "usage: $contract [--metadata-only]" ]]; then
  fail "invalid arguments must return status 2 with immediate usage diagnostics; status=$invalid_status output=[$invalid_output]"
fi

if git check-ignore --no-index -q Cargo.lock; then
  ignore_source=$(git check-ignore --no-index -v Cargo.lock 2>&1 || true)
  fail "root Cargo.lock must be admitted by .gitignore; match=[$ignore_source]"
fi

for nested_lock in \
  .github/issue-105-windows-probe/Cargo.lock \
  .github/issue-201-windows-probe/Cargo.lock
do
  if ! git check-ignore --no-index -q "$nested_lock"; then
    ignore_source=$(git check-ignore --no-index -v "$nested_lock" 2>&1 || true)
    fail "nested standalone-workspace lockfile must remain ignored: path=$nested_lock match=[$ignore_source]"
  fi
done

probe_root=$(mktemp -d "${TMPDIR:-/tmp}/finch-toolchain-contract.XXXXXX")
probe_worktree="$probe_root/worktree"
cleanup_probe() {
  if [[ -d "$probe_worktree" ]]; then
    git worktree remove --force "$probe_worktree" >/dev/null 2>&1 || true
  fi
  if [[ -d "$probe_root" ]]; then
    rmdir "$probe_root" >/dev/null 2>&1 || true
  fi
}
trap cleanup_probe EXIT

git worktree add --detach "$probe_worktree" HEAD >/dev/null
rm "$probe_worktree/Cargo.lock"

run_probe_contract() {
  (cd "$probe_worktree" && "$contract" "$@")
}

missing_output=""
missing_status=""
capture_failure missing_output missing_status \
  run_probe_contract --metadata-only
if [[ "$missing_status" -ne 1 \
  || "$missing_output" != *"Cargo.lock is tracked but missing from the worktree"* ]]; then
  fail "deleted-but-indexed Cargo.lock must return status 1 with the missing-worktree invariant; status=$missing_status output=[$missing_output]"
fi

git worktree remove --force "$probe_worktree" >/dev/null
git worktree add --detach "$probe_worktree" HEAD >/dev/null
probe_index="$probe_root/untracked.index"
GIT_INDEX_FILE="$probe_index" git -C "$probe_worktree" read-tree HEAD
GIT_INDEX_FILE="$probe_index" git -C "$probe_worktree" rm --cached --quiet Cargo.lock

run_untracked_probe_contract() {
  (cd "$probe_worktree" && GIT_INDEX_FILE="$probe_index" "$contract" "$@")
}

untracked_output=""
untracked_status=""
capture_failure untracked_output untracked_status \
  run_untracked_probe_contract --metadata-only
if [[ "$untracked_status" -ne 1 \
  || "$untracked_output" != *"Cargo.lock must be tracked so clean checkouts use the reviewed dependency graph"* ]]; then
  fail "untracked Cargo.lock must return status 1 with the reviewed-graph invariant; status=$untracked_status output=[$untracked_output]"
fi

git worktree remove --force "$probe_worktree" >/dev/null
rm "$probe_index"
rmdir "$probe_root"
trap - EXIT

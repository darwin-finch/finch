#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/brain_test_isolation.sh
source "$repo_root/scripts/lib/brain_test_isolation.sh"

if [[ "$#" -eq 0 ]]; then
  set -- cargo test --lib
fi

cd "$repo_root"

# Pin the supervisor image before running anything under it.
#
# `finch-test-supervisor` is a workspace binary target, so the supervised
# `cargo test` can relink it. Cargo replaces a binary by writing a new file and
# renaming it into place, which allocates a new inode — and the wrapper proof
# records the supervisor's inode, so the check fires against the supervisor's
# own rebuild and reports `supervisor executable identity changed`. That is the
# same message a genuine substitution produces (#259).
#
# `finch-test-supervisor-pinned` is not a Cargo target name, so nothing ever
# writes to it. Both `expected_supervisor_executable` in src/brain/mod.rs and
# the fallback below already preferred it; nothing created it.
built_supervisor="$repo_root/target/debug/finch-test-supervisor"
pinned_supervisor="$repo_root/target/debug/finch-test-supervisor-pinned"
if [[ -z "${FINCH_TEST_SUPERVISOR_BIN:-}" ]]; then
  cargo build --quiet --bin finch-test-supervisor
  # Refresh only when the built image actually differs, so repeated runs keep
  # the same inode and the fast path in `verify_supervisor_image` stays exact.
  if ! cmp -s "$built_supervisor" "$pinned_supervisor" 2>/dev/null; then
    # Write beside the target and rename, so a concurrent run never observes a
    # half-copied supervisor.
    staging="$pinned_supervisor.$$"
    cp "$built_supervisor" "$staging"
    chmod +x "$staging"
    mv -f "$staging" "$pinned_supervisor"
  fi
fi

default_supervisor="$pinned_supervisor"
[[ -x "$default_supervisor" ]] || default_supervisor="$built_supervisor"
supervisor="${FINCH_TEST_SUPERVISOR_BIN:-$default_supervisor}"
if [[ -z "${FINCH_TEST_TMP_PARENT:-}" ]]; then
  FINCH_TEST_TMP_PARENT="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
  export FINCH_TEST_TMP_PARENT
fi
if [[ -x "$supervisor" ]]; then
  exec "$supervisor" "$@"
fi
exec cargo run --quiet --bin finch-test-supervisor -- "$@"

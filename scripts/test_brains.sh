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
# `cargo test` relinks it. Cargo replaces a binary by writing a new file and
# renaming it into place, which allocates a new inode — and the wrapper proof
# records the supervisor's inode, so the check fired against the supervisor's
# own rebuild and reported `supervisor executable identity changed` (#259).
#
# CI has pinned since the isolation workflow was written
# (.github/workflows/issue-56-brain-isolation.yml installs both the debug and
# release copies at 0555). Local runs never did, which is why #259 reproduced
# only locally. This closes that divergence, using `install -m 0555` so the
# image the authority check trusts is not left owner-writable, and so a local
# copy is byte-for-byte the same artifact CI produces.
#
# `finch-test-supervisor-pinned` is not a Cargo target name, so nothing writes
# to it behind the running supervisor.
built_supervisor="$repo_root/target/debug/finch-test-supervisor"
pinned_supervisor="$repo_root/target/debug/finch-test-supervisor-pinned"
if [[ -z "${FINCH_TEST_SUPERVISOR_BIN:-}" ]]; then
  # Build only when there is nothing usable to pin. Building unconditionally
  # made every supervised launcher — including ones that run no cargo at all —
  # fail whenever the workspace did not compile, which is exactly when the
  # isolation harness is most wanted.
  if [[ ! -x "$built_supervisor" && ! -x "$pinned_supervisor" ]]; then
    cargo build --quiet --bin finch-test-supervisor
  fi
  if [[ -x "$built_supervisor" ]] && ! cmp -s "$built_supervisor" "$pinned_supervisor" 2>/dev/null; then
    # Stage beside the target and rename, so a concurrent run never observes a
    # half-copied supervisor. Refresh only when the bytes differ, so repeated
    # runs keep the same inode.
    staging="$pinned_supervisor.$$"
    trap 'rm -f "$staging"' EXIT
    install -m 0555 "$built_supervisor" "$staging"
    mv -f "$staging" "$pinned_supervisor"
    trap - EXIT
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

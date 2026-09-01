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
if [[ -z "${FINCH_TEST_SUPERVISOR_BIN:-}" ]]; then
  # Cargo's fingerprint is the maintained source/build freshness contract.
  # Merely finding an executable here is not enough: after switching
  # revisions, both the plain and pinned paths can still contain the previous
  # checkout's supervisor. In particular, that stale image can predate proof
  # fields or other authority fixes. Never fall back to it when the current
  # supervisor does not build; fail before granting test authority instead.
  # The private target override exists only so the maintained regression can
  # seed a hostile cache without mutating the shared workspace target. Normal
  # callers, including callers that set Cargo's own target variable for the
  # command being supervised, retain the repository's attested target root.
  cargo_target_dir="${FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR:-$repo_root/target}"
  cargo build --quiet --target-dir "$cargo_target_dir" --bin finch-test-supervisor
  cargo_target_dir="$(cd "$cargo_target_dir" && pwd -P)"
  built_supervisor="$cargo_target_dir/debug/finch-test-supervisor"
  built_digest="$(shasum -a 256 "$built_supervisor" | awk '{print $1}')"
  if [[ ! "$built_digest" =~ ^[0-9a-f]{64}$ ]]; then
    echo "could not compute the current supervisor image digest for $built_supervisor" >&2
    exit 74
  fi
  pinned_supervisor="$cargo_target_dir/debug/finch-test-supervisor-pinned-sha256-$built_digest"

  # Publish at a content-addressed path that is never replaced. A fixed pinned
  # pathname still has a compare/rename race: two launchers can both compare
  # old bytes, then the second rename unlinks the executable after the first
  # has exec'd it. The first process then appears as a deleted or different
  # image when its child verifies the proof. Hard-link creation is one atomic
  # create-if-absent operation; concurrent launchers either create this exact
  # digest path or verify the already-published identical immutable image.
  staging="$pinned_supervisor.$$"
  trap 'rm -f "$staging"' EXIT
  install -m 0555 "$built_supervisor" "$staging"

  # Deterministic production-boundary concurrency probe. It is inert unless
  # the maintained isolation regression supplies both private paths.
  if [[ -n "${FINCH_TEST_SUPERVISOR_PIN_READY_DIR:-}" || \
    -n "${FINCH_TEST_SUPERVISOR_PIN_CONTINUE_FILE:-}" ]]; then
    if [[ ! -d "${FINCH_TEST_SUPERVISOR_PIN_READY_DIR:-}" || \
      -z "${FINCH_TEST_SUPERVISOR_PIN_CONTINUE_FILE:-}" ]]; then
      echo 'supervisor pin publication probe requires a ready directory and continuation file' >&2
      exit 64
    fi
    printf '%s\n' "$pinned_supervisor" >"$FINCH_TEST_SUPERVISOR_PIN_READY_DIR/$$"
    for _ in {1..500}; do
      [[ -e "$FINCH_TEST_SUPERVISOR_PIN_CONTINUE_FILE" ]] && break
      sleep 0.01
    done
    if [[ ! -e "$FINCH_TEST_SUPERVISOR_PIN_CONTINUE_FILE" ]]; then
      echo "supervisor pin publication probe timed out for $pinned_supervisor" >&2
      exit 70
    fi
  fi

  if ln "$staging" "$pinned_supervisor" 2>/dev/null; then
    rm -f "$staging"
  elif [[ -f "$pinned_supervisor" && ! -L "$pinned_supervisor" && \
    -x "$pinned_supervisor" ]] && cmp -s "$staging" "$pinned_supervisor"; then
    rm -f "$staging"
  else
    echo "content-addressed supervisor path is not the expected immutable image: $pinned_supervisor" >&2
    exit 74
  fi
  trap - EXIT
  supervisor="$pinned_supervisor"
  unset FINCH_TEST_SUPERVISOR_BUILD_TARGET_DIR
else
  # Explicit callers retain the established plain or fixed-pinned launcher
  # contract. Their artifact lifecycle is deliberately caller-owned.
  supervisor="$FINCH_TEST_SUPERVISOR_BIN"
fi

if [[ -z "${FINCH_TEST_TMP_PARENT:-}" ]]; then
  FINCH_TEST_TMP_PARENT="$(cd "${TMPDIR:-/tmp}" && pwd -P)"
  export FINCH_TEST_TMP_PARENT
fi
if [[ -x "$supervisor" ]]; then
  exec "$supervisor" "$@"
fi
echo "test supervisor is not executable after freshness validation: $supervisor" >&2
exit 69

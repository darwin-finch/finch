#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/brain_test_isolation.sh
source "$repo_root/scripts/lib/brain_test_isolation.sh"

if [[ "$#" -eq 0 ]]; then
  set -- cargo test --lib
fi

cd "$repo_root"
supervisor="${FINCH_TEST_SUPERVISOR_BIN:-$repo_root/target/debug/finch-test-supervisor}"
if [[ -x "$supervisor" ]]; then
  exec "$supervisor" "$@"
fi
exec cargo run --quiet --bin finch-test-supervisor -- "$@"

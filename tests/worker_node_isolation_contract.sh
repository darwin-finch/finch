#!/usr/bin/env bash
set -euo pipefail

repository_root=$(git rev-parse --show-toplevel)
cd "$repository_root"

ambient_calls=$(rg -n \
  'get\(handle_node_(info|stats)\)|NodeInfo::load\(|NodeIdentity::load_or_create\(' \
  tests/worker_integration_test.rs tests/load_test.rs || true)
if [[ -n "$ambient_calls" ]]; then
  echo 'worker tests must not use ambient-HOME node identity or stats loaders:' >&2
  echo "$ambient_calls" >&2
  exit 1
fi

for test_file in tests/worker_integration_test.rs tests/load_test.rs; do
  grep -Fq 'IsolatedNodeTestState' "$test_file" || {
    echo "$test_file must construct opaque disposable node state" >&2
    exit 1
  }
  grep -Fq 'handle_node_info_from_state_directory' "$test_file" || {
    echo "$test_file must route node-info through explicit isolated state" >&2
    exit 1
  }
done

grep -Fq 'node_identity_before = node_identity_digest(&real_home)' \
  src/bin/finch-test-supervisor.rs || {
  echo 'supervisor must snapshot the caller real node_id before launch' >&2
  exit 1
}
grep -Fq 'node_identity_after = node_identity_digest(&real_home)' \
  src/bin/finch-test-supervisor.rs || {
  echo 'supervisor must snapshot the caller real node_id after cleanup' >&2
  exit 1
}

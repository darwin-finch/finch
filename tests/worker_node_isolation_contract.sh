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
  grep -Fq '#![cfg(unix)]' "$test_file" || {
    echo "$test_file must not compile the Unix descriptor seam on other targets" >&2
    exit 1
  }
  grep -Fq 'IsolatedNodeTestState' "$test_file" || {
    echo "$test_file must construct opaque disposable node state" >&2
    exit 1
  }
  grep -Fq 'handle_node_info_from_state_directory' "$test_file" || {
    echo "$test_file must route node-info through explicit isolated state" >&2
    exit 1
  }
done

grep -Fq 'real_node_identity = RealNodeIdentityGuard::pin(&real_home)' \
  src/bin/finch-test-supervisor.rs || {
  echo 'supervisor must pin the caller real HOME and Finch state before launch' >&2
  exit 1
}
grep -Fq 'real_node_identity.verify_pathnames(&real_home)' \
  src/bin/finch-test-supervisor.rs || {
  echo 'supervisor must revalidate caller HOME and Finch-state identities after cleanup' >&2
  exit 1
}
grep -Fq 'FINCH_TEST_FORCE_MANIFEST_AFTER_ERROR' \
  src/bin/finch-test-supervisor.rs || {
  echo 'supervisor must retain the deterministic dual-snapshot error regression hook' >&2
  exit 1
}

if rg -q 'node-id\.lock' src/node; then
  echo 'isolated node state must use its Arc-owned mutex, not a pathname lock' >&2
  exit 1
fi

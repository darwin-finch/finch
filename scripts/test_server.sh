#!/bin/bash
# Test script for HTTP server

script_path="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
source "$(dirname "$script_path")/lib/brain_test_isolation.sh"
finch_bin="${FINCH_BIN:-target/release/finch}"
if [[ "${FINCH_BRAIN_TEST_ISOLATED:-}" != 1 ]]; then
    case "$finch_bin" in
        target/debug/finch|*/target/debug/finch)
            export FINCH_TEST_SUPERVISOR_BIN="$(dirname "$script_path")/../target/debug/finch-test-supervisor" ;;
        target/release/finch|*/target/release/finch)
            export FINCH_TEST_SUPERVISOR_BIN="$(dirname "$script_path")/../target/release/finch-test-supervisor" ;;
    esac
fi
brain_test_isolation_reexec_launcher "$script_path" "$@"

set -euo pipefail

echo "Testing Shammah HTTP daemon mode..."
echo

# Check if binary exists
brain_test_isolation_require_finch_profile "$finch_bin" || {
    echo 'Finch binary and test supervisor must use the same debug/release profile' >&2
    exit 64
}
if [ ! -x "$finch_bin" ]; then
    echo "Error: Binary not found. Run 'cargo build --release' first."
    exit 1
fi

mkdir -p "$HOME/.finch"
if [[ ! -f "$HOME/.finch/config.toml" ]]; then
    printf '%s\n' '[[providers]]' 'type = "claude"' 'api_key = "isolated-health-test"' >"$HOME/.finch/config.toml"
fi
address_file="$HOME/.finch/test-server.addr"
daemon_pid=''
cleanup() {
    trap - EXIT INT TERM HUP
    rm -f -- "$address_file"
}
trap cleanup EXIT INT TERM HUP

# Port zero delegates endpoint allocation to the kernel. The daemon publishes
# the actual loopback address only inside this isolated HOME.
echo "Starting daemon on an ephemeral loopback port..."
FINCH_TEST_BOUND_ADDR_FILE="$address_file" "$finch_bin" daemon --bind "$FINCH_TEST_DAEMON_ADDR" &
DAEMON_PID=$!
daemon_pid="$DAEMON_PID"

echo "Daemon started with PID: $DAEMON_PID"
await_bound_address "$address_file" "$DAEMON_PID" || exit 1
daemon_url="http://$(cat "$address_file")"

# Test health endpoint
echo
echo "Testing /health endpoint..."
curl --fail --show-error --silent "$daemon_url/health" | jq '.'

# Test metrics endpoint
echo
echo "Testing /metrics endpoint..."
metrics="$(curl --fail --show-error --silent "$daemon_url/metrics")"
printf '%s\n' "$metrics" | head -5

echo
echo "The test supervisor will stop and reap the owned daemon process group."

echo
echo "Test complete!"

#!/bin/bash
# Test script to check TUI stdout debug output under the process supervisor.

script_path="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
source "$(dirname "$script_path")/lib/brain_test_isolation.sh"
brain_test_isolation_reexec_launcher "$script_path" "$@"

set -euo pipefail

finch_bin="${FINCH_BIN:-./target/debug/finch}"
output="$(mktemp "$HOME/finch-tui-debug.XXXXXX")"
cleanup() { rm -f -- "$output"; }
trap cleanup EXIT INT TERM HUP

"$finch_bin" >"$output" 2>&1 & finch_pid=$!
sleep 1
jobs -pr | awk -v pid="$finch_pid" '$1 == pid { found=1 } END { exit !found }' || {
  head -50 "$output"
  echo 'Finch exited before the supervised TUI smoke interval' >&2
  exit 1
}
head -50 "$output"
echo 'The test supervisor will stop and reap the owned TUI process group.'

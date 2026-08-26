#!/bin/bash
#  Test script to check TUI stdout debug output

script_path="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
source "$(dirname "$script_path")/lib/brain_test_isolation.sh"
brain_test_isolation_reexec_launcher "$script_path" "$@"

set -euo pipefail

finch_bin="${FINCH_BIN:-./target/debug/finch}"
output="$(mktemp "$HOME/finch-tui-debug.XXXXXX")"
finch_pid=''
timer_pid=''
timer_marker="$HOME/finch-tui-debug.timer"
cleanup() {
    trap - EXIT INT TERM HUP
    if [[ -n "$timer_pid" ]]; then kill "$timer_pid" 2>/dev/null || true; wait "$timer_pid" 2>/dev/null || true; fi
    if [[ -n "$finch_pid" ]]; then
        kill -TERM "$finch_pid" 2>/dev/null || true
        for _ in {1..20}; do kill -0 "$finch_pid" 2>/dev/null || break; sleep 0.05; done
        kill -TERM "$finch_pid" 2>/dev/null || true
        wait "$finch_pid" 2>/dev/null || true
    fi
    rm -f -- "$output" "$timer_marker"
}
trap cleanup EXIT INT TERM HUP

"$finch_bin" >"$output" 2>&1 & finch_pid=$!
brain_test_isolation_register_owned_pid "$finch_pid"
(
    sleep 1
    : >"$timer_marker"
    kill -TERM "$finch_pid" 2>/dev/null || true
) & timer_pid=$!
finch_status=0
wait "$finch_pid" 2>/dev/null || finch_status=$?
finch_pid=''
[[ -e "$timer_marker" ]] || {
    head -50 "$output"
    echo "Finch exited before the TUI smoke timeout (status $finch_status)" >&2
    exit 1
}
kill "$timer_pid" 2>/dev/null || true
wait "$timer_pid" 2>/dev/null || true
timer_pid=''
head -50 "$output"

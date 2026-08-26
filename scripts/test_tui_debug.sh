#!/bin/bash
#  Test script to check TUI stdout debug output

script_path="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
source "$(dirname "$script_path")/lib/brain_test_isolation.sh"
brain_test_isolation_reexec_launcher "$script_path" "$@"

# Make sure we're in interactive mode (not piped)
# Start finch and send Ctrl+C after 1 second

(sleep 1 && killall finch 2>/dev/null) &
./target/debug/finch 2>&1 | head -50

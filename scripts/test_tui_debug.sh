#!/bin/bash
#  Test script to check TUI stdout debug output

# Make sure we're in interactive mode (not piped)
# Start finch and send Ctrl+C after 1 second

(sleep 1 && killall finch 2>/dev/null) &
./target/debug/finch 2>&1 | head -50

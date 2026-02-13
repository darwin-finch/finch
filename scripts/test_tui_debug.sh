#!/bin/bash
#  Test script to check TUI stdout debug output

# Make sure we're in interactive mode (not piped)
# Start shammah and send Ctrl+C after 1 second

(sleep 1 && killall shammah 2>/dev/null) &
./target/debug/shammah 2>&1 | head -50

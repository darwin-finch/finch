#!/bin/bash
# Test script for HTTP server

set -e

echo "Testing Shammah HTTP daemon mode..."
echo

# Check if binary exists
if [ ! -f "target/release/shammah" ]; then
    echo "Error: Binary not found. Run 'cargo build --release' first."
    exit 1
fi

# Start daemon in background
echo "Starting daemon on port 18000..."
./target/release/shammah daemon --bind 127.0.0.1:18000 &
DAEMON_PID=$!

echo "Daemon started with PID: $DAEMON_PID"
sleep 2

# Test health endpoint
echo
echo "Testing /health endpoint..."
curl -s http://127.0.0.1:18000/health | jq '.' || echo "Health check failed"

# Test metrics endpoint
echo
echo "Testing /metrics endpoint..."
curl -s http://127.0.0.1:18000/metrics | head -5 || echo "Metrics check failed"

# Kill daemon
echo
echo "Stopping daemon..."
kill $DAEMON_PID 2>/dev/null || true
wait $DAEMON_PID 2>/dev/null || true

echo
echo "Test complete!"

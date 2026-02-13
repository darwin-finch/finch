#!/bin/bash
# Test script for model persistence and training visibility

set -e

echo "=== Testing Model Persistence and Training Visibility ==="
echo ""

# Clean up any existing models
echo "1. Cleaning up old models..."
rm -rf ~/.shammah/models/
mkdir -p ~/.shammah/models/

echo "2. First session - should create new models..."
echo "Testing persistence" | timeout 2 cargo run --quiet || true

echo ""
echo "3. Checking if models were saved..."
if [ -f ~/.shammah/models/threshold_router.json ]; then
    echo "✓ Router model saved"
    echo "Router stats:"
    jq '.total_queries' ~/.shammah/models/threshold_router.json
else
    echo "✗ Router model NOT saved"
fi

if [ -f ~/.shammah/models/threshold_validator.json ]; then
    echo "✓ Validator model saved"
else
    echo "✗ Validator model NOT saved"
fi

echo ""
echo "4. Second session - should load existing models..."
echo "/quit" | timeout 2 cargo run --quiet || true

echo ""
echo "5. Checking metrics..."
if [ -f ~/.shammah/metrics/$(date +%Y-%m-%d).jsonl ]; then
    echo "✓ Metrics file exists"
    echo "Number of logged queries:"
    wc -l ~/.shammah/metrics/$(date +%Y-%m-%d).jsonl

    echo ""
    echo "Sample metric (first entry):"
    head -1 ~/.shammah/metrics/$(date +%Y-%m-%d).jsonl | jq '.' || true
else
    echo "✗ Metrics file NOT found"
fi

echo ""
echo "=== Test Complete ==="

#!/usr/bin/env bash
# Test script for tool pass-through in daemon architecture
#
# This script tests that tools work correctly through the daemon API.

set -euo pipefail

script_path="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
source "$(dirname "$script_path")/lib/brain_test_isolation.sh"
brain_test_isolation_reexec_launcher "$script_path" "$@"

finch_bin="${FINCH_BIN:-target/release/finch}"
[[ -x "$finch_bin" ]] || { echo "Error: Binary not found. Run 'cargo build --release' first." >&2; exit 1; }
[[ -n "${ANTHROPIC_API_KEY:-}" ]] || { echo 'ANTHROPIC_API_KEY is required for this live pass-through smoke' >&2; exit 1; }
TEST_DIR=$(mktemp -d "$HOME/finch-tool-passthrough.XXXXXX")
address_file="$HOME/.finch/tool-passthrough.addr"
daemon_pid=''
cleanup() {
    trap - EXIT INT TERM HUP
    rm -rf -- "$TEST_DIR"
    rm -f -- "$address_file"
}
trap cleanup EXIT INT TERM HUP
mkdir -p "$HOME/.finch"
FINCH_TEST_BOUND_ADDR_FILE="$address_file" "$finch_bin" daemon --bind 127.0.0.1:0 & daemon_pid=$!
for _ in {1..100}; do [[ -s "$address_file" ]] && break; sleep 0.05; done
[[ -s "$address_file" ]] || { echo 'Daemon did not publish its bound address' >&2; exit 1; }
DAEMON_URL="http://$(cat "$address_file")"

echo "🧪 Testing Tool Pass-Through in Daemon Architecture"
echo "=================================================="
echo ""
echo "Test directory: $TEST_DIR"
echo "Daemon URL: $DAEMON_URL"
echo ""

# Create test files
cd "$TEST_DIR"
echo "test content" > test_file.txt
echo "another file" > test_file2.txt

echo "📝 Created test files:"
ls -1
echo ""

# Test 1: Check daemon is running
echo "Test 1: Check daemon health"
echo "----------------------------"
if curl --fail --show-error --silent "$DAEMON_URL/health" > /dev/null; then
    echo "✅ Daemon is running"
else
    echo "❌ Daemon is not running. Start it with: finch daemon"
    exit 1
fi
echo ""

# Test 2: Send query with tools (should receive tool_calls)
echo "Test 2: Request with tools (expect tool_calls)"
echo "----------------------------------------------"
RESPONSE=$(curl --fail --show-error --silent -X POST "$DAEMON_URL/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude",
    "messages": [{"role": "user", "content": "List all files in the current directory"}],
    "tools": [{
      "type": "function",
      "function": {
        "name": "bash",
        "description": "Execute bash command",
        "parameters": {
          "type": "object",
          "properties": {
            "command": {
              "type": "string",
              "description": "The bash command to execute"
            }
          }
        }
      }
    }]
  }')

echo "Response:"
echo "$RESPONSE" | jq .

if echo "$RESPONSE" | jq -e '.choices[0].message.tool_calls' > /dev/null 2>&1; then
    echo "✅ Received tool_calls from daemon"
else
    echo "No tool_calls in response" >&2
    exit 1
fi
echo ""

# Test 3: Multi-turn with tool results
echo "Test 3: Multi-turn with tool results"
echo "-------------------------------------"

# First, get the tool call from the previous response
TOOL_CALL_ID=$(echo "$RESPONSE" | jq -r '.choices[0].message.tool_calls[0].id // "call_test123"')
COMMAND=$(echo "$RESPONSE" | jq -r '.choices[0].message.tool_calls[0].function.arguments // "{\"command\":\"ls\"}"')

echo "Tool call ID: $TOOL_CALL_ID"
echo "Command: $COMMAND"
echo ""

# Simulate executing the tool locally
TOOL_RESULT=$(ls -1)
echo "Tool result (from local execution):"
echo "$TOOL_RESULT"
echo ""

# Send tool result back. jq performs all JSON escaping so model-supplied
# arguments and multiline tool output cannot corrupt the request body.
REQUEST2=$(jq -n \
  --arg id "$TOOL_CALL_ID" \
  --arg arguments "$COMMAND" \
  --arg result "$TOOL_RESULT" \
  '{
    model: "claude",
    messages: [
      {role: "user", content: "List all files in the current directory"},
      {role: "assistant", tool_calls: [{id: $id, type: "function", function: {name: "bash", arguments: $arguments}}]},
      {role: "tool", tool_call_id: $id, content: $result}
    ],
    tools: [{type: "function", function: {name: "bash", description: "Execute bash command", parameters: {type: "object", properties: {command: {type: "string"}}}}}]
  }')
RESPONSE2=$(curl --fail --show-error --silent -X POST "$DAEMON_URL/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d "$REQUEST2")

echo "Response after tool execution:"
echo "$RESPONSE2" | jq .

FINAL_CONTENT=$(echo "$RESPONSE2" | jq -r '.choices[0].message.content // ""')
if [ -n "$FINAL_CONTENT" ]; then
    echo "✅ Received final answer with tool results"
    echo "Final answer: $FINAL_CONTENT"
else
    echo "No content in final response" >&2
    exit 1
fi
echo ""

# Cleanup
cd "$HOME"
echo "🧹 Test directory will be removed by the ownership trap"
echo ""

echo "=================================================="
echo "✅ Tool pass-through tests complete!"
echo ""
echo "Summary:"
echo "- Daemon responded to health check"
echo "- Tool calls can be sent/received through API"
echo "- Multi-turn conversation with tool results works"

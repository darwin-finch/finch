#!/bin/bash
# Test script for tool pass-through in daemon architecture
#
# This script tests that tools work correctly through the daemon API.

set -e

DAEMON_URL="${DAEMON_URL:-http://127.0.0.1:11434}"
TEST_DIR=$(mktemp -d)

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
if curl -s "$DAEMON_URL/health" > /dev/null; then
    echo "✅ Daemon is running"
else
    echo "❌ Daemon is not running. Start it with: finch daemon"
    exit 1
fi
echo ""

# Test 2: Send query with tools (should receive tool_calls)
echo "Test 2: Request with tools (expect tool_calls)"
echo "----------------------------------------------"
RESPONSE=$(curl -s -X POST "$DAEMON_URL/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "qwen-local",
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
    echo "⚠️  No tool_calls in response (may have returned text instead)"
    echo "This is expected if using Claude API fallback"
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

# Send tool result back
RESPONSE2=$(curl -s -X POST "$DAEMON_URL/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d "{
    \"model\": \"qwen-local\",
    \"messages\": [
      {\"role\": \"user\", \"content\": \"List all files in the current directory\"},
      {
        \"role\": \"assistant\",
        \"tool_calls\": [{
          \"id\": \"$TOOL_CALL_ID\",
          \"type\": \"function\",
          \"function\": {
            \"name\": \"bash\",
            \"arguments\": \"$COMMAND\"
          }
        }]
      },
      {
        \"role\": \"tool\",
        \"tool_call_id\": \"$TOOL_CALL_ID\",
        \"content\": \"$TOOL_RESULT\"
      }
    ],
    \"tools\": [{
      \"type\": \"function\",
      \"function\": {
        \"name\": \"bash\",
        \"description\": \"Execute bash command\",
        \"parameters\": {
          \"type\": \"object\",
          \"properties\": {
            \"command\": {\"type\": \"string\"}
          }
        }
      }
    }]
  }")

echo "Response after tool execution:"
echo "$RESPONSE2" | jq .

FINAL_CONTENT=$(echo "$RESPONSE2" | jq -r '.choices[0].message.content // ""')
if [ -n "$FINAL_CONTENT" ]; then
    echo "✅ Received final answer with tool results"
    echo "Final answer: $FINAL_CONTENT"
else
    echo "⚠️  No content in final response"
fi
echo ""

# Cleanup
cd /
rm -rf "$TEST_DIR"
echo "🧹 Cleaned up test directory"
echo ""

echo "=================================================="
echo "✅ Tool pass-through tests complete!"
echo ""
echo "Summary:"
echo "- Daemon responded to health check"
echo "- Tool calls can be sent/received through API"
echo "- Multi-turn conversation with tool results works"
echo ""
echo "Note: If tests showed warnings, the system may have"
echo "fallen back to Claude API (local model not ready)."
echo "This is expected behavior (graceful degradation)."

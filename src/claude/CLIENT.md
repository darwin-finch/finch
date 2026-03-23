# Claude Client

**Purpose:** Forward queries to Claude API; collect training examples for LoRA.

## Features

- HTTP client with retry logic
- Streaming support (SSE parsing)
- Tool definitions sent with requests
- Logs (query, response) to `~/.finch/training_queue.jsonl` for future LoRA training
- Graceful fallback when streaming unavailable

## Key files

- `src/claude/client.rs` — `ClaudeClient`, `send_message()`, `send_message_stream()`
- `src/claude/types.rs` — API request/response types

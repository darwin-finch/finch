# Claude Client

**Purpose:** Forward queries to the Claude API.

## Features

- HTTP client with retry logic
- Streaming support (SSE parsing)
- Tool definitions sent with requests
- Does not collect requests or responses for training
- Graceful fallback when streaming unavailable

## Key files

- `src/claude/client.rs` — `ClaudeClient`, `send_message()`, `send_message_stream()`
- `src/claude/types.rs` — API request/response types

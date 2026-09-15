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
- `src/claude/types.rs` — Claude-API request/response envelopes only
  (`MessageRequest`, `MessageResponse`). The universal conversation types
  (`Message`, `ContentBlock`, `ImageSource`) live in
  `crate::providers::wire_types` and are used through the providers facade;
  this client consumes them like any other transport.

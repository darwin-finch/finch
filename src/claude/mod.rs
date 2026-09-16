// Claude API client module
// Public interface for interacting with Anthropic Claude API

mod client;

pub use client::ClaudeClient;
pub use finch_providers::{MessageRequest, MessageResponse, StreamDelta, StreamEvent};

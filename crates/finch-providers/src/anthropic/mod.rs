//! Anthropic Messages transport envelopes and SSE parsing.

mod streaming;
mod types;

pub(crate) use streaming::StreamEvent;
pub use types::{MessageRequest, MessageResponse};

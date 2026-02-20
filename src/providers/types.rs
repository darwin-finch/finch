// Unified request/response types for multi-provider LLM support
//
// These types abstract over provider-specific formats (Claude, OpenAI, Gemini, etc.)
// allowing the rest of the codebase to work with a unified interface.

use crate::claude::types::{ContentBlock, Message};
use crate::tools::types::ToolDefinition;
use serde::{Deserialize, Serialize};

/// Unified request format for all LLM providers
///
/// This wraps the existing Message format and adds provider-agnostic options.
/// Each provider implementation will transform this into their specific API format.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderRequest {
    /// Conversation messages (using Claude's Message format as the common denominator)
    pub messages: Vec<Message>,

    /// Model name (provider-specific)
    pub model: String,

    /// Maximum tokens to generate
    pub max_tokens: u32,

    /// System prompt (provider-specific handling: sent as `system` for Claude,
    /// prepended as a `{"role":"system"}` message for OpenAI-compatible providers)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,

    /// Tool definitions (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,

    /// Temperature (0.0 to 1.0, optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Whether to stream the response
    #[serde(skip)]
    pub stream: bool,
}

impl ProviderRequest {
    /// Create a new request from messages
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            model: String::new(), // Will be set by provider
            max_tokens: 4096,
            system: None,
            tools: None,
            temperature: None,
            stream: false,
        }
    }

    /// Set the model name
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Set max tokens
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Set system prompt
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Add tools to the request
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Enable streaming
    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// Set temperature
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Remove orphaned tool_use blocks from the end of the conversation.
    ///
    /// When a provider fails mid-agentic-loop (e.g. Grok 403), the conversation
    /// may end with an assistant message containing ToolUse blocks that have no
    /// corresponding ToolResult in the next message. Claude (and others) reject
    /// such histories with a 400 error. This method trims those orphaned tail
    /// messages so the fallback provider sees a clean conversation.
    pub fn sanitize_messages(&mut self) {
        use crate::claude::types::ContentBlock;

        loop {
            // Find the last assistant message index
            let last_assistant = self.messages.iter().rposition(|m| m.role == "assistant");
            let last_assistant_idx = match last_assistant {
                Some(i) => i,
                None => break,
            };

            // Check if it contains any ToolUse blocks
            let has_tool_use = self.messages[last_assistant_idx]
                .content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { .. }));

            if !has_tool_use {
                break;
            }

            // Check if the very next message has ToolResult blocks for all tool uses
            let tool_use_ids: Vec<String> = self.messages[last_assistant_idx]
                .content
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::ToolUse { id, .. } = b {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();

            let next_idx = last_assistant_idx + 1;
            let all_matched = if next_idx < self.messages.len() {
                tool_use_ids.iter().all(|id| {
                    self.messages[next_idx].content.iter().any(|b| {
                        matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == id)
                    })
                })
            } else {
                false
            };

            if all_matched {
                break;
            }

            // Orphaned — remove the assistant message (and any partial result after it)
            if next_idx < self.messages.len()
                && self.messages[next_idx].role == "user"
                && self.messages[next_idx]
                    .content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
            {
                self.messages.remove(next_idx);
            }
            self.messages.remove(last_assistant_idx);
        }
    }
}

/// Unified response format from LLM providers
///
/// This wraps the provider-specific response in a common format.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderResponse {
    /// Response ID (provider-specific)
    pub id: String,

    /// Model that generated the response
    pub model: String,

    /// Content blocks (text, tool_use, etc.)
    pub content: Vec<ContentBlock>,

    /// Why the model stopped generating
    pub stop_reason: Option<String>,

    /// Role of the responder (usually "assistant")
    pub role: String,

    /// Provider name (e.g., "claude", "openai", "gemini")
    pub provider: String,
}

impl ProviderResponse {
    /// Extract text from the response
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| block.as_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Check if response contains tool uses
    pub fn has_tool_uses(&self) -> bool {
        self.content.iter().any(|block| block.is_tool_use())
    }

    /// Extract tool uses from response
    pub fn tool_uses(&self) -> Vec<crate::tools::types::ToolUse> {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => Some(crate::tools::types::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Convert to Message for conversation history
    pub fn to_message(&self) -> Message {
        Message {
            role: self.role.clone(),
            content: self.content.clone(),
        }
    }
}

/// Stream chunk types for streaming responses
///
/// Re-export from generators module for convenience
pub use crate::generators::StreamChunk;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::types::Message;

    fn user_msg(text: &str) -> Message {
        Message::user(text)
    }

    #[test]
    fn test_provider_request_defaults() {
        let req = ProviderRequest::new(vec![user_msg("Hello")]);
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.model, "");
        assert_eq!(req.max_tokens, 4096);
        assert!(req.tools.is_none());
        assert!(req.temperature.is_none());
        assert!(!req.stream);
    }

    #[test]
    fn test_provider_request_builder_chain() {
        let req = ProviderRequest::new(vec![user_msg("Hello")])
            .with_model("claude-sonnet-4-6")
            .with_max_tokens(1024)
            .with_temperature(0.7)
            .with_stream(true);

        assert_eq!(req.model, "claude-sonnet-4-6");
        assert_eq!(req.max_tokens, 1024);
        assert_eq!(req.temperature, Some(0.7));
        assert!(req.stream);
    }

    #[test]
    fn test_provider_request_with_model_string_conversions() {
        // with_model accepts &str and String
        let req1 = ProviderRequest::new(vec![]).with_model("gpt-4");
        let req2 = ProviderRequest::new(vec![]).with_model("gemini-pro".to_string());
        assert_eq!(req1.model, "gpt-4");
        assert_eq!(req2.model, "gemini-pro");
    }

    #[test]
    fn test_provider_request_multiple_messages() {
        let req = ProviderRequest::new(vec![
            Message::user("first"),
            Message::assistant("response"),
            Message::user("second"),
        ]);
        assert_eq!(req.messages.len(), 3);
    }
}

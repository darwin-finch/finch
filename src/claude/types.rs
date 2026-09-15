// Claude API request/response envelopes.
//
// The universal conversation types (`Message`, `ContentBlock`, `ImageSource`)
// live in `crate::providers::wire_types` and are reached through the providers
// facade; this file only defines the Claude-API-specific request and response
// envelopes that wrap them on the wire.

use serde::{Deserialize, Serialize};

use crate::config::{DEFAULT_CLAUDE_MODEL, DEFAULT_MAX_TOKENS};
use crate::providers::{ContentBlock, Message};
use crate::tools::ToolDefinition;

#[derive(Debug, Clone, Serialize)]
pub struct MessageRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
}

impl MessageRequest {
    pub fn new(user_query: &str) -> Self {
        Self {
            model: DEFAULT_CLAUDE_MODEL.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            messages: vec![Message::user(user_query)],
            system: None,
            tools: None,
        }
    }

    /// Create request with full conversation context
    pub fn with_context(messages: Vec<Message>) -> Self {
        Self {
            model: DEFAULT_CLAUDE_MODEL.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            messages,
            system: None,
            tools: None,
        }
    }

    /// Set a system prompt for the request
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Append a user message to existing conversation
    pub fn append_user_message(mut self, content: String) -> Self {
        self.messages.push(Message::user(content));
        self
    }

    /// Add tools to the request
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = Some(tools);
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub response_type: String,
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub input_tokens: Option<u32>,
    #[serde(default)]
    pub output_tokens: Option<u32>,
    #[serde(default)]
    pub primary_allowance_used_percent: Option<f32>,
    #[serde(default)]
    pub secondary_allowance_used_percent: Option<f32>,
}

impl MessageResponse {
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
    pub fn tool_uses(&self) -> Vec<crate::tools::ToolUse> {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => Some(crate::tools::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Convert response to a Message for conversation history
    pub fn to_message(&self) -> Message {
        Message {
            role: self.role.clone(),
            content: self.content.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ContentBlock as WireContentBlock;
    use crate::providers::Message as WireMessage;

    // --- MessageRequest ---

    #[test]
    fn test_message_request_new_single_user_message() {
        let req = MessageRequest::new("test query");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[0].text(), "test query");
        assert!(req.tools.is_none());
    }

    #[test]
    fn test_message_request_append_user_message() {
        let req = MessageRequest::new("first").append_user_message("second".to_string());
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[1].text(), "second");
    }

    #[test]
    fn test_message_request_with_tools() {
        use crate::tools::ToolInputSchema;
        let tool = ToolDefinition {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            input_schema: ToolInputSchema::simple(vec![("file_path", "Path to read")]),
        };
        let req = MessageRequest::new("query").with_tools(vec![tool]);
        assert!(req.tools.is_some());
        assert_eq!(req.tools.unwrap().len(), 1);
    }

    #[test]
    fn test_message_request_with_context() {
        let messages = vec![WireMessage::user("hi"), WireMessage::assistant("hello")];
        let req = MessageRequest::with_context(messages);
        assert_eq!(req.messages.len(), 2);
    }

    #[test]
    fn test_message_response_to_message_preserves_content() {
        let response = MessageResponse {
            id: "resp_1".to_string(),
            response_type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![WireContentBlock::text("answer")],
            model: "claude-test".to_string(),
            stop_reason: Some("end_turn".to_string()),
            input_tokens: Some(1),
            output_tokens: Some(2),
            primary_allowance_used_percent: None,
            secondary_allowance_used_percent: None,
        };
        let message = response.to_message();
        assert_eq!(message.role, "assistant");
        assert_eq!(message.text(), "answer");
    }
}

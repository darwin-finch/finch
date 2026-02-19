// Claude API request/response types

use serde::{Deserialize, Serialize};
use serde_json::Value;

// Re-export tool types for convenience
pub use crate::tools::types::ToolDefinition;

/// Content block - supports text, tool_use, and tool_result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

impl ContentBlock {
    /// Check if this is a text block
    pub fn is_text(&self) -> bool {
        matches!(self, ContentBlock::Text { .. })
    }

    /// Check if this is a tool use block
    pub fn is_tool_use(&self) -> bool {
        matches!(self, ContentBlock::ToolUse { .. })
    }

    /// Extract text from text block
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        }
    }

    /// Create a text content block
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// Create a tool result content block
    pub fn tool_result(tool_use_id: String, content: String, is_error: Option<bool>) -> Self {
        Self::ToolResult {
            tool_use_id,
            content,
            is_error,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(with = "content_serializer")]
    pub content: Vec<ContentBlock>,
}

// Custom serializer to handle both string and array content
mod content_serializer {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(content: &Vec<ContentBlock>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // If only one text block, serialize as string for backwards compatibility
        if content.len() == 1 {
            if let ContentBlock::Text { text } = &content[0] {
                return text.serialize(serializer);
            }
        }
        // Otherwise serialize as array of content blocks
        content.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<ContentBlock>, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;

        let value: Value = Value::deserialize(deserializer)?;

        match value {
            Value::String(text) => Ok(vec![ContentBlock::Text { text }]),
            Value::Array(arr) => {
                let mut content_blocks = Vec::new();
                for item in arr {
                    let block: ContentBlock = ContentBlock::deserialize(item).map_err(|e| {
                        D::Error::custom(format!("Failed to parse content block: {}", e))
                    })?;
                    content_blocks.push(block);
                }
                Ok(content_blocks)
            }
            _ => Err(D::Error::custom(
                "Content must be string or array of content blocks",
            )),
        }
    }
}

impl Message {
    /// Create a user message with text content
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![ContentBlock::text(content.into())],
        }
    }

    /// Create an assistant message with text content
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: vec![ContentBlock::text(content.into())],
        }
    }

    /// Create a message with rich content blocks
    pub fn with_content(role: impl Into<String>, content: Vec<ContentBlock>) -> Self {
        Self {
            role: role.into(),
            content,
        }
    }

    /// Add a content block to this message
    pub fn add_content(mut self, block: ContentBlock) -> Self {
        self.content.push(block);
        self
    }

    /// Extract text content from this message
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| block.as_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Add tool result to this message
    pub fn add_tool_result(mut self, tool_use_id: String, result: String, is_error: bool) -> Self {
        self.content.push(ContentBlock::tool_result(
            tool_use_id,
            result,
            if is_error { Some(true) } else { None },
        ));
        self
    }

    /// Extract text from the message
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| block.as_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Check if message contains tool results
    pub fn has_tool_results(&self) -> bool {
        self.content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
    }

    /// Check if message has no text content
    pub fn is_empty_text(&self) -> bool {
        self.text().is_empty()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
}

impl MessageRequest {
    pub fn new(user_query: &str) -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
            messages: vec![Message::user(user_query)],
            tools: None,
        }
    }

    /// Create request with full conversation context
    pub fn with_context(messages: Vec<Message>) -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
            messages,
            tools: None,
        }
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

    /// Convert response to a Message for conversation history
    pub fn to_message(&self) -> Message {
        Message {
            role: self.role.clone(),
            content: self.content.clone(),
        }
    }
}

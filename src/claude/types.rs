// Claude API request/response types

use serde::{Deserialize, Serialize};
use serde_json::Value;

// Re-export tool types for convenience
pub use crate::tools::types::ToolDefinition;

/// Content block - supports text, image, tool_use, and tool_result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },

    /// Base64-encoded image (for pasted images, screenshots, etc.)
    #[serde(rename = "image")]
    Image { source: ImageSource },

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

/// Source for an image content block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String, // "base64"
    pub media_type: String, // "image/png", "image/jpeg", etc.
    pub data: String,       // Base64-encoded image data
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

    /// Create a base64 image content block
    pub fn image(media_type: impl Into<String>, base64_data: impl Into<String>) -> Self {
        Self::Image {
            source: ImageSource {
                source_type: "base64".to_string(),
                media_type: media_type.into(),
                data: base64_data.into(),
            },
        }
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
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
}

impl MessageRequest {
    pub fn new(user_query: &str) -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
            messages: vec![Message::user(user_query)],
            system: None,
            tools: None,
        }
    }

    /// Create request with full conversation context
    pub fn with_context(messages: Vec<Message>) -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    // --- ContentBlock helpers ---

    #[test]
    fn test_text_block_helpers() {
        let block = ContentBlock::text("hello");
        assert!(block.is_text());
        assert!(!block.is_tool_use());
        assert_eq!(block.as_text(), Some("hello"));
    }

    #[test]
    fn test_image_block_helpers() {
        let block = ContentBlock::image("image/png", "base64data");
        assert!(!block.is_text());
        assert!(!block.is_tool_use());
        assert_eq!(block.as_text(), None);
    }

    #[test]
    fn test_tool_use_block_helpers() {
        let block = ContentBlock::ToolUse {
            id: "id1".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"path": "/tmp/test"}),
        };
        assert!(block.is_tool_use());
        assert!(!block.is_text());
        assert_eq!(block.as_text(), None);
    }

    #[test]
    fn test_tool_result_block_helpers() {
        let block = ContentBlock::tool_result("id1".to_string(), "file contents".to_string(), None);
        assert!(!block.is_text());
        assert!(!block.is_tool_use());
        assert_eq!(block.as_text(), None);
    }

    #[test]
    fn test_tool_result_with_error_flag() {
        let block =
            ContentBlock::tool_result("id1".to_string(), "error msg".to_string(), Some(true));
        match block {
            ContentBlock::ToolResult {
                is_error,
                content,
                tool_use_id,
            } => {
                assert_eq!(is_error, Some(true));
                assert_eq!(content, "error msg");
                assert_eq!(tool_use_id, "id1");
            }
            _ => panic!("Expected ToolResult"),
        }
    }

    #[test]
    fn test_content_block_text_serde_roundtrip() {
        let block = ContentBlock::text("roundtrip");
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back.as_text(), Some("roundtrip"));
    }

    #[test]
    fn test_image_source_serde_roundtrip() {
        let block = ContentBlock::image("image/jpeg", "abc123");
        let json = serde_json::to_string(&block).unwrap();
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        match back {
            ContentBlock::Image { source } => {
                assert_eq!(source.source_type, "base64");
                assert_eq!(source.media_type, "image/jpeg");
                assert_eq!(source.data, "abc123");
            }
            _ => panic!("Expected Image"),
        }
    }

    // --- Message construction ---

    #[test]
    fn test_message_user() {
        let msg = Message::user("hello");
        assert_eq!(msg.role, "user");
        assert_eq!(msg.text_content(), "hello");
        assert_eq!(msg.text(), "hello");
    }

    #[test]
    fn test_message_assistant() {
        let msg = Message::assistant("world");
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.text(), "world");
    }

    #[test]
    fn test_message_text_content_joins_text_blocks_only() {
        let msg = Message::with_content(
            "user",
            vec![
                ContentBlock::text("first"),
                ContentBlock::ToolUse {
                    id: "id1".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({}),
                },
                ContentBlock::text("second"),
            ],
        );
        // text_content only concatenates text blocks, skipping ToolUse
        assert_eq!(msg.text_content(), "first\nsecond");
    }

    #[test]
    fn test_message_has_tool_results() {
        let msg = Message::user("test");
        assert!(!msg.has_tool_results());
        let msg_with = msg.add_tool_result("id1".to_string(), "result".to_string(), false);
        assert!(msg_with.has_tool_results());
    }

    #[test]
    fn test_message_is_empty_text_no_text_blocks() {
        let msg = Message::with_content(
            "user",
            vec![ContentBlock::ToolUse {
                id: "id".to_string(),
                name: "tool".to_string(),
                input: serde_json::json!({}),
            }],
        );
        assert!(msg.is_empty_text());
    }

    #[test]
    fn test_message_is_empty_text_has_text() {
        let msg = Message::user("not empty");
        assert!(!msg.is_empty_text());
    }

    #[test]
    fn test_message_add_content() {
        let msg = Message::user("initial").add_content(ContentBlock::text("extra"));
        assert_eq!(msg.content.len(), 2);
    }

    #[test]
    fn test_message_add_tool_result_success() {
        let msg =
            Message::user("query").add_tool_result("id1".to_string(), "output".to_string(), false);
        assert!(msg.has_tool_results());
        // is_error should be None when false (skipped by serde)
        match &msg.content[1] {
            ContentBlock::ToolResult { is_error, .. } => assert_eq!(*is_error, None),
            _ => panic!("Expected ToolResult"),
        }
    }

    #[test]
    fn test_message_add_tool_result_error() {
        let msg =
            Message::user("query").add_tool_result("id1".to_string(), "oops".to_string(), true);
        match &msg.content[1] {
            ContentBlock::ToolResult { is_error, .. } => assert_eq!(*is_error, Some(true)),
            _ => panic!("Expected ToolResult"),
        }
    }

    // --- Message serde (custom content_serializer) ---

    #[test]
    fn test_message_serde_single_text_serializes_as_string() {
        let msg = Message::user("hello world");
        let json = serde_json::to_string(&msg).unwrap();
        // Single text block → compact string, not array
        assert!(json.contains("\"hello world\""));
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.text(), "hello world");
    }

    #[test]
    fn test_message_serde_multi_block_serializes_as_array() {
        let msg = Message::with_content(
            "user",
            vec![
                ContentBlock::text("text part"),
                ContentBlock::tool_result("id1".to_string(), "result".to_string(), None),
            ],
        );
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content.len(), 2);
    }

    #[test]
    fn test_message_deserialized_from_plain_string() {
        // Claude API sometimes returns content as a plain string
        let json = r#"{"role":"user","content":"plain string content"}"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        assert_eq!(msg.text(), "plain string content");
    }

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
        use crate::tools::types::ToolInputSchema;
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
        let messages = vec![Message::user("hi"), Message::assistant("hello")];
        let req = MessageRequest::with_context(messages);
        assert_eq!(req.messages.len(), 2);
    }
}

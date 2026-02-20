// Core types for tool execution system
//
// Compatible with Claude API tool use format

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::ReplMode;
use crate::cli::ConversationHistory;
use crate::local::LocalGenerator;
use crate::models::tokenizer::TextTokenizer;
use crate::training::batch_trainer::BatchTrainer;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Context passed to tools during execution
pub struct ToolContext<'a> {
    /// Optional conversation history (for tools that need to save/restore state)
    pub conversation: Option<&'a ConversationHistory>,

    /// Optional function to save model weights (for restart tools)
    pub save_models: Option<&'a (dyn Fn() -> Result<()> + Send + Sync)>,

    /// Optional batch trainer for active learning tools
    pub batch_trainer: Option<Arc<RwLock<BatchTrainer>>>,

    /// Optional local generator for query_local tool
    pub local_generator: Option<Arc<RwLock<LocalGenerator>>>,

    /// Optional tokenizer for encoding/decoding text
    pub tokenizer: Option<Arc<TextTokenizer>>,

    /// Optional REPL mode for plan mode state
    pub repl_mode: Option<Arc<RwLock<ReplMode>>>,

    /// Optional plan content storage
    pub plan_content: Option<Arc<RwLock<Option<String>>>>,
}

/// Tool definition (Claude API-compatible)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: ToolInputSchema,
}

/// JSON Schema for tool input parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInputSchema {
    #[serde(rename = "type")]
    pub schema_type: String, // Usually "object"
    pub properties: Value,
    pub required: Vec<String>,
}

impl ToolInputSchema {
    /// Create a simple schema with required string parameters
    pub fn simple(params: Vec<(&str, &str)>) -> Self {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for (param_name, param_desc) in params.iter() {
            properties.insert(
                param_name.to_string(),
                serde_json::json!({
                    "type": "string",
                    "description": param_desc
                }),
            );
            required.push(param_name.to_string());
        }

        Self {
            schema_type: "object".to_string(),
            properties: Value::Object(properties),
            required,
        }
    }
}

/// Tool use request (from generator or Claude API)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUse {
    pub id: String,   // Format: toolu_[random]
    pub name: String, // Tool name
    pub input: Value, // Tool parameters (JSON object)
}

impl ToolUse {
    /// Generate unique tool use ID
    pub fn generate_id() -> String {
        use rand::Rng;
        let random: String = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(24)
            .map(char::from)
            .collect();
        format!("toolu_{}", random)
    }

    pub fn new(name: String, input: Value) -> Self {
        Self {
            id: Self::generate_id(),
            name,
            input,
        }
    }
}

/// Tool execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(tool_use_id: String, content: String) -> Self {
        Self {
            tool_use_id,
            content,
            is_error: false,
        }
    }

    pub fn error(tool_use_id: String, error_message: String) -> Self {
        Self {
            tool_use_id,
            content: error_message,
            is_error: true,
        }
    }
}

/// Extended ContentBlock enum to support tool use
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

    /// Check if this is a tool result block
    pub fn is_tool_result(&self) -> bool {
        matches!(self, ContentBlock::ToolResult { .. })
    }

    /// Extract text from text block
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        }
    }

    /// Extract tool use from tool use block
    pub fn as_tool_use(&self) -> Option<ToolUse> {
        match self {
            ContentBlock::ToolUse { id, name, input } => Some(ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_use_id_generation() {
        let id = ToolUse::generate_id();
        assert!(id.starts_with("toolu_"));
        assert_eq!(id.len(), 30); // "toolu_" + 24 chars
    }

    #[test]
    fn test_tool_result_success() {
        let result = ToolResult::success("toolu_123".to_string(), "Success".to_string());
        assert_eq!(result.tool_use_id, "toolu_123");
        assert_eq!(result.content, "Success");
        assert!(!result.is_error);
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolResult::error("toolu_123".to_string(), "Failed".to_string());
        assert_eq!(result.tool_use_id, "toolu_123");
        assert_eq!(result.content, "Failed");
        assert!(result.is_error);
    }

    #[test]
    fn test_content_block_text_serialization() {
        let block = ContentBlock::Text {
            text: "Hello".to_string(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"Hello\""));
    }

    #[test]
    fn test_content_block_tool_use_serialization() {
        let block = ContentBlock::ToolUse {
            id: "toolu_123".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "ls"}),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"tool_use\""));
        assert!(json.contains("\"name\":\"bash\""));
    }

    #[test]
    fn test_simple_input_schema() {
        let schema = ToolInputSchema::simple(vec![
            ("file_path", "The path to the file to read"),
            ("encoding", "The file encoding (utf-8, ascii, etc.)"),
        ]);

        assert_eq!(schema.schema_type, "object");
        assert_eq!(schema.required.len(), 2);
        assert!(schema.required.contains(&"file_path".to_string()));
        assert!(schema.required.contains(&"encoding".to_string()));
    }
}

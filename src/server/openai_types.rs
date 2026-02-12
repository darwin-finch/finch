// OpenAI-compatible API types
//
// These types match the OpenAI Chat Completions API format
// to enable compatibility with VSCode extensions and other tools.

use serde::{Deserialize, Serialize};

/// Request body for /v1/chat/completions endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    /// Model identifier (e.g., "qwen-local", "gpt-4")
    pub model: String,
    /// Messages in the conversation
    pub messages: Vec<ChatMessage>,
    /// Maximum tokens to generate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Temperature for sampling (0.0 to 2.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Top-p sampling parameter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Number of completions to generate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    /// Whether to stream responses (not yet supported)
    #[serde(default)]
    pub stream: bool,
    /// Stop sequences
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Tools available for function calling
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    /// Bypass routing and query local model directly (for testing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_only: Option<bool>,
}

/// Chat message in OpenAI format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Role: "system", "user", "assistant", or "tool"
    pub role: String,
    /// Message content (text)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Tool calls made by assistant
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// For tool role: the tool call ID this responds to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Optional name for the message sender
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Tool call in OpenAI format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique ID for this tool call
    pub id: String,
    /// Type: always "function" for now
    #[serde(rename = "type")]
    pub tool_type: String,
    /// Function details
    pub function: FunctionCall,
}

/// Function call details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Function name
    pub name: String,
    /// JSON-encoded arguments
    pub arguments: String,
}

/// Tool definition in OpenAI format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// Type: always "function"
    #[serde(rename = "type")]
    pub tool_type: String,
    /// Function details
    pub function: FunctionDefinition,
}

/// Function definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    /// Function name
    pub name: String,
    /// Function description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON schema for parameters
    pub parameters: serde_json::Value,
}

/// Response body for /v1/chat/completions endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    /// Unique ID for this completion
    pub id: String,
    /// Object type: "chat.completion"
    pub object: String,
    /// Unix timestamp of creation
    pub created: i64,
    /// Model used
    pub model: String,
    /// Completion choices
    pub choices: Vec<Choice>,
    /// Usage statistics
    pub usage: Usage,
}

/// Completion choice
#[derive(Debug, Serialize, Deserialize)]
pub struct Choice {
    /// Index in choices array
    pub index: u32,
    /// Generated message
    pub message: ChatMessage,
    /// Finish reason: "stop", "length", "tool_calls", etc.
    pub finish_reason: String,
}

/// Token usage statistics
#[derive(Debug, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens in prompt
    pub prompt_tokens: u32,
    /// Tokens in completion
    pub completion_tokens: u32,
    /// Total tokens
    pub total_tokens: u32,
}

/// Response for /v1/models endpoint
#[derive(Debug, Serialize)]
pub struct ModelsResponse {
    /// Object type: "list"
    pub object: String,
    /// List of available models
    pub data: Vec<Model>,
}

/// Model information
#[derive(Debug, Serialize)]
pub struct Model {
    /// Model ID
    pub id: String,
    /// Object type: "model"
    pub object: String,
    /// Creation timestamp
    pub created: i64,
    /// Owner organization
    pub owned_by: String,
}

impl ChatMessage {
    /// Create a new message
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Create a system message
    pub fn system(content: impl Into<String>) -> Self {
        Self::new("system", content)
    }

    /// Create a user message
    pub fn user(content: impl Into<String>) -> Self {
        Self::new("user", content)
    }

    /// Create an assistant message
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new("assistant", content)
    }
}

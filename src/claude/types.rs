// Claude API request/response types

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<Message>,
}

impl MessageRequest {
    pub fn new(user_query: &str) -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
            messages: vec![Message {
                role: "user".to_string(),
                content: user_query.to_string(),
            }],
        }
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

#[derive(Debug, Clone, Deserialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: String,
}

impl MessageResponse {
    /// Extract text from the response
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter(|block| block.block_type == "text")
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

//! Provider-facing tool-call wire types.
//!
//! These describe tools on the wire. Tool execution, permissions, and host
//! effects stay in the Finch application.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ContentBlock;

/// Tool definition (Claude API-compatible)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: ToolInputSchema,
}

/// JSON Schema for tool input parameters
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInputSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
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

/// Tool use request after adapter-level validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
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

    /// Convert to ContentBlock for conversation history
    pub fn to_content_block(&self) -> ContentBlock {
        ContentBlock::ToolUse {
            id: self.id.clone(),
            name: self.name.clone(),
            input: self.input.clone(),
        }
    }
}

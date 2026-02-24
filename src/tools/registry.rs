// Tool registry and trait definition
//
// Manages available tools and provides uniform execution interface

use crate::tools::types::{ToolContext, ToolDefinition, ToolInputSchema};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

/// Tool trait - all tools must implement this
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (e.g., "bash", "read", "glob")
    fn name(&self) -> &str;

    /// Human-readable description of what the tool does
    fn description(&self) -> &str;

    /// JSON Schema defining expected input parameters
    fn input_schema(&self) -> ToolInputSchema;

    /// Execute the tool with given input and context
    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String>;

    /// Get full tool definition (for Claude API)
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
        }
    }
}

/// Registry of available tools
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Create empty registry
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.insert(name, tool);
    }

    /// Get tool by name
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|b| b.as_ref())
    }

    /// Check if tool exists
    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// List all tool names
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Get all tool definitions (for Claude API)
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    /// Get all tools (for iteration)
    pub fn get_all_tools(&self) -> Vec<&dyn Tool> {
        self.tools.values().map(|t| t.as_ref()).collect()
    }

    /// Number of registered tools
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Check if registry is empty
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mock tool for testing
    struct MockTool {
        name: String,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "A mock tool for testing"
        }

        fn input_schema(&self) -> ToolInputSchema {
            ToolInputSchema::simple(vec![("param", "A test parameter")])
        }

        async fn execute(&self, _input: Value, _context: &ToolContext<'_>) -> Result<String> {
            Ok("Mock result".to_string())
        }
    }

    #[test]
    fn test_registry_registration() {
        let mut registry = ToolRegistry::new();
        let tool = MockTool {
            name: "test".to_string(),
        };
        registry.register(Box::new(tool));

        assert!(registry.has_tool("test"));
        assert!(!registry.has_tool("nonexistent"));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_registry_get_tool() {
        let mut registry = ToolRegistry::new();
        let tool = MockTool {
            name: "test".to_string(),
        };
        registry.register(Box::new(tool));

        let retrieved = registry.get("test");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name(), "test");
    }

    #[test]
    fn test_registry_tool_names() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool {
            name: "tool1".to_string(),
        }));
        registry.register(Box::new(MockTool {
            name: "tool2".to_string(),
        }));

        let names = registry.tool_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"tool1".to_string()));
        assert!(names.contains(&"tool2".to_string()));
    }

    #[tokio::test]
    async fn test_tool_execution() {
        let tool = MockTool {
            name: "test".to_string(),
        };

        // Create empty context for test
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
        };

        let result = tool
            .execute(serde_json::json!({"param": "value"}), &context)
            .await
            .unwrap();
        assert_eq!(result, "Mock result");
    }
}

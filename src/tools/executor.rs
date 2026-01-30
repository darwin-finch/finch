// Tool execution engine
//
// Executes tools with permission checks and multi-turn support

use crate::tools::permissions::{PermissionCheck, PermissionManager};
use crate::tools::registry::ToolRegistry;
use crate::tools::types::{ToolResult, ToolUse};
use anyhow::{Context, Result};
use tracing::{debug, error, info, instrument};

/// Tool executor - manages tool execution lifecycle
pub struct ToolExecutor {
    registry: ToolRegistry,
    permissions: PermissionManager,
}

impl ToolExecutor {
    /// Create new tool executor
    pub fn new(registry: ToolRegistry, permissions: PermissionManager) -> Self {
        Self {
            registry,
            permissions,
        }
    }

    /// Execute a single tool use
    #[instrument(skip(self, tool_use), fields(tool = %tool_use.name, id = %tool_use.id))]
    pub async fn execute_tool(&self, tool_use: &ToolUse) -> Result<ToolResult> {
        info!("Executing tool: {}", tool_use.name);

        // 1. Check if tool exists
        let tool = self
            .registry
            .get(&tool_use.name)
            .context(format!("Tool '{}' not found", tool_use.name))?;

        // 2. Check permissions
        let permission_check = self
            .permissions
            .check_tool_use(&tool_use.name, &tool_use.input);

        match permission_check {
            PermissionCheck::Allow => {
                debug!("Tool execution allowed");
            }
            PermissionCheck::AskUser(reason) => {
                // For now, deny if ask required (will implement user prompts in Phase 2)
                error!("Tool execution requires user confirmation: {}", reason);
                return Ok(ToolResult::error(
                    tool_use.id.clone(),
                    format!("Permission required: {}", reason),
                ));
            }
            PermissionCheck::Deny(reason) => {
                error!("Tool execution denied: {}", reason);
                return Ok(ToolResult::error(tool_use.id.clone(), reason));
            }
        }

        // 3. Execute tool
        match tool.execute(tool_use.input.clone()).await {
            Ok(output) => {
                info!("Tool executed successfully");
                Ok(ToolResult::success(tool_use.id.clone(), output))
            }
            Err(e) => {
                error!("Tool execution failed: {}", e);
                Ok(ToolResult::error(
                    tool_use.id.clone(),
                    format!("Execution error: {}", e),
                ))
            }
        }
    }

    /// Execute multiple tool uses in sequence
    #[instrument(skip(self, tool_uses))]
    pub async fn execute_tool_loop(&self, tool_uses: Vec<ToolUse>) -> Result<Vec<ToolResult>> {
        info!("Executing {} tool(s)", tool_uses.len());

        let mut results = Vec::new();

        for tool_use in tool_uses {
            let result = self.execute_tool(&tool_use).await?;
            results.push(result);
        }

        Ok(results)
    }

    /// Get reference to registry
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Get reference to permissions manager
    pub fn permissions(&self) -> &PermissionManager {
        &self.permissions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::Tool;
    use crate::tools::types::ToolInputSchema;
    use async_trait::async_trait;
    use serde_json::Value;

    // Mock tool for testing
    struct MockTool {
        should_fail: bool,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            "mock"
        }

        fn description(&self) -> &str {
            "A mock tool"
        }

        fn input_schema(&self) -> ToolInputSchema {
            ToolInputSchema::simple(vec![("param", "Test parameter")])
        }

        async fn execute(&self, input: Value) -> Result<String> {
            if self.should_fail {
                anyhow::bail!("Mock failure");
            }
            Ok(format!("Mock result: {}", input))
        }
    }

    fn create_test_executor(allow_tool: bool, tool_should_fail: bool) -> ToolExecutor {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool {
            should_fail: tool_should_fail,
        }));

        let permissions = if allow_tool {
            PermissionManager::new().with_default_rule(
                crate::tools::permissions::PermissionRule::Allow,
            )
        } else {
            PermissionManager::new().with_default_rule(
                crate::tools::permissions::PermissionRule::Deny,
            )
        };

        ToolExecutor::new(registry, permissions)
    }

    #[tokio::test]
    async fn test_execute_tool_success() {
        let executor = create_test_executor(true, false);
        let tool_use = ToolUse::new("mock".to_string(), serde_json::json!({"param": "value"}));

        let result = executor.execute_tool(&tool_use).await.unwrap();

        assert_eq!(result.tool_use_id, tool_use.id);
        assert!(!result.is_error);
        assert!(result.content.contains("Mock result"));
    }

    #[tokio::test]
    async fn test_execute_tool_not_found() {
        let executor = create_test_executor(true, false);
        let tool_use = ToolUse::new(
            "nonexistent".to_string(),
            serde_json::json!({"param": "value"}),
        );

        let result = executor.execute_tool(&tool_use).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_execute_tool_permission_denied() {
        let executor = create_test_executor(false, false);
        let tool_use = ToolUse::new("mock".to_string(), serde_json::json!({"param": "value"}));

        let result = executor.execute_tool(&tool_use).await.unwrap();

        assert_eq!(result.tool_use_id, tool_use.id);
        assert!(result.is_error);
        assert!(result.content.contains("not allowed"));
    }

    #[tokio::test]
    async fn test_execute_tool_execution_failure() {
        let executor = create_test_executor(true, true);
        let tool_use = ToolUse::new("mock".to_string(), serde_json::json!({"param": "value"}));

        let result = executor.execute_tool(&tool_use).await.unwrap();

        assert_eq!(result.tool_use_id, tool_use.id);
        assert!(result.is_error);
        assert!(result.content.contains("Execution error"));
    }

    #[tokio::test]
    async fn test_execute_tool_loop() {
        let executor = create_test_executor(true, false);
        let tool_uses = vec![
            ToolUse::new("mock".to_string(), serde_json::json!({"param": "1"})),
            ToolUse::new("mock".to_string(), serde_json::json!({"param": "2"})),
        ];

        let results = executor.execute_tool_loop(tool_uses).await.unwrap();

        assert_eq!(results.len(), 2);
        assert!(!results[0].is_error);
        assert!(!results[1].is_error);
    }
}

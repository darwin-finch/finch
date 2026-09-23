//! Read-only tool adapter for bounded structural source outlines.

use crate::source_index::SourceResolver;
use crate::tools::types::{ToolContext, ToolInputSchema};
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use finch_programs::ExecutionEffect;
use serde_json::Value;
use std::path::PathBuf;

/// Exposes the source-index facade without exposing parser implementation details.
pub struct CodeOutlineTool {
    workspace_root: PathBuf,
}

impl CodeOutlineTool {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            workspace_root: finch_tools_api::resolve_workspace_root(&workspace_root),
        }
    }
}

#[async_trait]
impl Tool for CodeOutlineTool {
    fn name(&self) -> &str {
        "code_outline"
    }

    fn effect(&self) -> ExecutionEffect {
        ExecutionEffect::WorkspaceRead
    }

    fn description(&self) -> &str {
        "Return a bounded structural outline of one workspace source file, with exact-byte identity, provenance, and source spans. The result omits source bodies."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "path": {
                    "type": "string",
                    "description": "Workspace-relative or contained absolute source-file path"
                }
            }),
            required: vec!["path".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .context("Missing path parameter")?;
        let result = SourceResolver::new(&self.workspace_root)?.outline(path)?;
        serde_json::to_string_pretty(&result).context("failed to serialize code outline")
    }

    fn workspace_root(&self) -> Option<&std::path::Path> {
        Some(&self.workspace_root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{PermissionManager, PermissionRule, ToolExecutor, ToolRegistry, ToolUse};
    use std::fs;

    #[tokio::test]
    async fn test_code_outline_runs_through_real_executor_without_source_body() {
        let workspace = tempfile::tempdir().expect("workspace");
        fs::create_dir(workspace.path().join(".git")).expect("workspace marker");
        fs::write(
            workspace.path().join("sample.rs"),
            "const PRIVATE: &str = \"body must stay private\";\nfn visible() {}\n",
        )
        .expect("source fixture");

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(CodeOutlineTool::new(workspace.path())));
        let permissions = PermissionManager::new()
            .with_default_rule(PermissionRule::Allow)
            .with_workspace_root(workspace.path().to_path_buf());
        let executor = ToolExecutor::new(
            registry,
            permissions,
            workspace.path().join("patterns.json"),
        )
        .expect("executor");
        let result = executor
            .execute_tool(
                &ToolUse::new(
                    "code_outline".to_string(),
                    serde_json::json!({"path": "sample.rs"}),
                ),
                None::<fn() -> anyhow::Result<()>>,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("tool result");

        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("visible"), "{}", result.content);
        assert!(!result.content.contains("body must stay private"));
        assert!(result.content.contains("content_sha256"));
    }

    #[test]
    fn test_executor_rejects_tool_permission_workspace_mismatch() {
        let authorized = tempfile::tempdir().expect("authorized workspace");
        let execution = tempfile::tempdir().expect("execution workspace");
        fs::create_dir(authorized.path().join(".git")).expect("authorized marker");
        fs::create_dir(execution.path().join(".git")).expect("execution marker");
        fs::write(
            execution.path().join("secret.rs"),
            "fn execution_secret() {}\n",
        )
        .expect("execution source");

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(CodeOutlineTool::new(execution.path())));
        let permissions = PermissionManager::new()
            .with_default_rule(PermissionRule::Allow)
            .with_workspace_root(authorized.path().to_path_buf());
        let error = ToolExecutor::new(
            registry,
            permissions,
            authorized.path().join("patterns.json"),
        )
        .err()
        .expect("mismatched roots must fail before execution");

        assert!(error.to_string().contains("differs from permission root"));
        assert!(!error.to_string().contains("execution_secret"));
    }
}

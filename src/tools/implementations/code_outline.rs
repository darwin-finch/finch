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
        "Return a bounded structural outline of one workspace source file, with exact-byte identity, provenance, and source spans. The result omits source bodies. With base_ref set, instead return a compact symbol-level delta (added/removed/signature_changed/body_changed) between the working-tree file and its contents at that Git revision."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "path": {
                    "type": "string",
                    "description": "Workspace-relative or contained absolute source-file path"
                },
                "base_ref": {
                    "type": "string",
                    "description": "Optional Git revision (e.g. \"HEAD\", \"main\", a commit SHA) to diff the file against. When set, the tool returns a structural delta instead of a full outline."
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
        let base_ref = input.get("base_ref").and_then(Value::as_str);
        let resolver = SourceResolver::new(&self.workspace_root)?;
        match base_ref {
            Some(base_ref) => {
                let result = resolver.outline_diff(path, base_ref)?;
                serde_json::to_string_pretty(&result)
                    .context("failed to serialize code outline diff")
            }
            None => {
                let result = resolver.outline(path)?;
                serde_json::to_string_pretty(&result).context("failed to serialize code outline")
            }
        }
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
    use std::path::Path;
    use std::process::Command;

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git fixture command failed: {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn executor_for(workspace: &Path) -> ToolExecutor {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(CodeOutlineTool::new(workspace)));
        let permissions = PermissionManager::new()
            .with_default_rule(PermissionRule::Allow)
            .with_workspace_root(workspace.to_path_buf());
        ToolExecutor::new(registry, permissions, workspace.join("patterns.json")).expect("executor")
    }

    #[tokio::test]
    async fn test_code_outline_diff_mode_reports_structural_delta_through_executor() {
        let workspace = tempfile::tempdir().expect("workspace");
        run_git(workspace.path(), &["init", "-q"]);
        let sample = workspace.path().join("sample.rs");
        fs::write(
            &sample,
            "fn removed() {}\nfn kept() {}\nfn body_only() { let a = 1; }\nfn signature_old(a: usize) {}\n",
        )
        .expect("base source");
        run_git(workspace.path(), &["add", "sample.rs"]);
        run_git(
            workspace.path(),
            &[
                "-c",
                "user.name=Finch Test",
                "-c",
                "user.email=finch-test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "base",
            ],
        );
        fs::write(
            &sample,
            "fn kept() {}\nfn body_only() { let a = 2; }\nfn signature_old(a: u64) {}\nfn added() {}\n",
        )
        .expect("current source");

        let executor = executor_for(workspace.path());
        let result = executor
            .execute_tool(
                &ToolUse::new(
                    "code_outline".to_string(),
                    serde_json::json!({"path": "sample.rs", "base_ref": "HEAD"}),
                ),
                None::<fn() -> anyhow::Result<()>>,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("tool result");

        assert!(
            !result.is_error,
            "diff mode must succeed through the executor: {}",
            result.content
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&result.content).expect("diff mode must return JSON");
        let changes = parsed
            .get("changes")
            .and_then(serde_json::Value::as_array)
            .expect("diff mode must return a changes list");
        let change_of = |name: &str| {
            changes
                .iter()
                .find(|change| change.get("name").and_then(serde_json::Value::as_str) == Some(name))
                .and_then(|change| {
                    change
                        .get("change")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
        };
        assert_eq!(
            change_of("added").as_deref(),
            Some("added"),
            "added symbol must be reported as added; diff: {}",
            result.content
        );
        assert_eq!(
            change_of("removed").as_deref(),
            Some("removed"),
            "removed symbol must be reported as removed; diff: {}",
            result.content
        );
        assert_eq!(
            change_of("body_only").as_deref(),
            Some("body_changed"),
            "body-only edit must not be reported as a signature change; diff: {}",
            result.content
        );
        assert_eq!(
            change_of("signature_old").as_deref(),
            Some("signature_changed"),
            "signature edit must be distinguished from a body-only edit; diff: {}",
            result.content
        );
        assert!(
            change_of("kept").is_none(),
            "unchanged symbol must not appear in the delta; diff: {}",
            result.content
        );
        assert!(
            !result.content.contains("let a = 2"),
            "diff mode must not copy source bodies into its result: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn test_code_outline_diff_mode_names_unsupported_language_in_error() {
        let workspace = tempfile::tempdir().expect("workspace");
        run_git(workspace.path(), &["init", "-q"]);
        let notes = workspace.path().join("notes.txt");
        fs::write(&notes, "first draft\n").expect("base source");
        run_git(workspace.path(), &["add", "notes.txt"]);
        run_git(
            workspace.path(),
            &[
                "-c",
                "user.name=Finch Test",
                "-c",
                "user.email=finch-test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "base",
            ],
        );
        fs::write(&notes, "second draft\n").expect("current source");

        let executor = executor_for(workspace.path());
        let result = executor
            .execute_tool(
                &ToolUse::new(
                    "code_outline".to_string(),
                    serde_json::json!({"path": "notes.txt", "base_ref": "HEAD"}),
                ),
                None::<fn() -> anyhow::Result<()>>,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("tool result");

        assert!(
            result.is_error,
            "unsupported-language diff must fail, not return windows: {}",
            result.content
        );
        assert!(
            result.content.contains("txt"),
            "error must name the unsupported language: {}",
            result.content
        );
    }

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

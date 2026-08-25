// Restart tool - prepares a frontend replacement after its Brain turn commits.

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;

const DEFERRED_RESTART_KEY: &str = "finch_deferred_frontend_restart_v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredFrontendRestart {
    pub reason: String,
    pub binary_path: PathBuf,
    pub binary_sha256: String,
}

impl DeferredFrontendRestart {
    pub(crate) fn verify(&self) -> Result<()> {
        let current = hash_file(&self.binary_path)?;
        anyhow::ensure!(
            current == self.binary_sha256,
            "restart candidate '{}' changed after approval (expected {}, found {})",
            self.binary_path.display(),
            self.binary_sha256,
            current
        );
        Ok(())
    }

    /// Prove that the approved artifact can at least be loaded and execute its
    /// argument parser before the current frontend gives up its runner lease.
    pub(crate) fn preflight(&self) -> Result<()> {
        self.verify()?;
        let output = std::process::Command::new(&self.binary_path)
            .arg("--version")
            .output()
            .with_context(|| {
                format!(
                    "could not start restart candidate '{}'",
                    self.binary_path.display()
                )
            })?;
        anyhow::ensure!(
            output.status.success(),
            "restart candidate '{}' failed its --version health check: {}",
            self.binary_path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }
}

/// A Brain restart deliberately does not carry the legacy conversation-file,
/// resume, prompt, or one-shot execution flags into the replacement process.
/// The canonical Brain log/checkpoint is the only restoration source. Preserve
/// only the user's terminal presentation choice.
pub(crate) fn frontend_replacement_args<I>(current: I, brain: &str) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = Vec::new();
    for argument in current.into_iter().skip(1) {
        if argument == "--raw" || argument == "--no-tui" {
            args.push(argument);
        }
    }
    args.push(OsString::from("--brain"));
    args.push(OsString::from(brain));
    args
}

pub struct RestartTool;

impl Default for RestartTool {
    fn default() -> Self {
        Self
    }
}

pub(crate) fn deferred_frontend_restart_from_tool_result(
    result: &std::result::Result<String, anyhow::Error>,
) -> Option<DeferredFrontendRestart> {
    let value: serde_json::Value = serde_json::from_str(result.as_ref().ok()?).ok()?;
    serde_json::from_value(value.get(DEFERRED_RESTART_KEY)?.clone()).ok()
}

fn hash_file(path: &std::path::Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[async_trait]
impl Tool for RestartTool {
    fn name(&self) -> &str {
        "restart_session"
    }

    fn description(&self) -> &str {
        "Prepare a verified Finch frontend binary for restart after the current canonical Brain run is durably committed.
        Use this only after building and testing the replacement binary. The tool records its exact path and SHA-256;
        Finch performs the replacement later, after the daemon acknowledges the completed Brain turn.

        IMPORTANT: this does not restart the daemon or bypass the Brain lifecycle."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![
            (
                "reason",
                "Why you're restarting (e.g., 'optimized router', 'added new tool')",
            ),
            (
                "binary_path",
                "Path to new binary (default: ./target/release/finch)",
            ),
        ])
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let reason = input["reason"]
            .as_str()
            .context("Missing reason parameter")?;

        let binary_path = input["binary_path"]
            .as_str()
            .unwrap_or("./target/release/finch");

        let binary_path = std::fs::canonicalize(binary_path).with_context(|| {
            format!("Binary not found at '{}'. Did you forget to build it?", binary_path)
        })?;
        let metadata = std::fs::metadata(&binary_path)?;
        if !metadata.is_file() {
            anyhow::bail!(
                "Restart candidate '{}' is not a regular file",
                binary_path.display()
            );
        }

        // Check if binary is executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = metadata.permissions();
            if permissions.mode() & 0o111 == 0 {
                anyhow::bail!("Binary at '{}' is not executable", binary_path.display());
            }
        }
        let intent = DeferredFrontendRestart {
            reason: reason.to_string(),
            binary_sha256: hash_file(&binary_path)?,
            binary_path,
        };
        Ok(serde_json::to_string(&serde_json::json!({
            DEFERRED_RESTART_KEY: intent,
        }))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_args_use_brain_as_the_only_state_source() {
        let args = frontend_replacement_args(
            [
                "finch",
                "--restore-session",
                "old.json",
                "--initial-prompt",
                "repeat me",
                "--raw",
                "--brain",
                "old-brain",
                "--lisp",
                "(+ 1 2)",
            ]
            .into_iter()
            .map(OsString::from),
            "durable-brain",
        );
        assert_eq!(
            args,
            ["--raw", "--brain", "durable-brain"]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn test_restart_requires_reason() {
        let tool = RestartTool::default();
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
            poset: None,
        };
        let input = serde_json::json!({
            "binary_path": "./target/release/finch"
        });

        let result = tool.execute(input, &context).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("reason"));
    }

    #[tokio::test]
    async fn test_restart_validates_binary_exists() {
        let tool = RestartTool::default();
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
            poset: None,
        };
        let input = serde_json::json!({
            "reason": "test",
            "binary_path": "/nonexistent/binary"
        });

        let result = tool.execute(input, &context).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn restart_tool_returns_a_hashed_deferred_intent() {
        let tool = RestartTool;
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
            poset: None,
        };
        let binary = std::env::current_exe().unwrap();
        let result = tool
            .execute(
                serde_json::json!({
                    "reason": "exercise the durable Brain restart path",
                    "binary_path": binary,
                }),
                &context,
            )
            .await;
        let intent = deferred_frontend_restart_from_tool_result(&result).unwrap();

        assert_eq!(intent.reason, "exercise the durable Brain restart path");
        assert_eq!(intent.binary_path, std::fs::canonicalize(binary).unwrap());
        assert_eq!(intent.binary_sha256.len(), 64);
        intent.verify().unwrap();
    }

    #[test]
    fn changed_restart_artifact_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("finch-candidate");
        std::fs::write(&binary, b"first").unwrap();
        let intent = DeferredFrontendRestart {
            reason: "test".into(),
            binary_path: binary.clone(),
            binary_sha256: hash_file(&binary).unwrap(),
        };
        std::fs::write(&binary, b"second").unwrap();

        assert!(intent
            .verify()
            .unwrap_err()
            .to_string()
            .contains("changed after approval"));
    }

    #[cfg(unix)]
    #[test]
    fn preflight_executes_the_approved_candidate_before_lease_release() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("finch-candidate");
        std::fs::write(&binary, b"#!/bin/sh\nprintf 'finch test\\n'\n").unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();
        let intent = DeferredFrontendRestart {
            reason: "test".into(),
            binary_path: binary.clone(),
            binary_sha256: hash_file(&binary).unwrap(),
        };

        intent.preflight().unwrap();
    }
}

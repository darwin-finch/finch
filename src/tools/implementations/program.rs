//! Provider-facing adapters for the shared Forth/Lisp runtime.

use crate::programs::{ExecutionEffect, ProgramLanguage};
use crate::runtime::{ProgramRuntime, ProgramSubmission};
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Arc;

/// Retrieve the exact versioned source-language contract instead of relying on
/// provider training data or a remembered vocabulary.
pub struct GetLanguageDefinitionTool;

#[async_trait]
impl Tool for GetLanguageDefinitionTool {
    fn name(&self) -> &str {
        "get_language_definition"
    }

    fn description(&self) -> &str {
        "Return Finch's exact shared VM, typed Lisp, typed Co-Forth, or machine-readable program-envelope definition. Use this before writing unfamiliar VM programs."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "language": {
                    "type": "string",
                    "enum": ["shared", "lisp", "forth", "schema"],
                    "description": "Definition to retrieve"
                }
            }),
            required: vec!["language".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let language = input["language"]
            .as_str()
            .context("get_language_definition: missing language")?;
        Ok(match language {
            "shared" => crate::programs::VM_LANGUAGE_DEFINITION,
            "lisp" => crate::programs::LISP_LANGUAGE_DEFINITION,
            "forth" => crate::programs::FORTH_LANGUAGE_DEFINITION,
            "schema" => crate::programs::LANGUAGE_SCHEMA,
            _ => anyhow::bail!("unknown Finch language definition: {language}"),
        }
        .to_string())
    }
}

pub struct SubmitProgramTool {
    runtime: Arc<ProgramRuntime>,
    caller: Option<crate::runtime::scheduler::AgentIdentity>,
}

impl SubmitProgramTool {
    pub fn new(runtime: Arc<ProgramRuntime>) -> Self {
        Self {
            runtime,
            caller: None,
        }
    }

    pub fn child(
        runtime: Arc<ProgramRuntime>,
        caller: crate::runtime::scheduler::AgentIdentity,
    ) -> Self {
        Self {
            runtime,
            caller: Some(caller),
        }
    }
}

#[async_trait]
impl Tool for SubmitProgramTool {
    fn name(&self) -> &str {
        "submit_program"
    }

    fn description(&self) -> &str {
        "Execute Forth or Lisp directly in Finch's persistent session VM. Returns structured values, output, diagnostics, and VM revisions without using the shell or conversational stack. The initial runtime accepts pure, VM-read, and VM-write effects."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "language": {
                    "type": "string",
                    "enum": ["forth", "lisp"],
                    "description": "Source language"
                },
                "source": {
                    "type": "string",
                    "description": "Exact Forth or Lisp source to execute"
                },
                "intent": {
                    "type": "string",
                    "description": "Short description used for audit and UI presentation"
                },
                "effect": {
                    "type": "string",
                    "enum": ["pure", "vm_read", "vm_write", "external_read", "external_write"],
                    "description": "Expected effect of this execution"
                },
                "declared_capabilities": {
                    "type": "array",
                    "items": { "type": "object" },
                    "description": "Optional exact typed capability requirements inferred while composing the program"
                },
                "manifest_generation": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "VM manifest generation used to compose the source"
                },
                "expected_revision": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional VM revision observed while composing positional stack operations"
                }
            }),
            required: vec![
                "source".to_string(),
                "intent".to_string(),
                "effect".to_string(),
                "manifest_generation".to_string(),
            ],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let source = input["source"]
            .as_str()
            .context("submit_program: missing source")?
            .to_string();
        let language = input
            .get("language")
            .and_then(Value::as_str)
            .map(str::parse)
            .transpose()?
            .unwrap_or_else(|| ProgramLanguage::infer_source(&source));
        let intent = input["intent"]
            .as_str()
            .context("submit_program: missing intent")?
            .to_string();
        let effect = ExecutionEffect::from_str(
            input["effect"]
                .as_str()
                .context("submit_program: missing effect")?,
        )?;
        let manifest_generation = input["manifest_generation"]
            .as_u64()
            .context("submit_program: missing manifest_generation")?;
        let expected_revision = input.get("expected_revision").and_then(Value::as_u64);
        let declared_capabilities = input
            .get("declared_capabilities")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();

        let outcome = self
            .runtime
            .submit_as(
                ProgramSubmission {
                    language,
                    source,
                    intent,
                    effect,
                    declared_capabilities,
                    manifest_generation,
                    expected_revision,
                    budget: None,
                },
                self.caller.clone(),
            )
            .await?;
        Ok(serde_json::to_string(&outcome)?)
    }
}

/// Compact state used to recover from a stale manifest or inspect revisions.
pub struct GetVmStateTool {
    runtime: Arc<ProgramRuntime>,
}

impl GetVmStateTool {
    pub fn new(runtime: Arc<ProgramRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for GetVmStateTool {
    fn name(&self) -> &str {
        "get_vm_state"
    }

    fn description(&self) -> &str {
        "Return the current Finch VM manifest generation and state revision."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({}),
            required: Vec::new(),
        }
    }

    async fn execute(&self, _input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let state = self.runtime.inspect().await?;
        Ok(json!({
            "manifest_generation": state.manifest_generation,
            "revision": state.revision,
            "stack": state.stack,
            "stack_top": state.stack.last(),
            "vocabulary": state.vocabulary,
            "languages": ["forth", "lisp"],
            "effects": ["pure", "vm_read", "vm_write", "external_read", "external_write"],
            "automation": self.runtime.automation().availability()
        })
        .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn language_definition_advertises_program_response_contract() {
        let tool = GetLanguageDefinitionTool;
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
            stack: None,
            poset: None,
        };
        let definition = tool
            .execute(json!({"language": "lisp"}), &context)
            .await
            .unwrap();
        assert!(definition.contains("(say \"Hello\")"));
        assert!(definition.contains("compiles directly"));
    }

    #[tokio::test]
    async fn tool_round_trips_structured_forth_result() {
        let runtime = Arc::new(ProgramRuntime::new());
        let tool = SubmitProgramTool::new(runtime);
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
            stack: None,
            poset: None,
        };
        let result = tool
            .execute(
                json!({
                    "language": "forth",
                    "source": "20 22 +",
                    "intent": "add",
                    "effect": "pure",
                    "manifest_generation": 1
                }),
                &context,
            )
            .await
            .unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["values"][0]["value"], 42);
    }
}

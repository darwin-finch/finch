//! Provider-facing adapters for the shared Forth/Lisp runtime.

use crate::programs::{ExecutionEffect, ProgramLanguage};
use crate::runtime::{ProgramRuntime, ProgramSubmission, TypedEffectSink};
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

/// Search the runtime's built-in typed vocabulary. This differs from the
/// persisted program registry: core words such as `say`, `path`, and
/// `file-read` exist before any user/project definition is promoted.
pub struct SearchVmVocabularyTool {
    runtime: Arc<ProgramRuntime>,
}

impl SearchVmVocabularyTool {
    pub fn new(runtime: Arc<ProgramRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for SearchVmVocabularyTool {
    fn name(&self) -> &str {
        "search_vm_vocabulary"
    }

    fn description(&self) -> &str {
        "Search Finch's built-in typed VM words and return exact stack signatures. Use this for VM discovery; search_vocabulary only searches persisted user/project definitions."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "query": {"type": "string", "description": "Case-insensitive word-name fragment"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Maximum matches (default 25)"}
            }),
            required: vec!["query".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let query = input["query"]
            .as_str()
            .context("search_vm_vocabulary: missing query")?
            .trim()
            .to_ascii_lowercase();
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(25)
            .clamp(1, 100) as usize;
        let state = self.runtime.inspect().await?;
        let matches = state
            .typed_vocabulary
            .into_iter()
            .filter(|entry| entry.name.to_ascii_lowercase().contains(&query))
            .take(limit)
            .map(|entry| json!({"name": entry.name, "signature": entry.signature}))
            .collect::<Vec<_>>();
        Ok(json!({
            "query": query,
            "matches": matches,
            "truncated": matches.len() == limit,
            "manifest_generation": state.manifest_generation,
        })
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
        "Execute Forth or Lisp directly in Finch's persistent session VM. Returns structured values, portable output events, diagnostics, and VM revisions without using the shell or conversational stack. Effects and concrete capabilities are verified by the typed runtime."
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
                    "enum": ["pure", "vm_read", "vm_write", "workspace_read", "workspace_write", "external_read", "external_write", "destructive", "unclassified"],
                    "description": "Declared upper bound; typed capability inference remains authoritative"
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

    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String> {
        let source = input["source"]
            .as_str()
            .context("submit_program: missing source")?
            .to_string();
        let language = input
            .get("language")
            .and_then(Value::as_str)
            .map(str::parse)
            .transpose()?
            .map(Ok)
            .unwrap_or_else(|| ProgramLanguage::infer_wire_source(&source))?;
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

        let submission = ProgramSubmission {
            language,
            source,
            intent,
            effect,
            declared_capabilities,
            manifest_generation,
            expected_revision,
            budget: None,
        };
        let defer_program_effects = context
            .live_output
            .as_ref()
            .is_some_and(|output| output.defer_program_effects());
        let outcome = if let Some(live_output) = context.live_output.clone() {
            // The coordinator binds this callback to the particular WorkUnit
            // which owns this tool use. It is deliberately constructed per
            // submission, never installed as a mutable global runtime sink.
            let effect_sink: TypedEffectSink = Arc::new(move |envelope| {
                live_output.vm_effect_envelope(envelope);
            });
            if defer_program_effects && self.caller.is_none() {
                self.runtime
                    .submit_with_deferred_program_effects(submission, effect_sink)
                    .await?
            } else {
                self.runtime
                    .submit_as_typed_only_with_typed_effect_sink(
                        submission,
                        self.caller.clone(),
                        effect_sink,
                    )
                    .await?
            }
        } else {
            self.runtime
                .submit_as_typed_only(submission, self.caller.clone())
                .await?
        };
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
            "typed_vocabulary_count": state.typed_vocabulary.len(),
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
    use std::sync::Mutex;

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

        let shared = tool
            .execute(json!({"language": "shared"}), &context)
            .await
            .unwrap();
        assert!(shared.contains("otherwise treats the source as Forth"));
        assert!(shared.contains("s\"Your response to the human\" say"));
    }

    #[tokio::test]
    async fn built_in_vm_vocabulary_is_searchable_without_source_tree_access() {
        let tool = SearchVmVocabularyTool::new(Arc::new(ProgramRuntime::new()));
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
        let result: Value = serde_json::from_str(
            &tool.execute(json!({"query": "say"}), &context)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "say"));
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

    #[tokio::test]
    async fn provider_tool_uses_the_compact_wire_language_discriminator() {
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

        let forth: Value = serde_json::from_str(
            &tool
                .execute(
                    json!({
                        "source": "20 22 +",
                        "intent": "add",
                        "effect": "pure",
                        "manifest_generation": 1
                    }),
                    &context,
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(forth["status"], "completed");
        assert_eq!(forth["values"][0]["value"], 42);

        let lisp: Value = serde_json::from_str(
            &tool
                .execute(
                    json!({
                        "source": "  (+ 3 4)",
                        "intent": "add with Lisp",
                        "effect": "pure",
                        "manifest_generation": 1
                    }),
                    &context,
                )
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(lisp["status"], "completed");
        assert_eq!(
            lisp["values"].as_array().unwrap().last().unwrap()["value"],
            7
        );
    }

    #[tokio::test]
    async fn provider_submission_never_falls_back_to_legacy_forth() {
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

        let outcome = tool
            .execute(
                json!({
                    "language": "forth",
                    // This classic definition is accepted by the legacy
                    // interpreter but lacks the typed signature required by
                    // the shared provider runtime.
                    "source": ": legacy-double 2 * ;",
                    "intent": "define a word",
                    "effect": "vm_write",
                    "manifest_generation": 1
                }),
                &context,
            )
            .await
            .unwrap();
        let outcome: Value = serde_json::from_str(&outcome).unwrap();
        assert_eq!(outcome["status"], "failed");
        assert!(outcome["vm_diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "E-FORTH-SIG-001"));
    }

    #[tokio::test]
    async fn typed_say_uses_the_callers_per_run_output_binding() {
        let runtime = Arc::new(ProgramRuntime::new());
        let tool = SubmitProgramTool::new(runtime);
        let emitted = Arc::new(Mutex::new(Vec::new()));
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: Some({
                let emitted = Arc::clone(&emitted);
                Arc::new(move |text| emitted.lock().unwrap().push(text))
            }),
            stack: None,
            poset: None,
        };

        tool.execute(
            json!({
                "language": "lisp",
                "source": "(begin (say \"first\") (say \" second\"))",
                "intent": "stream a response",
                "effect": "pure",
                "manifest_generation": 1
            }),
            &context,
        )
        .await
        .unwrap();

        assert_eq!(
            &*emitted.lock().unwrap(),
            &vec!["first".to_string(), " second".to_string()]
        );
    }
}

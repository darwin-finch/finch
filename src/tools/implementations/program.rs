//! Provider-facing adapters for the shared Forth/Lisp runtime.

use crate::memory::MemorySystem;
use crate::programs::{ExecutionEffect, ProgramLanguage, ProgramRef};
use crate::runtime::{ProgramRuntime, ProgramSubmission, TypedEffectSink};
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use crate::vm::vocabulary::core_word_documentation as vm_core_word_documentation;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

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

/// Frontend syntax is deliberately not represented as a callable vocabulary
/// word.  Returning it alongside word matches prevents a model from treating
/// a failed lookup for `if`, `define`, or `while` as evidence that the
/// construct is unavailable, while preserving the distinction between source
/// syntax and a runtime function with a stack signature.
struct SourceSyntaxEntry {
    name: &'static str,
    languages: &'static [&'static str],
    description: &'static str,
}

fn source_syntax_contract(entry: &SourceSyntaxEntry) -> Value {
    json!({
        "kind": "syntax",
        "name": entry.name,
        "languages": entry.languages,
        "description": entry.description,
        "source": null,
        "source_note": "This is verified source syntax, not a callable runtime word and therefore has no independent stack signature. Retrieve get_language_definition for its complete grammar and worked examples.",
    })
}

const SOURCE_SYNTAX: &[SourceSyntaxEntry] = &[
    SourceSyntaxEntry {
        name: "if",
        languages: &["lisp", "forth"],
        description: "Typed conditional. Lisp: (if condition then else); Co-Forth: condition if ... else ... then.",
    },
    SourceSyntaxEntry {
        name: "match",
        languages: &["lisp"],
        description: "Type-directed exhaustive option/result match: some/none or ok/err arms.",
    },
    SourceSyntaxEntry {
        name: "match-option",
        languages: &["lisp"],
        description: "Exhaustive option branch with a bound some payload and a none arm.",
    },
    SourceSyntaxEntry {
        name: "match-result",
        languages: &["lisp"],
        description: "Exhaustive result branch with bound ok and err payloads.",
    },
    SourceSyntaxEntry {
        name: "if-some",
        languages: &["forth"],
        description: "Exhaustive Co-Forth option branch: option if-some ... else ... then.",
    },
    SourceSyntaxEntry {
        name: "if-ok",
        languages: &["forth"],
        description: "Exhaustive Co-Forth result branch: result if-ok ... else ... then.",
    },
    SourceSyntaxEntry {
        name: "case",
        languages: &["forth"],
        description: "Typed integer selector: value case literal of ... endof ... otherwise ... endcase. Arms do not fall through.",
    },
    SourceSyntaxEntry {
        name: "begin",
        languages: &["lisp", "forth"],
        description: "Sequencing form. Lisp evaluates expressions left-to-right; Co-Forth begins a loop with while/repeat.",
    },
    SourceSyntaxEntry {
        name: "while",
        languages: &["lisp", "forth"],
        description: "Metered typed loop. Its body must preserve the declared loop stack row.",
    },
    SourceSyntaxEntry {
        name: "break",
        languages: &["lisp", "forth"],
        description: "Named structured loop exit; it must preserve the target loop stack row.",
    },
    SourceSyntaxEntry {
        name: "continue",
        languages: &["lisp", "forth"],
        description: "Named structured loop continuation; it must preserve the target loop stack row.",
    },
    SourceSyntaxEntry {
        name: "define",
        languages: &["lisp"],
        description: "Persistent typed function definition. Recursive functions require an explicit return type.",
    },
    SourceSyntaxEntry {
        name: "lambda",
        languages: &["lisp"],
        description: "Typed lexical closure expression; parameters use (name : type).",
    },
    SourceSyntaxEntry {
        name: "let",
        languages: &["lisp"],
        description: "Lexical immutable bindings: (let ((name value) ...) body...).",
    },
    SourceSyntaxEntry {
        name: ":",
        languages: &["forth"],
        description: "Persistent typed word definition: : name ( S inputs -- S outputs ! effects ) body ;.",
    },
    SourceSyntaxEntry {
        name: "locals|",
        languages: &["forth"],
        description: "First form of a typed word definition; names all declared inputs in bottom-to-top order.",
    },
    SourceSyntaxEntry {
        name: "s\"",
        languages: &["forth"],
        description: "Typed Co-Forth string literal. s\"text\" pushes string; it does not emit output until passed to say or another word.",
    },
    SourceSyntaxEntry {
        name: "s\"\"\"",
        languages: &["forth"],
        description: "Verbatim Co-Forth string literal for prose or multiline text; it ends at the next triple quote.",
    },
    SourceSyntaxEntry {
        name: "[']",
        languages: &["forth"],
        description: "Quote a persistent typed word as a closure; invoke it with execute.",
    },
    SourceSyntaxEntry {
        name: ".\"",
        languages: &["forth"],
        description: "Standard Co-Forth output literal, lowered to s\"...\" say.",
    },
];

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
        "Compatibility search for Finch's built-in typed VM words and matching Lisp/Co-Forth source syntax. Prefer search_word for canonical cross-scope discovery; search_vocabulary only searches persisted user/project definitions."
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
            .map(|entry| {
                let documentation = vm_core_word_documentation(&entry.name);
                json!({
                    "name": entry.name,
                    "signature": entry.signature,
                    "summary": documentation.summary,
                    "lisp": documentation.lisp,
                    "forth": documentation.forth,
                })
            })
            .collect::<Vec<_>>();
        let syntax_matches = SOURCE_SYNTAX
            .iter()
            .filter(|entry| entry.name.to_ascii_lowercase().contains(&query))
            .take(limit)
            .map(|entry| {
                json!({
                    "name": entry.name,
                    "languages": entry.languages,
                    "description": entry.description,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "query": query,
            "matches": matches,
            "syntax_matches": syntax_matches,
            "truncated": matches.len() == limit || syntax_matches.len() == limit,
            "manifest_generation": state.manifest_generation,
        })
        .to_string())
    }
}

/// Retrieve the complete protocol documentation for one built-in typed word.
///
/// This is intentionally separate from `search_vm_vocabulary`: search is a
/// compact relevance operation, while inspection provides the worked example a
/// provider needs before composing an unfamiliar capability-bearing call.
pub struct InspectVmWordTool {
    runtime: Arc<ProgramRuntime>,
}

impl InspectVmWordTool {
    pub fn new(runtime: Arc<ProgramRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for InspectVmWordTool {
    fn name(&self) -> &str {
        "inspect_vm_word"
    }

    fn description(&self) -> &str {
        "Compatibility inspection for one built-in typed Finch VM word or verified Lisp/Co-Forth source form. Runtime words return signature/capability contracts; syntax forms return their language grammar role. Prefer inspect_word for canonical core, syntax, or persisted-definition inspection; inspect_program remains the legacy persisted-definition alias."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "name": {"type": "string", "description": "Exact built-in typed VM word name"}
            }),
            required: vec!["name".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let name = input["name"]
            .as_str()
            .context("inspect_vm_word: missing name")?;
        let state = self.runtime.inspect().await?;
        let entry = state
            .typed_vocabulary
            .into_iter()
            .find(|entry| entry.name == name);
        if let Some(entry) = entry {
            let documentation = vm_core_word_documentation(&entry.name);
            return Ok(json!({
                "kind": "core",
                "name": entry.name,
                "signature": entry.signature,
                "summary": documentation.summary,
                "lisp": documentation.lisp,
                "forth": documentation.forth,
                "example": documentation.example,
                "source": null,
                "source_note": "Built-in VM words are host bindings, not mutable source definitions. Their signature, capability requirements, and protocol documentation are the inspectable contract.",
                "manifest_generation": state.manifest_generation,
            })
            .to_string());
        }
        if let Some(entry) = SOURCE_SYNTAX.iter().find(|entry| entry.name == name) {
            let mut contract = source_syntax_contract(entry);
            contract["manifest_generation"] = json!(state.manifest_generation);
            return Ok(contract.to_string());
        }
        anyhow::bail!("unknown built-in typed VM word or source syntax '{name}'")
    }
}

/// Canonical vocabulary search across built-in VM words, source syntax, and
/// persisted definitions. The older split tools remain available as compact
/// compatibility views while providers migrate to this single entry point.
pub struct SearchWordTool {
    runtime: Arc<ProgramRuntime>,
    memory: Option<Arc<MemorySystem>>,
}

impl SearchWordTool {
    pub fn new(runtime: Arc<ProgramRuntime>, memory: Option<Arc<MemorySystem>>) -> Self {
        Self { runtime, memory }
    }
}

#[async_trait]
impl Tool for SearchWordTool {
    fn name(&self) -> &str {
        "search_word"
    }

    fn description(&self) -> &str {
        "Search all Finch vocabulary: built-in typed VM words, Lisp/Co-Forth source syntax, and persisted task/session/project/user definitions. Returns compact contracts only; call inspect_word for one exact contract or source version."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "query": {"type": "string", "description": "Case-insensitive word-name, documentation, or source-syntax fragment"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Maximum matches in each result category (default 25)"}
            }),
            required: vec!["query".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let query = input["query"]
            .as_str()
            .context("search_word: missing query")?
            .trim()
            .to_ascii_lowercase();
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(25)
            .clamp(1, 100) as usize;
        let state = self.runtime.inspect().await?;
        let core_matches = state
            .typed_vocabulary
            .into_iter()
            .filter(|entry| entry.name.to_ascii_lowercase().contains(&query))
            .take(limit)
            .map(|entry| {
                let documentation = vm_core_word_documentation(&entry.name);
                json!({
                    "kind": "core",
                    "name": entry.name,
                    "signature": entry.signature,
                    "summary": documentation.summary,
                    "lisp": documentation.lisp,
                    "forth": documentation.forth,
                })
            })
            .collect::<Vec<_>>();
        let syntax_matches = SOURCE_SYNTAX
            .iter()
            .filter(|entry| entry.name.to_ascii_lowercase().contains(&query))
            .take(limit)
            .map(|entry| {
                json!({
                    "kind": "syntax",
                    "name": entry.name,
                    "languages": entry.languages,
                    "description": entry.description,
                })
            })
            .collect::<Vec<_>>();
        let program_matches = match &self.memory {
            Some(memory) => memory
                .search_program_definitions(&query, limit)
                .await?
                .into_iter()
                .map(|definition| {
                    json!({
                        "kind": "program",
                        "id": definition.reference.id,
                        "version": definition.reference.version,
                        "name": definition.name,
                        "language": definition.language,
                        "documentation": definition.documentation,
                        "signature": definition.signature,
                        "effect": definition.effect,
                        "scope": definition.scope,
                        "trust": definition.trust,
                    })
                })
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };
        Ok(json!({
            "query": query,
            "core_matches": core_matches,
            "syntax_matches": syntax_matches,
            "program_matches": program_matches,
            "program_registry_available": self.memory.is_some(),
            "manifest_generation": state.manifest_generation,
        })
        .to_string())
    }
}

/// Canonical inspection for one core word or immutable persisted definition.
pub struct InspectWordTool {
    runtime: Arc<ProgramRuntime>,
    memory: Option<Arc<MemorySystem>>,
}

impl InspectWordTool {
    pub fn new(runtime: Arc<ProgramRuntime>, memory: Option<Arc<MemorySystem>>) -> Self {
        Self { runtime, memory }
    }
}

#[async_trait]
impl Tool for InspectWordTool {
    fn name(&self) -> &str {
        "inspect_word"
    }

    fn description(&self) -> &str {
        "Inspect one Finch core word by exact name, or an immutable persisted definition by id/version. Core words return their typed host contract; persisted definitions additionally return exact source, provenance, dependencies, and tests."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "name": {"type": "string", "description": "Exact core word name, or exact persisted definition name when the registry is available"},
                "id": {"type": "string", "description": "Persisted program UUID returned by search_word"},
                "version": {"type": "integer", "minimum": 0, "description": "Immutable persisted program version returned by search_word"}
            }),
            required: Vec::new(),
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        if let Some(name) = input.get("name").and_then(Value::as_str) {
            let state = self.runtime.inspect().await?;
            if let Some(entry) = state
                .typed_vocabulary
                .into_iter()
                .find(|entry| entry.name == name)
            {
                let documentation = vm_core_word_documentation(&entry.name);
                return Ok(json!({
                    "kind": "core",
                    "name": entry.name,
                    "signature": entry.signature,
                    "summary": documentation.summary,
                    "lisp": documentation.lisp,
                    "forth": documentation.forth,
                    "example": documentation.example,
                    "source": null,
                    "source_note": "Built-in VM words are host bindings, not mutable source definitions. Their signature, capability requirements, and protocol documentation are the inspectable contract.",
                    "manifest_generation": state.manifest_generation,
                })
                .to_string());
            }
            if let Some(entry) = SOURCE_SYNTAX.iter().find(|entry| entry.name == name) {
                let mut contract = source_syntax_contract(entry);
                contract["manifest_generation"] = json!(state.manifest_generation);
                return Ok(contract.to_string());
            }
            let memory = self.memory.as_ref().context(
                "inspect_word: no persisted program registry is available; use an exact built-in word name",
            )?;
            let definitions = memory.search_program_definitions(name, 20).await?;
            let definition = definitions
                .into_iter()
                .find(|definition| definition.name == name)
                .with_context(|| format!("unknown Finch word '{name}'"))?;
            return Ok(persisted_definition_contract(definition).to_string());
        }

        let id = input
            .get("id")
            .and_then(Value::as_str)
            .context("inspect_word: provide name or id/version")?;
        let version = input
            .get("version")
            .and_then(Value::as_u64)
            .context("inspect_word: id requires version")?;
        let memory = self
            .memory
            .as_ref()
            .context("inspect_word: no persisted program registry is available")?;
        let reference = ProgramRef {
            id: Uuid::from_str(id).context("inspect_word: invalid program id")?,
            version,
        };
        let definition = memory
            .get_program_definition(&reference)
            .await?
            .context("inspect_word: program version not found")?;
        Ok(persisted_definition_contract(definition).to_string())
    }
}

fn persisted_definition_contract(definition: crate::programs::ProgramDefinition) -> Value {
    json!({
        "kind": "program",
        "id": definition.reference.id,
        "version": definition.reference.version,
        "name": definition.name,
        "language": definition.language,
        "source": definition.source,
        "documentation": definition.documentation,
        "signature": definition.signature,
        "effect": definition.effect,
        "capabilities": definition.capabilities,
        "dependencies": definition.dependencies,
        "tests": definition.tests,
        "scope": definition.scope,
        "trust": definition.trust,
        "provenance": definition.provenance,
        "source_hash": definition.source_hash,
        "environment_hash": definition.environment_hash,
    })
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
        "Return compact Finch VM manifest, revision, stack, and capability state. Use search_word or inspect_word to discover vocabulary contracts."
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
            "stack_depth": state.stack.len(),
            "vocabulary_count": state.vocabulary.len(),
            "typed_vocabulary_count": state.typed_vocabulary.len(),
            "granted_capabilities": state.granted_capabilities,
            "languages": ["forth", "lisp"],
            "effects": ["pure", "vm_read", "vm_write", "external_read", "external_write"],
            "vocabulary_discovery": "Use search_word(query) followed by inspect_word(name) for targeted contracts.",
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
    async fn vm_state_is_compact_and_points_to_targeted_vocabulary_discovery() {
        let tool = GetVmStateTool::new(Arc::new(ProgramRuntime::new()));
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
            &tool.execute(json!({}), &context).await.unwrap(),
        )
        .unwrap();

        assert!(result.get("vocabulary").is_none());
        assert!(result["vocabulary_count"].as_u64().is_some_and(|count| count > 0));
        assert!(result["typed_vocabulary_count"]
            .as_u64()
            .is_some_and(|count| count > 0));
        assert!(result["vocabulary_discovery"]
            .as_str()
            .is_some_and(|message| message.contains("search_word")));
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
            &tool
                .execute(json!({"query": "say"}), &context)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(result["matches"].as_array().unwrap().iter().any(|entry| {
            entry["name"] == "say"
                && entry["summary"]
                    .as_str()
                    .is_some_and(|summary| summary.contains("exact text chunk"))
                && entry["lisp"] == "(say text)"
        }));
    }

    #[tokio::test]
    async fn inspect_vm_word_returns_contract_not_source_tree_details() {
        let tool = InspectVmWordTool::new(Arc::new(ProgramRuntime::new()));
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
            &tool
                .execute(json!({"name": "file-slice"}), &context)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["name"], "file-slice");
        assert!(result["signature"]
            .as_str()
            .is_some_and(|signature| signature.contains("path<")));
        assert!(result["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("bounded byte range")));
        assert_eq!(result["source"], Value::Null);
        assert!(result["example"]
            .as_str()
            .is_some_and(|example| example.contains("data.csv")));
    }

    #[tokio::test]
    async fn inspect_word_explains_source_syntax_without_claiming_a_word_signature() {
        let runtime = Arc::new(ProgramRuntime::new());
        let legacy = InspectVmWordTool::new(Arc::clone(&runtime));
        let canonical = InspectWordTool::new(runtime, None);
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

        for tool in [&legacy as &dyn Tool, &canonical as &dyn Tool] {
            let result: Value = serde_json::from_str(
                &tool.execute(json!({"name": "while"}), &context)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(result["kind"], "syntax");
            assert_eq!(result["name"], "while");
            assert!(result["languages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|language| language == "lisp"));
            assert!(result.get("signature").is_none());
            assert!(result["source_note"]
                .as_str()
                .is_some_and(|note| note.contains("not a callable runtime word")));
        }
    }

    #[tokio::test]
    async fn canonical_word_tools_inspect_the_same_core_contracts() {
        let runtime = Arc::new(ProgramRuntime::new());
        let search = SearchWordTool::new(Arc::clone(&runtime), None);
        let inspect = InspectWordTool::new(runtime, None);
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
        let found: Value = serde_json::from_str(
            &search
                .execute(json!({"query": "file-slice"}), &context)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(found["core_matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "file-slice"));
        assert!(!found["program_registry_available"].as_bool().unwrap());

        let contract: Value = serde_json::from_str(
            &inspect
                .execute(json!({"name": "file-slice"}), &context)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(contract["kind"], "core");
        assert_eq!(contract["name"], "file-slice");
        assert_eq!(contract["source"], Value::Null);
    }

    #[tokio::test]
    async fn source_syntax_is_discoverable_without_lying_about_callable_words() {
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
            &tool
                .execute(json!({"query": "if"}), &context)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["name"] != "if"));
        assert!(result["syntax_matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "if"));

        let case_result: Value = serde_json::from_str(
            &tool
                .execute(json!({"query": "case"}), &context)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(case_result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["name"] != "case"));
        assert!(case_result["syntax_matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "case"));

        let json_result: Value = serde_json::from_str(
            &tool
                .execute(json!({"query": "json-get"}), &context)
                .await
                .unwrap(),
        )
        .unwrap();
        let json_word = json_result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "json-get")
            .expect("managed JSON field lookup must be discoverable");
        assert!(json_word["summary"]
            .as_str()
            .unwrap()
            .contains("managed JSON"));
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

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

/// Model-facing documentation for an executable core word.  This deliberately
/// lives beside the provider adapter rather than in Rust doc comments: Rust
/// comments are useful to Finch developers, but are not a stable part of the
/// VM protocol a provider can discover at runtime.
#[derive(Debug, Clone, Copy)]
struct CoreWordDocumentation {
    summary: &'static str,
    lisp: &'static str,
    forth: &'static str,
    example: &'static str,
}

fn core_word_documentation(name: &str) -> CoreWordDocumentation {
    // Keep this total.  A model should never infer that a valid core word has
    // no semantics merely because its detailed prose has not been expanded
    // yet; its exact signature remains the normative contract.
    match name {
        "say" => CoreWordDocumentation {
            summary: "Append one exact text chunk to the current response stream. It adds no space or newline and leaves no value on the stack.",
            lisp: "(say text)",
            forth: "text say",
            example: "(say (str-cat \"answer: \" (int-to-string (+ 2 3))))",
        },
        "emit" => CoreWordDocumentation {
            summary: "Alias of say for a terminal response chunk. Prefer say in provider responses.",
            lisp: "(emit text)",
            forth: "text emit",
            example: "s\"progress\\n\" emit",
        },
        "output-open" => CoreWordDocumentation {
            summary: "Create an independent, host-issued output handle for a progress or replaceable status item.",
            lisp: "(output-open title)",
            forth: "title output-open",
            example: "(let ((h (output-open \"Download\"))) (output-status h \"starting\"))",
        },
        "output-append" => CoreWordDocumentation {
            summary: "Append exact text to an explicit output handle; unlike say it does not select a global active work item.",
            lisp: "(output-append handle text)",
            forth: "handle text output-append",
            example: "h s\"chunk complete\\n\" output-append",
        },
        "output-replace" => CoreWordDocumentation {
            summary: "Replace an explicit output handle's displayed body with text.",
            lisp: "(output-replace handle text)",
            forth: "handle text output-replace",
            example: "h s\"42% complete\" output-replace",
        },
        "output-status" => CoreWordDocumentation {
            summary: "Set transient status text on an explicit output handle.",
            lisp: "(output-status handle text)",
            forth: "handle text output-status",
            example: "h s\"running\" output-status",
        },
        "output-progress" => CoreWordDocumentation {
            summary: "Set bounded progress on an explicit output handle as completed and total integer units.",
            lisp: "(output-progress handle completed total)",
            forth: "handle completed total output-progress",
            example: "h 42 100 output-progress",
        },
        "output-complete" => CoreWordDocumentation {
            summary: "Mark an explicit output handle complete.",
            lisp: "(output-complete handle)",
            forth: "handle output-complete",
            example: "h output-complete",
        },
        "output-fail" => CoreWordDocumentation {
            summary: "Mark an explicit output handle failed with a human-readable reason.",
            lisp: "(output-fail handle reason)",
            forth: "handle reason output-fail",
            example: "h s\"network unavailable\" output-fail",
        },
        "path" => CoreWordDocumentation {
            summary: "Resolve text as a normalized workspace-relative path value. It cannot escape the workspace root.",
            lisp: "(path relative-text)",
            forth: "relative-text path",
            example: "(file-read (path \"src/main.rs\"))",
        },
        "host-path" => CoreWordDocumentation {
            summary: "Resolve text under the explicitly installed host-machine root. This identifies a host path but grants no authority by itself.",
            lisp: "(host-path text)",
            forth: "text host-path",
            example: "s\"/tmp/report.txt\" host-path",
        },
        "file-read" => CoreWordDocumentation {
            summary: "Read all bytes from an authorized workspace path. Prefer file-slice or cursor resources for large inputs.",
            lisp: "(file-read path)",
            forth: "path file-read",
            example: "(file-read (path \"Cargo.toml\"))",
        },
        "file-slice" => CoreWordDocumentation {
            summary: "Read a bounded byte range from an authorized workspace path: offset and maximum byte count.",
            lisp: "(file-slice path offset length)",
            forth: "path offset length file-slice",
            example: "(file-slice (path \"data.csv\") 0 4096)",
        },
        "file-size" => CoreWordDocumentation {
            summary: "Return the byte length of an authorized workspace file without reading its contents.",
            lisp: "(file-size path)",
            forth: "path file-size",
            example: "(file-size (path \"data.csv\"))",
        },
        "file-lines-open" => CoreWordDocumentation {
            summary: "Open an authorized text-file line cursor. The opaque cursor owns no forgeable path authority.",
            lisp: "(file-lines-open path)",
            forth: "path file-lines-open",
            example: "(file-lines-open (path \"large.log\"))",
        },
        "file-lines-next" => CoreWordDocumentation {
            summary: "Return some(line) from a line cursor or none at EOF. Close the cursor when finished.",
            lisp: "(file-lines-next cursor)",
            forth: "cursor file-lines-next",
            example: "(match-option (file-lines-next c) (some line (say line)) (none (file-lines-close c)))",
        },
        "file-lines-close" => CoreWordDocumentation {
            summary: "Close a line cursor and release its host resource.",
            lisp: "(file-lines-close cursor)",
            forth: "cursor file-lines-close",
            example: "c file-lines-close",
        },
        "csv-open" | "csv-next" | "csv-close" => CoreWordDocumentation {
            summary: "Open, advance, or close an authorized CSV record cursor. csv-next returns some(list<string>) or none at EOF; use it instead of loading a large CSV at once.",
            lisp: "(csv-open path), (csv-next cursor), (csv-close cursor)",
            forth: "path csv-open; cursor csv-next; cursor csv-close",
            example: "(let ((c (csv-open (path \"data.csv\")))) (csv-next c))",
        },
        "file-write" | "host-file-write" => CoreWordDocumentation {
            summary: "Write bytes to an authorized refined path. This is an external mutation and requires an explicit write capability grant.",
            lisp: "(file-write path bytes)",
            forth: "path bytes file-write",
            example: "(file-write (path \"generated.txt\") (bytes \"hello\\n\"))",
        },
        "host-file-read" => CoreWordDocumentation {
            summary: "Read all bytes from an authorized host-machine path. It requires both an installed host root and a matching read grant.",
            lisp: "(host-file-read path)",
            forth: "path host-file-read",
            example: "(host-file-read (host-path \"/tmp/report.txt\"))",
        },
        "process-run" => CoreWordDocumentation {
            summary: "Run an approved executable directly with a list of string arguments; it never invokes a shell. Use proposal-open for editable scripts.",
            lisp: "(process-run command arguments)",
            forth: "command arguments process-run",
            example: "(process-run \"git\" (list \"status\" \"--short\"))",
        },
        "proposal-open" => CoreWordDocumentation {
            summary: "Ask the host to open a human-editable artifact proposal. Approval may execute the edited artifact through its normal validator, return edited text for chat, or cancel; this word does not run a shell itself.",
            lisp: "(proposal-open language title source)",
            forth: "language title source proposal-open",
            example: "(proposal-open \"python\" \"Report\" \"print('hello')\\n\")",
        },
        "mem-recall" | "mem-store" => CoreWordDocumentation {
            summary: "Read matching session memory entries or store one text memory entry. Both use the host memory tree and require their respective memory capability.",
            lisp: "(mem-recall query), (mem-store text)",
            forth: "query mem-recall; text mem-store",
            example: "(mem-store \"tested release candidate\")",
        },
        "agent-spawn" | "agent-await" | "agent-poll" | "agent-cancel" => CoreWordDocumentation {
            summary: "Create or control a separate typed child-agent task. Agent tasks have their own stack, budget, ancestry, and attenuated grants; they are not fibers or shared-stack threads.",
            lisp: "(agent-spawn task), (agent-poll handle), (agent-await handle), (agent-cancel handle)",
            forth: "task agent-spawn; handle agent-poll; handle agent-await; handle agent-cancel",
            example: "(agent-poll (agent-spawn \"summarize recent test failures\"))",
        },
        "yield" => CoreWordDocumentation {
            summary: "Cooperatively return the remaining VM frames to Finch's event-loop trampoline. It is stack-neutral and may occur repeatedly; it is not a first-class continuation or generator value.",
            lisp: "(yield)",
            forth: "yield",
            example: "(begin (say \"working...\") (yield) (say \"done\"))",
        },
        "some" | "none" | "is-some" | "unwrap" => CoreWordDocumentation {
            summary: "Construct, test, or project typed option values. Prefer exhaustive match-option/if-some over unwrap when none is expected control flow.",
            lisp: "(some value), (none), (is-some option), (unwrap option)",
            forth: "value some; none; option is-some; option unwrap",
            example: "(match-option (some 42) (some n (say (int-to-string n))) (none (say \"missing\")))",
        },
        "ok" | "err" | "is-ok" | "result-unwrap" | "result-error" => CoreWordDocumentation {
            summary: "Construct, test, or project typed result values. Prefer exhaustive match-result/if-ok over projecting an unknown branch.",
            lisp: "(ok value), (err error), (is-ok result), (result-unwrap result)",
            forth: "value ok; error err; result is-ok; result result-unwrap",
            example: "(match-result (ok 42) (ok n (say (int-to-string n))) (err e (say e)))",
        },
        "network-connect" | "network-send" => CoreWordDocumentation {
            summary: "Open an approved network connection or send bytes over an existing opaque socket. The socket is not forgeable and calls remain capability-checked.",
            lisp: "(network-connect host port), (network-send socket bytes)",
            forth: "host port network-connect; socket bytes network-send",
            example: "(network-connect \"example.com\" 443)",
        },
        "schedule-create" => CoreWordDocumentation {
            summary: "Create a capability-bound scheduled event using a callback descriptor and time. Scheduled work never gains new authority when it fires.",
            lisp: "(schedule-create callback when)",
            forth: "callback when schedule-create",
            example: "(schedule-create \"daily-summary\" 1770000000)",
        },
        "vm-vocabulary" => CoreWordDocumentation {
            summary: "Return the serialized current typed vocabulary. Use the external search_vm_vocabulary/describe_vm_word tools for compact targeted discovery.",
            lisp: "(vm-vocabulary)",
            forth: "vm-vocabulary",
            example: "(say (vm-vocabulary))",
        },
        "automation-availability" | "automation-displays" | "automation-windows" | "automation-click" | "automation-type" => CoreWordDocumentation {
            summary: "Inspect or operate desktop automation through the host adapter. Availability and every concrete target remain capability-checked at the execution boundary.",
            lisp: "(automation-availability), (automation-click x y button count), (automation-type text delay)",
            forth: "automation-availability; x y button count automation-click; text delay automation-type",
            example: "(automation-availability)",
        },
        "list-length" | "list-get" => CoreWordDocumentation {
            summary: "Return a typed list's length or one element at a zero-based integer index.",
            lisp: "(list-length items), (list-get items index)",
            forth: "items list-length; items index list-get",
            example: "(list-get (list 4 8 15 16) 2)",
        },
        "str-cat" | "bytes" | "int-to-string" | "atoi" | "space" => CoreWordDocumentation {
            summary: "Pure text/byte conversion helpers. str-cat preserves both inputs exactly; say adds no formatting of its own.",
            lisp: "(str-cat left right), (bytes text), (int-to-string n), (atoi text), (space)",
            forth: "left right str-cat; text bytes; n int-to-string; text atoi; space",
            example: "s\"answer: \" 42 int-to-string str-cat say",
        },
        "dup" | "drop" | "swap" => CoreWordDocumentation {
            summary: "Pure stack shuffles. Prefer Lisp let bindings or Co-Forth locals for complex programs rather than deep positional juggling.",
            lisp: "Usually use let instead of stack shuffles.",
            forth: "value dup; value drop; left right swap",
            example: "3 dup * int-to-string say",
        },
        "+" | "-" | "*" | "/" | "mod" | "negate" | "abs" | "=" | "<" | ">" | "<=" | ">=" | "not" => CoreWordDocumentation {
            summary: "Pure typed arithmetic, comparison, or boolean operation. Operators consume their inputs and push one result.",
            lisp: "(+ a b), (- a b), (* a b), (<= a b), (not flag)",
            forth: "a b +; a b -; a b *; a b <=; flag not",
            example: "(say (int-to-string (+ (* 6 7) 1)))",
        },
        _ => CoreWordDocumentation {
            summary: "Typed Finch core word. Its exact stack signature and capability requirements are the normative contract; retrieve the language definition for shared control-flow rules.",
            lisp: "Use this word in normal prefix Lisp call position.",
            forth: "Use this word in normal postfix Co-Forth position.",
            example: "Use search_vm_vocabulary with this exact name to inspect its signature.",
        },
    }
}

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
        "Search Finch's built-in typed VM words and return exact stack signatures, plus matching Lisp/Co-Forth source syntax. Use this for VM discovery; search_vocabulary only searches persisted user/project definitions."
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
                let documentation = core_word_documentation(&entry.name);
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
        "Inspect one built-in typed Finch VM word. Returns its exact signature, capability requirements, semantics, both source spellings, and a worked example. For persisted user/project words, use inspect_program with the immutable id/version returned by search_vocabulary."
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
            .find(|entry| entry.name == name)
            .with_context(|| format!("unknown built-in typed VM word '{name}'"))?;
        let documentation = core_word_documentation(&entry.name);
        Ok(json!({
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
        assert!(result["matches"].as_array().unwrap().is_empty());
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
        assert!(case_result["matches"].as_array().unwrap().is_empty());
        assert!(case_result["syntax_matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "case"));
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

# local capsule: local-model pattern classification and response generation

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/local/`: `LocalGenerator`, pattern classification, and template response generation
for the optional local-model path. Model loading belongs to `src/models`; provider-independent
generation contracts belong to `finch-generation`; Finch adapters belong to `src/generators`.

**Facade:** `generator`, `patterns`, and `tiered_history` are private children. Callers outside
this directory use `crate::local::{LocalGenerator, GeneratedResponse}`. The template generator,
pattern classifier, and discrete-tier history compaction (`TierAssigner`, #1266) are
implementation details; do not expose child-module paths as a shortcut.

**Dependencies:** model adapters, generation responses, provider messages, tool wire definitions,
training support, and configuration. This module generates candidate responses; it does not own
tool execution, provider transport, model bootstrap, or application policy.

**Tool markup (non-streaming only, #1276):** `try_generate_from_pattern_with_tools` formats
passed `ToolDefinition`s into the prompt via `crate::models::ToolPromptFormatter` and parses any
`<tool_use>` markup the model emits back out via `crate::models::ToolCallParser` -- the same
shared formatter/parser `src/generators/qwen.rs`'s in-process path uses, so both paths speak the
same tool-markup contract. This is markup interpretation, not tool execution: the returned
`tool_uses`/`ContentBlock::ToolUse` values are proposals only. Streaming tool-call detection is
out of scope here; the daemon's SSE path does not yet call this formatting/parsing step.

**Invariants and lifetimes:** `LocalGenerator` is shared behind an application-owned lock; its
generation methods mutate classifier/generator state. A missing or low-confidence local response
is not a successful turn: the caller decides whether to forward. Neural model handles are
injected by the REPL or server after model loading; this module must not start loading or execute
tools. A configured family name is an identity hint, not proof that a model is loaded or supported.
The streaming path calls the injected model's `TextGeneration` port, not a concrete engine type;
keep tokenization, callbacks, and final decode backend-neutral when extending it.

**System-message precedence is explicit.** A caller-provided system message wins. When none is
present, `TemplateGenerator` uses `Persona::default().to_system_message()` as its one canonical
fallback; a synthetic tool-definition block is composed after that persona and must not suppress
it. The local path never reads the retired `~/.finch/constitution.md` file. The recording-backend
regression `local_daemon_boundary_uses_default_persona_without_reading_legacy_constitution_file`
pins this through `LocalGenerator::try_generate_from_pattern_with_tools`.

Local-model history compaction (#1266, design #1260) makes no generator/model call and never
regresses a history entry to a less-compressed tier: `TierAssigner::assign_and_compress`
(`tiered_history.rs`) takes only `(current_question, history, budget_tokens, count_tokens)` --
no generator handle reaches it, so a model call is structurally impossible -- and a previously
assigned tier is always the floor for later calls on the same entry (by content). This is what
keeps most of the local prompt byte-identical turn to turn for llama.cpp's own KV-cache reuse;
see `same_entry_at_same_budget_produces_byte_identical_output_across_calls` and
`tier_never_regresses_when_budget_grows_after_compaction` in `tiered_history.rs`.

**The history budget must be reduced by the real system-prompt cost, not a flat overhead
constant (#1310).** `TemplateGenerator::prompt_parts` (`generator.rs`) resolves `system_prompt`
-- which already carries any tool-definitions block `LocalGenerator::inject_tool_definitions`
prepended -- *before* computing the history token budget, then subtracts that system prompt's
real tokenized cost (`count_tokens(&system_prompt)`) from `context_length()` on top of
`LOCAL_RESPONSE_TOKEN_RESERVE` and the small fixed `LOCAL_PROMPT_OVERHEAD_RESERVE` (chat-template
markers only). Before this fix, only the flat 64-token `LOCAL_PROMPT_OVERHEAD_RESERVE` was
subtracted, so a large tool-definitions block silently consumed history's assumed headroom
instead of shrinking the history budget to compensate -- a brand-new Brain's first, trivial turn
(one line, two recalled memories, the full local tool catalog) measured live at 8152 of an
8192-token context before any real conversation existed. If you add another variable-size
system-prompt contributor, route its cost through this same `system_prompt`-then-budget ordering
rather than adding another flat reserve constant.

**Tool-definitions injection stays under a bounded, measured token cost (#1310).**
`ToolPromptFormatter::format_tools_for_prompt` (`crate::models`) emits one shared XML
`<tool_use>` example (not one per tool) and a single compact `name(param: type, ...): description`
line per tool, replacing a format that repeated a full `**Parameters:**` list and a full XML
`**Example:**` block for every registered tool. Against the real registered-tool catalog (36
tools as of #1310 -- more than a same-line-only grep of `repl.rs` counts, since several
registrations span multiple lines), a real llama.cpp tokenizer (Qwen 2.5 1.5B Instruct) measured
the old format at 5707 tokens for the tool-definitions block alone; the current format measures
2664 tokens for the same catalog, a 53% reduction. `LocalGenerator::inject_tool_definitions`
(`mod.rs`) logs this block's own token cost via `tracing::debug!` (`tool_prompt_tokens`),
separately from the combined-prompt total `LlamaCppGenerator::generate_inner` logs at decode time
(`src/models/loaders/llama_cpp.rs`), since that call site only sees the final token vector and
cannot attribute how many tokens came from this block.
`cli::repl::always_allow_tests::test_tool_definitions_prompt_block_stays_within_a_bounded_word_budget`
bounds this against the real, current tool registry so silent catalog growth or a reversion to
per-tool boilerplate trips a visible failure, and
`cli::repl::always_allow_tests::local_daemon_boundary_first_turn_with_real_tool_catalog_and_recalled_memory_fits_context`
reproduces the reported scenario at the `LocalGenerator::try_generate_from_pattern_with_tools`
production boundary (confirmed to fail against the pre-#1310 format and pass against the current
one).

**A tool-result-only follow-up message gets a named, actionable error, not a generic "No user
message found" (crash-only fix for #1228; the real feature -- local models actually reading tool
output -- stays open).** `TemplateGenerator::prompt_parts` (`generator.rs`) requires its last
"user"-role message to carry a `ContentBlock::Text`. After a local-model tool call executes, the
follow-up round posts the tool result back as `role: "user"` with only a `ContentBlock::ToolResult`
(`Message::with_content("user", vec![ContentBlock::ToolResult { .. }])` in `src/server/handlers.rs`)
-- a real user-role message with no text block. Before this fix, that case fell through to the same
`ok_or_else(|| anyhow!("No user message found"))` used for "there is no user message in the array
at all," which reached the end user as an opaque `{"error":{"message":"No user message found",
"type":"generation_failed"}}` 500 (reported live twice: a fresh-session `find_code` follow-up and a
one-file `read` follow-up). `prompt_parts` now distinguishes the two: a missing user message keeps
the original generic text, while a present-but-textless user message whose content is a
`ContentBlock::ToolResult` returns `LOCAL_TOOL_RESULT_FOLLOWUP_UNSUPPORTED_MESSAGE` ("Local models
don't yet support continuing a conversation after a tool call (#1228); try a cloud provider for
tool-using turns."), so the failure names the real, already-tracked gap instead of reading as a
malformed request. This does not synthesize or guess at an answer to the tool output -- it still
fails the turn, honestly, since local generation genuinely cannot read a tool result here yet.
Covered by `prompt_parts_reports_the_local_tool_result_followup_gap_not_the_generic_no_user_message_text`
(fails before the fix with the generic text, passes after with the specific one) and
`prompt_parts_keeps_the_generic_message_when_there_is_no_user_message_at_all` (`generator.rs`).
`src/generators/qwen.rs`'s `generate_single_turn` has the identical `ok_or_else(|| anyhow!("No user
message found"))?` pattern but is not reachable via this scenario: every real caller (`query_processor.rs`'s
`generator.generate(messages, Some(tool_definitions))` call, "for Qwen or fallback") always passes
the full registered tool set, so a tool-result follow-up routes through `generate_proposing_tools`
instead, which already formats a `ContentBlock::ToolResult` into `"[Tool Result: ...]"` text without
crashing -- left unchanged.

**Extension rule:** keep family-specific request adaptation in `src/generators` and model loading
in `src/models`. Add a flat facade entry only for a demonstrated caller need; do not expose
`generator` or `patterns` as public modules. Keep persistence and learning changes covered by
`local::` tests and the REPL/adapter path that consumes their result.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- local::`. Run
`python3 scripts/check_facade_boundaries.py` whenever the module surface changes, and run the full
supervised workspace suite before extraction.

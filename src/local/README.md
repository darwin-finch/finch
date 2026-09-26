# Optional local generation

`local` coordinates candidate responses from a configured local model and learned response
patterns. It classifies queries and exposes a `LocalGenerator` that callers may share while
turns are processed. It does not decide whether a candidate should replace a provider turn,
load a model, execute proposed tools, or promise that a configured model family works on the
current machine. Those decisions stay with the REPL/server, `src/models`, and
`src/generators` respectively.

Two current callers show the boundary:

1. The [interactive REPL](../cli/repl.rs) constructs a `LocalGenerator` with the model handle
   obtained from its trainer, then asks `try_generate_from_pattern` for a candidate when routing
   permits local generation. `None` or an error leaves the REPL free to forward to its provider;
   after a provider response, the REPL may pass feedback to `learn_from_claude`. The REPL owns
   routing, provider fallback, and conversation state.
2. The [Qwen compatibility adapter](../generators/qwen.rs) receives the application's shared
   generator handle. For a complete text turn it calls `try_generate_from_pattern` from a
   blocking task and wraps the result as a provider-independent `GeneratorResponse`. The adapter
   owns family-specific identity and response metadata; this module only supplies candidate text
   and its configured model name.
3. The daemon's OpenAI-compatible handler
   ([`src/server/openai_handlers.rs`](../server/openai_handlers.rs), `handle_local_only_query` and
   the `local_only` branch of `handle_chat_completions`) calls `try_generate_from_pattern_with_tools`
   directly with the request's tool definitions. When tools are present, this module formats them
   into the prompt and parses any `<tool_use>` markup the model emits back into real `tool_uses`
   (#1276), using the same `ToolPromptFormatter`/`ToolCallParser` the Qwen adapter's own
   tool-proposing path uses -- covering only the non-streaming daemon path; streaming tool calls
   remain future work.

The [agent contract](AGENTS.md) covers dependencies and invariants. [`mod.rs`](mod.rs) is the
flat callable facade; method signatures and return types live in Rust source/rustdoc, not a
generated catalog. A configured model name is not evidence that the local backend is ready.

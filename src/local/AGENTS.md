# local capsule: local-model pattern classification and response generation

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/local/`: `LocalGenerator`, pattern classification, and template response generation
for the optional local-model path. Model loading belongs to `src/models`; provider-independent
generation contracts belong to `finch-generation`; Finch adapters belong to `src/generators`.

**Facade:** `generator` and `patterns` are private children. Callers outside this directory use
`crate::local::{LocalGenerator, GeneratedResponse}`. The template generator and pattern
classifier are implementation details; do not expose child-module paths as a shortcut.

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

**Extension rule:** keep family-specific request adaptation in `src/generators` and model loading
in `src/models`. Add a flat facade entry only for a demonstrated caller need; do not expose
`generator` or `patterns` as public modules. Keep persistence and learning changes covered by
`local::` tests and the REPL/adapter path that consumes their result.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- local::`. Run
`python3 scripts/check_facade_boundaries.py` whenever the module surface changes, and run the full
supervised workspace suite before extraction.

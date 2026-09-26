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

**Invariants and lifetimes:** `LocalGenerator` is shared behind an application-owned lock; its
generation methods mutate classifier/generator state. A missing or low-confidence local response
is not a successful turn: the caller decides whether to forward. Neural model handles are
injected by the REPL or server after model loading; this module must not start loading or execute
tools. A configured family name is an identity hint, not proof that a model is loaded or supported.
The streaming path calls the injected model's `TextGeneration` port, not a concrete engine type;
keep tokenization, callbacks, and final decode backend-neutral when extending it.

Local-model history compaction (#1266, design #1260) makes no generator/model call and never
regresses a history entry to a less-compressed tier: `TierAssigner::assign_and_compress`
(`tiered_history.rs`) takes only `(current_question, history, budget_tokens, count_tokens)` --
no generator handle reaches it, so a model call is structurally impossible -- and a previously
assigned tier is always the floor for later calls on the same entry (by content). This is what
keeps most of the local prompt byte-identical turn to turn for llama.cpp's own KV-cache reuse;
see `same_entry_at_same_budget_produces_byte_identical_output_across_calls` and
`tier_never_regresses_when_budget_grows_after_compaction` in `tiered_history.rs`.

**Extension rule:** keep family-specific request adaptation in `src/generators` and model loading
in `src/models`. Add a flat facade entry only for a demonstrated caller need; do not expose
`generator` or `patterns` as public modules. Keep persistence and learning changes covered by
`local::` tests and the REPL/adapter path that consumes their result.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- local::`. Run
`python3 scripts/check_facade_boundaries.py` whenever the module surface changes, and run the full
supervised workspace suite before extraction.

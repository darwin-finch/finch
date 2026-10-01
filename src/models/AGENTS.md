# models capsule: local model loading, routing, and training configuration

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/models/` (llama.cpp GGUF chat loading, bootstrap, adapters, sampling,
threshold routing, LoRA configuration, and neural embedding load). The adjacent `local`,
`generators`, `training`, `feedback`, `router`, and `logging` trees on the DESIGN.md models
row have their own ownership and are not part of this facade.

**Facade:** child modules are private, so the deliberate `pub use` list in
[`mod.rs`](mod.rs) is the callable public surface; rustdoc supplies method signatures.
Callers outside this directory use `crate::models::Item`; they must not name `bootstrap`,
`gguf_download`, `loaders`, `neural_embedding`, `unified_loader`, `adapters`, or any other child.

**Dependencies:** `config` (`ExecutionTarget`), `memory` (`EmbeddingEngine` for
neural embeddings), and `tools` (prompt/parser types). Production code under `src/models/**`
must not name `crate::cli`. Bootstrap takes a models-owned [`ModelProgress`] port; CLI
implements it and injects it directly through `BootstrapLoader::new`; there is no process-global
model-progress registry. The daemon process downloads and loads the user-selected chat GGUF;
memory owns its own model download. Determinate bytes live in `GeneratorState` and cross the
daemon boundary through `/v1/status`; models must not mutate a terminal status bar.
Do not extract `finch-models` until remaining edges are measured and this reverse edge
stays gone.

**Lifetimes and extension:** `GeneratorState` is shared across bootstrap and server readers;
failed or disabled loading must be represented as state, not a fabricated ready model.
Keep family-specific adapters and engine capability claims here, inject host progress at
composition roots, and add facade exports only for a demonstrated caller need. Do not let
loaders import CLI presentation code.

**Loaders are experimental.** Configuration and loader code are not proof of end-to-end
provider or local-model conformance.

The daemon loads either a Finch-managed, immutable Hugging Face GGUF selected by setup or a
caller-supplied chat-LLM `.gguf` file through `backend.model_path`. Managed artifacts are pinned
by repository, commit, filename, byte size, and SHA-256; partial downloads are resumable and a
file is committed only after verification. This user-configured model selector must not also
select memory's embedding/reranking models.
It owns the process-wide llama.cpp backend and creates a fresh context per generation request.
The generator keeps one opaque llama.cpp sequence snapshot for the preceding prompt and restores it
only when those tokens are an exact strict prefix of the next request; it then evaluates only the
new suffix. Equal, shortened, or divergent prompts are evaluated in full, and replacing the loaded
generator discards the cache.
Memory model selection and persisted embedding identity belong to the separate memory work.
`execution_target = auto` permits GPU offload on macOS when the compiled llama.cpp backend
reports it; `cpu` forbids offload. ONNX and Candle are not chat providers, and ORT is not a
production dependency. The separately owned frontend memory path uses a fixed managed
bge-small-en-v1.5 Q8_0 GGUF through llama.cpp, with a hashed-n-gram fallback while that artifact is
unavailable. The chat-model selector does not configure that embedding model.

**One family declaration, claims from the catalog.** `unified_loader::ModelFamily` is the single
model-family declaration (its variant names are the persisted config wire form; a source-scan test
fails if a second enum reappears). Family capability claims exist only as
`ModelFamily::local_engine_capabilities()` (`FamilyEngineCapabilities`) and must record only
engine-proven paths — no quality or fitness marketing; the former claim-carrying
`ModelFamily::description` and `InferenceProvider::description` are deleted. The compatibility
matrix keeps only its wired job, repository resolution (`get_repository`); its unused family
query functions were deleted rather than wired, and the adapters' duplicate family enum is gone
(adapters are selected by model name via `AdapterRegistry::get_adapter`).

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- models::` plus a caller
smoke (`local::`, `config::backend`). Run the full suite when changing a re-exported `pub`
item. Run `python3 scripts/check_docs.py` and `python3 scripts/check_facade_boundaries.py`
when changing the capsule or facade; do not regenerate a symbol catalog.

**`LlamaCppGenerator`'s prompt `decode()` call never exceeds llama.cpp's `n_batch`.** llama.cpp
does not chunk a `decode()` call internally: `GGML_ASSERT(n_tokens_all <= cparams.n_batch)` in
its `llama-context.cpp` aborts the whole process (`SIGABRT`, a C++ `abort()` no Rust code can
catch, not a panic or `Result::Err`) if a caller ever exceeds it, taking down every session
attached to the daemon. `src/models/loaders/llama_cpp.rs`'s `LlamaCppGenerator::generate_inner`
evaluates a prompt across successive `decode()` calls of at most `n_batch` tokens each
(`plan_prompt_decode_chunks`), requesting logits only on the prompt's true final token
regardless of which chunk it lands in; this composes with the prompt-cache skip-forward, which
only changes where chunking starts. `n_batch`/`n_ubatch` are set explicitly in
`context_params()` (`LOCAL_N_BATCH` = 2048, `LOCAL_N_UBATCH` = 512, matching llama.cpp's own
library defaults and `llama-server`'s pairing) rather than left to the crate default, so the
values have a documented, intentional home. Issue #1292 traced a live production SIGABRT to
this exact assertion: a fresh Brain's first turn (one recalled memory, ordinary conversation,
and the tool-definitions block) produced a 7991-token prompt evaluated in one `decode()` call.
Proof: `test_plan_prompt_decode_chunks_splits_long_prompt_into_n_batch_sized_windows`,
`test_plan_prompt_decode_chunks_composes_with_cache_hit_skip_forward`,
`test_only_the_prompts_true_final_token_requests_logits_across_chunk_boundaries`, and
`test_resolve_batch_sizes_caps_both_at_a_small_context` cover the chunk-boundary and batch-size
arithmetic without a real GGUF; the production-boundary proof against the real native
`decode()` call is `test_real_gguf_chat_decodes_prompt_larger_than_n_batch_without_aborting`
(`#[ignore]`-gated on `FINCH_TEST_GGUF_CHAT`, since only the real llama.cpp binding can
reproduce or disprove a native abort).

**A chunked `decode()` call that keeps failing transiently gets a bounded retry, not an
unbounded one and not a bare surfaced error (#1317).** Post-#1295, a chunked prompt decode
(cold, no cache hit, well under `n_batch` per chunk) intermittently failed with a native
`Decode Error -3: unknown` from `context.decode()`, then succeeded on a bare retry of the
identical query moments later. Investigation ruled out two Finch-side bugs before adding any
mitigation: same-model `decode()` calls are serialized behind a single `RwLock`/`Mutex` chain
from the daemon and CLI request paths down to `GeneratorModel` (no two decode() calls against
one loaded chat model can ever run concurrently), and `llama_backend_init()` is idempotent and
self-guarded at the native level (`ggml_backend_load_all()` only runs
`if (!ggml_backend_reg_count())`), so this capsule's two independent per-module `LlamaBackend`
`OnceCell`s (chat vs. embedding, see `neural_embedding.rs`'s own comment on that gap) do not
corrupt shared state by both calling it. llama.cpp's own contract for `llama_decode`'s return
value (`llama.h`) says any value other than `1` (`NoKvCacheSlot`) or `2` (aborted) rolls the
context's memory state back to what it was before the call, making a bare retry of the same
batch safe. `decode_with_retry` (`llama_cpp.rs`) retries up to `DECODE_RETRY_ATTEMPTS` (3)
times, only for that safe-to-retry class (`is_retryable_decode_error`: a fatal `Unknown` code
below `-1`) -- never `NoKvCacheSlot` (a deterministic capacity condition that would fail
identically every retry) or `NTokensZero` (a Finch-side empty-batch bug no retry can fix).
Proof: `test_is_retryable_decode_error_only_accepts_fatal_unknown_codes`,
`test_decode_with_retry_recovers_from_a_transient_failure_within_budget`,
`test_decode_with_retry_does_not_retry_a_non_retryable_error`, and
`test_decode_with_retry_gives_up_after_exactly_max_attempts` cover the predicate and the bound's
exact-once terminal state without a real GGUF; the intermittent native failure itself is not
independently reproducible on demand, so there is no production-boundary test forcing the exact
native fault -- this is a scoped, evidence-based mitigation for a confirmed-separate issue from
#1292/#1296 above (#1317 is a clean `Result::Err`, both before and after #1295's chunking fix,
never a `SIGABRT`), not a claim that the underlying native flakiness is fully understood or
eliminated.

**`NeuralEmbeddingEngine::embed()`'s prompt `decode()` call never exceeds the embedding
context's batch size (#1296).** Same crash class as the invariant above, different loader,
confirmed via a real crash backtrace (`ggml_abort` <- `llama_context::encode` <- `llama_decode`
<- `NeuralEmbeddingEngine::embed` <- `MemorySystem::query_with_sources`) from embedding an
ordinary ~700-word memory-recall query, and independently reproduced pre-fix in this repo
(`signal: 6, SIGABRT`) against the real bge-small-en-v1.5 GGUF. The exact native assertion
differs from `LlamaCppGenerator`'s: this bidirectional, CLS-pooling model's `decode()` call
dispatches internally to llama.cpp's *encoder* path, which asserts
`GGML_ASSERT(cparams.n_ubatch >= n_tokens && "encoder requires n_ubatch >= n_tokens")`
(`llama-context.cpp:1447`) -- gated on `n_ubatch`, not `n_batch` as the causal decoder path is.
The fix is truncation (`truncate_to_batch`), not `LlamaCppGenerator`'s multi-call chunking: a
`decode()` call commits its tokens' key/value state to the KV cache and freezes their hidden
states, so an earlier chunk's tokens could never attend forward into a later chunk's content --
fine for causal chat generation, which only ever needs the prompt's final token's logits, but
incompatible with this model's bidirectional pooling, which requires every token to attend to
every other token simultaneously (and which the encoder path's own `n_ubatch >= n_tokens`
assertion rules out even attempting across multiple calls). `context_params()` now sets
`n_batch`/`n_ubatch` explicitly to `EMBEDDING_N_BATCH` (512, matching bge-small-en-v1.5's own
trained max sequence length, not an arbitrary Finch choice), and `embed()` truncates any
tokenized input past that length before building the batch, logging a `tracing::warn!` when it
does. Proof: `test_truncate_to_batch_caps_over_length_input`,
`test_truncate_to_batch_leaves_short_input_unchanged`, and
`test_context_params_sets_explicit_batch_sizes_matching_n_ctx` cover the cap and batch-size
arithmetic without a real GGUF; the production-boundary proof against the real native `decode()`
call is `test_real_gguf_embed_over_length_input_does_not_abort` (`#[ignore]`-gated on
`FINCH_TEST_EMBEDDING_GGUF`), confirmed against this exact test to abort pre-fix and pass
post-fix.

**`ToolPromptFormatter::format_tools_for_prompt`'s injected block stays bounded, not a fixed
multi-thousand-token tax (#1310).** Issue #1292/#1295 fixed the chunked-`decode()` crash above;
the very next live turn against a fresh Brain showed the fix's failure mode had moved from a
process abort to a graceful-but-severe context exhaustion: a one-line arithmetic question, with
two recalled memories and nothing else unusual, produced an 8152-token prompt against an
8192-token context before any real conversation existed. The dominant cause was
`format_tools_for_prompt` (`tool_prompt.rs`) formatting every registered tool's full description,
a per-parameter `**Parameters:**` list, and a full XML `**Example:**` block into the prompt on
every local-model turn, unconditionally, regardless of whether the query needed tools at all --
measured at 5707 tokens for the real then-current 36-tool registry (a real llama.cpp tokenizer,
Qwen 2.5 1.5B Instruct; more tools than a same-line-only grep of `repl.rs` counts, since several
registrations span multiple lines). The format now emits one shared XML example (not one per
tool) and a compact `name(param: type, ...): description` line per tool, measuring 2664 tokens
for the same real catalog -- a 53% reduction. `src/local/AGENTS.md` documents the companion fix to
`TemplateGenerator::prompt_parts`'s history budget, which must subtract this block's real cost
rather than a small flat overhead reserve, and names the tests that bound and reproduce this at
the production boundary.

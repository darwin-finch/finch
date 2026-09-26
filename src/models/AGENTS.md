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
reports it; `cpu` forbids offload. ONNX and Candle are not chat providers. ORT remains only
behind the separately owned frontend memory model path until that subsystem migrates it.

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

**Known gap, not covered by the above:** `neural_embedding.rs`'s `NeuralEmbeddingEngine::embed()`
builds one unchunked `LlamaBatch` sized to the full tokenized input and calls `decode()` once,
with no `n_batch` set explicitly and no length cap before the call. It is the same crash shape
this invariant fixes for chat generation, just not yet fixed here: embedding text long enough to
exceed the context's resolved batch size can hit the same native abort. Fix it the same way
(chunk `embed()`'s `decode()` call, or cap and reject an over-length input before it) before
claiming this invariant covers the whole capsule instead of only `LlamaCppGenerator`.

# models capsule: local model loading, routing, and training configuration

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/models/` (llama.cpp GGUF chat loading, bootstrap, adapters, sampling,
threshold routing, LoRA configuration, and neural embedding load). The adjacent `local`,
`generators`, `training`, `feedback`, `router`, and `logging` trees on the DESIGN.md models
row have their own ownership and are not part of this facade.

**Facade:** child modules are private, so the deliberate `pub use` list in
[`mod.rs`](mod.rs) is the callable public surface; rustdoc supplies method signatures.
Callers outside this directory use `crate::models::Item`; they must not name `bootstrap`,
`download`, `loaders`, `neural_embedding`, `unified_loader`, `adapters`, or any other child.

**Dependencies:** `config` (`ExecutionTarget`, `CoreMlConfig`), `memory` (`EmbeddingEngine` for
neural embeddings), and `tools` (prompt/parser types). Production code under `src/models/**`
must not name `crate::cli`. Bootstrap takes a models-owned [`ModelProgress`] port; CLI
implements it and injects it at composition roots (`BootstrapLoader::new`,
`install_model_progress` in `run_daemon` and interactive `main`). The daemon process is
the one that loads the user-selected chat GGUF; memory owns its own model download. The
daemon progress sink must be installed independently of the interactive process's sink.
Do not extract `finch-models` until remaining edges are measured and this reverse edge
stays gone.

**Lifetimes and extension:** `GeneratorState` is shared across bootstrap and server readers;
failed or disabled loading must be represented as state, not a fabricated ready model.
Keep family-specific adapters and engine capability claims here, inject host progress at
composition roots, and add facade exports only for a demonstrated caller need. Do not let
loaders import CLI presentation code.

**Loaders are experimental.** Configuration and loader code are not proof of end-to-end
provider or local-model conformance.

The daemon loads a caller-supplied chat-LLM `.gguf` file through
`backend.model_path`; that user-configured model selector must not also select memory's
embedding/reranking models.
It owns the process-wide llama.cpp backend and creates a fresh context per generation request.
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

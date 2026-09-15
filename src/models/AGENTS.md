# models capsule: local model loading, routing, and training configuration

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/models/` (ONNX/Candle loaders, bootstrap, download, adapters, sampling,
threshold routing, LoRA configuration, and neural embedding load), plus the adjacent
`local`, `generators`, `training`, `feedback`, `router`, and `logging` trees listed on
the DESIGN.md models row. Those adjacent trees are not this facade; this capsule is
`src/models/` only.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature. Child
modules are private, so the `pub use` list in `src/models/mod.rs` is the whole public surface.
Callers outside this directory use `crate::models::Item`; they must not name `bootstrap`,
`download`, `loaders`, `neural_embedding`, `unified_loader`, `adapters`, or any other child.

**Dependencies:** `config` (`ExecutionTarget`, `CoreMlConfig`), `memory` (`EmbeddingEngine` for
neural embeddings), and `tools` (prompt/parser types). Production code under `src/models/**`
must not name `crate::cli`. Bootstrap and download take a models-owned [`ModelProgress`]
port; CLI implements it and injects it at composition roots (`BootstrapLoader::new`,
`install_model_progress`). Do not extract `finch-models` until remaining edges are measured
and this reverse edge stays gone.

**Loaders are experimental.** Configuration variants and loader code are not proof of
end-to-end provider or local-model conformance. Do not change ONNX/Candle/loader/routing/training
behavior in a facade commit.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- models::` plus a caller
smoke (`local::`, `config::backend`). Run the full suite when changing a re-exported `pub`
item. Regenerate the facade digest with `python3 scripts/generate_interfaces.py --write`
whenever the public surface changes.

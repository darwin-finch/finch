# finch-memory: MemTree storage and retrieval

This crate gives Finch long-term memory across turns and sessions: a hierarchical semantic index
(MemTree) over SQLite, with a TF-IDF fallback embedding engine so it works with no model loaded,
plus the program-index table that lets Finch look up previously authored programs by name or
content. It exists as its own crate — with embeddings injected rather than owned — so memory
retrieval quality can be worked on without pulling in ONNX, Candle, tokenizers, Hugging Face, or
any application/TUI code.

Ownership, dependencies, invariants, and test commands are in [`AGENTS.md`](AGENTS.md); the exact
Rust surface is [`src/lib.rs`](src/lib.rs).

## Scope

Callers inject an `EmbeddingEngine`; this crate never selects or downloads a model itself — that's
`src/models/neural_embedding.rs` in the root crate. Program *identity* types (what a program is)
stay in `finch-programs`; this crate only stores `ProgramIndexRecord` rows for lookup.

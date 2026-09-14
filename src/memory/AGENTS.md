# memory capsule: MemTree memory and retrieval

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/memory/` (MemTree over SQLite `schema.sql`, embeddings and the TF-IDF fallback,
retrieval quality, and the opaque program-index table), `src/memory_status.rs`,
and `src/workbook.rs`. Neural embedding model loading is `src/models/neural_embedding.rs`.
Program-definition mapping, authored files, and VM manifests are `src/program_registry.rs`.
Brain event logs are `src/brain/`.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature. Child
modules are private, so the `pub use` list in `src/memory/mod.rs` is the whole public surface, and
`scripts/check_subsystems.py` rejects a `pub mod` there.

**Dependencies:** none. Callers inject `EmbeddingEngine`; constructors never select or download
models. Program identity types stay in `programs`; memory stores `ProgramIndexRecord` rows.
The planned `finch-memory` crate must exclude ONNX, Candle, tokenizers, and Hugging Face.

**Durability:** changes to `schema.sql`, persistence, or retrieval order need the restart and
replay cases from the root [testing rules](../../CLAUDE.md#testing-mandatory).

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- memory:: memory_status:: workbook::` and
`./scripts/test_brains.sh cargo test --test memory_integration_test`. Run the full suite when
changing `schema.sql` or a re-exported `pub` item.

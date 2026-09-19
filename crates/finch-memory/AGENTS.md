# memory capsule: MemTree memory and retrieval

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-memory/src/` (MemTree over SQLite `schema.sql`, embeddings and the TF-IDF
fallback, retrieval quality, and the opaque program-index table), and `memory_status.rs`. Neural
embedding model loading is `src/models/neural_embedding.rs`. Program-definition mapping, authored
files, and VM manifests are `src/program_registry.rs`. Brain event logs are `src/brain/`. The
XLSX-bomb-bounding utility `src/workbook.rs` is not owned here — it has no reference from anything
in this crate; its only callers are `src/runtime/hostio.rs` and `src/cli/tui/mod.rs`.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature. The
`embeddings`, `memtree`, `program_registry`, and `quality` children are private; the `pub use` list
at the top of `crates/finch-memory/src/lib.rs` is their whole public surface, and
`scripts/check_subsystems.py` rejects a `pub mod` for any of them. `memory_status` is the one
public child module (`pub mod memory_status;`), carried over unchanged from when it lived beside
`src/memory` as its own top-level module — callers use `finch_memory::memory_status::observed`
etc. by design, not through a flat re-export. (`scripts/check_subsystems.py` does not exist in this
repo as of #870 — only `scripts/seam_cost.py` and `scripts/generate_interfaces.py` do.)

**Dependencies:** none — `python3 scripts/seam_cost.py crates/finch-memory/src` reports zero
outgoing edges. Callers inject `EmbeddingEngine`; constructors never select or download models.
Program identity types stay in `programs`; memory stores `ProgramIndexRecord` rows. `finch-memory`
excludes ONNX, Candle, tokenizers, and Hugging Face — `src/models/neural_embedding.rs` owns those
and implements the injected `EmbeddingEngine` port.

**Durability:** changes to `schema.sql`, persistence, or retrieval order need the restart and
replay cases from the root [testing rules](../../CLAUDE.md#testing-mandatory).

**`test-support` feature:** the hydration batch/completion/sweep pause seam
(`register_hydration_batch_pause`, `HYDRATION_BATCH`) drives `Loading`/`Degraded` states from
`src/runtime/tests.rs` in the root crate. It used to be `#[cfg(test)]`, which stopped working once
that caller crossed a crate boundary — `cfg(test)` is local to the crate being compiled and is
never set when a dependent crate links this one as an ordinary dependency. It is now
`#[cfg(any(test, feature = "test-support"))]`; the root crate's `[dev-dependencies]` enables
`test-support` on `finch-memory` so `cargo test --workspace` still exercises the real path, while
`[dependencies]` (the release build) does not request the feature, so it never ships.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-memory` and
`./scripts/test_brains.sh cargo test --test memory_integration_test`. Run the full suite when
changing `schema.sql` or a re-exported `pub` item.

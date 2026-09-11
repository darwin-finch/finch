# memory capsule: MemTree memory and retrieval

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/memory/` (MemTree over SQLite `schema.sql`, embeddings and the TF-IDF fallback,
the neural embedding engine, retrieval quality, the program registry), `src/memory_status.rs`,
and `src/workbook.rs`. Local model loading is `src/models/`; Brain event logs are `src/brain/`.

**Interface:** no facade is enforced yet (#541 phase 3); use `MemorySystem` and the `pub use`
re-exports in `src/memory/mod.rs`.

**Dependencies:** layer 1 in [`subsystems.toml`](../../subsystems.toml), no allowed edges. Debt:
`memory → programs` (`memory::program_registry`, `MemorySystem::save_lisp_define`); importing any
other subsystem fails `scripts/check_subsystems.py`, and the check will not stop a new `programs`
import, so add none. The planned
`finch-memory` crate must exclude ONNX, Candle, tokenizers, and Hugging Face; only
`neural_embedding.rs` uses them today, so add no new uses.

**Durability:** changes to `schema.sql`, persistence, or retrieval order need the restart and
replay cases from the root [testing rules](../../CLAUDE.md#testing-mandatory).

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- memory:: memory_status:: workbook::` and
`./scripts/test_brains.sh cargo test --test memory_integration_test`. Run the full suite when
changing `schema.sql` or a re-exported `pub` item.

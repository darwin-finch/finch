# memory capsule: MemTree memory and retrieval

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-memory/src/` (MemTree over SQLite `schema.sql`, embeddings and the TF-IDF
fallback, retrieval quality, and the opaque program-index table), and `memory_status.rs`. Neural
embedding model loading is `src/models/neural_embedding.rs`. Program-definition mapping, authored
files, and VM manifests are `src/program_registry.rs`. Brain event logs are `src/brain/`. The
XLSX-bomb-bounding utility `src/workbook.rs` is not owned here — it has no reference from anything
in this crate; its production callers are `src/runtime/hostio.rs` and `src/cli/tui/mod.rs`, and
`src/runtime/tests.rs` also calls `crate::workbook::fixtures::*` directly to build XLSX fixtures for
the #282 (two-cell spreadsheet exhausts memory) cell-count-bomb regression tests.

**Interface:** the `embeddings`, `memtree`, `program_registry`, and `quality` children are private;
the `pub use` list at the top of `crates/finch-memory/src/lib.rs` is their whole public surface —
read it directly for exact signatures — and `scripts/check_subsystems.py` rejects a `pub mod` for
any of them. `memory_status` is the one public child module (`pub mod memory_status;`), carried
over unchanged from when it lived beside `src/memory` as its own top-level module — callers use
`finch_memory::memory_status::observed` etc. by design, not through a flat re-export.
(`scripts/check_subsystems.py` does not exist in this repo as of the finch-memory extraction, #870
— only `scripts/seam_cost.py` does.)

**Dependencies:** none — `python3 scripts/seam_cost.py crates/finch-memory/src` reports zero
outgoing edges. Callers inject `EmbeddingEngine`; constructors never select or download models.
Program identity types stay in `programs`; memory stores `ProgramIndexRecord` rows. `finch-memory`
excludes ONNX, Candle, tokenizers, and Hugging Face — `src/models/neural_embedding.rs` owns those
and implements the injected `EmbeddingEngine` port.

The SQLite connection is injectable the same way: `MemorySystem::new_with_connection` takes an
already-open `Arc<Mutex<Connection>>` and never calls `Connection::open` itself — it requires that
connection to be uncontended on entry and returns `Err`, not a panic, if it isn't. `new_with_engine`
is a thin wrapper around the shared `MemorySystem::open_connection(db_path)` (dir creation, open,
WAL) plus `new_with_connection`. The composition root (`src/cli/repl.rs`) calls
`open_connection` and `new_with_connection` directly so the injected path is actually exercised;
test call sites may keep using the path-based `new`/`new_with_engine` convenience constructors.

**Durability:** changes to `schema.sql`, persistence, or retrieval order need the restart and
replay cases from the root [testing rules](../../CLAUDE.md#testing-mandatory).

**`test-support` feature:** the hydration batch/completion/sweep pause seam
(`register_hydration_batch_pause`, `HYDRATION_BATCH`) drives `Loading`/`Degraded` states from
`src/runtime/tests.rs` in the root crate. It used to be `#[cfg(test)]`, which stopped working once
that caller crossed a crate boundary — `cfg(test)` is local to the crate being compiled and is
never set when a dependent crate links this one as an ordinary dependency. It is now
`#[cfg(any(test, feature = "test-support"))]`; the root crate's `[dev-dependencies]` enables
`test-support` on `finch-memory` so `cargo test --workspace` still exercises the real path, while
`[dependencies]` (the release build) does not request the feature, so it never ships. See the
`test-support` feature comment in `Cargo.toml` and the block comment at the first
`#[cfg(any(test, feature = "test-support"))]` use in `src/lib.rs` for the failure mode a bare
`#[cfg(test)]` seam would have caused instead.

**Which cross-crate test-seam pattern to use (for #867's later extractions too):** two patterns
exist in this workspace for exposing something a dependent crate's tests need to reach, and they
solve different problems.

- **An always-`pub` test double**, unconditionally compiled and exported through the normal facade
  `pub use` — `crates/finch-generation/src/scripted.rs`'s `ScriptedBackend`/`ScriptedStep` are the
  precedent. Use this when the thing a dependent crate's tests need is a self-contained *value* —
  a fake/mock implementation of a trait, a builder, a fixture — that does not require any
  conditionally-compiled hook inside the crate's own production control flow to do anything. Its
  cost is a small amount of always-shipped, inert code in the release binary.
- **A `test-support` Cargo feature**, gating `#[cfg(any(test, feature = "test-support"))]` — this
  crate's hydration pauses are the precedent (added by #870). Use this when the seam is not just a
  value but requires production functions (here, `MemorySystem::new`) to call conditionally-compiled
  hooks inline in their real control flow; an always-`pub`, always-compiled version of that would
  mean the hook-calling code itself ships in every release build, not just an inert helper type.
  The feature keeps that hook code out of `[dependencies]` (release) while `[dev-dependencies]`
  requests it for `cargo test --workspace`.

Default to the always-`pub` test double; reach for a `test-support` feature only when the seam has
to reach into a production function's own conditionally-compiled branches, as here.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-memory` and
`./scripts/test_brains.sh cargo test --test memory_integration_test`. Run the full suite when
changing `schema.sql` or a re-exported `pub` item.

# Finch memory agent contract

Supplements the root [agent rules](../../AGENTS.md). Read the [memory README](README.md) for
ownership and caller workflows. The flat facade is [`src/lib.rs`](src/lib.rs); use
`cargo doc -p finch-memory --no-deps --open` for public methods on re-exported types. Child
modules, including `memory_status`, are private.

## Dependencies and extension rules

- This crate owns MemTree storage and retrieval, SQLite schema and hydration, TF-IDF fallback,
  and opaque `ProgramIndexRecord` rows. It has no production dependency on another Finch crate.
  Callers inject an `EmbeddingEngine`; `src/models/neural_embedding.rs` owns neural model loading.
- The root [`src/program_registry.rs`](../../src/program_registry.rs) maps program definitions to
  memory's opaque rows and owns canonical authored source files and VM manifests. Brain event
  journals belong to `finch-brain`, not this crate.
- Keep external capabilities as deliberate flat exports from `src/lib.rs`. Do not expose child
  module paths or add a generated signature catalog. The four memory-status capabilities used by
  runtime, tools, and CLI are flat exports; `status_line` is crate-only.

## Invariants and lifetimes

- The turn-level injection gate (#1134) is decided before the per-result
  floor: when `MemoryConfig::min_turn_relevance_score` is set (default `None`,
  off) and even the best retrieved weighted score falls strictly below it,
  `query_with_sources` returns an empty set for the turn and logs the skip at
  `info` with the floor, the best score, and the candidate count. The
  per-result `min_relevance_score` floor still applies whenever injection
  happens; the gate does not change retrieval ordering.
- Retrieval may proceed while the tree hydrates, but every caller must report the coverage of
  the index it actually read. Sample hydration before and after a read and use `observed`; never
  upgrade a partial read to `Ready` because hydration completed afterward.
- `MemorySystem::new_with_connection` takes an already-open, uncontended SQLite connection and
  returns an error rather than panicking when it cannot acquire it. `open_connection` and the
  path-based convenience constructors share the same WAL setup; the REPL exercises injection in
  production.
- Keep canonical source and the rebuildable program index separate. Changes to `schema.sql`,
  retrieval ordering, or hydration require restart, replay, and partial-index tests.
- Cross-crate tests that pause hydration use the `test-support` feature because a dependent
  crate's tests do not enable this crate's `cfg(test)`. Keep such hooks out of release builds;
  prefer an always-public inert test double when no production control-flow hook is needed.

## Focused proof

```bash
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-memory --lib
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-memory --test gate_observability_test
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test --test memory_integration_test
```

The gate's log-observability proof lives in its own integration binary
(`gate_observability_test`) because tracing caches per-callsite interest
globally within a process; a `with_default` capture running beside parallel
tests that exercise the same callsites can lose the asserted line to a
concurrent no-dispatcher cache recompute.

For facade or schema changes, run the supervised workspace suite. The proposed
`scripts/check_subsystems.py` is absent; use `scripts/seam_cost.py` for dependency evidence.

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
- `src/routing_tree.rs` is a real, tested, standalone binary routing tree (candidate-selected PCA
  axes via successive Hotelling deflation, dual-insert, stability-gated splitting, incrementally
  maintained real centroids, adaptive/beam/backtrack search, a verified removal primitive) plus its
  own `routing_tree/persistence.rs` (save/load against `routing_points`/`routing_nodes`/
  `routing_leaf_membership`, additive to `schema.sql`). Ported from a sibling research repo's
  validated D reference (`fractal-corpus-curation`'s `BUILD_ARCHITECTURE.md`/`EXPERIMENT_LOG.md`
  §29-§56); compiled, unit- and persistence-round-trip-tested (32 tests), **not yet wired into
  `MemorySystem`** — `MemTree`'s similarity-threshold/promote-on-close-match routing is still the
  live mechanism. Wiring it in (replacing `MemTree`'s routing internals while keeping this crate's
  persistence/hydration/quality/provenance layers, which are algorithm-agnostic) is real, scoped,
  outstanding follow-up work, not a design question still open. A learned per-node scoring layer
  (the sibling repo's own `self_play_router.d`) was deliberately deferred, not ported — across every
  tested configuration in that repo and in a separate Rust spike, it has not been shown to reliably
  beat this plain structural mechanism; `descend_beam`/`descend_adaptive` accept an optional
  `BranchScorer` closure as the seam it would plug into later, unused for now.

## Invariants and lifetimes

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
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-memory --lib routing_tree
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test --test memory_integration_test
```

For facade or schema changes, run the supervised workspace suite. The proposed
`scripts/check_subsystems.py` is absent; use `scripts/seam_cost.py` for dependency evidence.

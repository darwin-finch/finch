# Finch memory agent contract

Supplements the root [agent rules](../../AGENTS.md). Read the [memory README](README.md) for
ownership and caller workflows. The flat facade is [`src/lib.rs`](src/lib.rs); use
`cargo doc -p finch-memory --no-deps --open` for public methods on re-exported types. Child
modules, including `memory_status`, are private.

## Dependencies and extension rules

- This crate owns `RoutingTree` storage and retrieval, SQLite schema and hydration, TF-IDF
  fallback, and opaque `ProgramIndexRecord` rows. It has no production dependency on another Finch
  crate. Callers inject an `EmbeddingEngine`; `src/models/neural_embedding.rs` owns neural model
  loading.
- The root [`src/program_registry.rs`](../../src/program_registry.rs) maps program definitions to
  memory's opaque rows and owns canonical authored source files and VM manifests. Brain event
  journals belong to `finch-brain`, not this crate.
- Keep external capabilities as deliberate flat exports from `src/lib.rs`. Do not expose child
  module paths or add a generated signature catalog. The four memory-status capabilities used by
  runtime, tools, and CLI are flat exports; `status_line` is crate-only.
- `src/routing_tree.rs` is a real, tested, standalone binary routing tree (candidate-selected PCA
  axes via successive Hotelling deflation, dual-insert, stability-gated splitting, incrementally
  maintained real centroids, adaptive/beam/backtrack search, a verified removal primitive) plus its
  own `routing_tree/persistence.rs` for save/load. The schema it persists to (`schema.sql`:
  routing_points, routing_nodes, routing_leaf_membership) is the sole memory-index schema — it
  replaced tree_nodes/MemTree outright, not additively. Ported from a sibling research repo's
  validated D reference
  (`fractal-corpus-curation`'s `BUILD_ARCHITECTURE.md`/`EXPERIMENT_LOG.md` §29-§56) and **wired
  into `MemorySystem` via the `RoutingMemTree` facade** (`src/routing_memory.rs`) as the sole
  routing mechanism; `MemTree` and its `tree_nodes` schema no longer exist in this crate. A learned
  per-node scoring layer (the sibling repo's own `self_play_router.d`) was deliberately deferred,
  not ported — across every tested configuration in that repo and in a separate Rust spike, it has
  not been shown to reliably beat this plain structural mechanism; `descend_beam`/`descend_adaptive`
  accept an optional `BranchScorer` closure as the seam it would plug into later, unused for now.
- RoutingTree hydration is atomic: `MemorySystem::hydrate_in_background` makes one
  `RoutingMemTree::load` call with no intermediate checkpoint, unlike MemTree's own batched
  loader. Two real, disclosed regressions follow from that, not yet replicated: (1) no
  partial/batched hydration resilience — a load either fully succeeds or is `Failed` outright, so
  `HydrationStatus::Degraded` is no longer reachable through production hydration at all (the
  variant and its projection are still real and tested, via
  `MemorySystem::force_degraded_for_test`, a `test-support`-gated direct-injection seam); (2) the
  iterative parent-pointer walks in `remove_point`'s downdate and `insert_into`'s update use
  `.expect()` on a missing parent, which panics rather than returning a diagnosable `Result` if
  `routing_nodes.parent_id` were ever corrupted on disk — MemTree's #274 fix for this exact class
  of problem (detect a cycle, name it, return `Err`) was not replicated.

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
- Cross-crate tests that pause hydration, or that force a `Degraded` status, use the
  `test-support` feature because a dependent crate's tests do not enable this crate's `cfg(test)`.
  Keep such hooks out of release builds; prefer an always-public inert test double when no
  production control-flow hook is needed. There is no batch-hydration pause anymore — RoutingTree
  hydration is atomic, so there is no batch boundary left to pause at; only the completion and
  projection-sweep pauses remain live.

## Focused proof

```bash
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-memory --lib
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-memory --lib routing_tree
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

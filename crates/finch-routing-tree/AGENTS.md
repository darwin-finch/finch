# Finch routing-tree agent contract

Supplements the root [agent rules](../../AGENTS.md). Read the [routing-tree README](README.md) for
purpose and caller workflows. The flat facade is [`src/lib.rs`](src/lib.rs); use
`cargo doc -p finch-routing-tree --no-deps --open` for public methods on re-exported types. Child
modules — the tree implementation and its `routing_tree/persistence.rs` codec — are private.

## Dependencies and extension rules

- This crate owns the `RoutingTree` mechanism (candidate-selected PCA axes via successive Hotelling
  deflation, dual-insert, stability-gated splitting, incrementally maintained real centroids,
  adaptive/beam/backtrack search, a verified removal primitive) and the SQLite save/load codec for
  the tree's durable rows. It has **no production dependency on any other Finch crate**; callers
  depend on it, never the reverse. `finch-memory` is the sole in-repo caller today, through its
  `RoutingMemTree` facade.
- The crate does not own a schema. `routing_points`/`routing_nodes`/`routing_leaf_membership` DDL
  lives with the caller (in Finch, `finch-memory`'s `schema.sql`, the sole memory-index schema).
  The crate only opens a caller-provided `rusqlite::Connection` and reads/writes those rows.
  `routing_tree/test_schema.sql` is a test fixture copied verbatim from that schema's routing
  section — if the routing DDL changes there, mirror it here or these tests stop testing what
  production runs against.
- Ported from a sibling research repo's validated D reference
  (`fractal-corpus-curation`'s `BUILD_ARCHITECTURE.md`/`EXPERIMENT_LOG.md` §29-§56). A learned
  per-node scoring layer (the sibling repo's own `self_play_router.d`) was deliberately deferred,
  not ported — across every tested configuration in that repo and in a separate Rust spike, it has
  not been shown to reliably beat this plain structural mechanism; `descend_beam`/`descend_adaptive`
  accept an optional `BranchScorer` closure as the seam it would plug into later, unused for now.
  `RoutingConfig.onlineAxisRefinement` was also deliberately omitted (the D reference recommends
  leaving it off); portable later if a concrete need shows up.
- Persistence uses the dirty-tracking discipline (mark on mutation, save only what changed, clear
  on hydrate): `dirty_node_ids`/`mark_persisted` are the caller-facing pair — a save that fails or
  is cancelled before committing leaves the marks set, so the next save rewrites them.
  `save_dirty_nodes` is the standalone wrapper; `write_dirty_nodes_within` composes into a caller's
  larger transaction (point content, tree structure, and caller provenance in ONE transaction).
  A single insert dirties every node on its path (each node's `real_centroid` updates) — a real,
  larger fan-out than the earlier per-insert dirty set, disclosed, not an oversight.
- The crate knows nothing about text semantics, importance, hydration state machines, or recall
  gating. Point ids are permanent from the moment `insert` assigns them (removal tombstones the
  slot; no renumbering, no promotion). Canonical point content is persisted exactly once regardless
  of dual-insert fan-out; `bucket_deflated` is never persisted — it is deterministically
  reconstructed on load from the canonical embedding plus the frozen `(anchor, direction)` chain.

## Invariants

- Fixed seed, deterministic structure: identical (config, dim, seed, insert order) reproduces an
  identical tree; a freshly loaded tree continues the persisted structure. Callers that must
  re-hydrate into the same behavior use the same fixed seed every run.
- Load is atomic in shape: `load_routing_tree` either returns a fully linked tree or `Err`; there
  is no partial tree. What hydration *scheduling* (background, partial reads, degradation) looks
  like is the caller's lifecycle, not this crate's.
- A real, disclosed regression carried from the port, not yet fixed: the iterative parent-pointer
  walks in `remove_point`'s downdate and `insert_into`'s update use `.expect()` on a missing
  parent, which panics rather than returning a diagnosable `Result` if `routing_nodes.parent_id`
  were ever corrupted on disk — the memory index's earlier cycle-detect-and-return-`Err` fix for
  this exact class of problem was not replicated here.
- No test-support feature exists: no cross-crate test seam is needed — nothing pauses the tree.
  All tests are plain `#[cfg(test)]` within this crate.

## Focused proof

```bash
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-routing-tree --lib
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-routing-tree --lib routing_tree
```

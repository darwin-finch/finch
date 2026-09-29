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
- **Fixed (issue #1384):** `dim` is fixed for the life of a store, not just for the life of one
  in-memory tree — `load_routing_tree` now rejects, with a named `Err`, a caller-supplied `dim`
  that disagrees with the dimensionality of the embeddings that store was actually built and split
  under (checked against the first loaded `routing_points` row). Before this, reopening an existing
  store with a different `dim` than the session that built it (the real, reachable trigger: Finch's
  own composition root deliberately swaps the hashed-n-gram fallback for a neural embedding engine
  on the *next restart* once a background download completes, against the SAME on-disk store —
  `src/cli/repl.rs`'s own comment) silently produced a tree whose `self.dim` field no longer matched
  its own persisted node geometry: every node's `real_centroid`, and every decision node's
  `anchor`/`direction`, stayed at the OLD dimension. Hydration itself stayed internally consistent
  (its replay-descent only ever compares a node's persisted data against that same store's own
  persisted embeddings) and returned `Ok`, so the mismatch surfaced later and unpredictably: the
  next real insert indexed one of those shorter, persisted per-node arrays with `self.dim` and
  panicked — in `projection` (`routing_tree.rs:181`, the issue's own report) if it reached a
  decision node, or earlier still in `insert_into`'s own `real_centroid` update loop (which runs on
  every visited node, before `projection` is ever reached) for other old/new dimension
  combinations — on whatever thread happened to be inserting at the time, a background
  hydration/indexing worker in production. `RoutingTree::insert` itself still does not validate
  `point.len()` against `self.dim` per-call (see its own doc comment) — the fix is at the tree's
  construction boundary, not per-insert, since a consistent `dim` for a store's whole lifetime was
  already the load-bearing (if previously unenforced) assumption every other invariant here
  depends on. `test_reloading_a_store_at_a_different_embedding_dimension_never_panics_on_a_later_insert`
  in `src/routing_tree/persistence.rs` reproduces the exact panic end-to-end (real persistence, a
  real split tree, a real subsequent insert) and fails before this fix, not just against a
  synthetic mismatched-slice call into `projection`; `crates/finch-memory`'s own
  `test_hydrating_a_store_built_at_a_different_embedding_dimension_settles_failed_not_stuck`
  covers the same defect through the real async hydration path a background tokio worker uses in
  production.
- **Fixed (issue #1329):** `remove_point`'s two iterative parent-pointer walks (the primary
  root-leaf downdate and the dual-entry-to-divergence-node downdate) used to `.expect()` a
  missing parent and panic if `routing_nodes.parent_id` were ever corrupted on disk — dormant only
  because nothing called removal in production before #1329 exposed it through a real
  user-facing tool (`memory_remove`/`remove_memory`). Both walks now fail closed with a named
  `Err` instead, mirroring #274's own precedent for the identical class of problem in the legacy
  index (detect and name it, return `Err`, never panic), and are bounded to `nodes.len()` steps so
  a corrupt cycle returns an error instead of looping forever. `remove_point` also now rejects an
  out-of-range `point_id` up front (`self.removed_flag[point_id]`'s own `Vec` indexing used to
  panic on one, and a `memory_id` a caller hands `remove_memory` — e.g. a hand-typed or
  hallucinated `node:999999` — is exactly the kind of external input that can produce one).
  `test_remove_point_out_of_range_errors_rather_than_panicking`,
  `test_remove_point_with_corrupted_primary_parent_chain_errors_rather_than_panicking`,
  `test_remove_point_with_corrupted_dual_parent_chain_errors_rather_than_panicking` in
  `src/routing_tree/tests.rs` force each condition directly on the in-memory tree (not a disk
  fixture) and assert `Err`, not a panic. `insert_into`'s own `.expect()`
  (`"dual insert must carry its divergence node"`) is a different, narrower case — the one call
  site always passes `Some`, so it stays provably unreachable by construction and was out of
  scope here.
- No test-support feature exists: no cross-crate test seam is needed — nothing pauses the tree.
  All tests are plain `#[cfg(test)]` within this crate.
- `RoutingConfig::default()`'s `dual_insert_threshold` is `0.10`, not `0.0` (issue #1322, corrected
  2026-09-26): the field's own doc comment already called this "the single largest accuracy lever
  measured" in the D reference port, but the crate shipped it disabled. A dispatched investigation
  measured `descend_adaptive_top_k` against brute-force cosine on Finch's actual
  `HashedNgramEmbedding` (dim=2048) embeddings and found real, monotonic accuracy loss with corpus
  size at `0.0` (top-10 overlap 91.5%→51.5%, N=500→100,000) that `0.10` substantially mitigates
  (97.2%→76.7% over the same range) — a real, measured, no-new-dependency win at every tested size,
  though the same degrade-with-N trend still shows through at the corrected value, just shifted
  later (mitigation, not a fix for the underlying navigation-accuracy question tracked by #1322's
  larger brute-force/HNSW fork decision). Independently re-measured on this worktree's own code
  with a smaller synthetic topic-structured corpus (30 held-out queries against `HashedNgramEmbedding`
  embeddings, N up to 8,000): top-10 overlap improved from 0.820→0.887 at N=500 to 0.270→0.473 at
  N=8,000 (`0.0`→`0.10`), the same direction and, at this corpus's higher-N end, an even larger
  relative gain — confirms the fix on real code, not just the cited investigation's own numbers.
  The known cost (leaf-membership fans out ~2.43x at `threshold=0.10` per the field's own doc
  comment) is real; re-calibrate if a specific deployment's storage/query-cost budget can't absorb
  it.

## Focused proof

```bash
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-routing-tree --lib
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-routing-tree --lib routing_tree
```

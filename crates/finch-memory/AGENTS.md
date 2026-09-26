# Finch memory agent contract

Supplements the root [agent rules](../../AGENTS.md). Read the [memory README](README.md) for
ownership and caller workflows. The flat facade is [`src/lib.rs`](src/lib.rs); use
`cargo doc -p finch-memory --no-deps --open` for public methods on re-exported types. Child
modules, including `memory_status`, are private.

## Dependencies and extension rules

- This crate owns the SQLite schema and hydration, the hashed-n-gram fallback embedding
  (`HashedNgramEmbedding`, `src/embeddings.rs` — feature-hashed, weighted word/character-n-gram
  bag, not TF-IDF: it has no corpus-level document-frequency statistic), and opaque `ProgramIndexRecord`
  rows. The `RoutingTree` mechanism itself lives in `crates/finch-routing-tree` — a dependency-free
  foundational crate this crate depends on (its only production dependency on another Finch crate;
  the tree depends on nothing Finch-side, so no cycle). Callers inject an `EmbeddingEngine`;
  `src/models/neural_embedding.rs` owns neural model loading.
- The root [`src/program_registry.rs`](../../src/program_registry.rs) maps program definitions to
  memory's opaque rows and owns canonical authored source files and VM manifests. Brain event
  journals belong to `finch-brain`, not this crate.
- Keep external capabilities as deliberate flat exports from `src/lib.rs`. Do not expose child
  module paths or add a generated signature catalog. The four memory-status capabilities used by
  runtime, tools, and CLI are flat exports; `status_line` is crate-only.
- `RoutingTree` (candidate-selected PCA axes via successive Hotelling deflation, dual-insert,
  stability-gated splitting, incrementally maintained real centroids, adaptive/beam/backtrack
  search, a verified removal primitive) and its save/load codec live in
  [`crates/finch-routing-tree`](../finch-routing-tree/AGENTS.md); see that capsule for the
  mechanism's own rules. This crate owns what wraps it: `schema.sql`
  (`routing_points`, `routing_nodes`, `routing_leaf_membership`) is the sole memory-index schema —
  it replaced tree_nodes/MemTree outright, not additively — and `src/routing_memory.rs`'s
  `RoutingMemTree` facade wires the tree into `MemorySystem` as the sole routing mechanism. The
  tree's provenance is the sibling research repo's validated D reference
  (`fractal-corpus-curation`'s `BUILD_ARCHITECTURE.md`/`EXPERIMENT_LOG.md` §29-§56); a learned
  per-node scoring layer (the sibling repo's own `self_play_router.d`) was deliberately deferred,
  not ported — across every tested configuration in that repo and in a separate Rust spike, it has
  not been shown to reliably beat this plain structural mechanism; `descend_beam`/`descend_adaptive`
  accept an optional `BranchScorer` closure as the seam it would plug into later, unused for now.
- RoutingTree hydration is atomic: `MemorySystem::hydrate_in_background` makes one
  `RoutingMemTree::load` call with no intermediate checkpoint, unlike MemTree's own batched
  loader. A real, disclosed regression follows from that, not yet replicated: no partial/batched
  hydration resilience — a load either fully succeeds or is `Failed` outright, so
  `HydrationStatus::Degraded` is no longer reachable through production hydration at all (the
  variant and its projection are still real and tested, via
  `MemorySystem::force_degraded_for_test`, a `test-support`-gated direct-injection seam). The
  other disclosed regression of the port — the iterative parent-pointer walks in
  `RoutingTree::remove_point`'s downdate and `insert_into`'s update using `.expect()` on a missing
  parent — moved with the mechanism; see the [routing-tree
  capsule](../finch-routing-tree/AGENTS.md).

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
- `RoutingMemTree::insert_occurrence` (`routing_memory.rs`) never dedups by text: every
  occurrence gets its own independent point/embedding, even when the text exactly repeats,
  because it calls a non-deduping insert instead of `insert_with_effect` —
  `test_insert_occurrence_mints_distinct_uuids_for_repeated_identical_text` and
  `test_insert_occurrence_does_not_bump_importance_of_earlier_occurrence_on_duplicate_text` in
  `src/routing_memory/tests.rs`. `insert_with_effect` itself has no production caller left:
  `project_stored_conversation_inner` (`lib.rs`) calls `insert_occurrence` for both
  `insert_conversation`'s plain path and `insert_brain_conversation`'s named-Brain path (they
  share one implementation), so repeating identical content through either now produces a new
  memory each time instead of bumping an earlier one's importance in place —
  `test_storing_identical_content_repeatedly_creates_distinct_occurrences` in `lib.rs` pins the
  new behavior; `insert_with_effect` stays only as a tested, directly-callable primitive of
  `RoutingMemTree` with no current caller of its own. Disclosed narrowing, now moot in production
  but still real if `insert_with_effect` is ever called again: after a `RoutingMemTree::load`
  reload, `text_index` is rebuilt from every persisted point indiscriminately (no provenance
  column distinguishes an occurrence-created point from a plain one), so a later
  `insert_with_effect` call could dedup onto a point that started life as an occurrence — see
  `load`'s doc comment. `insert_occurrence` itself is unaffected either way.
- `project_stored_conversation_inner` resolves each new occurrence's `prev` durably, not from an
  in-memory cache: `MemorySystem::last_occurrence_uuid_for_session` joins
  `conversations`/`memory_sources`/`routing_occurrences` for the turn's own `session_id` (now
  threaded from `PendingConversation` through `PENDING_PROJECTION_SQL`) to find that session's
  most recently projected occurrence, so the chain survives a process restart with no rebuild
  step and no separate recovery path — `test_occurrence_chain_survives_a_process_restart` in
  `lib.rs`. A turn stored with no `session_id` always gets `prev = None` (nothing to chain from).
  `MemorySystem::save_routing_occurrence` mints the occurrence, persists its row, the point's
  content, the changed routing nodes, the provenance row, and an attempted `link_next` from
  `prev` all inside ONE SQLite transaction — the same atomicity discipline
  `project_stored_conversation_inner`'s own comments require of the point/tree persistence it
  replaced (`save_routing_insert`), extended to cover the occurrence row and the link so a
  rollback undoes all of it together, never an occurrence naming a point that was never
  persisted. `LinkNextError::Conflict` there is logged and does NOT fail the call or roll back
  the transaction: the new occurrence and its point are still real and committed, only the
  backward link from `prev` did not win — `test_a_link_conflict_does_not_fail_or_corrupt_the_losing_turn`
  in `lib.rs`. Retry-safety for a named-Brain turn needs no extra bookkeeping at the occurrence
  layer: the existing `already_classified` / deterministic-id short-circuit in
  `insert_conversation_record` already gates the entire projection (point AND occurrence
  together) on `conversation_id`, the same guarantee it already gave the point alone —
  `test_a_brain_retry_projects_its_own_stranded_turn` in `lib.rs`.
- `RoutingMemTree::retrieve` resolves near-tied cosine scores (within `NEAR_TIE_EPSILON = 0.01`,
  routing_memory.rs — an order of magnitude below `MemoryConfig::min_relevance_score`'s default
  0.15 floor) using occurrence-chain neighbor context. This tie-break was previously wired but
  inert (nothing populated `routing_occurrences`); now that `project_stored_conversation_inner`
  calls `insert_occurrence`/`link_next` on every projected turn (above), it has real chain data
  to compare against in production, not only in its own direct unit tests. For each tied
  candidate it walks its
  occurrence's `prev`/`next` via `routing_occurrences`, compares those neighbors' embeddings
  against the same query embedding already passed to `retrieve`, and reorders the tie by which
  candidate's surrounding conversation matches better. This can change which candidates survive
  the `top_k` truncation, not just their order. A candidate with no occurrence row, or whose
  occurrence has neither `prev` nor `next` set, falls back to raw-cosine order instead of
  crashing or being excluded — `test_retrieve_prefers_near_tied_candidate_whose_occurrence_neighbor_matches_query_context`
  and `test_retrieve_tie_break_falls_back_gracefully_when_neither_candidate_has_occurrence_context`
  in `src/routing_memory/tests.rs`.
  Because the tie-break reads the `routing_occurrences` table (`src/schema.sql`), `RoutingMemTree::retrieve`
  now takes `conn: &Connection`; both call sites in `lib.rs`
  (`query_with_sources`, `conversation_summary`) acquire the db lock before the tree lock, the
  same order `stats()` already used, to avoid a lock-order deadlock.
- `counterpart_turn` (`lib.rs`, used by `query_recall`'s rendering) pairs a retrieved turn with
  its real reply/question via that same occurrence chain, not by wall-clock proximity: for a
  retrieved user turn it walks the turn's own occurrence `next` (the reply is whatever occurrence
  comes right after it in the session's chain); for a retrieved assistant turn it walks `prev`.
  This is a real request/response link, so it cannot be fooled by some other same-session,
  opposite-role turn landing closer in time than the actual reply — the bug this replaced
  (`ORDER BY ABS(c.timestamp - ?4) ASC` picking the nearest-in-time row regardless of whether it
  was the real counterpart). The old nearest-timestamp query survives only as
  `counterpart_turn_by_nearest_timestamp`, a fallback used when the retrieved turn has no
  occurrence row at all (a legacy leaf predating occurrence chains, or a classifier-discarded
  turn) or its occurrence exists but has no `next`/`prev` set yet (first/last turn of its chain,
  the same documented case `RoutingMemTree::retrieve`'s tie-break above falls back on) —
  `test_counterpart_turn_follows_occurrence_chain_not_nearest_timestamp` in `lib.rs` reproduces
  the mis-pairing (a third, unrelated same-session turn made numerically closest in timestamp)
  and asserts both `counterpart_turn` and `render_recall_entry` return/display the true reply.

## Focused proof

```bash
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-memory --lib
.agents/skills/finch-backlog/scripts/with-cargo-slot ./scripts/test_brains.sh cargo test -p finch-routing-tree --lib
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

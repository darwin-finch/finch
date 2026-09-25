# Finch memory

`finch-memory` stores and retrieves the local semantic index, maintaining a SQLite-backed store
that can hydrate in the background while a query proceeds. The tree mechanism underneath —
[`RoutingTree`](../finch-routing-tree/README.md), in its own dependency-free crate — is owned by
`finch-routing-tree`; this crate owns the schema, hydration, retrieval coverage reporting, and
opaque program-index rows wrapped around it. It does not choose a neural embedding model,
interpret programs, write canonical program source, or own named-Brain event journals.

Recall applies two filters in order: a turn-level injection gate
(`MemoryConfig::min_turn_relevance_score`, default off) first decides whether anything is
injected at all -- a turn whose best retrieved weighted score falls strictly below the floor
injects nothing and logs why -- and then the per-result `min_relevance_score` floor drops
individual weak matches from the turns that are allowed. Neither filter changes retrieval
ordering; both are uncalibrated starting points pending real score distributions.

Two callers show the ownership boundary:

1. The [interactive REPL](../../src/cli/repl.rs) chooses an embedding engine from application
   configuration, calls `MemorySystem::open_connection`, and injects that connection into
   `MemorySystem::new_with_connection`. It then asks the application `ProgramRegistry` to index
   selected authored roots. Memory owns the SQLite store and hydration; the REPL owns model
   selection, startup timing, and which roots to synchronize.
2. The [program registry](../../src/program_registry.rs) writes canonical authored source and
   converts `ProgramDefinition` values into opaque `ProgramIndexRecord` rows for
   `MemorySystem::index_program_record`. Memory persists and searches those rows without knowing
   Forth, Lisp, or manifest semantics. The registry rebuilds the discovery index from source.

The [agent contract](AGENTS.md) covers dependency, hydration, and persistence rules. The
[flat facade](src/lib.rs) and `cargo doc -p finch-memory --no-deps --open` provide the public API.

## Why MemTree's routing was replaced

MemTree's own retrieval was a flat, exhaustive cosine scan over every leaf — exact, but O(n) per
query, a real ceiling as stored memories grow.
[`finch-routing-tree`](../finch-routing-tree/README.md) is its replacement, now the sole routing
mechanism behind `MemorySystem` through the `RoutingMemTree` facade (`src/routing_memory.rs`): a
binary tree with genuinely fitted split axes (candidate-selected PCA via successive Hotelling
deflation, not MemTree's similarity-threshold promotion) and sub-linear adaptive/beam search,
ported from a sibling research repo's validated design (see the routing-tree crate's docs for
provenance and what was deliberately deferred). `MemTree` and its `tree_nodes` schema are gone;
the [agent contract](AGENTS.md) has the current invariants and the disclosed regressions the port
carries.

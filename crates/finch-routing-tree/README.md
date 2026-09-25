# Finch routing-tree

`finch-routing-tree` is the standalone binary routing tree over embeddings behind Finch's semantic
recall: real, data-fitted split axes (candidate-selected PCA via successive Hotelling deflation,
not random hyperplanes), online/incremental construction (points insert one at a time), dual-insert
boundary hedging, incrementally maintained real centroids enabling O(1) best-first adaptive search,
and a verified removal primitive — plus SQLite save/load for the tree's durable rows.

The crate has no dependency on any other Finch crate. It owns the tree mechanism and its
persistence codec; the caller owns everything around it:

1. **Schema and connection.** The caller creates `routing_points`/`routing_nodes`/
   `routing_leaf_membership` (in Finch, `finch-memory`'s `schema.sql`) and hands the crate an open
   `rusqlite::Connection`.
2. **Embeddings.** The caller embeds text into fixed-dimension vectors however it chooses and
   inserts the vectors; the tree knows nothing about text.
3. **Lifecycle.** Background hydration, partial reads, and degradation policy are the caller's
   storage discipline; `load_routing_tree` is one atomic all-or-nothing rebuild.

[finch-memory](../finch-memory/README.md) is the sole in-repo caller today: its `RoutingMemTree`
facade wraps this tree as the sole routing mechanism behind `MemorySystem`. The same shape serves
any embed-then-retrieve corpus — index a batch of vectors, persist through the caller's
transaction, then search sub-linearly:

```rust
let mut tree = finch_routing_tree::RoutingTree::new(
    finch_routing_tree::RoutingConfig::default(), dim, fixed_seed,
);
let point_id = tree.insert(embedding);
// search
let results = tree.descend_adaptive_top_k(&query, top_k, false);
// persist against the caller's own schema and transaction
finch_routing_tree::save_point(&tx, point_id, &text, &embedding, importance, created_at)?;
let dirty = tree.dirty_node_ids();
finch_routing_tree::write_dirty_nodes_within(&tree, &dirty, &tx)?;
tx.commit()?;
tree.mark_persisted(&dirty);
// later: full atomic rebuild from durable rows
let (tree, points) = finch_routing_tree::load_routing_tree(&conn, cfg, dim, fixed_seed)?;
```

The [agent contract](AGENTS.md) covers dependency, persistence, and provenance rules. The
[flat facade](src/lib.rs) and `cargo doc -p finch-routing-tree --no-deps --open` provide the public
API.

## Why this is its own crate

The mechanism — embed text, index it, retrieve by similarity, sub-linear search over a fitted
binary tree — is generically useful beyond conversation memory. Keeping it out of `finch-memory`
draws the line the memory crate's own docs already draw: memory owns schema, hydration, quality,
and recall policy; the tree is the mechanism underneath, usable standalone (extracted in the
routing-tree issue, #1179, part 1).

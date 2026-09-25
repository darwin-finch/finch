//! A standalone binary routing tree over embeddings: real, data-fitted split axes
//! (candidate-selected PCA via successive Hotelling deflation), online/incremental construction,
//! dual-insert boundary hedging, incrementally maintained real centroids enabling O(1)
//! best-first adaptive search, and a verified removal primitive — plus SQLite save/load against
//! `routing_points`/`routing_nodes`/`routing_leaf_membership`.
//!
//! The crate has no dependency on any other Finch crate. It owns the tree mechanism and its
//! persistence codec, not the schema that creates those tables and not the surrounding
//! hydration lifecycle: the caller owns the SQLite connection and its schema (in Finch,
//! `finch-memory`'s `schema.sql`), and wires load/save into its own storage discipline.
//! `finch-memory` is the sole in-repo caller today, via its `RoutingMemTree` facade; the tree
//! is deliberately usable standalone for any embed-then-retrieve corpus.
//!
//! Ported from `~/repos/fractal-corpus-curation`'s `source/routing_tree.d` (validated there
//! through `EXPERIMENT_LOG.md` §54, plus §55/§56's later structural closes). See the tree
//! module's own documentation for the full provenance and the deliberately deferred pieces
//! (a learned per-node scorer seam, online axis refinement).

mod routing_tree;

pub use routing_tree::persistence::{
    load_routing_tree, mark_point_removed, save_dirty_nodes, save_point, write_dirty_nodes_within,
};
pub use routing_tree::{
    AdaptiveResult, AdaptiveTopKResult, BranchScorer, RoutingConfig, RoutingTree, TopKCandidate,
};

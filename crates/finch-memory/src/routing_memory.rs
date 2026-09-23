//! [`RoutingMemTree`]: a `MemTree`-shaped facade over [`RoutingTree`](crate::routing_tree), so
//! `MemorySystem` in `lib.rs` can hold this in place of `MemTree` with a minimal-diff call-site
//! swap rather than a rewrite of the surrounding persistence/hydration/quality machinery (already
//! confirmed algorithm-agnostic).
//!
//! Two real, disclosed behavioral differences from `MemTree`, both deliberate:
//!
//! - **No promotion.** A point's id is permanent from the moment `insert` assigns it, regardless
//!   of how the tree restructures around it later (`RoutingTree`'s own design property) -- unlike
//!   `MemTree`, where a leaf can be promoted into a new parent and its content moved to a
//!   different node id. There is nothing for a caller's `memory_sources` row to follow after the
//!   fact; `RoutingInsertEffect` has no `promotion` field at all, not an always-`None` one.
//! - **No importance-weighted re-ranking.** `MemTree::retrieve` boosts a Critical/High memory's
//!   score (x1.4/x1.2) so it can outrank a closer but less important match. This facade keeps the
//!   one filter that is a real content-safety decision (importance=0 / Discard is never returned)
//!   but does not yet replicate the boost -- a disclosed simplification, not a silent drop.

use crate::routing_tree::persistence as routing_persistence;
use crate::routing_tree::{AdaptiveTopKResult, RoutingConfig, RoutingTree};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;

/// Fixed, not random: every process that opens the same store must seed identical
/// candidate/stability-gate randomness for a freshly-built or freshly-reloaded tree to behave
/// identically to what produced the persisted structure it's continuing.
pub(crate) const FIXED_SEED: u64 = 0x46_49_4e_43_48; // "FINCH", ascii bytes -- a real constant, not a magic-looking one

pub(crate) type PointId = u64;

#[derive(Debug, Clone)]
pub(crate) struct PointMeta {
    pub text: String,
    pub importance: u8,
    pub created_at: i64,
}

/// What an insert did -- `MemTree::InsertEffect`'s shape, minus `promotion` (see module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoutingInsertEffect {
    pub point_id: PointId,
    pub deduplicated: bool,
}

pub(crate) struct RoutingMemTree {
    tree: RoutingTree,
    meta: HashMap<PointId, PointMeta>,
    /// Exact-text dedup index, mirroring `MemTree::find_leaf_by_text` -- content already stored
    /// gets its importance raised in place (if the new occurrence is more important) instead of a
    /// second point.
    text_index: HashMap<String, PointId>,
}

impl RoutingMemTree {
    pub(crate) fn new_with_dim(dim: usize) -> Self {
        Self {
            tree: RoutingTree::new(RoutingConfig::default(), dim, FIXED_SEED),
            meta: HashMap::new(),
            text_index: HashMap::new(),
        }
    }

    /// Rebuild from durable rows via `routing_tree::persistence::load_routing_tree`. `created_at`
    /// is not tracked by that function's own metadata tuple (text, importance only), so it is
    /// re-read here directly -- a second, small query rather than widening that function's return
    /// shape for one caller's own bookkeeping need.
    pub(crate) fn load(conn: &Connection, dim: usize) -> Result<Self> {
        let (tree, points) = routing_persistence::load_routing_tree(
            conn,
            RoutingConfig::default(),
            dim,
            FIXED_SEED,
        )?;
        let mut meta = HashMap::with_capacity(points.len());
        let mut text_index = HashMap::with_capacity(points.len());
        for (point_id, text, importance) in points {
            let pid = point_id as PointId;
            let created_at: i64 = conn.query_row(
                "SELECT created_at FROM routing_points WHERE point_id = ?1",
                [pid as i64],
                |r| r.get(0),
            )?;
            text_index.insert(text.clone(), pid);
            meta.insert(
                pid,
                PointMeta {
                    text,
                    importance,
                    created_at,
                },
            );
        }
        Ok(Self {
            tree,
            meta,
            text_index,
        })
    }

    pub(crate) fn insert_with_effect(
        &mut self,
        text: String,
        embedding: Vec<f32>,
        importance: u8,
        created_at: i64,
    ) -> RoutingInsertEffect {
        if let Some(&existing) = self.text_index.get(&text) {
            if let Some(m) = self.meta.get_mut(&existing) {
                if importance > m.importance {
                    m.importance = importance;
                    // The tree itself has no notion of importance, so nothing there needs marking
                    // dirty -- only the point-metadata side changed, and the caller persists that
                    // via `save_point` the same way it does for a brand new point.
                }
            }
            return RoutingInsertEffect {
                point_id: existing,
                deduplicated: true,
            };
        }
        let point_id = self.tree.insert(embedding) as PointId;
        self.text_index.insert(text.clone(), point_id);
        self.meta.insert(
            point_id,
            PointMeta {
                text,
                importance,
                created_at,
            },
        );
        RoutingInsertEffect {
            point_id,
            deduplicated: false,
        }
    }

    /// Top-k real candidates by cosine similarity, importance=0 (Discard) excluded -- the one
    /// `MemTree::retrieve` filter this facade keeps (module doc: no boost re-ranking yet).
    pub(crate) fn retrieve(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Vec<(PointId, String, f32)> {
        if top_k == 0 {
            return Vec::new();
        }
        // Oversample: some of the tree's real top-k may be importance=0 and get filtered below,
        // so ask for a wider pool before truncating to what the caller actually asked for.
        let AdaptiveTopKResult {
            top_k: candidates, ..
        } = self
            .tree
            .descend_adaptive_top_k(query_embedding, top_k + 16, false);
        candidates
            .into_iter()
            .filter_map(|c| {
                let pid = c.point_id as PointId;
                let m = self.meta.get(&pid)?;
                if m.importance == 0 {
                    return None;
                }
                Some((pid, m.text.clone(), c.cos as f32))
            })
            .take(top_k)
            .collect()
    }

    pub(crate) fn get_point(&self, point_id: PointId) -> Option<&PointMeta> {
        self.meta.get(&point_id)
    }

    /// Every live (non-removed) point's id, metadata, and canonical embedding -- for callers that
    /// need to scan the whole store (e.g. `conversation_summary`'s centroid windows), not the
    /// query-time hot path.
    pub(crate) fn iter_points(&self) -> impl Iterator<Item = (PointId, &PointMeta, &[f32])> {
        self.meta
            .iter()
            .map(|(&pid, m)| (pid, m, self.tree.embedding_of(pid as usize)))
    }

    pub(crate) fn remove(&mut self, point_id: PointId) -> anyhow::Result<()> {
        self.tree.remove_point(point_id as usize)?;
        if let Some(m) = self.meta.remove(&point_id) {
            self.text_index.remove(&m.text);
        }
        Ok(())
    }

    pub(crate) fn size(&self) -> usize {
        self.meta.len()
    }

    pub(crate) fn tree(&self) -> &RoutingTree {
        &self.tree
    }

    pub(crate) fn tree_mut(&mut self) -> &mut RoutingTree {
        &mut self.tree
    }
}

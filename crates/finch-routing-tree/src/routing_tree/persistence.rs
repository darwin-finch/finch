//! Save/load for [`RoutingTree`] against `routing_points`/`routing_nodes`/`routing_leaf_membership`
//! (the caller's schema). Reuses the dirty-tracking discipline the mechanism was built with (mark
//! on mutation, save only what changed, clear on hydrate) -- `RoutingTree::dirty_node_ids`/
//! `mark_persisted` are the same shape as the memory index's earlier dirty-nodes discipline,
//! deliberately.
//!
//! Canonical point content (text + embedding) is persisted separately from tree structure, in
//! `routing_points` -- `RoutingTree` itself has no notion of text, only embeddings and structure.
//! `bucket_deflated` (each leaf entry's per-node deflated residual cache) is NOT persisted: it is
//! deterministically recomputable from a point's canonical embedding plus the frozen
//! `(anchor, direction)` chain of every node it passes through, so [`load_routing_tree`]
//! reconstructs it once on load rather than storing a second, derived, embedding-sized copy per
//! leaf entry.
//!
//! This module is scoped to the tree's own durable rows alone. It opens no schema and owns no
//! hydration lifecycle: creating those tables is the caller's schema's job (in Finch,
//! `finch-memory`'s `schema.sql`), and wiring save/load into a hydration state machine
//! (`Loading`/`Degraded`/`Ready` progression, partial reads) belongs to the caller too.

use super::{normalize_in_place, projection, to_double, Node, RoutingConfig, RoutingTree};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};

fn encode_f32(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn decode_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn encode_f64(v: &[f64]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn decode_f64(bytes: &[u8]) -> Vec<f64> {
    bytes
        .chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// Persist a point's canonical content. Separate from tree structure -- called once per new
/// point, alongside (not instead of) [`save_dirty_nodes`].
pub fn save_point(
    conn: &Connection,
    point_id: usize,
    text: &str,
    embedding: &[f32],
    importance: u8,
    created_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO routing_points (point_id, text, embedding, importance, removed, created_at)
         VALUES (?1, ?2, ?3, ?4, 0, ?5)
         ON CONFLICT(point_id) DO UPDATE SET
             text = excluded.text,
             embedding = excluded.embedding,
             importance = excluded.importance",
        params![
            point_id as i64,
            text,
            encode_f32(embedding),
            importance as i64,
            created_at
        ],
    )
    .context("save_point: insert/update routing_points")?;
    Ok(())
}

pub fn mark_point_removed(conn: &Connection, point_id: usize) -> Result<()> {
    conn.execute(
        "UPDATE routing_points SET removed = 1 WHERE point_id = ?1",
        params![point_id as i64],
    )
    .context("mark_point_removed")?;
    Ok(())
}

/// Write every node `tree.dirty_node_ids()` names, then mark them persisted -- same
/// read-dirty/write/mark-persisted shape `MemTree`'s own `save_all_nodes_to_db` uses. For a leaf,
/// membership rows are fully resynced (delete then reinsert) rather than diffed, since a leaf's
/// bucket is small (bounded by `leaf_capacity` plus a few dual entries) and this keeps the write
/// path simple and obviously correct. A node that just converted from leaf to decision node (or
/// already was one) has any stale membership rows deleted unconditionally first -- harmless if
/// there were none.
pub fn save_dirty_nodes(tree: &mut RoutingTree, conn: &Connection) -> Result<()> {
    let dirty = tree.dirty_node_ids();
    if dirty.is_empty() {
        return Ok(());
    }
    let tx = conn
        .unchecked_transaction()
        .context("save_dirty_nodes: begin transaction")?;
    write_dirty_nodes_within(tree, &dirty, &tx)?;
    tx.commit().context("save_dirty_nodes: commit")?;
    tree.mark_persisted(&dirty);
    Ok(())
}

/// Writes every id in `dirty` against `conn` (a plain connection, or a `Transaction` via deref)
/// without opening its own transaction or marking anything persisted -- the piece
/// [`save_dirty_nodes`] wraps for standalone use, and what a caller composing a larger atomic
/// write (point content, tree structure, and its own provenance row in ONE transaction) calls
/// directly instead.
pub fn write_dirty_nodes_within(
    tree: &RoutingTree,
    dirty: &[usize],
    conn: &Connection,
) -> Result<()> {
    for &node_id in dirty {
        let is_leaf = tree.is_leaf(node_id);
        let parent = tree.parent_of(node_id);
        let left = tree.left_of(node_id);
        let right = tree.right_of(node_id);
        let anchor = tree.anchor_of(node_id);
        let direction = tree.direction_of(node_id);
        let real_centroid = tree.real_centroid_raw(node_id);

        conn.execute(
            "INSERT INTO routing_nodes
             (node_id, parent_id, is_leaf, left_id, right_id, anchor, direction, split_at_global_count, real_centroid, real_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(node_id) DO UPDATE SET
                 parent_id = excluded.parent_id,
                 is_leaf = excluded.is_leaf,
                 left_id = excluded.left_id,
                 right_id = excluded.right_id,
                 anchor = excluded.anchor,
                 direction = excluded.direction,
                 split_at_global_count = excluded.split_at_global_count,
                 real_centroid = excluded.real_centroid,
                 real_count = excluded.real_count",
            params![
                node_id as i64,
                parent.map(|p| p as i64),
                is_leaf as i64,
                left.map(|l| l as i64),
                right.map(|r| r as i64),
                if anchor.is_empty() { None } else { Some(encode_f64(anchor)) },
                if direction.is_empty() { None } else { Some(encode_f64(direction)) },
                tree.split_at_global_count_of(node_id) as i64,
                encode_f64(real_centroid),
                tree.real_count_of(node_id) as i64,
            ],
        )
        .context("write_dirty_nodes_within: upsert routing_nodes")?;

        conn.execute(
            "DELETE FROM routing_leaf_membership WHERE leaf_node_id = ?1",
            params![node_id as i64],
        )
        .context("write_dirty_nodes_within: clear stale membership")?;
        if is_leaf {
            for (point_id, is_dual, divergence_node_id) in tree.membership_of(node_id) {
                conn.execute(
                    "INSERT INTO routing_leaf_membership (leaf_node_id, point_id, is_dual, divergence_node_id) VALUES (?1, ?2, ?3, ?4)",
                    params![node_id as i64, point_id as i64, is_dual as i64, divergence_node_id.map(|d| d as i64)],
                )
                .context("write_dirty_nodes_within: insert membership row")?;
            }
        }
    }
    Ok(())
}

struct LoadedMembership {
    leaf_node_id: usize,
    point_id: usize,
    is_dual: bool,
    divergence_node_id: Option<usize>,
}

/// Rebuild a full [`RoutingTree`] from durable rows. Returns the tree plus each surviving point's
/// `(point_id, text, importance)` -- `RoutingTree` itself has no notion of text, so the caller owns
/// that mapping.
pub fn load_routing_tree(
    conn: &Connection,
    cfg: RoutingConfig,
    dim: usize,
    seed: u64,
) -> Result<(RoutingTree, Vec<(usize, String, u8)>)> {
    let tx = conn
        .unchecked_transaction()
        .context("load_routing_tree: begin read transaction")?;
    let loaded = load_routing_tree_within(&tx, cfg, dim, seed)?;
    tx.commit()
        .context("load_routing_tree: commit read transaction")?;
    Ok(loaded)
}

/// Rebuild a full [`RoutingTree`] through an existing SQLite transaction.
///
/// Every query in the load observes that transaction's one database snapshot. Callers that are
/// already inside a larger read or write transaction use this entry point so the loader neither
/// opens a nested transaction nor combines rows from different commits.
pub fn load_routing_tree_within(
    conn: &Connection,
    cfg: RoutingConfig,
    dim: usize,
    seed: u64,
) -> Result<(RoutingTree, Vec<(usize, String, u8)>)> {
    let node_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM routing_nodes", [], |r| r.get(0))
        .context("load_routing_tree: count routing_nodes")?;
    #[cfg(test)]
    tests::pause_after_node_count(conn);
    let spherical = cfg.spherical_mode;
    let tree = RoutingTree::new(cfg, dim, seed);
    if node_count == 0 {
        let point_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM routing_points", [], |r| r.get(0))
            .context("load_routing_tree: count routing_points without nodes")?;
        let membership_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM routing_leaf_membership", [], |r| {
                r.get(0)
            })
            .context("load_routing_tree: count routing_leaf_membership without nodes")?;
        anyhow::ensure!(
            point_count == 0 && membership_count == 0,
            "load_routing_tree: routing_nodes is empty but the store still contains {point_count} routing_points rows and {membership_count} routing_leaf_membership rows"
        );
        return Ok((tree, Vec::new()));
    }

    // Points first -- node reconstruction needs each point's canonical embedding to rebuild
    // bucket_deflated by replaying descent.
    let mut point_stmt = conn.prepare("SELECT point_id, text, embedding, importance, removed FROM routing_points ORDER BY point_id").context("load_routing_tree: prepare points")?;
    let mut points: Vec<Vec<f32>> = Vec::new();
    let mut removed_flag: Vec<bool> = Vec::new();
    let mut metadata: Vec<(usize, String, u8)> = Vec::new();
    let rows = point_stmt
        .query_map([], |r| {
            let point_id: i64 = r.get(0)?;
            let text: String = r.get(1)?;
            let embedding_bytes: Vec<u8> = r.get(2)?;
            let importance: i64 = r.get(3)?;
            let removed: i64 = r.get(4)?;
            Ok((
                point_id as usize,
                text,
                embedding_bytes,
                importance as u8,
                removed != 0,
            ))
        })
        .context("load_routing_tree: query points")?;
    for row in rows {
        let (point_id, text, embedding_bytes, importance, removed) =
            row.context("load_routing_tree: read point row")?;
        anyhow::ensure!(point_id == points.len(), "load_routing_tree: routing_points.point_id must be contiguous from 0, got {point_id} at position {}", points.len());
        anyhow::ensure!(
            embedding_bytes.len() % std::mem::size_of::<f32>() == 0,
            "load_routing_tree: routing point {point_id} embedding blob has {} bytes, which is not an exact multiple of {} bytes per f32",
            embedding_bytes.len(),
            std::mem::size_of::<f32>()
        );
        let embedding = decode_f32(&embedding_bytes);
        anyhow::ensure!(
            embedding.len() == dim,
            "load_routing_tree: routing point {point_id} embedding has {} values ({} bytes), expected {dim} values",
            embedding.len(),
            embedding_bytes.len()
        );
        if !removed {
            metadata.push((point_id, text, importance));
        }
        points.push(embedding);
        removed_flag.push(removed);
    }

    // Every persisted embedding was written by the same fixed `dim` this store was built and
    // split under (`RoutingTree::insert` never validates `point.len()` against `self.dim` itself
    // -- see `RoutingTree::insert`'s own doc). A caller reopening this store with a DIFFERENT
    // `dim` (the composition root's embedding engine changed since the store was built -- e.g.
    // Finch deliberately downloads a neural embedding model in the background "for the next
    // restart", swapping the hashed-n-gram fallback for it on a later run against the SAME
    // on-disk store) must be refused here, before any node or membership is touched: this
    // function's own replay-descent below only ever compares a node's persisted `anchor`/
    // `direction` against that SAME store's persisted embeddings, so it stays internally
    // consistent and would not itself catch a `dim` mismatch -- the resulting tree's `self.dim`
    // field would simply stop matching every persisted node's `real_centroid` and every persisted
    // decision node's `anchor`/`direction`, and the next real insert would index one of those
    // shorter, persisted arrays with `self.dim` and panic (issue #1384: "index out of bounds: the
    // len is 0 but the index is 0" at this crate's `routing_tree.rs:181`, `projection`, reached
    // from a background hydration/indexing worker -- `insert_into`'s own `real_centroid` update
    // loop, which runs on every visited node before `projection` is ever reached, panics the same
    // way for other old/new dimension combinations).
    // Node structure.
    let mut node_stmt = conn
        .prepare("SELECT node_id, parent_id, is_leaf, left_id, right_id, anchor, direction, split_at_global_count, real_centroid, real_count FROM routing_nodes ORDER BY node_id")
        .context("load_routing_tree: prepare nodes")?;
    let mut nodes: Vec<Node> = Vec::new();
    let rows = node_stmt
        .query_map([], |r| {
            let node_id: i64 = r.get(0)?;
            let parent_id: Option<i64> = r.get(1)?;
            let is_leaf: i64 = r.get(2)?;
            let left_id: Option<i64> = r.get(3)?;
            let right_id: Option<i64> = r.get(4)?;
            let anchor: Option<Vec<u8>> = r.get(5)?;
            let direction: Option<Vec<u8>> = r.get(6)?;
            let split_at_global_count: i64 = r.get(7)?;
            let real_centroid: Vec<u8> = r.get(8)?;
            let real_count: i64 = r.get(9)?;
            Ok((
                node_id as usize,
                parent_id.map(|p| p as usize),
                is_leaf != 0,
                left_id.map(|l| l as usize),
                right_id.map(|r| r as usize),
                anchor,
                direction,
                split_at_global_count as usize,
                real_centroid,
                real_count as usize,
            ))
        })
        .context("load_routing_tree: query nodes")?;
    for row in rows {
        let (
            node_id,
            parent,
            is_leaf,
            left,
            right,
            anchor,
            direction,
            split_at_global_count,
            real_centroid,
            real_count,
        ) = row.context("load_routing_tree: read node row")?;
        anyhow::ensure!(node_id == nodes.len(), "load_routing_tree: routing_nodes.node_id must be contiguous from 0, got {node_id} at position {}", nodes.len());
        for (field, blob) in [
            ("anchor", anchor.as_deref()),
            ("direction", direction.as_deref()),
        ] {
            if let Some(blob) = blob {
                anyhow::ensure!(
                    blob.len() % std::mem::size_of::<f64>() == 0,
                    "load_routing_tree: routing node {node_id} {field} blob has {} bytes, which is not an exact multiple of {} bytes per f64",
                    blob.len(),
                    std::mem::size_of::<f64>()
                );
            }
        }
        anyhow::ensure!(
            real_centroid.len() % std::mem::size_of::<f64>() == 0,
            "load_routing_tree: routing node {node_id} real_centroid blob has {} bytes, which is not an exact multiple of {} bytes per f64",
            real_centroid.len(),
            std::mem::size_of::<f64>()
        );
        let mut node = Node::leaf();
        node.is_leaf = is_leaf;
        node.left = left;
        node.right = right;
        node.anchor = anchor.map(|b| decode_f64(&b)).unwrap_or_default();
        node.direction = direction.map(|b| decode_f64(&b)).unwrap_or_default();
        node.split_at_global_count = split_at_global_count;
        node.real_centroid = decode_f64(&real_centroid);
        node.real_count = real_count;
        node.parent = parent;

        // A decision node's `anchor`/`direction` are set exactly once, together, at split time
        // (`try_split`, both always length `self.dim` by construction -- see `routing_tree.rs`),
        // and frozen forever after. A decision node whose persisted `anchor`/`direction` is
        // missing or a different length than this store's own `dim` (most commonly a fully
        // degenerate, zero-length pair -- issue #1384's actual reported shape, distinct from the
        // whole-store dimension mismatch #1398 already refuses above) can never have been produced
        // by this crate's own write path; loading it anyway leaves a tree that looks fully hydrated
        // but panics the first time descent or insert reaches this node and `projection`
        // (`routing_tree.rs:181`) indexes a shorter anchor/direction against a full-length point --
        // on whatever thread happens to be inserting at the time, a background hydration/indexing
        // worker in production. Refuse up front instead, before any node is wired into the tree.
        anyhow::ensure!(
            node.is_leaf || (node.anchor.len() == dim && node.direction.len() == dim),
            "load_routing_tree: decision node {node_id} has a degenerate anchor/direction (anchor \
             len {}, direction len {}, expected {dim}) -- this store has a corrupted routing node \
             that predates this check; refusing to load a tree that would panic in `projection` on \
             its next insert or descent (issue #1384)",
            node.anchor.len(),
            node.direction.len(),
        );
        anyhow::ensure!(
            node.real_centroid.len() == dim,
            "load_routing_tree: routing node {node_id} real_centroid has {} values ({} bytes), expected {dim} values",
            node.real_centroid.len(),
            real_centroid.len()
        );
        nodes.push(node);
    }

    anyhow::ensure!(
        !nodes.is_empty(),
        "load_routing_tree: routing_points exist but routing_nodes is empty"
    );
    for (node_id, node) in nodes.iter().enumerate() {
        if let Some(parent_id) = node.parent {
            anyhow::ensure!(
                parent_id < nodes.len(),
                "load_routing_tree: node {node_id} references parent {parent_id}, but only {} routing_nodes rows exist",
                nodes.len()
            );
        }
        for (side, child_id) in [("left", node.left), ("right", node.right)] {
            if let Some(child_id) = child_id {
                anyhow::ensure!(
                    child_id < nodes.len(),
                    "load_routing_tree: node {node_id} references {side} child {child_id}, but only {} routing_nodes rows exist",
                    nodes.len()
                );
            }
        }
        anyhow::ensure!(
            node.is_leaf || (node.left.is_some() && node.right.is_some()),
            "load_routing_tree: decision node {node_id} must have both left and right children"
        );
    }

    // Leaf membership.
    let mut member_stmt = conn.prepare("SELECT leaf_node_id, point_id, is_dual, divergence_node_id FROM routing_leaf_membership").context("load_routing_tree: prepare membership")?;
    let memberships: Vec<LoadedMembership> = member_stmt
        .query_map([], |r| {
            let leaf_node_id: i64 = r.get(0)?;
            let point_id: i64 = r.get(1)?;
            let is_dual: i64 = r.get(2)?;
            let divergence_node_id: Option<i64> = r.get(3)?;
            Ok(LoadedMembership {
                leaf_node_id: leaf_node_id as usize,
                point_id: point_id as usize,
                is_dual: is_dual != 0,
                divergence_node_id: divergence_node_id.map(|d| d as usize),
            })
        })
        .context("load_routing_tree: query membership")?
        .collect::<rusqlite::Result<_>>()
        .context("load_routing_tree: read membership rows")?;

    // Reconstruct each leaf's bucket_deflated by replaying descent through the now-fully-loaded
    // node structure, using each point's canonical embedding -- the deterministic recomputation
    // `bucket_deflated` exists to avoid persisting redundantly (module doc). A dual entry replays
    // normally down to its own `divergence_node_id`, takes the OTHER child there once, then
    // continues normally (a dual copy never spawns further dual copies, matching insertion).
    let mut membership_tuples: Vec<(usize, usize, bool, Option<usize>)> =
        Vec::with_capacity(memberships.len());
    for m in &memberships {
        anyhow::ensure!(
            m.point_id < points.len(),
            "load_routing_tree: membership references point {} but only {} routing_points rows exist",
            m.point_id,
            points.len()
        );
        anyhow::ensure!(
            m.leaf_node_id < nodes.len(),
            "load_routing_tree: membership references leaf node {} but only {} routing_nodes rows exist",
            m.leaf_node_id,
            nodes.len()
        );
        anyhow::ensure!(
            nodes[m.leaf_node_id].is_leaf,
            "load_routing_tree: membership references node {} as a leaf, but that node is a decision node",
            m.leaf_node_id
        );
        if let Some(divergence_node_id) = m.divergence_node_id {
            anyhow::ensure!(
                divergence_node_id < nodes.len(),
                "load_routing_tree: membership references divergence node {divergence_node_id} but only {} routing_nodes rows exist",
                nodes.len()
            );
            anyhow::ensure!(
                !nodes[divergence_node_id].is_leaf,
                "load_routing_tree: membership for point {} references leaf node {divergence_node_id} as its dual divergence decision",
                m.point_id
            );
        }
        anyhow::ensure!(
            m.is_dual == m.divergence_node_id.is_some(),
            "load_routing_tree: membership for point {} has inconsistent dual flag ({}) and divergence node ({:?})",
            m.point_id,
            m.is_dual,
            m.divergence_node_id
        );
        let embedding = &points[m.point_id];
        let mut x = to_double(embedding);
        if spherical {
            normalize_in_place(&mut x);
        }
        let mut cur = 0usize; // root is always node 0
        let mut traversed = 0usize;
        let mut encountered_divergence = false;
        while !nodes[cur].is_leaf {
            traversed += 1;
            anyhow::ensure!(
                traversed <= nodes.len(),
                "load_routing_tree: membership for point {} encountered a cycle before reaching leaf {}",
                m.point_id,
                m.leaf_node_id
            );
            let anchor = &nodes[cur].anchor;
            let dir = &nodes[cur].direction;
            let proj = projection(&x, anchor, dir);
            let favored_right = proj >= 0.0;
            let at_divergence = m.is_dual && Some(cur) == m.divergence_node_id;
            encountered_divergence |= at_divergence;
            let go_right = if at_divergence {
                !favored_right
            } else {
                favored_right
            };
            for i in 0..x.len() {
                x[i] -= proj * dir[i];
            }
            if spherical {
                normalize_in_place(&mut x);
            }
            cur = if go_right {
                nodes[cur]
                    .right
                    .ok_or_else(|| anyhow::anyhow!(
                        "load_routing_tree: decision node {cur} has no right child while replaying point {}",
                        m.point_id
                    ))?
            } else {
                nodes[cur]
                    .left
                    .ok_or_else(|| anyhow::anyhow!(
                        "load_routing_tree: decision node {cur} has no left child while replaying point {}",
                        m.point_id
                    ))?
            };
            anyhow::ensure!(
                cur < nodes.len(),
                "load_routing_tree: membership for point {} traversed to missing node {cur}",
                m.point_id
            );
        }
        if cur != m.leaf_node_id {
            tracing::warn!(
                point_id = m.point_id,
                recorded_leaf = m.leaf_node_id,
                repaired_leaf = cur,
                "load_routing_tree: auto-repairing membership for point {}: recorded leaf was {}, traversal reached leaf {cur}",
                m.point_id,
                m.leaf_node_id
            );
        }
        anyhow::ensure!(
            !m.is_dual || encountered_divergence,
            "load_routing_tree: dual membership for point {} names divergence decision {:?}, but replay never encountered that decision on the path to leaf {}",
            m.point_id,
            m.divergence_node_id,
            m.leaf_node_id
        );
        let target_leaf = cur;
        nodes[target_leaf].bucket_ids.push(m.point_id);
        nodes[target_leaf].bucket_deflated.push(x);
        nodes[target_leaf].bucket_is_dual.push(m.is_dual);
        membership_tuples.push((target_leaf, m.point_id, m.is_dual, m.divergence_node_id));
    }

    let mut tree = tree;
    tree.install_loaded_state(nodes, points, removed_flag, &membership_tuples);
    Ok((tree, metadata))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing_tree::splitmix64_uniform_half;
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Condvar, LazyLock, Mutex};
    use tempfile::NamedTempFile;

    #[derive(Default)]
    struct LoadPause {
        state: Mutex<(bool, bool)>,
        changed: Condvar,
    }

    impl LoadPause {
        fn pause(&self) {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.0 = true;
            self.changed.notify_all();
            while !state.1 {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }

        fn wait_until_reached(&self) -> bool {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (state, _) = self
                .changed
                .wait_timeout_while(state, std::time::Duration::from_secs(5), |state| !state.0)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.0
        }

        fn release(&self) {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.1 = true;
            self.changed.notify_all();
        }
    }

    static LOAD_PAUSES: LazyLock<Mutex<HashMap<std::path::PathBuf, Arc<LoadPause>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    fn register_load_pause(path: std::path::PathBuf) -> Arc<LoadPause> {
        let pause = Arc::new(LoadPause::default());
        let replaced = LOAD_PAUSES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.clone(), Arc::clone(&pause));
        assert!(
            replaced.is_none(),
            "a routing-tree load pause is already registered for {}",
            path.display()
        );
        pause
    }

    pub(super) fn pause_after_node_count(conn: &Connection) {
        let Some(path) = conn.path().map(std::path::PathBuf::from) else {
            return;
        };
        let pause = LOAD_PAUSES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&path);
        if let Some(pause) = pause {
            pause.pause();
        }
    }

    fn expect_load_error(
        result: Result<(RoutingTree, Vec<(usize, String, u8)>)>,
        message: &str,
    ) -> anyhow::Error {
        match result {
            Err(error) => error,
            Ok(_) => panic!("{message}"),
        }
    }

    const DIM: usize = 16;

    fn test_corpus(n_per_cluster: usize) -> Vec<Vec<f32>> {
        let clusters = 4usize;
        let mut points = Vec::with_capacity(n_per_cluster * clusters);
        let mut state = 0xC0FFEE_u64;
        for c in 0..clusters {
            for _ in 0..n_per_cluster {
                let mut v = vec![0.0f32; DIM];
                v[c % DIM] = 3.0;
                for slot in v.iter_mut() {
                    *slot += splitmix64_uniform_half(&mut state) as f32 * 0.6;
                }
                points.push(v);
            }
        }
        points
    }

    fn test_corpus_build_heldout(
        n_build_per_cluster: usize,
        n_heldout_per_cluster: usize,
    ) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
        let clusters = 4usize;
        let per_cluster = n_build_per_cluster + n_heldout_per_cluster;
        let all = test_corpus(per_cluster);
        let mut build = Vec::new();
        let mut heldout = Vec::new();
        for c in 0..clusters {
            let base = c * per_cluster;
            build.extend_from_slice(&all[base..base + n_build_per_cluster]);
            heldout.extend_from_slice(&all[base + n_build_per_cluster..base + per_cluster]);
        }
        (build, heldout)
    }

    #[test]
    fn test_save_then_load_round_trips_structure_and_centroids() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("test_schema.sql")).unwrap();

        let points = test_corpus(20);
        let mut tree = RoutingTree::new(RoutingConfig::default(), DIM, 7);
        for (i, p) in points.iter().enumerate() {
            let pid = tree.insert(p.clone());
            assert_eq!(pid, i);
            save_point(
                &conn,
                pid,
                &format!("memory {pid}"),
                p,
                1,
                1000 + pid as i64,
            )
            .unwrap();
        }
        save_dirty_nodes(&mut tree, &conn).unwrap();
        assert!(
            tree.dirty_node_ids().is_empty(),
            "save_dirty_nodes must clear the dirty set on success"
        );

        let (loaded, metadata) =
            load_routing_tree(&conn, RoutingConfig::default(), DIM, 7).unwrap();
        assert_eq!(metadata.len(), points.len());
        assert_eq!(loaded.node_count(), tree.node_count());

        for id in 0..tree.node_count() {
            assert_eq!(
                loaded.is_leaf(id),
                tree.is_leaf(id),
                "node {id}: is_leaf mismatch after reload"
            );
            assert_eq!(
                loaded.left_of(id),
                tree.left_of(id),
                "node {id}: left mismatch after reload"
            );
            assert_eq!(
                loaded.right_of(id),
                tree.right_of(id),
                "node {id}: right mismatch after reload"
            );
            assert_eq!(
                loaded.real_count_of(id),
                tree.real_count_of(id),
                "node {id}: real_count mismatch after reload"
            );
            let orig_centroid = tree.centroid_of(id);
            let loaded_centroid = loaded.centroid_of(id);
            for d in 0..DIM {
                assert!(
                    (orig_centroid[d] - loaded_centroid[d]).abs() < 1e-9,
                    "node {id} dim {d}: real_centroid mismatch after reload"
                );
            }
            if !tree.is_leaf(id) {
                let orig_dir = tree.direction_of(id);
                let loaded_dir = loaded.direction_of(id);
                for d in 0..DIM {
                    assert!(
                        (orig_dir[d] - loaded_dir[d]).abs() < 1e-12,
                        "node {id} dim {d}: direction mismatch after reload"
                    );
                }
            } else {
                let mut orig_bucket = tree.bucket_of(id).to_vec();
                let mut loaded_bucket = loaded.bucket_of(id).to_vec();
                orig_bucket.sort_unstable();
                loaded_bucket.sort_unstable();
                assert_eq!(
                    orig_bucket, loaded_bucket,
                    "leaf {id}: bucket membership mismatch after reload"
                );
            }
        }
    }

    #[test]
    fn test_loaded_tree_matches_original_on_adaptive_search() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("test_schema.sql")).unwrap();

        let (build, heldout) = test_corpus_build_heldout(20, 5);
        let mut tree = RoutingTree::new(RoutingConfig::default(), DIM, 7);
        for p in &build {
            let pid = tree.insert(p.clone());
            save_point(
                &conn,
                pid,
                &format!("memory {pid}"),
                p,
                1,
                1000 + pid as i64,
            )
            .unwrap();
        }
        save_dirty_nodes(&mut tree, &conn).unwrap();

        let (loaded, _) = load_routing_tree(&conn, RoutingConfig::default(), DIM, 7).unwrap();

        for q in &heldout {
            let orig = tree.descend_adaptive(q, None, false);
            let reloaded = loaded.descend_adaptive(q, None, false);
            assert_eq!(
                orig.best_point_id, reloaded.best_point_id,
                "a reloaded tree must answer descend_adaptive identically to the original"
            );
            assert!((orig.best_cos - reloaded.best_cos).abs() < 1e-9);
        }
    }

    #[test]
    fn test_loaded_tree_accepts_further_inserts_without_panicking() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("test_schema.sql")).unwrap();

        let points = test_corpus(15);
        let mut tree = RoutingTree::new(RoutingConfig::default(), DIM, 7);
        for p in &points {
            let pid = tree.insert(p.clone());
            save_point(
                &conn,
                pid,
                &format!("memory {pid}"),
                p,
                1,
                1000 + pid as i64,
            )
            .unwrap();
        }
        save_dirty_nodes(&mut tree, &conn).unwrap();

        let (mut loaded, _) = load_routing_tree(&conn, RoutingConfig::default(), DIM, 7).unwrap();
        let more = test_corpus(25); // a fresh, larger draw so some points overflow existing leaves
        for p in more.iter().skip(50) {
            loaded.insert(p.clone());
        }
        for id in 0..loaded.node_count() {
            if !loaded.is_leaf(id) {
                assert!(
                    loaded.left_of(id).is_some() && loaded.right_of(id).is_some(),
                    "node {id}: decision node missing a child after post-reload inserts"
                );
            }
        }
    }

    #[test]
    fn test_dual_insert_membership_reconstructs_correctly_after_reload() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("test_schema.sql")).unwrap();

        let points = test_corpus(15);
        let mut cfg = RoutingConfig::default();
        cfg.dual_insert_threshold = 0.5; // generous, matches routing_tree's own dual-insert test
        let mut tree = RoutingTree::new(cfg.clone(), DIM, 7);
        for (i, p) in points.iter().enumerate() {
            let pid = tree.insert(p.clone());
            assert_eq!(pid, i);
            save_point(
                &conn,
                pid,
                &format!("memory {pid}"),
                p,
                1,
                1000 + pid as i64,
            )
            .unwrap();
        }
        save_dirty_nodes(&mut tree, &conn).unwrap();

        let dual_entries_exist = (0..points.len()).any(|pid| !tree.dual_entries_of[pid].is_empty());
        assert!(dual_entries_exist, "this corpus/threshold combination should produce at least one dual entry -- test isn't exercising what it claims to");

        let (loaded, _) = load_routing_tree(&conn, cfg, DIM, 7).unwrap();
        assert_eq!(loaded.node_count(), tree.node_count());

        for id in 0..tree.node_count() {
            if tree.is_leaf(id) {
                let mut orig_bucket = tree.bucket_of(id).to_vec();
                let mut loaded_bucket = loaded.bucket_of(id).to_vec();
                orig_bucket.sort_unstable();
                loaded_bucket.sort_unstable();
                assert_eq!(
                    orig_bucket, loaded_bucket,
                    "leaf {id}: dual-insert-inclusive bucket membership mismatch after reload"
                );
            }
            assert_eq!(
                loaded.real_count_of(id),
                tree.real_count_of(id),
                "node {id}: real_count (which counts dual visits too) mismatch after reload"
            );
        }

        // The reconstructed bucket_deflated must be usable for a real split: force one more
        // insert into an existing leaf and confirm it doesn't panic or corrupt structure.
        let mut loaded = loaded;
        loaded.insert(points[0].clone());
    }

    #[test]
    fn test_removed_point_is_not_returned_in_metadata_after_reload() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("test_schema.sql")).unwrap();

        let points = test_corpus(15);
        let mut tree = RoutingTree::new(RoutingConfig::default(), DIM, 7);
        for (i, p) in points.iter().enumerate() {
            let pid = tree.insert(p.clone());
            assert_eq!(pid, i);
            save_point(
                &conn,
                pid,
                &format!("memory {pid}"),
                p,
                1,
                1000 + pid as i64,
            )
            .unwrap();
        }
        save_dirty_nodes(&mut tree, &conn).unwrap();
        tree.remove_point(3).unwrap();
        save_dirty_nodes(&mut tree, &conn).unwrap();
        mark_point_removed(&conn, 3).unwrap();

        let (_, metadata) = load_routing_tree(&conn, RoutingConfig::default(), DIM, 7).unwrap();
        assert!(
            !metadata.iter().any(|(pid, _, _)| *pid == 3),
            "a removed point must not appear in reload metadata"
        );
        assert_eq!(metadata.len(), points.len() - 1);
    }

    /// Regression for issue #1384: "Startup panic in RoutingTree::projection on a fresh, empty
    /// memory index (dimension-0 anchor/direction)".
    ///
    /// A store is built and split at one embedding dimension, then reloaded with a DIFFERENT
    /// dimension -- exactly what happens across an ordinary `finch` restart in production once a
    /// background-downloaded neural embedding model becomes available and
    /// `select_memory_embedding_engine` switches away from the hashed-n-gram fallback it used to
    /// build the existing store (`src/cli/repl.rs`'s own "for the *next* restart" comment: the
    /// download deliberately does not apply mid-session, but the *next* session reopens the SAME
    /// on-disk store under a different dimension).
    ///
    /// Before the fix, `load_routing_tree` accepted any caller-supplied `dim` unconditionally.
    /// Hydration itself does not touch `self.dim` -- the replay-descent it does only ever compares
    /// a node's persisted `anchor`/`direction` against that same store's own persisted embeddings,
    /// so it stayed internally consistent and returned `Ok` even though the resulting tree's
    /// `self.dim` field no longer matched its own persisted node geometry (`anchor`/`direction`,
    /// AND every node's `real_centroid`, all persisted at the OLD dimension). The panic only
    /// surfaced on the NEXT real insert: `insert_into` indexes every visited node's persisted
    /// `real_centroid` by `self.dim` before it ever reaches a decision node's `anchor`/`direction`
    /// via `projection`, so which of the two panics first (`insert_into`, `routing_tree.rs` around
    /// its real_centroid update loop, or `projection`, `routing_tree.rs:181`) depends on the exact
    /// old/new dimensions -- both are the same defect (a persisted per-node array whose length no
    /// longer agrees with `self.dim`), and this test (a larger new `dim`) and #1384's original
    /// report (a persisted length of 0) happen to hit different ones, verified live below.
    ///
    /// This test reproduces the real defect at the production boundary: real SQLite persistence, a
    /// real split tree (`node_count() > 1` is asserted below so the test cannot pass vacuously
    /// against a still-unsplit, single-leaf tree), and a real subsequent insert -- not a synthetic
    /// unit call into `projection` with hand-built mismatched slices. Before the fix, the `Ok` arm
    /// below is taken and its `insert` call panics, failing this test. After the fix,
    /// `load_routing_tree` itself returns `Err` and the `Ok` arm -- the only place that could ever
    /// panic -- is unreachable.
    #[test]
    fn test_reloading_a_store_at_a_different_embedding_dimension_never_panics_on_a_later_insert() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("test_schema.sql")).unwrap();

        // Session 1: build and persist a real, split tree at DIM.
        let points = test_corpus(20);
        let mut tree = RoutingTree::new(RoutingConfig::default(), DIM, 7);
        for (i, p) in points.iter().enumerate() {
            let pid = tree.insert(p.clone());
            assert_eq!(pid, i);
            save_point(
                &conn,
                pid,
                &format!("memory {pid}"),
                p,
                1,
                1000 + pid as i64,
            )
            .unwrap();
        }
        save_dirty_nodes(&mut tree, &conn).unwrap();
        assert!(
            tree.node_count() > 1,
            "test setup: expected at least one real split (a decision node with real anchor/\
             direction) before reloading at a different dimension, got node_count={} -- without a \
             split this test would pass vacuously (nothing to route a mismatched insert through)",
            tree.node_count()
        );

        // Session 2: reopen the SAME store with a LARGER dimension -- a real embedding engine
        // change between restarts, the shape #1384's report traces back to.
        let bigger_dim = DIM * 2;
        match load_routing_tree(&conn, RoutingConfig::default(), bigger_dim, 7) {
            Err(_) => {
                // The fix: refused up front, before any node or membership was touched. Nothing
                // was reloaded, so there is nothing left that could panic on insert -- this is the
                // fixed behavior and the test is done.
            }
            Ok((mut reloaded, _)) => {
                // Pre-fix (or if the dimension guard in `load_routing_tree` is ever weakened):
                // `reloaded.dim` is `bigger_dim`, but every persisted node's `real_centroid` (and
                // every persisted decision node's `anchor`/`direction`) is still `DIM` long. The
                // root already split (asserted above), so this insert immediately visits it and
                // panics -- `insert_into`'s real_centroid update loop reaches `self.dim` (32)
                // against a `DIM`-long (16) persisted `real_centroid` before the descent even
                // reaches `projection`, confirmed live: "index out of bounds: the len is 16 but
                // the index is 16". The exact site (this loop vs. `projection`, `routing_tree.rs:
                // 181`, #1384's own report) depends on the old/new dimensions -- both are the same
                // defect this fix closes.
                reloaded.insert(vec![0.5_f32; bigger_dim]);
            }
        }
    }

    /// Regression for the #1384 REOPENING: live re-testing found the original panic still
    /// reproduced 5/5 on "fresh workspaces", but every one of those actually shared one
    /// pre-existing, already-corrupted `~/.finch/memory.db` (memory stores are keyed by `$HOME`,
    /// not by the working directory a "fresh workspace" repro changes into) -- not a genuinely
    /// fresh, empty store. Reading #1398's fix closely shows it addresses a DIFFERENT mechanism: a
    /// whole-store `dim` mismatch, checked against the first loaded `routing_points` row. That
    /// check does nothing here, because this store's `dim` is perfectly consistent with every
    /// point's own embedding -- only ONE already-corrupted decision node's persisted
    /// `anchor`/`direction` is degenerate (zero-length), a persisted row this crate's own write
    /// path (`try_split`, always sets both together to length `self.dim`) could never have
    /// produced from a genuinely fresh store; the true creation-time root cause of the corrupted
    /// row itself was not identified.
    ///
    /// This test reproduces the real defect at the production boundary: real SQLite persistence, a
    /// real split tree (`node_count() > 1` asserted below), then the SAME degenerate shape live
    /// reports actually hit -- a zero-length `anchor`/`direction` written directly into
    /// `routing_nodes` for a real decision node -- not a synthetic unit call into `projection` with
    /// a hand-built empty slice. Before the fix, `load_routing_tree` accepts this row unchanged
    /// (`Option<Vec<u8>>::None` decodes to an empty `Vec` via `unwrap_or_default`) and returns
    /// `Ok`; the very next `insert` that reaches this node panics in `projection`
    /// (`routing_tree.rs:181`) exactly as #1384 reports: "index out of bounds: the len is 0 but
    /// the index is 0". After the fix, `load_routing_tree` itself returns `Err` up front and the
    /// panicking `insert` is never reached.
    #[test]
    fn test_loading_a_decision_node_with_a_degenerate_anchor_fails_closed_instead_of_panicking() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("test_schema.sql")).unwrap();

        let points = test_corpus(20);
        let mut tree = RoutingTree::new(RoutingConfig::default(), DIM, 7);
        for (i, p) in points.iter().enumerate() {
            let pid = tree.insert(p.clone());
            assert_eq!(pid, i);
            save_point(
                &conn,
                pid,
                &format!("memory {pid}"),
                p,
                1,
                1000 + pid as i64,
            )
            .unwrap();
        }
        save_dirty_nodes(&mut tree, &conn).unwrap();
        assert!(
            !tree.is_leaf(0),
            "test setup: expected the root to have split into a real decision node before \
             corrupting it, got node_count={} -- without a split there is no decision node to \
             corrupt and this test would pass vacuously",
            tree.node_count()
        );

        // Directly corrupt the root's persisted anchor/direction to NULL/zero-length -- the exact
        // on-disk shape live reports hit, independent of how it was actually produced (an older,
        // already-superseded write path, or on-disk damage; not reproduced here since the true
        // creation-time cause is a separate, unresolved question from this defensive fix).
        conn.execute(
            "UPDATE routing_nodes SET anchor = NULL, direction = NULL WHERE node_id = 0",
            [],
        )
        .unwrap();

        // Same `dim` the store was built under -- #1398's whole-store dimension check does not and
        // should not fire here; only the new per-node degenerate-anchor check should.
        let result = load_routing_tree(&conn, RoutingConfig::default(), DIM, 7);
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!(
                "load_routing_tree must refuse a decision node with a degenerate anchor/direction \
                 instead of returning Ok and deferring the panic to the next insert or descent"
            ),
        };
        let message = format!("{err:#}");
        assert!(
            message.contains("node 0") && message.contains("1384"),
            "error must name the corrupted node id and reference issue #1384 so an operator can \
             act on it, got: {message}"
        );
    }

    #[test]
    fn test_load_snapshot_does_not_mix_rows_from_a_commit_between_queries() {
        let temp = NamedTempFile::new().unwrap();
        let writer = Connection::open(temp.path()).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA busy_timeout=5000;
                 PRAGMA foreign_keys=OFF;",
            )
            .unwrap();
        writer
            .execute_batch(include_str!("test_schema.sql"))
            .unwrap();
        let point = vec![0.25_f32; DIM];
        let mut tree = RoutingTree::new(RoutingConfig::default(), DIM, 7);
        let point_id = tree.insert(point.clone());
        save_point(&writer, point_id, "snapshot baseline", &point, 1, 1000).unwrap();
        save_dirty_nodes(&mut tree, &writer).unwrap();

        let reader = Connection::open(temp.path()).unwrap();
        reader
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .unwrap();
        let writer_mode: String = writer
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let reader_mode: String = reader
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            (writer_mode.as_str(), reader_mode.as_str()),
            ("wal", "wal"),
            "both independently opened connections must be in WAL mode before the test brackets a read snapshot with a writer commit"
        );
        let pause = register_load_pause(temp.path().to_path_buf());
        let load = std::thread::spawn(move || {
            load_routing_tree(&reader, RoutingConfig::default(), DIM, 7)
        });
        assert!(
            pause.wait_until_reached(),
            "the loader must pause after its first SELECT established the explicit read snapshot"
        );

        writer
            .execute(
                "INSERT INTO routing_leaf_membership
                 (leaf_node_id, point_id, is_dual, divergence_node_id)
                 VALUES (0, 999999, 0, NULL)",
                [],
            )
            .unwrap();
        pause.release();
        let (loaded, metadata) = load
            .join()
            .expect("the paused loader thread must not panic")
            .expect("the in-flight load must remain on its pre-commit SQLite snapshot");
        assert_eq!(
            (loaded.point_count(), metadata.len()),
            (1, 1),
            "one explicit read transaction must install only the coherent pre-commit tree"
        );

        let error = expect_load_error(
            load_routing_tree(&writer, RoutingConfig::default(), DIM, 7),
            "a later snapshot must see and reject the malformed committed membership",
        );
        assert!(
            error
                .to_string()
                .contains("membership references point 999999"),
            "the following snapshot must name the newly visible malformed reference; got {error:#}"
        );
    }

    #[test]
    fn test_load_rejects_points_when_routing_nodes_is_empty() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("test_schema.sql")).unwrap();
        save_point(&conn, 0, "orphan point", &vec![0.5; DIM], 1, 1000).unwrap();

        let error = expect_load_error(
            load_routing_tree(&conn, RoutingConfig::default(), DIM, 7),
            "points without any routing node must be rejected, not treated as empty",
        );
        assert!(
            error
                .to_string()
                .contains("routing_nodes is empty but the store still contains 1 routing_points"),
            "the load error must name the orphaned durable row class; got {error:#}"
        );
    }

    #[test]
    fn test_load_validates_every_point_embedding_dimension_and_blob_width() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("test_schema.sql")).unwrap();
        let points = test_corpus(1);
        let mut tree = RoutingTree::new(RoutingConfig::default(), DIM, 7);
        for (point_id, point) in points.iter().take(2).enumerate() {
            assert_eq!(tree.insert(point.clone()), point_id);
            save_point(
                &conn,
                point_id,
                &format!("memory {point_id}"),
                point,
                1,
                1000 + point_id as i64,
            )
            .unwrap();
        }
        save_dirty_nodes(&mut tree, &conn).unwrap();

        conn.execute(
            "UPDATE routing_points SET embedding = ?1 WHERE point_id = 1",
            params![vec![0_u8; (DIM - 1) * std::mem::size_of::<f32>()]],
        )
        .unwrap();
        let wrong_dimension = expect_load_error(
            load_routing_tree(&conn, RoutingConfig::default(), DIM, 7),
            "every point row, not only point zero, must match the configured dimension",
        );
        assert!(
            wrong_dimension
                .to_string()
                .contains("routing point 1 embedding has 15 values (60 bytes), expected 16 values"),
            "the malformed point error must name its row and actual/expected lengths; got {wrong_dimension:#}"
        );

        conn.execute(
            "UPDATE routing_points SET embedding = ?1 WHERE point_id = 1",
            params![vec![0_u8; DIM * std::mem::size_of::<f32>() + 1]],
        )
        .unwrap();
        let partial_value = expect_load_error(
            load_routing_tree(&conn, RoutingConfig::default(), DIM, 7),
            "a point blob with trailing bytes must not be silently truncated",
        );
        assert!(
            partial_value
                .to_string()
                .contains("routing point 1 embedding blob has 65 bytes"),
            "the partial-value error must name the point and exact byte length; got {partial_value:#}"
        );
    }

    #[test]
    fn test_load_validates_every_node_vector_dimension_and_blob_width() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("test_schema.sql")).unwrap();
        let points = test_corpus(20);
        let mut tree = RoutingTree::new(RoutingConfig::default(), DIM, 7);
        for (point_id, point) in points.iter().enumerate() {
            assert_eq!(tree.insert(point.clone()), point_id);
            save_point(
                &conn,
                point_id,
                &format!("memory {point_id}"),
                point,
                1,
                1000 + point_id as i64,
            )
            .unwrap();
        }
        save_dirty_nodes(&mut tree, &conn).unwrap();

        conn.execute(
            "UPDATE routing_nodes SET real_centroid = ?1 WHERE node_id = 0",
            params![vec![0_u8; (DIM - 1) * std::mem::size_of::<f64>()]],
        )
        .unwrap();
        let wrong_centroid = expect_load_error(
            load_routing_tree(&conn, RoutingConfig::default(), DIM, 7),
            "every node centroid must match the configured dimension",
        );
        assert!(
            wrong_centroid
                .to_string()
                .contains("routing node 0 real_centroid has 15 values (120 bytes), expected 16 values"),
            "the malformed centroid error must name its node and actual/expected lengths; got {wrong_centroid:#}"
        );

        conn.execute(
            "UPDATE routing_nodes SET real_centroid = ?1 WHERE node_id = 0",
            params![encode_f64(tree.real_centroid_raw(0))],
        )
        .unwrap();
        let decision_id: i64 = conn
            .query_row(
                "SELECT node_id FROM routing_nodes WHERE is_leaf = 0 ORDER BY node_id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut malformed_anchor = encode_f64(tree.anchor_of(decision_id as usize));
        malformed_anchor.push(0);
        conn.execute(
            "UPDATE routing_nodes SET anchor = ?1 WHERE node_id = ?2",
            params![malformed_anchor, decision_id],
        )
        .unwrap();
        let partial_value = expect_load_error(
            load_routing_tree(&conn, RoutingConfig::default(), DIM, 7),
            "a node vector blob with trailing bytes must not be silently truncated",
        );
        assert!(
            partial_value.to_string().contains(&format!(
                "routing node {decision_id} anchor blob has 129 bytes"
            )),
            "the partial-value error must name the node, field, and exact byte length; got {partial_value:#}"
        );
    }

    #[test]
    fn test_load_rejects_dual_divergence_decision_not_encountered_on_the_leaf_path() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("test_schema.sql")).unwrap();

        let points = test_corpus(30);
        let mut tree = RoutingTree::new(RoutingConfig::default(), DIM, 7);
        for (point_id, point) in points.iter().enumerate() {
            assert_eq!(tree.insert(point.clone()), point_id);
            save_point(
                &conn,
                point_id,
                &format!("memory {point_id}"),
                point,
                1,
                1000 + point_id as i64,
            )
            .unwrap();
        }
        save_dirty_nodes(&mut tree, &conn).unwrap();

        let decisions: Vec<usize> = conn
            .prepare("SELECT node_id FROM routing_nodes WHERE is_leaf = 0 ORDER BY node_id")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .map(|row| row.unwrap() as usize)
            .collect();
        let memberships: Vec<(usize, usize)> = conn
            .prepare(
                "SELECT leaf_node_id, point_id FROM routing_leaf_membership
                 WHERE is_dual = 0 ORDER BY point_id, leaf_node_id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)? as usize,
                    row.get::<_, i64>(1)? as usize,
                ))
            })
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        let mut selected = None;
        for (leaf_id, point_id) in memberships {
            let mut ancestors = HashSet::new();
            let mut current = Some(leaf_id);
            while let Some(node_id) = current {
                ancestors.insert(node_id);
                current = conn
                    .query_row(
                        "SELECT parent_id FROM routing_nodes WHERE node_id = ?1",
                        [node_id as i64],
                        |row| row.get::<_, Option<i64>>(0),
                    )
                    .unwrap()
                    .map(|value| value as usize);
            }
            if let Some(&off_path_decision) = decisions
                .iter()
                .find(|decision| !ancestors.contains(decision))
            {
                selected = Some((leaf_id, point_id, off_path_decision));
                break;
            }
        }
        let (leaf_id, point_id, off_path_decision) = selected.expect(
            "test corpus must produce a decision outside at least one primary membership path",
        );
        conn.execute(
            "UPDATE routing_leaf_membership
             SET is_dual = 1, divergence_node_id = ?3
             WHERE leaf_node_id = ?1 AND point_id = ?2",
            params![leaf_id as i64, point_id as i64, off_path_decision as i64],
        )
        .unwrap();

        let error = expect_load_error(
            load_routing_tree(&conn, RoutingConfig::default(), DIM, 7),
            "an off-path dual divergence reference must fail closed",
        );
        assert!(
            error.to_string().contains("replay never encountered"),
            "a valid decision id is not sufficient unless replay actually encounters it; got {error:#}"
        );
    }

    #[test]
    fn test_load_routing_tree_auto_repairs_traversal_mismatch_and_preserves_recall() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("test_schema.sql")).unwrap();

        let points = test_corpus(20);
        let mut tree = RoutingTree::new(RoutingConfig::default(), DIM, 7);
        for (i, p) in points.iter().enumerate() {
            let pid = tree.insert(p.clone());
            assert_eq!(pid, i);
            save_point(
                &conn,
                pid,
                &format!("memory {pid}"),
                p,
                1,
                1000 + pid as i64,
            )
            .unwrap();
        }
        save_dirty_nodes(&mut tree, &conn).unwrap();

        let mut leaves = Vec::new();
        for id in 0..tree.node_count() {
            if tree.is_leaf(id) {
                leaves.push(id);
            }
        }
        assert!(
            leaves.len() >= 2,
            "tree must have split into at least 2 leaves"
        );

        // Pick point 0 and find which leaf it is currently in
        let cur_leaf: usize = conn
            .query_row(
                "SELECT leaf_node_id FROM routing_leaf_membership WHERE is_dual = 0 AND point_id = 0",
                [],
                |r| r.get::<_, i64>(0).map(|id| id as usize),
            )
            .unwrap();
        let other_leaf = leaves.iter().copied().find(|&l| l != cur_leaf).unwrap();

        // Corrupt membership table so point 0 points to other_leaf
        conn.execute(
            "UPDATE routing_leaf_membership SET leaf_node_id = ?1 WHERE point_id = 0 AND is_dual = 0",
            params![other_leaf as i64],
        )
        .unwrap();

        // Hydration must now succeed and auto-repair point 0 into cur_leaf
        let (loaded, metadata) =
            load_routing_tree(&conn, RoutingConfig::default(), DIM, 7).unwrap();
        assert_eq!(metadata.len(), points.len());

        // Assert point 0 was auto-repaired into cur_leaf
        assert!(
            loaded.bucket_of(cur_leaf).contains(&0),
            "point 0 must be auto-repaired into cur_leaf where traversal stopped"
        );
        assert!(
            !loaded.bucket_of(other_leaf).contains(&0),
            "point 0 must not remain in other_leaf"
        );

        // Recall for point 0 must succeed
        let result = loaded.descend_adaptive(&points[0], None, false);
        assert_eq!(
            result.best_point_id,
            Some(0),
            "recall for auto-repaired point must find point 0"
        );
    }
}

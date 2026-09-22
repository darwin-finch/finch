//! Save/load for [`RoutingTree`] against `routing_points`/`routing_nodes`/`routing_leaf_membership`
//! (`schema.sql`). Reuses `finch-memory`'s own established dirty-tracking discipline (mark on
//! mutation, save only what changed, clear on hydrate) -- `RoutingTree::dirty_node_ids`/
//! `mark_persisted` are the same shape as `MemTree::dirty_nodes`/`mark_persisted`, deliberately.
//!
//! Canonical point content (text + embedding) is persisted separately from tree structure, in
//! `routing_points` -- `RoutingTree` itself has no notion of text, only embeddings and structure.
//! `bucket_deflated` (each leaf entry's per-node deflated residual cache) is NOT persisted: it is
//! deterministically recomputable from a point's canonical embedding plus the frozen
//! `(anchor, direction)` chain of every node it passes through, so [`load_routing_tree`]
//! reconstructs it once on load rather than storing a second, derived, embedding-sized copy per
//! leaf entry.
//!
//! This module is scoped to `RoutingTree` alone -- wiring it into `MemorySystem`'s own hydration
//! state machine (the `Loading`/`Degraded`/`Ready` progression, batch-by-batch partial reads) is a
//! separate, larger piece of work, not yet done. What's here is real, tested, and usable standalone.

use super::{
    normalize_in_place, projection, splitmix64_uniform_half, to_double, Node, RoutingConfig,
    RoutingTree,
};
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
#[allow(dead_code)] // wired into MemorySystem in a follow-up (see module doc)
pub(crate) fn save_point(
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

#[allow(dead_code)] // wired into MemorySystem in a follow-up (see module doc)
pub(crate) fn mark_point_removed(conn: &Connection, point_id: usize) -> Result<()> {
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
pub(crate) fn save_dirty_nodes(tree: &mut RoutingTree, conn: &Connection) -> Result<()> {
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
pub(crate) fn write_dirty_nodes_within(
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
#[allow(dead_code)] // wired into MemorySystem in a follow-up (see module doc)
pub(crate) fn load_routing_tree(
    conn: &Connection,
    cfg: RoutingConfig,
    dim: usize,
    seed: u64,
) -> Result<(RoutingTree, Vec<(usize, String, u8)>)> {
    let node_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM routing_nodes", [], |r| r.get(0))
        .context("load_routing_tree: count routing_nodes")?;
    let spherical = cfg.spherical_mode;
    let tree = RoutingTree::new(cfg, dim, seed);
    if node_count == 0 {
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
                decode_f32(&embedding_bytes),
                importance as u8,
                removed != 0,
            ))
        })
        .context("load_routing_tree: query points")?;
    for row in rows {
        let (point_id, text, embedding, importance, removed) =
            row.context("load_routing_tree: read point row")?;
        anyhow::ensure!(point_id == points.len(), "load_routing_tree: routing_points.point_id must be contiguous from 0, got {point_id} at position {}", points.len());
        if !removed {
            metadata.push((point_id, text, importance));
        }
        points.push(embedding);
        removed_flag.push(removed);
    }

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
        nodes.push(node);
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
    for m in &memberships {
        let embedding = &points[m.point_id];
        let mut x = to_double(embedding);
        if spherical {
            normalize_in_place(&mut x);
        }
        let mut cur = 0usize; // root is always node 0
        while cur != m.leaf_node_id {
            let anchor = &nodes[cur].anchor;
            let dir = &nodes[cur].direction;
            let proj = projection(&x, anchor, dir);
            let favored_right = proj >= 0.0;
            let at_divergence = m.is_dual && Some(cur) == m.divergence_node_id;
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
                    .expect("decision node must have a right child")
            } else {
                nodes[cur]
                    .left
                    .expect("decision node must have a left child")
            };
        }
        nodes[m.leaf_node_id].bucket_ids.push(m.point_id);
        nodes[m.leaf_node_id].bucket_deflated.push(x);
        nodes[m.leaf_node_id].bucket_is_dual.push(m.is_dual);
    }

    let membership_tuples: Vec<(usize, usize, bool, Option<usize>)> = memberships
        .iter()
        .map(|m| (m.leaf_node_id, m.point_id, m.is_dual, m.divergence_node_id))
        .collect();
    let mut tree = tree;
    tree.install_loaded_state(nodes, points, removed_flag, &membership_tuples);
    Ok((tree, metadata))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        conn.execute_batch(include_str!("../schema.sql")).unwrap();

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
        conn.execute_batch(include_str!("../schema.sql")).unwrap();

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
        conn.execute_batch(include_str!("../schema.sql")).unwrap();

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
        conn.execute_batch(include_str!("../schema.sql")).unwrap();

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
        conn.execute_batch(include_str!("../schema.sql")).unwrap();

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
}

// MemTree implementation - Hierarchical semantic memory
//
// Based on "From Isolated Conversations to Hierarchical Schemas:
// Dynamic Tree Memory Representation for LLMs" (arXiv:2410.14052)
//
// Key properties:
// - O(log N) insertion (real-time, no rebuild)
// - Hierarchical structure (not flat RAG)
// - Semantic similarity-based navigation
// - Aggregated parent summaries

use super::embeddings::{average_embeddings, cosine_similarity};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

/// Node ID in the tree
pub type NodeId = u64;

/// Similarity above which two memories are treated as variants of one another
/// rather than as a parent and child.
///
/// The tokenizer drops tokens shorter than two characters, so "feature number
/// 1" and "feature number 2" produce *identical* embeddings while their text
/// differs. Exact-text dedup misses them, and promoting on each one builds a
/// chain one level deeper per variant — the 2,525-level chain measured on the
/// dogfood store was exactly this shape. Variants become siblings instead.
const NEAR_IDENTICAL_SIMILARITY: f32 = 0.99;

/// Floor for the depth-adaptive threshold.
const MIN_SIMILARITY_THRESHOLD: f32 = 0.05;

/// Base threshold at the root: `theta_0` in the paper's notation, and the
/// starting point of the depth-adaptive curve in `threshold_at_depth`.
const BASE_SIMILARITY_THRESHOLD: f32 = 0.4;

/// Rate at which the threshold rises with depth, `lambda` in the paper.
const THRESHOLD_GROWTH_RATE: f32 = 0.5;

/// Depth used to normalize the exponent.
///
/// The paper's headline formula is `theta(d) = theta_0 * e^(lambda * d)`, but
/// taken literally with `theta_0 = 0.4` and `lambda = 0.5` it exceeds 1.0 at
/// depth 2, which cosine similarity can never reach — no node below depth 1
/// could ever be descended into, yet the paper reports trees of depth 13.
/// Appendix A.1.3 states the threshold "adjusts based on current_depth and
/// max_depth", so the exponent is normalized by a depth scale. This constant is
/// that scale, set to the maximum depth reported in the paper's experiments.
const THRESHOLD_DEPTH_SCALE: f32 = 13.0;

/// Hard ceiling so the threshold stays inside cosine similarity's range.
///
/// This must stay ABOVE `NEAR_IDENTICAL_SIMILARITY`. When it sat below, pairs
/// in the band between the two cleared the (saturated) threshold at every
/// depth yet were never recognised as variants, so they promoted without
/// bound. Measured with the ceiling at 0.98, content at pairwise 0.985 reaches
/// depth 60 across the 60 distinct vectors of
/// `test_content_in_the_ceiling_band_does_not_chain` — one level per distinct
/// input — against depth 25 with the ordering correct.
const MAX_SIMILARITY_THRESHOLD: f32 = 0.995;

/// What an insert did, so the caller can keep durable provenance correct.
///
/// A promotion moves the original content into a new child. Any
/// `memory_sources` row pointing at the promoted node must follow that content
/// to `moved`, or the row ends up attributing a conversation to an aggregate
/// whose embedding is the mean of two different memories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertEffect {
    /// The node holding the inserted content.
    pub node: NodeId,
    /// True when the content already existed and no node was created.
    pub deduplicated: bool,
    /// Set when a leaf became a parent: `(promoted, moved)`.
    pub promotion: Option<(NodeId, NodeId)>,
}

/// A node in the MemTree
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub text: String,
    pub embedding: Vec<f32>,
    pub level: usize,
    pub created_at: i64,
    /// Importance tier (0=Discard, 1=Normal, 2=High, 3=Critical).
    /// Stored as u8 to keep TreeNode cheap to clone.
    /// Root node always has importance=0.
    pub importance: u8,
}

/// MemTree - Hierarchical semantic memory structure
pub struct MemTree {
    /// Nodes whose persisted columns have changed since the last save.
    ///
    /// Persistence used to write every node on every insert, so the cost of
    /// storing one memory was proportional to everything stored before it --
    /// 16,782 rows and 131 MiB of embeddings rewritten per turn on the dogfood
    /// host, which is what filled a 142 MiB write-ahead log (#250).
    ///
    /// Only the tree's own mutators record here. `all_nodes_mut` deliberately
    /// does not: its callers are the hydration paths, which are reconstructing
    /// the tree *from* the durable rows and must not mark them for rewriting.
    /// The cost of that choice is that a future mutator which forgets to mark
    /// loses data rather than merely writing too much, so
    /// `test_every_inserted_memory_survives_a_reload` exists to catch it.
    dirty: HashSet<NodeId>,
    root: NodeId,
    nodes: HashMap<NodeId, TreeNode>,
    next_id: NodeId,
}

impl MemTree {
    /// Create a new empty MemTree with the TF-IDF default embedding dimension (2048).
    pub fn new() -> Self {
        Self::new_with_dim(2048)
    }

    /// Create a new empty MemTree with a specified root embedding dimension.
    ///
    /// Use this when swapping in a neural embedding engine whose dimension
    /// differs from the TF-IDF default (e.g. 384 for all-MiniLM-L6-v2).
    pub fn new_with_dim(dim: usize) -> Self {
        let root_id = 0;
        let mut nodes = HashMap::new();

        // Create root node (placeholder) — zero vector of the given dimension
        let root = TreeNode {
            id: root_id,
            parent: None,
            children: Vec::new(),
            text: String::from("ROOT"),
            embedding: vec![0.0; dim],
            level: 0,
            created_at: chrono::Utc::now().timestamp(),
            importance: 0, // synthetic — not a real memory
        };

        nodes.insert(root_id, root);

        Self {
            root: root_id,
            nodes,
            next_id: 1,
            // The root is synthetic and never inserted, but it has to reach
            // the table before any child does: `parent_id` is a
            // self-referential foreign key and SQLite enforces it immediately.
            dirty: HashSet::from([root_id]),
        }
    }

    /// Similarity a child must reach to be descended into at `depth`.
    ///
    /// `theta(d) = theta_0 * e^(lambda * d / d_scale)`. Deeper nodes hold more
    /// specific information and demand a closer match, which is what keeps the
    /// hierarchy from flattening into one bucket.
    fn threshold_at_depth(depth: usize) -> f32 {
        let normalized = depth as f32 / THRESHOLD_DEPTH_SCALE;
        (BASE_SIMILARITY_THRESHOLD * (THRESHOLD_GROWTH_RATE * normalized).exp())
            .clamp(MIN_SIMILARITY_THRESHOLD, MAX_SIMILARITY_THRESHOLD)
    }

    /// Find an existing leaf holding exactly this text.
    ///
    /// Only leaves are considered. Internal nodes hold aggregated content, so
    /// matching against them would attach new information to a summary rather
    /// than to the memory it summarizes.
    fn find_leaf_by_text(&self, text: &str) -> Option<NodeId> {
        self.nodes
            .values()
            .find(|node| node.id != self.root && node.children.is_empty() && node.text == text)
            .map(|node| node.id)
    }

    /// Insert text with embedding into the tree.
    ///
    /// Algorithm from the MemTree paper (arXiv:2410.14052):
    ///
    /// 1. Identical content already stored as a leaf is returned as-is. The
    ///    paper assumes distinct observations; a conversational store sees the
    ///    same turn repeatedly, and re-inserting it is what produced 16,782
    ///    nodes for 567 distinct texts (#250).
    /// 2. From the root, score the new embedding against **each child** and
    ///    descend into the best child that clears `threshold_at_depth`.
    ///    Scoring against the current node instead — whose embedding is the
    ///    mean of its children — compares against an average that resembles
    ///    nothing once the node is wide, which is what produced a single node
    ///    with 13,650 children.
    /// 3. If no child clears the threshold, the new content becomes a child of
    ///    the current node.
    /// 4. If the descent lands on a leaf, that leaf is **promoted to a parent**
    ///    holding the original content and the new content as siblings.
    ///    Appending beneath the leaf instead is what produced a 2,500-deep
    ///    single-node chain.
    /// 5. Ancestor embeddings are re-aggregated.
    ///
    /// `importance` is the tier assigned by `MemoryClassifier` (0-3).
    pub fn insert(&mut self, text: String, embedding: Vec<f32>, importance: u8) -> Result<NodeId> {
        self.insert_with_effect(text, embedding, importance)
            .map(|effect| effect.node)
    }

    /// Insert, reporting what happened so durable provenance can be kept correct.
    ///
    /// **A returned `Err` leaves the tree mutated.** `attach_child` and
    /// `promote_leaf` insert the new node and push it into its parent's
    /// `children` before aggregating, and neither unwinds when aggregation
    /// fails. A caller that keeps using the tree afterwards will find the
    /// half-inserted node — and `find_leaf_by_text` will deduplicate to it, so
    /// an identical retry returns `Ok` off the wreckage of the failed attempt.
    /// The one production caller restores from the durable snapshot; a new one
    /// must do the same or restore some other way.
    pub fn insert_with_effect(
        &mut self,
        text: String,
        embedding: Vec<f32>,
        importance: u8,
    ) -> Result<InsertEffect> {
        if let Some(existing) = self.find_leaf_by_text(&text) {
            // Content already stored. Raise importance if this occurrence is
            // more important — a fact first seen in passing and later stored
            // explicitly must gain the retrieval boost it earned.
            if let Some(node) = self.nodes.get_mut(&existing) {
                if importance > node.importance {
                    node.importance = importance;
                    self.dirty.insert(existing);
                }
            }
            return Ok(InsertEffect {
                node: existing,
                deduplicated: true,
                promotion: None,
            });
        }

        let created_at = chrono::Utc::now().timestamp();
        let mut current = self.root;
        let mut depth = 0usize;
        // `children` is rebuilt from `parent_id` at load, so the same corrupt
        // chain that made aggregation recurse forever also makes the root a
        // child of one of its own descendants. Descent would then loop
        // root -> a -> root forever whenever similarity clears the threshold,
        // which it always does on a degenerate chain: a single-child parent's
        // embedding is exactly its child's, so cosine is 1.0 at every hop. An
        // unbounded hang is harder to diagnose than the abort this guard's
        // sibling in `update_parent_aggregation` replaced.
        let mut seen: std::collections::HashSet<NodeId> = std::collections::HashSet::new();

        loop {
            anyhow::ensure!(
                seen.insert(current),
                "memtree: insert descent revisits node {current}; the stored \
                 tree is corrupt"
            );
            let threshold = Self::threshold_at_depth(depth);

            let node = self.nodes.get(&current).ok_or_else(|| {
                anyhow::anyhow!("memtree: node {} not found during insert", current)
            })?;

            // Score every child; descend into the best one that clears the
            // depth-adaptive threshold.
            let mut best: Option<(NodeId, f32)> = None;
            for &child_id in &node.children {
                let child = self
                    .nodes
                    .get(&child_id)
                    .ok_or_else(|| anyhow::anyhow!("memtree: child node {} not found", child_id))?;
                let similarity = cosine_similarity(&embedding, &child.embedding);
                if similarity >= threshold && best.map_or(true, |(_, b)| similarity > b) {
                    best = Some((child_id, similarity));
                }
            }

            let Some((chosen, best_similarity)) = best else {
                let node = self.attach_child(current, text, embedding, importance, created_at)?;
                return Ok(InsertEffect {
                    node,
                    deduplicated: false,
                    promotion: None,
                });
            };

            if self
                .nodes
                .get(&chosen)
                .is_some_and(|child| child.children.is_empty())
            {
                // Near-identical content is a variant, not a refinement, so it
                // belongs beside its siblings rather than beneath them.
                //
                // But only once a cluster exists to hold them. At the root
                // there is no cluster yet, so the first variant promotes to
                // create one and every later variant lands inside it: root
                // fan-out 1 and depth 2, however many arrive.
                //
                // Be precise about what this does and does not fix. The
                // cluster head's own fan-out is unbounded — 13,650 near-identical
                // VARIANTS of one status line give one node with 13,650
                // children, the same shape #250 measured, one level below where
                // it used to sit. Byte-identical repeats never get here: they
                // are deduplicated to a single childless leaf.
                // That is intended: those are variants of a single message and
                // belong together, retrieval scans linearly either way, and
                // insert cost is unchanged. What this rule fixes is the depth,
                // not the width.
                if best_similarity >= NEAR_IDENTICAL_SIMILARITY && current != self.root {
                    let node =
                        self.attach_child(current, text, embedding, importance, created_at)?;
                    return Ok(InsertEffect {
                        node,
                        deduplicated: false,
                        promotion: None,
                    });
                }

                let (inserted, moved) =
                    self.promote_leaf(chosen, text, embedding, importance, created_at)?;
                return Ok(InsertEffect {
                    node: inserted,
                    deduplicated: false,
                    promotion: Some((chosen, moved)),
                });
            }

            current = chosen;
            depth += 1;
        }
    }

    /// Attach new content as a child of `parent`.
    fn attach_child(
        &mut self,
        parent_id: NodeId,
        text: String,
        embedding: Vec<f32>,
        importance: u8,
        created_at: i64,
    ) -> Result<NodeId> {
        let parent_level = self
            .nodes
            .get(&parent_id)
            .ok_or_else(|| anyhow::anyhow!("memtree: node {} not found", parent_id))?
            .level;

        let new_id = self.next_id;
        self.next_id += 1;

        self.nodes.insert(
            new_id,
            TreeNode {
                id: new_id,
                parent: Some(parent_id),
                children: Vec::new(),
                text,
                embedding,
                level: parent_level + 1,
                created_at,
                importance,
            },
        );

        self.dirty.insert(new_id);

        let parent = self
            .nodes
            .get_mut(&parent_id)
            .ok_or_else(|| anyhow::anyhow!("memtree: node {} not found", parent_id))?;
        // `children` is rebuilt from `parent_id` at load, so the parent needs
        // no row of its own for this -- but `update_parent_aggregation` is
        // about to rewrite its embedding, and that marks it.
        parent.children.push(new_id);

        self.update_parent_aggregation(parent_id)?;
        Ok(new_id)
    }

    /// Turn a matched leaf into a parent of two siblings.
    ///
    /// The leaf's original content moves into a new child, and the new content
    /// becomes its sibling. The promoted node retains the original text as a
    /// provisional summary: the paper replaces it with an LLM aggregation of
    /// both children, which is not implemented yet and is tracked on #250, so
    /// until then the parent is labelled by the memory it was formed around.
    ///
    /// Because an internal node may therefore share text with a child until
    /// aggregation lands, `find_leaf_by_text` deliberately matches leaves only.
    fn promote_leaf(
        &mut self,
        leaf_id: NodeId,
        text: String,
        embedding: Vec<f32>,
        importance: u8,
        created_at: i64,
    ) -> Result<(NodeId, NodeId)> {
        let (original_text, original_embedding, original_importance, original_created_at, level) = {
            let leaf = self
                .nodes
                .get(&leaf_id)
                .ok_or_else(|| anyhow::anyhow!("memtree: leaf {} not found", leaf_id))?;
            (
                leaf.text.clone(),
                leaf.embedding.clone(),
                leaf.importance,
                leaf.created_at,
                leaf.level,
            )
        };

        let moved_id = self.next_id;
        self.next_id += 1;
        let inserted_id = self.next_id;
        self.next_id += 1;

        self.nodes.insert(
            moved_id,
            TreeNode {
                id: moved_id,
                parent: Some(leaf_id),
                children: Vec::new(),
                text: original_text,
                embedding: original_embedding,
                level: level + 1,
                created_at: original_created_at,
                importance: original_importance,
            },
        );

        self.nodes.insert(
            inserted_id,
            TreeNode {
                id: inserted_id,
                parent: Some(leaf_id),
                children: Vec::new(),
                text,
                embedding,
                level: level + 1,
                created_at,
                importance,
            },
        );

        self.dirty.insert(moved_id);
        self.dirty.insert(inserted_id);

        let promoted = self
            .nodes
            .get_mut(&leaf_id)
            .ok_or_else(|| anyhow::anyhow!("memtree: leaf {} not found", leaf_id))?;
        promoted.children.push(moved_id);
        promoted.children.push(inserted_id);

        self.update_parent_aggregation(leaf_id)?;
        Ok((inserted_id, moved_id))
    }

    /// Deepest level present in the tree. Diagnostic, and asserted by tests
    /// that guard against the chain regression.
    pub fn max_depth(&self) -> usize {
        self.nodes
            .values()
            .map(|node| node.level)
            .max()
            .unwrap_or(0)
    }

    /// Update a node's embedding to the average of its children, then do the
    /// same for each of its ancestors.
    ///
    /// Iterative with a visited set, not recursive. `parent` links come from
    /// `tree_nodes` rows on disk, and a corrupt or self-referential chain — a
    /// node that is its own ancestor — made this recurse until the thread
    /// overflowed its stack, which aborts the **process** (SIGABRT) rather than
    /// returning an error. That is unrecoverable and takes down whatever else
    /// the process was doing. A cycle is now a plain `Err` naming the node
    /// (#274).
    fn update_parent_aggregation(&mut self, node_id: NodeId) -> Result<()> {
        // Two passes: validate the whole chain, then mutate it.
        //
        // Walking and rewriting embeddings as we go, erroring only on the
        // revisit, leaves a corrupt store with half-updated aggregates — and a
        // later dedup-path insert calls `save_all_nodes_to_db`, which writes
        // every node, so those reach disk and change retrieval scoring. An
        // error here must leave the tree exactly as it found it.
        let mut chain: Vec<NodeId> = Vec::new();
        let mut visited: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
        let mut current = node_id;
        let mut previous = node_id;

        loop {
            if !visited.insert(current) {
                anyhow::bail!(
                    "memtree: parent chain from node {node_id} cycles: node \
                     {previous} points back at node {current}; the stored tree \
                     is corrupt"
                );
            }
            let node = self.nodes.get(&current).ok_or_else(|| {
                anyhow::anyhow!("memtree: node {current} not found during aggregation")
            })?;
            let parent = node.parent;
            chain.push(current);
            let Some(parent_id) = parent else {
                break;
            };
            previous = current;
            current = parent_id;
        }

        for id in chain {
            // A childless node keeps its own embedding, but its ancestors are
            // still walked: the caller may have just moved a child away from
            // one of them.
            let node = self.nodes.get(&id).ok_or_else(|| {
                anyhow::anyhow!("memtree: node {id} disappeared between validation and update")
            })?;
            let child_embeddings: Vec<_> = node
                .children
                .iter()
                .filter_map(|child_id| self.nodes.get(child_id))
                .map(|child| &child.embedding)
                .collect();
            if child_embeddings.is_empty() {
                continue;
            }
            let aggregated = average_embeddings(&child_embeddings);
            let node = self.nodes.get_mut(&id).ok_or_else(|| {
                anyhow::anyhow!("memtree: node {id} disappeared before its embedding update")
            })?;
            node.embedding = aggregated;
            self.dirty.insert(id);
        }

        Ok(())
    }

    /// Retrieve top-k most relevant nodes (flat retrieval with importance weighting).
    ///
    /// Score = cosine_similarity × importance_boost, where:
    ///   - Critical (3) → ×1.4  (decisions, bugs, explicit rules)
    ///   - High    (2) → ×1.2  (file refs, code patterns, preferences)
    ///   - Normal  (1) → ×1.0  (general Q&A)
    ///   - Discard (0) →  0.0  (never returned)
    ///
    /// This means a Critical memory at 0.70 similarity scores 0.98, beating a
    /// Normal memory at 0.85 — important things surface even when slightly less
    /// semantically close to the query.
    pub fn retrieve(&self, query_embedding: &[f32], top_k: usize) -> Vec<(NodeId, String, f32)> {
        let mut results: Vec<_> = self
            .nodes
            .values()
            .filter(|node| {
                // Leaves only. An internal node's text is the provisional label
                // inherited from the leaf it was formed around, duplicated from
                // one of its own children, and its embedding is the mean of its
                // subtree. Returning them lets one memory occupy several result
                // slots and surfaces an averaged vector as if it were a memory.
                // When aggregation produces real summaries they can return.
                node.id != self.root && node.importance > 0 && node.children.is_empty()
            })
            .map(|node| {
                let similarity = cosine_similarity(query_embedding, &node.embedding);
                let boost = match node.importance {
                    3 => 1.4_f32,
                    2 => 1.2_f32,
                    _ => 1.0_f32,
                };
                (node.id, node.text.clone(), similarity * boost)
            })
            .collect();

        // Sort by weighted score descending
        results.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        results.into_iter().take(top_k).collect()
    }

    /// Get node by ID
    pub fn get_node(&self, id: NodeId) -> Option<&TreeNode> {
        self.nodes.get(&id)
    }

    /// Get all nodes (for serialization)
    pub fn all_nodes(&self) -> &HashMap<NodeId, TreeNode> {
        &self.nodes
    }

    /// Mutable access to nodes map (used by persistence layer to reconstruct tree).
    pub fn all_nodes_mut(&mut self) -> &mut HashMap<NodeId, TreeNode> {
        &mut self.nodes
    }

    /// Take the set of nodes whose persisted columns changed, clearing it.
    ///
    /// The caller is committing them; anything it does not write is lost, so
    /// this drains rather than copies only because the write happens in the
    /// same transaction. A failed save must put them back -- see
    /// `restore_dirty`.
    pub fn take_dirty(&mut self) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self.dirty.drain().collect();
        // Ascending, so a parent row lands before the child that references
        // it. New nodes always take a higher id than the parent they attach
        // to, and the root is 0.
        ids.sort_unstable();
        ids
    }

    /// Put drained ids back after a save that did not commit.
    ///
    /// Without this a failed transaction would leave the tree believing it had
    /// been persisted, and the next successful save would skip those nodes
    /// permanently.
    pub fn restore_dirty(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        self.dirty.extend(ids);
    }

    /// Forget pending changes, because the tree now matches the durable rows.
    ///
    /// Only for the hydration paths, which replace the tree wholesale from
    /// disk.
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    /// Set the next_id counter (used after loading from disk to avoid ID collisions).
    pub fn set_next_id(&mut self, id: NodeId) {
        self.next_id = id;
    }

    /// Get tree size (number of nodes excluding root)
    pub fn size(&self) -> usize {
        self.nodes.len().saturating_sub(1)
    }
}

impl Default for MemTree {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embeddings::{EmbeddingEngine, TfIdfEmbedding};

    /// Unit vector pointing mostly along `axis`, with `noise` spread elsewhere.
    fn vec_on(axis: usize, dim: usize, noise: f32) -> Vec<f32> {
        let mut v = vec![noise; dim];
        v[axis] = 1.0;
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / norm).collect()
    }

    /// A three-level tree built by hand: root 0 -> 1 -> {2, 3}.
    ///
    /// Explicit rather than produced by `insert`, so these tests pin
    /// aggregation and nothing else. Deriving the shape from promotion made
    /// them depend on `NEAR_IDENTICAL_SIMILARITY` and the threshold constants,
    /// and pick their leaf out of a `HashMap` iteration order.
    fn explicit_tree() -> MemTree {
        let mut tree = MemTree::new_with_dim(4);
        {
            let nodes = tree.all_nodes_mut();
            nodes.get_mut(&0).expect("root").children = vec![1];
            for (id, parent, children, axis) in [
                (1u64, 0u64, vec![2u64, 3u64], 0usize),
                (2, 1, Vec::new(), 1),
                (3, 1, Vec::new(), 2),
            ] {
                nodes.insert(
                    id,
                    TreeNode {
                        id,
                        parent: Some(parent),
                        children,
                        text: format!("node {id}"),
                        embedding: vec_on(axis, 4, 0.0),
                        level: if id == 1 { 1 } else { 2 },
                        created_at: 0,
                        importance: 1,
                    },
                );
            }
        }
        tree.set_next_id(4);
        tree
    }

    #[test]
    fn test_aggregation_reports_a_parent_cycle_instead_of_aborting_the_process() {
        // `parent` links are rebuilt from `tree_nodes` rows on disk. A corrupt
        // chain — a node that is its own ancestor — made
        // `update_parent_aggregation` recurse until the thread overflowed its
        // stack, which is SIGABRT: the whole process dies, taking down whatever
        // else it was doing, and no `Result` is ever returned (#274).
        //
        // The entry node must have children. The old implementation returned
        // early on a childless node, so passing a leaf here never entered the
        // recursion at all and the test would have gone red under the bug for
        // an unrelated reason — which is what the first version of this test
        // did.
        let mut tree = explicit_tree();
        tree.all_nodes_mut().get_mut(&0).expect("root").parent = Some(1);

        let error = tree
            .update_parent_aggregation(1)
            .expect_err("a parent cycle must be an error, not a stack overflow");
        let message = error.to_string();
        assert!(
            message.contains("cycles: node 0 points back at node 1"),
            "the error must name the hop that closes the cycle so a corrupt \
             store is diagnosable; got {message:?}"
        );
    }

    #[test]
    fn test_aggregation_reports_a_self_parenting_node() {
        // The degenerate case: a node whose parent is itself, which is what an
        // off-by-one in `set_next_id` produces. It has children, so the old
        // code recursed into itself forever.
        let mut tree = explicit_tree();
        tree.all_nodes_mut().get_mut(&1).expect("node 1").parent = Some(1);

        let error = tree
            .update_parent_aggregation(1)
            .expect_err("a self-parenting node must be an error");
        assert!(
            error
                .to_string()
                .contains("cycles: node 1 points back at node 1"),
            "got {error}"
        );
    }

    #[test]
    fn test_a_failed_aggregation_leaves_every_embedding_untouched() {
        // Rewriting embeddings as the walk proceeds and only erroring on the
        // revisit left a corrupt store with half-updated aggregates — and a
        // later dedup-path insert calls `save_all_nodes_to_db`, which writes
        // every node, so those reached disk and changed retrieval scoring.
        let mut tree = explicit_tree();
        tree.all_nodes_mut().get_mut(&0).expect("root").parent = Some(1);
        let before: Vec<(NodeId, Vec<f32>)> = tree
            .all_nodes()
            .iter()
            .map(|(id, node)| (*id, node.embedding.clone()))
            .collect();

        tree.update_parent_aggregation(1)
            .expect_err("the cycle must be reported");

        for (id, embedding) in before {
            assert_eq!(
                tree.all_nodes()[&id].embedding,
                embedding,
                "node {id}'s embedding was rewritten before the cycle was \
                 detected; a failed aggregation must change nothing"
            );
        }
    }

    #[test]
    fn test_insert_descent_reports_a_children_cycle_instead_of_hanging() {
        // `children` is rebuilt from `parent_id` at load, so the same on-disk
        // corruption that cycles the parent chain also makes the root a child
        // of one of its own descendants. The descent loop had no guard, so it
        // walked root -> 1 -> root forever — an unbounded hang, which is harder
        // to diagnose than the abort this change replaced.
        let mut tree = explicit_tree();
        {
            let nodes = tree.all_nodes_mut();
            nodes.get_mut(&1).expect("node 1").children.push(0);
            nodes.get_mut(&0).expect("root").parent = Some(1);
            // Root and node 1 match the incoming memory exactly, so descent
            // always clears the threshold between them; the real leaves do not,
            // so neither is ever chosen and the near-identical variant rule —
            // which only fires on a childless choice — never short-circuits the
            // walk. Descent therefore runs root -> 1 -> root -> 1.
            nodes.get_mut(&0).expect("root").embedding = vec_on(0, 4, 0.0);
            nodes.get_mut(&1).expect("node 1").embedding = vec_on(0, 4, 0.0);
            nodes.get_mut(&2).expect("leaf 2").embedding = vec_on(2, 4, 0.0);
            nodes.get_mut(&3).expect("leaf 3").embedding = vec_on(2, 4, 0.0);
        }

        let error = tree
            .insert(
                "a new memory arriving into a corrupt tree".to_string(),
                vec_on(0, 4, 0.0),
                1,
            )
            .expect_err("descent through a children cycle must be an error");
        assert!(
            error.to_string().contains("descent revisits node"),
            "got {error}"
        );
    }

    #[test]
    fn test_aggregation_from_a_leaf_updates_every_ancestor() {
        // The guard must not stop the walk, and the walk must not stop at a
        // childless node. Aggregating from a leaf used to return immediately
        // without touching a single ancestor — dead in production only because
        // both callers happen to pass a node that just gained children, and a
        // trap for the next caller. A change at a leaf must reach the root.
        let mut tree = explicit_tree();
        let root_before = tree.all_nodes()[&0].embedding.clone();
        let intermediate_before = tree.all_nodes()[&1].embedding.clone();

        tree.all_nodes_mut().get_mut(&2).expect("leaf").embedding = vec_on(3, 4, 0.0);
        tree.update_parent_aggregation(2)
            .expect("a well-formed chain must aggregate");

        assert_ne!(
            intermediate_before,
            tree.all_nodes()[&1].embedding,
            "the leaf's parent must be recomputed"
        );
        assert_ne!(
            root_before,
            tree.all_nodes()[&0].embedding,
            "the change must reach the root; an early return leaves stale \
             aggregates all the way up"
        );
    }

    #[test]
    fn test_insert_deduplicates_identical_text() {
        let mut tree = MemTree::new_with_dim(8);
        let embedding = vec_on(0, 8, 0.0);

        let first = tree
            .insert("same turn".to_string(), embedding.clone(), 1)
            .unwrap();
        let size_after_first = tree.size();

        for _ in 0..100 {
            let again = tree
                .insert("same turn".to_string(), embedding.clone(), 1)
                .unwrap();
            assert_eq!(again, first, "identical text must resolve to the same node");
        }

        assert_eq!(
            tree.size(),
            size_after_first,
            "re-inserting identical text must not grow the tree; \
             this is the defect that produced 16,782 nodes for 567 distinct texts"
        );
    }

    #[test]
    fn test_repeated_identical_inserts_do_not_form_a_chain() {
        let mut tree = MemTree::new_with_dim(8);
        let embedding = vec_on(0, 8, 0.0);

        for _ in 0..500 {
            tree.insert("(say \"Hello\")".to_string(), embedding.clone(), 1)
                .unwrap();
        }

        assert_eq!(
            tree.max_depth(),
            1,
            "identical content must not deepen the tree; the measured store had \
             a 2,525-level chain of one repeated message"
        );
    }

    #[test]
    fn test_matched_leaf_is_promoted_to_parent_of_siblings() {
        let mut tree = MemTree::new_with_dim(8);

        let first = tree
            .insert("first memory".to_string(), vec_on(0, 8, 0.0), 1)
            .unwrap();
        // Close enough to clear the root-depth threshold and land on the leaf.
        tree.insert("second memory".to_string(), vec_on(0, 8, 0.2), 1)
            .unwrap();

        let promoted = tree.get_node(first).expect("promoted node must exist");
        assert_eq!(
            promoted.children.len(),
            2,
            "a matched leaf must become a parent of two siblings, not gain a \
             single child beneath it"
        );

        let child_texts: Vec<&str> = promoted
            .children
            .iter()
            .map(|id| tree.get_node(*id).unwrap().text.as_str())
            .collect();
        assert!(
            child_texts.contains(&"first memory"),
            "the original content must move down into a child"
        );
        assert!(
            child_texts.contains(&"second memory"),
            "the new content must become its sibling"
        );
    }

    #[test]
    fn test_no_single_child_chains_form() {
        let mut tree = MemTree::new_with_dim(16);

        for cluster in 0..4 {
            for variant in 0..8 {
                let noise = 0.01 * variant as f32;
                tree.insert(
                    format!("cluster {cluster} variant {variant}"),
                    vec_on(cluster, 16, noise),
                    1,
                )
                .unwrap();
            }
        }

        let single_child_nodes = tree
            .all_nodes()
            .values()
            .filter(|node| node.children.len() == 1)
            .count();
        assert_eq!(
            single_child_nodes, 0,
            "no node may have exactly one child; the measured store had 3,115 \
             such nodes forming chains"
        );
    }

    #[test]
    fn test_dissimilar_content_does_not_collapse_into_one_parent() {
        let mut tree = MemTree::new_with_dim(16);

        for axis in 0..8 {
            tree.insert(format!("topic {axis}"), vec_on(axis, 16, 0.0), 1)
                .unwrap();
        }

        // The root is excluded deliberately, and that exclusion is load-bearing
        // rather than an oversight: unrelated memories have nothing to nest
        // under, so a wide root is the CORRECT outcome here — asserted directly
        // by `test_unrelated_content_still_attaches_to_the_root`. What must not
        // happen is dissimilar content collecting under one *internal* node,
        // which is what scoring a parent's averaged embedding produced.
        //
        // The root-pile failure mode is covered separately, by the root fan-out
        // assertion in `test_near_identical_variants_do_not_build_a_chain`. A
        // fix that moved the pile onto node 0 would fail there.
        let widest = tree
            .all_nodes()
            .values()
            .filter(|node| node.id != 0)
            .map(|node| node.children.len())
            .max()
            .unwrap_or(0);

        assert!(
            widest <= 2,
            "dissimilar topics must not pile onto a single internal parent; \
             widest non-root fan-out was {widest}. The measured store had one \
             node with 13,650 children because descent scored the parent's \
             averaged embedding instead of each child."
        );
    }

    #[test]
    fn test_ceiling_stays_above_the_variant_cutoff() {
        // Load-bearing ordering. When the ceiling sat below the variant cutoff,
        // pairs in the band between them cleared the saturated threshold at
        // every depth yet were never recognised as variants, so they promoted
        // without bound — a family at pairwise 0.985 builds a 500-deep chain
        // instead of stopping at 25.
        //
        // This assertion pins the constants; the behaviour is pinned by
        // `test_content_in_the_ceiling_band_does_not_chain`, whose fixture sits
        // inside the band at 0.98507 and fails with the ceiling at 0.98. Both
        // are needed: this one measures nothing, and that one would not catch
        // the constants being reordered past each other in the other
        // direction.
        assert!(
            MAX_SIMILARITY_THRESHOLD > NEAR_IDENTICAL_SIMILARITY,
            "the depth-adaptive ceiling ({MAX_SIMILARITY_THRESHOLD}) must stay \
             above the variant cutoff ({NEAR_IDENTICAL_SIMILARITY}), or the \
             band between them promotes without bound"
        );
    }

    #[test]
    fn test_content_in_the_ceiling_band_does_not_chain() {
        // A fixture whose pairwise similarity is 0.98507: above the ceiling
        // this constant previously held (0.95, and 0.98 in the failure mode
        // being guarded), and below `NEAR_IDENTICAL_SIMILARITY`.
        //
        // That band is the one that discriminates. Content at or above 0.99 is
        // caught by the variant rule first and gives depth 2 whatever the
        // ceiling is, so a fixture there proves nothing. An earlier version of
        // this test sat at 0.97579 and passed unchanged with the ceiling
        // lowered to 0.98 — the exact defect it names.
        //
        // Verified: with the ordering correct this bounds at depth 25; with the
        // ceiling at 0.98 the same input chains one level per insert.
        let mut tree = MemTree::new_with_dim(64);
        // 60, not 120: with `i % 60` over 0..120 every item had an exact
        // embedding twin, so half the inserts took the variant branch and
        // contributed nothing. Depth was identical for 60 and 120.
        for i in 0..60 {
            let mut v = vec![0.05_f32; 64];
            v[1 + i] += 0.05;
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            tree.insert(
                format!("banded item {i}"),
                v.iter().map(|x| x / norm).collect(),
                1,
            )
            .unwrap();
        }
        assert!(
            tree.max_depth() <= 26,
            "depth must be bounded by the ceiling, not by the input count; \
             got {} for 60 inserts",
            tree.max_depth()
        );
    }

    #[test]
    fn test_threshold_rises_with_depth() {
        let root = MemTree::threshold_at_depth(0);
        let deep = MemTree::threshold_at_depth(13);

        assert!(deep > root, "deeper nodes must demand a closer match");
        assert!(
            MemTree::threshold_at_depth(1000) <= MAX_SIMILARITY_THRESHOLD,
            "threshold must stay inside cosine similarity's range at any depth"
        );
    }

    #[test]
    fn test_near_identical_variants_do_not_build_a_chain() {
        // The tokenizer drops tokens shorter than two characters, so "feature
        // number 1" and "feature number 2" produce identical embeddings while
        // their text differs. Exact-text dedup misses them. Promoting on each
        // one added a level per variant, which is the shape of the 2,525-deep
        // chain measured on the dogfood store — the headline defect was fixed
        // only for byte-identical text.
        let mut tree = MemTree::new_with_dim(8);
        let embedding = vec_on(0, 8, 0.0);
        for i in 0..40 {
            tree.insert(
                format!("How do I implement feature number {i} correctly?"),
                embedding.clone(),
                1,
            )
            .unwrap();
        }

        assert!(
            tree.max_depth() <= 2,
            "variants of one message must become siblings, not a chain; got \
             depth {} for 40 variants",
            tree.max_depth()
        );

        // Depth alone is not enough: attaching every variant to the root also
        // gives depth 2, and that is the wide-node half of the same defect.
        // The variants must land inside one cluster.
        let root_children = tree.get_node(0).unwrap().children.len();
        assert!(
            root_children <= 2,
            "variants must form a cluster, not spread across the root; got \
             {root_children} root children for 40 variants"
        );

        // And pin where the width actually goes, so the shape is asserted
        // rather than merely implied: one cluster head holding them all. This
        // is the fan-out the rule deliberately does not bound, and stating it
        // here means a change that re-spreads them cannot pass quietly.
        let widest_nonroot = tree
            .all_nodes()
            .values()
            .filter(|node| node.id != 0)
            .map(|node| node.children.len())
            .max()
            .unwrap_or(0);
        assert_eq!(
            widest_nonroot, 40,
            "the 40 variants must sit under one cluster head. The second insert \
             promotes the first, which puts TWO children under the head — the \
             moved original and the new variant — and the remaining 38 join \
             them"
        );

        let leaves = tree
            .all_nodes()
            .values()
            .filter(|node| node.id != 0 && node.children.is_empty())
            .count();
        assert_eq!(leaves, 40, "no variant may be lost or merged");
    }

    #[test]
    fn test_unrelated_content_still_attaches_to_the_root() {
        // Restored: deleted in the previous round and replaced by a weaker
        // property assertion, though it still held. Eight mutually orthogonal
        // memories have nothing to nest under, so each is its own root child.
        // This is the hard structural complement to the clustering test.
        let mut tree = MemTree::new_with_dim(16);
        for axis in 0..8 {
            tree.insert(format!("topic {axis}"), vec_on(axis, 16, 0.0), 1)
                .unwrap();
        }
        assert_eq!(
            tree.get_node(0).unwrap().children.len(),
            8,
            "orthogonal memories must not be forced together"
        );
    }

    #[test]
    fn test_dedup_raises_importance_but_not_lowers_it() {
        let mut tree = MemTree::new_with_dim(8);
        let embedding = vec_on(0, 8, 0.0);
        let first = tree
            .insert(
                "the deploy key lives in the vault".to_string(),
                embedding.clone(),
                1,
            )
            .unwrap();

        // Same fact, later stored explicitly at a higher tier.
        tree.insert(
            "the deploy key lives in the vault".to_string(),
            embedding.clone(),
            3,
        )
        .unwrap();
        assert_eq!(
            tree.get_node(first).unwrap().importance,
            3,
            "an explicit store must upgrade a fact first seen in passing"
        );

        // A later low-importance mention must not demote it.
        tree.insert(
            "the deploy key lives in the vault".to_string(),
            embedding,
            1,
        )
        .unwrap();
        assert_eq!(tree.get_node(first).unwrap().importance, 3);
    }

    #[test]
    fn test_insert_effect_reports_promotion_so_provenance_can_follow() {
        let mut tree = MemTree::new_with_dim(8);
        let first = tree
            .insert("first memory".to_string(), vec_on(0, 8, 0.0), 1)
            .unwrap();
        let effect = tree
            .insert_with_effect("second memory".to_string(), vec_on(0, 8, 0.2), 1)
            .unwrap();

        let (promoted, moved) = effect
            .promotion
            .expect("promoting a leaf must be reported so memory_sources can follow the text");
        assert_eq!(promoted, first, "the promoted node is the matched leaf");
        assert_eq!(
            tree.get_node(moved).unwrap().text,
            "first memory",
            "the moved child holds the original text, so provenance belongs there"
        );
        assert!(!effect.deduplicated);
    }

    #[test]
    fn test_retrieve_returns_leaves_not_provisional_internal_labels() {
        let mut tree = MemTree::new_with_dim(8);
        tree.insert("first memory".to_string(), vec_on(0, 8, 0.0), 1)
            .unwrap();
        tree.insert("second memory".to_string(), vec_on(0, 8, 0.2), 1)
            .unwrap();

        // The promoted parent duplicates a child's text. Returning both would
        // let one memory occupy several of the five context slots.
        let results = tree.retrieve(&vec_on(0, 8, 0.0), 10);
        let first_count = results
            .iter()
            .filter(|(_, text, _)| text == "first memory")
            .count();
        assert_eq!(first_count, 1, "a memory must appear once; got {results:?}");
    }

    #[test]
    fn test_memtree_creation() {
        let tree = MemTree::new();
        assert_eq!(tree.size(), 0); // No nodes except root
    }

    #[test]
    fn test_memtree_insert() {
        let mut tree = MemTree::new();
        let engine = TfIdfEmbedding::new();

        let emb = engine.embed("test text").unwrap();
        let node_id = tree.insert("test text".to_string(), emb, 1).unwrap();

        assert_eq!(tree.size(), 1);
        assert!(tree.get_node(node_id).is_some());
    }

    #[test]
    fn test_memtree_insert_multiple() {
        let mut tree = MemTree::new();
        let engine = TfIdfEmbedding::new();

        // Insert similar texts
        let texts = vec!["rust programming", "rust coding", "python programming"];

        for text in texts {
            let emb = engine.embed(text).unwrap();
            tree.insert(text.to_string(), emb, 1).unwrap();
        }

        // Three distinct memories are stored as three leaves. The tree may hold
        // more nodes than that: promoting a matched leaf to a parent moves its
        // content into a child and adds an internal node. Asserting
        // `size() == inserts` asserted flatness, which is the defect #250
        // describes.
        let leaves = tree
            .all_nodes()
            .values()
            .filter(|node| node.id != 0 && node.children.is_empty())
            .count();
        assert_eq!(
            leaves, 3,
            "each distinct memory must be stored exactly once"
        );
        assert!(
            tree.size() >= 3,
            "internal nodes are expected in addition to leaves"
        );
    }

    #[test]
    fn test_memtree_retrieve() {
        let mut tree = MemTree::new();
        let engine = TfIdfEmbedding::new();

        // Insert nodes
        let emb1 = engine.embed("rust programming").unwrap();
        tree.insert("rust programming".to_string(), emb1, 1)
            .unwrap();

        let emb2 = engine.embed("python coding").unwrap();
        tree.insert("python coding".to_string(), emb2, 1).unwrap();

        // Query
        let query_emb = engine.embed("rust").unwrap();
        let results = tree.retrieve(&query_emb, 2);

        assert_eq!(results.len(), 2);
        // "rust programming" should appear somewhere in results (ordering may vary due to
        // parent aggregation updating embeddings as the tree grows)
        assert!(results.iter().any(|(_, text, _)| text.contains("rust")));
    }

    #[test]
    fn test_memtree_hierarchy() {
        let mut tree = MemTree::new();
        let engine = TfIdfEmbedding::new();

        // Insert similar texts (should create hierarchy)
        let emb1 = engine.embed("rust").unwrap();
        let id1 = tree.insert("rust".to_string(), emb1, 1).unwrap();

        let emb2 = engine.embed("rust programming").unwrap();
        let id2 = tree
            .insert("rust programming".to_string(), emb2, 1)
            .unwrap();

        // Check hierarchy
        let node1 = tree.get_node(id1).unwrap();
        let node2 = tree.get_node(id2).unwrap();

        // These two texts are only weakly similar under the default TF-IDF
        // engine, so they legitimately remain siblings. The similarity is
        // pinned here because it exposes a calibration gap: the paper's
        // theta_0 = 0.4 is tuned for neural embeddings, while Finch's default
        // 2048-dimensional lexical vectors score much lower for text a reader
        // would call related. Tracked on #250.
        let measured = cosine_similarity(&node1.embedding, &node2.embedding);
        assert!(
            measured < BASE_SIMILARITY_THRESHOLD,
            "expected TF-IDF similarity below theta_0 for these inputs, got {measured}"
        );
        assert_eq!(
            node1.level, node2.level,
            "below the threshold, unrelated-enough memories are siblings"
        );

        // Content that IS close enough must form a hierarchy.
        let mut close = MemTree::new_with_dim(8);
        let base = close
            .insert("alpha".to_string(), vec_on(0, 8, 0.0), 1)
            .unwrap();
        let near = close
            .insert("alpha prime".to_string(), vec_on(0, 8, 0.2), 1)
            .unwrap();
        assert_ne!(
            close.get_node(base).unwrap().level,
            close.get_node(near).unwrap().level,
            "sufficiently similar content must nest"
        );
    }

    // --- Regression: node lookup uses ? not .unwrap() ---
    //
    // These tests verify that inserting many nodes succeeds without panicking
    // and that get_node returns None for unknown IDs instead of crashing.

    #[test]
    fn test_memtree_unknown_node_returns_none_not_panic() {
        let tree = MemTree::new();
        // Arbitrary IDs that don't exist in the tree (root is node 0, so skip it)
        assert!(tree.get_node(9999).is_none());
        assert!(tree.get_node(1000).is_none());
        // Root node (ID 0) always exists
        assert!(tree.get_node(0).is_some());
    }

    #[test]
    fn test_memtree_insert_many_does_not_panic() {
        // Previously, node-not-found during tree traversal would panic.
        // This test inserts enough nodes to exercise the traversal path.
        let mut tree = MemTree::new();
        let engine = TfIdfEmbedding::new();
        let texts = [
            "alpha",
            "beta",
            "gamma",
            "delta",
            "epsilon",
            "alpha variant",
            "beta variant",
            "gamma coding",
        ];
        for text in &texts {
            let emb = engine.embed(text).unwrap();
            tree.insert(text.to_string(), emb, 1).unwrap();
        }
        // Every distinct text is stored exactly once as a leaf. The node count
        // exceeds the input count because promoting a matched leaf adds an
        // internal node; asserting equality asserted a flat list.
        let leaves = tree
            .all_nodes()
            .values()
            .filter(|node| node.id != 0 && node.children.is_empty())
            .count();
        assert_eq!(leaves, texts.len(), "no memory may be lost or duplicated");
        assert!(
            tree.max_depth() <= texts.len(),
            "traversal must not degenerate into a chain"
        );
    }

    // ── Importance ───────────────────────────────────────────────────────────

    #[test]
    fn test_insert_stores_importance() {
        let mut tree = MemTree::new();
        let engine = TfIdfEmbedding::new();
        let emb = engine.embed("we decided to use anyhow").unwrap();
        let id = tree
            .insert("we decided to use anyhow".to_string(), emb, 3)
            .unwrap();
        assert_eq!(tree.get_node(id).unwrap().importance, 3);
    }

    #[test]
    fn test_critical_node_outranks_normal_node_in_retrieval() {
        // Insert a Normal node and a Critical node with similar content.
        // The Critical node should rank first even if slightly less similar.
        let mut tree = MemTree::new();
        let engine = TfIdfEmbedding::new();

        let emb_normal = engine.embed("rust programming tips").unwrap();
        tree.insert(
            "rust programming tips".to_string(),
            emb_normal,
            1, /* Normal */
        )
        .unwrap();

        let emb_critical = engine.embed("always use anyhow for rust errors").unwrap();
        tree.insert(
            "always use anyhow for rust errors".to_string(),
            emb_critical,
            3, /* Critical */
        )
        .unwrap();

        let query = engine.embed("rust").unwrap();
        let results = tree.retrieve(&query, 2);

        assert_eq!(results.len(), 2);
        // Critical node must appear at position 0 (highest weighted score)
        assert!(
            results[0].1.contains("always"),
            "Critical node should rank first: {:?}",
            results.iter().map(|(_, t, _)| t).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_discard_nodes_not_returned_in_retrieve() {
        let mut tree = MemTree::new();
        let engine = TfIdfEmbedding::new();

        let emb = engine.embed("ok").unwrap();
        tree.insert("ok".to_string(), emb, 0 /* Discard */).unwrap();

        let query = engine.embed("ok").unwrap();
        let results = tree.retrieve(&query, 5);

        // Discard-importance nodes must never be returned
        assert!(
            results.is_empty(),
            "Discard nodes must not appear in retrieval: {:?}",
            results
        );
    }
}

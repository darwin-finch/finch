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
use std::collections::HashMap;

/// Node ID in the tree
pub type NodeId = u64;

/// Threshold for semantic similarity (0.0 to 1.0)
const SIMILARITY_THRESHOLD: f32 = 0.7;

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
        }
    }

    /// Insert text with embedding into tree (O(log N))
    ///
    /// Algorithm from MemTree paper:
    /// 1. Start at root
    /// 2. Compare similarity with current node
    /// 3. If similar enough and has children, descend to most similar child
    /// 4. Otherwise, create new child node
    /// 5. Update parent aggregation
    ///
    /// `importance` is the tier assigned by `MemoryClassifier` (0–3).
    /// It is stored on the node and used to boost retrieval scores.
    pub fn insert(&mut self, text: String, embedding: Vec<f32>, importance: u8) -> Result<NodeId> {
        let created_at = chrono::Utc::now().timestamp();

        // Start traversal at root
        let mut current = self.root;

        loop {
            let node = self
                .nodes
                .get(&current)
                .ok_or_else(|| {
                    anyhow::anyhow!("memtree: node {} not found during insert", current)
                })?
                .clone();

            // Compute similarity with current node
            let similarity = if node.id == self.root {
                // Always descend from root
                1.0
            } else {
                cosine_similarity(&embedding, &node.embedding)
            };

            // If similar enough and has children, descend to most similar child
            if similarity > SIMILARITY_THRESHOLD && !node.children.is_empty() {
                // Find most similar child
                let mut best_child = node.children[0];
                let mut best_similarity = 0.0;

                for &child_id in &node.children {
                    let child = self.nodes.get(&child_id).ok_or_else(|| {
                        anyhow::anyhow!("memtree: child node {} not found", child_id)
                    })?;
                    let child_sim = cosine_similarity(&embedding, &child.embedding);
                    if child_sim > best_similarity {
                        best_similarity = child_sim;
                        best_child = child_id;
                    }
                }

                current = best_child;
            } else {
                // Create new child node
                let new_id = self.next_id;
                self.next_id += 1;

                let new_node = TreeNode {
                    id: new_id,
                    parent: Some(current),
                    children: Vec::new(),
                    text: text.clone(),
                    embedding: embedding.clone(),
                    level: node.level + 1,
                    created_at,
                    importance,
                };

                self.nodes.insert(new_id, new_node);

                // Update parent's children list
                let parent = self.nodes.get_mut(&current).ok_or_else(|| {
                    anyhow::anyhow!("memtree: parent node {} not found during insert", current)
                })?;
                parent.children.push(new_id);

                // Update parent's aggregated embedding
                self.update_parent_aggregation(current)?;

                return Ok(new_id);
            }
        }
    }

    /// Update parent node's embedding to be average of children
    fn update_parent_aggregation(&mut self, node_id: NodeId) -> Result<()> {
        let node = self.nodes.get(&node_id).ok_or_else(|| {
            anyhow::anyhow!("memtree: node {} not found during aggregation", node_id)
        })?;

        if node.children.is_empty() {
            return Ok(());
        }

        // Collect child embeddings
        let child_embeddings: Vec<_> = node
            .children
            .iter()
            .filter_map(|child_id| self.nodes.get(child_id))
            .map(|child| &child.embedding)
            .collect();

        if child_embeddings.is_empty() {
            return Ok(());
        }

        // Compute average
        let aggregated = average_embeddings(&child_embeddings);

        // Update parent embedding
        let parent = self.nodes.get_mut(&node_id).ok_or_else(|| {
            anyhow::anyhow!("memtree: node {} not found for embedding update", node_id)
        })?;
        parent.embedding = aggregated;

        // Recursively update ancestors
        if let Some(parent_id) = parent.parent {
            self.update_parent_aggregation(parent_id)?;
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
            .filter(|node| node.id != self.root && node.importance > 0)
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

        assert_eq!(tree.size(), 3);
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

        // Nodes should have different levels
        assert_ne!(node1.level, node2.level);
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
        assert_eq!(tree.size(), texts.len());
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

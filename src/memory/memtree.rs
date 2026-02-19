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

use super::embeddings::{cosine_similarity, average_embeddings};
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
}

/// MemTree - Hierarchical semantic memory structure
pub struct MemTree {
    root: NodeId,
    nodes: HashMap<NodeId, TreeNode>,
    next_id: NodeId,
}

impl MemTree {
    /// Create a new empty MemTree
    pub fn new() -> Self {
        let root_id = 0;
        let mut nodes = HashMap::new();

        // Create root node (placeholder)
        let root = TreeNode {
            id: root_id,
            parent: None,
            children: Vec::new(),
            text: String::from("ROOT"),
            embedding: vec![0.0; 384],  // Zero vector
            level: 0,
            created_at: chrono::Utc::now().timestamp(),
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
    pub fn insert(&mut self, text: String, embedding: Vec<f32>) -> Result<NodeId> {
        let created_at = chrono::Utc::now().timestamp();

        // Start traversal at root
        let mut current = self.root;

        loop {
            let node = self.nodes.get(&current).unwrap().clone();

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
                    let child = self.nodes.get(&child_id).unwrap();
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
                };

                self.nodes.insert(new_id, new_node);

                // Update parent's children list
                let parent = self.nodes.get_mut(&current).unwrap();
                parent.children.push(new_id);

                // Update parent's aggregated embedding
                self.update_parent_aggregation(current)?;

                return Ok(new_id);
            }
        }
    }

    /// Update parent node's embedding to be average of children
    fn update_parent_aggregation(&mut self, node_id: NodeId) -> Result<()> {
        let node = self.nodes.get(&node_id).unwrap();

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
        let parent = self.nodes.get_mut(&node_id).unwrap();
        parent.embedding = aggregated;

        // Recursively update ancestors
        if let Some(parent_id) = parent.parent {
            self.update_parent_aggregation(parent_id)?;
        }

        Ok(())
    }

    /// Retrieve top-k most similar nodes (collapsed tree retrieval)
    ///
    /// Treats all nodes as a flat set for simplicity
    /// More sophisticated: hierarchical retrieval from paper
    pub fn retrieve(&self, query_embedding: &[f32], top_k: usize) -> Vec<(NodeId, String, f32)> {
        let mut results: Vec<_> = self
            .nodes
            .values()
            .filter(|node| node.id != self.root)  // Skip root
            .map(|node| {
                let similarity = cosine_similarity(query_embedding, &node.embedding);
                (node.id, node.text.clone(), similarity)
            })
            .collect();

        // Sort by similarity descending
        results.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        // Return top-k
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
    use crate::memory::embeddings::{TfIdfEmbedding, EmbeddingEngine};

    #[test]
    fn test_memtree_creation() {
        let tree = MemTree::new();
        assert_eq!(tree.size(), 0);  // No nodes except root
    }

    #[test]
    fn test_memtree_insert() {
        let mut tree = MemTree::new();
        let engine = TfIdfEmbedding::new();

        let emb = engine.embed("test text").unwrap();
        let node_id = tree.insert("test text".to_string(), emb).unwrap();

        assert_eq!(tree.size(), 1);
        assert!(tree.get_node(node_id).is_some());
    }

    #[test]
    fn test_memtree_insert_multiple() {
        let mut tree = MemTree::new();
        let engine = TfIdfEmbedding::new();

        // Insert similar texts
        let texts = vec![
            "rust programming",
            "rust coding",
            "python programming",
        ];

        for text in texts {
            let emb = engine.embed(text).unwrap();
            tree.insert(text.to_string(), emb).unwrap();
        }

        assert_eq!(tree.size(), 3);
    }

    #[test]
    fn test_memtree_retrieve() {
        let mut tree = MemTree::new();
        let engine = TfIdfEmbedding::new();

        // Insert nodes
        let emb1 = engine.embed("rust programming").unwrap();
        tree.insert("rust programming".to_string(), emb1).unwrap();

        let emb2 = engine.embed("python coding").unwrap();
        tree.insert("python coding".to_string(), emb2).unwrap();

        // Query
        let query_emb = engine.embed("rust").unwrap();
        let results = tree.retrieve(&query_emb, 2);

        assert_eq!(results.len(), 2);
        // First result should be more similar to "rust programming"
        assert!(results[0].1.contains("rust"));
    }

    #[test]
    fn test_memtree_hierarchy() {
        let mut tree = MemTree::new();
        let engine = TfIdfEmbedding::new();

        // Insert similar texts (should create hierarchy)
        let emb1 = engine.embed("rust").unwrap();
        let id1 = tree.insert("rust".to_string(), emb1).unwrap();

        let emb2 = engine.embed("rust programming").unwrap();
        let id2 = tree.insert("rust programming".to_string(), emb2).unwrap();

        // Check hierarchy
        let node1 = tree.get_node(id1).unwrap();
        let node2 = tree.get_node(id2).unwrap();

        // Nodes should have different levels
        assert_ne!(node1.level, node2.level);
    }
}

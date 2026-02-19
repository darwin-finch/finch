// MemTree implementation
//
// O(log N) insertion, hierarchical semantic navigation

use anyhow::Result;
use std::collections::HashMap;

pub type NodeId = usize;

/// A node in the MemTree
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub text: String,
    pub embedding: Vec<f32>,
    pub level: usize,
}

/// MemTree - hierarchical semantic memory structure
pub struct MemTree {
    root: NodeId,
    nodes: HashMap<NodeId, TreeNode>,
    next_id: NodeId,
}

impl MemTree {
    /// Create new empty tree
    pub fn new() -> Self {
        let root = TreeNode {
            id: 0,
            parent: None,
            children: Vec::new(),
            text: "<root>".to_string(),
            embedding: vec![0.0; 384], // TODO: Proper embedding size
            level: 0,
        };

        let mut nodes = HashMap::new();
        nodes.insert(0, root);

        Self {
            root: 0,
            nodes,
            next_id: 1,
        }
    }

    /// Insert new node (O(log N) traversal)
    pub fn insert(&mut self, _text: String, _embedding: Vec<f32>) -> Result<NodeId> {
        // TODO: Traverse tree comparing semantic similarity
        // TODO: Create new leaf node at appropriate location
        // TODO: Update parent aggregations
        let new_id = self.next_id;
        self.next_id += 1;
        Ok(new_id)
    }

    /// Retrieve top-k most similar nodes
    pub fn retrieve(&self, _query_embedding: &[f32], _top_k: usize) -> Result<Vec<String>> {
        // TODO: Collapsed tree retrieval (treat all nodes as flat set)
        // TODO: Compute similarity for all nodes
        // TODO: Sort by similarity
        // TODO: Return top-k texts
        Ok(Vec::new())
    }

    /// Update parent node's aggregated embedding
    fn update_parent_aggregation(&mut self, _node_id: NodeId) -> Result<()> {
        // TODO: Compute weighted average of children embeddings
        // TODO: Recursively update ancestors
        Ok(())
    }
}

impl Default for MemTree {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute cosine similarity between two embeddings
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

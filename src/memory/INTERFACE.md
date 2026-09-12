# memory — public interface

Generated from [`src/memory/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/memory/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)
- **May depend on:** nothing. Debt: `programs`.

Everything below is what callers outside this subsystem can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Canonical source identity for a conversation pair projected from one successful named-Brain run.
pub struct BrainConversationProvenance { … }
/// Summary of conversation topics derived from MemTree centroid queries.
pub struct ConversationSummaryLines { … }
pub(crate) struct HydrationBatchPauseRegistration { … }
/// Progress of the background MemTree hydration.
pub enum HydrationStatus { Ready, Loading, Degraded, Failed }
pub struct InspectedMemory { … }
/// MemTree - Hierarchical semantic memory structure
pub struct MemTree { … }
impl MemTree {
    /// Get all nodes (for serialization)
    pub fn all_nodes(&self) -> &HashMap<NodeId, TreeNode>;
    /// Get node by ID
    pub fn get_node(&self, id: NodeId) -> Option<&TreeNode>;
    /// Insert text with embedding into the tree.
    pub fn insert(&mut self, text: String, embedding: Vec<f32>, importance: u8) -> Result<NodeId>;
    /// Insert, reporting what happened so durable provenance can be kept correct.
    pub fn insert_with_effect(&mut self, text: String, embedding: Vec<f32>, importance: u8) -> Result<InsertEffect>;
    /// Deepest level present in the tree.
    pub fn max_depth(&self) -> usize;
    /// Create a new empty MemTree with the TF-IDF default embedding dimension (2048).
    pub fn new() -> Self;
    /// Create a new empty MemTree with a specified root embedding dimension.
    pub fn new_with_dim(dim: usize) -> Self;
    /// Retrieve top-k most relevant nodes (flat retrieval with importance weighting).
    pub fn retrieve(&self, query_embedding: &[f32], top_k: usize) -> Vec<(NodeId, String, f32)>;
    /// Set the next_id counter (used after loading from disk to avoid ID collisions).
    pub fn set_next_id(&mut self, id: NodeId);
    /// Get tree size (number of nodes excluding root)
    pub fn size(&self) -> usize;
}
/// Classifies and pre-processes a conversation turn for MemTree storage.
pub struct MemoryClassifier;
impl MemoryClassifier {
    pub fn new() -> Self;
    /// Decide whether to add this turn to MemTree, and if so: - what key content to store (extracted/compressed prose) - which importance tier to assign  Returns `N…
    pub fn process(&self, role: &str, content: &str) -> Option<(String, MemoryImportance)>;
}
/// Configuration for memory system
pub struct MemoryConfig { … }
/// How important a piece of content is for long-term memory.
pub enum MemoryImportance { Discard, Normal, High, Critical }
impl MemoryImportance {
    /// Canonical DB representation (0–3).
    pub fn as_u8(self) -> u8;
    /// Reconstruct from the DB value; unknown values fall back to Normal.
    pub fn from_u8(v: u8) -> Self;
    /// Multiplier applied to cosine similarity during retrieval so high-signal memories surface first even when slightly less semantically similar.
    pub fn retrieval_boost(self) -> f32;
}
pub struct MemorySearchResult { … }
/// Stable metadata for the canonical conversation row behind one semantic memory.
pub struct MemorySourceMetadata { … }
/// Memory statistics
pub struct MemoryStats { … }
/// Memory system with MemTree and SQLite storage
pub struct MemorySystem { … }
/// ONNX sentence transformer embedding engine.
pub struct NeuralEmbeddingEngine { … }
impl NeuralEmbeddingEngine {
    /// Async version: download model using a blocking thread pool.
    pub async fn ensure_downloaded() -> Result<PathBuf>;
    /// Download the embedding model from HuggingFace if not already cached.
    pub fn download_sync() -> Result<PathBuf>;
    /// Try to find the model in the HuggingFace cache without downloading.
    pub fn find_in_cache() -> Option<PathBuf>;
    /// Load a pre-downloaded embedding model from a directory.
    pub fn load(model_dir: &Path) -> Result<Self>;
}
/// Node ID in the tree
pub type NodeId = u64;
/// Word + character n-gram TF-IDF embedding engine  Dramatically better than a pure hash approach: - Tokenises into words (lowercase, alphanumeric) - Generates…
pub struct TfIdfEmbedding { … }
impl TfIdfEmbedding {
    pub fn new() -> Self;
}
/// A node in the MemTree
pub struct TreeNode { … }
```

## Traits

```rust
/// Trait for embedding engines
pub trait EmbeddingEngine: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
    fn dimension(&self) -> usize;
}
```

## Functions

```rust
/// Compute the average of multiple unit embeddings (then re-normalise)
pub fn average_embeddings(embeddings: &[&Vec<f32>]) -> Vec<f32> { … }
/// Compute cosine similarity between two embedding vectors
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 { … }
/// Hold the production loader after `after_loaded` nodes so a test can observe a genuinely `Loading` index.
pub(crate) fn register_hydration_batch_pause(path: PathBuf, after_loaded: usize) -> (HydrationBatchPauseRegistration, watch::Receiver<bool>, watch::Sender<bool>) { … }
```

## Constants

```rust
/// Nodes hydrated per batch.
pub(crate) const HYDRATION_BATCH: usize = 512;
```

## Referenced but not exported

These types appear in the signatures above but the facade does not export them, so a caller can hold a value and never name its type. Export them or change the signature: `InsertEffect`

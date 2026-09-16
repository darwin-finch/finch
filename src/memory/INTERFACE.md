# memory — public interface

Generated from [`src/memory/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/memory/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Canonical source identity for a conversation pair projected from one successful named-Brain run.
pub struct BrainConversationProvenance { … }
/// Summary of conversation topics derived from MemTree centroid queries.
pub struct ConversationSummaryLines { … }
#[cfg(test)]
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
    /// Whether the content is template noise that must never become a memory.
    pub fn is_noise(&self, content: &str) -> bool;
    /// Whether recall must hold this text back.
    pub fn is_recall_noise(&self, content: &str) -> bool;
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
impl MemorySystem {
    /// Derive a short topic summary without any LLM call.
    pub async fn conversation_summary(&self, depth: usize) -> Result<ConversationSummaryLines>;
    /// Derive context-summary lines from one Finch session only.
    pub async fn conversation_summary_for_session(&self, session_id: &str, depth: usize) -> Result<ConversationSummaryLines>;
    /// Wait until every persisted node is in memory.
    pub async fn ensure_hydrated(&self) -> Result<()>;
    /// Look up one immutable program-index version.
    pub async fn get_program_index(&self, id: &str, version: u64) -> Result<Option<ProgramIndexRecord>>;
    /// Resolve the newest non-deprecated version of a scoped program name.
    pub async fn get_program_index_by_name(&self, name: &str, language: Option<&str>) -> Result<Option<ProgramIndexRecord>>;
    /// Get recent conversations (for context window)
    pub async fn get_recent_conversations(&self, limit: usize) -> Result<Vec<(String, String)>>;
    /// Update the rebuildable SQLite projection for one source-backed definition.
    pub async fn index_program_record(&self, mut record: ProgramIndexRecord) -> Result<ProgramIndexRef>;
    /// Index many records in one transaction.
    pub async fn index_program_records(&self, records: Vec<ProgramIndexRecord>) -> Result<usize>;
    /// Structural summary of the semantic index: (leaf count, max depth, widest fan-out below the root).
    pub async fn index_shape(&self) -> (usize, usize, usize);
    /// Insert one side of a successful named-Brain turn exactly once.
    pub async fn insert_brain_conversation(&self, role: &str, content: &str, model: Option<&str>, session_id: Option<&str>, provenance: &BrainConversationProvenance) -> Result<bool>;
    /// Insert a conversation turn into memory
    pub async fn insert_conversation(&self, role: &str, content: &str, model: Option<&str>, session_id: Option<&str>) -> Result<()>;
    /// Resolve the stable ID returned by `query_with_sources`.
    pub async fn inspect_memory(&self, memory_id: &str) -> Result<Option<InspectedMemory>>;
    /// Load canonical index rows for every current non-deprecated version.
    pub async fn latest_program_indexes(&self) -> Result<Vec<ProgramIndexRecord>>;
    /// Load legacy persisted Lisp definitions for explicit migration tooling.
    pub async fn load_lisp_defines(&self) -> Result<Vec<String>>;
    /// Current monotonic registry generation used to invalidate stale manifests.
    pub async fn program_registry_generation(&self) -> Result<u64>;
    /// Query memory for relevant context.
    pub async fn query(&self, query_text: &str, top_k: Option<usize>) -> Result<Vec<String>>;
    /// Query semantic memory while retaining a stable reference to the canonical stored turn behind every new-format leaf.
    pub async fn query_with_sources(&self, query_text: &str, top_k: Option<usize>) -> Result<Vec<MemorySearchResult>>;
    /// Project every conversation that was stored but never indexed, and report how many were repaired.
    pub async fn recover_pending_projections(&self) -> Result<usize>;
    /// Persist a successful Lisp `(define ...)` expression for session replay.
    pub async fn save_lisp_define(&self, expr: &str) -> Result<()>;
    /// Search current program-index versions using a compact lexical score.
    pub async fn search_program_indexes(&self, query: &str, limit: usize) -> Result<Vec<ProgramIndexRecord>>;
    /// Get memory statistics
    pub async fn stats(&self) -> Result<MemoryStats>;
    /// Progress of the background hydration, for status surfaces.
    pub fn hydration_status(&self) -> HydrationStatus;
    /// Create a new memory system with the TF-IDF fallback engine.
    pub fn new(config: MemoryConfig) -> Result<Self>;
    /// Create a memory system that embeds with the caller-supplied engine.
    pub fn new_with_engine(config: MemoryConfig, embedding_engine: Arc<dyn EmbeddingEngine>) -> Result<Self>;
    /// Root containing user-readable program sources beside the memory database.
    pub fn program_source_root(&self) -> PathBuf;
}
/// Node ID in the tree
pub type NodeId = u64;
/// One stored program-index row.
pub struct ProgramIndexRecord { … }
/// Identity of one immutable program-index version.
pub struct ProgramIndexRef { … }
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
#[cfg(test)]
pub(crate) fn register_hydration_batch_pause(path: PathBuf, after_loaded: usize) -> (HydrationBatchPauseRegistration, watch::Receiver<bool>, watch::Sender<bool>) { … }
```

## Constants

```rust
/// Nodes hydrated per batch.
pub(crate) const HYDRATION_BATCH: usize = 512;
```

## Referenced but not exported

These types appear in the signatures above but the facade does not export them, so a caller can hold a value and never name its type. Export them or change the signature: `InsertEffect`

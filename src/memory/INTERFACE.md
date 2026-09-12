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
/// Classifies and pre-processes a conversation turn for MemTree storage.
pub struct MemoryClassifier;
/// Configuration for memory system
pub struct MemoryConfig { … }
/// How important a piece of content is for long-term memory.
pub enum MemoryImportance { Discard, Normal, High, Critical }
pub struct MemorySearchResult { … }
/// Stable metadata for the canonical conversation row behind one semantic memory.
pub struct MemorySourceMetadata { … }
/// Memory statistics
pub struct MemoryStats { … }
/// Memory system with MemTree and SQLite storage
pub struct MemorySystem { … }
/// ONNX sentence transformer embedding engine.
pub struct NeuralEmbeddingEngine { … }
/// Node ID in the tree
pub type NodeId = u64;
/// Word + character n-gram TF-IDF embedding engine  Dramatically better than a pure hash approach: - Tokenises into words (lowercase, alphanumeric) - Generates…
pub struct TfIdfEmbedding { … }
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

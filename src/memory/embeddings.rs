// Embedding engine for semantic similarity
//
// Converts text to vector embeddings for MemTree

use anyhow::Result;

/// Trait for embedding engines
pub trait EmbeddingEngine: Send + Sync {
    /// Generate embedding vector for text
    fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Get embedding dimension
    fn dimension(&self) -> usize;
}

/// Simple TF-IDF embedding engine (placeholder for now)
///
/// In production, this would use:
/// - Sentence transformers (all-MiniLM-L6-v2)
/// - Local model's hidden states
/// - OpenAI embeddings API
///
/// For MVP, we use simple TF-IDF which is fast and requires no ML model
pub struct TfIdfEmbedding {
    dimension: usize,
}

impl TfIdfEmbedding {
    pub fn new() -> Self {
        Self {
            dimension: 384,  // Standard embedding dimension
        }
    }

    /// Simple hash-based embedding (deterministic, fast)
    ///
    /// This is a placeholder implementation. In production, use:
    /// - sentence-transformers for quality
    /// - Local ONNX model for privacy
    /// - OpenAI embeddings for ease
    fn simple_hash_embed(&self, text: &str) -> Vec<f32> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut embedding = vec![0.0; self.dimension];

        // Split into words and generate features
        let words: Vec<&str> = text.split_whitespace().collect();

        for (i, word) in words.iter().enumerate() {
            let mut hasher = DefaultHasher::new();
            word.hash(&mut hasher);
            let hash = hasher.finish();

            // Map hash to multiple dimensions
            for j in 0..4 {
                let idx = ((hash >> (j * 16)) as usize + i) % self.dimension;
                embedding[idx] += 1.0;
            }
        }

        // Normalize to unit vector
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut embedding {
                *x /= norm;
            }
        }

        embedding
    }
}

impl EmbeddingEngine for TfIdfEmbedding {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.simple_hash_embed(text))
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

impl Default for TfIdfEmbedding {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute cosine similarity between two embeddings
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

/// Compute average of multiple embeddings
pub fn average_embeddings(embeddings: &[&Vec<f32>]) -> Vec<f32> {
    if embeddings.is_empty() {
        return Vec::new();
    }

    let dim = embeddings[0].len();
    let mut avg = vec![0.0; dim];

    for emb in embeddings {
        for (i, val) in emb.iter().enumerate() {
            if i < dim {
                avg[i] += val;
            }
        }
    }

    let count = embeddings.len() as f32;
    for val in &mut avg {
        *val /= count;
    }

    // Normalize to unit vector
    let norm: f32 = avg.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut avg {
            *x /= norm;
        }
    }

    avg
}

#[cfg(test)]
mod tests {
    use super::*;
    // Import trait to use embed() method
    use super::EmbeddingEngine;

    #[test]
    fn test_embedding_dimension() {
        let engine = TfIdfEmbedding::new();
        assert_eq!(engine.dimension(), 384);
    }

    #[test]
    fn test_embedding_generation() {
        let engine = TfIdfEmbedding::new();
        let emb = engine.embed("Hello world").unwrap();
        assert_eq!(emb.len(), 384);
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let engine = TfIdfEmbedding::new();
        let emb1 = engine.embed("test").unwrap();
        let emb2 = engine.embed("test").unwrap();
        let sim = cosine_similarity(&emb1, &emb2);

        // Should be very close to 1.0 (identical)
        assert!((sim - 1.0).abs() < 0.01, "Similarity: {}", sim);
    }

    #[test]
    fn test_cosine_similarity_different() {
        let engine = TfIdfEmbedding::new();
        let emb1 = engine.embed("rust programming").unwrap();
        let emb2 = engine.embed("python data science").unwrap();
        let sim = cosine_similarity(&emb1, &emb2);

        // Should be less than 1.0 (different)
        assert!(sim < 1.0);
    }

    #[test]
    fn test_average_embeddings() {
        let engine = TfIdfEmbedding::new();
        let emb1 = engine.embed("test1").unwrap();
        let emb2 = engine.embed("test2").unwrap();

        let avg = average_embeddings(&[&emb1, &emb2]);
        assert_eq!(avg.len(), 384);
    }
}

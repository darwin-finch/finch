// Embedding engine for semantic similarity
//
// Converts text to vector embeddings for MemTree insertion and retrieval.

use anyhow::Result;

/// Trait for embedding engines
pub trait EmbeddingEngine: Send + Sync {
    /// Generate embedding vector for text
    fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Get embedding dimension
    fn dimension(&self) -> usize;
}

/// Word + character n-gram TF-IDF embedding engine
///
/// Dramatically better than a pure hash approach:
/// - Tokenises into words (lowercase, alphanumeric)
/// - Generates character bigrams and trigrams from each word
/// - Maps every token to multiple dimensions via FNV-64 (fewer collisions than DefaultHasher)
/// - Applies a simple IDF proxy: shorter tokens get lower weight (common stop-words de-emphasised)
/// - Normalises to a unit vector
///
/// Quality is sufficient for technical-text retrieval (code discussions, function names,
/// error messages) where terms are distinctive. A neural sentence transformer will be
/// added as an optional upgrade once the ONNX infrastructure is ready.
pub struct TfIdfEmbedding {
    dimension: usize,
}

impl TfIdfEmbedding {
    pub fn new() -> Self {
        Self { dimension: 2048 }
    }

    /// FNV-64 hash — fewer collisions than DefaultHasher for short strings
    fn fnv64(bytes: &[u8]) -> u64 {
        const OFFSET: u64 = 0xcbf29ce484222325;
        const PRIME: u64 = 0x00000100000001b3;
        let mut h = OFFSET;
        for &b in bytes {
            h = h.wrapping_mul(PRIME);
            h ^= b as u64;
        }
        h
    }

    /// Map a token string to `slots` positions in a `dim`-dimensional vector,
    /// adding `weight` to each position.
    fn add_token(embedding: &mut [f32], token: &str, weight: f32, slots: usize) {
        let dim = embedding.len();
        let bytes = token.as_bytes();
        for slot in 0..slots {
            // Mix slot index into the hash so each slot lands in a different bucket
            let mut mixed = bytes.to_vec();
            mixed.push(slot as u8);
            let h = Self::fnv64(&mixed) as usize;
            embedding[h % dim] += weight;
        }
    }

    fn embed_text(&self, text: &str) -> Vec<f32> {
        let mut embedding = vec![0.0f32; self.dimension];

        // Tokenise: lowercase, split on non-alphanumeric
        let words: Vec<String> = text
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| w.len() >= 2)
            .map(|w| w.to_string())
            .collect();

        for word in &words {
            // IDF proxy: weight by log(len+1) so single-char tokens weigh less
            // and rare long words weigh more.
            let word_weight = (word.len() as f32 + 1.0).ln();

            // Whole-word token (4 slots for good coverage)
            Self::add_token(&mut embedding, word, word_weight, 4);

            // Character bigrams
            let chars: Vec<char> = word.chars().collect();
            for i in 0..chars.len().saturating_sub(1) {
                let bigram: String = chars[i..i + 2].iter().collect();
                Self::add_token(&mut embedding, &bigram, word_weight * 0.4, 2);
            }

            // Character trigrams (most useful for technical terms)
            for i in 0..chars.len().saturating_sub(2) {
                let trigram: String = chars[i..i + 3].iter().collect();
                Self::add_token(&mut embedding, &trigram, word_weight * 0.6, 3);
            }
        }

        // L2 normalise to unit vector
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut embedding {
                *x /= norm;
            }
        }

        embedding
    }
}

impl Default for TfIdfEmbedding {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingEngine for TfIdfEmbedding {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.embed_text(text))
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

/// Compute cosine similarity between two embedding vectors
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

/// Compute the average of multiple unit embeddings (then re-normalise)
pub fn average_embeddings(embeddings: &[&Vec<f32>]) -> Vec<f32> {
    if embeddings.is_empty() {
        return Vec::new();
    }
    let dim = embeddings[0].len();
    let mut avg = vec![0.0f32; dim];
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

    #[test]
    fn test_embedding_dimension() {
        let engine = TfIdfEmbedding::new();
        assert_eq!(engine.dimension(), 2048);
    }

    #[test]
    fn test_embedding_generation() {
        let engine = TfIdfEmbedding::new();
        let emb = engine.embed("Hello world").unwrap();
        assert_eq!(emb.len(), 2048);
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let engine = TfIdfEmbedding::new();
        let emb1 = engine.embed("rust lifetimes borrow checker").unwrap();
        let emb2 = engine.embed("rust lifetimes borrow checker").unwrap();
        let sim = cosine_similarity(&emb1, &emb2);
        assert!((sim - 1.0).abs() < 0.001, "Identical texts should have similarity ~1.0, got {}", sim);
    }

    #[test]
    fn test_cosine_similarity_related() {
        let engine = TfIdfEmbedding::new();
        let emb1 = engine.embed("rust async await tokio").unwrap();
        let emb2 = engine.embed("rust async programming tokio runtime").unwrap();
        let emb3 = engine.embed("python machine learning pandas numpy").unwrap();

        let sim_related = cosine_similarity(&emb1, &emb2);
        let sim_unrelated = cosine_similarity(&emb1, &emb3);

        // Related texts should score higher than unrelated
        assert!(
            sim_related > sim_unrelated,
            "Related texts (sim={:.3}) should outscore unrelated (sim={:.3})",
            sim_related,
            sim_unrelated
        );
    }

    #[test]
    fn test_cosine_similarity_technical_terms() {
        let engine = TfIdfEmbedding::new();

        // These share character n-grams ("ort", "sort") but different semantics — just checks non-crash
        let emb1 = engine.embed("quicksort algorithm").unwrap();
        let emb2 = engine.embed("ONNX runtime ort").unwrap();
        let sim = cosine_similarity(&emb1, &emb2);
        assert!(sim >= 0.0 && sim <= 1.0);
    }

    #[test]
    fn test_embedding_is_unit_vector() {
        let engine = TfIdfEmbedding::new();
        let emb = engine.embed("the quick brown fox jumps over the lazy dog").unwrap();
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.001, "Embedding should be a unit vector, norm={}", norm);
    }

    #[test]
    fn test_empty_text() {
        let engine = TfIdfEmbedding::new();
        let emb = engine.embed("").unwrap();
        assert_eq!(emb.len(), 2048);
        // Zero vector for empty input
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert_eq!(norm, 0.0);
    }

    #[test]
    fn test_average_embeddings() {
        let engine = TfIdfEmbedding::new();
        let emb1 = engine.embed("test one").unwrap();
        let emb2 = engine.embed("test two").unwrap();
        let avg = average_embeddings(&[&emb1, &emb2]);
        assert_eq!(avg.len(), 2048);
        // Average should be a unit vector
        let norm: f32 = avg.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_coding_term_similarity() {
        let engine = TfIdfEmbedding::new();

        // Two descriptions of the same concept should score well
        let e1 = engine.embed("authentication JWT token bearer header").unwrap();
        let e2 = engine.embed("auth JWT bearer token authorization").unwrap();
        let e3 = engine.embed("database migration schema alter table column").unwrap();

        let sim_auth = cosine_similarity(&e1, &e2);
        let sim_cross = cosine_similarity(&e1, &e3);

        assert!(
            sim_auth > sim_cross,
            "Auth texts (sim={:.3}) should outscore cross-domain (sim={:.3})",
            sim_auth,
            sim_cross
        );
    }
}

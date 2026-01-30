// TF-IDF pattern matcher

use rust_stemmers::{Algorithm, Stemmer};
use std::collections::HashMap;

use super::library::{Pattern, PatternLibrary};

pub struct PatternMatcher {
    library: PatternLibrary,
    stemmer: Stemmer,
    similarity_threshold: f64,
}

impl PatternMatcher {
    pub fn new(library: PatternLibrary, similarity_threshold: f64) -> Self {
        Self {
            library,
            stemmer: Stemmer::create(Algorithm::English),
            similarity_threshold,
        }
    }

    /// Find the best matching pattern for a query
    pub fn find_match(&self, query: &str) -> Option<(Pattern, f64)> {
        let query_tokens = self.tokenize_and_stem(query);
        let query_vec = self.create_bow(&query_tokens);

        tracing::debug!("Query tokens: {:?}", query_tokens);

        let mut best_match: Option<(Pattern, f64)> = None;

        for pattern in &self.library.patterns {
            let pattern_tokens: Vec<String> = pattern
                .keywords
                .iter()
                .flat_map(|kw| self.tokenize_and_stem(kw))
                .collect();

            let pattern_vec = self.create_bow(&pattern_tokens);
            let similarity = self.cosine_similarity(&query_vec, &pattern_vec);

            tracing::debug!(
                "Pattern {} similarity: {:.3} (tokens: {:?})",
                pattern.id,
                similarity,
                pattern_tokens
            );

            if similarity >= self.similarity_threshold {
                if let Some((_, best_sim)) = &best_match {
                    if similarity > *best_sim {
                        best_match = Some((pattern.clone(), similarity));
                    }
                } else {
                    best_match = Some((pattern.clone(), similarity));
                }
            }
        }

        best_match
    }

    /// Tokenize and stem text
    fn tokenize_and_stem(&self, text: &str) -> Vec<String> {
        text.to_lowercase()
            .split_whitespace()
            .map(|word| {
                // Remove punctuation
                let clean_word: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
                self.stemmer.stem(&clean_word).to_string()
            })
            .filter(|word| !word.is_empty())
            .collect()
    }

    /// Create bag-of-words vector
    fn create_bow(&self, tokens: &[String]) -> HashMap<String, f64> {
        let mut bow: HashMap<String, f64> = HashMap::new();
        for token in tokens {
            *bow.entry(token.clone()).or_insert(0.0) += 1.0;
        }

        // Normalize
        let total: f64 = bow.values().sum();
        if total > 0.0 {
            for value in bow.values_mut() {
                *value /= total;
            }
        }

        bow
    }

    /// Calculate cosine similarity between two bag-of-words vectors
    fn cosine_similarity(&self, vec1: &HashMap<String, f64>, vec2: &HashMap<String, f64>) -> f64 {
        let mut dot_product = 0.0;
        let mut mag1 = 0.0;
        let mut mag2 = 0.0;

        // Get all unique words
        let all_words: std::collections::HashSet<_> = vec1.keys().chain(vec2.keys()).collect();

        for word in all_words {
            let v1 = vec1.get(word).unwrap_or(&0.0);
            let v2 = vec2.get(word).unwrap_or(&0.0);

            dot_product += v1 * v2;
            mag1 += v1 * v1;
            mag2 += v2 * v2;
        }

        if mag1 == 0.0 || mag2 == 0.0 {
            return 0.0;
        }

        dot_product / (mag1.sqrt() * mag2.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_and_stem() {
        let library = PatternLibrary { patterns: vec![] };
        let matcher = PatternMatcher::new(library, 0.85);

        let tokens = matcher.tokenize_and_stem("Running quickly!");
        assert!(tokens.contains(&"run".to_string()));
        assert!(tokens.contains(&"quick".to_string()));
    }

    #[test]
    fn test_cosine_similarity() {
        let library = PatternLibrary { patterns: vec![] };
        let matcher = PatternMatcher::new(library, 0.85);

        let mut vec1 = HashMap::new();
        vec1.insert("hello".to_string(), 1.0);

        let mut vec2 = HashMap::new();
        vec2.insert("hello".to_string(), 1.0);

        let sim = matcher.cosine_similarity(&vec1, &vec2);
        assert!((sim - 1.0).abs() < 0.001);
    }
}

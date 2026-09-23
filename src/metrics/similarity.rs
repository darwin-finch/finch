// Semantic similarity calculation for comparing local and Claude responses

use anyhow::Result;
use std::collections::HashSet;

/// Calculate semantic similarity between two texts (0.0 = different, 1.0 = identical)
///
/// Phase 1: Uses Jaccard similarity (word overlap) as a fast approximation
/// TODO: Replace with an injected semantic-embedding implementation.
pub fn semantic_similarity(text1: &str, text2: &str) -> Result<f64> {
    let words1 = tokenize(text1);
    let words2 = tokenize(text2);

    if words1.is_empty() && words2.is_empty() {
        return Ok(1.0); // Both empty = identical
    }

    let intersection = words1.intersection(&words2).count();
    let union = words1.union(&words2).count();

    if union == 0 {
        return Ok(0.0);
    }

    // Jaccard similarity: |A ∩ B| / |A ∪ B|
    Ok(intersection as f64 / union as f64)
}

/// Tokenize text into normalized words
fn tokenize(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty() && w.len() > 2) // Skip very short words
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_texts() {
        let text = "Rust is a systems programming language";
        let sim = semantic_similarity(text, text).unwrap();
        assert_eq!(sim, 1.0);
    }

    #[test]
    fn test_completely_different() {
        let text1 = "Rust programming language";
        let text2 = "Python web framework";
        let sim = semantic_similarity(text1, text2).unwrap();
        assert!(sim < 0.3); // Very low similarity
    }

    #[test]
    fn test_partial_overlap() {
        let text1 = "Rust is a systems programming language focused on safety";
        let text2 = "Rust is a modern programming language with strong types";
        let sim = semantic_similarity(text1, text2).unwrap();
        // Jaccard similarity is typically lower than semantic embeddings would give
        // Both have "rust", "programming", "language" after filtering
        // Should show some overlap but not be identical
        assert!(sim > 0.2 && sim < 0.8); // Moderate similarity with word overlap
        println!("Actual similarity: {:.2}", sim); // For debugging
    }

    #[test]
    fn test_empty_texts() {
        let sim = semantic_similarity("", "").unwrap();
        assert_eq!(sim, 1.0); // Both empty = identical

        let sim = semantic_similarity("hello", "").unwrap();
        assert_eq!(sim, 0.0); // One empty = no overlap
    }

    #[test]
    fn test_case_insensitive() {
        let text1 = "RUST PROGRAMMING";
        let text2 = "rust programming";
        let sim = semantic_similarity(text1, text2).unwrap();
        assert_eq!(sim, 1.0); // Case shouldn't matter
    }

    #[test]
    fn test_punctuation_ignored() {
        let text1 = "Hello, world!";
        let text2 = "Hello world";
        let sim = semantic_similarity(text1, text2).unwrap();
        assert_eq!(sim, 1.0); // Punctuation shouldn't matter
    }
}

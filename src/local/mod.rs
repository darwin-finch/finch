// Local Generation Module
//
// Handles local response generation through pattern classification and learned responses
// This is the core of Shammah's "95% local processing" capability

pub mod generator;
pub mod patterns;

pub use generator::{GeneratedResponse, ResponseGenerator};
pub use patterns::{PatternClassifier, QueryPattern};

use anyhow::Result;

/// Local generation system that coordinates pattern classification and response generation
pub struct LocalGenerator {
    pattern_classifier: PatternClassifier,
    response_generator: ResponseGenerator,
    enabled: bool,
}

impl LocalGenerator {
    /// Create new local generator
    pub fn new() -> Self {
        let pattern_classifier = PatternClassifier::new();
        let response_generator = ResponseGenerator::new(pattern_classifier.clone());

        Self {
            pattern_classifier,
            response_generator,
            enabled: true,
        }
    }

    /// Try to generate a local response
    pub fn try_generate(&mut self, query: &str) -> Result<Option<String>> {
        if !self.enabled {
            return Ok(None);
        }

        // Classify the query
        let (pattern, confidence) = self.pattern_classifier.classify(query);

        // Only try local generation if confidence is high enough
        if confidence < 0.7 {
            return Ok(None);
        }

        // Try to generate response
        match self.response_generator.generate(query) {
            Ok(response) => {
                // Only return if confidence is high enough
                if response.confidence >= 0.7 {
                    Ok(Some(response.text))
                } else {
                    Ok(None)
                }
            }
            Err(_) => Ok(None),
        }
    }

    /// Learn from a Claude response
    pub fn learn_from_claude(&mut self, query: &str, response: &str, quality_score: f64) {
        self.response_generator
            .learn_from_claude(query, response, quality_score);
    }

    /// Enable/disable local generation
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Check if enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get pattern classifier
    pub fn pattern_classifier(&self) -> &PatternClassifier {
        &self.pattern_classifier
    }

    /// Get response generator
    pub fn response_generator(&mut self) -> &mut ResponseGenerator {
        &mut self.response_generator
    }
}

impl Default for LocalGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_generation_greeting() {
        let mut generator = LocalGenerator::new();

        // Try to generate response for greeting
        let result = generator.try_generate("Hello!");
        assert!(result.is_ok());

        if let Ok(Some(response)) = result {
            assert!(!response.is_empty());
            assert!(response.to_lowercase().contains("hello") || response.to_lowercase().contains("hi"));
        }
    }

    #[test]
    fn test_local_generation_complex_query() {
        let mut generator = LocalGenerator::new();

        // Complex query should return None (forward to Claude)
        let result = generator.try_generate(
            "Explain the implementation details of Rust's async/await system including how the compiler transforms async functions into state machines"
        );

        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Should forward to Claude
    }

    #[test]
    fn test_learn_from_claude() {
        let mut generator = LocalGenerator::new();

        // Learn from a Claude response
        generator.learn_from_claude(
            "What is Rust?",
            "Rust is a systems programming language focused on safety, speed, and concurrency.",
            0.9,
        );

        // Learning should not crash
        // (Response may or may not be used for local generation depending on confidence)
    }
}

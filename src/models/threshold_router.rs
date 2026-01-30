// Threshold-based Router - Simple statistics-based routing
// Shows immediate improvement without neural network training overhead

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Query category for pattern matching
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QueryCategory {
    Greeting,    // "hi", "hello"
    Definition,  // "what is X", "who is X"
    HowTo,       // "how to X", "how do I X"
    Explanation, // "explain X"
    Code,        // Contains code blocks
    Debugging,   // "error", "fix", "bug"
    Comparison,  // "X vs Y", "difference between"
    Opinion,     // "should I", "is it better"
    Other,
}

/// Statistics for a query category
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryStats {
    pub local_attempts: usize,
    pub successes: usize,
    pub failures: usize,
    pub avg_confidence: f64,
}

impl Default for CategoryStats {
    fn default() -> Self {
        Self {
            local_attempts: 0,
            successes: 0,
            failures: 0,
            avg_confidence: 0.0,
        }
    }
}

impl CategoryStats {
    fn success_rate(&self) -> f64 {
        if self.local_attempts == 0 {
            0.0
        } else {
            self.successes as f64 / self.local_attempts as f64
        }
    }
}

/// Threshold-based router using statistics
pub struct ThresholdRouter {
    /// Statistics per category
    category_stats: HashMap<QueryCategory, CategoryStats>,

    /// Global statistics
    total_queries: usize,
    total_local_attempts: usize,
    total_successes: usize,

    /// Adaptive thresholds
    confidence_threshold: f64,
    min_samples: usize,

    /// Target forward rate (5% eventually)
    target_forward_rate: f64,
}

impl ThresholdRouter {
    /// Create new threshold router with conservative defaults
    pub fn new() -> Self {
        Self {
            category_stats: HashMap::new(),
            total_queries: 0,
            total_local_attempts: 0,
            total_successes: 0,
            confidence_threshold: 0.95, // Very conservative at start
            min_samples: 3,             // Need 3 examples before trying
            target_forward_rate: 0.05,  // Target: 5% forward
        }
    }

    /// Decide whether to try local generation
    pub fn should_try_local(&self, query: &str) -> bool {
        // During first few queries, be very conservative
        if self.total_queries < 10 {
            return false;
        }

        // Categorize the query
        let category = Self::categorize_query(query);

        // Look up statistics for this category
        if let Some(stats) = self.category_stats.get(&category) {
            // Have enough samples?
            if stats.local_attempts >= self.min_samples {
                // Success rate above threshold?
                return stats.success_rate() >= self.confidence_threshold;
            }
        }

        // Default: forward (conservative)
        false
    }

    /// Learn from a routing attempt
    pub fn learn(&mut self, query: &str, was_successful: bool) {
        self.total_queries += 1;

        let category = Self::categorize_query(query);
        let stats = self
            .category_stats
            .entry(category)
            .or_insert_with(CategoryStats::default);

        stats.local_attempts += 1;
        self.total_local_attempts += 1;

        if was_successful {
            stats.successes += 1;
            self.total_successes += 1;
        } else {
            stats.failures += 1;
        }

        // Update confidence threshold adaptively
        self.update_threshold();
    }

    /// Update threshold based on current performance
    fn update_threshold(&mut self) {
        // Only start adapting after 50 queries
        if self.total_queries < 50 {
            return;
        }

        let current_forward_rate = if self.total_queries == 0 {
            1.0
        } else {
            1.0 - (self.total_local_attempts as f64 / self.total_queries as f64)
        };

        // If forwarding too much, become more aggressive (lower threshold)
        if current_forward_rate > self.target_forward_rate + 0.1 {
            self.confidence_threshold *= 0.995; // Slowly decrease
        }
        // If local attempts failing, become more conservative (raise threshold)
        else if self.total_local_attempts > 0 {
            let success_rate = self.total_successes as f64 / self.total_local_attempts as f64;
            if success_rate < 0.7 {
                self.confidence_threshold *= 1.005; // Slowly increase
            }
        }

        // Clamp to reasonable range
        self.confidence_threshold = self.confidence_threshold.clamp(0.60, 0.95);

        // Reduce min_samples as we get more confident
        if self.total_queries > 100 && self.min_samples > 2 {
            self.min_samples = 2;
        }
        if self.total_queries > 500 && self.min_samples > 1 {
            self.min_samples = 1;
        }
    }

    /// Categorize a query into a category
    fn categorize_query(query: &str) -> QueryCategory {
        let lower = query.to_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();

        // Check for code
        if query.contains("```") || query.contains("fn ") || query.contains("def ") {
            return QueryCategory::Code;
        }

        // Check for debugging
        if lower.contains("error")
            || lower.contains("fix")
            || lower.contains("bug")
            || lower.contains("broken")
            || lower.contains("doesn't work")
        {
            return QueryCategory::Debugging;
        }

        // Check first few words for patterns
        if words.len() >= 2 {
            let first_two = format!("{} {}", words[0], words[1]);

            if first_two.starts_with("what is")
                || first_two.starts_with("who is")
                || first_two.starts_with("what are")
            {
                return QueryCategory::Definition;
            }

            if first_two.starts_with("how to")
                || first_two.starts_with("how do")
                || first_two.starts_with("how can")
            {
                return QueryCategory::HowTo;
            }
        }

        // Check for greetings (short queries)
        if words.len() <= 3 {
            if lower.starts_with("hi")
                || lower.starts_with("hello")
                || lower.starts_with("hey")
                || lower == "good morning"
                || lower == "good afternoon"
            {
                return QueryCategory::Greeting;
            }
        }

        // Check for explanation requests
        if lower.contains("explain") || lower.contains("describe") || lower.starts_with("why") {
            return QueryCategory::Explanation;
        }

        // Check for comparisons
        if lower.contains(" vs ")
            || lower.contains(" versus ")
            || lower.contains("difference between")
            || lower.contains("compare")
        {
            return QueryCategory::Comparison;
        }

        // Check for opinions
        if lower.contains("should i")
            || lower.contains("is it better")
            || lower.contains("recommend")
        {
            return QueryCategory::Opinion;
        }

        QueryCategory::Other
    }

    /// Get statistics
    pub fn stats(&self) -> ThresholdRouterStats {
        let forward_rate = if self.total_queries == 0 {
            1.0
        } else {
            1.0 - (self.total_local_attempts as f64 / self.total_queries as f64)
        };

        let success_rate = if self.total_local_attempts == 0 {
            0.0
        } else {
            self.total_successes as f64 / self.total_local_attempts as f64
        };

        ThresholdRouterStats {
            total_queries: self.total_queries,
            total_local_attempts: self.total_local_attempts,
            total_successes: self.total_successes,
            forward_rate,
            success_rate,
            confidence_threshold: self.confidence_threshold,
            min_samples: self.min_samples,
            categories: self.category_stats.clone(),
        }
    }

    /// Save router state to disk
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load router state from disk
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let json = std::fs::read_to_string(path)?;
        let router = serde_json::from_str(&json)?;
        Ok(router)
    }
}

impl Serialize for ThresholdRouter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ThresholdRouter", 7)?;
        state.serialize_field("category_stats", &self.category_stats)?;
        state.serialize_field("total_queries", &self.total_queries)?;
        state.serialize_field("total_local_attempts", &self.total_local_attempts)?;
        state.serialize_field("total_successes", &self.total_successes)?;
        state.serialize_field("confidence_threshold", &self.confidence_threshold)?;
        state.serialize_field("min_samples", &self.min_samples)?;
        state.serialize_field("target_forward_rate", &self.target_forward_rate)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for ThresholdRouter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ThresholdRouterData {
            category_stats: HashMap<QueryCategory, CategoryStats>,
            total_queries: usize,
            total_local_attempts: usize,
            total_successes: usize,
            confidence_threshold: f64,
            min_samples: usize,
            target_forward_rate: f64,
        }

        let data = ThresholdRouterData::deserialize(deserializer)?;
        Ok(ThresholdRouter {
            category_stats: data.category_stats,
            total_queries: data.total_queries,
            total_local_attempts: data.total_local_attempts,
            total_successes: data.total_successes,
            confidence_threshold: data.confidence_threshold,
            min_samples: data.min_samples,
            target_forward_rate: data.target_forward_rate,
        })
    }
}

/// Statistics snapshot
#[derive(Debug, Clone)]
pub struct ThresholdRouterStats {
    pub total_queries: usize,
    pub total_local_attempts: usize,
    pub total_successes: usize,
    pub forward_rate: f64,
    pub success_rate: f64,
    pub confidence_threshold: f64,
    pub min_samples: usize,
    pub categories: HashMap<QueryCategory, CategoryStats>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_categorization() {
        assert_eq!(
            ThresholdRouter::categorize_query("What is Rust?"),
            QueryCategory::Definition
        );
        assert_eq!(
            ThresholdRouter::categorize_query("How do I use lifetimes?"),
            QueryCategory::HowTo
        );
        assert_eq!(
            ThresholdRouter::categorize_query("Hello!"),
            QueryCategory::Greeting
        );
        assert_eq!(
            ThresholdRouter::categorize_query("Fix this error: ..."),
            QueryCategory::Debugging
        );
        assert_eq!(
            ThresholdRouter::categorize_query("Explain ownership"),
            QueryCategory::Explanation
        );
    }

    #[test]
    fn test_learning() {
        let mut router = ThresholdRouter::new();

        // First 10 queries: always forward
        for i in 0..10 {
            assert!(!router.should_try_local("test query"));
            router.learn("test query", false);
        }

        // Learn that greetings work
        for _ in 0..5 {
            router.learn("Hello", true);
        }

        // After 3 successes, should try greetings
        assert!(router.should_try_local("Hi there"));
    }

    #[test]
    fn test_adaptive_threshold() {
        let mut router = ThresholdRouter::new();
        let initial_threshold = router.confidence_threshold;

        // Simulate scenario: many queries, but we're forwarding most
        // This should make threshold decrease (become more aggressive)
        for i in 0..100 {
            // Only try local on every 10th query (90% forward rate)
            if i % 10 == 0 {
                router.learn("test", true);  // Local attempt succeeded
            }
            router.total_queries += 1;  // Count all queries
        }

        // With 90% forward rate (way above 5% target), threshold should decrease
        assert!(router.confidence_threshold < initial_threshold);
    }
}

// Threshold-based Router - Simple statistics-based routing
// Shows immediate improvement without neural network training overhead

use anyhow::Result;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

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

    /// Session ID to prevent double-counting on save/load cycles
    /// Runtime-only field (not saved to disk)
    /// Each program run gets a unique ID. When saving:
    /// - Same ID = same process saving again (replace)
    /// - Different ID = concurrent process (merge)
    session_id: String,

    /// Track if this session has saved to disk
    /// First save after load: Check for concurrent changes (merge if needed)
    /// Subsequent saves: This session owns the file (replace)
    /// Uses AtomicBool for interior mutability (allows mutation through &self, thread-safe)
    /// Runtime-only field (not saved to disk via manual Serialize/Deserialize)
    has_saved_this_session: AtomicBool,

    /// Track if this router was loaded from disk (vs created new)
    /// If loaded from disk, don't merge on first save (we already have that data!)
    /// Runtime-only field
    loaded_from_disk: bool,
}

impl Clone for ThresholdRouter {
    fn clone(&self) -> Self {
        Self {
            category_stats: self.category_stats.clone(),
            total_queries: self.total_queries,
            total_local_attempts: self.total_local_attempts,
            total_successes: self.total_successes,
            confidence_threshold: self.confidence_threshold,
            min_samples: self.min_samples,
            target_forward_rate: self.target_forward_rate,
            session_id: self.session_id.clone(),
            has_saved_this_session: AtomicBool::new(
                self.has_saved_this_session
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
            loaded_from_disk: self.loaded_from_disk,
        }
    }
}

impl ThresholdRouter {
    /// Create new threshold router with balanced defaults
    pub fn new() -> Self {
        Self {
            category_stats: HashMap::new(),
            total_queries: 0,
            total_local_attempts: 0,
            total_successes: 0,
            confidence_threshold: 0.75, // Balanced threshold (75% success rate)
            min_samples: 2,             // Need 2 examples before trying
            target_forward_rate: 0.05,  // Target: 5% forward
            session_id: Uuid::new_v4().to_string(),
            has_saved_this_session: AtomicBool::new(false),
            loaded_from_disk: false,
        }
    }

    /// Decide whether to try local generation
    pub fn should_try_local(&self, query: &str) -> bool {
        // CHANGED: Now that we have a real ONNX model, try local by default
        // Only forward if we've learned this category fails consistently

        // Categorize the query
        let category = Self::categorize_query(query);

        // Look up statistics for this category
        if let Some(stats) = self.category_stats.get(&category) {
            // If we have enough samples and success rate is LOW, forward
            if stats.local_attempts >= self.min_samples {
                // Only forward if success rate is BELOW threshold (inverted logic)
                if stats.success_rate() < self.confidence_threshold {
                    return false; // Forward to Claude
                }
            }
        }

        // Default: try local (now that we have ONNX model)
        true
    }

    /// Learn from a local generation attempt (called only when we tried local)
    pub fn learn_local_attempt(&mut self, query: &str, was_successful: bool) {
        self.total_queries += 1;
        self.total_local_attempts += 1;

        let category = Self::categorize_query(query);
        let stats = self.category_stats.entry(category).or_default();

        stats.local_attempts += 1;

        if was_successful {
            stats.successes += 1;
            self.total_successes += 1;
        } else {
            stats.failures += 1;
        }

        // Update confidence threshold adaptively
        self.update_threshold();
    }

    /// Learn from a forwarded query (called when we forwarded to Claude)
    pub fn learn_forwarded(&mut self, _query: &str) {
        self.total_queries += 1;
        // Don't increment total_local_attempts - we didn't try local generation

        // Optionally: track which categories get forwarded for future analysis
        // For now, just count the query

        // Update confidence threshold adaptively
        self.update_threshold();
    }

    /// Deprecated: Use learn_local_attempt() or learn_forwarded() instead
    /// This method is kept for backward compatibility but logs a warning
    #[deprecated(
        since = "0.2.0",
        note = "Use learn_local_attempt() or learn_forwarded() instead"
    )]
    pub fn learn(&mut self, query: &str, was_successful: bool) {
        tracing::warn!(
            "learn() is deprecated - use learn_local_attempt() or learn_forwarded() instead"
        );
        // For backward compatibility, assume all calls are local attempts
        self.learn_local_attempt(query, was_successful);
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
        if words.len() <= 3
            && (lower.starts_with("hi")
                || lower.starts_with("hello")
                || lower.starts_with("hey")
                || lower == "good morning"
                || lower == "good afternoon")
        {
            return QueryCategory::Greeting;
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
    /// Save router state with concurrent-safe merging
    /// Acquires exclusive lock, merges with existing state if from different session, writes atomically
    ///
    /// Prevents double-counting:
    /// - First save after load: Check for concurrent changes
    /// - Subsequent saves: This session owns the file (replace)
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();

        // Create lock file
        let lock_path = path.with_extension("lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;

        // Acquire exclusive lock (blocks until available)
        lock_file.lock_exclusive()?;

        // Decide: merge or replace?
        let to_save = if self.has_saved_this_session.load(Ordering::Relaxed) {
            // We already saved once - we own this file, just replace
            let json = serde_json::to_string(self)?;
            serde_json::from_str(&json)?
        } else if self.loaded_from_disk {
            // First save after loading from disk
            // We already have all the data from disk, just replace (don't merge!)
            let json = serde_json::to_string(self)?;
            serde_json::from_str(&json)?
        } else if path.exists() {
            // First save of new session, file exists
            // Check for concurrent changes and merge if needed
            match Self::load(path) {
                Ok(existing) => {
                    // Different session wrote this file, merge
                    self.merge_with(&existing)
                }
                Err(_) => {
                    // Load failed, just save current state
                    let json = serde_json::to_string(self)?;
                    serde_json::from_str(&json)?
                }
            }
        } else {
            // No existing file
            let json = serde_json::to_string(self)?;
            serde_json::from_str(&json)?
        };

        // Write atomically (write to temp, then rename)
        let temp_path = path.with_extension("tmp");
        let json = serde_json::to_string_pretty(&to_save)?;
        std::fs::write(&temp_path, json)?;
        std::fs::rename(temp_path, path)?;

        // Mark that we've saved (using atomic bool for thread safety)
        self.has_saved_this_session.store(true, Ordering::Relaxed);

        // Lock automatically released when lock_file drops
        Ok(())
    }

    /// Merge this router's statistics with another router (for concurrent sessions)
    /// Preserves the current session's session_id
    fn merge_with(&self, other: &Self) -> Self {
        let mut merged = Self::new();

        // Merge category statistics
        for (category, my_stats) in &self.category_stats {
            let other_stats = other.category_stats.get(category);
            let merged_stats = if let Some(other_stats) = other_stats {
                CategoryStats {
                    local_attempts: my_stats.local_attempts + other_stats.local_attempts,
                    successes: my_stats.successes + other_stats.successes,
                    failures: my_stats.failures + other_stats.failures,
                    // Average the confidence scores
                    avg_confidence: (my_stats.avg_confidence + other_stats.avg_confidence) / 2.0,
                }
            } else {
                my_stats.clone()
            };
            merged.category_stats.insert(*category, merged_stats);
        }

        // Add categories that only exist in other
        for (category, other_stats) in &other.category_stats {
            if !merged.category_stats.contains_key(category) {
                merged.category_stats.insert(*category, other_stats.clone());
            }
        }

        // Merge global statistics
        merged.total_queries = self.total_queries + other.total_queries;
        merged.total_local_attempts = self.total_local_attempts + other.total_local_attempts;
        merged.total_successes = self.total_successes + other.total_successes;

        // Average the confidence threshold
        merged.confidence_threshold =
            (self.confidence_threshold + other.confidence_threshold) / 2.0;

        // Keep the current session's ID (not the merged one's)
        merged.session_id = self.session_id.clone();

        merged
    }

    /// Load router state from disk
    /// Generates a new session ID to represent this program run
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let json = std::fs::read_to_string(path)?;
        let mut router: ThresholdRouter = serde_json::from_str(&json)?;
        // Generate NEW session ID for this program run
        router.session_id = Uuid::new_v4().to_string();
        // Mark as not yet saved in this session
        router
            .has_saved_this_session
            .store(false, Ordering::Relaxed);
        // Mark as loaded from disk (already have this data, don't merge on first save)
        router.loaded_from_disk = true;
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
        // session_id is runtime-only (#[serde(skip)])
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
            // Runtime-only fields, will be set by load() or new()
            session_id: String::new(),
            has_saved_this_session: AtomicBool::new(false),
            loaded_from_disk: false, // Will be set to true by load() if needed
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

        // With ONNX model, default is to try local
        // First 10 queries: should try local by default (no statistics yet)
        for _ in 0..10 {
            assert!(router.should_try_local("test query")); // Changed: now tries local by default
            router.learn_forwarded("test query");
        }

        // Verify: 10 queries, 0 local attempts
        assert_eq!(router.total_queries, 10);
        assert_eq!(router.total_local_attempts, 0);

        // Learn that greetings work (local attempts)
        for _ in 0..5 {
            router.learn_local_attempt("Hello", true);
        }

        // Verify: 15 queries total, 5 local attempts, 5 successes
        assert_eq!(router.total_queries, 15);
        assert_eq!(router.total_local_attempts, 5);
        assert_eq!(router.total_successes, 5);

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
                router.learn_local_attempt("test", true); // Local attempt succeeded
            } else {
                router.learn_forwarded("test"); // Forwarded to Claude
            }
        }

        // Verify correct counts: 100 total, 10 local attempts
        assert_eq!(router.total_queries, 100);
        assert_eq!(router.total_local_attempts, 10);

        // With 90% forward rate (way above 5% target), threshold should decrease
        assert!(router.confidence_threshold < initial_threshold);
    }
}

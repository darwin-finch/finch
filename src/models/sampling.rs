// Context-Aware Sampling System
// Samples 5% of queries to Claude with full conversation context for validation

use anyhow::{Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};

/// Sampling configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingConfig {
    /// Base sampling rate (0.0 - 1.0)
    pub base_rate: f64,

    /// Multiplier for architecture questions
    pub architecture_multiplier: f64,

    /// Multiplier for security-related questions
    pub security_multiplier: f64,

    /// Multiplier for uncertain responses
    pub uncertainty_multiplier: f64,

    /// Whether sampling is enabled
    pub enabled: bool,
}

impl Default for SamplingConfig {
    fn default() -> Self {
        Self {
            base_rate: 0.05, // 5% baseline
            architecture_multiplier: 3.0,
            security_multiplier: 5.0,
            uncertainty_multiplier: 2.0,
            enabled: true,
        }
    }
}

/// Categories of queries for sampling prioritization
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryCategory {
    Architecture,
    Security,
    Performance,
    Testing,
    General,
}

impl QueryCategory {
    /// Detect category from query text
    pub fn from_query(query: &str) -> Self {
        let query_lower = query.to_lowercase();

        // Architecture keywords
        let arch_keywords = [
            "architecture",
            "design",
            "structure",
            "pattern",
            "microservice",
            "distributed",
            "system design",
            "scalability",
        ];

        // Security keywords
        let security_keywords = [
            "security",
            "authentication",
            "authorization",
            "vulnerability",
            "injection",
            "xss",
            "csrf",
            "encryption",
            "credential",
        ];

        // Performance keywords
        let perf_keywords = [
            "performance",
            "optimize",
            "slow",
            "latency",
            "throughput",
            "memory",
            "cpu",
            "cache",
        ];

        // Testing keywords
        let test_keywords = ["test", "testing", "unittest", "integration test", "mock"];

        if arch_keywords.iter().any(|k| query_lower.contains(k)) {
            QueryCategory::Architecture
        } else if security_keywords.iter().any(|k| query_lower.contains(k)) {
            QueryCategory::Security
        } else if perf_keywords.iter().any(|k| query_lower.contains(k)) {
            QueryCategory::Performance
        } else if test_keywords.iter().any(|k| query_lower.contains(k)) {
            QueryCategory::Testing
        } else {
            QueryCategory::General
        }
    }

    /// Get sampling multiplier for this category
    pub fn sampling_multiplier(&self, config: &SamplingConfig) -> f64 {
        match self {
            QueryCategory::Architecture => config.architecture_multiplier,
            QueryCategory::Security => config.security_multiplier,
            QueryCategory::Performance => 1.5,
            QueryCategory::Testing => 1.0,
            QueryCategory::General => 1.0,
        }
    }
}

/// Sampling decision
#[derive(Debug, Clone)]
pub struct SamplingDecision {
    /// Whether to sample this query
    pub should_sample: bool,

    /// Query category
    pub category: QueryCategory,

    /// Effective sampling rate used
    pub effective_rate: f64,

    /// Reason for sampling (or not)
    pub reason: String,
}

/// Sampler that decides when to send queries to Claude
pub struct Sampler {
    config: SamplingConfig,
    rng: rand::rngs::ThreadRng,
}

impl Sampler {
    /// Create new sampler
    pub fn new(config: SamplingConfig) -> Self {
        Self {
            config,
            rng: rand::thread_rng(),
        }
    }

    /// Decide whether to sample this query
    pub fn should_sample(&mut self, query: &str, _confidence: Option<f64>) -> SamplingDecision {
        if !self.config.enabled {
            return SamplingDecision {
                should_sample: false,
                category: QueryCategory::General,
                effective_rate: 0.0,
                reason: "Sampling disabled".to_string(),
            };
        }

        // Detect category
        let category = QueryCategory::from_query(query);

        // Calculate effective sampling rate
        let multiplier = category.sampling_multiplier(&self.config);
        let effective_rate = (self.config.base_rate * multiplier).min(1.0);

        // Random sampling based on effective rate
        let random_value = self.rng.gen::<f64>();
        let should_sample = random_value < effective_rate;

        let reason = if should_sample {
            format!(
                "Sampled (category: {:?}, rate: {:.1}%)",
                category,
                effective_rate * 100.0
            )
        } else {
            format!("Not sampled (random: {:.3} >= {:.3})", random_value, effective_rate)
        };

        SamplingDecision {
            should_sample,
            category,
            effective_rate,
            reason,
        }
    }

    /// Get current config
    pub fn config(&self) -> &SamplingConfig {
        &self.config
    }

    /// Update config
    pub fn set_config(&mut self, config: SamplingConfig) {
        self.config = config;
    }
}

/// Result of comparing local vs Claude responses
#[derive(Debug, Clone)]
pub struct ComparisonResult {
    /// Local (Qwen) response
    pub local_response: String,

    /// Claude response (with context)
    pub claude_response: String,

    /// Whether responses are similar
    pub similar: bool,

    /// Similarity score (0.0 - 1.0)
    pub similarity_score: f64,

    /// Context was provided to both
    pub context_provided: bool,
}

impl ComparisonResult {
    /// Check if responses differ significantly
    pub fn differ_significantly(&self) -> bool {
        !self.similar || self.similarity_score < 0.7
    }

    /// Create from responses
    pub fn new(local_response: String, claude_response: String) -> Self {
        // Simple similarity check (can be improved with better algorithms)
        let similarity_score = Self::compute_similarity(&local_response, &claude_response);
        let similar = similarity_score > 0.8;

        Self {
            local_response,
            claude_response,
            similar,
            similarity_score,
            context_provided: true,
        }
    }

    /// Compute similarity between two responses
    /// TODO: Use better algorithm (e.g., embeddings, edit distance)
    fn compute_similarity(a: &str, b: &str) -> f64 {
        // Very simple: compare word overlap
        let a_lower = a.to_lowercase();
        let b_lower = b.to_lowercase();

        let words_a: std::collections::HashSet<_> = a_lower
            .split_whitespace()
            .collect();
        let words_b: std::collections::HashSet<_> = b_lower
            .split_whitespace()
            .collect();

        let intersection = words_a.intersection(&words_b).count();
        let union = words_a.union(&words_b).count();

        if union == 0 {
            0.0
        } else {
            intersection as f64 / union as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_category_detection() {
        assert_eq!(
            QueryCategory::from_query("How should I design the architecture?"),
            QueryCategory::Architecture
        );

        assert_eq!(
            QueryCategory::from_query("Is this vulnerable to SQL injection?"),
            QueryCategory::Security
        );

        assert_eq!(
            QueryCategory::from_query("How can I optimize this?"),
            QueryCategory::Performance
        );

        assert_eq!(
            QueryCategory::from_query("What's 2+2?"),
            QueryCategory::General
        );
    }

    #[test]
    fn test_sampling_multipliers() {
        let config = SamplingConfig::default();

        assert_eq!(
            QueryCategory::Architecture.sampling_multiplier(&config),
            3.0
        );
        assert_eq!(
            QueryCategory::Security.sampling_multiplier(&config),
            5.0
        );
        assert_eq!(QueryCategory::General.sampling_multiplier(&config), 1.0);
    }

    #[test]
    fn test_sampling_decision() {
        let config = SamplingConfig {
            base_rate: 0.5, // 50% for predictable testing
            architecture_multiplier: 2.0,
            security_multiplier: 2.0,
            uncertainty_multiplier: 2.0,
            enabled: true,
        };

        let mut sampler = Sampler::new(config);

        // Run multiple times to test randomness
        let mut sampled_count = 0;
        for _ in 0..100 {
            let decision = sampler.should_sample("general query", None);
            if decision.should_sample {
                sampled_count += 1;
            }
        }

        // Should be around 50% (allow variance)
        assert!(sampled_count > 30 && sampled_count < 70);
    }

    #[test]
    fn test_sampling_disabled() {
        let config = SamplingConfig {
            base_rate: 1.0, // 100%
            enabled: false, // But disabled
            ..Default::default()
        };

        let mut sampler = Sampler::new(config);
        let decision = sampler.should_sample("any query", None);

        assert!(!decision.should_sample);
        assert!(decision.reason.contains("disabled"));
    }

    #[test]
    fn test_similarity_computation() {
        let result1 = ComparisonResult::new(
            "This is a test response".to_string(),
            "This is a test response".to_string(),
        );
        assert!(result1.similarity_score > 0.9);

        let result2 = ComparisonResult::new(
            "Completely different text here".to_string(),
            "Totally unrelated content over there".to_string(),
        );
        assert!(result2.similarity_score < 0.3);
    }

    #[test]
    fn test_differ_significantly() {
        let similar = ComparisonResult::new(
            "Use a mutex here".to_string(),
            "You should use a mutex here".to_string(),
        );
        assert!(!similar.differ_significantly());

        let different = ComparisonResult::new(
            "Use a mutex for thread safety".to_string(),
            "Use message passing instead of shared state".to_string(),
        );
        assert!(different.differ_significantly());
    }
}

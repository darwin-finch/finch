// Threshold-based Validator - Simple heuristics for quality assessment
// Uses rule-based checks instead of neural network

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Quality signals that can be measured heuristically
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QualitySignal {
    TooShort,        // Response < 20 chars
    TooLong,         // Response > 5000 chars
    Repetitive,      // Repeated phrases
    Incomplete,      // Ends mid-sentence
    NoContent,       // Empty or whitespace only
    HasCode,         // Contains code blocks
    WellFormatted,   // Has paragraphs, proper structure
    AnswersQuestion, // Contains keywords from query
}

/// Statistics for learning quality thresholds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityStats {
    pub total_validations: usize,
    pub approved: usize,
    pub rejected: usize,

    // Track which signals correlate with quality
    pub signal_stats: HashMap<QualitySignal, SignalStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct SignalStats {
    pub present_and_good: usize,
    pub present_and_bad: usize,
    pub absent_and_good: usize,
    pub absent_and_bad: usize,
}


impl SignalStats {
    fn precision(&self) -> f64 {
        let total_present = self.present_and_good + self.present_and_bad;
        if total_present == 0 {
            0.5
        } else {
            self.present_and_good as f64 / total_present as f64
        }
    }
}

/// Threshold-based validator
pub struct ThresholdValidator {
    /// Learning statistics
    stats: QualityStats,

    /// Adaptive thresholds
    min_length: usize,
    max_length: usize,
    min_relevance_score: f64,

    /// Be conservative at start
    total_validations: usize,
}

impl ThresholdValidator {
    /// Create new threshold validator with conservative defaults
    pub fn new() -> Self {
        Self {
            stats: QualityStats {
                total_validations: 0,
                approved: 0,
                rejected: 0,
                signal_stats: HashMap::new(),
            },
            min_length: 20,
            max_length: 5000,
            min_relevance_score: 0.3,
            total_validations: 0,
        }
    }

    /// Validate a response quality (0 = bad, 1 = good)
    pub fn validate(&self, query: &str, response: &str) -> bool {
        // During first 10 validations, be very conservative (reject everything)
        // This forces learning from Claude responses first
        if self.total_validations < 10 {
            return false;
        }

        // Check all quality signals
        let signals = self.check_signals(query, response);

        // Calculate quality score
        let score = self.calculate_quality_score(&signals);

        // Conservative threshold: need 0.7+ to approve
        score >= 0.7
    }

    /// Calculate quality score without validation side effects
    /// Returns a score from 0.0 (very bad) to 1.0 (excellent)
    pub fn quality_score(&self, query: &str, response: &str) -> f64 {
        let signals = self.check_signals(query, response);
        self.calculate_quality_score(&signals)
    }

    /// Check all quality signals
    fn check_signals(&self, query: &str, response: &str) -> Vec<QualitySignal> {
        let mut signals = Vec::new();

        // Check length
        if response.len() < self.min_length {
            signals.push(QualitySignal::TooShort);
        }
        if response.len() > self.max_length {
            signals.push(QualitySignal::TooLong);
        }

        // Check if empty or whitespace only
        if response.trim().is_empty() {
            signals.push(QualitySignal::NoContent);
        }

        // Check for repetition
        if self.is_repetitive(response) {
            signals.push(QualitySignal::Repetitive);
        }

        // Check if incomplete (ends mid-sentence)
        if self.is_incomplete(response) {
            signals.push(QualitySignal::Incomplete);
        }

        // Check for code blocks
        if response.contains("```") {
            signals.push(QualitySignal::HasCode);
        }

        // Check formatting
        if self.is_well_formatted(response) {
            signals.push(QualitySignal::WellFormatted);
        }

        // Check relevance to query
        if self.answers_question(query, response) {
            signals.push(QualitySignal::AnswersQuestion);
        }

        signals
    }

    /// Calculate quality score from signals
    fn calculate_quality_score(&self, signals: &[QualitySignal]) -> f64 {
        let mut score = 0.5; // Start neutral

        for signal in signals {
            match signal {
                // Bad signals
                QualitySignal::TooShort => score -= 0.3,
                QualitySignal::TooLong => score -= 0.1,
                QualitySignal::Repetitive => score -= 0.4,
                QualitySignal::Incomplete => score -= 0.3,
                QualitySignal::NoContent => score -= 0.5,

                // Good signals
                QualitySignal::HasCode => score += 0.1,
                QualitySignal::WellFormatted => score += 0.2,
                QualitySignal::AnswersQuestion => score += 0.3,
            }
        }

        // Use learned weights if we have enough data
        if self.total_validations > 50 {
            for signal in signals {
                if let Some(signal_stats) = self.stats.signal_stats.get(signal) {
                    let precision = signal_stats.precision();
                    // Adjust score based on learned precision
                    score += (precision - 0.5) * 0.2;
                }
            }
        }

        score.clamp(0.0, 1.0)
    }

    /// Learn from actual validation result
    pub fn learn(&mut self, query: &str, response: &str, was_actually_good: bool) {
        self.total_validations += 1;
        self.stats.total_validations += 1;

        if was_actually_good {
            self.stats.approved += 1;
        } else {
            self.stats.rejected += 1;
        }

        // Learn signal correlations
        let signals = self.check_signals(query, response);
        let signals_present: std::collections::HashSet<_> = signals.into_iter().collect();

        // Update stats for all signals
        for signal in &[
            QualitySignal::TooShort,
            QualitySignal::TooLong,
            QualitySignal::Repetitive,
            QualitySignal::Incomplete,
            QualitySignal::NoContent,
            QualitySignal::HasCode,
            QualitySignal::WellFormatted,
            QualitySignal::AnswersQuestion,
        ] {
            let signal_stats = self
                .stats
                .signal_stats
                .entry(*signal)
                .or_default();

            let present = signals_present.contains(signal);

            match (present, was_actually_good) {
                (true, true) => signal_stats.present_and_good += 1,
                (true, false) => signal_stats.present_and_bad += 1,
                (false, true) => signal_stats.absent_and_good += 1,
                (false, false) => signal_stats.absent_and_bad += 1,
            }
        }
    }

    /// Check if response is repetitive
    fn is_repetitive(&self, response: &str) -> bool {
        if response.len() < 50 {
            return false;
        }

        // Look for repeated phrases
        let words: Vec<&str> = response.split_whitespace().collect();
        if words.len() < 10 {
            return false;
        }

        // Check for 3-word phrases that repeat
        let mut phrases = HashMap::new();
        for window in words.windows(3) {
            let phrase = window.join(" ");
            *phrases.entry(phrase).or_insert(0) += 1;
        }

        // If any phrase repeats 3+ times, it's repetitive
        phrases.values().any(|&count| count >= 3)
    }

    /// Check if response is incomplete
    fn is_incomplete(&self, response: &str) -> bool {
        let trimmed = response.trim();
        if trimmed.is_empty() {
            return true;
        }

        // Check last character
        let last_char = trimmed.chars().last().unwrap();

        // Good endings: . ! ? )
        // Bad endings: , ; : ( or letter (mid-word)
        !matches!(last_char, '.' | '!' | '?' | ')' | '"' | '\'')
    }

    /// Check if well formatted
    fn is_well_formatted(&self, response: &str) -> bool {
        // Has multiple paragraphs
        let paragraphs = response.split("\n\n").count();

        // Has proper punctuation
        let has_periods = response.contains('.');

        // Not just one long run-on sentence
        let sentence_count = response.matches('.').count()
            + response.matches('!').count()
            + response.matches('?').count();

        paragraphs >= 2 || (has_periods && sentence_count >= 2)
    }

    /// Check if response answers the question
    fn answers_question(&self, query: &str, response: &str) -> bool {
        let query_lower = query.to_lowercase();
        let response_lower = response.to_lowercase();

        // Extract important words from query (skip common words)
        let stopwords = [
            "a", "an", "the", "is", "are", "what", "how", "why", "when", "where", "who", "which",
            "do", "does", "did", "can", "could", "should", "would",
        ];

        let query_words: Vec<&str> = query_lower
            .split_whitespace()
            .filter(|w| w.len() > 3 && !stopwords.contains(w))
            .collect();

        if query_words.is_empty() {
            return true; // Can't determine, give benefit of doubt
        }

        // Check how many query keywords appear in response
        let matches = query_words
            .iter()
            .filter(|word| response_lower.contains(*word))
            .count();

        let relevance = matches as f64 / query_words.len() as f64;
        relevance >= self.min_relevance_score
    }

    /// Get statistics
    pub fn stats(&self) -> ValidatorStats {
        let approval_rate = if self.stats.total_validations == 0 {
            0.0
        } else {
            self.stats.approved as f64 / self.stats.total_validations as f64
        };

        ValidatorStats {
            total_validations: self.stats.total_validations,
            approved: self.stats.approved,
            rejected: self.stats.rejected,
            approval_rate,
            signal_stats: self.stats.signal_stats.clone(),
        }
    }

    /// Save validator state
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.stats)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load validator state
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let json = std::fs::read_to_string(path)?;
        let stats = serde_json::from_str(&json)?;
        Ok(Self {
            stats,
            min_length: 20,
            max_length: 5000,
            min_relevance_score: 0.3,
            total_validations: 0,
        })
    }
}

/// Statistics snapshot
#[derive(Debug, Clone)]
pub struct ValidatorStats {
    pub total_validations: usize,
    pub approved: usize,
    pub rejected: usize,
    pub approval_rate: f64,
    pub signal_stats: HashMap<QualitySignal, SignalStats>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validation() {
        let validator = ThresholdValidator::new();

        // Good response
        let good = "Rust is a systems programming language. It focuses on safety and performance. \
                   It has a strong type system and ownership model.";

        // After warm-up period
        let mut val = ThresholdValidator::new();
        for _ in 0..15 {
            val.learn("warmup", "warmup response", true);
        }

        // Now test
        assert!(val.validate("What is Rust?", good));
    }

    #[test]
    fn test_too_short() {
        let validator = ThresholdValidator::new();
        let signals = validator.check_signals("test", "Hi");
        assert!(signals.contains(&QualitySignal::TooShort));
    }

    #[test]
    fn test_repetitive() {
        let validator = ThresholdValidator::new();
        let repetitive = "This is a test. This is a test. This is a test. This is a test.";
        assert!(validator.is_repetitive(repetitive));
    }

    #[test]
    fn test_incomplete() {
        let validator = ThresholdValidator::new();
        assert!(validator.is_incomplete("This is an incomplete"));
        assert!(!validator.is_incomplete("This is complete."));
    }

    #[test]
    fn test_answers_question() {
        let validator = ThresholdValidator::new();
        let query = "What is Rust programming language?";
        let good_response = "Rust is a modern programming language focused on safety.";
        let bad_response = "The weather is nice today.";

        assert!(validator.answers_question(query, good_response));
        assert!(!validator.answers_question(query, bad_response));
    }
}

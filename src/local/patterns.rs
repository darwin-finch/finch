// Pattern Classifier - Learns to categorize queries into response patterns
//
// Analyzes Claude's responses to learn what types of queries map to what types of responses
// This feeds into both routing decisions and response generation

use crate::models::learning::{
    LearningModel, ModelExpectation, ModelPrediction, ModelStats, PredictionData,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Query patterns learned from Claude's responses
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum QueryPattern {
    /// Simple greeting/social
    Greeting,
    /// Definition or factual query
    Definition,
    /// How-to or procedural
    HowTo,
    /// Explanation request
    Explanation,
    /// Code-related
    Code,
    /// Debugging/troubleshooting
    Debugging,
    /// Comparison
    Comparison,
    /// Opinion/recommendation
    Opinion,
    /// Complex/multi-part
    Complex,
    /// Other/unknown
    Other,
}

impl QueryPattern {
    /// Convert to string
    pub fn as_str(&self) -> &str {
        match self {
            Self::Greeting => "greeting",
            Self::Definition => "definition",
            Self::HowTo => "how_to",
            Self::Explanation => "explanation",
            Self::Code => "code",
            Self::Debugging => "debugging",
            Self::Comparison => "comparison",
            Self::Opinion => "opinion",
            Self::Complex => "complex",
            Self::Other => "other",
        }
    }

    /// Parse from string
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "greeting" => Self::Greeting,
            "definition" => Self::Definition,
            "how_to" | "howto" => Self::HowTo,
            "explanation" => Self::Explanation,
            "code" => Self::Code,
            "debugging" => Self::Debugging,
            "comparison" => Self::Comparison,
            "opinion" => Self::Opinion,
            "complex" => Self::Complex,
            _ => Self::Other,
        }
    }
}

/// Pattern statistics for tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PatternStats {
    count: usize,
    avg_response_length: usize,
    local_success_rate: f64,
    confidence: f64,
}

impl Default for PatternStats {
    fn default() -> Self {
        Self {
            count: 0,
            avg_response_length: 0,
            local_success_rate: 0.0,
            confidence: 0.5,
        }
    }
}

/// Pattern classifier that learns from Claude's responses
#[derive(Clone)]
pub struct PatternClassifier {
    patterns: HashMap<QueryPattern, PatternStats>,
    total_classifications: usize,
    stats: ModelStats,
}

impl PatternClassifier {
    /// Create new pattern classifier
    pub fn new() -> Self {
        Self {
            patterns: HashMap::new(),
            total_classifications: 0,
            stats: ModelStats::default(),
        }
    }

    /// Classify a query based on learned patterns
    pub fn classify(&self, query: &str) -> (QueryPattern, f64) {
        // Simple keyword-based classification for now
        let query_lower = query.to_lowercase();

        // Check for greetings
        if query_lower.contains("hello")
            || query_lower.contains("hi ")
            || query_lower.contains("hey")
        {
            let stats = self.patterns.get(&QueryPattern::Greeting);
            return (
                QueryPattern::Greeting,
                stats.map(|s| s.confidence).unwrap_or(0.7),
            );
        }

        // Check for definitions
        if query_lower.starts_with("what is")
            || query_lower.starts_with("what are")
            || query_lower.starts_with("who is")
        {
            let stats = self.patterns.get(&QueryPattern::Definition);
            return (
                QueryPattern::Definition,
                stats.map(|s| s.confidence).unwrap_or(0.6),
            );
        }

        // Check for how-to
        if query_lower.starts_with("how to")
            || query_lower.starts_with("how do i")
            || query_lower.starts_with("how can i")
        {
            let stats = self.patterns.get(&QueryPattern::HowTo);
            return (
                QueryPattern::HowTo,
                stats.map(|s| s.confidence).unwrap_or(0.5),
            );
        }

        // Check for code
        if query_lower.contains("```")
            || query_lower.contains("function")
            || query_lower.contains("class ")
            || query_lower.contains("error:")
        {
            let stats = self.patterns.get(&QueryPattern::Code);
            return (
                QueryPattern::Code,
                stats.map(|s| s.confidence).unwrap_or(0.4),
            );
        }

        // Default to Other with low confidence
        (QueryPattern::Other, 0.3)
    }

    /// Extract features from a query
    fn extract_features(&self, query: &str) -> Vec<String> {
        let mut features = Vec::new();

        // Length feature
        features.push(format!(
            "length:{}",
            if query.len() < 50 {
                "short"
            } else if query.len() < 200 {
                "medium"
            } else {
                "long"
            }
        ));

        // Question mark
        if query.contains('?') {
            features.push("has_question_mark".to_string());
        }

        // Code indicators
        if query.contains("```") || query.contains("function") || query.contains("class") {
            features.push("contains_code".to_string());
        }

        // Starts with question words
        let query_lower = query.to_lowercase();
        for word in &["what", "how", "why", "when", "where", "who"] {
            if query_lower.starts_with(word) {
                features.push(format!("starts_with:{}", word));
                break;
            }
        }

        features
    }
}

impl Default for PatternClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl LearningModel for PatternClassifier {
    fn update(&mut self, _input: &str, expected: &ModelExpectation) -> Result<()> {
        // Extract pattern from expectation
        let pattern = match expected {
            ModelExpectation::PatternLabel { category, .. } => QueryPattern::from_str(category),
            _ => QueryPattern::Other,
        };

        // Update pattern statistics
        let stats = self.patterns.entry(pattern).or_default();
        stats.count += 1;

        self.total_classifications += 1;
        self.stats.total_updates += 1;
        self.stats.last_update = Some(chrono::Utc::now());

        Ok(())
    }

    fn predict(&self, input: &str) -> Result<ModelPrediction> {
        let (pattern, confidence) = self.classify(input);
        let features = self.extract_features(input);

        Ok(ModelPrediction {
            confidence,
            data: PredictionData::Pattern {
                category: pattern.as_str().to_string(),
                features,
            },
        })
    }

    fn save(&self, path: &Path) -> Result<()> {
        let json =
            serde_json::to_string_pretty(self).context("Failed to serialize pattern classifier")?;
        std::fs::write(path, json).context("Failed to write pattern classifier")?;
        Ok(())
    }

    fn load(path: &Path) -> Result<Self> {
        let json = std::fs::read_to_string(path).context("Failed to read pattern classifier")?;
        let classifier =
            serde_json::from_str(&json).context("Failed to deserialize pattern classifier")?;
        Ok(classifier)
    }

    fn name(&self) -> &str {
        "pattern_classifier"
    }

    fn stats(&self) -> ModelStats {
        self.stats.clone()
    }
}

// Manual Serialize/Deserialize to handle HashMap and stats
impl Serialize for PatternClassifier {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("PatternClassifier", 3)?;
        state.serialize_field("patterns", &self.patterns)?;
        state.serialize_field("total_classifications", &self.total_classifications)?;
        state.serialize_field("stats", &self.stats)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for PatternClassifier {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct PatternClassifierData {
            patterns: HashMap<QueryPattern, PatternStats>,
            total_classifications: usize,
            stats: ModelStats,
        }

        let data = PatternClassifierData::deserialize(deserializer)?;
        Ok(PatternClassifier {
            patterns: data.patterns,
            total_classifications: data.total_classifications,
            stats: data.stats,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_pattern_as_str_all_variants() {
        assert_eq!(QueryPattern::Greeting.as_str(), "greeting");
        assert_eq!(QueryPattern::Definition.as_str(), "definition");
        assert_eq!(QueryPattern::HowTo.as_str(), "how_to");
        assert_eq!(QueryPattern::Explanation.as_str(), "explanation");
        assert_eq!(QueryPattern::Code.as_str(), "code");
        assert_eq!(QueryPattern::Debugging.as_str(), "debugging");
        assert_eq!(QueryPattern::Comparison.as_str(), "comparison");
        assert_eq!(QueryPattern::Opinion.as_str(), "opinion");
        assert_eq!(QueryPattern::Complex.as_str(), "complex");
        assert_eq!(QueryPattern::Other.as_str(), "other");
    }

    #[test]
    fn test_query_pattern_from_str_roundtrip() {
        let variants = [
            QueryPattern::Greeting,
            QueryPattern::Definition,
            QueryPattern::HowTo,
            QueryPattern::Explanation,
            QueryPattern::Code,
            QueryPattern::Debugging,
            QueryPattern::Comparison,
            QueryPattern::Opinion,
            QueryPattern::Complex,
            QueryPattern::Other,
        ];
        for variant in &variants {
            let s = variant.as_str();
            let parsed = QueryPattern::from_str(s);
            assert_eq!(&parsed, variant, "from_str roundtrip failed for {:?}", variant);
        }
    }

    #[test]
    fn test_query_pattern_from_str_case_insensitive() {
        assert_eq!(QueryPattern::from_str("GREETING"), QueryPattern::Greeting);
        assert_eq!(QueryPattern::from_str("HowTo"), QueryPattern::HowTo);
        assert_eq!(QueryPattern::from_str("HOWTO"), QueryPattern::HowTo);
        assert_eq!(QueryPattern::from_str("CODE"), QueryPattern::Code);
    }

    #[test]
    fn test_query_pattern_from_str_unknown_is_other() {
        assert_eq!(QueryPattern::from_str("xyz_unknown"), QueryPattern::Other);
        assert_eq!(QueryPattern::from_str(""), QueryPattern::Other);
    }

    #[test]
    fn test_query_pattern_serde_roundtrip() {
        let pattern = QueryPattern::Debugging;
        let json = serde_json::to_string(&pattern).unwrap();
        let decoded: QueryPattern = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, pattern);
    }

    #[test]
    fn test_pattern_classifier_classify_greeting() {
        let classifier = PatternClassifier::new();
        let (pattern, confidence) = classifier.classify("hello there");
        assert_eq!(pattern, QueryPattern::Greeting);
        assert!(confidence > 0.0);
    }

    #[test]
    fn test_pattern_classifier_classify_definition() {
        let classifier = PatternClassifier::new();
        let (pattern, _) = classifier.classify("what is Rust?");
        assert_eq!(pattern, QueryPattern::Definition);
    }

    #[test]
    fn test_pattern_classifier_classify_howto() {
        let classifier = PatternClassifier::new();
        let (pattern, _) = classifier.classify("how do i use lifetimes");
        assert_eq!(pattern, QueryPattern::HowTo);
    }

    #[test]
    fn test_pattern_classifier_classify_code() {
        let classifier = PatternClassifier::new();
        let (pattern, _) = classifier.classify("```rust\nfn main() {}\n```");
        assert_eq!(pattern, QueryPattern::Code);
    }

    #[test]
    fn test_pattern_classifier_total_classifications_zero_on_new() {
        let classifier = PatternClassifier::new();
        assert_eq!(classifier.total_classifications, 0);
    }
}

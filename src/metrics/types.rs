// Metrics data types

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Response comparison data for training effectiveness
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResponseComparison {
    /// Local response if a local attempt was made
    pub local_response: Option<String>,
    /// Claude's response (either primary or fallback)
    pub claude_response: String,
    /// Quality score from validator (0.0-1.0)
    pub quality_score: f64,
    /// Semantic similarity between local and Claude (0.0-1.0, if both exist)
    pub similarity_score: Option<f64>,
    /// Divergence: 1.0 - similarity (if both exist)
    pub divergence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMetric {
    pub timestamp: DateTime<Utc>,
    pub query_hash: String,
    pub routing_decision: String,
    pub pattern_id: Option<String>,
    pub confidence: Option<f64>,
    pub forward_reason: Option<String>,
    pub response_time_ms: u64,
    /// Response comparison data
    #[serde(default)]
    pub comparison: ResponseComparison,
    /// Router confidence scores
    pub router_confidence: Option<f64>,
    pub validator_confidence: Option<f64>,
}

impl RequestMetric {
    pub fn new(
        query_hash: String,
        routing_decision: String,
        pattern_id: Option<String>,
        confidence: Option<f64>,
        forward_reason: Option<String>,
        response_time_ms: u64,
        comparison: ResponseComparison,
        router_confidence: Option<f64>,
        validator_confidence: Option<f64>,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            query_hash,
            routing_decision,
            pattern_id,
            confidence,
            forward_reason,
            response_time_ms,
            comparison,
            router_confidence,
            validator_confidence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_comparison(local: Option<&str>, claude: &str, quality: f64) -> ResponseComparison {
        ResponseComparison {
            local_response: local.map(str::to_string),
            claude_response: claude.to_string(),
            quality_score: quality,
            similarity_score: None,
            divergence: None,
        }
    }

    #[test]
    fn test_response_comparison_default() {
        let c = ResponseComparison::default();
        assert_eq!(c.claude_response, "");
        assert_eq!(c.quality_score, 0.0);
        assert!(c.local_response.is_none());
        assert!(c.similarity_score.is_none());
        assert!(c.divergence.is_none());
    }

    #[test]
    fn test_response_comparison_with_local() {
        let c = ResponseComparison {
            local_response: Some("local answer".to_string()),
            claude_response: "claude answer".to_string(),
            quality_score: 0.9,
            similarity_score: Some(0.85),
            divergence: Some(0.15),
        };
        assert_eq!(c.quality_score, 0.9);
        assert_eq!(c.similarity_score, Some(0.85));
        assert_eq!(c.divergence, Some(0.15));
    }

    #[test]
    fn test_request_metric_new() {
        let comparison = make_comparison(None, "The answer is 42", 1.0);
        let metric = RequestMetric::new(
            "hash_abc123".to_string(),
            "forward".to_string(),
            None,
            None,
            None,
            150,
            comparison,
            Some(0.8),
            None,
        );
        assert_eq!(metric.query_hash, "hash_abc123");
        assert_eq!(metric.routing_decision, "forward");
        assert_eq!(metric.response_time_ms, 150);
        assert_eq!(metric.router_confidence, Some(0.8));
        assert!(metric.validator_confidence.is_none());
        assert!(metric.pattern_id.is_none());
    }

    #[test]
    fn test_request_metric_local_route() {
        let comparison = make_comparison(Some("local resp"), "claude resp", 0.95);
        let metric = RequestMetric::new(
            "hash_xyz".to_string(),
            "local".to_string(),
            Some("greeting_pattern".to_string()),
            Some(0.92),
            None,
            42,
            comparison,
            Some(0.92),
            Some(0.88),
        );
        assert_eq!(metric.routing_decision, "local");
        assert_eq!(metric.pattern_id.as_deref(), Some("greeting_pattern"));
        assert_eq!(metric.confidence, Some(0.92));
        assert_eq!(metric.validator_confidence, Some(0.88));
    }

    #[test]
    fn test_request_metric_serde_roundtrip() {
        let comparison = make_comparison(None, "42", 1.0);
        let metric = RequestMetric::new(
            "hash_roundtrip".to_string(),
            "local".to_string(),
            None, None, None, 100, comparison, None, None,
        );
        let json = serde_json::to_string(&metric).unwrap();
        let decoded: RequestMetric = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.query_hash, metric.query_hash);
        assert_eq!(decoded.response_time_ms, metric.response_time_ms);
    }

    #[test]
    fn test_response_comparison_serde_roundtrip() {
        let c = ResponseComparison {
            local_response: Some("local".to_string()),
            claude_response: "claude".to_string(),
            quality_score: 0.75,
            similarity_score: Some(0.9),
            divergence: Some(0.1),
        };
        let json = serde_json::to_string(&c).unwrap();
        let decoded: ResponseComparison = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.quality_score, 0.75);
        assert_eq!(decoded.similarity_score, Some(0.9));
    }
}

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

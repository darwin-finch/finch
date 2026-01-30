// Metrics data types

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMetric {
    pub timestamp: DateTime<Utc>,
    pub query_hash: String,
    pub routing_decision: String,
    pub pattern_id: Option<String>,
    pub confidence: Option<f64>,
    pub forward_reason: Option<String>,
    pub response_time_ms: u64,
}

impl RequestMetric {
    pub fn new(
        query_hash: String,
        routing_decision: String,
        pattern_id: Option<String>,
        confidence: Option<f64>,
        forward_reason: Option<String>,
        response_time_ms: u64,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            query_hash,
            routing_decision,
            pattern_id,
            confidence,
            forward_reason,
            response_time_ms,
        }
    }
}

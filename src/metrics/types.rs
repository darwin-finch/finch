// Metrics data types

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Source-free response comparison aggregates.
///
/// The response fields remain in memory only for API and legacy JSONL reading
/// compatibility. New metrics JSONL records never serialize response text.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResponseComparison {
    /// Legacy local response, accepted when reading old metrics only.
    #[serde(default, skip_serializing)]
    pub local_response: Option<String>,
    /// Legacy provider response, accepted when reading old metrics only.
    #[serde(default, skip_serializing)]
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

/// Why a provider's first Finch VM wire submission was not accepted.
///
/// Keep this deliberately coarse and source-free: conformance reporting needs
/// provider/model aggregates, not a second log of user prompts or generated
/// programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireFailureClass {
    RawProse,
    MarkdownFence,
    InventedWord,
    StackOrType,
    WrongLanguageDispatch,
    MissingOutputEffect,
    Capability,
    Other,
}

/// One terminal provider-wire attempt, including whether bounded repair was
/// needed and whether it succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireAdherenceMetric {
    pub timestamp: DateTime<Utc>,
    pub provider: String,
    pub model: String,
    /// Receiver surface such as `interactive`, `one_shot`, or `named_brain`.
    pub surface: String,
    pub first_pass_valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<WireFailureClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<String>,
    pub repair_attempted: bool,
    pub repaired_successfully: bool,
    pub terminal_failure: bool,
}

impl WireAdherenceMetric {
    pub fn first_pass(
        provider: impl Into<String>,
        model: impl Into<String>,
        surface: impl Into<String>,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            provider: provider.into(),
            model: model.into(),
            surface: surface.into(),
            first_pass_valid: true,
            failure_class: None,
            diagnostic_code: None,
            repair_attempted: false,
            repaired_successfully: false,
            terminal_failure: false,
        }
    }
}

impl RequestMetric {
    #[allow(clippy::too_many_arguments)]
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

impl ResponseComparison {
    /// Construct the source-free aggregate shape used by new metric producers.
    pub fn aggregates(
        quality_score: f64,
        similarity_score: Option<f64>,
        divergence: Option<f64>,
    ) -> Self {
        Self {
            local_response: None,
            claude_response: String::new(),
            quality_score,
            similarity_score,
            divergence,
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
    fn test_request_metric_serialization_is_source_free() {
        let comparison = make_comparison(None, "42", 1.0);
        let metric = RequestMetric::new(
            "hash_roundtrip".to_string(),
            "local".to_string(),
            None,
            None,
            None,
            100,
            comparison,
            None,
            None,
        );
        let json = serde_json::to_string(&metric).unwrap();
        assert!(!json.contains("42"));
        assert!(!json.contains("claude_response"));
        assert!(!json.contains("local_response"));
        let decoded: RequestMetric = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.query_hash, metric.query_hash);
        assert_eq!(decoded.response_time_ms, metric.response_time_ms);
        assert_eq!(decoded.comparison.quality_score, 1.0);
        assert!(decoded.comparison.claude_response.is_empty());
        assert!(decoded.comparison.local_response.is_none());
    }

    #[test]
    fn test_response_comparison_serializes_only_aggregates() {
        let c = ResponseComparison {
            local_response: Some("LOCAL_SENSITIVE_SENTINEL".to_string()),
            claude_response: "PROVIDER_SENSITIVE_SENTINEL".to_string(),
            quality_score: 0.75,
            similarity_score: Some(0.9),
            divergence: Some(0.1),
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("LOCAL_SENSITIVE_SENTINEL"));
        assert!(!json.contains("PROVIDER_SENSITIVE_SENTINEL"));
        let decoded: ResponseComparison = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.quality_score, 0.75);
        assert_eq!(decoded.similarity_score, Some(0.9));
        assert!(decoded.local_response.is_none());
        assert!(decoded.claude_response.is_empty());
    }

    #[test]
    fn test_legacy_response_text_metrics_remain_readable_but_reserialize_source_free() {
        let legacy = r#"{
            "timestamp":"2026-02-14T12:00:00Z",
            "query_hash":"legacy_hash",
            "routing_decision":"local_attempted",
            "pattern_id":null,
            "confidence":0.8,
            "forward_reason":"low_quality",
            "response_time_ms":123,
            "comparison":{
                "local_response":"legacy local source",
                "claude_response":"legacy provider source",
                "quality_score":0.75,
                "similarity_score":0.9,
                "divergence":0.1
            },
            "router_confidence":0.8,
            "validator_confidence":0.75
        }"#;

        let decoded: RequestMetric = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            decoded.comparison.local_response.as_deref(),
            Some("legacy local source")
        );
        assert_eq!(decoded.comparison.claude_response, "legacy provider source");
        assert_eq!(decoded.comparison.similarity_score, Some(0.9));
        assert_eq!(decoded.routing_decision, "local_attempted");

        let rewritten = serde_json::to_string(&decoded).unwrap();
        assert!(!rewritten.contains("legacy local source"));
        assert!(!rewritten.contains("legacy provider source"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&rewritten).unwrap()["comparison"]
                ["quality_score"],
            0.75
        );
    }

    #[test]
    fn wire_adherence_metric_does_not_require_source_text() {
        let mut metric = WireAdherenceMetric::first_pass("xai", "grok", "interactive");
        metric.first_pass_valid = false;
        metric.failure_class = Some(WireFailureClass::RawProse);
        metric.diagnostic_code = Some("E-LINK-002".into());
        metric.repair_attempted = true;
        metric.repaired_successfully = true;

        let json = serde_json::to_string(&metric).unwrap();
        assert!(!json.contains("generated_source"));
        assert_eq!(
            serde_json::from_str::<WireAdherenceMetric>(&json).unwrap(),
            metric
        );
    }
}

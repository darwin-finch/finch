// Routing decision logic

use crate::models::{ThresholdRouter, ThresholdRouterStats};
use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone)]
pub enum ForwardReason {
    NoMatch,
    LowConfidence,
    ModelNotReady, // New: Model is still loading/downloading
}

impl ForwardReason {
    pub fn as_str(&self) -> &str {
        match self {
            ForwardReason::NoMatch => "no_match",
            ForwardReason::LowConfidence => "low_confidence",
            ForwardReason::ModelNotReady => "model_not_ready",
        }
    }
}

#[derive(Debug, Clone)]
pub enum RouteDecision {
    // Keep Local variant for backward compatibility, but it's no longer used
    Local { pattern_id: String, confidence: f64 },
    Forward { reason: ForwardReason },
}

#[derive(Clone)]
pub struct Router {
    threshold_router: ThresholdRouter,
}

impl Router {
    pub fn new(threshold_router: ThresholdRouter) -> Self {
        Self {
            threshold_router,
        }
    }

    /// Make a routing decision for a query
    pub fn route(&self, query: &str) -> RouteDecision {
        // Layer 1: Data-driven routing - use threshold model
        if self.threshold_router.should_try_local(query) {
            let stats = self.threshold_router.stats();
            tracing::info!(
                "Routing decision: LOCAL (threshold confidence: {:.2})",
                stats.confidence_threshold
            );
            return RouteDecision::Local {
                pattern_id: "threshold_based".to_string(),
                confidence: stats.confidence_threshold,
            };
        }

        // Layer 2: Default fallback - forward when uncertain
        tracing::info!("Routing decision: FORWARD (threshold too low)");
        RouteDecision::Forward {
            reason: ForwardReason::NoMatch,
        }
    }

    /// Make routing decision with generator state check (progressive bootstrap support)
    ///
    /// This method checks if the generator is ready before considering local routing.
    /// If the model is still loading/downloading, it forwards to Claude for graceful degradation.
    pub fn route_with_generator_check(
        &self,
        query: &str,
        generator_is_ready: bool,
    ) -> RouteDecision {
        // Layer 0: Check if generator is ready (progressive bootstrap)
        if !generator_is_ready {
            tracing::info!("Routing decision: FORWARD (model not ready yet)");
            return RouteDecision::Forward {
                reason: ForwardReason::ModelNotReady,
            };
        }

        // Otherwise, use normal routing logic
        self.route(query)
    }

    /// Learn from a local generation attempt
    pub fn learn_local_attempt(&mut self, query: &str, was_successful: bool) {
        self.threshold_router
            .learn_local_attempt(query, was_successful);
    }

    /// Learn from a forwarded query
    pub fn learn_forwarded(&mut self, query: &str) {
        self.threshold_router.learn_forwarded(query);
    }

    /// Deprecated: Use learn_local_attempt() or learn_forwarded() instead
    #[deprecated(
        since = "0.2.0",
        note = "Use learn_local_attempt() or learn_forwarded() instead"
    )]
    #[allow(deprecated)]
    pub fn learn(&mut self, query: &str, was_successful: bool) {
        self.threshold_router.learn(query, was_successful);
    }

    /// Get threshold router statistics
    pub fn stats(&self) -> ThresholdRouterStats {
        self.threshold_router.stats()
    }

    /// Save threshold router state to disk
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        self.threshold_router.save(path)
    }

    /// Load threshold router state from disk
    pub fn load_threshold<P: AsRef<Path>>(path: P) -> Result<ThresholdRouter> {
        ThresholdRouter::load(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ThresholdRouter;

    fn make_router() -> Router {
        Router::new(ThresholdRouter::new())
    }

    #[test]
    fn test_forward_reason_as_str() {
        assert_eq!(ForwardReason::NoMatch.as_str(), "no_match");
        assert_eq!(ForwardReason::LowConfidence.as_str(), "low_confidence");
        assert_eq!(ForwardReason::ModelNotReady.as_str(), "model_not_ready");
    }

    #[test]
    fn test_route_returns_decision() {
        let router = make_router();
        // Default ThresholdRouter tries local — any decision is valid
        match router.route("hello") {
            RouteDecision::Local { .. } | RouteDecision::Forward { .. } => {}
        }
    }

    #[test]
    fn test_route_with_generator_not_ready_always_forwards() {
        let router = make_router();
        // When generator isn't ready, ALL queries must forward
        for query in &["hello", "what is Rust?", "how do I fix this error?", "explain ownership"] {
            match router.route_with_generator_check(query, false) {
                RouteDecision::Forward { reason: ForwardReason::ModelNotReady } => {}
                other => panic!("Expected ModelNotReady forward for {:?}, got {:?}", query, other),
            }
        }
    }

    #[test]
    fn test_route_with_generator_ready_uses_normal_routing() {
        let router = make_router();
        // When generator is ready, routing is driven by ThresholdRouter stats
        // We just verify we get a valid decision — not ModelNotReady
        match router.route_with_generator_check("hello", true) {
            RouteDecision::Forward { reason: ForwardReason::ModelNotReady } => {
                panic!("Should not return ModelNotReady when generator IS ready")
            }
            RouteDecision::Local { .. } | RouteDecision::Forward { .. } => {}
        }
    }

    #[test]
    fn test_learn_local_attempt_updates_stats() {
        let mut router = make_router();
        let before = router.stats().total_queries;
        router.learn_local_attempt("hello world", true);
        let after = router.stats().total_queries;
        assert_eq!(after, before + 1);
    }

    #[test]
    fn test_learn_forwarded_updates_stats() {
        let mut router = make_router();
        let before = router.stats().total_queries;
        router.learn_forwarded("complex multi-part question about async Rust");
        let after = router.stats().total_queries;
        assert_eq!(after, before + 1);
        // Forwarded queries don't increment local_attempts
        assert_eq!(router.stats().total_local_attempts, 0);
    }

    #[test]
    fn test_stats_returns_valid_rates() {
        let router = make_router();
        let stats = router.stats();
        assert!(stats.forward_rate >= 0.0 && stats.forward_rate <= 1.0);
        assert!(stats.success_rate >= 0.0 && stats.success_rate <= 1.0);
        assert!(stats.confidence_threshold > 0.0 && stats.confidence_threshold <= 1.0);
    }

    #[test]
    fn test_route_save_and_load_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut router = make_router();
        router.learn_local_attempt("hello", true);
        router.learn_forwarded("complex query");
        router.save(tmp.path()).unwrap();

        let loaded_threshold = Router::load_threshold(tmp.path()).unwrap();
        let loaded_router = Router::new(loaded_threshold);
        let stats = loaded_router.stats();
        assert_eq!(stats.total_queries, 2);
    }
}

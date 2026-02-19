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

// FIXME: Tests disabled - CrisisDetector was removed from the routing system
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_forward_reason_as_str() {
        assert_eq!(ForwardReason::NoMatch.as_str(), "no_match");
        assert_eq!(ForwardReason::LowConfidence.as_str(), "low_confidence");
        assert_eq!(ForwardReason::ModelNotReady.as_str(), "model_not_ready");
    }

    // #[test]
    // fn test_route_with_generator_check_not_ready() {
    //     // FIXME: CrisisDetector no longer exists
    // }

    // #[test]
    // fn test_route_with_generator_check_ready() {
    //     // FIXME: CrisisDetector no longer exists
    // }
}

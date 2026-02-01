// Routing decision logic

use crate::crisis::CrisisDetector;
use crate::models::{ThresholdRouter, ThresholdRouterStats};
use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone)]
pub enum ForwardReason {
    Crisis,
    NoMatch,
    LowConfidence,
}

impl ForwardReason {
    pub fn as_str(&self) -> &str {
        match self {
            ForwardReason::Crisis => "crisis",
            ForwardReason::NoMatch => "no_match",
            ForwardReason::LowConfidence => "low_confidence",
        }
    }
}

#[derive(Debug, Clone)]
pub enum RouteDecision {
    // Keep Local variant for backward compatibility, but it's no longer used
    Local { pattern_id: String, confidence: f64 },
    Forward { reason: ForwardReason },
}

pub struct Router {
    crisis_detector: CrisisDetector,
    threshold_router: ThresholdRouter,
}

impl Router {
    pub fn new(crisis_detector: CrisisDetector, threshold_router: ThresholdRouter) -> Self {
        Self {
            crisis_detector,
            threshold_router,
        }
    }

    /// Make a routing decision for a query
    pub fn route(&self, query: &str) -> RouteDecision {
        // Layer 1: Safety gate - check for crisis
        if self.crisis_detector.detect_crisis(query) {
            tracing::info!("Routing decision: FORWARD (crisis detected)");
            return RouteDecision::Forward {
                reason: ForwardReason::Crisis,
            };
        }

        // Layer 2: Data-driven routing - use threshold model
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

        // Layer 3: Default fallback - forward when uncertain
        tracing::info!("Routing decision: FORWARD (threshold too low)");
        RouteDecision::Forward {
            reason: ForwardReason::NoMatch,
        }
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

    #[test]
    fn test_forward_reason_as_str() {
        assert_eq!(ForwardReason::Crisis.as_str(), "crisis");
        assert_eq!(ForwardReason::NoMatch.as_str(), "no_match");
        assert_eq!(ForwardReason::LowConfidence.as_str(), "low_confidence");
    }
}

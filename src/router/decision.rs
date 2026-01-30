// Routing decision logic

use crate::crisis::CrisisDetector;
use crate::patterns::{Pattern, PatternMatcher};

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
    Local { pattern: Pattern, confidence: f64 },
    Forward { reason: ForwardReason },
}

pub struct Router {
    pattern_matcher: PatternMatcher,
    crisis_detector: CrisisDetector,
}

impl Router {
    pub fn new(pattern_matcher: PatternMatcher, crisis_detector: CrisisDetector) -> Self {
        Self {
            pattern_matcher,
            crisis_detector,
        }
    }

    /// Make a routing decision for a query
    pub fn route(&self, query: &str) -> RouteDecision {
        // Step 1: Check for crisis
        if self.crisis_detector.detect_crisis(query) {
            tracing::info!("Routing decision: FORWARD (crisis detected)");
            return RouteDecision::Forward {
                reason: ForwardReason::Crisis,
            };
        }

        // Step 2: Check for pattern match
        if let Some((pattern, confidence)) = self.pattern_matcher.find_match(query) {
            tracing::info!(
                "Routing decision: LOCAL (pattern: {}, confidence: {:.2})",
                pattern.id,
                confidence
            );
            return RouteDecision::Local {
                pattern,
                confidence,
            };
        }

        // Step 3: Default to forward
        tracing::info!("Routing decision: FORWARD (no pattern match)");
        RouteDecision::Forward {
            reason: ForwardReason::NoMatch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crisis::CrisisDetector;
    use crate::patterns::{PatternLibrary, PatternMatcher};

    #[test]
    fn test_forward_reason_as_str() {
        assert_eq!(ForwardReason::Crisis.as_str(), "crisis");
        assert_eq!(ForwardReason::NoMatch.as_str(), "no_match");
        assert_eq!(ForwardReason::LowConfidence.as_str(), "low_confidence");
    }
}

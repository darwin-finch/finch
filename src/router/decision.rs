// Routing decision logic

use crate::crisis::CrisisDetector;

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
}

impl Router {
    pub fn new(crisis_detector: CrisisDetector) -> Self {
        Self {
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

        // Step 2: No pattern matching - always forward for now
        // (In future commits, this becomes tool-based routing)
        tracing::info!("Routing decision: FORWARD (no local processing)");
        RouteDecision::Forward {
            reason: ForwardReason::NoMatch,
        }
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

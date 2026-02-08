// Hybrid Router - Best of both threshold and neural approaches
//
// Strategy:
// - Phase 1 (queries 1-50): Use threshold-based routing
// - Phase 2 (queries 51-200): Hybrid (threshold + neural with low weight)
// - Phase 3 (queries 201+): Primarily neural with threshold fallback
//
// Benefits:
// - Immediate value from query 1 (threshold models)
// - Interpretable statistics and debugging
// - Smooth transition to neural models as data accumulates
// - Can fall back to thresholds if neural models uncertain

use super::{ForwardReason, RouteDecision};
use crate::models::{
    ModelConfig, ModelEnsemble, Quality, RouteDecision as ModelRouteDecision, ThresholdRouter,
    ThresholdValidator,
};
use anyhow::{Context, Result};

/// Hybrid routing strategy
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HybridStrategy {
    /// Pure threshold-based (queries 1-50)
    ThresholdOnly,
    /// Hybrid: threshold + neural with low weight (queries 51-200)
    Hybrid { neural_weight: f64 },
    /// Primarily neural with threshold fallback (queries 201+)
    NeuralPrimary { threshold_fallback: bool },
}

impl HybridStrategy {
    /// Determine strategy based on query count
    pub fn from_query_count(count: usize) -> Self {
        match count {
            0..=50 => HybridStrategy::ThresholdOnly,
            51..=200 => {
                // Gradually increase neural weight from 0.1 to 0.9
                let progress = (count - 51) as f64 / 150.0;
                let neural_weight = 0.1 + (0.8 * progress);
                HybridStrategy::Hybrid { neural_weight }
            }
            _ => HybridStrategy::NeuralPrimary {
                threshold_fallback: true,
            },
        }
    }
}

/// Hybrid router combining threshold and neural approaches
pub struct HybridRouter {
    /// Threshold-based router (always used)
    threshold_router: ThresholdRouter,
    /// Threshold-based validator (always used)
    threshold_validator: ThresholdValidator,
    /// Neural network ensemble (used after cold start)
    neural_ensemble: Option<ModelEnsemble>,
    /// Total queries processed
    query_count: usize,
}

impl HybridRouter {
    /// Create a new hybrid router (threshold-only initially)
    pub fn new() -> Result<Self> {
        Ok(Self {
            threshold_router: ThresholdRouter::new(),
            threshold_validator: ThresholdValidator::new(),
            neural_ensemble: None,
            query_count: 0,
        })
    }

    /// Create with both threshold and neural models
    pub fn with_neural(config: ModelConfig) -> Result<Self> {
        let ensemble = ModelEnsemble::new(config).context("Failed to create neural ensemble")?;

        Ok(Self {
            threshold_router: ThresholdRouter::new(),
            threshold_validator: ThresholdValidator::new(),
            neural_ensemble: Some(ensemble),
            query_count: 0,
        })
    }

    /// Enable neural models (call after collecting enough data)
    pub fn enable_neural(&mut self, config: ModelConfig) -> Result<()> {
        if self.neural_ensemble.is_none() {
            let ensemble =
                ModelEnsemble::new(config).context("Failed to create neural ensemble")?;
            self.neural_ensemble = Some(ensemble);
        }
        Ok(())
    }

    /// Get current routing strategy
    pub fn strategy(&self) -> HybridStrategy {
        HybridStrategy::from_query_count(self.query_count)
    }

    /// Make routing decision using hybrid approach
    pub fn route(&self, query: &str) -> Result<RouteDecision> {
        let strategy = self.strategy();

        match strategy {
            HybridStrategy::ThresholdOnly => {
                // Pure threshold-based
                let should_try = self.threshold_router.should_try_local(query);
                if should_try {
                    Ok(RouteDecision::Local {
                        pattern_id: "threshold".to_string(),
                        confidence: 0.8,
                    })
                } else {
                    Ok(RouteDecision::Forward {
                        reason: ForwardReason::LowConfidence,
                    })
                }
            }

            HybridStrategy::Hybrid { neural_weight } => {
                // Combine threshold and neural decisions
                let threshold_decision = self.threshold_router.should_try_local(query);

                if let Some(ref ensemble) = self.neural_ensemble {
                    let neural_decision = ensemble.route(query)?;
                    let neural_says_local = matches!(neural_decision, ModelRouteDecision::Local);

                    // Weighted combination
                    let threshold_weight = 1.0 - neural_weight;
                    let combined_score = (threshold_decision as u8 as f64 * threshold_weight)
                        + (neural_says_local as u8 as f64 * neural_weight);

                    if combined_score > 0.5 {
                        Ok(RouteDecision::Local {
                            pattern_id: "hybrid".to_string(),
                            confidence: combined_score,
                        })
                    } else {
                        Ok(RouteDecision::Forward {
                            reason: ForwardReason::LowConfidence,
                        })
                    }
                } else {
                    // No neural model yet, fall back to threshold
                    if threshold_decision {
                        Ok(RouteDecision::Local {
                            pattern_id: "threshold".to_string(),
                            confidence: 0.8,
                        })
                    } else {
                        Ok(RouteDecision::Forward {
                            reason: ForwardReason::LowConfidence,
                        })
                    }
                }
            }

            HybridStrategy::NeuralPrimary { threshold_fallback } => {
                // Primarily neural, with threshold fallback
                if let Some(ref ensemble) = self.neural_ensemble {
                    let neural_decision = ensemble.route(query)?;

                    match neural_decision {
                        ModelRouteDecision::Local => {
                            // Neural says local, but check threshold for safety
                            if threshold_fallback {
                                let threshold_agrees =
                                    self.threshold_router.should_try_local(query);
                                if threshold_agrees {
                                    Ok(RouteDecision::Local {
                                        pattern_id: "neural".to_string(),
                                        confidence: 0.9,
                                    })
                                } else {
                                    // Threshold disagrees, play it safe and forward
                                    Ok(RouteDecision::Forward {
                                        reason: ForwardReason::LowConfidence,
                                    })
                                }
                            } else {
                                Ok(RouteDecision::Local {
                                    pattern_id: "neural".to_string(),
                                    confidence: 0.9,
                                })
                            }
                        }
                        ModelRouteDecision::Forward => Ok(RouteDecision::Forward {
                            reason: ForwardReason::NoMatch,
                        }),
                    }
                } else {
                    // No neural model, fall back to threshold
                    let should_try = self.threshold_router.should_try_local(query);
                    if should_try {
                        Ok(RouteDecision::Local {
                            pattern_id: "threshold".to_string(),
                            confidence: 0.8,
                        })
                    } else {
                        Ok(RouteDecision::Forward {
                            reason: ForwardReason::LowConfidence,
                        })
                    }
                }
            }
        }
    }

    /// Generate response locally (tries neural first, falls back to template)
    pub fn generate_local(&self, query: &str) -> Result<String> {
        if let Some(ref ensemble) = self.neural_ensemble {
            ensemble
                .generate_local(query)
                .context("Failed to generate local response")
        } else {
            // No neural model, return error
            anyhow::bail!("Neural generation not available yet")
        }
    }

    /// Validate response using hybrid approach
    pub fn validate(&self, query: &str, response: &str) -> Result<bool> {
        let strategy = self.strategy();

        match strategy {
            HybridStrategy::ThresholdOnly => {
                // Pure threshold validation
                Ok(self.threshold_validator.validate(query, response))
            }

            HybridStrategy::Hybrid { neural_weight } => {
                // Combine threshold and neural validation
                let threshold_result = self.threshold_validator.validate(query, response);

                if let Some(ref ensemble) = self.neural_ensemble {
                    let neural_result = ensemble.validate(query, response)?;
                    let neural_says_good = matches!(neural_result, Quality::Good);

                    // Weighted combination (require both to agree at high weight)
                    let threshold_weight = 1.0 - neural_weight;
                    let combined_score = (threshold_result as u8 as f64 * threshold_weight)
                        + (neural_says_good as u8 as f64 * neural_weight);

                    Ok(combined_score > 0.6) // Stricter threshold for validation
                } else {
                    Ok(threshold_result)
                }
            }

            HybridStrategy::NeuralPrimary { threshold_fallback } => {
                // Primarily neural validation
                if let Some(ref ensemble) = self.neural_ensemble {
                    let neural_result = ensemble.validate(query, response)?;

                    if threshold_fallback {
                        // Both must agree
                        let threshold_result = self.threshold_validator.validate(query, response);
                        let neural_says_good = matches!(neural_result, Quality::Good);
                        Ok(neural_says_good && threshold_result)
                    } else {
                        Ok(matches!(neural_result, Quality::Good))
                    }
                } else {
                    Ok(self.threshold_validator.validate(query, response))
                }
            }
        }
    }

    /// Learn from Claude response
    pub fn learn_from_claude(
        &mut self,
        query: &str,
        response: &str,
        was_forwarded: bool,
    ) -> Result<()> {
        self.query_count += 1;

        // Always learn with threshold models
        if was_forwarded {
            // We forwarded directly to Claude (no local attempt)
            self.threshold_router.learn_forwarded(query);
        } else {
            // We tried local generation (regardless of success)
            self.threshold_router.learn_local_attempt(query, true);
        }
        self.threshold_validator.learn(query, response, true);

        // Learn with neural models if available
        if let Some(ref mut ensemble) = self.neural_ensemble {
            ensemble.learn_from_claude(query, response, was_forwarded)?;
        }

        Ok(())
    }

    /// Save all models
    pub fn save(&self, models_dir: &str) -> Result<()> {
        std::fs::create_dir_all(models_dir).context("Failed to create models directory")?;

        // Save threshold models
        self.threshold_router
            .save(format!("{}/threshold_router.json", models_dir))?;
        self.threshold_validator
            .save(format!("{}/threshold_validator.json", models_dir))?;

        // Save neural models if present
        if let Some(ref ensemble) = self.neural_ensemble {
            ensemble.save(format!("{}/neural", models_dir))?;
        }

        Ok(())
    }

    /// Get comprehensive statistics
    pub fn stats(&self) -> HybridRouterStats {
        let threshold_router_stats = self.threshold_router.stats();
        let threshold_validator_stats = self.threshold_validator.stats();

        let neural_stats = self.neural_ensemble.as_ref().map(|e| e.stats());

        HybridRouterStats {
            query_count: self.query_count,
            strategy: self.strategy(),
            threshold_router: threshold_router_stats,
            threshold_validator: threshold_validator_stats,
            neural_ensemble: neural_stats,
        }
    }
}

/// Comprehensive statistics from hybrid router
#[derive(Debug)]
pub struct HybridRouterStats {
    pub query_count: usize,
    pub strategy: HybridStrategy,
    pub threshold_router: crate::models::ThresholdRouterStats,
    pub threshold_validator: crate::models::ValidatorStats,
    pub neural_ensemble: Option<crate::models::EnsembleStats>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strategy_progression() {
        assert_eq!(
            HybridStrategy::from_query_count(10),
            HybridStrategy::ThresholdOnly
        );
        assert_eq!(
            HybridStrategy::from_query_count(50),
            HybridStrategy::ThresholdOnly
        );

        match HybridStrategy::from_query_count(100) {
            HybridStrategy::Hybrid { neural_weight } => {
                assert!(neural_weight > 0.1 && neural_weight < 0.9);
            }
            _ => panic!("Expected Hybrid strategy"),
        }

        match HybridStrategy::from_query_count(250) {
            HybridStrategy::NeuralPrimary { threshold_fallback } => {
                assert!(threshold_fallback);
            }
            _ => panic!("Expected NeuralPrimary strategy"),
        }
    }

    #[test]
    fn test_hybrid_router_creation() {
        let router = HybridRouter::new();
        assert!(router.is_ok());
    }

    #[test]
    fn test_learning() -> Result<()> {
        let mut router = HybridRouter::new()?;

        for i in 0..10 {
            router.learn_from_claude("test query", "test response", true)?;
            assert_eq!(router.query_count, i + 1);
        }

        Ok(())
    }
}

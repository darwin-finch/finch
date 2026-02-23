// Phase 2: Model-based routing with online learning
// Uses the ModelEnsemble (Router, Generator, Validator) for intelligent routing

use super::{ForwardReason, RouteDecision};
use crate::models::{ModelConfig, ModelEnsemble, Quality, RouteDecision as ModelRouteDecision};
use anyhow::{Context, Result};

/// Model-based router using neural networks
pub struct ModelRouter {
    ensemble: ModelEnsemble,
}

impl ModelRouter {
    /// Create a new model router with default configuration
    pub fn new() -> Result<Self> {
        let config = ModelConfig::default();
        let ensemble = ModelEnsemble::new(config).context("Failed to create model ensemble")?;

        Ok(Self { ensemble })
    }

    /// Create a model router with custom configuration
    pub fn with_config(config: ModelConfig) -> Result<Self> {
        let ensemble = ModelEnsemble::new(config).context("Failed to create model ensemble")?;

        Ok(Self { ensemble })
    }

    /// Make a routing decision using the neural network ensemble
    pub async fn route(&self, query: &str) -> Result<RouteDecision> {
        // Get decision from model ensemble
        let decision = self
            .ensemble
            .route(query)
            .await
            .context("Failed to get routing decision from models")?;

        // Convert to Phase 1 RouteDecision format
        match decision {
            ModelRouteDecision::Local => {
                // For now, return a dummy pattern
                // In the future, we might want to generate a pattern ID based on the query
                Ok(RouteDecision::Forward {
                    reason: ForwardReason::NoMatch, // Placeholder
                })
            }
            ModelRouteDecision::Forward => Ok(RouteDecision::Forward {
                reason: ForwardReason::NoMatch,
            }),
            ModelRouteDecision::Remote => Ok(RouteDecision::Forward {
                reason: ForwardReason::NoMatch,
            }),
        }
    }

    /// Generate a response locally using the generator model
    pub async fn generate_local(&self, query: &str) -> Result<String> {
        self.ensemble
            .generate_local(query)
            .await
            .context("Failed to generate local response")
    }

    /// Validate a generated response
    pub async fn validate(&self, query: &str, response: &str) -> Result<bool> {
        let quality = self
            .ensemble
            .validate(query, response)
            .await
            .context("Failed to validate response")?;

        Ok(matches!(quality, Quality::Good))
    }

    /// Learn from a Claude response (online learning)
    ///
    /// Call this after forwarding to Claude to update the models
    pub fn learn_from_claude(
        &mut self,
        query: &str,
        claude_response: &str,
        was_forwarded_because_router: bool,
    ) -> Result<()> {
        self.ensemble
            .learn_from_claude(query, claude_response, was_forwarded_because_router)
            .context("Failed to learn from Claude response")
    }

    /// Learn from a local generation attempt
    ///
    /// Call this after trying local generation (whether successful or not)
    pub fn learn_from_local_attempt(
        &mut self,
        query: &str,
        local_response: &str,
        was_good: bool,
        claude_response_if_bad: Option<&str>,
    ) -> Result<()> {
        let quality = if was_good {
            Quality::Good
        } else {
            Quality::Bad
        };

        self.ensemble
            .learn_from_local_attempt(query, local_response, quality, claude_response_if_bad)
            .context("Failed to learn from local attempt")
    }

    /// Save the models to disk
    pub fn save_models(&self, models_dir: &str) -> Result<()> {
        let path = std::path::Path::new(models_dir);
        self.ensemble.save(path).context("Failed to save models")
    }

    /// Get statistics about the ensemble
    pub fn stats(&self) -> (usize, f64) {
        let stats = self.ensemble.stats();
        (stats.query_count, stats.learning_rate)
    }

    /// Get query count
    pub fn query_count(&self) -> usize {
        self.ensemble.query_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_router_creation() {
        let router = ModelRouter::new();
        assert!(router.is_ok());
    }
}

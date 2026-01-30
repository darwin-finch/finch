// Model Ensemble - Coordinates Router, Generator, and Validator
// Implements the online learning training loop

use anyhow::{Context, Result};
use candle_core::Tensor;
use std::path::Path;

use super::common::{get_device, ModelConfig, Saveable};
use super::generator::GeneratorModel;
use super::router::RouterModel;
use super::tokenizer::TextTokenizer;
use super::validator::ValidatorModel;

/// Decision made by the router
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDecision {
    /// Forward to Claude API
    Forward,
    /// Try local generation
    Local,
}

/// Quality assessment by the validator
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    /// Response is good enough
    Good,
    /// Response is not good enough (need to forward)
    Bad,
}

/// The full 3-model ensemble with online learning
pub struct ModelEnsemble {
    router: RouterModel,
    generator: GeneratorModel,
    validator: ValidatorModel,
    tokenizer: TextTokenizer,
    config: ModelConfig,
    query_count: usize,
    learning_rate: f64,
}

impl ModelEnsemble {
    /// Create new ensemble with random initialization
    pub fn new(config: ModelConfig) -> Result<Self> {
        let router = RouterModel::new(&config).context("Failed to create router model")?;

        let generator = GeneratorModel::new(&config).context("Failed to create generator model")?;

        let validator = ValidatorModel::new(&config).context("Failed to create validator model")?;

        let tokenizer = TextTokenizer::default().context("Failed to create tokenizer")?;

        Ok(Self {
            router,
            generator,
            validator,
            tokenizer,
            config,
            query_count: 0,
            learning_rate: 0.001, // Default Adam learning rate
        })
    }

    /// Load ensemble from saved models
    pub fn load<P: AsRef<Path>>(_models_dir: P) -> Result<Self> {
        // TODO: Implement proper loading
        unimplemented!("Model loading not yet implemented")
    }

    /// Save ensemble to disk
    pub fn save<P: AsRef<Path>>(&self, models_dir: P) -> Result<()> {
        let models_dir = models_dir.as_ref();
        std::fs::create_dir_all(models_dir).context("Failed to create models directory")?;

        self.router
            .save(&models_dir.join("router.safetensors"))
            .context("Failed to save router model")?;

        self.generator
            .save(&models_dir.join("generator.safetensors"))
            .context("Failed to save generator model")?;

        self.validator
            .save(&models_dir.join("validator.safetensors"))
            .context("Failed to save validator model")?;

        self.tokenizer
            .save(&models_dir.join("tokenizer.json"), true)
            .context("Failed to save tokenizer")?;

        Ok(())
    }

    /// Get the router's decision: forward or local?
    pub fn route(&self, query: &str) -> Result<RouteDecision> {
        // Cold start: always forward for first 50 queries
        if self.query_count < 50 {
            return Ok(RouteDecision::Forward);
        }

        // Tokenize query
        let device = get_device()?;
        let query_tokens = self.tokenizer.encode_as_tensor(query, &device)?;

        // Get router prediction
        let should_try_local = self.router.predict(&query_tokens)?;

        // Conservative mode (queries 50-200): higher bar
        if self.query_count < 200 {
            // Only try local if router is very confident
            // For now, just use the binary decision
            // TODO: Could add confidence threshold here if we change to probability output
            if should_try_local {
                Ok(RouteDecision::Local)
            } else {
                Ok(RouteDecision::Forward)
            }
        } else {
            // Normal mode: trust the router
            if should_try_local {
                Ok(RouteDecision::Local)
            } else {
                Ok(RouteDecision::Forward)
            }
        }
    }

    /// Generate response locally
    pub fn generate_local(&self, query: &str) -> Result<String> {
        let device = get_device()?;
        let query_tokens = self.tokenizer.encode_as_tensor(query, &device)?;

        // Generate response tokens
        let max_new_tokens = 512;
        let response_ids = self.generator.generate(&query_tokens, max_new_tokens)?;

        // Decode back to text
        let response = self.tokenizer.decode(&response_ids, true)?;

        Ok(response)
    }

    /// Validate a generated response
    pub fn validate(&self, query: &str, response: &str) -> Result<Quality> {
        let device = get_device()?;

        // Tokenize query and response
        let query_tokens = self.tokenizer.encode_as_tensor(query, &device)?;
        let response_tokens = self.tokenizer.encode_as_tensor(response, &device)?;

        // Get validator decision
        let is_good = self.validator.validate(&query_tokens, &response_tokens)?;

        if is_good {
            Ok(Quality::Good)
        } else {
            Ok(Quality::Bad)
        }
    }

    /// Learn from a Claude response (online learning)
    ///
    /// This is called after every forward to Claude.
    /// It updates all three models based on the interaction.
    pub fn learn_from_claude(
        &mut self,
        query: &str,
        claude_response: &str,
        was_forwarded_because_router: bool,
    ) -> Result<()> {
        self.query_count += 1;

        let device = get_device()?;
        let query_tokens = self.tokenizer.encode_as_tensor(query, &device)?;

        // Always train the generator (distillation from Claude)
        let claude_response_ids = self.tokenizer.encode(claude_response, true)?;
        self.generator
            .update(&query_tokens, &claude_response_ids, self.learning_rate)?;

        // Train router based on whether this was a good decision
        if was_forwarded_because_router {
            // Router said "forward" - this is correct if we got here
            // Target: 0 (forward was correct)
            self.router
                .update(&query_tokens, false, self.learning_rate)?;
        }

        // Strategic sampling: occasionally try local generation too
        // This gives us training data for router and validator
        if self.should_sample() {
            let local_response = self.generate_local(query)?;

            // Measure divergence (semantic similarity)
            // For now, simple string comparison as placeholder
            let divergence = self.measure_divergence(&local_response, claude_response);
            let threshold = 0.2; // 20% difference is acceptable

            if divergence < threshold {
                // Local response was good enough - router should have tried local
                // Target: 1 (should try local)
                self.router
                    .update(&query_tokens, true, self.learning_rate)?;
            } else {
                // Local response was not good - router was right to forward
                // Target: 0 (forward was correct)
                self.router
                    .update(&query_tokens, false, self.learning_rate)?;
            }
        }

        Ok(())
    }

    /// Learn from a local generation that was validated
    pub fn learn_from_local_attempt(
        &mut self,
        query: &str,
        local_response: &str,
        quality: Quality,
        claude_response_if_bad: Option<&str>,
    ) -> Result<()> {
        self.query_count += 1;

        let device = get_device()?;
        let query_tokens = self.tokenizer.encode_as_tensor(query, &device)?;
        let response_tokens = self.tokenizer.encode_as_tensor(local_response, &device)?;

        match quality {
            Quality::Good => {
                // Validator said good - this is a success!
                // Router was correct to try local: target = 1
                self.router
                    .update(&query_tokens, true, self.learning_rate)?;

                // Validator was correct: target = 1 (good)
                self.validator
                    .update(&query_tokens, &response_tokens, true, self.learning_rate)?;
            }
            Quality::Bad => {
                // Validator rejected - this is a failure
                // Router should have forwarded: target = 0
                self.router
                    .update(&query_tokens, false, self.learning_rate)?;

                // Validator was correct: target = 0 (bad)
                self.validator.update(
                    &query_tokens,
                    &response_tokens,
                    false,
                    self.learning_rate,
                )?;

                // If we have Claude's response, train generator on it
                if let Some(claude_resp) = claude_response_if_bad {
                    let claude_ids = self.tokenizer.encode(claude_resp, true)?;
                    self.generator
                        .update(&query_tokens, &claude_ids, self.learning_rate)?;
                }
            }
        }

        Ok(())
    }

    /// Determine if we should sample (get both local and Claude response)
    fn should_sample(&self) -> bool {
        use rand::Rng;

        let sampling_rate = match self.query_count {
            0..=100 => 0.50,    // 50% - learn quickly
            101..=500 => 0.20,  // 20% - still learning
            501..=2000 => 0.10, // 10% - refinement
            _ => 0.05,          // 5% - maintenance
        };

        rand::thread_rng().gen::<f64>() < sampling_rate
    }

    /// Measure divergence between two responses
    /// Returns a value between 0.0 (identical) and 1.0 (completely different)
    fn measure_divergence(&self, response1: &str, response2: &str) -> f64 {
        // Simple placeholder: character-level difference
        // TODO: Implement proper semantic similarity using embeddings
        let len1 = response1.len();
        let len2 = response2.len();

        if len1 == 0 && len2 == 0 {
            return 0.0;
        }

        // Simple Levenshtein-like metric
        let max_len = len1.max(len2);
        let min_len = len1.min(len2);

        let length_diff = (max_len - min_len) as f64 / max_len as f64;

        // TODO: Replace with proper semantic similarity
        length_diff
    }

    /// Get current statistics
    pub fn stats(&self) -> EnsembleStats {
        EnsembleStats {
            query_count: self.query_count,
            learning_rate: self.learning_rate,
        }
    }

    /// Get query count
    pub fn query_count(&self) -> usize {
        self.query_count
    }
}

/// Statistics about the ensemble
#[derive(Debug, Clone)]
pub struct EnsembleStats {
    pub query_count: usize,
    pub learning_rate: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensemble_creation() {
        let config = ModelConfig::default();
        let ensemble = ModelEnsemble::new(config);
        assert!(ensemble.is_ok());
    }

    #[test]
    fn test_cold_start_routing() -> Result<()> {
        let config = ModelConfig::default();
        let ensemble = ModelEnsemble::new(config)?;

        // First 50 queries should always forward
        let decision = ensemble.route("Hello, world!")?;
        assert_eq!(decision, RouteDecision::Forward);

        Ok(())
    }

    #[test]
    fn test_sampling_rate() -> Result<()> {
        let config = ModelConfig::default();
        let mut ensemble = ModelEnsemble::new(config)?;

        // Early queries should sample frequently
        ensemble.query_count = 50;
        let mut sample_count = 0;
        for _ in 0..100 {
            if ensemble.should_sample() {
                sample_count += 1;
            }
        }
        // Should be around 50 (50% sampling rate)
        assert!(sample_count > 30 && sample_count < 70);

        Ok(())
    }

    #[test]
    fn test_divergence_measurement() -> Result<()> {
        let config = ModelConfig::default();
        let ensemble = ModelEnsemble::new(config)?;

        // Identical strings
        let div1 = ensemble.measure_divergence("hello", "hello");
        assert_eq!(div1, 0.0);

        // Completely different lengths
        let div2 = ensemble.measure_divergence("short", "this is much longer");
        assert!(div2 > 0.5);

        Ok(())
    }
}

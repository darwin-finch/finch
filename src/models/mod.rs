// Machine learning models
// All models support online learning (update after each forward to Claude)

pub mod adapters;  // Local model adapters (chat templates, token IDs)
pub mod bootstrap; // Progressive bootstrap for instant startup
pub mod common;
pub mod compatibility; // Model compatibility matrix (which models work with which targets)
pub mod download;
pub mod generator_new; // New unified generator (ONNX-based)
pub mod learning;
pub mod loaders; // ONNX model loader
pub mod lora; // LoRA fine-tuning configuration (Python training, Phase 5)
pub mod manager;
pub mod model_selector;
pub mod persistence;
pub mod sampling; // Context-aware sampling system
pub mod threshold_router;
pub mod threshold_validator;
pub mod tokenizer; // Phase 4: Stub for compatibility
pub mod tool_parser;  // Phase 6: Parse tool calls from model output (XML)
pub mod tool_prompt;  // Phase 6: Format tool definitions for model prompts
pub mod unified_loader; // Generic loader for ONNX models

pub use adapters::{AdapterRegistry, LocalModelAdapter, GenerationConfig as AdapterGenerationConfig};
pub use bootstrap::{BootstrapLoader, DownloadProgressSnapshot, GeneratorState};
#[allow(deprecated)]
pub use common::{
    device_info, get_device_with_preference, is_metal_available, DevicePreference,
    GeneratorConfig, ModelConfig, Saveable,
};
pub use compatibility::{
    get_available_sizes, get_compatible_families, get_repository, get_supported_targets,
    is_compatible, ModelCompatibility,
};
pub use download::{DownloadProgress, ModelDownloader};
pub use generator_new::{GeneratorModel, TextGeneration, TokenCallback};
pub use learning::{LearningModel, ModelExpectation, ModelPrediction, ModelStats, PredictionData};
pub use lora::{
    LoRATrainingAdapter, LoRAConfig, LoRATrainer, TrainingCoordinator, TrainingStats, WeightedExample,
    ExampleBuffer,
};
pub use manager::{ModelManager, OverallStats, TrainingReport};
pub use model_selector::{ModelSelector, QwenSize};
pub use persistence::{load_model_metadata, model_exists, save_model_with_metadata, ModelMetadata};
pub use sampling::{ComparisonResult, QueryCategory, Sampler, SamplingConfig, SamplingDecision};
pub use threshold_router::{
    QueryCategory as ThresholdQueryCategory, ThresholdRouter, ThresholdRouterStats,
};
pub use threshold_validator::{QualitySignal, ThresholdValidator, ValidatorStats};
pub use tokenizer::TextTokenizer; // Phase 4: Stub for compatibility
pub use tool_parser::ToolCallParser;  // Phase 6: Parse tool calls from model output
pub use tool_prompt::ToolPromptFormatter;  // Phase 6: Format tool definitions for prompts
pub use unified_loader::{ModelFamily, ModelLoadConfig, ModelSize, UnifiedModelLoader};

/// Stub for removed RouterModel (Phase 4: Candle-based)
#[derive(Debug)]
pub struct RouterModel;

impl RouterModel {
    pub fn new(_config: &ModelConfig) -> anyhow::Result<Self> {
        anyhow::bail!("RouterModel removed in Phase 4 (Candle-based)")
    }

    pub fn load(_path: &std::path::Path) -> anyhow::Result<Self> {
        anyhow::bail!("RouterModel removed in Phase 4 (Candle-based)")
    }

    pub fn save(&self, _path: &std::path::Path) -> anyhow::Result<()> {
        anyhow::bail!("RouterModel removed in Phase 4 (Candle-based)")
    }

    pub fn predict(&self, _input: &str) -> anyhow::Result<f64> {
        anyhow::bail!("RouterModel removed in Phase 4 (Candle-based)")
    }
}

/// Stub for removed ValidatorModel (Phase 4: Candle-based)
#[derive(Debug)]
pub struct ValidatorModel;

impl ValidatorModel {
    pub fn new(_config: &ModelConfig) -> anyhow::Result<Self> {
        anyhow::bail!("ValidatorModel removed in Phase 4 (Candle-based)")
    }

    pub fn load(_path: &std::path::Path) -> anyhow::Result<Self> {
        anyhow::bail!("ValidatorModel removed in Phase 4 (Candle-based)")
    }

    pub fn save(&self, _path: &std::path::Path) -> anyhow::Result<()> {
        anyhow::bail!("ValidatorModel removed in Phase 4 (Candle-based)")
    }

    pub fn validate(&self, _input: &str, _output: &str) -> anyhow::Result<f64> {
        anyhow::bail!("ValidatorModel removed in Phase 4 (Candle-based)")
    }
}

/// Stub for removed EnsembleStats (Phase 4: Candle-based)
#[derive(Debug, Clone)]
pub struct EnsembleStats {
    pub query_count: usize,
    pub learning_rate: f64,
}

/// Stub for removed ModelEnsemble (Phase 4: Candle-based)
#[derive(Debug)]
pub struct ModelEnsemble;

impl ModelEnsemble {
    // Phase 4: Multiple signatures for compatibility
    pub fn new(_config: ModelConfig) -> anyhow::Result<Self> {
        Ok(Self)
    }

    pub fn from_models(
        _router: std::sync::Arc<tokio::sync::RwLock<RouterModel>>,
        _generator: std::sync::Arc<tokio::sync::RwLock<GeneratorModel>>,
        _validator: std::sync::Arc<tokio::sync::RwLock<ValidatorModel>>,
    ) -> Self {
        Self
    }

    pub async fn generate(&self, _query: &str) -> anyhow::Result<(String, RouteDecision)> {
        anyhow::bail!("ModelEnsemble removed in Phase 4 (Candle-based)")
    }

    pub async fn route(&self, _query: &str) -> anyhow::Result<RouteDecision> {
        anyhow::bail!("ModelEnsemble removed in Phase 4 (Candle-based)")
    }

    pub async fn generate_local(&self, _query: &str) -> anyhow::Result<String> {
        anyhow::bail!("ModelEnsemble removed in Phase 4 (Candle-based)")
    }

    pub async fn validate(&self, _query: &str, _response: &str) -> anyhow::Result<Quality> {
        anyhow::bail!("ModelEnsemble removed in Phase 4 (Candle-based)")
    }

    pub fn context(&self) -> Vec<String> {
        vec![]
    }

    pub fn stats(&self) -> EnsembleStats {
        EnsembleStats {
            query_count: 0,
            learning_rate: 0.0,
        }
    }

    pub fn save(&self, _path: &std::path::Path) -> anyhow::Result<()> {
        anyhow::bail!("ModelEnsemble removed in Phase 4 (Candle-based)")
    }

    pub fn query_count(&self) -> usize {
        0
    }

    pub fn learn_from_local_attempt(
        &self,
        _query: &str,
        _response: &str,
        _quality: Quality,
        _claude_response_if_bad: Option<&str>,
    ) -> anyhow::Result<()> {
        anyhow::bail!("ModelEnsemble removed in Phase 4 (Candle-based)")
    }

    pub fn learn_from_claude(&mut self, _query: &str, _response: &str, _was_forwarded: bool) -> anyhow::Result<()> {
        anyhow::bail!("ModelEnsemble removed in Phase 4 (Candle-based)")
    }
}

/// Stub for removed Quality (Phase 4: Candle-based)
#[derive(Debug, Clone)]
pub enum Quality {
    Low,
    Medium,
    High,
    Good,
    Bad,
}

/// Stub for removed RouteDecision (Phase 4: Candle-based)
#[derive(Debug, Clone)]
pub enum RouteDecision {
    Local,
    Remote,
    Forward,
}

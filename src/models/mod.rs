// Machine learning models
// All models support online learning (update after each forward to Claude)

pub mod adapters; // Local model adapters (chat templates, token IDs)
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
pub mod tool_parser; // Phase 6: Parse tool calls from model output (XML)
pub mod tool_prompt; // Phase 6: Format tool definitions for model prompts
pub mod unified_loader; // Generic loader for ONNX models

pub use adapters::{
    AdapterRegistry, GenerationConfig as AdapterGenerationConfig, LocalModelAdapter,
};
pub use bootstrap::{BootstrapLoader, DownloadProgressSnapshot, GeneratorState};
#[allow(deprecated)]
pub use common::{
    device_info, get_device_with_preference, is_metal_available, DevicePreference, GeneratorConfig,
    ModelConfig, Saveable,
};
pub use compatibility::{
    get_available_sizes, get_compatible_families, get_repository, get_supported_targets,
    is_compatible, ModelCompatibility,
};
pub use download::{DownloadProgress, ModelDownloader};
pub use generator_new::{GeneratorModel, TextGeneration, TokenCallback};
pub use learning::{LearningModel, ModelExpectation, ModelPrediction, ModelStats, PredictionData};
pub use lora::{
    ExampleBuffer, LoRAConfig, LoRATrainer, LoRATrainingAdapter, TrainingCoordinator,
    TrainingStats, WeightedExample,
};
pub use manager::{ModelManager, OverallStats, TrainingReport};
pub use model_selector::{ModelSelector, QwenSize};
#[allow(deprecated)]
pub use persistence::{load_model_metadata, model_exists, save_model_with_metadata, ModelMetadata};
pub use sampling::{ComparisonResult, QueryCategory, Sampler, SamplingConfig, SamplingDecision};
pub use threshold_router::{
    QueryCategory as ThresholdQueryCategory, ThresholdRouter, ThresholdRouterStats,
};
pub use threshold_validator::{QualitySignal, ThresholdValidator, ValidatorStats};
pub use tokenizer::TextTokenizer; // Phase 4: Stub for compatibility
pub use tool_parser::ToolCallParser; // Phase 6: Parse tool calls from model output
pub use tool_prompt::ToolPromptFormatter; // Phase 6: Format tool definitions for prompts
pub use unified_loader::{ModelFamily, ModelLoadConfig, ModelSize, UnifiedModelLoader};


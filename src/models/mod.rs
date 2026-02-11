// Machine learning models
// All models support online learning (update after each forward to Claude)

pub mod bootstrap; // Progressive bootstrap for instant startup
pub mod common;
pub mod download;
// pub mod ensemble; // Phase 4: Commented out (Candle-based)
// pub mod generator; // Phase 4: Commented out (Legacy Candle transformer)
pub mod generator_new; // New unified generator (ONNX-based)
pub mod learning;
pub mod loaders; // ONNX model loader
pub mod lora; // LoRA fine-tuning configuration (Python training, Phase 5)
pub mod manager;
pub mod model_selector;
pub mod persistence;
// pub mod router; // Phase 4: Commented out (Candle-based)
pub mod sampling; // Context-aware sampling system
pub mod threshold_router;
pub mod threshold_validator;
// pub mod tokenizer; // Phase 4: Commented out (Candle-based)
pub mod unified_loader; // Generic loader for ONNX models
// pub mod validator; // Phase 4: Commented out (Candle-based)

pub use bootstrap::{BootstrapLoader, DownloadProgressSnapshot, GeneratorState};
pub use common::{GeneratorConfig, ModelConfig};
pub use download::{DownloadProgress, ModelDownloader};
// pub use ensemble::{EnsembleStats, ModelEnsemble, Quality, RouteDecision}; // Phase 4: Candle-based
pub use generator_new::{GeneratorModel, TextGeneration};
pub use learning::{LearningModel, ModelExpectation, ModelPrediction, ModelStats, PredictionData};
pub use lora::{
    LoRAAdapter, LoRAConfig, LoRATrainer, TrainingCoordinator, TrainingStats, WeightedExample,
    ExampleBuffer,
};
pub use manager::{ModelManager, OverallStats, TrainingReport};
pub use model_selector::{ModelSelector, QwenSize};
pub use persistence::{load_model_metadata, model_exists, save_model_with_metadata, ModelMetadata};
// pub use router::RouterModel; // Phase 4: Candle-based
pub use sampling::{ComparisonResult, QueryCategory, Sampler, SamplingConfig, SamplingDecision};
pub use threshold_router::{
    QueryCategory as ThresholdQueryCategory, ThresholdRouter, ThresholdRouterStats,
};
pub use threshold_validator::{QualitySignal, ThresholdValidator, ValidatorStats};
// pub use tokenizer::TextTokenizer; // Phase 4: Candle-based
pub use unified_loader::{ModelFamily, ModelLoadConfig, ModelSize, UnifiedModelLoader};
// pub use validator::ValidatorModel; // Phase 4: Candle-based

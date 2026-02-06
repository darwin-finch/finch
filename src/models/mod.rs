// Machine learning models
// All models support online learning (update after each forward to Claude)

pub mod bootstrap; // Progressive bootstrap for instant startup
pub mod common;
pub mod download;
pub mod ensemble;
pub mod generator; // Legacy custom transformer
pub mod generator_new; // New unified generator
pub mod learning;
pub mod lora; // LoRA fine-tuning (placeholder)
pub mod manager;
pub mod model_selector;
pub mod persistence;
pub mod qwen_loader;
pub mod router;
pub mod threshold_router;
pub mod threshold_validator;
pub mod tokenizer;
pub mod validator;

pub use bootstrap::{BootstrapLoader, DownloadProgressSnapshot, GeneratorState};
pub use common::{
    device_info, get_device, get_device_with_preference, is_metal_available, DevicePreference,
    GeneratorConfig, ModelConfig, Saveable,
};
pub use download::{DownloadProgress, ModelDownloader};
pub use ensemble::{EnsembleStats, ModelEnsemble, Quality, RouteDecision};
// Export both old and new generator APIs for compatibility
pub use generator::GeneratorModel as LegacyGeneratorModel;
pub use generator_new::{GeneratorModel, TextGeneration};
pub use learning::{LearningModel, ModelExpectation, ModelPrediction, ModelStats, PredictionData};
pub use lora::{LoRAAdapter, LoRAConfig};
pub use manager::{ModelManager, OverallStats, TrainingReport};
pub use model_selector::{ModelSelector, QwenSize};
pub use persistence::{load_model_metadata, model_exists, save_model_with_metadata, ModelMetadata};
pub use qwen_loader::{LoadedQwenModel, QwenConfig, QwenLoader};
pub use router::RouterModel;
pub use threshold_router::{QueryCategory, ThresholdRouter, ThresholdRouterStats};
pub use threshold_validator::{QualitySignal, ThresholdValidator, ValidatorStats};
pub use tokenizer::TextTokenizer;
pub use validator::ValidatorModel;

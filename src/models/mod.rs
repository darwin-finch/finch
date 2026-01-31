// Machine learning models
// All models support online learning (update after each forward to Claude)

pub mod common;
pub mod ensemble;
pub mod generator;
pub mod learning;
pub mod manager;
pub mod router;
pub mod threshold_router;
pub mod threshold_validator;
pub mod tokenizer;
pub mod validator;

pub use common::{
    device_info, get_device, get_device_with_preference, is_metal_available, DevicePreference,
    ModelConfig, Saveable,
};
pub use ensemble::{EnsembleStats, ModelEnsemble, Quality, RouteDecision};
pub use generator::GeneratorModel;
pub use learning::{LearningModel, ModelExpectation, ModelPrediction, ModelStats, PredictionData};
pub use manager::{ModelManager, OverallStats, TrainingReport};
pub use router::RouterModel;
pub use threshold_router::{QueryCategory, ThresholdRouter, ThresholdRouterStats};
pub use threshold_validator::{QualitySignal, ThresholdValidator, ValidatorStats};
pub use tokenizer::TextTokenizer;
pub use validator::ValidatorModel;

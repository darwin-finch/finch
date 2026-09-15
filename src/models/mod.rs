// Machine learning models
// All models support online learning (update after each forward to Claude)

mod adapters; // Local model adapters (chat templates, token IDs)
mod bootstrap; // Progressive bootstrap for instant startup
mod common;
mod compatibility; // Model compatibility matrix (which models work with which targets)
mod download;
mod generator_new; // New unified generator (ONNX-based)
mod learning;
mod loaders; // ONNX model loader
mod lora; // LoRA fine-tuning configuration (Python training, Phase 5)
mod manager;
mod model_selector;
mod neural_embedding;
mod persistence;
mod progress;
mod sampling; // Context-aware sampling system
mod threshold_router;
mod threshold_validator;
mod tokenizer; // Phase 4: Stub for compatibility
mod tool_parser; // Phase 6: Parse tool calls from model output (XML)
mod tool_prompt; // Phase 6: Format tool definitions for model prompts
mod unified_loader; // Generic loader for ONNX models

pub use adapters::{
    AdapterRegistry, DeepSeekAdapter, GenerationConfig as AdapterGenerationConfig, LlamaAdapter,
    LocalModelAdapter, MistralAdapter, PhiAdapter, QwenAdapter,
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
pub use loaders::onnx::LoadedOnnxModel;
pub use lora::{
    ExampleBuffer, LoRAConfig, LoRATrainer, LoRATrainingAdapter, TrainingCoordinator,
    TrainingStats, WeightedExample,
};
pub use manager::{ModelManager, OverallStats, TrainingReport};
pub use model_selector::{ModelSelection, ModelSelector, QwenSize};
pub use neural_embedding::{select_memory_embedding_engine, NeuralEmbeddingEngine};
#[allow(deprecated)]
pub use persistence::{load_model_metadata, model_exists, save_model_with_metadata, ModelMetadata};
pub use progress::{
    install_model_progress, DownloadProgressDisplay, ModelProgress, SilentModelProgress,
};
pub use sampling::{ComparisonResult, QueryCategory, Sampler, SamplingConfig, SamplingDecision};
pub use threshold_router::{
    QueryCategory as ThresholdQueryCategory, ThresholdRouter, ThresholdRouterStats,
};
pub use threshold_validator::{QualitySignal, ThresholdValidator, ValidatorStats};
pub use tokenizer::TextTokenizer; // Phase 4: Stub for compatibility
pub use tool_parser::ToolCallParser; // Phase 6: Parse tool calls from model output
pub use tool_prompt::ToolPromptFormatter; // Phase 6: Format tool definitions for prompts
pub use unified_loader::{
    InferenceProvider, ModelFamily, ModelLoadConfig, ModelSize, UnifiedModelLoader,
};

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    #[test]
    fn models_facade_keeps_child_modules_private() {
        let facade = include_str!("mod.rs");
        let published = facade
            .lines()
            .map(str::trim_start)
            .filter(|line| !line.starts_with("//"))
            .filter(|line| line.starts_with("pub mod "))
            .collect::<Vec<_>>();
        assert!(
            published.is_empty(),
            "models facade must keep child modules private; found: {published:?}"
        );
    }

    #[test]
    fn models_production_sources_do_not_import_cli() {
        let needle = ["crate::", "cli"].concat();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/models");
        let mut hits = Vec::new();
        collect_cli_imports(&root, &root, &needle, &mut hits);
        assert!(
            hits.is_empty(),
            "models production code must not import {needle}; found: {hits:?}"
        );
    }

    fn collect_cli_imports(root: &Path, dir: &Path, needle: &str, hits: &mut Vec<String>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                hits.push(format!("failed to read {}: {error}", dir.display()));
                return;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_cli_imports(root, &path, needle, hits);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                hits.push(format!("failed to read {}", path.display()));
                continue;
            };
            for (index, line) in source.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                if line.contains(needle) {
                    let rel = path.strip_prefix(root).unwrap_or(&path);
                    hits.push(format!("{}:{}", rel.display(), index + 1));
                }
            }
        }
    }
}

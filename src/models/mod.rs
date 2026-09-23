// Optional local-model loading, adapters, and routing support.
// Configuration and loader code are not evidence of backend conformance.

mod adapters; // Local model adapters (chat templates, token IDs)
mod bootstrap; // Progressive bootstrap for instant startup
mod common;
mod generator_new;
mod gguf_download;
mod learning;
mod loaders;
mod lora; // LoRA fine-tuning configuration (Python training, Phase 5)
mod manager;
mod model_selector;
mod neural_embedding;
mod persistence;
mod progress;
mod sampling; // Context-aware sampling system
mod threshold_router;
mod threshold_validator;
mod tool_parser; // Phase 6: Parse tool calls from model output (XML)
mod tool_prompt; // Phase 6: Format tool definitions for model prompts
mod unified_loader; // llama.cpp GGUF chat loader

pub use adapters::{
    AdapterRegistry, DeepSeekAdapter, GenerationConfig as AdapterGenerationConfig, LlamaAdapter,
    LocalModelAdapter, MistralAdapter, PhiAdapter, QwenAdapter,
};
pub use bootstrap::{BootstrapLoader, DownloadProgressSnapshot, GeneratorState};
#[allow(deprecated)]
pub use common::{DevicePreference, GeneratorConfig, ModelConfig, Saveable};
pub use generator_new::{GeneratorModel, TextGeneration, TokenCallback};
pub use gguf_download::{managed_gguf_artifact, GgufQuantization, ManagedGgufArtifact};
pub use learning::{LearningModel, ModelExpectation, ModelPrediction, ModelStats, PredictionData};
pub use lora::{
    ExampleBuffer, LoRAConfig, LoRATrainer, LoRATrainingAdapter, TrainingCoordinator,
    TrainingStats, WeightedExample,
};
pub use manager::{ModelManager, OverallStats, TrainingReport};
pub use model_selector::{ModelSelection, ModelSelector, QwenSize};
pub use neural_embedding::{select_memory_embedding_engine, NeuralEmbeddingEngine};
pub use persistence::{load_model_metadata, model_exists, ModelMetadata};
pub use progress::{ModelProgress, SilentModelProgress};
pub use sampling::{ComparisonResult, QueryCategory, Sampler, SamplingConfig, SamplingDecision};
pub use threshold_router::{
    QueryCategory as ThresholdQueryCategory, ThresholdRouter, ThresholdRouterStats,
};
pub use threshold_validator::{QualitySignal, ThresholdValidator, ValidatorStats};
pub use tool_parser::ToolCallParser; // Phase 6: Parse tool calls from model output
pub use tool_prompt::ToolPromptFormatter; // Phase 6: Format tool definitions for prompts
pub use unified_loader::{
    FamilyEngineCapabilities, InferenceProvider, ModelFamily, ModelLoadConfig, ModelSize,
    UnifiedModelLoader,
};

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// #781: family description surfaces must not carry capability or quality
    /// claims the local engine cannot honor ("Best overall quality",
    /// "good for chat", ...). The claim-carrying `ModelFamily::description`
    /// was deleted with this ticket; the scan pins its absence from the loader
    /// module, which now states identity and engine-proven claims only.
    #[test]
    fn test_family_claim_surface_carries_no_capability_claims() {
        let source = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/models/unified_loader.rs"),
        )
        .expect("read unified_loader.rs");
        let forbidden = [
            "Recommended",
            "Best overall",
            "good for chat",
            "popular choice",
            "Efficient",
            "Specialized for coding",
            "fn description",
        ];
        let hits: Vec<&str> = forbidden
            .iter()
            .filter(|phrase| source.contains(*phrase))
            .copied()
            .collect();
        assert!(
            hits.is_empty(),
            "model family surfaces must state identity, not capability or quality \
             claims the local engine has not proven; found {hits:?} in unified_loader.rs"
        );
    }

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
        let group = ["crate:", ":{"].concat();
        let cli_path = ["cli", "::"].concat();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/models");
        let mut hits = Vec::new();
        collect_cli_imports(&root, &root, &needle, &group, &cli_path, &mut hits);
        let grouped = [group.as_str(), "cli", "::...}"].concat();
        assert!(
            hits.is_empty(),
            "models production code must not import {needle} or {grouped}; found: {hits:?}"
        );
    }

    fn collect_cli_imports(
        root: &Path,
        dir: &Path,
        needle: &str,
        group: &str,
        cli_path: &str,
        hits: &mut Vec<String>,
    ) {
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
                collect_cli_imports(root, &path, needle, group, cli_path, hits);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                hits.push(format!("failed to read {}", path.display()));
                continue;
            };
            let mut in_crate_group = false;
            for (index, line) in source.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                let grouped_cli = in_crate_group && crate_group_names_cli(trimmed, cli_path);
                if line.contains(needle) || grouped_cli {
                    let rel = path.strip_prefix(root).unwrap_or(&path);
                    hits.push(format!("{}:{}", rel.display(), index + 1));
                }
                if trimmed.contains(group) {
                    in_crate_group = true;
                    if crate_group_names_cli(trimmed, cli_path) {
                        let rel = path.strip_prefix(root).unwrap_or(&path);
                        let hit = format!("{}:{}", rel.display(), index + 1);
                        if !hits.contains(&hit) {
                            hits.push(hit);
                        }
                    }
                }
                if in_crate_group && trimmed.contains('}') {
                    in_crate_group = false;
                }
            }
        }
    }

    fn crate_group_names_cli(line: &str, cli_path: &str) -> bool {
        let trimmed = line.trim();
        trimmed.contains(cli_path)
            || trimmed == "cli"
            || trimmed.starts_with("cli,")
            || trimmed.starts_with("cli}")
    }
}

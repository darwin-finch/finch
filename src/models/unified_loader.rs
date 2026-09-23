//! Local chat-model identity and the llama.cpp GGUF loader.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::generator_new::TextGeneration;
use crate::config::ExecutionTarget;

/// The daemon's local chat inference engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum InferenceProvider {
    /// llama.cpp loading a caller-selected GGUF file.
    #[serde(rename = "llama_cpp")]
    #[default]
    LlamaCpp,
}

impl InferenceProvider {
    /// Human-readable engine name.
    pub fn name(self) -> &'static str {
        "llama.cpp (GGUF)"
    }
}

/// Configuration for loading a local chat model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelLoadConfig {
    /// Engine identity.
    #[serde(default)]
    pub provider: InferenceProvider,
    /// Prompt/adaptation family selected by the user.
    pub family: ModelFamily,
    /// Descriptive size used in profile labels.
    pub size: ModelSize,
    /// `auto` allows GPU offload and `cpu` disables it.
    #[serde(alias = "backend")]
    pub target: ExecutionTarget,
    /// Explicit local GGUF artifact.
    #[serde(default)]
    pub model_path: Option<PathBuf>,
}

impl ModelLoadConfig {
    /// Human-readable requested execution policy.
    pub fn requested_target_name(&self) -> String {
        self.target.name().to_string()
    }
}

/// Supported chat-model families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelFamily {
    /// Qwen 2.x family.
    Qwen2,
    /// Gemma 2 family.
    Gemma2,
    /// Llama 3 family.
    Llama3,
    /// Mistral family.
    Mistral,
    /// Phi family.
    Phi,
    /// DeepSeek family.
    DeepSeek,
}

/// Capabilities proven through Finch's local chat path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyEngineCapabilities {
    /// The served path emits incremental token callbacks.
    pub engine_streaming: bool,
    /// The served path proposes parsed tool markup.
    pub tool_markup_proposals: bool,
}

impl ModelFamily {
    /// Stable user-facing family name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Qwen2 => "Qwen 2.5",
            Self::Gemma2 => "Gemma 2",
            Self::Llama3 => "Llama 3",
            Self::Mistral => "Mistral",
            Self::Phi => "Phi",
            Self::DeepSeek => "DeepSeek",
        }
    }

    /// Capabilities proven by Finch's integration for this family.
    pub fn local_engine_capabilities(self) -> FamilyEngineCapabilities {
        match self {
            Self::Qwen2 => FamilyEngineCapabilities {
                engine_streaming: true,
                tool_markup_proposals: true,
            },
            _ => FamilyEngineCapabilities {
                engine_streaming: false,
                tool_markup_proposals: false,
            },
        }
    }
}

/// Descriptive model-size category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelSize {
    /// Small local model.
    Small,
    /// Medium local model.
    Medium,
    /// Large local model.
    Large,
    /// Extra-large local model.
    XLarge,
}

impl ModelSize {
    /// Family-specific display label retained for profile identity.
    pub fn to_size_string(self, family: ModelFamily) -> &'static str {
        match (family, self) {
            (ModelFamily::Qwen2, Self::Small) => "1.5B",
            (ModelFamily::Qwen2, Self::Medium) => "3B",
            (ModelFamily::Qwen2, Self::Large) => "7B",
            (ModelFamily::Qwen2, Self::XLarge) => "14B",
            (ModelFamily::Gemma2, Self::Small) => "2b",
            (ModelFamily::Gemma2, Self::Medium) => "9b",
            (ModelFamily::Gemma2, Self::Large | Self::XLarge) => "27b",
            (ModelFamily::Llama3, Self::Small) => "3B",
            (ModelFamily::Llama3, Self::Medium) => "8B",
            (ModelFamily::Llama3, Self::Large | Self::XLarge) => "70B",
            (ModelFamily::Mistral, Self::Small | Self::Medium) => "7B",
            (ModelFamily::Mistral, Self::Large | Self::XLarge) => "22B",
            (ModelFamily::Phi, Self::Small) => "2B",
            (ModelFamily::Phi, Self::Medium) => "3.8B",
            (ModelFamily::Phi, Self::Large | Self::XLarge) => "14B",
            (ModelFamily::DeepSeek, Self::Small) => "1.3B",
            (ModelFamily::DeepSeek, Self::Medium) => "6.7B",
            (ModelFamily::DeepSeek, Self::Large) => "16B",
            (ModelFamily::DeepSeek, Self::XLarge) => "33B",
        }
    }
}

fn gpu_offload_policy(target: ExecutionTarget) -> bool {
    match target {
        ExecutionTarget::Auto => true,
        ExecutionTarget::Cpu => false,
    }
}

/// Loads the daemon's configured GGUF chat model.
pub struct UnifiedModelLoader;

impl UnifiedModelLoader {
    /// Construct the stateless GGUF loader.
    pub fn new() -> Result<Self> {
        Ok(Self)
    }

    /// Load the configured GGUF through llama.cpp.
    pub fn load(&self, config: ModelLoadConfig) -> Result<Box<dyn TextGeneration>> {
        let path = config
            .model_path
            .as_ref()
            .context("llama.cpp requires backend.model_path pointing to a local .gguf file; run `finch setup`")?;
        if path.extension().and_then(|value| value.to_str()) != Some("gguf") {
            anyhow::bail!("backend.model_path must point to a .gguf chat model; run `finch setup`");
        }
        let model = super::loaders::llama_cpp::LlamaCppGenerator::load_with_offload(
            path,
            gpu_offload_policy(config.target),
            Some(config.family.name()),
        )?;
        Ok(Box::new(model))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llama_cpp_provider_round_trips() {
        let encoded = serde_json::to_string(&InferenceProvider::LlamaCpp).unwrap();
        assert_eq!(encoded, "\"llama_cpp\"");
        assert_eq!(
            serde_json::from_str::<InferenceProvider>(&encoded).unwrap(),
            InferenceProvider::LlamaCpp
        );
    }

    #[test]
    fn removed_chat_provider_names_are_rejected() {
        for removed in ["onnx", "candle"] {
            let encoded = format!("\"{removed}\"");
            assert!(
                serde_json::from_str::<InferenceProvider>(&encoded).is_err(),
                "removed provider {removed} must not deserialize"
            );
        }
    }

    #[test]
    fn target_policy_honors_explicit_cpu() {
        assert!(gpu_offload_policy(ExecutionTarget::Auto));
        assert!(!gpu_offload_policy(ExecutionTarget::Cpu));
    }
}

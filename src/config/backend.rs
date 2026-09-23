// Backend Configuration - Device selection and model management

use crate::models::{ModelFamily, ModelSize};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Execution target for inference (hardware where code runs)
///
/// The llama.cpp chat path accepts `Auto` (allow GPU offload) or `Cpu`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionTarget {
    /// Disable GPU offload.
    #[serde(rename = "cpu")]
    Cpu,

    /// Auto-detect best available target
    #[serde(rename = "auto")]
    Auto,
}

/// Legacy alias for compatibility during migration
#[deprecated(note = "Use ExecutionTarget instead")]
pub type BackendDevice = ExecutionTarget;

impl ExecutionTarget {
    /// Get short name for logging
    pub fn name(&self) -> &'static str {
        match self {
            ExecutionTarget::Cpu => "CPU",
            ExecutionTarget::Auto => "Auto",
        }
    }

    /// Get human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            ExecutionTarget::Cpu => "CPU only (GPU offload disabled)",
            ExecutionTarget::Auto => "Allow llama.cpp to offload to an available GPU",
        }
    }

    /// Check if this execution target is available on the current system
    ///
    /// Simplified legacy compatibility query; this is not llama.cpp capability detection.
    pub fn is_available(&self) -> bool {
        true
    }

    /// Get list of available execution targets on this system
    pub fn available_targets() -> Vec<ExecutionTarget> {
        vec![ExecutionTarget::Auto, ExecutionTarget::Cpu]
    }

    /// Legacy alias for available_targets()
    #[deprecated(note = "Use available_targets() instead")]
    pub fn available_devices() -> Vec<ExecutionTarget> {
        Self::available_targets()
    }

    /// Select best available execution target automatically
    pub fn auto_select() -> ExecutionTarget {
        ExecutionTarget::Cpu
    }
}

/// Backend configuration for model inference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    /// Enable local chat inference after the user configures a GGUF (default: false).
    #[serde(default = "default_backend_enabled")]
    pub enabled: bool,

    /// Chat inference provider. New configurations use llama.cpp.
    #[serde(default = "default_inference_provider")]
    pub inference_provider: crate::models::InferenceProvider,

    /// Selected llama.cpp offload policy.
    #[serde(alias = "device")] // Support old config field name
    pub execution_target: ExecutionTarget,

    /// Model family to use (Qwen2, Gemma2, etc.)
    #[serde(default = "default_model_family")]
    pub model_family: ModelFamily,

    /// Model size variant (Small, Medium, Large, XLarge)
    #[serde(default = "default_model_size")]
    pub model_size: ModelSize,

    /// Existing absolute GGUF path for a custom local chat model.
    pub model_path: Option<PathBuf>,

    /// Immutable Hugging Face GGUF selected and managed by Finch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_artifact: Option<crate::models::ManagedGgufArtifact>,

    /// Fallback execution target chain
    #[serde(
        default = "default_fallback_chain",
        deserialize_with = "deserialize_fallback_chain"
    )]
    pub fallback_chain: Vec<ExecutionTarget>,

    /// Legacy field alias for backward compatibility
    #[serde(skip)]
    #[deprecated(note = "Use execution_target instead")]
    pub device: Option<ExecutionTarget>,
}

fn default_backend_enabled() -> bool {
    false
}

fn default_inference_provider() -> crate::models::InferenceProvider {
    crate::models::InferenceProvider::LlamaCpp
}

fn default_model_family() -> ModelFamily {
    ModelFamily::Qwen2
}

fn default_model_size() -> ModelSize {
    ModelSize::Medium
}

fn default_fallback_chain() -> Vec<ExecutionTarget> {
    vec![ExecutionTarget::Auto, ExecutionTarget::Cpu]
}

/// Custom deserializer for fallback_chain that filters out deprecated/invalid entries (like "metal")
fn deserialize_fallback_chain<'de, D>(deserializer: D) -> Result<Vec<ExecutionTarget>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;

    // Deserialize as Vec<String> first to handle invalid variants gracefully
    let strings: Vec<String> = Vec::deserialize(deserializer)?;

    let mut targets = Vec::new();
    for s in strings {
        match s.as_str() {
            "cpu" => targets.push(ExecutionTarget::Cpu),
            "auto" => targets.push(ExecutionTarget::Auto),
            other => {
                tracing::warn!(
                    "Skipping unknown execution target '{}' in fallback_chain",
                    other
                );
            }
        }
    }

    // If no valid targets remain, use default
    if targets.is_empty() {
        Ok(default_fallback_chain())
    } else {
        Ok(targets)
    }
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            enabled: default_backend_enabled(),
            inference_provider: default_inference_provider(),
            execution_target: ExecutionTarget::Auto,
            model_family: default_model_family(),
            model_size: default_model_size(),
            model_path: None,
            managed_artifact: None,
            fallback_chain: default_fallback_chain(),
            #[allow(deprecated)]
            device: None,
        }
    }
}

impl BackendConfig {
    /// Describe the requested target policy without implying observed placement.
    pub fn requested_target_name(&self) -> String {
        self.execution_target.name().to_string()
    }

    /// Create new backend config with execution target
    pub fn with_target(target: ExecutionTarget) -> Self {
        Self {
            enabled: true,
            inference_provider: default_inference_provider(),
            execution_target: target,
            model_family: default_model_family(),
            model_size: default_model_size(),
            model_path: None,
            managed_artifact: None,
            fallback_chain: default_fallback_chain(),
            #[allow(deprecated)]
            device: None,
        }
    }

    /// Legacy alias for with_target()
    #[deprecated(note = "Use with_target() instead")]
    pub fn with_device(target: ExecutionTarget) -> Self {
        Self::with_target(target)
    }

    /// Create new backend config with model family and size
    pub fn with_model(target: ExecutionTarget, family: ModelFamily, size: ModelSize) -> Self {
        Self {
            enabled: true,
            inference_provider: default_inference_provider(),
            execution_target: target,
            model_family: family,
            model_size: size,
            model_path: None,
            managed_artifact: None,
            fallback_chain: default_fallback_chain(),
            #[allow(deprecated)]
            device: None,
        }
    }

    /// Get the effective execution target (resolve Auto to concrete target)
    pub fn effective_target(&self) -> ExecutionTarget {
        match self.execution_target {
            ExecutionTarget::Auto => ExecutionTarget::auto_select(),
            target => target,
        }
    }

    /// Legacy alias for effective_target()
    #[deprecated(note = "Use effective_target() instead")]
    pub fn effective_device(&self) -> ExecutionTarget {
        self.effective_target()
    }

    /// Get execution target (for backward compatibility, returns execution_target)
    #[deprecated(note = "Use execution_target field directly")]
    pub fn get_device(&self) -> ExecutionTarget {
        self.execution_target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_local_chat_is_disabled_until_a_user_configures_it() {
        let config = BackendConfig::default();
        assert!(
            !config.enabled && config.model_path.is_none(),
            "a fresh config must not attempt to load an unspecified local chat model: {config:?}"
        );
        assert_eq!(
            config.inference_provider,
            crate::models::InferenceProvider::LlamaCpp
        );
    }

    #[test]
    fn test_execution_target_cpu_always_available() {
        assert!(ExecutionTarget::Cpu.is_available());
        assert!(ExecutionTarget::Auto.is_available());
    }

    #[test]
    fn test_execution_target_cpu_name() {
        assert_eq!(ExecutionTarget::Cpu.name(), "CPU");
        assert_eq!(ExecutionTarget::Auto.name(), "Auto");
    }

    #[test]
    fn test_execution_target_cpu_description_non_empty() {
        assert!(!ExecutionTarget::Cpu.description().is_empty());
        assert!(!ExecutionTarget::Auto.description().is_empty());
    }

    #[test]
    fn test_execution_target_serde_roundtrip() {
        let original = ExecutionTarget::Cpu;
        let json = serde_json::to_string(&original).unwrap();
        let decoded: ExecutionTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn removed_execution_targets_are_rejected() {
        for removed in ["coreml", "cuda", "metal"] {
            let encoded = format!("\"{removed}\"");
            assert!(
                serde_json::from_str::<ExecutionTarget>(&encoded).is_err(),
                "removed target {removed} must not deserialize"
            );
        }
    }

    #[test]
    fn test_backend_config_default() {
        let config = BackendConfig::default();
        // CPU is always available; default should be something valid
        assert!(config.execution_target.is_available());
    }

    #[test]
    fn test_auto_select_returns_valid_target() {
        let target = ExecutionTarget::auto_select();
        assert!(target.is_available());
        // auto_select should never return Auto itself
        assert_ne!(target, ExecutionTarget::Auto);
    }

    #[test]
    fn test_effective_target_resolves_auto() {
        let mut config = BackendConfig::default();
        config.execution_target = ExecutionTarget::Auto;
        let effective = config.effective_target();
        // Should resolve to a concrete target, not Auto
        assert_ne!(effective, ExecutionTarget::Auto);
        assert!(effective.is_available());
    }
}

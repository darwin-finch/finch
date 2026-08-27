// Backend Configuration - Device selection and model management

use crate::models::unified_loader::{ModelFamily, ModelSize};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Execution target for inference (hardware where code runs)
///
/// All targets use ONNX Runtime as the inference provider.
/// The target determines which ONNX Runtime execution provider is used:
/// - CoreML: Uses the requested CoreML compute-unit policy on Apple platforms
/// - CPU: Uses CPU execution provider (universal fallback)
/// - CUDA: Uses CUDA execution provider for NVIDIA GPUs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionTarget {
    /// CoreML execution provider (macOS only, ONNX Runtime)
    #[cfg(target_os = "macos")]
    #[serde(rename = "coreml")]
    CoreML,

    /// NVIDIA CUDA GPU (Windows/Linux, fast)
    #[cfg(feature = "cuda")]
    #[serde(rename = "cuda")]
    Cuda,

    /// CPU execution provider (universal fallback)
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
            #[cfg(target_os = "macos")]
            ExecutionTarget::CoreML => "CoreML (Auto: ANE/GPU/CPU)",
            #[cfg(feature = "cuda")]
            ExecutionTarget::Cuda => "CUDA (GPU)",
            ExecutionTarget::Cpu => "CPU",
            ExecutionTarget::Auto => "Auto",
        }
    }

    /// Get human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            #[cfg(target_os = "macos")]
            ExecutionTarget::CoreML => "CoreML with automatic compute-unit selection (ANE/GPU/CPU)",
            #[cfg(feature = "cuda")]
            ExecutionTarget::Cuda => "NVIDIA GPU (CUDA) - Very fast on supported hardware",
            ExecutionTarget::Cpu => "CPU (Universal Fallback) - Slower than specialized hardware",
            ExecutionTarget::Auto => "Auto-detect best available target",
        }
    }

    /// Check if this execution target is available on the current system
    ///
    /// Simplified: assumes platform support = availability
    /// ONNX Runtime will handle actual device detection at runtime
    pub fn is_available(&self) -> bool {
        match self {
            #[cfg(target_os = "macos")]
            ExecutionTarget::CoreML => true, // Assume CoreML available on all macOS
            #[cfg(feature = "cuda")]
            ExecutionTarget::Cuda => true, // Assume CUDA available if compiled with feature
            ExecutionTarget::Cpu => true,  // Always available
            ExecutionTarget::Auto => true, // Always available
        }
    }

    /// Get list of available execution targets on this system
    pub fn available_targets() -> Vec<ExecutionTarget> {
        let mut targets = vec![];

        #[cfg(target_os = "macos")]
        {
            if ExecutionTarget::CoreML.is_available() {
                targets.push(ExecutionTarget::CoreML);
            }
        }

        #[cfg(feature = "cuda")]
        {
            if ExecutionTarget::Cuda.is_available() {
                targets.push(ExecutionTarget::Cuda);
            }
        }

        targets.push(ExecutionTarget::Cpu);
        targets
    }

    /// Legacy alias for available_targets()
    #[deprecated(note = "Use available_targets() instead")]
    pub fn available_devices() -> Vec<ExecutionTarget> {
        Self::available_targets()
    }

    /// Select best available execution target automatically
    pub fn auto_select() -> ExecutionTarget {
        #[cfg(target_os = "macos")]
        {
            if ExecutionTarget::CoreML.is_available() {
                return ExecutionTarget::CoreML;
            }
        }

        #[cfg(feature = "cuda")]
        {
            if ExecutionTarget::Cuda.is_available() {
                return ExecutionTarget::Cuda;
            }
        }

        ExecutionTarget::Cpu
    }
}

/// Compute units Finch asks CoreML to consider.
///
/// This is a requested policy, not evidence that CoreML assigned any operation
/// to a particular device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CoreMlComputeUnits {
    /// Allow CoreML to choose among all compatible compute units.
    #[default]
    All,
    /// Restrict CoreML to CPU and compatible Apple Neural Engine devices.
    CpuAndNeuralEngine,
    /// Restrict CoreML to CPU and compatible GPU devices.
    CpuAndGpu,
    /// Restrict CoreML to CPU.
    CpuOnly,
}

impl CoreMlComputeUnits {
    /// Human-readable requested policy. This does not describe observed placement.
    pub fn name(self) -> &'static str {
        match self {
            Self::All => "Auto: ANE/GPU/CPU",
            Self::CpuAndNeuralEngine => "CPU + ANE",
            Self::CpuAndGpu => "CPU + GPU",
            Self::CpuOnly => "CPU only",
        }
    }
}

/// CoreML execution-provider options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CoreMlConfig {
    /// Requested CoreML compute units.
    pub compute_units: CoreMlComputeUnits,
    /// Ask CoreML to log its compute plan for placement diagnostics.
    pub profile_compute_plan: bool,
    /// Allow CoreML to take nodes inside Loop, Scan, and If subgraphs.
    pub enable_subgraphs: bool,
}

/// Backend configuration for model inference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    /// Enable local model inference (default: true)
    /// Set to false for proxy-only mode (no local model, teacher APIs only)
    #[serde(default = "default_backend_enabled")]
    pub enabled: bool,

    /// Inference provider (ONNX Runtime or Candle)
    #[serde(default = "default_inference_provider")]
    pub inference_provider: crate::models::unified_loader::InferenceProvider,

    /// Selected execution target (where code runs: CoreML/CPU/CUDA)
    #[serde(alias = "device")] // Support old config field name
    pub execution_target: ExecutionTarget,

    /// Requested CoreML policy and opt-in diagnostics.
    #[serde(default)]
    pub coreml: CoreMlConfig,

    /// Model family to use (Qwen2, Gemma2, etc.)
    #[serde(default = "default_model_family")]
    pub model_family: ModelFamily,

    /// Model size variant (Small, Medium, Large, XLarge)
    #[serde(default = "default_model_size")]
    pub model_size: ModelSize,

    /// Model repository (optional override)
    /// If not specified, automatically selected from compatibility matrix
    pub model_repo: Option<String>,

    /// Path to downloaded model
    pub model_path: Option<PathBuf>,

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
    true
}

fn default_inference_provider() -> crate::models::unified_loader::InferenceProvider {
    crate::models::unified_loader::InferenceProvider::Onnx // ONNX Runtime is the default
}

fn default_model_family() -> ModelFamily {
    ModelFamily::Qwen2
}

fn default_model_size() -> ModelSize {
    ModelSize::Medium
}

fn default_fallback_chain() -> Vec<ExecutionTarget> {
    #[cfg(target_os = "macos")]
    return vec![ExecutionTarget::CoreML, ExecutionTarget::Cpu];

    #[cfg(all(not(target_os = "macos"), feature = "cuda"))]
    return vec![ExecutionTarget::Cuda, ExecutionTarget::Cpu];

    #[cfg(all(not(target_os = "macos"), not(feature = "cuda")))]
    return vec![ExecutionTarget::Cpu];
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
            "coreml" => {
                #[cfg(target_os = "macos")]
                targets.push(ExecutionTarget::CoreML);
            }
            "cpu" => targets.push(ExecutionTarget::Cpu),
            "cuda" => {
                #[cfg(feature = "cuda")]
                targets.push(ExecutionTarget::Cuda);
            }
            "auto" => targets.push(ExecutionTarget::Auto),
            "metal" => {
                // Silently skip deprecated "metal" variant
                tracing::warn!("Skipping deprecated 'metal' execution target in config");
            }
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
            coreml: CoreMlConfig::default(),
            model_family: default_model_family(),
            model_size: default_model_size(),
            model_repo: None,
            model_path: None,
            fallback_chain: default_fallback_chain(),
            #[allow(deprecated)]
            device: None,
        }
    }
}

impl BackendConfig {
    /// Describe the requested target policy without implying observed placement.
    pub fn requested_target_name(&self) -> String {
        #[cfg(target_os = "macos")]
        if self.execution_target == ExecutionTarget::CoreML {
            return format!("CoreML ({})", self.coreml.compute_units.name());
        }

        self.execution_target.name().to_string()
    }

    /// Create new backend config with execution target
    pub fn with_target(target: ExecutionTarget) -> Self {
        Self {
            enabled: default_backend_enabled(),
            inference_provider: default_inference_provider(),
            execution_target: target,
            coreml: CoreMlConfig::default(),
            model_family: default_model_family(),
            model_size: default_model_size(),
            model_repo: None,
            model_path: None,
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
            enabled: default_backend_enabled(),
            inference_provider: default_inference_provider(),
            execution_target: target,
            coreml: CoreMlConfig::default(),
            model_family: family,
            model_size: size,
            model_repo: None,
            model_path: None,
            fallback_chain: default_fallback_chain(),
            #[allow(deprecated)]
            device: None,
        }
    }

    /// Get the model repository for the selected target and model size
    ///
    /// Uses compatibility matrix to resolve repository automatically
    pub fn get_model_repo(&self, _model_size: &str) -> String {
        if let Some(repo) = &self.model_repo {
            return repo.clone();
        }

        // Use compatibility matrix to get repository
        crate::models::compatibility::get_repository(
            self.inference_provider,
            self.model_family,
            self.model_size,
        )
        .unwrap_or_else(|| {
            // Fallback for compatibility
            "onnx-community/Qwen2.5-1.5B-Instruct".to_string()
        })
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

    #[cfg(target_os = "macos")]
    #[test]
    fn test_coreml_auto_label_is_truthful() {
        let name = ExecutionTarget::CoreML.name();
        let description = ExecutionTarget::CoreML.description();
        assert!(name.contains("Auto"));
        assert!(!name.contains("ANE only"));
        assert!(!description.to_ascii_lowercase().contains("fastest"));

        let config = BackendConfig::with_target(ExecutionTarget::CoreML);
        assert_eq!(config.requested_target_name(), "CoreML (Auto: ANE/GPU/CPU)");

        for (policy, expected) in [
            (CoreMlComputeUnits::All, "Auto: ANE/GPU/CPU"),
            (CoreMlComputeUnits::CpuAndNeuralEngine, "CPU + ANE"),
            (CoreMlComputeUnits::CpuAndGpu, "CPU + GPU"),
            (CoreMlComputeUnits::CpuOnly, "CPU only"),
        ] {
            let mut config = BackendConfig::with_target(ExecutionTarget::CoreML);
            config.coreml.compute_units = policy;
            assert_eq!(
                config.requested_target_name(),
                format!("CoreML ({expected})")
            );
        }
    }

    #[test]
    fn test_live_configuration_guide_does_not_claim_auto_is_ane_only_or_fastest() {
        let guide = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/docs/CONFIGURATION.md"
        ));
        assert!(!guide.contains("Use Apple Neural Engine"));
        assert!(!guide.contains("ANE unavailable"));
        assert!(!guide.contains("CoreML (ANE)"));
        assert!(!guide.contains("Fastest on Mac"));
        assert!(guide.contains("does not prove where"));
    }

    #[test]
    fn test_coreml_compute_units_serde_round_trip() {
        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct Wrapper {
            compute_units: CoreMlComputeUnits,
        }

        for policy in [
            CoreMlComputeUnits::All,
            CoreMlComputeUnits::CpuAndNeuralEngine,
            CoreMlComputeUnits::CpuAndGpu,
            CoreMlComputeUnits::CpuOnly,
        ] {
            let original = Wrapper {
                compute_units: policy,
            };
            let encoded = toml::to_string(&original).unwrap();
            assert_eq!(toml::from_str::<Wrapper>(&encoded).unwrap(), original);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_legacy_coreml_target_defaults_to_all_compute_units() {
        let legacy = r#"
            execution_target = "coreml"
            model_family = "Qwen2"
            model_size = "Medium"
        "#;
        let decoded: BackendConfig = toml::from_str(legacy).unwrap();
        assert_eq!(decoded.coreml, CoreMlConfig::default());
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

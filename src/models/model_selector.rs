// Model Selector - RAM-based Qwen variant selection
// Automatically selects appropriate model size based on available system memory
// Works on all platforms (macOS, Linux, Windows) via sysinfo crate.

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Minimum RAM required to run any local model (GB).
/// Below this threshold, finch runs in cloud-only mode.
pub const MIN_LOCAL_MODEL_RAM_GB: usize = 3;

/// Qwen model size variants — ordered from smallest to largest
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QwenSize {
    /// Qwen-2.5-0.5B-Instruct (~1GB RAM) — old laptops, constrained devices
    Qwen500M,
    /// Qwen-2.5-1.5B-Instruct (~3GB RAM) — 8GB machines
    Qwen1_5B,
    /// Qwen-2.5-3B-Instruct (~6GB RAM) — 16GB machines
    Qwen3B,
    /// Qwen-2.5-7B-Instruct (~14GB RAM) — 32GB machines
    Qwen7B,
    /// Qwen-2.5-14B-Instruct (~28GB RAM) — 64GB+ machines
    Qwen14B,
}

impl QwenSize {
    /// Get HuggingFace model ID for this variant
    pub fn model_id(&self) -> &'static str {
        match self {
            QwenSize::Qwen500M => "Qwen/Qwen2.5-0.5B-Instruct",
            QwenSize::Qwen1_5B => "Qwen/Qwen2.5-1.5B-Instruct",
            QwenSize::Qwen3B => "Qwen/Qwen2.5-3B-Instruct",
            QwenSize::Qwen7B => "Qwen/Qwen2.5-7B-Instruct",
            QwenSize::Qwen14B => "Qwen/Qwen2.5-14B-Instruct",
        }
    }

    /// Get ONNX community repository ID
    pub fn onnx_repo_id(&self) -> &'static str {
        match self {
            QwenSize::Qwen500M => "onnx-community/Qwen2.5-0.5B-Instruct-ONNX",
            QwenSize::Qwen1_5B => "onnx-community/Qwen2.5-1.5B-Instruct-ONNX",
            QwenSize::Qwen3B => "onnx-community/Qwen2.5-3B-Instruct-ONNX",
            QwenSize::Qwen7B => "onnx-community/Qwen2.5-7B-Instruct-ONNX",
            QwenSize::Qwen14B => "onnx-community/Qwen2.5-14B-Instruct-ONNX",
        }
    }

    /// Get approximate RAM requirement in GB
    pub fn ram_requirement_gb(&self) -> usize {
        match self {
            QwenSize::Qwen500M => 1,
            QwenSize::Qwen1_5B => 3,
            QwenSize::Qwen3B => 6,
            QwenSize::Qwen7B => 14,
            QwenSize::Qwen14B => 28,
        }
    }

    /// Get human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            QwenSize::Qwen500M => "Qwen 0.5B (minimal — old laptops, 4GB RAM)",
            QwenSize::Qwen1_5B => "Qwen 1.5B (balanced — 8GB RAM)",
            QwenSize::Qwen3B => "Qwen 3B (capable — 16GB RAM)",
            QwenSize::Qwen7B => "Qwen 7B (powerful — 32GB RAM)",
            QwenSize::Qwen14B => "Qwen 14B (maximum — 64GB+ RAM)",
        }
    }

    /// Get approximate download size in GB
    pub fn download_size_gb(&self) -> f64 {
        match self {
            QwenSize::Qwen500M => 0.5,
            QwenSize::Qwen1_5B => 1.5,
            QwenSize::Qwen3B => 3.0,
            QwenSize::Qwen7B => 7.0,
            QwenSize::Qwen14B => 14.0,
        }
    }
}

/// Result of model selection — either a specific model or cloud-only mode
#[derive(Debug, Clone)]
pub enum ModelSelection {
    /// Run a local model
    Local(QwenSize),
    /// RAM too low for any local model — use teacher APIs only
    CloudOnly { ram_gb: usize },
}

impl ModelSelection {
    pub fn is_cloud_only(&self) -> bool {
        matches!(self, ModelSelection::CloudOnly { .. })
    }

    pub fn model(&self) -> Option<QwenSize> {
        match self {
            ModelSelection::Local(size) => Some(*size),
            ModelSelection::CloudOnly { .. } => None,
        }
    }
}

/// Model selection based on system resources
pub struct ModelSelector;

impl ModelSelector {
    /// Select appropriate model based on available system RAM.
    ///
    /// Thresholds (conservative — leaves headroom for OS + other processes):
    /// - <3GB  → CloudOnly (no local model possible)
    /// - 3-6GB → Qwen-0.5B (~1GB model)
    /// - 6-12GB → Qwen-1.5B (~3GB model)
    /// - 12-24GB → Qwen-3B  (~6GB model)
    /// - 24-48GB → Qwen-7B  (~14GB model)
    /// - 48GB+   → Qwen-14B (~28GB model)
    pub fn select_for_system() -> Result<ModelSelection> {
        let ram_gb = Self::get_total_ram_gb();
        tracing::info!("System RAM: {}GB", ram_gb);

        let selection = match ram_gb {
            ram if ram < MIN_LOCAL_MODEL_RAM_GB => {
                tracing::warn!(
                    "Only {}GB RAM — running in cloud-only mode (teacher API)",
                    ram
                );
                ModelSelection::CloudOnly { ram_gb: ram }
            }
            ram if ram < 6 => {
                tracing::info!("{}GB RAM → Qwen-0.5B", ram);
                ModelSelection::Local(QwenSize::Qwen500M)
            }
            ram if ram < 12 => {
                tracing::info!("{}GB RAM → Qwen-1.5B", ram);
                ModelSelection::Local(QwenSize::Qwen1_5B)
            }
            ram if ram < 24 => {
                tracing::info!("{}GB RAM → Qwen-3B", ram);
                ModelSelection::Local(QwenSize::Qwen3B)
            }
            ram if ram < 48 => {
                tracing::info!("{}GB RAM → Qwen-7B", ram);
                ModelSelection::Local(QwenSize::Qwen7B)
            }
            ram => {
                tracing::info!("{}GB RAM → Qwen-14B", ram);
                ModelSelection::Local(QwenSize::Qwen14B)
            }
        };

        Ok(selection)
    }

    /// Backwards-compat wrapper — returns the Qwen variant or defaults to 1.5B in cloud-only
    pub fn select_model_for_system() -> Result<QwenSize> {
        match Self::select_for_system()? {
            ModelSelection::Local(size) => Ok(size),
            ModelSelection::CloudOnly { .. } => {
                tracing::info!("Cloud-only mode, using 1.5B as nominal model size");
                Ok(QwenSize::Qwen1_5B)
            }
        }
    }

    /// Select model with manual override
    pub fn select_model_with_override(override_size: Option<QwenSize>) -> Result<QwenSize> {
        if let Some(size) = override_size {
            let ram_gb = Self::get_total_ram_gb();
            let required = size.ram_requirement_gb();

            if ram_gb < required {
                tracing::warn!(
                    "Manual override: {} requires {}GB but system has {}GB — may OOM",
                    size.description(),
                    required,
                    ram_gb
                );
            }

            Ok(size)
        } else {
            Self::select_model_for_system()
        }
    }

    /// Get total system RAM in GB.
    /// Uses sysinfo crate — works on macOS, Linux, and Windows.
    pub fn get_total_ram_gb() -> usize {
        use sysinfo::System;
        let mut sys = System::new();
        sys.refresh_memory();
        let bytes = sys.total_memory(); // bytes
        let gb = bytes / (1024 * 1024 * 1024);
        if gb == 0 {
            // sysinfo couldn't determine RAM — conservative fallback
            tracing::warn!("Could not detect system RAM, assuming 8GB");
            8
        } else {
            gb as usize
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qwen_size_model_ids() {
        assert_eq!(QwenSize::Qwen500M.model_id(), "Qwen/Qwen2.5-0.5B-Instruct");
        assert_eq!(QwenSize::Qwen1_5B.model_id(), "Qwen/Qwen2.5-1.5B-Instruct");
        assert_eq!(QwenSize::Qwen3B.model_id(), "Qwen/Qwen2.5-3B-Instruct");
        assert_eq!(QwenSize::Qwen7B.model_id(), "Qwen/Qwen2.5-7B-Instruct");
        assert_eq!(QwenSize::Qwen14B.model_id(), "Qwen/Qwen2.5-14B-Instruct");
    }

    #[test]
    fn test_onnx_repo_ids() {
        assert!(QwenSize::Qwen500M.onnx_repo_id().contains("0.5B"));
        assert!(QwenSize::Qwen1_5B.onnx_repo_id().contains("1.5B"));
        assert!(QwenSize::Qwen14B.onnx_repo_id().contains("14B"));
    }

    #[test]
    fn test_ram_requirements() {
        assert!(QwenSize::Qwen500M.ram_requirement_gb() < QwenSize::Qwen1_5B.ram_requirement_gb());
        assert!(QwenSize::Qwen1_5B.ram_requirement_gb() < QwenSize::Qwen3B.ram_requirement_gb());
        assert!(QwenSize::Qwen3B.ram_requirement_gb() < QwenSize::Qwen7B.ram_requirement_gb());
        assert!(QwenSize::Qwen7B.ram_requirement_gb() < QwenSize::Qwen14B.ram_requirement_gb());
    }

    #[test]
    fn test_cloud_only_threshold() {
        // Machines with less than MIN_LOCAL_MODEL_RAM_GB should be cloud-only
        assert!(MIN_LOCAL_MODEL_RAM_GB <= QwenSize::Qwen500M.ram_requirement_gb() + 2);
    }

    #[test]
    fn test_model_selection_from_low_ram() {
        // Low RAM → cloud-only
        let tiny_ram = 2; // GB
        let selection = match tiny_ram {
            ram if ram < MIN_LOCAL_MODEL_RAM_GB => ModelSelection::CloudOnly { ram_gb: ram },
            _ => ModelSelection::Local(QwenSize::Qwen500M),
        };
        assert!(selection.is_cloud_only());
        assert!(selection.model().is_none());
    }

    #[test]
    fn test_model_selection_from_4gb_ram() {
        // 4-6GB → Qwen 0.5B
        let ram = 4;
        let model = match ram {
            r if r < MIN_LOCAL_MODEL_RAM_GB => None,
            r if r < 6 => Some(QwenSize::Qwen500M),
            _ => Some(QwenSize::Qwen1_5B),
        };
        assert_eq!(model, Some(QwenSize::Qwen500M));
    }

    #[test]
    fn test_get_ram_returns_nonzero() {
        let ram = ModelSelector::get_total_ram_gb();
        assert!(ram >= 1, "RAM detection should return at least 1GB");
    }

    #[test]
    fn test_select_model() {
        let result = ModelSelector::select_model_for_system();
        assert!(result.is_ok());
    }

    #[test]
    fn test_select_for_system() {
        let result = ModelSelector::select_for_system();
        assert!(result.is_ok());
    }

    #[test]
    fn test_manual_override() {
        let model = ModelSelector::select_model_with_override(Some(QwenSize::Qwen1_5B));
        assert!(model.is_ok());
        assert_eq!(model.unwrap(), QwenSize::Qwen1_5B);
    }

    #[test]
    fn test_manual_override_tiny() {
        let model = ModelSelector::select_model_with_override(Some(QwenSize::Qwen500M));
        assert!(model.is_ok());
        assert_eq!(model.unwrap(), QwenSize::Qwen500M);
    }
}

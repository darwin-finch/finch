// Model Selector - RAM-based Qwen variant selection
// Automatically selects appropriate model size based on available system memory

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Qwen model size variants optimized for different RAM configurations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QwenSize {
    /// Qwen-2.5-1.5B-Instruct (~3GB RAM, for 8GB Macs)
    Qwen1_5B,
    /// Qwen-2.5-3B-Instruct (~6GB RAM, for 16GB Macs)
    Qwen3B,
    /// Qwen-2.5-7B-Instruct (~14GB RAM, for 32GB Macs)
    Qwen7B,
    /// Qwen-2.5-14B-Instruct (~28GB RAM, for 64GB+ Macs)
    Qwen14B,
}

impl QwenSize {
    /// Get HuggingFace model ID for this variant
    pub fn model_id(&self) -> &'static str {
        match self {
            QwenSize::Qwen1_5B => "Qwen/Qwen2.5-1.5B-Instruct",
            QwenSize::Qwen3B => "Qwen/Qwen2.5-3B-Instruct",
            QwenSize::Qwen7B => "Qwen/Qwen2.5-7B-Instruct",
            QwenSize::Qwen14B => "Qwen/Qwen2.5-14B-Instruct",
        }
    }

    /// Get approximate RAM requirement in GB
    pub fn ram_requirement_gb(&self) -> usize {
        match self {
            QwenSize::Qwen1_5B => 3,
            QwenSize::Qwen3B => 6,
            QwenSize::Qwen7B => 14,
            QwenSize::Qwen14B => 28,
        }
    }

    /// Get human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            QwenSize::Qwen1_5B => "Qwen 1.5B (optimized for 8GB Macs)",
            QwenSize::Qwen3B => "Qwen 3B (optimized for 16GB Macs)",
            QwenSize::Qwen7B => "Qwen 7B (optimized for 32GB Macs)",
            QwenSize::Qwen14B => "Qwen 14B (optimized for 64GB+ Macs)",
        }
    }

    /// Get download size estimate in GB
    pub fn download_size_gb(&self) -> f64 {
        match self {
            QwenSize::Qwen1_5B => 1.5,
            QwenSize::Qwen3B => 3.0,
            QwenSize::Qwen7B => 7.0,
            QwenSize::Qwen14B => 14.0,
        }
    }
}

/// Model selection based on system resources
pub struct ModelSelector;

impl ModelSelector {
    /// Select appropriate Qwen model based on available system RAM
    ///
    /// Uses conservative thresholds to ensure model fits in memory:
    /// - 8GB Mac → Qwen-1.5B (leaves 5GB for OS)
    /// - 16GB Mac → Qwen-3B (leaves 10GB for OS)
    /// - 32GB Mac → Qwen-7B (leaves 18GB for OS)
    /// - 64GB+ Mac → Qwen-14B (leaves 36GB+ for OS)
    pub fn select_model_for_system() -> Result<QwenSize> {
        let ram_gb = Self::get_available_ram_gb()?;

        tracing::info!("System RAM: {}GB", ram_gb);

        let model = match ram_gb {
            ram if ram < 12 => {
                tracing::info!("Selected Qwen-1.5B for {}GB RAM", ram);
                QwenSize::Qwen1_5B
            }
            ram if ram < 24 => {
                tracing::info!("Selected Qwen-3B for {}GB RAM", ram);
                QwenSize::Qwen3B
            }
            ram if ram < 48 => {
                tracing::info!("Selected Qwen-7B for {}GB RAM", ram);
                QwenSize::Qwen7B
            }
            ram => {
                tracing::info!("Selected Qwen-14B for {}GB RAM", ram);
                QwenSize::Qwen14B
            }
        };

        Ok(model)
    }

    /// Select model with manual override
    pub fn select_model_with_override(override_size: Option<QwenSize>) -> Result<QwenSize> {
        if let Some(size) = override_size {
            let ram_gb = Self::get_available_ram_gb()?;
            let required = size.ram_requirement_gb();

            if ram_gb < required {
                tracing::warn!(
                    "Manual override: {} requires {}GB but system has {}GB - may run out of memory",
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

    /// Get available system RAM in GB
    #[cfg(target_os = "macos")]
    fn get_available_ram_gb() -> Result<usize> {
        use std::process::Command;

        let output = Command::new("sysctl")
            .args(&["-n", "hw.memsize"])
            .output()
            .context("Failed to execute sysctl to get RAM size")?;

        let memsize_str =
            String::from_utf8(output.stdout).context("Failed to parse sysctl output")?;

        let memsize_bytes: u64 = memsize_str
            .trim()
            .parse()
            .context("Failed to parse memory size as number")?;

        // Convert bytes to GB (rounded down)
        let ram_gb = (memsize_bytes / (1024 * 1024 * 1024)) as usize;

        Ok(ram_gb)
    }

    #[cfg(not(target_os = "macos"))]
    fn get_available_ram_gb() -> Result<usize> {
        // Default to conservative estimate for non-macOS systems
        tracing::warn!("RAM detection not implemented for this platform, defaulting to 16GB");
        Ok(16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qwen_size_model_ids() {
        assert_eq!(QwenSize::Qwen1_5B.model_id(), "Qwen/Qwen2.5-1.5B-Instruct");
        assert_eq!(QwenSize::Qwen3B.model_id(), "Qwen/Qwen2.5-3B-Instruct");
        assert_eq!(QwenSize::Qwen7B.model_id(), "Qwen/Qwen2.5-7B-Instruct");
        assert_eq!(QwenSize::Qwen14B.model_id(), "Qwen/Qwen2.5-14B-Instruct");
    }

    #[test]
    fn test_ram_requirements() {
        assert_eq!(QwenSize::Qwen1_5B.ram_requirement_gb(), 3);
        assert_eq!(QwenSize::Qwen3B.ram_requirement_gb(), 6);
        assert_eq!(QwenSize::Qwen7B.ram_requirement_gb(), 14);
        assert_eq!(QwenSize::Qwen14B.ram_requirement_gb(), 28);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_get_ram() {
        let ram = ModelSelector::get_available_ram_gb();
        assert!(ram.is_ok());
        let ram_gb = ram.unwrap();
        assert!(ram_gb >= 4, "System should have at least 4GB RAM");
    }

    #[test]
    fn test_select_model() {
        // Should select some model without error
        let model = ModelSelector::select_model_for_system();
        assert!(model.is_ok());
    }

    #[test]
    fn test_manual_override() {
        let model = ModelSelector::select_model_with_override(Some(QwenSize::Qwen1_5B));
        assert!(model.is_ok());
        assert_eq!(model.unwrap(), QwenSize::Qwen1_5B);
    }
}

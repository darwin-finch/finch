// Unified provider entry — covers both cloud and local inference backends.

use crate::config::backend::ExecutionTarget;
use crate::models::unified_loader::{InferenceProvider, ModelFamily, ModelSize};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_true() -> bool {
    true
}

fn default_inference_provider() -> InferenceProvider {
    InferenceProvider::Onnx
}

fn default_execution_target() -> ExecutionTarget {
    ExecutionTarget::Auto
}

fn default_model_family() -> ModelFamily {
    ModelFamily::Qwen2
}

fn default_model_size() -> ModelSize {
    ModelSize::Medium
}

/// A single provider entry — either a cloud API or a local inference backend.
///
/// Serializes with a `type` tag, e.g.:
/// ```toml
/// [[providers]]
/// type = "grok"
/// api_key = "xai-..."
///
/// [[providers]]
/// type = "local"
/// inference_provider = "onnx"
/// execution_target = "coreml"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ProviderEntry {
    Claude {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Openai {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Grok {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Gemini {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Mistral {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Groq {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Local {
        #[serde(default = "default_inference_provider")]
        inference_provider: InferenceProvider,
        #[serde(default = "default_execution_target")]
        execution_target: ExecutionTarget,
        #[serde(default = "default_model_family")]
        model_family: ModelFamily,
        #[serde(default = "default_model_size")]
        model_size: ModelSize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_repo: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_path: Option<PathBuf>,
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

impl ProviderEntry {
    /// Human-readable name for UI display.
    pub fn display_name(&self) -> &str {
        match self {
            Self::Claude { name, .. } => name.as_deref().unwrap_or("Claude"),
            Self::Openai { name, .. } => name.as_deref().unwrap_or("OpenAI"),
            Self::Grok { name, .. } => name.as_deref().unwrap_or("Grok"),
            Self::Gemini { name, .. } => name.as_deref().unwrap_or("Gemini"),
            Self::Mistral { name, .. } => name.as_deref().unwrap_or("Mistral"),
            Self::Groq { name, .. } => name.as_deref().unwrap_or("Groq"),
            Self::Local { name, .. } => name.as_deref().unwrap_or("Local"),
        }
    }

    /// Short provider-type tag (e.g. "claude", "grok", "local").
    pub fn provider_type(&self) -> &'static str {
        match self {
            Self::Claude { .. } => "claude",
            Self::Openai { .. } => "openai",
            Self::Grok { .. } => "grok",
            Self::Gemini { .. } => "gemini",
            Self::Mistral { .. } => "mistral",
            Self::Groq { .. } => "groq",
            Self::Local { .. } => "local",
        }
    }

    /// True for `Local` variants.
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

    /// API key for cloud variants; `None` for Local.
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::Claude { api_key, .. } => Some(api_key.as_str()),
            Self::Openai { api_key, .. } => Some(api_key.as_str()),
            Self::Grok { api_key, .. } => Some(api_key.as_str()),
            Self::Gemini { api_key, .. } => Some(api_key.as_str()),
            Self::Mistral { api_key, .. } => Some(api_key.as_str()),
            Self::Groq { api_key, .. } => Some(api_key.as_str()),
            Self::Local { .. } => None,
        }
    }

    /// Optional model override (cloud providers only).
    pub fn model(&self) -> Option<&str> {
        match self {
            Self::Claude { model, .. } => model.as_deref(),
            Self::Openai { model, .. } => model.as_deref(),
            Self::Grok { model, .. } => model.as_deref(),
            Self::Gemini { model, .. } => model.as_deref(),
            Self::Mistral { model, .. } => model.as_deref(),
            Self::Groq { model, .. } => model.as_deref(),
            Self::Local { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloud_serde_roundtrip() {
        let entry = ProviderEntry::Claude {
            api_key: "sk-ant-test".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
            base_url: None,
            name: Some("Claude Primary".to_string()),
        };
        let toml = toml::to_string(&entry).unwrap();
        let decoded: ProviderEntry = toml::from_str(&toml).unwrap();
        assert_eq!(entry, decoded);
    }

    #[test]
    fn test_local_serde_roundtrip() {
        let entry = ProviderEntry::Local {
            inference_provider: InferenceProvider::Onnx,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
            enabled: true,
            name: Some("Local Qwen 3B".to_string()),
        };
        let toml = toml::to_string(&entry).unwrap();
        let decoded: ProviderEntry = toml::from_str(&toml).unwrap();
        assert_eq!(entry, decoded);
    }

    #[test]
    fn test_grok_serde_roundtrip() {
        let entry = ProviderEntry::Grok {
            api_key: "xai-test".to_string(),
            model: Some("grok-code-fast-1".to_string()),
            name: None,
        };
        let toml = toml::to_string(&entry).unwrap();
        let decoded: ProviderEntry = toml::from_str(&toml).unwrap();
        assert_eq!(entry, decoded);
    }

    #[test]
    fn test_display_name_fallback() {
        let entry = ProviderEntry::Claude {
            api_key: "key".to_string(),
            model: None,
            base_url: None,
            name: None,
        };
        assert_eq!(entry.display_name(), "Claude");
    }

    #[test]
    fn test_display_name_custom() {
        let entry = ProviderEntry::Grok {
            api_key: "key".to_string(),
            model: None,
            name: Some("Grok (Primary)".to_string()),
        };
        assert_eq!(entry.display_name(), "Grok (Primary)");
    }

    #[test]
    fn test_is_local() {
        let local = ProviderEntry::Local {
            inference_provider: InferenceProvider::Onnx,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
            enabled: true,
            name: None,
        };
        assert!(local.is_local());

        let cloud = ProviderEntry::Claude {
            api_key: "key".to_string(),
            model: None,
            base_url: None,
            name: None,
        };
        assert!(!cloud.is_local());
    }

    #[test]
    fn test_api_key_none_for_local() {
        let local = ProviderEntry::Local {
            inference_provider: InferenceProvider::Onnx,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
            enabled: true,
            name: None,
        };
        assert!(local.api_key().is_none());
    }

    #[test]
    fn test_provider_type_tags() {
        assert_eq!(
            ProviderEntry::Claude {
                api_key: "k".to_string(),
                model: None,
                base_url: None,
                name: None
            }
            .provider_type(),
            "claude"
        );
        assert_eq!(
            ProviderEntry::Grok {
                api_key: "k".to_string(),
                model: None,
                name: None
            }
            .provider_type(),
            "grok"
        );
        assert_eq!(
            ProviderEntry::Local {
                inference_provider: InferenceProvider::Onnx,
                execution_target: ExecutionTarget::Auto,
                model_family: ModelFamily::Qwen2,
                model_size: ModelSize::Medium,
                model_repo: None,
                model_path: None,
                enabled: true,
                name: None,
            }
            .provider_type(),
            "local"
        );
    }

    #[test]
    fn test_array_of_providers_toml() {
        let providers = vec![
            ProviderEntry::Grok {
                api_key: "xai-test".to_string(),
                model: Some("grok-code-fast-1".to_string()),
                name: Some("Grok (Primary)".to_string()),
            },
            ProviderEntry::Claude {
                api_key: "sk-ant-test".to_string(),
                model: None,
                base_url: None,
                name: None,
            },
            ProviderEntry::Local {
                inference_provider: InferenceProvider::Onnx,
                execution_target: ExecutionTarget::Auto,
                model_family: ModelFamily::Qwen2,
                model_size: ModelSize::Medium,
                model_repo: None,
                model_path: None,
                enabled: true,
                name: None,
            },
        ];

        // Serialize as TOML array
        #[derive(Serialize, Deserialize)]
        struct Wrapper {
            providers: Vec<ProviderEntry>,
        }
        let w = Wrapper {
            providers: providers.clone(),
        };
        let toml_str = toml::to_string(&w).unwrap();
        let decoded: Wrapper = toml::from_str(&toml_str).unwrap();
        assert_eq!(decoded.providers.len(), 3);
        assert_eq!(decoded.providers[0].provider_type(), "grok");
        assert_eq!(decoded.providers[1].provider_type(), "claude");
        assert_eq!(decoded.providers[2].provider_type(), "local");
    }
}

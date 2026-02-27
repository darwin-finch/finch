// Provider factory
//
// Creates LLM providers based on teacher/provider configuration

use anyhow::{bail, Context, Result};

use super::claude::ClaudeProvider;
use super::gemini::GeminiProvider;
use super::openai::OpenAIProvider;
use super::LlmProvider;
use crate::config::{ProviderEntry, TeacherEntry};

// ---------------------------------------------------------------------------
// New API: ProviderEntry-based (unified)
// ---------------------------------------------------------------------------

/// Create a cloud `LlmProvider` from a unified `ProviderEntry`.
///
/// Returns an error for `Local` variants — those use a different code path
/// (`create_local_generator`).
pub fn create_provider_from_entry(entry: &ProviderEntry) -> Result<Box<dyn LlmProvider>> {
    match entry {
        ProviderEntry::Claude { api_key, model, .. } => {
            let mut provider = ClaudeProvider::new(api_key.clone())?;
            if let Some(m) = model {
                provider = provider.with_model(m.clone());
            }
            Ok(Box::new(provider))
        }

        ProviderEntry::Openai { api_key, model, .. } => {
            let mut provider = OpenAIProvider::new_openai(api_key.clone())?;
            if let Some(m) = model {
                provider = provider.with_model(m.clone());
            }
            Ok(Box::new(provider))
        }

        ProviderEntry::Grok { api_key, model, .. } => {
            let mut provider = OpenAIProvider::new_grok(api_key.clone())?;
            if let Some(m) = model {
                provider = provider.with_model(m.clone());
            }
            Ok(Box::new(provider))
        }

        ProviderEntry::Gemini { api_key, model, .. } => {
            let mut provider = GeminiProvider::new(api_key.clone())?;
            if let Some(m) = model {
                provider = provider.with_model(m.clone());
            }
            Ok(Box::new(provider))
        }

        ProviderEntry::Mistral { api_key, model, .. } => {
            let mut provider = OpenAIProvider::new_mistral(api_key.clone())?;
            if let Some(m) = model {
                provider = provider.with_model(m.clone());
            }
            Ok(Box::new(provider))
        }

        ProviderEntry::Groq { api_key, model, .. } => {
            let mut provider = OpenAIProvider::new_groq(api_key.clone())?;
            if let Some(m) = model {
                provider = provider.with_model(m.clone());
            }
            Ok(Box::new(provider))
        }

        ProviderEntry::Ollama { base_url, model, .. } => {
            Ok(Box::new(OpenAIProvider::new_ollama(base_url.clone(), model.clone())?))
        }

        ProviderEntry::RemoteDaemon { address, .. } => {
            Ok(Box::new(OpenAIProvider::new_remote_daemon(address.clone())?))
        }

        ProviderEntry::Local { .. } => {
            bail!("Local providers use a local generator — call create_local_generator() instead")
        }
    }
}

/// Create providers from a slice of unified `ProviderEntry` values.
/// Only cloud entries are included; `Local` variants are silently skipped.
pub fn create_providers_from_entries(
    entries: &[ProviderEntry],
) -> Result<Vec<Box<dyn LlmProvider>>> {
    let cloud: Vec<_> = entries.iter().filter(|e| !e.is_local()).collect();
    if cloud.is_empty() {
        bail!("No cloud provider entries configured");
    }
    cloud
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            create_provider_from_entry(entry)
                .with_context(|| format!("Failed to create provider #{}", idx + 1))
        })
        .collect()
}

/// Return a single `LlmProvider` from a slice of unified entries.
/// Multiple cloud providers are wrapped in a `FallbackChain`.
pub fn create_provider_from_entries(entries: &[ProviderEntry]) -> Result<Box<dyn LlmProvider>> {
    let providers = create_providers_from_entries(entries)?;
    if providers.len() == 1 {
        Ok(providers
            .into_iter()
            .next()
            .expect("len == 1 checked above"))
    } else {
        use super::FallbackChain;
        Ok(Box::new(FallbackChain::new(providers)))
    }
}

// ---------------------------------------------------------------------------
// Legacy API: TeacherEntry-based (kept for backward compat)
// ---------------------------------------------------------------------------

/// Create providers from teacher entries in priority order.
pub fn create_providers(teachers: &[TeacherEntry]) -> Result<Vec<Box<dyn LlmProvider>>> {
    if teachers.is_empty() {
        bail!("No teacher providers configured");
    }

    teachers
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            create_provider_from_teacher(entry)
                .with_context(|| format!("Failed to create teacher provider #{}", idx + 1))
        })
        .collect()
}

/// Create a single provider from a `TeacherEntry`.
pub fn create_provider_from_teacher(entry: &TeacherEntry) -> Result<Box<dyn LlmProvider>> {
    match entry.provider.to_lowercase().as_str() {
        "claude" => {
            let mut provider = ClaudeProvider::new(entry.api_key.clone())?;
            if let Some(model) = &entry.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        "openai" => {
            let mut provider = OpenAIProvider::new_openai(entry.api_key.clone())?;
            if let Some(model) = &entry.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        "grok" => {
            let mut provider = OpenAIProvider::new_grok(entry.api_key.clone())?;
            if let Some(model) = &entry.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        "gemini" => {
            let mut provider = GeminiProvider::new(entry.api_key.clone())?;
            if let Some(model) = &entry.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        "mistral" => {
            let mut provider = OpenAIProvider::new_mistral(entry.api_key.clone())?;
            if let Some(model) = &entry.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        "groq" => {
            let mut provider = OpenAIProvider::new_groq(entry.api_key.clone())?;
            if let Some(model) = &entry.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        _ => bail!("Unknown provider: {}", entry.provider),
    }
}

/// Create a fallback chain with all teachers in priority order.
pub fn create_provider(teachers: &[TeacherEntry]) -> Result<Box<dyn LlmProvider>> {
    let providers = create_providers(teachers)?;

    if providers.len() == 1 {
        Ok(providers
            .into_iter()
            .next()
            .expect("len == 1 checked above"))
    } else {
        use super::FallbackChain;
        Ok(Box::new(FallbackChain::new(providers)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExecutionTarget;
    use crate::config::{ProviderEntry, TeacherEntry};
    use crate::models::unified_loader::{InferenceProvider, ModelFamily, ModelSize};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn entry(provider: &str, api_key: &str) -> TeacherEntry {
        TeacherEntry {
            provider: provider.to_string(),
            api_key: api_key.to_string(),
            model: None,
            base_url: None,
            name: None,
        }
    }

    fn entry_with_model(provider: &str, api_key: &str, model: &str) -> TeacherEntry {
        TeacherEntry {
            provider: provider.to_string(),
            api_key: api_key.to_string(),
            model: Some(model.to_string()),
            base_url: None,
            name: None,
        }
    }

    fn pentry(variant: ProviderEntry) -> ProviderEntry {
        variant
    }

    // -----------------------------------------------------------------------
    // TeacherEntry-based tests (legacy API)
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_claude_provider() {
        let provider = create_provider_from_teacher(&entry("claude", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "claude");
    }

    #[test]
    fn test_create_openai_provider() {
        let provider = create_provider_from_teacher(&entry("openai", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "openai");
    }

    #[test]
    fn test_create_grok_provider() {
        let provider = create_provider_from_teacher(&entry("grok", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "grok");
    }

    #[test]
    fn test_create_gemini_provider() {
        let provider = create_provider_from_teacher(&entry("gemini", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "gemini");
    }

    #[test]
    fn test_create_mistral_provider() {
        let provider = create_provider_from_teacher(&entry("mistral", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "mistral");
    }

    #[test]
    fn test_create_groq_provider() {
        let provider = create_provider_from_teacher(&entry("groq", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "groq");
    }

    #[test]
    fn test_unknown_provider_returns_error() {
        let result = create_provider_from_teacher(&entry("unknown_provider_xyz", "test-key"));
        assert!(result.is_err());
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("Unknown provider"));
    }

    #[test]
    fn test_empty_teachers_returns_error() {
        let result = create_providers(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_multiple_teachers() {
        let teachers = vec![entry("openai", "key-1"), entry("claude", "key-2")];
        let providers = create_providers(&teachers).unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].name(), "openai");
        assert_eq!(providers[1].name(), "claude");
    }

    #[test]
    fn test_single_teacher_returns_direct_provider_not_fallback() {
        let teachers = vec![entry("claude", "key-1")];
        let provider = create_provider(&teachers).unwrap();
        assert_eq!(provider.name(), "claude");
    }

    #[test]
    fn test_custom_model_is_applied() {
        let e = entry_with_model("openai", "key", "gpt-4o-mini");
        let provider = create_provider_from_teacher(&e).unwrap();
        assert_eq!(provider.default_model(), "gpt-4o-mini");
    }

    #[test]
    fn test_case_insensitive_provider_name() {
        let mut e = entry("claude", "key");
        e.provider = "Claude".to_string();
        let provider = create_provider_from_teacher(&e);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "claude");
    }

    #[test]
    fn test_same_provider_different_models() {
        let teachers = vec![
            TeacherEntry {
                provider: "openai".to_string(),
                api_key: "test-key".to_string(),
                model: Some("gpt-4o".to_string()),
                base_url: None,
                name: Some("GPT-4o (best)".to_string()),
            },
            TeacherEntry {
                provider: "openai".to_string(),
                api_key: "test-key".to_string(),
                model: Some("gpt-4o-mini".to_string()),
                base_url: None,
                name: Some("GPT-4o-mini (cheaper)".to_string()),
            },
        ];

        let providers = create_providers(&teachers).unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].name(), "openai");
        assert_eq!(providers[1].name(), "openai");
        assert_eq!(providers[0].default_model(), "gpt-4o");
        assert_eq!(providers[1].default_model(), "gpt-4o-mini");
    }

    // -----------------------------------------------------------------------
    // ProviderEntry-based tests (new API)
    // -----------------------------------------------------------------------

    #[test]
    fn test_provider_entry_claude() {
        let p = pentry(ProviderEntry::Claude {
            api_key: "sk-ant-test".to_string(),
            model: None,
            base_url: None,
            name: None,
        });
        let provider = create_provider_from_entry(&p).unwrap();
        assert_eq!(provider.name(), "claude");
    }

    #[test]
    fn test_provider_entry_grok() {
        let p = pentry(ProviderEntry::Grok {
            api_key: "xai-test".to_string(),
            model: Some("grok-code-fast-1".to_string()),
            name: None,
        });
        let provider = create_provider_from_entry(&p).unwrap();
        assert_eq!(provider.name(), "grok");
        assert_eq!(provider.default_model(), "grok-code-fast-1");
    }

    #[test]
    fn test_provider_entry_local_returns_error() {
        let p = pentry(ProviderEntry::Local {
            inference_provider: InferenceProvider::Onnx,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
            enabled: true,
            name: None,
        });
        let result = create_provider_from_entry(&p);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_providers_from_entries_skips_local() {
        let entries = vec![
            ProviderEntry::Claude {
                api_key: "key".to_string(),
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
        let providers = create_providers_from_entries(&entries).unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].name(), "claude");
    }

    #[test]
    fn test_create_providers_from_entries_empty_cloud_errors() {
        let entries = vec![ProviderEntry::Local {
            inference_provider: InferenceProvider::Onnx,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
            enabled: true,
            name: None,
        }];
        let result = create_providers_from_entries(&entries);
        assert!(result.is_err());
    }
}

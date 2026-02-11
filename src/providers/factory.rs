// Provider factory
//
// Creates LLM providers based on teacher configuration

use anyhow::{anyhow, bail, Context, Result};

use super::claude::ClaudeProvider;
use super::gemini::GeminiProvider;
use super::openai::OpenAIProvider;
use super::LlmProvider;
use crate::config::TeacherEntry;

/// Create providers from teacher entries in priority order
///
/// The first provider in the returned list is the active teacher.
/// Additional providers are available for easy switching via config reordering.
pub fn create_providers(teachers: &[TeacherEntry]) -> Result<Vec<Box<dyn LlmProvider>>> {
    if teachers.is_empty() {
        bail!("No teacher providers configured");
    }

    teachers
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            create_provider_from_entry(entry)
                .with_context(|| format!("Failed to create teacher provider #{}", idx + 1))
        })
        .collect()
}

/// Create a single provider from a teacher entry
fn create_provider_from_entry(entry: &TeacherEntry) -> Result<Box<dyn LlmProvider>> {
    match entry.provider.as_str() {
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

/// Create the active teacher provider (first in teacher list)
///
/// This is the primary teacher that the local model will learn from.
pub fn create_provider(teachers: &[TeacherEntry]) -> Result<Box<dyn LlmProvider>> {
    teachers
        .first()
        .ok_or_else(|| anyhow!("No teacher providers configured"))
        .and_then(|entry| create_provider_from_entry(entry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProviderSettings, TeacherEntry};
    use std::collections::HashMap;

    #[test]
    fn test_create_claude_provider() {
        let mut settings = HashMap::new();
        settings.insert(
            "claude".to_string(),
            ProviderSettings {
                api_key: "test-key".to_string(),
                model: None,
                base_url: None,
            },
        );

        let config = TeacherConfig {
            provider: Some("claude".to_string()),
            settings,
            teachers: vec![],
        };

        let provider = create_provider(&config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "claude");
    }

    #[test]
    fn test_create_openai_provider() {
        let mut settings = HashMap::new();
        settings.insert(
            "openai".to_string(),
            ProviderSettings {
                api_key: "test-key".to_string(),
                model: None,
                base_url: None,
            },
        );

        let config = TeacherConfig {
            provider: Some("openai".to_string()),
            settings,
            teachers: vec![],
        };

        let provider = create_provider(&config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "openai");
    }

    #[test]
    fn test_create_grok_provider() {
        let mut settings = HashMap::new();
        settings.insert(
            "grok".to_string(),
            ProviderSettings {
                api_key: "test-key".to_string(),
                model: None,
                base_url: None,
            },
        );

        let config = TeacherConfig {
            provider: Some("grok".to_string()),
            settings,
            teachers: vec![],
        };

        let provider = create_provider(&config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "grok");
    }

    #[test]
    fn test_create_gemini_provider() {
        let mut settings = HashMap::new();
        settings.insert(
            "gemini".to_string(),
            ProviderSettings {
                api_key: "test-key".to_string(),
                model: None,
                base_url: None,
            },
        );

        let config = TeacherConfig {
            provider: Some("gemini".to_string()),
            settings,
            teachers: vec![],
        };

        let provider = create_provider(&config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "gemini");
    }

    #[test]
    fn test_unknown_provider() {
        let config = TeacherConfig {
            provider: Some("unknown".to_string()),
            settings: HashMap::new(),
            teachers: vec![],
        };

        let provider = create_provider(&config);
        assert!(provider.is_err());
    }

    #[test]
    fn test_multiple_teachers() {
        let config = TeacherConfig {
            provider: None,
            settings: HashMap::new(),
            teachers: vec![
                TeacherEntry {
                    provider: "openai".to_string(),
                    api_key: "test-key-1".to_string(),
                    model: Some("gpt-4o".to_string()),
                    base_url: None,
                    name: Some("GPT-4o".to_string()),
                },
                TeacherEntry {
                    provider: "claude".to_string(),
                    api_key: "test-key-2".to_string(),
                    model: None,
                    base_url: None,
                    name: Some("Claude Sonnet".to_string()),
                },
            ],
        };

        let providers = create_providers(&config).unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].name(), "openai");
        assert_eq!(providers[1].name(), "claude");
    }

    #[test]
    fn test_active_teacher() {
        let config = TeacherConfig {
            provider: None,
            settings: HashMap::new(),
            teachers: vec![
                TeacherEntry {
                    provider: "openai".to_string(),
                    api_key: "test-key-1".to_string(),
                    model: Some("gpt-4o".to_string()),
                    base_url: None,
                    name: Some("GPT-4o (active)".to_string()),
                },
                TeacherEntry {
                    provider: "claude".to_string(),
                    api_key: "test-key-2".to_string(),
                    model: None,
                    base_url: None,
                    name: Some("Claude (backup)".to_string()),
                },
            ],
        };

        // create_provider should return the FIRST teacher (active one)
        let provider = create_provider(&config).unwrap();
        assert_eq!(provider.name(), "openai");
        assert_eq!(provider.default_model(), "gpt-4o");
    }

    #[test]
    fn test_legacy_config_compatibility() {
        let mut settings = HashMap::new();
        settings.insert(
            "claude".to_string(),
            ProviderSettings {
                api_key: "test-key".to_string(),
                model: Some("claude-opus-4-6".to_string()),
                base_url: None,
            },
        );

        let config = TeacherConfig {
            provider: Some("claude".to_string()),
            settings,
            teachers: vec![],
        };

        let entries = config.get_teachers();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].provider, "claude");
        assert_eq!(entries[0].model, Some("claude-opus-4-6".to_string()));
    }

    #[test]
    fn test_same_provider_different_models() {
        let config = TeacherConfig {
            provider: None,
            settings: HashMap::new(),
            teachers: vec![
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
            ],
        };

        let providers = create_providers(&config).unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].name(), "openai");
        assert_eq!(providers[1].name(), "openai");
        // Both are OpenAI but with different models
        assert_eq!(providers[0].default_model(), "gpt-4o");
        assert_eq!(providers[1].default_model(), "gpt-4o-mini");
    }
}

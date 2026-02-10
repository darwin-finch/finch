// Provider factory
//
// Creates LLM providers based on configuration

use anyhow::{bail, Result};

use super::claude::ClaudeProvider;
use super::gemini::GeminiProvider;
use super::openai::OpenAIProvider;
use super::LlmProvider;
use crate::config::FallbackConfig;

/// Create a provider based on the fallback configuration
pub fn create_provider(config: &FallbackConfig) -> Result<Box<dyn LlmProvider>> {
    let provider_name = config.provider.as_str();

    // Get settings for the selected provider
    let settings = config
        .get_current_settings()
        .ok_or_else(|| anyhow::anyhow!("No settings found for provider: {}", provider_name))?;

    match provider_name {
        "claude" => {
            let mut provider = ClaudeProvider::new(settings.api_key.clone())?;
            if let Some(model) = &settings.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        "openai" => {
            let provider = OpenAIProvider::new_openai(settings.api_key.clone())?;
            // TODO: Support custom model override
            Ok(Box::new(provider))
        }

        "grok" => {
            let provider = OpenAIProvider::new_grok(settings.api_key.clone())?;
            // TODO: Support custom model override
            Ok(Box::new(provider))
        }

        "gemini" => {
            let mut provider = GeminiProvider::new(settings.api_key.clone())?;
            if let Some(model) = &settings.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        _ => bail!("Unknown provider: {}", provider_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderSettings;
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

        let config = FallbackConfig {
            provider: "claude".to_string(),
            settings,
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

        let config = FallbackConfig {
            provider: "openai".to_string(),
            settings,
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

        let config = FallbackConfig {
            provider: "grok".to_string(),
            settings,
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

        let config = FallbackConfig {
            provider: "gemini".to_string(),
            settings,
        };

        let provider = create_provider(&config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "gemini");
    }

    #[test]
    fn test_unknown_provider() {
        let config = FallbackConfig {
            provider: "unknown".to_string(),
            settings: HashMap::new(),
        };

        let provider = create_provider(&config);
        assert!(provider.is_err());
    }
}

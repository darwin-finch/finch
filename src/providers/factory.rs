// Provider factory
//
// Creates LLM providers based on teacher configuration

use anyhow::{bail, Context, Result};

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
pub fn create_provider_from_entry(entry: &TeacherEntry) -> Result<Box<dyn LlmProvider>> {
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

/// Create a fallback chain with all teachers in priority order
///
/// The first teacher is the primary provider, additional teachers are fallbacks.
/// If the primary fails, the system will try the next teacher automatically.
pub fn create_provider(teachers: &[TeacherEntry]) -> Result<Box<dyn LlmProvider>> {
    let providers = create_providers(teachers)?;

    if providers.len() == 1 {
        // Single provider - return directly (no fallback needed)
        Ok(providers.into_iter().next().unwrap())
    } else {
        // Multiple providers - wrap in fallback chain
        use super::FallbackChain;
        Ok(Box::new(FallbackChain::new(providers)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TeacherEntry;

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

    #[test]
    fn test_create_claude_provider() {
        let provider = create_provider_from_entry(&entry("claude", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "claude");
    }

    #[test]
    fn test_create_openai_provider() {
        let provider = create_provider_from_entry(&entry("openai", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "openai");
    }

    #[test]
    fn test_create_grok_provider() {
        let provider = create_provider_from_entry(&entry("grok", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "grok");
    }

    #[test]
    fn test_create_gemini_provider() {
        let provider = create_provider_from_entry(&entry("gemini", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "gemini");
    }

    #[test]
    fn test_create_mistral_provider() {
        let provider = create_provider_from_entry(&entry("mistral", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "mistral");
    }

    #[test]
    fn test_create_groq_provider() {
        let provider = create_provider_from_entry(&entry("groq", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "groq");
    }

    #[test]
    fn test_unknown_provider_returns_error() {
        let result = create_provider_from_entry(&entry("unknown_provider_xyz", "test-key"));
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("Unknown provider"));
    }

    #[test]
    fn test_empty_teachers_returns_error() {
        let result = create_providers(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_multiple_teachers() {
        let teachers = vec![
            entry("openai", "key-1"),
            entry("claude", "key-2"),
        ];
        let providers = create_providers(&teachers).unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].name(), "openai");
        assert_eq!(providers[1].name(), "claude");
    }

    #[test]
    fn test_single_teacher_returns_direct_provider_not_fallback() {
        // Single provider should NOT be wrapped in a FallbackChain
        let teachers = vec![entry("claude", "key-1")];
        let provider = create_provider(&teachers).unwrap();
        assert_eq!(provider.name(), "claude");
    }

    #[test]
    fn test_custom_model_is_applied() {
        let e = entry_with_model("openai", "key", "gpt-4o-mini");
        let provider = create_provider_from_entry(&e).unwrap();
        assert_eq!(provider.default_model(), "gpt-4o-mini");
    }

    #[test]
    fn test_case_insensitive_provider_name() {
        let mut e = entry("claude", "key");
        e.provider = "Claude".to_string();
        let provider = create_provider_from_entry(&e);
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
        // Both are OpenAI but with different models
        assert_eq!(providers[0].default_model(), "gpt-4o");
        assert_eq!(providers[1].default_model(), "gpt-4o-mini");
    }
}

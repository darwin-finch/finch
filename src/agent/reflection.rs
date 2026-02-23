// Reflection engine — periodically updates the agent's persona based on completed work

use anyhow::{Context, Result};
use std::path::Path;

use crate::claude::types::{Message, MessageRequest};
use crate::claude::ClaudeClient;
use crate::config::persona::Persona;

/// Sends completed task summaries to the teacher API and patches the persona file
pub struct ReflectionEngine {
    client: ClaudeClient,
    model: String,
}

impl ReflectionEngine {
    pub fn new(client: ClaudeClient, model: String) -> Self {
        Self { client, model }
    }

    /// Reflect on completed tasks and update the persona's system prompt.
    ///
    /// Returns the new system prompt (or an empty string if nothing changed).
    pub async fn reflect(
        &self,
        persona: &Persona,
        persona_path: Option<&Path>,
        completed_tasks: &[String],
    ) -> Result<String> {
        if completed_tasks.is_empty() {
            return Ok(String::new());
        }

        let task_list = completed_tasks
            .iter()
            .enumerate()
            .map(|(i, t)| format!("{}. {}", i + 1, t))
            .collect::<Vec<_>>()
            .join("\n");

        let current_prompt = persona.to_system_message();

        let prompt = format!(
            "You are helping an autonomous AI agent update its persona based on recent work.\n\n\
             Current persona system prompt:\n```\n{}\n```\n\n\
             Recently completed tasks:\n{}\n\n\
             Based on this work, write an updated system_prompt that reflects what the agent \
             has learned or specialised in. Keep it concise (2-5 sentences). \
             Respond with ONLY the new system_prompt text, nothing else.",
            current_prompt, task_list
        );

        let request = MessageRequest {
            model: self.model.clone(),
            max_tokens: 512,
            messages: vec![Message::user(&prompt)],
            system: Some(
                "You are a helpful assistant that updates AI agent personas concisely.".to_string(),
            ),
            tools: None,
        };

        let response = self
            .client
            .send_message(&request)
            .await
            .context("Failed to get reflection response from teacher API")?;

        let new_prompt = response.text().trim().to_string();

        // Patch the persona file if we have a writable path
        if !new_prompt.is_empty() {
            if let Some(path) = persona_path {
                if path.exists() {
                    if let Err(e) = patch_persona_file(path, &new_prompt) {
                        tracing::warn!("Failed to patch persona file: {}", e);
                    }
                }
            }
        }

        Ok(new_prompt)
    }
}

/// Update only the `behavior.system_prompt` key in a persona TOML file.
/// All other fields (name, git_name, git_email, tone, etc.) are preserved.
pub(crate) fn patch_persona_file(path: &Path, new_prompt: &str) -> Result<()> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read persona file: {}", path.display()))?;

    let mut persona: toml::Value =
        toml::from_str(&contents).context("Failed to parse persona TOML for reflection")?;

    if let Some(behavior) = persona.get_mut("behavior") {
        if let Some(sp) = behavior.get_mut("system_prompt") {
            *sp = toml::Value::String(new_prompt.to_string());
        }
    }

    let updated =
        toml::to_string_pretty(&persona).context("Failed to serialize updated persona")?;

    std::fs::write(path, updated)
        .with_context(|| format!("Failed to write updated persona: {}", path.display()))?;

    tracing::info!("Persona updated via reflection at {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::persona::Persona;
    use std::fs;
    use tempfile::NamedTempFile;

    fn write_persona_file(content: &str) -> NamedTempFile {
        let f = NamedTempFile::new().unwrap();
        fs::write(f.path(), content).unwrap();
        f
    }

    const BASIC_PERSONA_TOML: &str = r#"
[persona]
name = "Test"
description = "Test persona"
version = "1.0"

[behavior]
system_prompt = "You are a test assistant."
tone = "Casual"
verbosity = "Concise"
focus = "Helpfulness"
"#;

    // ── patch_persona_file ────────────────────────────────────────────────────

    #[test]
    fn test_patch_updates_system_prompt() {
        let f = write_persona_file(BASIC_PERSONA_TOML);
        patch_persona_file(f.path(), "Updated system prompt.").unwrap();

        let persona = Persona::load(f.path()).unwrap();
        assert_eq!(persona.behavior.system_prompt, "Updated system prompt.");
    }

    #[test]
    fn test_patch_preserves_name_and_metadata() {
        let f = write_persona_file(BASIC_PERSONA_TOML);
        patch_persona_file(f.path(), "New prompt.").unwrap();

        let persona = Persona::load(f.path()).unwrap();
        assert_eq!(persona.persona.name, "Test");
        assert_eq!(persona.persona.description, "Test persona");
        assert_eq!(persona.persona.version, "1.0");
    }

    #[test]
    fn test_patch_preserves_behavior_fields() {
        let f = write_persona_file(BASIC_PERSONA_TOML);
        patch_persona_file(f.path(), "New prompt.").unwrap();

        let persona = Persona::load(f.path()).unwrap();
        assert_eq!(persona.behavior.tone, "Casual");
        assert_eq!(persona.behavior.verbosity, "Concise");
        assert_eq!(persona.behavior.focus, "Helpfulness");
    }

    #[test]
    fn test_patch_preserves_git_identity() {
        let toml_with_git = r#"
[persona]
name = "Agent"
description = "Agent persona"

[behavior]
system_prompt = "Original."
git_name = "Vesper"
git_email = "vesper@local.finch"
"#;
        let f = write_persona_file(toml_with_git);
        patch_persona_file(f.path(), "Updated.").unwrap();

        let persona = Persona::load(f.path()).unwrap();
        assert_eq!(persona.behavior.system_prompt, "Updated.");
        assert_eq!(persona.behavior.git_name.as_deref(), Some("Vesper"));
        assert_eq!(
            persona.behavior.git_email.as_deref(),
            Some("vesper@local.finch")
        );
    }

    #[test]
    fn test_patch_is_idempotent() {
        let f = write_persona_file(BASIC_PERSONA_TOML);
        patch_persona_file(f.path(), "First update.").unwrap();
        patch_persona_file(f.path(), "Second update.").unwrap();

        let persona = Persona::load(f.path()).unwrap();
        assert_eq!(persona.behavior.system_prompt, "Second update.");
    }

    #[test]
    fn test_patch_roundtrip_is_parseable() {
        // After patching, the file must still be parseable as a Persona
        let f = write_persona_file(BASIC_PERSONA_TOML);
        let long_prompt = "A".repeat(500);
        patch_persona_file(f.path(), &long_prompt).unwrap();

        let persona = Persona::load(f.path()).unwrap();
        assert_eq!(persona.behavior.system_prompt, long_prompt);
    }

    #[test]
    fn test_patch_prompt_with_special_characters() {
        let f = write_persona_file(BASIC_PERSONA_TOML);
        let tricky = "You work on Rust. Use `unwrap()` sparingly.\nAvoid O(n²) algorithms.";
        patch_persona_file(f.path(), tricky).unwrap();

        let persona = Persona::load(f.path()).unwrap();
        assert_eq!(persona.behavior.system_prompt, tricky);
    }

    #[test]
    fn test_patch_nonexistent_file_returns_error() {
        let result = patch_persona_file(Path::new("/nonexistent/path.toml"), "prompt");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("persona file"));
    }

    #[test]
    fn test_patch_invalid_toml_returns_error() {
        let f = write_persona_file("not valid {{{{ toml");
        let result = patch_persona_file(f.path(), "prompt");
        assert!(result.is_err());
    }

    // ── reflect() early-exit (no network call) ────────────────────────────────

    #[tokio::test]
    async fn test_reflect_empty_tasks_returns_early_without_api_call() {
        // The early-exit path (empty tasks) returns before calling the teacher API.
        // We use a mock provider that panics if called, proving no API call is made.
        use crate::claude::ClaudeClient;
        use crate::providers::types::{ProviderRequest, ProviderResponse, StreamChunk};
        use crate::providers::LlmProvider;
        use anyhow::Result;
        use tokio::sync::mpsc::Receiver;

        struct PanicProvider;

        #[async_trait::async_trait]
        impl LlmProvider for PanicProvider {
            async fn send_message(&self, _: &ProviderRequest) -> Result<ProviderResponse> {
                panic!("send_message should not be called for empty task list");
            }
            async fn send_message_stream(
                &self,
                _: &ProviderRequest,
            ) -> Result<Receiver<Result<StreamChunk>>> {
                panic!("send_message_stream should not be called for empty task list");
            }
            fn name(&self) -> &str {
                "panic"
            }
            fn default_model(&self) -> &str {
                "none"
            }
        }

        let client = ClaudeClient::with_provider(Box::new(PanicProvider));
        let eng = ReflectionEngine::new(client, "unused".to_string());
        let persona = Persona::default();

        let result = eng.reflect(&persona, None, &[]).await.unwrap();
        assert!(
            result.is_empty(),
            "expected empty string for empty task list"
        );
    }
}

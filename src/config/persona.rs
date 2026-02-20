// Persona system for customizing AI behavior
//
// Enables per-machine customization (e.g., "Louis" on laptop, "Analyst" on desktop)

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// A persona defines how the AI should behave
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Persona {
    /// Metadata about the persona
    pub persona: PersonaMetadata,

    /// Behavior configuration
    pub behavior: PersonaBehavior,
}

/// Metadata about a persona
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaMetadata {
    /// Persona name (e.g., "Louis", "Expert Coder")
    pub name: String,

    /// Description of this persona
    pub description: String,

    /// Version string
    #[serde(default)]
    pub version: String,
}

/// Behavior configuration for a persona
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaBehavior {
    /// System prompt that defines behavior
    pub system_prompt: String,

    /// Tone (e.g., "Professional", "Casual", "Technical")
    #[serde(default)]
    pub tone: String,

    /// Verbosity level ("Concise", "Detailed", "Verbose")
    #[serde(default)]
    pub verbosity: String,

    /// Focus area (e.g., "Helpfulness", "Accuracy", "Creativity")
    #[serde(default)]
    pub focus: String,

    /// Example interactions (for few-shot learning)
    #[serde(default)]
    pub examples: Vec<PersonaExample>,

    /// Git author name used when agent commits changes autonomously
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_name: Option<String>,

    /// Git author email used when agent commits changes autonomously
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_email: Option<String>,
}

/// Example interaction for few-shot learning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaExample {
    pub user: String,
    pub assistant: String,
}

impl Persona {
    /// Load persona from TOML file
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read persona from {}", path.display()))?;

        toml::from_str(&contents)
            .context("Failed to parse persona TOML")
    }

    /// Load built-in persona by name
    pub fn load_builtin(name: &str) -> Result<Self> {
        let template = match name {
            "default" => include_str!("../../data/personas/default.toml"),
            "expert-coder" => include_str!("../../data/personas/expert-coder.toml"),
            "teacher" => include_str!("../../data/personas/teacher.toml"),
            "analyst" => include_str!("../../data/personas/analyst.toml"),
            "creative" => include_str!("../../data/personas/creative.toml"),
            "researcher" => include_str!("../../data/personas/researcher.toml"),
            "autonomous" => include_str!("../../data/personas/autonomous.toml"),
            _ => anyhow::bail!("Unknown builtin persona: {}", name),
        };

        toml::from_str(template)
            .with_context(|| format!("Failed to parse builtin persona: {}", name))
    }

    /// Load persona by name: checks ~/.finch/personas/<name>.toml first, then builtins
    pub fn load_by_name(name: &str) -> Result<Self> {
        if let Some(home) = dirs::home_dir() {
            let user_path = home.join(".finch/personas").join(format!("{}.toml", name));
            if user_path.exists() {
                return Self::load(&user_path).with_context(|| {
                    format!("Failed to load user persona from {}", user_path.display())
                });
            }
        }
        Self::load_builtin(name)
    }

    /// Get system prompt formatted for injection
    pub fn to_system_message(&self) -> String {
        // Optionally include examples in system prompt
        if self.behavior.examples.is_empty() {
            self.behavior.system_prompt.clone()
        } else {
            let mut prompt = self.behavior.system_prompt.clone();
            prompt.push_str("\n\nExample interactions:\n");
            for example in &self.behavior.examples {
                prompt.push_str(&format!("\nUser: {}\nAssistant: {}\n", example.user, example.assistant));
            }
            prompt
        }
    }

    /// Get persona name
    pub fn name(&self) -> &str {
        &self.persona.name
    }

    /// Get persona tone
    pub fn tone(&self) -> &str {
        &self.behavior.tone
    }

    /// Get persona verbosity
    pub fn verbosity(&self) -> &str {
        &self.behavior.verbosity
    }

    /// Get persona focus
    pub fn focus(&self) -> &str {
        &self.behavior.focus
    }

    /// List available builtin personas
    pub fn list_builtins() -> Vec<&'static str> {
        vec!["default", "expert-coder", "teacher", "analyst", "creative", "researcher", "autonomous"]
    }
}

impl Default for Persona {
    fn default() -> Self {
        Self {
            persona: PersonaMetadata {
                name: "Default".to_string(),
                description: "Helpful AI assistant".to_string(),
                version: "1.0".to_string(),
            },
            behavior: PersonaBehavior {
                system_prompt: "You are a helpful AI assistant. Provide clear, accurate, and concise responses.".to_string(),
                tone: "Professional".to_string(),
                verbosity: "Balanced".to_string(),
                focus: "Helpfulness".to_string(),
                examples: Vec::new(),
                git_name: None,
                git_email: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_persona(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    // ── default ───────────────────────────────────────────────────────────────

    #[test]
    fn test_default_persona_name() {
        let persona = Persona::default();
        assert_eq!(persona.name(), "Default");
    }

    #[test]
    fn test_default_persona_has_system_prompt() {
        let persona = Persona::default();
        assert!(!persona.behavior.system_prompt.is_empty());
    }

    #[test]
    fn test_default_persona_has_no_git_identity() {
        let persona = Persona::default();
        assert!(persona.behavior.git_name.is_none());
        assert!(persona.behavior.git_email.is_none());
    }

    // ── builtins ──────────────────────────────────────────────────────────────

    #[test]
    fn test_all_builtins_load_without_error() {
        for name in Persona::list_builtins() {
            let result = Persona::load_builtin(name);
            assert!(result.is_ok(), "Failed to load builtin persona: {}", name);
        }
    }

    #[test]
    fn test_builtins_list_includes_autonomous() {
        assert!(Persona::list_builtins().contains(&"autonomous"));
    }

    #[test]
    fn test_autonomous_builtin_has_git_identity() {
        let persona = Persona::load_builtin("autonomous").unwrap();
        assert!(
            persona.behavior.git_name.is_some(),
            "autonomous persona should have git_name"
        );
        assert!(
            persona.behavior.git_email.is_some(),
            "autonomous persona should have git_email"
        );
    }

    #[test]
    fn test_autonomous_builtin_has_non_empty_system_prompt() {
        let persona = Persona::load_builtin("autonomous").unwrap();
        assert!(!persona.behavior.system_prompt.is_empty());
    }

    #[test]
    fn test_unknown_builtin_returns_error() {
        let result = Persona::load_builtin("nonexistent-persona-xyz");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Unknown builtin persona"));
    }

    // ── load from file ────────────────────────────────────────────────────────

    #[test]
    fn test_load_from_toml_file() {
        let toml = r#"
[persona]
name = "Custom"
description = "A custom persona"
version = "2.0"

[behavior]
system_prompt = "You are a custom assistant."
tone = "Casual"
"#;
        let f = write_persona(toml);
        let persona = Persona::load(f.path()).unwrap();
        assert_eq!(persona.name(), "Custom");
        assert_eq!(persona.behavior.system_prompt, "You are a custom assistant.");
        assert_eq!(persona.behavior.tone, "Casual");
    }

    #[test]
    fn test_load_file_with_git_identity() {
        let toml = r#"
[persona]
name = "Vesper"
description = "Named agent"

[behavior]
system_prompt = "I am Vesper."
git_name = "Vesper"
git_email = "vesper@example.com"
"#;
        let f = write_persona(toml);
        let persona = Persona::load(f.path()).unwrap();
        assert_eq!(persona.behavior.git_name.as_deref(), Some("Vesper"));
        assert_eq!(persona.behavior.git_email.as_deref(), Some("vesper@example.com"));
    }

    #[test]
    fn test_load_file_without_git_identity_gives_none() {
        let toml = r#"
[persona]
name = "Simple"
description = "No git fields"

[behavior]
system_prompt = "Simple assistant."
"#;
        let f = write_persona(toml);
        let persona = Persona::load(f.path()).unwrap();
        assert!(persona.behavior.git_name.is_none());
        assert!(persona.behavior.git_email.is_none());
    }

    #[test]
    fn test_load_nonexistent_file_returns_error() {
        let result = Persona::load(Path::new("/nonexistent/persona.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_load_invalid_toml_returns_error() {
        let f = write_persona("not valid {{ toml at all");
        let result = Persona::load(f.path());
        assert!(result.is_err());
    }

    // ── load_by_name fallthrough ──────────────────────────────────────────────

    #[test]
    fn test_load_by_name_falls_through_to_builtin() {
        // There is no ~/.finch/personas/default.toml in the test environment
        // (or if there is, the test still passes since the builtin is valid)
        let persona = Persona::load_by_name("default");
        assert!(persona.is_ok(), "load_by_name should fall back to builtin");
    }

    #[test]
    fn test_load_by_name_unknown_returns_error() {
        let result = Persona::load_by_name("definitely-does-not-exist-xyz");
        assert!(result.is_err());
    }

    // ── to_system_message ─────────────────────────────────────────────────────

    #[test]
    fn test_to_system_message_no_examples() {
        let persona = Persona::default();
        let msg = persona.to_system_message();
        assert_eq!(msg, persona.behavior.system_prompt);
    }

    #[test]
    fn test_to_system_message_with_examples_appends_them() {
        let mut persona = Persona::default();
        persona.behavior.examples.push(PersonaExample {
            user: "What is 2+2?".to_string(),
            assistant: "4".to_string(),
        });
        let msg = persona.to_system_message();
        assert!(msg.contains("What is 2+2?"));
        assert!(msg.contains("Example interactions"));
    }

    // ── accessor methods ──────────────────────────────────────────────────────

    #[test]
    fn test_accessors_return_correct_values() {
        let persona = Persona::default();
        assert_eq!(persona.tone(), "Professional");
        assert_eq!(persona.verbosity(), "Balanced");
        assert_eq!(persona.focus(), "Helpfulness");
    }

    // ── serde round-trip ──────────────────────────────────────────────────────

    #[test]
    fn test_git_fields_not_serialized_when_none() {
        let persona = Persona::default();
        // Serialize via toml and ensure git_name/git_email don't appear
        let toml_str = toml::to_string(&persona).unwrap();
        assert!(!toml_str.contains("git_name"), "git_name should be absent when None");
        assert!(!toml_str.contains("git_email"), "git_email should be absent when None");
    }

    #[test]
    fn test_git_fields_serialized_when_present() {
        let mut persona = Persona::default();
        persona.behavior.git_name = Some("Bot".to_string());
        persona.behavior.git_email = Some("bot@finch".to_string());
        let toml_str = toml::to_string(&persona).unwrap();
        assert!(toml_str.contains("git_name"), "git_name should be present");
        assert!(toml_str.contains("git_email"), "git_email should be present");
    }
}

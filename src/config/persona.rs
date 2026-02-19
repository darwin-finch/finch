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
    /// Persona name (e.g., "Louis", "Expert Coder")
    pub name: String,

    /// Description of this persona
    pub description: String,

    /// System prompt that defines behavior
    pub system_prompt: String,

    /// Example interactions (for few-shot learning)
    #[serde(default)]
    pub examples: Vec<PersonaExample>,

    /// Tone (e.g., "Professional", "Casual", "Technical")
    #[serde(default)]
    pub tone: String,

    /// Verbosity level ("Concise", "Detailed", "Verbose")
    #[serde(default)]
    pub verbosity: String,

    /// Focus area (e.g., "Helpfulness", "Accuracy", "Creativity")
    #[serde(default)]
    pub focus: String,
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
            _ => anyhow::bail!("Unknown builtin persona: {}", name),
        };

        toml::from_str(template)
            .with_context(|| format!("Failed to parse builtin persona: {}", name))
    }

    /// Get system prompt formatted for injection
    pub fn to_system_message(&self) -> String {
        // Optionally include examples in system prompt
        if self.examples.is_empty() {
            self.system_prompt.clone()
        } else {
            let mut prompt = self.system_prompt.clone();
            prompt.push_str("\n\nExample interactions:\n");
            for example in &self.examples {
                prompt.push_str(&format!("\nUser: {}\nAssistant: {}\n", example.user, example.assistant));
            }
            prompt
        }
    }

    /// List available builtin personas
    pub fn list_builtins() -> Vec<&'static str> {
        vec!["default", "expert-coder", "teacher", "analyst", "creative", "researcher"]
    }
}

impl Default for Persona {
    fn default() -> Self {
        Self {
            name: "Default".to_string(),
            description: "Helpful AI assistant".to_string(),
            system_prompt: "You are a helpful AI assistant. Provide clear, accurate, and concise responses.".to_string(),
            examples: Vec::new(),
            tone: "Professional".to_string(),
            verbosity: "Balanced".to_string(),
            focus: "Helpfulness".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_persona() {
        let persona = Persona::default();
        assert_eq!(persona.name, "Default");
        assert!(!persona.system_prompt.is_empty());
    }

    #[test]
    fn test_builtin_personas() {
        for name in Persona::list_builtins() {
            let persona = Persona::load_builtin(name);
            assert!(persona.is_ok(), "Failed to load builtin persona: {}", name);
        }
    }
}

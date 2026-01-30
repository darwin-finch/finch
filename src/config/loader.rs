// Configuration loader
// Loads API key from ~/.claude/settings.json or environment variable

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::fs;

use super::settings::Config;

#[derive(Debug, Deserialize)]
struct ClaudeSettings {
    env: Option<ClaudeEnv>,
}

#[derive(Debug, Deserialize)]
struct ClaudeEnv {
    #[serde(rename = "ANTHROPIC_API_KEY")]
    anthropic_api_key: Option<String>,
}

/// Load configuration from Claude Code settings or environment
pub fn load_config() -> Result<Config> {
    // Try loading from ~/.claude/settings.json first
    if let Some(api_key) = try_load_from_claude_settings()? {
        return Ok(Config::new(api_key));
    }

    // Fall back to environment variable
    if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
        if !api_key.is_empty() {
            return Ok(Config::new(api_key));
        }
    }

    // No API key found
    bail!(
        "Claude API key not found\n\n\
        Checked locations:\n\
        1. ~/.claude/settings.json (env.ANTHROPIC_API_KEY)\n\
        2. Environment variable: $ANTHROPIC_API_KEY\n\n\
        Please set your API key in one of these locations.\n\n\
        Quick setup:\n\
        export ANTHROPIC_API_KEY=\"sk-ant-...\"\n\n\
        Or configure Claude Code:\n\
        https://docs.anthropic.com/en/docs/claude-code"
    );
}

fn try_load_from_claude_settings() -> Result<Option<String>> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let settings_path = home.join(".claude/settings.json");

    if !settings_path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&settings_path)
        .with_context(|| format!("Failed to read {}", settings_path.display()))?;

    let settings: ClaudeSettings =
        serde_json::from_str(&contents).context("Failed to parse Claude settings.json")?;

    Ok(settings.env.and_then(|env| env.anthropic_api_key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_creation() {
        let config = Config::new("test-key".to_string());
        assert_eq!(config.api_key, "test-key");
        assert_eq!(config.similarity_threshold, 0.2);
    }
}

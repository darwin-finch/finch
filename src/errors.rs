// User-friendly error messages
//
// Provides helpers to convert technical errors into actionable messages
// that guide users toward solutions.

use anyhow::{Context, Result};
use crossterm::style::Stylize;
use std::fmt;

/// Localized text helper
fn t(key: &str) -> &'static str {
    let lang = std::env::var("LANG").unwrap_or_default();
    let locale = &lang[..lang.len().min(2)];
    match (locale, key) {
        ("es", "try")            => "Intenta:",
        ("es", "suggestion")     => "Sugerencia:",
        ("es", "possible_causes")=> "Posibles causas:",
        ("fr", "try")            => "Essayez:",
        ("fr", "suggestion")     => "Suggestion:",
        ("fr", "possible_causes")=> "Causes possibles:",
        ("de", "try")            => "Versuchen Sie:",
        ("de", "suggestion")     => "Vorschlag:",
        ("de", "possible_causes")=> "Mögliche Ursachen:",
        (_,    "try")            => "Try:",
        (_,    "suggestion")     => "Suggestion:",
        (_,    "possible_causes")=> "Possible causes:",
        _                        => ":",
    }
}

/// Wrap an error with user-friendly context
pub trait UserFriendlyError {
    fn user_context(self, message: &str) -> Self;
    fn user_context_with_suggestion(self, problem: &str, suggestion: &str) -> Self;
}

impl<T> UserFriendlyError for Result<T> {
    fn user_context(self, message: &str) -> Self {
        self.with_context(|| message.to_string())
    }

    fn user_context_with_suggestion(self, problem: &str, suggestion: &str) -> Self {
        self.with_context(|| {
            format!(
                "{}\n\n{}  {}",
                problem,
                t("suggestion").yellow().bold(),
                suggestion,
            )
        })
    }
}

/// Format a connection refused error with helpful suggestions
pub fn connection_refused_error(address: &str) -> String {
    format!(
        "Could not connect to daemon at {}\n\n\
        {}\n\
        • Daemon is not running\n\
        • Daemon crashed or failed to start\n\
        • Wrong bind address\n\n\
        {}\n\
        1. Start the daemon:\n\
           {}\n\n\
        2. Check daemon logs:\n\
           {}\n\n\
        3. Check if daemon is running:\n\
           {}",
        address,
        t("possible_causes").yellow().bold(),
        t("try").green().bold(),
        "finch daemon-start".cyan(),
        "tail -f ~/.finch/daemon.log".cyan(),
        "ps aux | grep \"finch daemon\"".cyan(),
    )
}

/// Format a model not found error with helpful suggestions
pub fn model_not_found_error(model_name: &str) -> String {
    format!(
        "Model '{}' not found\n\n\
        {}\n\
        • Model not downloaded yet\n\
        • Model download failed\n\
        • Wrong model name\n\n\
        {}\n\
        1. Run setup wizard to download models:\n\
           {}\n\n\
        2. Check model cache:\n\
           {}\n\n\
        3. Verify model name in config:\n\
           {}",
        model_name,
        "Possible causes:".yellow().bold(),
        "Try:".green().bold(),
        "finch setup".cyan(),
        "ls ~/.cache/huggingface/hub/".cyan(),
        "cat ~/.finch/config.toml".cyan(),
    )
}

/// Format an API key error with helpful suggestions
pub fn api_key_invalid_error(provider: &str) -> String {
    format!(
        "{} API key is invalid or missing\n\n\
        {}\n\
        • API key not set in config\n\
        • API key format is incorrect\n\
        • API key has been revoked\n\n\
        {}\n\
        1. Run setup wizard:\n\
           {}\n\n\
        2. Check your config file:\n\
           {}\n\n\
        3. Verify API key format:\n\
           • Claude: sk-ant-...\n\
           • OpenAI: sk-...\n\
           • Gemini: AI...\n\n\
        4. Get a new API key:\n\
           • Claude: https://console.anthropic.com/\n\
           • OpenAI: https://platform.openai.com/api-keys\n\
           • Google: https://makersuite.google.com/app/apikey",
        provider,
        "Possible causes:".yellow().bold(),
        "Try:".green().bold(),
        "finch setup".cyan(),
        "cat ~/.finch/config.toml".cyan(),
    )
}

/// Format a config parse error with helpful suggestions
pub fn config_parse_error(error: &str) -> String {
    format!(
        "Failed to parse config file\n\n\
        {}  {}\n\n\
        {}\n\
        1. Check config file syntax:\n\
           {}\n\n\
        2. Validate TOML format online:\n\
           https://www.toml-lint.com/\n\n\
        3. Backup and regenerate config:\n\
           {}\n\
           {}\n\n\
        4. Common mistakes:\n\
           • Missing quotes around strings\n\
           • Unclosed brackets []\n\
           • Invalid TOML syntax",
        "Error:".yellow().bold(),
        error,
        "Try:".green().bold(),
        "cat ~/.finch/config.toml".cyan(),
        "mv ~/.finch/config.toml ~/.finch/config.toml.backup".cyan(),
        "finch setup".cyan(),
    )
}

/// Format a file not found error with helpful suggestions
pub fn file_not_found_error(path: &str, description: &str) -> String {
    format!(
        "{} not found: {}\n\n\
        {}\n\
        • File has been deleted\n\
        • Wrong path specified\n\
        • Permissions issue\n\n\
        {}\n\
        1. Check if file exists:\n\
           {}\n\n\
        2. Check parent directory:\n\
           {}\n\n\
        3. Verify file permissions:\n\
           {}",
        description,
        path,
        "Possible causes:".yellow().bold(),
        "Try:".green().bold(),
        format!("ls -la {path}").cyan(),
        format!("ls -la $(dirname \"{path}\")").cyan(),
        format!("ls -l {path}").cyan(),
    )
}

/// Format a permission denied error with helpful suggestions
pub fn permission_denied_error(path: &str, operation: &str) -> String {
    format!(
        "Permission denied: cannot {} {}\n\n\
        {}\n\
        • Insufficient file permissions\n\
        • File owned by another user\n\
        • Parent directory not writable\n\n\
        {}\n\
        1. Check file permissions:\n\
           {}\n\n\
        2. Fix permissions if you own the file:\n\
           {}\n\n\
        3. Check parent directory permissions:\n\
           {}",
        operation,
        path,
        "Possible causes:".yellow().bold(),
        "Try:".green().bold(),
        format!("ls -la {path}").cyan(),
        format!("chmod u+rw {path}").cyan(),
        format!("ls -la $(dirname \"{path}\")").cyan(),
    )
}

/// Format a daemon already running error
pub fn daemon_already_running_error(pid: u32) -> String {
    format!(
        "Daemon is already running (PID: {})\n\n\
        {}\n\
        1. Stop the existing daemon:\n\
           {}\n\n\
        2. Start a new daemon:\n\
           {}",
        pid,
        "To restart the daemon:".green().bold(),
        "finch daemon-stop".cyan(),
        "finch daemon-start".cyan(),
    )
}

/// Format a model loading error with helpful suggestions
pub fn model_loading_error(model_name: &str, error: &str) -> String {
    format!(
        "Failed to load model '{}'\n\n\
        {}  {}\n\n\
        {}\n\
        • Corrupted model files\n\
        • Insufficient RAM\n\
        • Incompatible model format\n\n\
        {}\n\
        1. Clear model cache and redownload:\n\
           {}\n\
           {}\n\n\
        2. Check available RAM:\n\
           {}  (Linux)\n\
           {}  (macOS)\n\n\
        3. Try a smaller model:\n\
           • 1.5B models: ~2GB RAM\n\
           • 3B models: ~4GB RAM\n\
           • 7B models: ~8GB RAM",
        model_name,
        "Error:".yellow().bold(),
        error,
        "Possible causes:".yellow().bold(),
        "Try:".green().bold(),
        format!("rm -rf ~/.cache/huggingface/hub/models--*{model_name}").cyan(),
        "finch setup".cyan(),
        "free -h".cyan(),
        "vm_stat".cyan(),
    )
}

/// Wrap a generic error with suggestions
pub fn wrap_error_with_suggestion(error: impl fmt::Display, suggestion: &str) -> String {
    format!(
        "{}\n\n{}  {}",
        error,
        t("suggestion").yellow().bold(),
        suggestion,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_refused_has_helpful_message() {
        let msg = connection_refused_error("127.0.0.1:8000");
        assert!(msg.contains("daemon-start"));
        assert!(msg.contains("daemon.log"));
    }

    #[test]
    fn test_model_not_found_has_setup_suggestion() {
        let msg = model_not_found_error("qwen-3b");
        assert!(msg.contains("finch setup"));
        assert!(msg.contains("~/.cache/huggingface"));
    }

    #[test]
    fn test_api_key_invalid_has_provider_urls() {
        let msg = api_key_invalid_error("Claude");
        assert!(msg.contains("console.anthropic.com"));
        assert!(msg.contains("sk-ant-"));
    }
}

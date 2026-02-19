// User-friendly error messages
//
// Provides helpers to convert technical errors into actionable messages
// that guide users toward solutions.
//
// Localization Support:
// The error messages support localization via the LANG environment variable.
// Currently supported: English (en_US), with framework for future languages.

use anyhow::{Context, Result};
use std::fmt;

/// Get the current locale from environment
fn get_locale() -> &'static str {
    // Check LANG environment variable
    if let Ok(lang) = std::env::var("LANG") {
        if lang.starts_with("es") {
            return "es";
        } else if lang.starts_with("fr") {
            return "fr";
        } else if lang.starts_with("de") {
            return "de";
        } else if lang.starts_with("zh") {
            return "zh";
        } else if lang.starts_with("ja") {
            return "ja";
        }
    }
    "en" // Default to English
}

/// Localized text helper
fn t(key: &str) -> String {
    let locale = get_locale();

    let text = match (locale, key) {
        // English (default)
        ("en", "try") => "Try:",
        ("en", "suggestion") => "Suggestion:",
        ("en", "possible_causes") => "Possible causes:",
        ("en", "error") => "Error:",

        // Spanish
        ("es", "try") => "Intenta:",
        ("es", "suggestion") => "Sugerencia:",
        ("es", "possible_causes") => "Posibles causas:",
        ("es", "error") => "Error:",

        // French
        ("fr", "try") => "Essayez:",
        ("fr", "suggestion") => "Suggestion:",
        ("fr", "possible_causes") => "Causes possibles:",
        ("fr", "error") => "Erreur:",

        // German
        ("de", "try") => "Versuchen Sie:",
        ("de", "suggestion") => "Vorschlag:",
        ("de", "possible_causes") => "Mögliche Ursachen:",
        ("de", "error") => "Fehler:",

        // Default fallback
        _ => match key {
            "try" => "Try:",
            "suggestion" => "Suggestion:",
            "possible_causes" => "Possible causes:",
            "error" => "Error:",
            _ => key,
        }
    };

    text.to_string()
}

/// Wrap an error with user-friendly context
pub trait UserFriendlyError {
    /// Add user-friendly context to this error
    fn user_context(self, message: &str) -> Self;

    /// Add user-friendly context with a suggestion
    fn user_context_with_suggestion(self, problem: &str, suggestion: &str) -> Self;
}

impl<T> UserFriendlyError for Result<T> {
    fn user_context(self, message: &str) -> Self {
        self.with_context(|| format!("{}", message))
    }

    fn user_context_with_suggestion(self, problem: &str, suggestion: &str) -> Self {
        self.with_context(|| {
            format!(
                "{}\n\n\x1b[1;33m{}:\x1b[0m {}",
                problem, t("suggestion"), suggestion
            )
        })
    }
}

/// Format a connection refused error with helpful suggestions
pub fn connection_refused_error(address: &str) -> String {
    format!(
        "Could not connect to daemon at {}\n\n\
        \x1b[1;33m{}:\x1b[0m\n\
        • Daemon is not running\n\
        • Daemon crashed or failed to start\n\
        • Wrong bind address\n\n\
        \x1b[1;32m{}:\x1b[0m\n\
        1. Start the daemon:\n\
           \x1b[36mfinch daemon-start\x1b[0m\n\n\
        2. Check daemon logs:\n\
           \x1b[36mtail -f ~/.finch/daemon.log\x1b[0m\n\n\
        3. Check if daemon is running:\n\
           \x1b[36mps aux | grep \"finch daemon\"\x1b[0m",
        address, t("possible_causes"), t("try")
    )
}

/// Format a model not found error with helpful suggestions
pub fn model_not_found_error(model_name: &str) -> String {
    format!(
        "Model '{}' not found\n\n\
        \x1b[1;33mPossible causes:\x1b[0m\n\
        • Model not downloaded yet\n\
        • Model download failed\n\
        • Wrong model name\n\n\
        \x1b[1;32mTry:\x1b[0m\n\
        1. Run setup wizard to download models:\n\
           \x1b[36mfinch setup\x1b[0m\n\n\
        2. Check model cache:\n\
           \x1b[36mls ~/.cache/huggingface/hub/\x1b[0m\n\n\
        3. Verify model name in config:\n\
           \x1b[36mcat ~/.finch/config.toml\x1b[0m",
        model_name
    )
}

/// Format an API key error with helpful suggestions
pub fn api_key_invalid_error(provider: &str) -> String {
    format!(
        "{} API key is invalid or missing\n\n\
        \x1b[1;33mPossible causes:\x1b[0m\n\
        • API key not set in config\n\
        • API key format is incorrect\n\
        • API key has been revoked\n\n\
        \x1b[1;32mTry:\x1b[0m\n\
        1. Run setup wizard:\n\
           \x1b[36mfinch setup\x1b[0m\n\n\
        2. Check your config file:\n\
           \x1b[36mcat ~/.finch/config.toml\x1b[0m\n\n\
        3. Verify API key format:\n\
           • Claude: sk-ant-...\n\
           • OpenAI: sk-...\n\
           • Gemini: AI...\n\n\
        4. Get a new API key:\n\
           • Claude: https://console.anthropic.com/\n\
           • OpenAI: https://platform.openai.com/api-keys\n\
           • Google: https://makersuite.google.com/app/apikey",
        provider
    )
}

/// Format a config parse error with helpful suggestions
pub fn config_parse_error(error: &str) -> String {
    format!(
        "Failed to parse config file\n\n\
        \x1b[1;33mError:\x1b[0m {}\n\n\
        \x1b[1;32mTry:\x1b[0m\n\
        1. Check config file syntax:\n\
           \x1b[36mcat ~/.finch/config.toml\x1b[0m\n\n\
        2. Validate TOML format online:\n\
           https://www.toml-lint.com/\n\n\
        3. Backup and regenerate config:\n\
           \x1b[36mmv ~/.finch/config.toml ~/.finch/config.toml.backup\x1b[0m\n\
           \x1b[36mfinch setup\x1b[0m\n\n\
        4. Common mistakes:\n\
           • Missing quotes around strings\n\
           • Unclosed brackets []\n\
           • Invalid TOML syntax",
        error
    )
}

/// Format a file not found error with helpful suggestions
pub fn file_not_found_error(path: &str, description: &str) -> String {
    format!(
        "{} not found: {}\n\n\
        \x1b[1;33mPossible causes:\x1b[0m\n\
        • File has been deleted\n\
        • Wrong path specified\n\
        • Permissions issue\n\n\
        \x1b[1;32mTry:\x1b[0m\n\
        1. Check if file exists:\n\
           \x1b[36mls -la {}\x1b[0m\n\n\
        2. Check parent directory:\n\
           \x1b[36mls -la $(dirname \"{}\")\x1b[0m\n\n\
        3. Verify file permissions:\n\
           \x1b[36mls -l {}\x1b[0m",
        description, path, path, path, path
    )
}

/// Format a permission denied error with helpful suggestions
pub fn permission_denied_error(path: &str, operation: &str) -> String {
    format!(
        "Permission denied: cannot {} {}\n\n\
        \x1b[1;33mPossible causes:\x1b[0m\n\
        • Insufficient file permissions\n\
        • File owned by another user\n\
        • Parent directory not writable\n\n\
        \x1b[1;32mTry:\x1b[0m\n\
        1. Check file permissions:\n\
           \x1b[36mls -la {}\x1b[0m\n\n\
        2. Fix permissions if you own the file:\n\
           \x1b[36mchmod u+rw {}\x1b[0m\n\n\
        3. Check parent directory permissions:\n\
           \x1b[36mls -la $(dirname \"{}\")\x1b[0m",
        operation, path, path, path, path
    )
}

/// Format a daemon already running error
pub fn daemon_already_running_error(pid: u32) -> String {
    format!(
        "Daemon is already running (PID: {})\n\n\
        \x1b[1;32mTo restart the daemon:\x1b[0m\n\
        1. Stop the existing daemon:\n\
           \x1b[36mfinch daemon-stop\x1b[0m\n\n\
        2. Start a new daemon:\n\
           \x1b[36mfinch daemon-start\x1b[0m",
        pid
    )
}

/// Format a model loading error with helpful suggestions
pub fn model_loading_error(model_name: &str, error: &str) -> String {
    format!(
        "Failed to load model '{}'\n\n\
        \x1b[1;33mError:\x1b[0m {}\n\n\
        \x1b[1;33mPossible causes:\x1b[0m\n\
        • Corrupted model files\n\
        • Insufficient RAM\n\
        • Incompatible model format\n\n\
        \x1b[1;32mTry:\x1b[0m\n\
        1. Clear model cache and redownload:\n\
           \x1b[36mrm -rf ~/.cache/huggingface/hub/models--*{}\x1b[0m\n\
           \x1b[36mfinch setup\x1b[0m\n\n\
        2. Check available RAM:\n\
           \x1b[36mfree -h\x1b[0m  (Linux)\n\
           \x1b[36mvm_stat\x1b[0m   (macOS)\n\n\
        3. Try a smaller model:\n\
           • 1.5B models: ~2GB RAM\n\
           • 3B models: ~4GB RAM\n\
           • 7B models: ~8GB RAM",
        model_name, error, model_name
    )
}

/// Wrap a generic error with suggestions
pub fn wrap_error_with_suggestion(error: impl fmt::Display, suggestion: &str) -> String {
    format!(
        "{}\n\n\
        \x1b[1;33m{}:\x1b[0m {}",
        error, t("suggestion"), suggestion
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

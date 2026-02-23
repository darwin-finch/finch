// Live LLM test infrastructure
//
// Live tests verify structural contracts with real API calls. They are:
// - Gated by FINCH_LIVE_TESTS=1 (never run in normal CI)
// - Marked #[ignore] so `cargo test` skips them by default
// - Run with: FINCH_LIVE_TESTS=1 cargo test -- --include-ignored live_
//
// API keys are resolved from env vars first (CI), then ~/.finch/config.toml (local dev).
//
// Structure:
//   tests/live.rs            <- this file (shared helpers + submodule declarations)
//   tests/live/providers.rs  <- per-provider smoke tests
//   tests/live/parity.rs     <- cross-provider behavioral parity tests
//   tests/live/impcpd.rs     <- IMPCPD JSON contract tests

#[path = "live/providers.rs"]
pub mod providers;
#[path = "live/parity.rs"]
pub mod parity;
#[path = "live/impcpd.rs"]
pub mod impcpd;

use finch::config::TeacherEntry;
use finch::providers::LlmProvider;

/// Returns true when live tests should run (FINCH_LIVE_TESTS=1 or =true).
pub fn live_tests_enabled() -> bool {
    std::env::var("FINCH_LIVE_TESTS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Resolve an API key for a named provider.
///
/// Priority:
/// 1. Environment variable (e.g. ANTHROPIC_API_KEY for "claude") — for CI
/// 2. ~/.finch/config.toml — for local development
///
/// Returns `None` if no key is available; tests should skip.
pub fn resolve_api_key(provider: &str) -> Option<String> {
    let env_var = match provider {
        "claude" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "grok" => "XAI_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "groq" => "GROQ_API_KEY",
        _ => return None,
    };

    // 1. Environment variable (CI / explicit override)
    if let Ok(k) = std::env::var(env_var) {
        if !k.trim().is_empty() {
            return Some(k);
        }
    }

    // 2. Config file (local dev)
    if let Ok(cfg) = finch::config::load_config() {
        for p in &cfg.providers {
            if p.provider_type() == provider {
                if let Some(k) = p.api_key() {
                    return Some(k.to_string());
                }
            }
        }
    }

    None
}

/// Create a live `LlmProvider` from a provider name, or `None` to skip.
pub fn make_provider(name: &str) -> Option<Box<dyn LlmProvider>> {
    let key = resolve_api_key(name)?;
    let entry = TeacherEntry {
        provider: name.to_string(),
        api_key: key,
        model: None,
        base_url: None,
        name: None,
    };
    finch::providers::create_provider_from_teacher(&entry).ok()
}

/// Return all providers for which an API key is available.
pub fn all_available_providers() -> Vec<(&'static str, Box<dyn LlmProvider>)> {
    ["claude", "openai", "grok", "gemini", "mistral", "groq"]
        .iter()
        .filter_map(|&name| make_provider(name).map(|p| (name, p)))
        .collect()
}

#[cfg(test)]
mod infra_tests {
    use super::*;

    #[test]
    fn test_resolve_api_key_unknown_provider_returns_none() {
        assert!(resolve_api_key("nonexistent_provider_xyz").is_none());
    }

    #[test]
    fn test_live_tests_enabled_returns_bool_without_panic() {
        let _ = live_tests_enabled();
    }

    #[test]
    fn test_make_provider_unknown_returns_none() {
        assert!(make_provider("nonexistent_xyz").is_none());
    }
}

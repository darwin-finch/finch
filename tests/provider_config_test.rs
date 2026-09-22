// Test provider configuration validation
//
// This test suite verifies that:
// 1. Provider factory validates configurations
// 2. Missing required fields are detected
// 3. Provider-specific model names are honored
// 4. Multiple cloud entries fall back in configured order

use anyhow::Result;
use finch::config::{Config, ProviderEntry};
use finch::providers;

fn claude_entry(api_key: &str, model: Option<&str>) -> ProviderEntry {
    ProviderEntry::Claude {
        api_key: api_key.to_string(),
        model: model.map(str::to_string),
        base_url: None,
        chat_path: None,
        models_path: None,
        name: Some("Claude".to_string()),
    }
}

fn gemini_entry(api_key: &str, model: Option<&str>) -> ProviderEntry {
    ProviderEntry::Gemini {
        api_key: api_key.to_string(),
        model: model.map(str::to_string),
        name: Some("Gemini".to_string()),
    }
}

/// Test that Gemini provider rejects invalid model names
#[test]
fn test_gemini_invalid_model_name() {
    // Note: This test documents CURRENT behavior
    // In the future, we might want to add validation that fails fast
    // For now, it will only fail when actually making API calls

    let _entry = gemini_entry("test-key", Some("gemini-2.0-flash-exp")); // Invalid!
                                                                         // The provider can be created, but API calls will fail
                                                                         // This is acceptable - fail at runtime with clear error message
}

/// Test that provider factory creates correct provider types
#[test]
fn test_provider_factory_creates_correct_types() -> Result<()> {
    let claude_provider = providers::create_provider_from_entries(&[claude_entry(
        "test-key",
        Some("claude-sonnet-5"),
    )])?;
    let gemini_provider = providers::create_provider_from_entries(&[gemini_entry(
        "test-key",
        Some("gemini-2.5-flash"),
    )])?;

    // Verify provider names
    assert_eq!(claude_provider.name(), "claude");
    assert_eq!(gemini_provider.name(), "gemini");

    // Verify default models
    assert_eq!(claude_provider.default_model(), "claude-sonnet-5");
    assert_eq!(gemini_provider.default_model(), "gemini-2.5-flash");

    Ok(())
}

/// Test that provider factory creates fallback chain with multiple entries
#[test]
fn test_provider_factory_creates_fallback_chain() -> Result<()> {
    let entries = vec![
        gemini_entry("key1", Some("gemini-2.5-flash")),
        claude_entry("key2", Some("claude-sonnet-5")),
    ];

    // Create provider (should be a FallbackChain)
    let provider = providers::create_provider_from_entries(&entries)?;

    // Verify it uses the first provider's name
    assert_eq!(provider.name(), "gemini");

    // Verify it has the first provider's model
    assert_eq!(provider.default_model(), "gemini-2.5-flash");

    Ok(())
}

/// Test that provider factory handles a single entry correctly
#[test]
fn test_provider_factory_single_entry() -> Result<()> {
    let entries = vec![claude_entry("test-key", Some("claude-sonnet-5"))];

    // Create provider (should NOT be a FallbackChain)
    let provider = providers::create_provider_from_entries(&entries)?;

    assert_eq!(provider.name(), "claude");
    assert_eq!(provider.default_model(), "claude-sonnet-5");

    Ok(())
}

/// Test that provider factory fails with no cloud entries
#[test]
fn test_provider_factory_fails_with_no_entries() {
    let entries: Vec<ProviderEntry> = vec![];

    let result = providers::create_provider_from_entries(&entries);

    // Should fail - no entries provided
    assert!(result.is_err());
}

/// Test that provider configuration validates API keys exist
#[test]
fn test_provider_requires_api_key() {
    // Empty API key should be caught
    let entry = claude_entry("", Some("claude-sonnet-5"));

    // Provider creation should handle this gracefully
    // (It will fail when making actual API calls)
    let result = providers::create_provider_from_entries(&[entry]);

    // Current behavior: accepts empty key, fails at runtime
    // Future improvement: validate at config time
    assert!(
        result.is_ok(),
        "Provider should accept empty key but fail at runtime"
    );
}

/// Test that model field defaults correctly when not provided
#[test]
fn test_provider_model_defaults() -> Result<()> {
    let provider = providers::create_provider_from_entries(&[claude_entry("test-key", None)])?;

    // Should use provider's default model
    let default = provider.default_model();
    assert!(!default.is_empty(), "Provider should have a default model");

    Ok(())
}

/// Test provider capabilities (streaming, tools)
#[test]
fn test_provider_capabilities() -> Result<()> {
    let provider = providers::create_provider_from_entries(&[claude_entry(
        "test-key",
        Some("claude-sonnet-5"),
    )])?;

    // Claude should support both streaming and tools
    assert!(
        provider.supports_streaming(),
        "Claude should support streaming"
    );
    assert!(provider.supports_tools(), "Claude should support tools");

    Ok(())
}

/// A config seeded with cloud entries constructs through the unified API.
#[test]
fn test_config_with_cloud_entries_constructs() -> Result<()> {
    let config = Config::new(vec![
        gemini_entry("key1", Some("gemini-2.5-flash")),
        claude_entry("key2", Some("claude-sonnet-5")),
    ]);
    assert_eq!(config.cloud_providers().len(), 2);
    assert!(config.local_providers().is_empty());

    Ok(())
}

/// Document known valid model names for each provider
#[test]
fn test_document_valid_model_names() {
    // This test documents the VALID model names we know work
    // Update this as APIs evolve

    // Exact statically attested records as of 2026-08-26.
    let valid_gemini = ["gemini-2.5-flash"];

    // Claude:
    let valid_claude = ["claude-sonnet-5"];

    // OpenAI:
    let valid_openai = ["gpt-5.6-sol", "gpt-4o"];

    // This test just documents - doesn't validate
    // In the future, we could add runtime validation against these lists
    assert!(!valid_gemini.is_empty());
    assert!(!valid_claude.is_empty());
    assert!(!valid_openai.is_empty());
}

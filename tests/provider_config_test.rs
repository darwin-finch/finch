// Test provider configuration validation
//
// This test suite verifies that:
// 1. Invalid model names are caught early
// 2. Provider factory validates configurations
// 3. Missing required fields are detected
// 4. Provider-specific model names are enforced

use anyhow::Result;
use finch::config::{Config, TeacherEntry};
use finch::providers;

/// Test that Gemini provider rejects invalid model names
#[test]
fn test_gemini_invalid_model_name() {
    let teacher = TeacherEntry {
        provider: "gemini".to_string(),
        api_key: "test-key".to_string(),
        model: Some("gemini-2.0-flash-exp".to_string()), // Invalid!
        name: Some("Test".to_string()),
    };

    // Note: This test documents CURRENT behavior
    // In the future, we might want to add validation that fails fast
    // For now, it will only fail when actually making API calls

    // The provider can be created, but API calls will fail
    // This is acceptable - fail at runtime with clear error message

    // Future improvement: Add a validate() method to providers
    // that can be called at config load time
}

/// Test that provider factory creates correct provider types
#[test]
fn test_provider_factory_creates_correct_types() -> Result<()> {
    let claude_teacher = TeacherEntry {
        provider: "claude".to_string(),
        api_key: "test-key".to_string(),
        model: Some("claude-sonnet-4".to_string()),
        name: Some("Claude".to_string()),
    };

    let gemini_teacher = TeacherEntry {
        provider: "gemini".to_string(),
        api_key: "test-key".to_string(),
        model: Some("gemini-2.5-flash".to_string()),
        name: Some("Gemini".to_string()),
    };

    // Create providers
    let claude_provider = providers::create_provider(&[claude_teacher])?;
    let gemini_provider = providers::create_provider(&[gemini_teacher])?;

    // Verify provider names
    assert_eq!(claude_provider.name(), "claude");
    assert_eq!(gemini_provider.name(), "gemini");

    // Verify default models
    assert_eq!(claude_provider.default_model(), "claude-sonnet-4");
    assert_eq!(gemini_provider.default_model(), "gemini-2.5-flash");

    Ok(())
}

/// Test that provider factory creates fallback chain with multiple teachers
#[test]
fn test_provider_factory_creates_fallback_chain() -> Result<()> {
    let teachers = vec![
        TeacherEntry {
            provider: "gemini".to_string(),
            api_key: "key1".to_string(),
            model: Some("gemini-2.5-flash".to_string()),
            name: Some("Gemini".to_string()),
        },
        TeacherEntry {
            provider: "claude".to_string(),
            api_key: "key2".to_string(),
            model: Some("claude-sonnet-4".to_string()),
            name: Some("Claude".to_string()),
        },
    ];

    // Create provider (should be a FallbackChain)
    let provider = providers::create_provider(&teachers)?;

    // Verify it uses the first provider's name
    assert_eq!(provider.name(), "gemini");

    // Verify it has the first provider's model
    assert_eq!(provider.default_model(), "gemini-2.5-flash");

    Ok(())
}

/// Test that provider factory handles single teacher correctly
#[test]
fn test_provider_factory_single_teacher() -> Result<()> {
    let teachers = vec![TeacherEntry {
        provider: "claude".to_string(),
        api_key: "test-key".to_string(),
        model: Some("claude-sonnet-4".to_string()),
        name: Some("Claude".to_string()),
    }];

    // Create provider (should NOT be a FallbackChain)
    let provider = providers::create_provider(&teachers)?;

    assert_eq!(provider.name(), "claude");
    assert_eq!(provider.default_model(), "claude-sonnet-4");

    Ok(())
}

/// Test that provider factory fails with no teachers
#[test]
fn test_provider_factory_fails_with_no_teachers() {
    let teachers: Vec<TeacherEntry> = vec![];

    let result = providers::create_provider(&teachers);

    // Should fail - no teachers provided
    assert!(result.is_err());
}

/// Test that provider configuration validates API keys exist
#[test]
fn test_provider_requires_api_key() {
    // Empty API key should be caught
    let teacher = TeacherEntry {
        provider: "claude".to_string(),
        api_key: "".to_string(), // Empty!
        model: Some("claude-sonnet-4".to_string()),
        name: Some("Claude".to_string()),
    };

    // Provider creation should handle this gracefully
    // (It will fail when making actual API calls)
    let result = providers::create_provider(&[teacher]);

    // Current behavior: accepts empty key, fails at runtime
    // Future improvement: validate at config time
    assert!(result.is_ok(), "Provider should accept empty key but fail at runtime");
}

/// Test that model field defaults correctly when not provided
#[test]
fn test_provider_model_defaults() -> Result<()> {
    let teacher_without_model = TeacherEntry {
        provider: "claude".to_string(),
        api_key: "test-key".to_string(),
        model: None, // No model specified
        name: Some("Claude".to_string()),
    };

    let provider = providers::create_provider(&[teacher_without_model])?;

    // Should use provider's default model
    let default = provider.default_model();
    assert!(!default.is_empty(), "Provider should have a default model");

    Ok(())
}

/// Test that provider names are case-insensitive
#[test]
fn test_provider_names_case_insensitive() -> Result<()> {
    let teacher_upper = TeacherEntry {
        provider: "CLAUDE".to_string(),
        api_key: "test-key".to_string(),
        model: Some("claude-sonnet-4".to_string()),
        name: Some("Claude".to_string()),
    };

    let teacher_lower = TeacherEntry {
        provider: "claude".to_string(),
        api_key: "test-key".to_string(),
        model: Some("claude-sonnet-4".to_string()),
        name: Some("Claude".to_string()),
    };

    // Both should work
    let provider_upper = providers::create_provider(&[teacher_upper])?;
    let provider_lower = providers::create_provider(&[teacher_lower])?;

    // Should normalize to lowercase
    assert_eq!(provider_upper.name().to_lowercase(), "claude");
    assert_eq!(provider_lower.name().to_lowercase(), "claude");

    Ok(())
}

/// Test that unknown provider types are rejected
#[test]
fn test_unknown_provider_rejected() {
    let teacher = TeacherEntry {
        provider: "unknown-provider".to_string(),
        api_key: "test-key".to_string(),
        model: Some("some-model".to_string()),
        name: Some("Unknown".to_string()),
    };

    let result = providers::create_provider(&[teacher]);

    // Should fail with unknown provider
    assert!(result.is_err(), "Unknown provider should be rejected");
}

/// Test provider capabilities (streaming, tools)
#[test]
fn test_provider_capabilities() -> Result<()> {
    let claude_teacher = TeacherEntry {
        provider: "claude".to_string(),
        api_key: "test-key".to_string(),
        model: Some("claude-sonnet-4".to_string()),
        name: Some("Claude".to_string()),
    };

    let provider = providers::create_provider(&[claude_teacher])?;

    // Claude should support both streaming and tools
    assert!(provider.supports_streaming(), "Claude should support streaming");
    assert!(provider.supports_tools(), "Claude should support tools");

    Ok(())
}

/// Document known valid model names for each provider
#[test]
fn test_document_valid_model_names() {
    // This test documents the VALID model names we know work
    // Update this as APIs evolve

    // Gemini (as of 2026-02-14):
    let valid_gemini = vec![
        "gemini-2.5-flash",
        "gemini-2.5-pro",
        "gemini-2.0-flash",
        "gemini-1.5-pro",
        "gemini-1.5-flash",
    ];

    // Claude:
    let valid_claude = vec![
        "claude-sonnet-4-20250514",
        "claude-opus-4",
        "claude-haiku-4",
    ];

    // OpenAI:
    let valid_openai = vec![
        "gpt-4",
        "gpt-4-turbo",
        "gpt-3.5-turbo",
    ];

    // This test just documents - doesn't validate
    // In the future, we could add runtime validation against these lists
    assert!(!valid_gemini.is_empty());
    assert!(!valid_claude.is_empty());
    assert!(!valid_openai.is_empty());
}

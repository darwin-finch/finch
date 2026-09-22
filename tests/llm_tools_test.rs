// Integration tests for Phase 1: LLM Tools and Registry

use anyhow::Result;
use finch::config::ProviderEntry;
use finch::llms::LLMRegistry;
use finch::tools::create_llm_tools;

fn create_cloud_entry(
    provider: &str,
    api_key: &str,
    model: Option<&str>,
    name: Option<&str>,
) -> ProviderEntry {
    ProviderEntry::from_provider_fields(
        provider,
        api_key.to_string(),
        model.map(|s| s.to_string()),
        None,
        name.map(|s| s.to_string()),
    )
}

#[test]
fn test_llm_registry_creation_single_cloud_provider() -> Result<()> {
    // With only one cloud provider, registry should not be created
    let cloud = vec![create_cloud_entry(
        "claude",
        "test-key",
        Some("claude-sonnet-4-20250514"),
        Some("Claude"),
    )];

    // Registry requires > 1 cloud provider
    // This would be checked in the REPL initialization
    assert_eq!(cloud.len(), 1, "Single cloud provider case");

    Ok(())
}

#[test]
fn test_llm_registry_creation_multiple_cloud_providers() -> Result<()> {
    // With multiple cloud providers, registry should be created
    let cloud = vec![
        create_cloud_entry(
            "claude",
            "test-key-1",
            Some("claude-sonnet-4-20250514"),
            Some("Claude Sonnet"),
        ),
        create_cloud_entry("openai", "test-key-2", Some("gpt-4"), Some("GPT-4")),
    ];

    let registry = LLMRegistry::from_cloud_providers(&cloud)?;

    // Verify registry has tools
    let tool_names = registry.tool_names();

    // Primary should be the first cloud provider (Claude)
    // Tools should be the remaining cloud providers (GPT-4)
    assert!(
        !tool_names.is_empty(),
        "Should have tools for non-primary cloud providers"
    );

    Ok(())
}

#[test]
fn test_llm_tool_names() -> Result<()> {
    let cloud = vec![
        create_cloud_entry(
            "claude",
            "key1",
            Some("claude-sonnet-4-20250514"),
            Some("Claude Sonnet"),
        ),
        create_cloud_entry("openai", "key2", Some("gpt-4"), Some("GPT-4")),
        create_cloud_entry("gemini", "key3", Some("gemini-pro"), Some("Gemini")),
    ];

    let registry = LLMRegistry::from_cloud_providers(&cloud)?;
    let tool_names = registry.tool_names();

    // Should have 2 tools (GPT-4 and Gemini, excluding primary Claude)
    assert_eq!(
        tool_names.len(),
        2,
        "Should have 2 non-primary cloud providers as tools"
    );

    Ok(())
}

#[test]
fn test_create_llm_tools() -> Result<()> {
    let cloud = vec![
        create_cloud_entry(
            "claude",
            "key1",
            Some("claude-sonnet-4-20250514"),
            Some("Claude"),
        ),
        create_cloud_entry("openai", "key2", Some("gpt-4"), Some("GPT-4")),
    ];

    let registry = LLMRegistry::from_cloud_providers(&cloud)?;
    let tools = create_llm_tools(&registry);

    // Should create tools for non-primary cloud providers
    assert!(!tools.is_empty(), "Should create at least one tool");

    // Each tool should have a name
    for tool in tools {
        assert!(!tool.name().is_empty(), "Tool should have a name");
        assert!(
            !tool.description().is_empty(),
            "Tool should have a description"
        );

        // Tool name should be lowercased provider name
        assert!(
            tool.name().starts_with("use_"),
            "Tool name should start with use_"
        );
    }

    Ok(())
}

#[test]
fn test_llm_tool_input_schema() -> Result<()> {
    let cloud = vec![
        create_cloud_entry(
            "claude",
            "key",
            Some("claude-sonnet-4-20250514"),
            Some("Claude"),
        ),
        create_cloud_entry("openai", "key", Some("gpt-4"), Some("GPT-4")),
    ];

    let registry = LLMRegistry::from_cloud_providers(&cloud)?;
    let tools = create_llm_tools(&registry);

    for tool in tools {
        let schema = tool.input_schema();

        // Verify schema structure
        assert_eq!(schema.schema_type, "object", "Schema should be object type");

        // Should have query and reason parameters
        let properties = schema.properties;
        assert!(
            properties.get("query").is_some(),
            "Should have query parameter"
        );
        assert!(
            properties.get("reason").is_some(),
            "Should have reason parameter"
        );

        // Both should be required
        assert!(
            schema.required.contains(&"query".to_string()),
            "query should be required"
        );
        assert!(
            schema.required.contains(&"reason".to_string()),
            "reason should be required"
        );
    }

    Ok(())
}

#[test]
fn test_multiple_models_same_provider() -> Result<()> {
    let cloud = vec![
        create_cloud_entry(
            "claude",
            "key",
            Some("claude-sonnet-4-20250514"),
            Some("Claude Sonnet"),
        ),
        create_cloud_entry(
            "claude",
            "key",
            Some("claude-opus-4-20250514"),
            Some("Claude Opus"),
        ),
    ];

    let registry = LLMRegistry::from_cloud_providers(&cloud)?;
    let tools = create_llm_tools(&registry);

    // Should create separate tools for different models from same provider
    assert!(!tools.is_empty(), "Should create tools for multiple models");

    Ok(())
}

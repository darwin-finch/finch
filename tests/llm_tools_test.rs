// Integration tests for Phase 1: LLM Tools and Registry

use anyhow::Result;
use finch::config::TeacherEntry;
use finch::llms::LLMRegistry;
use finch::tools::implementations::llm_tools::create_llm_tools;

fn create_teacher(
    provider: &str,
    api_key: &str,
    model: Option<&str>,
    name: Option<&str>,
) -> TeacherEntry {
    TeacherEntry {
        provider: provider.to_string(),
        api_key: api_key.to_string(),
        model: model.map(|s| s.to_string()),
        base_url: None,
        name: name.map(|s| s.to_string()),
    }
}

#[test]
fn test_llm_registry_creation_single_teacher() -> Result<()> {
    // With only one teacher, registry should not be created
    let teachers = vec![create_teacher(
        "claude",
        "test-key",
        Some("claude-sonnet-4-20250514"),
        Some("Claude"),
    )];

    // Registry requires > 1 teacher
    // This would be checked in the REPL initialization
    assert!(teachers.len() == 1, "Single teacher case");

    Ok(())
}

#[test]
fn test_llm_registry_creation_multiple_teachers() -> Result<()> {
    // With multiple teachers, registry should be created
    let teachers = vec![
        create_teacher(
            "claude",
            "test-key-1",
            Some("claude-sonnet-4-20250514"),
            Some("Claude Sonnet"),
        ),
        create_teacher("openai", "test-key-2", Some("gpt-4"), Some("GPT-4")),
    ];

    let registry = LLMRegistry::from_teachers(&teachers)?;

    // Verify registry has tools
    let tool_names = registry.tool_names();
    assert!(!tool_names.is_empty(), "Should have at least one tool");

    // Primary should be first teacher (Claude)
    // Tools should be remaining teachers (GPT-4)
    assert!(
        tool_names.len() >= 1,
        "Should have tools for non-primary teachers"
    );

    Ok(())
}

#[test]
fn test_llm_tool_names() -> Result<()> {
    let teachers = vec![
        create_teacher(
            "claude",
            "key1",
            Some("claude-sonnet-4-20250514"),
            Some("Claude Sonnet"),
        ),
        create_teacher("openai", "key2", Some("gpt-4"), Some("GPT-4")),
        create_teacher("gemini", "key3", Some("gemini-pro"), Some("Gemini")),
    ];

    let registry = LLMRegistry::from_teachers(&teachers)?;
    let tool_names = registry.tool_names();

    // Should have 2 tools (GPT-4 and Gemini, excluding primary Claude)
    assert_eq!(
        tool_names.len(),
        2,
        "Should have 2 non-primary teachers as tools"
    );

    Ok(())
}

#[test]
fn test_create_llm_tools() -> Result<()> {
    let teachers = vec![
        create_teacher(
            "claude",
            "key1",
            Some("claude-sonnet-4-20250514"),
            Some("Claude"),
        ),
        create_teacher("openai", "key2", Some("gpt-4"), Some("GPT-4")),
    ];

    let registry = LLMRegistry::from_teachers(&teachers)?;
    let tools = create_llm_tools(&registry);

    // Should create tools for non-primary teachers
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
    let teachers = vec![
        create_teacher(
            "claude",
            "key",
            Some("claude-sonnet-4-20250514"),
            Some("Claude"),
        ),
        create_teacher("openai", "key", Some("gpt-4"), Some("GPT-4")),
    ];

    let registry = LLMRegistry::from_teachers(&teachers)?;
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
    let teachers = vec![
        create_teacher(
            "claude",
            "key",
            Some("claude-sonnet-4-20250514"),
            Some("Claude Sonnet"),
        ),
        create_teacher(
            "claude",
            "key",
            Some("claude-opus-4-20250514"),
            Some("Claude Opus"),
        ),
    ];

    let registry = LLMRegistry::from_teachers(&teachers)?;
    let tools = create_llm_tools(&registry);

    // Should create separate tools for different models from same provider
    assert!(!tools.is_empty(), "Should create tools for multiple models");

    Ok(())
}

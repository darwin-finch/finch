// Integration tests for Phase 2: Persona System

use finch::config::Persona;
use anyhow::Result;

#[test]
fn test_load_builtin_personas() -> Result<()> {
    // Test that all 6 builtin personas can be loaded
    let personas = vec!["default", "expert-coder", "teacher", "analyst", "creative", "researcher"];

    for persona_name in personas {
        let persona = Persona::load_builtin(persona_name)?;
        // Name in file might have spaces (e.g., "Expert Coder" vs "expert-coder")
        assert!(!persona.name().is_empty(), "Name should not be empty for {}", persona_name);
        assert!(!persona.behavior.system_prompt.is_empty(), "System prompt should not be empty for {}", persona_name);
        assert!(!persona.tone().is_empty(), "Tone should be set for {}", persona_name);
        assert!(!persona.verbosity().is_empty(), "Verbosity should be set for {}", persona_name);
        assert!(!persona.focus().is_empty(), "Focus should be set for {}", persona_name);
    }

    Ok(())
}

#[test]
fn test_persona_system_message() -> Result<()> {
    let persona = Persona::load_builtin("default")?;
    let system_message = persona.to_system_message();

    // System message should be the system prompt text
    assert_eq!(system_message, persona.behavior.system_prompt);
    assert!(!system_message.is_empty());

    Ok(())
}

#[test]
fn test_persona_examples() -> Result<()> {
    // Some personas have examples
    let expert_coder = Persona::load_builtin("expert-coder")?;
    let analyst = Persona::load_builtin("analyst")?;

    // Verify example structure (if they exist)
    for example in &expert_coder.behavior.examples {
        assert!(!example.user.is_empty(), "User example should not be empty");
        assert!(!example.assistant.is_empty(), "Assistant example should not be empty");
    }

    for example in &analyst.behavior.examples {
        assert!(!example.user.is_empty(), "User example should not be empty");
        assert!(!example.assistant.is_empty(), "Assistant example should not be empty");
    }

    Ok(())
}

#[test]
fn test_invalid_persona_name() {
    // Should return error for non-existent persona
    let result = Persona::load_builtin("nonexistent-persona");
    assert!(result.is_err(), "Should fail to load non-existent persona");
}

#[test]
fn test_persona_default() {
    // Test Default trait implementation
    let persona = Persona::default();
    assert!(!persona.name().is_empty());
    assert!(!persona.behavior.system_prompt.is_empty());
}

#[test]
fn test_all_personas_have_unique_characteristics() -> Result<()> {
    // Verify that each persona has distinct characteristics
    let personas = vec!["default", "expert-coder", "teacher", "analyst", "creative", "researcher"];
    let mut prompts = std::collections::HashSet::new();

    for persona_name in personas {
        let persona = Persona::load_builtin(persona_name)?;

        // Each persona should have a unique system prompt
        assert!(
            prompts.insert(persona.behavior.system_prompt.clone()),
            "Persona {} has duplicate system prompt",
            persona_name
        );
    }

    Ok(())
}

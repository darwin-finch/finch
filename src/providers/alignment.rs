// Universal alignment prompt — normalizes LLM output behavior across all providers.
//
// Different LLM providers (Claude, GPT-4, Grok, Gemini, Mistral, Groq) have different
// default styles. The universal alignment prompt enforces consistent structural behavior
// so that Finch can swap to the cheapest available provider without breaking features
// that depend on structured output (e.g. IMPCPD critique JSON, numbered plan steps).

/// Prompt that normalizes output discipline across all LLM providers.
///
/// Prepend this to any `system` prompt (or embed in the user message when a
/// system-level override is not available) before sending a structured-output request.
pub const UNIVERSAL_ALIGNMENT_PROMPT: &str = "\
## Output Discipline

These rules override any stylistic defaults:

1. When asked for JSON, return ONLY the JSON. No markdown code fences. No prose before \
or after. The first character of your response must be `[` or `{`.
2. When given a numbered format (1. Step one\n2. Step two), follow it exactly.
3. When given field names or schema, use them verbatim — no renaming, no extras.
4. Do not add unsolicited caveats, disclaimers, or explanations unless the instruction \
explicitly requests them.
5. Treat every instruction as binding, not advisory.";

/// Inject the alignment prompt into an existing system prompt, or return it standalone.
///
/// The alignment instructions are prepended so they take priority over any other
/// stylistic context in the system prompt.
pub fn with_alignment(system: Option<&str>) -> String {
    match system {
        Some(existing) if !existing.trim().is_empty() => {
            format!("{}\n\n{}", UNIVERSAL_ALIGNMENT_PROMPT.trim(), existing)
        }
        _ => UNIVERSAL_ALIGNMENT_PROMPT.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_with_alignment_no_system() {
        let result = with_alignment(None);
        assert!(result.contains("Output Discipline"));
        assert!(result.starts_with("## Output Discipline"));
    }

    #[test]
    fn test_with_alignment_empty_system() {
        let result = with_alignment(Some(""));
        // Empty system treated same as None — just the alignment prompt, no extra suffix
        assert!(result.contains("Output Discipline"));
        assert_eq!(result, UNIVERSAL_ALIGNMENT_PROMPT.trim());
    }

    #[test]
    fn test_with_alignment_prepends_to_existing() {
        let result = with_alignment(Some("Be a helpful assistant."));
        assert!(result.starts_with("## Output Discipline"));
        assert!(result.contains("Be a helpful assistant."));
        // Alignment comes first
        let align_pos = result.find("Output Discipline").unwrap();
        let system_pos = result.find("Be a helpful").unwrap();
        assert!(align_pos < system_pos);
    }

    #[test]
    fn test_with_alignment_whitespace_only_system() {
        let result = with_alignment(Some("   \n  "));
        // Whitespace-only treated same as None
        assert!(result.starts_with("## Output Discipline"));
    }

    #[test]
    fn test_universal_alignment_prompt_has_json_rule() {
        assert!(UNIVERSAL_ALIGNMENT_PROMPT.contains("JSON"));
        assert!(UNIVERSAL_ALIGNMENT_PROMPT.contains("code fences"));
    }

    #[test]
    fn test_universal_alignment_prompt_has_numbered_format_rule() {
        assert!(UNIVERSAL_ALIGNMENT_PROMPT.contains("numbered format"));
    }
}

// Mistral Model Adapter
//
// Handles Mistral-specific chat template and token IDs.
// Mistral models: Mistral 7B, Mixtral 8x7B, Mistral Small/Medium/Large

use super::{GenerationConfig, LocalModelAdapter};

/// Adapter for Mistral model family
pub struct MistralAdapter;

impl LocalModelAdapter for MistralAdapter {
    fn format_chat_prompt(&self, system: &str, user_message: &str) -> String {
        // Mistral instruction format (no explicit system role)
        // System message is prepended to user message
        // Reference: https://docs.mistral.ai/guides/prompting_capabilities/
        format!("<s>[INST] {}\n\n{} [/INST]", system, user_message)
    }

    fn eos_token_id(&self) -> u32 {
        // Mistral EOS token ID
        2
    }

    fn bos_token_id(&self) -> Option<u32> {
        // Mistral BOS token ID (represented as <s> in template)
        Some(1)
    }

    fn clean_output(&self, raw_output: &str) -> String {
        // Remove Mistral template markers and trailing whitespace
        let cleaned = raw_output
            .split("</s>")
            .next()
            .unwrap_or(raw_output)
            .split("[/INST]")
            .last()
            .unwrap_or(raw_output)
            .trim()
            .to_string();

        // Remove any instruction markers that might have been generated
        if cleaned.starts_with("[INST]") || cleaned.starts_with("<s>") {
            cleaned
                .trim_start_matches("[INST]")
                .trim_start_matches("<s>")
                .trim()
                .to_string()
        } else {
            cleaned
        }
    }

    fn family_name(&self) -> &str {
        "Mistral"
    }

    fn generation_config(&self) -> GenerationConfig {
        GenerationConfig {
            temperature: 0.7,
            top_p: 0.95,
            top_k: 50,
            repetition_penalty: 1.0,
            max_tokens: 512,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mistral_format() {
        let adapter = MistralAdapter;
        let prompt = adapter.format_chat_prompt("You are a helpful assistant.", "What is 2+2?");

        assert!(prompt.starts_with("<s>[INST]"));
        assert!(prompt.contains("You are a helpful assistant."));
        assert!(prompt.contains("What is 2+2?"));
        assert!(prompt.ends_with("[/INST]"));
    }

    #[test]
    fn test_mistral_clean_output() {
        let adapter = MistralAdapter;

        // Test cleaning with end marker
        let raw = "The answer is 4</s>";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(cleaned, "The answer is 4");

        // Test cleaning instruction markers
        let raw2 = "[INST] should be removed [/INST] The answer is 4";
        let cleaned2 = adapter.clean_output(raw2);
        assert_eq!(cleaned2, "The answer is 4");

        // Test no markers
        let raw3 = "Just a response";
        let cleaned3 = adapter.clean_output(raw3);
        assert_eq!(cleaned3, "Just a response");
    }

    #[test]
    fn test_mistral_token_ids() {
        let adapter = MistralAdapter;
        assert_eq!(adapter.eos_token_id(), 2);
        assert_eq!(adapter.bos_token_id(), Some(1));
    }
}

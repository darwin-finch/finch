// Gemma Model Adapter
//
// Handles Gemma-specific chat template and token IDs.
// Gemma models: Gemma 2 2B, 9B, 27B (instruction-tuned)
//
// Gemma's chat template is NOT Llama's. It has no system role, uses
// `<start_of_turn>`/`<end_of_turn>` turn markers instead of Llama 3's
// `<|start_header_id|>`/`<|eot_id|>`, and its literal BOS marker is `<bos>`
// (Llama 3's is `<|begin_of_text|>`; Llama 2/Mistral's is `<s>`). None of
// Llama's special-token text exists in Gemma's GGUF vocabulary, so routing
// Gemma through `LlamaAdapter` produces a prompt that tokenizes as garbage.
// Reference: https://ai.google.dev/gemma/docs/core/prompt-structure and the
// `google/gemma-2-9b-it` tokenizer_config.json chat template, both of which
// give `<bos><start_of_turn>user\n{content}<end_of_turn>\n<start_of_turn>model\n`.

use super::{GenerationConfig, LocalModelAdapter};

/// Adapter for Gemma model family (Gemma 2 turn format)
pub struct GemmaAdapter;

impl LocalModelAdapter for GemmaAdapter {
    fn format_chat_prompt(&self, system: &str, user_message: &str) -> String {
        // Gemma has no dedicated system role in its chat template. Google's
        // documented guidance is to fold any system instructions into the
        // leading user turn.
        // Reference: https://ai.google.dev/gemma/docs/core/prompt-structure
        let user_turn = if system.is_empty() {
            user_message.to_string()
        } else {
            format!("{system}\n\n{user_message}")
        };
        format!("<bos><start_of_turn>user\n{user_turn}<end_of_turn>\n<start_of_turn>model\n")
    }

    fn eos_token_id(&self) -> u32 {
        // Gemma 2 instruction-tuned chat stops on <end_of_turn>, not the base
        // <eos> (1); mirrors LlamaAdapter using <|eot_id|> rather than
        // <|end_of_text|>.
        107
    }

    fn bos_token_id(&self) -> Option<u32> {
        // Gemma's BOS token ID (represented literally as <bos> in the template).
        Some(2)
    }

    fn clean_output(&self, raw_output: &str) -> String {
        // Remove Gemma template markers and trailing whitespace.
        let cleaned = raw_output
            .split("<end_of_turn>")
            .next()
            .unwrap_or(raw_output)
            .split("<eos>")
            .next()
            .unwrap_or(raw_output)
            .trim()
            .to_string();

        // Remove any turn markers that might have been generated/echoed.
        if let Some(rest) = cleaned.strip_prefix("<start_of_turn>model") {
            rest.trim_start_matches('\n').trim().to_string()
        } else {
            cleaned
        }
    }

    fn family_name(&self) -> &str {
        "Gemma"
    }

    fn generation_config(&self) -> GenerationConfig {
        GenerationConfig {
            temperature: 0.7,
            top_p: 0.95,
            top_k: 64,
            repetition_penalty: 1.0,
            max_tokens: 512,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gemma_format_uses_real_gemma_tokens_not_llama() {
        let adapter = GemmaAdapter;
        let prompt = adapter.format_chat_prompt("You are a helpful assistant.", "What is 2+2?");

        assert!(
            prompt.starts_with("<bos><start_of_turn>user\n"),
            "prompt must open with Gemma's literal BOS marker and user turn: {prompt:?}"
        );
        assert!(prompt.contains("You are a helpful assistant."));
        assert!(prompt.contains("What is 2+2?"));
        assert!(prompt.contains("<end_of_turn>\n<start_of_turn>model\n"));
        assert!(
            prompt.ends_with("<start_of_turn>model\n"),
            "prompt must end ready for the model's turn: {prompt:?}"
        );

        // Never emit Llama's special tokens; they don't exist in Gemma's vocab.
        assert!(!prompt.contains("<|begin_of_text|>"));
        assert!(!prompt.contains("<|start_header_id|>"));
        assert!(!prompt.contains("<|eot_id|>"));
    }

    #[test]
    fn test_gemma_format_without_system_omits_blank_turn() {
        let adapter = GemmaAdapter;
        let prompt = adapter.format_chat_prompt("", "Hello");
        assert_eq!(
            prompt,
            "<bos><start_of_turn>user\nHello<end_of_turn>\n<start_of_turn>model\n"
        );
    }

    #[test]
    fn test_gemma_clean_output() {
        let adapter = GemmaAdapter;

        let raw = "The answer is 4<end_of_turn>";
        assert_eq!(adapter.clean_output(raw), "The answer is 4");

        let raw2 = "Response here<eos>";
        assert_eq!(adapter.clean_output(raw2), "Response here");

        let raw3 = "<start_of_turn>model\nEchoed turn marker<end_of_turn>";
        assert_eq!(adapter.clean_output(raw3), "Echoed turn marker");

        let raw4 = "Just a response";
        assert_eq!(adapter.clean_output(raw4), "Just a response");
    }

    #[test]
    fn test_gemma_token_ids() {
        let adapter = GemmaAdapter;
        assert_eq!(adapter.eos_token_id(), 107);
        assert_eq!(adapter.bos_token_id(), Some(2));
    }

    #[test]
    fn test_gemma_family_name() {
        assert_eq!(GemmaAdapter.family_name(), "Gemma");
    }
}

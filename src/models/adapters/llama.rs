// Llama Model Adapter
//
// Handles Llama-specific chat template and token IDs.
// Llama models: Llama 3, Llama 3.1, Llama 3.2

use super::{LocalModelAdapter, GenerationConfig};

/// Adapter for Llama model family (Llama 3+ format)
pub struct LlamaAdapter;

impl LocalModelAdapter for LlamaAdapter {
    fn format_chat_prompt(&self, system: &str, user_message: &str) -> String {
        // Llama 3 chat template format
        // Reference: https://llama.meta.com/docs/model-cards-and-prompt-formats/meta-llama-3/
        format!(
            "<|begin_of_text|><|start_header_id|>system<|end_header_id|>\n\n{}<|eot_id|>\
             <|start_header_id|>user<|end_header_id|>\n\n{}<|eot_id|>\
             <|start_header_id|>assistant<|end_header_id|>\n\n",
            system, user_message
        )
    }

    fn eos_token_id(&self) -> u32 {
        // Llama 3 EOS token ID
        128009
    }

    fn bos_token_id(&self) -> Option<u32> {
        // Llama 3 BOS token ID
        Some(128000)
    }

    fn clean_output(&self, raw_output: &str) -> String {
        // Remove Llama template markers and trailing whitespace
        let cleaned = raw_output
            .split("<|eot_id|>")
            .next()
            .unwrap_or(raw_output)
            .split("<|end_of_text|>")
            .next()
            .unwrap_or(raw_output)
            .trim()
            .to_string();

        // Remove any header markers that might have been generated
        if cleaned.starts_with("<|start_header_id|>") {
            // Try to extract content after header
            if let Some(content_start) = cleaned.find("<|end_header_id|>") {
                cleaned[content_start + 17..].trim().to_string()
            } else {
                cleaned
            }
        } else {
            cleaned
        }
    }

    fn family_name(&self) -> &str {
        "Llama"
    }

    fn generation_config(&self) -> GenerationConfig {
        GenerationConfig {
            temperature: 0.6,
            top_p: 0.9,
            top_k: 40,
            repetition_penalty: 1.1,
            max_tokens: 512,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llama_format() {
        let adapter = LlamaAdapter;
        let prompt = adapter.format_chat_prompt(
            "You are a helpful assistant.",
            "What is 2+2?"
        );

        assert!(prompt.contains("<|begin_of_text|>"));
        assert!(prompt.contains("<|start_header_id|>system<|end_header_id|>"));
        assert!(prompt.contains("You are a helpful assistant."));
        assert!(prompt.contains("<|start_header_id|>user<|end_header_id|>"));
        assert!(prompt.contains("What is 2+2?"));
        assert!(prompt.contains("<|start_header_id|>assistant<|end_header_id|>"));
    }

    #[test]
    fn test_llama_clean_output() {
        let adapter = LlamaAdapter;

        // Test cleaning with eot_id marker
        let raw = "The answer is 4<|eot_id|>";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(cleaned, "The answer is 4");

        // Test cleaning with end_of_text marker
        let raw2 = "Response here<|end_of_text|>";
        let cleaned2 = adapter.clean_output(raw2);
        assert_eq!(cleaned2, "Response here");

        // Test no markers
        let raw3 = "Just a response";
        let cleaned3 = adapter.clean_output(raw3);
        assert_eq!(cleaned3, "Just a response");
    }

    #[test]
    fn test_llama_token_ids() {
        let adapter = LlamaAdapter;
        assert_eq!(adapter.eos_token_id(), 128009);
        assert_eq!(adapter.bos_token_id(), Some(128000));
    }
}

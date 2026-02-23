// Phi Model Adapter
//
// Handles Phi-specific chat template and token IDs.
// Phi models: Phi-2, Phi-3, Phi-3.5
//
// Phi-3 uses ChatML format similar to Qwen, but with some differences.
// Reference: https://huggingface.co/microsoft/Phi-3-mini-4k-instruct

use super::{GenerationConfig, LocalModelAdapter};

/// Adapter for Phi model family (Phi-2, Phi-3, Phi-3.5)
pub struct PhiAdapter;

impl LocalModelAdapter for PhiAdapter {
    fn format_chat_prompt(&self, system: &str, user_message: &str) -> String {
        // Phi-3 uses ChatML-style format with specific tokens
        // Format: <|system|>\n{system}<|end|>\n<|user|>\n{user}<|end|>\n<|assistant|>\n
        format!(
            "<|system|>\n{}<|end|>\n<|user|>\n{}<|end|>\n<|assistant|>\n",
            system, user_message
        )
    }

    fn eos_token_id(&self) -> u32 {
        // Phi-3 EOS token ID (end token)
        // Note: This may vary by model version, 32000 is common for Phi-3
        32000
    }

    fn bos_token_id(&self) -> Option<u32> {
        // Phi-3 BOS token ID
        Some(1)
    }

    fn clean_output(&self, raw_output: &str) -> String {
        // Remove Phi template markers and clean output
        let mut cleaned = raw_output
            .split("<|end|>")
            .next()
            .unwrap_or(raw_output)
            .split("<|endoftext|>")
            .next()
            .unwrap_or(raw_output)
            .trim()
            .to_string();

        // Remove any role markers that might have been generated
        for marker in &["<|system|>", "<|user|>", "<|assistant|>"] {
            if cleaned.starts_with(marker) {
                cleaned = cleaned.replacen(marker, "", 1).trim().to_string();
            }
        }

        // Handle potential question/answer pattern in output
        // Sometimes model includes the question in the response
        if cleaned.contains("user\n") && cleaned.contains("assistant\n") {
            // Find the last occurrence of "assistant\n" and take content after it
            if let Some(idx) = cleaned.rfind("assistant\n") {
                cleaned = cleaned[idx + 10..].trim().to_string();
            }
        }

        // Remove any remaining embedded role patterns
        cleaned = cleaned
            .replace("\nuser\n", " ")
            .replace("\nassistant\n", " ")
            .replace("\nsystem\n", " ");

        // Detect question/answer pattern and extract answer
        if cleaned.contains("?\n") || cleaned.contains("? ") {
            let lines: Vec<&str> = cleaned.lines().collect();
            if lines.len() > 1 {
                // If first line is a question and second is answer, extract answer
                if lines[0].contains('?') && !lines[1].contains('?') {
                    return lines[1..].join("\n").trim().to_string();
                }
            }
        }

        cleaned
    }

    fn family_name(&self) -> &str {
        "Phi"
    }

    fn generation_config(&self) -> GenerationConfig {
        GenerationConfig {
            temperature: 0.7,
            top_p: 0.95,
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
    fn test_phi_format() {
        let adapter = PhiAdapter;
        let prompt = adapter.format_chat_prompt("You are a helpful assistant.", "What is 2+2?");

        assert!(prompt.contains("<|system|>"));
        assert!(prompt.contains("You are a helpful assistant."));
        assert!(prompt.contains("<|end|>"));
        assert!(prompt.contains("<|user|>"));
        assert!(prompt.contains("What is 2+2?"));
        assert!(prompt.contains("<|assistant|>"));
    }

    #[test]
    fn test_phi_clean_output() {
        let adapter = PhiAdapter;

        // Test cleaning with end marker
        let raw = "The answer is 4<|end|>";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(cleaned, "The answer is 4");

        // Test cleaning with endoftext marker
        let raw2 = "Response here<|endoftext|>";
        let cleaned2 = adapter.clean_output(raw2);
        assert_eq!(cleaned2, "Response here");

        // Test no markers
        let raw3 = "Just a response";
        let cleaned3 = adapter.clean_output(raw3);
        assert_eq!(cleaned3, "Just a response");

        // Test with role markers
        let raw4 = "<|assistant|>The answer is 4";
        let cleaned4 = adapter.clean_output(raw4);
        assert_eq!(cleaned4, "The answer is 4");
    }

    #[test]
    fn test_phi_clean_output_question_answer() {
        let adapter = PhiAdapter;

        // Test question/answer pattern
        let raw = "What is 2+2?\n4";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(cleaned, "4");

        // Test with more context
        let raw2 = "user\nWhat is 2+2?\nassistant\n4";
        let cleaned2 = adapter.clean_output(raw2);
        assert_eq!(cleaned2, "4");
    }

    #[test]
    fn test_phi_token_ids() {
        let adapter = PhiAdapter;
        assert_eq!(adapter.eos_token_id(), 32000);
        assert_eq!(adapter.bos_token_id(), Some(1));
    }

    #[test]
    fn test_phi_generation_config() {
        let adapter = PhiAdapter;
        let config = adapter.generation_config();
        assert_eq!(config.temperature, 0.7);
        assert_eq!(config.top_p, 0.95);
        assert_eq!(config.top_k, 40);
    }
}

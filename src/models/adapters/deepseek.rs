// DeepSeek Model Adapter
//
// Handles DeepSeek-specific chat template and token IDs.
// DeepSeek models: DeepSeek-Coder, DeepSeek-V2, DeepSeek-V3
//
// DeepSeek uses a simple chat template format.
// Reference: https://huggingface.co/deepseek-ai/deepseek-coder-6.7b-instruct

use super::{LocalModelAdapter, GenerationConfig};

/// Adapter for DeepSeek model family (DeepSeek-Coder, DeepSeek-V2, etc.)
pub struct DeepSeekAdapter;

impl LocalModelAdapter for DeepSeekAdapter {
    fn format_chat_prompt(&self, system: &str, user_message: &str) -> String {
        // DeepSeek uses a simple format with special tokens
        // Format: <｜begin▁of▁sentence｜>{system}\n\n### Instruction:\n{user}\n\n### Response:\n
        format!(
            "<｜begin▁of▁sentence｜>{}\n\n### Instruction:\n{}\n\n### Response:\n",
            system, user_message
        )
    }

    fn eos_token_id(&self) -> u32 {
        // DeepSeek EOS token ID
        32021
    }

    fn bos_token_id(&self) -> Option<u32> {
        // DeepSeek BOS token ID
        Some(32013)
    }

    fn clean_output(&self, raw_output: &str) -> String {
        // Remove DeepSeek template markers, ChatML tokens, and reasoning markers
        // DeepSeek-R1-Distill-Qwen uses a mix of DeepSeek, ChatML, and reasoning tokens
        let mut cleaned = raw_output
            .split("<｜end▁of▁sentence｜>")
            .next()
            .unwrap_or(raw_output)
            .split("</s>")
            .next()
            .unwrap_or(raw_output)
            .split("</think>")
            .next()
            .unwrap_or(raw_output)
            .split("<|im_end|>")
            .next()
            .unwrap_or(raw_output)
            .split("<|endoftext|>")
            .next()
            .unwrap_or(raw_output)
            .trim()
            .to_string();

        // Remove template markers if they appear in output
        // Include both DeepSeek-specific markers and ChatML/reasoning tokens
        for marker in &[
            "<｜begin▁of▁sentence｜>",
            "<｜end▁of▁sentence｜>",
            "### Instruction:",
            "### Response:",
            "<think>",
            "</think>",
            "<|im_start|>user",
            "<|im_start|>system",
            "<|im_start|>assistant",
            "<|im_end|>",
        ] {
            if let Some(idx) = cleaned.find(marker) {
                if marker == &"### Response:" {
                    // Keep content after "### Response:"
                    cleaned = cleaned[idx + marker.len()..].trim().to_string();
                } else {
                    // Remove the marker
                    cleaned = cleaned.replace(marker, "").trim().to_string();
                }
            }
        }

        // Handle potential instruction/response pattern in output
        if cleaned.contains("### Instruction:") && cleaned.contains("### Response:") {
            // Find the last occurrence of "### Response:" and take content after it
            if let Some(idx) = cleaned.rfind("### Response:") {
                cleaned = cleaned[idx + 14..].trim().to_string();
            }
        }

        // Remove any remaining markdown artifacts from code models
        // DeepSeek-Coder might include code block markers
        if cleaned.starts_with("```") {
            let lines: Vec<&str> = cleaned.lines().collect();
            if lines.len() > 2 && lines[0].starts_with("```") {
                // Check if last line is closing ```
                if lines.last().map(|l| l.trim()) == Some("```") {
                    // Extract content between markers
                    cleaned = lines[1..lines.len()-1].join("\n").trim().to_string();
                }
            }
        }

        cleaned
    }

    fn family_name(&self) -> &str {
        "DeepSeek"
    }

    fn generation_config(&self) -> GenerationConfig {
        GenerationConfig {
            temperature: 0.8,  // Slightly higher for creative code generation
            top_p: 0.95,
            top_k: 50,
            repetition_penalty: 1.05,  // Lower penalty for code (repetition is natural)
            max_tokens: 2048,  // Longer for code generation
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deepseek_format() {
        let adapter = DeepSeekAdapter;
        let prompt = adapter.format_chat_prompt(
            "You are a helpful coding assistant.",
            "Write a function to check if a number is prime"
        );

        assert!(prompt.contains("<｜begin▁of▁sentence｜>"));
        assert!(prompt.contains("You are a helpful coding assistant."));
        assert!(prompt.contains("### Instruction:"));
        assert!(prompt.contains("Write a function to check if a number is prime"));
        assert!(prompt.contains("### Response:"));
    }

    #[test]
    fn test_deepseek_clean_output() {
        let adapter = DeepSeekAdapter;

        // Test cleaning with end marker
        let raw = "def is_prime(n):\n    return n > 1<｜end▁of▁sentence｜>";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(cleaned, "def is_prime(n):\n    return n > 1");

        // Test cleaning with </s> marker
        let raw2 = "Here is the code</s>";
        let cleaned2 = adapter.clean_output(raw2);
        assert_eq!(cleaned2, "Here is the code");

        // Test no markers
        let raw3 = "Just a response";
        let cleaned3 = adapter.clean_output(raw3);
        assert_eq!(cleaned3, "Just a response");
    }

    #[test]
    fn test_deepseek_clean_with_template() {
        let adapter = DeepSeekAdapter;

        // Test with response marker in output
        let raw = "### Response:\nHere is the answer";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(cleaned, "Here is the answer");

        // Test with full template in output
        let raw2 = "### Instruction:\nSomething\n### Response:\nThe answer";
        let cleaned2 = adapter.clean_output(raw2);
        assert_eq!(cleaned2, "The answer");
    }

    #[test]
    fn test_deepseek_clean_code_blocks() {
        let adapter = DeepSeekAdapter;

        // Test with code block markers
        let raw = "```python\ndef hello():\n    print('hello')\n```";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(cleaned, "def hello():\n    print('hello')");

        // Test with language-specific marker
        let raw2 = "```rust\nfn main() {}\n```";
        let cleaned2 = adapter.clean_output(raw2);
        assert_eq!(cleaned2, "fn main() {}");
    }

    #[test]
    fn test_deepseek_token_ids() {
        let adapter = DeepSeekAdapter;
        assert_eq!(adapter.eos_token_id(), 32021);
        assert_eq!(adapter.bos_token_id(), Some(32013));
    }

    #[test]
    fn test_deepseek_generation_config() {
        let adapter = DeepSeekAdapter;
        let config = adapter.generation_config();
        assert_eq!(config.temperature, 0.8);
        assert_eq!(config.top_p, 0.95);
        assert_eq!(config.max_tokens, 2048);
    }
}

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

        let mut cleaned = raw_output.to_string();

        // Step 1: Remove <think>...</think> sections (including nested ones)
        // Handle reasoning markers first before other processing
        while let Some(think_start) = cleaned.find("<think>") {
            if let Some(think_end) = cleaned[think_start..].find("</think>") {
                let end_pos = think_start + think_end + 8; // 8 = "</think>".len()
                cleaned = format!("{}{}", &cleaned[..think_start], &cleaned[end_pos..]);
            } else {
                // Unclosed <think>, just remove the marker
                cleaned = cleaned.replace("<think>", "");
                break;
            }
        }

        // Step 2: Find last occurrence of assistant marker (handles template echoing)
        if let Some(last_assistant_start) = cleaned.rfind("<|im_start|>assistant") {
            cleaned = cleaned[last_assistant_start + 22..].to_string(); // Skip "<|im_start|>assistant\n"
        }

        // Step 3: Remove end markers (ChatML + DeepSeek)
        cleaned = cleaned
            .split("<｜end▁of▁sentence｜>")
            .next()
            .unwrap_or(&cleaned)
            .split("</s>")
            .next()
            .unwrap_or(&cleaned)
            .split("<|im_end|>")
            .next()
            .unwrap_or(&cleaned)
            .split("<|endoftext|>")
            .next()
            .unwrap_or(&cleaned)
            .trim()
            .to_string();

        // Step 4: Handle role names as plain text (when treated as regular tokens)
        // Find the LAST occurrence of "assistant\n" or "assistant " (without special tokens)
        if let Some(last_assistant_pos) = cleaned.rfind("assistant\n") {
            // Take everything after "assistant\n"
            cleaned = cleaned[last_assistant_pos + 10..].to_string(); // "assistant\n".len() = 10
        } else if let Some(last_assistant_pos) = cleaned.rfind("assistant ") {
            // Handle "assistant " (space instead of newline)
            cleaned = cleaned[last_assistant_pos + 10..].to_string(); // "assistant ".len() = 10
        }

        // Step 5: Remove all remaining special tokens and role markers
        cleaned = cleaned.replace("\nuser\n", "\n");
        cleaned = cleaned.replace("\nsystem\n", "\n");
        cleaned = cleaned.replace("\nassistant\n", "\n");
        cleaned = cleaned.replace("<｜begin▁of▁sentence｜>", "");
        cleaned = cleaned.replace("<｜end▁of▁sentence｜>", "");
        cleaned = cleaned.replace("<|im_start|>user", "");
        cleaned = cleaned.replace("<|im_start|>system", "");
        cleaned = cleaned.replace("<|im_start|>assistant", "");
        cleaned = cleaned.replace("### Instruction:", "");
        cleaned = cleaned.replace("### Response:", "");

        // Step 6: Remove leading role names (if any remain)
        cleaned = cleaned
            .trim_start_matches("system")
            .trim_start_matches("user")
            .trim_start_matches("assistant")
            .trim_start_matches('\n')
            .trim()
            .to_string();

        // Step 7: Remove markdown code block markers (DeepSeek-Coder specific)
        if cleaned.starts_with("```") {
            let lines: Vec<&str> = cleaned.lines().collect();
            if lines.len() > 2 && lines[0].starts_with("```") {
                // Check if last line is closing ```
                if lines.last().map(|l| l.trim()) == Some("```") {
                    // Extract content between markers
                    cleaned = lines[1..lines.len()-1].join("\n");
                }
            }
        }

        cleaned.trim().to_string()
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

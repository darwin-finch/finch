// Qwen Model Adapter
//
// Handles Qwen-specific chat template (ChatML format) and token IDs.
// Qwen models: Qwen2.5, Qwen2, Qwen1.5

use super::{LocalModelAdapter, GenerationConfig};

/// Adapter for Qwen model family (ChatML format)
pub struct QwenAdapter;

impl LocalModelAdapter for QwenAdapter {
    fn format_chat_prompt(&self, system: &str, user_message: &str) -> String {
        // ChatML format used by Qwen models
        // Reference: https://github.com/QwenLM/Qwen/blob/main/README.md
        format!(
            "<|im_start|>system\n{}<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
            system, user_message
        )
    }

    fn eos_token_id(&self) -> u32 {
        // Qwen2/Qwen2.5 EOS token ID
        151643
    }

    fn bos_token_id(&self) -> Option<u32> {
        // Qwen doesn't use explicit BOS token in ChatML
        None
    }

    fn clean_output(&self, raw_output: &str) -> String {
        // The model might echo the system prompt - we need to extract ONLY the actual answer
        let mut cleaned = raw_output;

        // If the model echoed the template, find the last "assistant" section
        if let Some(last_assistant_start) = cleaned.rfind("<|im_start|>assistant") {
            cleaned = &cleaned[last_assistant_start + 22..]; // Skip "<|im_start|>assistant\n"
        }

        // Remove end markers
        cleaned = cleaned
            .split("<|im_end|>")
            .next()
            .unwrap_or(cleaned)
            .split("<|endoftext|>")
            .next()
            .unwrap_or(cleaned)
            .trim();

        // Remove ChatML role markers if they leaked through
        cleaned = cleaned
            .trim_start_matches("system")
            .trim_start_matches("user")
            .trim_start_matches("assistant")
            .trim_start_matches('\n')
            .trim();

        // AGGRESSIVE: If the output starts with constitution text, skip to the actual answer
        // Constitution typically starts with "You are Shammah" or "# Shammah Constitution"
        if cleaned.starts_with("You are Shammah") || cleaned.starts_with("# Shammah Constitution") {
            // The actual answer is usually at the end after all the instructions
            // Try multiple strategies:

            // Strategy 1: Look for common question-answer separators
            for separator in &["\n\n##", "\n\nExamples", "\n\nRemember:", "---\n", "## Examples"] {
                if let Some(sep_pos) = cleaned.find(separator) {
                    // Answer is likely after this section, so skip the rest of constitution
                    cleaned = &cleaned[sep_pos..];
                    break;
                }
            }

            // Strategy 2: If output is very long (>200 chars) and starts with constitution,
            // the answer is likely the LAST paragraph
            if cleaned.len() > 200 {
                // Split by double newline and take the last non-empty paragraph
                let paragraphs: Vec<&str> = cleaned.split("\n\n").collect();
                if let Some(last_para) = paragraphs.iter().rev().find(|p| !p.trim().is_empty() && p.len() < 100) {
                    cleaned = last_para.trim();
                }
            }
        }

        // If still too long (>500 chars), something went wrong - take last line as fallback
        if cleaned.len() > 500 {
            if let Some(last_line) = cleaned.lines().last() {
                if !last_line.trim().is_empty() && last_line.len() < 200 {
                    cleaned = last_line.trim();
                }
            }
        }

        cleaned.trim().to_string()
    }

    fn family_name(&self) -> &str {
        "Qwen"
    }

    fn generation_config(&self) -> GenerationConfig {
        GenerationConfig {
            temperature: 0.7,
            top_p: 0.8,
            top_k: 20,
            repetition_penalty: 1.05,
            max_tokens: 512,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qwen_format() {
        let adapter = QwenAdapter;
        let prompt = adapter.format_chat_prompt(
            "You are a helpful assistant.",
            "What is 2+2?"
        );

        assert!(prompt.contains("<|im_start|>system"));
        assert!(prompt.contains("You are a helpful assistant."));
        assert!(prompt.contains("<|im_start|>user"));
        assert!(prompt.contains("What is 2+2?"));
        assert!(prompt.contains("<|im_start|>assistant"));
        assert!(prompt.ends_with("<|im_start|>assistant\n"));
    }

    #[test]
    fn test_qwen_clean_output() {
        let adapter = QwenAdapter;

        // Test cleaning with end marker
        let raw = "The answer is 4<|im_end|>";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(cleaned, "The answer is 4");

        // Test cleaning with multiple markers
        let raw2 = "Response here<|im_end|>extra stuff<|endoftext|>";
        let cleaned2 = adapter.clean_output(raw2);
        assert_eq!(cleaned2, "Response here");

        // Test no markers
        let raw3 = "Just a response";
        let cleaned3 = adapter.clean_output(raw3);
        assert_eq!(cleaned3, "Just a response");
    }

    #[test]
    fn test_qwen_token_ids() {
        let adapter = QwenAdapter;
        assert_eq!(adapter.eos_token_id(), 151643);
        assert_eq!(adapter.bos_token_id(), None);
    }
}

// Qwen Model Adapter
//
// Handles Qwen-specific chat template (ChatML format) and token IDs.
// Qwen models: Qwen2.5, Qwen2, Qwen1.5

use super::{GenerationConfig, LocalModelAdapter};

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
        Self::clean_output_static(raw_output)
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

impl QwenAdapter {
    /// Static method for cleaning output without adapter instance
    /// This can be called from message rendering for streaming responses
    pub fn clean_output_static(raw_output: &str) -> String {
        // IMPORTANT: If output contains tool XML markers, use minimal cleaning
        // to preserve the tool_use and tool_result blocks intact
        if raw_output.contains("<tool_use>") || raw_output.contains("<tool_result>") {
            // Minimal cleaning: only remove chat template markers
            return raw_output
                .split("<|im_end|>")
                .next()
                .unwrap_or(raw_output)
                .split("<|endoftext|>")
                .next()
                .unwrap_or(raw_output)
                .split("<｜end▁of▁sentence｜>")
                .next()
                .unwrap_or(raw_output)
                .replace("<|im_start|>assistant\n", "")
                .replace("<|im_start|>assistant", "")
                .replace("<think>", "")
                .replace("</think>", "")
                .trim()
                .to_string();
        }

        // The model might echo the system prompt - we need to extract ONLY the actual answer
        let mut cleaned = raw_output;

        // Step 1: Handle special tokens (ChatML format with markers)
        // If the model echoed the template, find the last "assistant" section with markers
        if let Some(last_assistant_start) = cleaned.rfind("<|im_start|>assistant") {
            cleaned = &cleaned[last_assistant_start + 22..]; // Skip "<|im_start|>assistant\n"
        }

        // Remove end markers (ChatML + DeepSeek + reasoning)
        cleaned = cleaned
            .split("<|im_end|>")
            .next()
            .unwrap_or(cleaned)
            .split("<|endoftext|>")
            .next()
            .unwrap_or(cleaned)
            .split("<｜end▁of▁sentence｜>")
            .next()
            .unwrap_or(cleaned)
            .split("</think>")
            .next()
            .unwrap_or(cleaned)
            .trim();

        // Step 2: Handle role names as plain text (when tokenizer treats them as regular tokens)
        // Find the LAST occurrence of "assistant\n" or "assistant " (without special tokens)
        // This handles cases like: "user\nWhat is 2+2?\nassistant\n4"
        if let Some(last_assistant_pos) = cleaned.rfind("assistant\n") {
            // Take everything after "assistant\n"
            cleaned = &cleaned[last_assistant_pos + 10..]; // "assistant\n".len() = 10
        } else if let Some(last_assistant_pos) = cleaned.rfind("assistant ") {
            // Handle "assistant " (space instead of newline)
            cleaned = &cleaned[last_assistant_pos + 10..]; // "assistant ".len() = 10
        }

        // Step 3: Remove embedded role patterns and special tokens
        // Replace patterns like "\nuser\n", "\nsystem\n", "\nassistant\n" with just "\n"
        // Also remove DeepSeek BOS tokens and ChatML markers
        let mut temp = cleaned.to_string();
        temp = temp.replace("\nuser\n", "\n");
        temp = temp.replace("\nsystem\n", "\n");
        temp = temp.replace("\nassistant\n", "\n");
        temp = temp.replace("<｜begin▁of▁sentence｜>", "");
        temp = temp.replace("<｜end▁of▁sentence｜>", "");
        temp = temp.replace("<think>", "");
        temp = temp.replace("</think>", "");
        temp = temp.replace("<|im_start|>user", "");
        temp = temp.replace("<|im_start|>system", "");
        temp = temp.replace("<|im_start|>assistant", "");
        cleaned = &temp;

        // Step 4: Remove leading role names (if any remain after above steps)
        cleaned = cleaned
            .trim_start_matches("system")
            .trim_start_matches("user")
            .trim_start_matches("assistant")
            .trim_start_matches('\n')
            .trim();

        // Step 5: Detect question/answer pattern and extract just the answer
        // Pattern: "What is X?\nAnswer" → extract "Answer"
        // IMPORTANT: Only trigger this for SHORT responses (2-3 lines) that look like prompt echoes
        // Don't truncate normal multi-line responses that happen to contain questions!
        let lines: Vec<&str> = cleaned.lines().collect();
        if lines.len() == 2 || lines.len() == 3 {
            // Only for very short responses that look like prompt echoes
            if let Some(first_line) = lines.first() {
                // First line should end with '?' AND be a reasonable question length (< 100 chars)
                if first_line.trim().ends_with('?') && first_line.len() < 100 {
                    // Last line should be short (< 50 chars) like a direct answer
                    if let Some(last_line) = lines.iter().rev().find(|l| !l.trim().is_empty()) {
                        if last_line.len() < 50 {
                            cleaned = last_line.trim();
                        }
                    }
                }
            }
        }

        // Step 6: AGGRESSIVE: If the output starts with constitution text, skip to the actual answer
        // Constitution typically starts with "You are Shammah" or "# Shammah Constitution"
        if cleaned.starts_with("You are Shammah") || cleaned.starts_with("# Shammah Constitution") {
            // The actual answer is usually at the end after all the instructions
            // Try multiple strategies:

            // Strategy 1: Look for common question-answer separators
            for separator in &[
                "\n\n##",
                "\n\nExamples",
                "\n\nRemember:",
                "---\n",
                "## Examples",
            ] {
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
                if let Some(last_para) = paragraphs
                    .iter()
                    .rev()
                    .find(|p| !p.trim().is_empty() && p.len() < 100)
                {
                    cleaned = last_para.trim();
                }
            }
        }

        // Step 7: If still too long (>500 chars), something went wrong - take last line as fallback
        if cleaned.len() > 500 {
            if let Some(last_line) = cleaned.lines().last() {
                if !last_line.trim().is_empty() && last_line.len() < 200 {
                    cleaned = last_line.trim();
                }
            }
        }

        cleaned.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qwen_format() {
        let adapter = QwenAdapter;
        let prompt = adapter.format_chat_prompt("You are a helpful assistant.", "What is 2+2?");

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

    #[test]
    fn test_clean_echo_with_answer() {
        let adapter = QwenAdapter;

        // Test case 1: Echo with embedded role (THE MAIN PROBLEM CASE)
        // This is what the ONNX model currently generates
        let raw = "user\nWhat is 2+2?\nassistant\n4";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(cleaned, "4");

        // Test case 2: Echo with role names and spaces
        let raw2 = "user What is Rust?\nassistant Rust is a systems programming language";
        let cleaned2 = adapter.clean_output(raw2);
        assert_eq!(cleaned2, "Rust is a systems programming language");

        // Test case 3: Multiple role patterns
        let raw3 = "system\nYou are helpful\nuser\nTest\nassistant\nResponse";
        let cleaned3 = adapter.clean_output(raw3);
        assert_eq!(cleaned3, "Response");
    }

    #[test]
    fn test_clean_question_answer_pattern() {
        let adapter = QwenAdapter;

        // Test case 1: Question with answer on next line
        let raw = "What is Rust?\nRust is a systems programming language";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(cleaned, "Rust is a systems programming language");

        // Test case 2: Question with multi-line answer
        let raw2 =
            "How do I print in Rust?\nYou can use println! macro\nExample: println!(\"Hello\");";
        let cleaned2 = adapter.clean_output(raw2);
        // Should extract the answer (not the question)
        assert!(cleaned2.contains("println!"));
        assert!(!cleaned2.starts_with("How do I"));
    }

    #[test]
    fn test_clean_embedded_role_patterns() {
        let adapter = QwenAdapter;

        // Test removing embedded role patterns in the middle of text
        let raw = "Here is\nuser\nsome text\nassistant\nwith roles";
        let cleaned = adapter.clean_output(raw);
        // Role patterns should be removed or collapsed
        assert!(!cleaned.contains("user\n"));
        assert!(!cleaned.contains("assistant\n"));
    }

    #[test]
    fn test_clean_preserves_good_output() {
        let adapter = QwenAdapter;

        // Test that clean output without artifacts is preserved
        let raw = "The answer is 42";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(cleaned, "The answer is 42");

        // Test multi-line clean output
        let raw2 = "Here is the code:\nfn main() {\n    println!(\"Hello\");\n}";
        let cleaned2 = adapter.clean_output(raw2);
        assert_eq!(cleaned2, raw2);
    }

    #[test]
    fn test_clean_preserves_tool_xml() {
        let adapter = QwenAdapter;

        // Test that tool_use blocks are preserved
        let raw = r#"I'll read the file for you.

<tool_use>
  <name>read</name>
  <parameters>{"file_path": "/tmp/test.txt"}</parameters>
</tool_use>"#;

        let cleaned = adapter.clean_output(raw);
        assert!(cleaned.contains("<tool_use>"));
        assert!(cleaned.contains("<name>read</name>"));
        assert!(cleaned.contains("<parameters>"));
        assert!(cleaned.contains("</tool_use>"));
        assert!(cleaned.contains("I'll read the file"));
    }

    #[test]
    fn test_clean_preserves_tool_result_xml() {
        let adapter = QwenAdapter;

        // Test that tool_result blocks are preserved
        let raw = r#"Here are the results:

<tool_result id="toolu_123">
File contents here
</tool_result>

Based on the file contents..."#;

        let cleaned = adapter.clean_output(raw);
        assert!(cleaned.contains("<tool_result"));
        assert!(cleaned.contains("toolu_123"));
        assert!(cleaned.contains("File contents here"));
        assert!(cleaned.contains("</tool_result>"));
    }
}

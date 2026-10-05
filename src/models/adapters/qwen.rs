// Qwen Model Adapter
//
// Handles Qwen-specific chat template (ChatML format) and token IDs.
// Qwen models: Qwen2.5, Qwen2, Qwen1.5

use super::{GenerationConfig, LocalModelAdapter};

/// Adapter for Qwen model family (ChatML format)
pub struct QwenAdapter;

impl LocalModelAdapter for QwenAdapter {
    fn format_chat_prompt(&self, system: &str, user_message: &str) -> String {
        let (history, question) = super::parse_history_from_query(user_message);
        self.format_chat_history(system, &history, question)
    }

    fn format_chat_history(
        &self,
        system: &str,
        history: &[(&str, &str)],
        user_message: &str,
    ) -> String {
        // ChatML format used by Qwen models
        // Reference: https://github.com/QwenLM/Qwen/blob/main/README.md
        let mut prompt = format!("<|im_start|>system\n{}<|im_end|>\n", system);
        for (role, content) in history {
            prompt.push_str(&format!("<|im_start|>{}\n{}<|im_end|>\n", role, content));
        }
        prompt.push_str(&format!(
            "<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
            user_message
        ));
        prompt
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

        let mut cleaned = raw_output;

        // Step 1: Remove reasoning blocks (<think>...</think>) if present.
        // In reasoning models, thoughts precede the final response; keep what follows </think>.
        let mut without_think = cleaned.to_string();
        while let Some(think_start) = without_think.find("<think>") {
            if let Some(think_end) = without_think[think_start..].find("</think>") {
                let end_pos = think_start + think_end + "</think>".len();
                without_think = format!(
                    "{}{}",
                    &without_think[..think_start],
                    &without_think[end_pos..]
                );
            } else {
                without_think = without_think.replace("<think>", "");
                break;
            }
        }
        let without_think_str = without_think;
        cleaned = &without_think_str;

        // Step 2: Handle special tokens (ChatML format with markers)
        // If the model echoed the template, find the last "assistant" section with markers
        if let Some(last_assistant_start) = cleaned.rfind("<|im_start|>assistant") {
            let after = &cleaned[last_assistant_start + "<|im_start|>assistant".len()..];
            cleaned = after.strip_prefix('\n').unwrap_or(after);
        }

        // Remove end markers (ChatML + DeepSeek)
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
            .trim();

        // Step 3: Handle role names as plain text (when tokenizer treats them as regular tokens)
        // Only strip if the response actually contains role markers (starts with assistant/user/system
        // or contains user/system turns) to avoid truncating legitimate prose mentioning "assistant".
        let has_role_markers = cleaned.starts_with("assistant\n")
            || cleaned.starts_with("assistant ")
            || cleaned.starts_with("user\n")
            || cleaned.starts_with("user ")
            || cleaned.starts_with("system\n")
            || cleaned.starts_with("system ")
            || cleaned.contains("\nuser\n")
            || cleaned.contains("\nuser ")
            || cleaned.contains("\nsystem\n")
            || cleaned.contains("\nsystem ");

        if has_role_markers {
            if let Some(last_pos) = cleaned.rfind("\nassistant\n") {
                cleaned = &cleaned[last_pos + 1 + 10..];
            } else if let Some(last_pos) = cleaned.rfind("\nassistant ") {
                cleaned = &cleaned[last_pos + 1 + 10..];
            } else if cleaned.starts_with("assistant\n") {
                cleaned = &cleaned[10..];
            } else if cleaned.starts_with("assistant ") {
                cleaned = &cleaned[10..];
            }
        }

        // Step 4: Remove embedded role patterns and special tokens
        let mut temp = cleaned.to_string();
        temp = temp.replace("\nuser\n", "\n");
        temp = temp.replace("\nsystem\n", "\n");
        temp = temp.replace("\nassistant\n", "\n");
        temp = temp.replace("<｜begin▁of▁sentence｜>", "");
        temp = temp.replace("<｜end▁of▁sentence｜>", "");
        temp = temp.replace("<|im_start|>user", "");
        temp = temp.replace("<|im_start|>system", "");
        temp = temp.replace("<|im_start|>assistant", "");
        let temp_str = temp;
        cleaned = &temp_str;

        // Step 5: Remove leading role names (if any remain after above steps)
        cleaned = cleaned
            .trim_start_matches("system")
            .trim_start_matches("user")
            .trim_start_matches("assistant")
            .trim_start_matches('\n')
            .trim();

        // Step 6: Detect question/answer pattern and extract just the answer
        // Pattern: "What is X?\nAnswer" → extract "Answer"
        // IMPORTANT: Only trigger this for SHORT responses (2-3 lines) that look like prompt echoes.
        let lines: Vec<&str> = cleaned.lines().collect();
        if lines.len() == 2 || lines.len() == 3 {
            if let Some(first_line) = lines.first() {
                if first_line.trim().ends_with('?') && first_line.len() < 100 {
                    if let Some(last_line) = lines.iter().rev().find(|l| !l.trim().is_empty()) {
                        if last_line.len() < 50 {
                            cleaned = last_line.trim();
                        }
                    }
                }
            }
        }

        // Step 7: If the output starts with constitution text, skip to the actual answer
        if cleaned.starts_with("You are Shammah") || cleaned.starts_with("# Shammah Constitution") {
            for separator in &[
                "\n\n##",
                "\n\nExamples",
                "\n\nRemember:",
                "---\n",
                "## Examples",
            ] {
                if let Some(sep_pos) = cleaned.find(separator) {
                    cleaned = &cleaned[sep_pos..];
                    break;
                }
            }
            if let Some(q_pos) = cleaned.rfind('?') {
                if let Some(answer_start) = cleaned[q_pos..].find("\n\n") {
                    cleaned = &cleaned[q_pos + answer_start + 2..];
                }
            }
        }

        // Legitimate responses (poems, code, programs, explanations) can be thousands
        // of characters long. Never truncate based on length.
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
    fn test_qwen_format_multi_turn() {
        let adapter = QwenAdapter;
        let history = vec![
            ("user", "Hello!"),
            ("assistant", "Hi there! How can I help you?"),
        ];
        let prompt =
            adapter.format_chat_history("You are a helpful assistant.", &history, "What is 2+2?");

        let expected = "<|im_start|>system\n\
                        You are a helpful assistant.<|im_end|>\n\
                        <|im_start|>user\n\
                        Hello!<|im_end|>\n\
                        <|im_start|>assistant\n\
                        Hi there! How can I help you?<|im_end|>\n\
                        <|im_start|>user\n\
                        What is 2+2?<|im_end|>\n\
                        <|im_start|>assistant\n";
        assert_eq!(prompt, expected);

        // Also verify format_chat_prompt with serialized query decomposes to the same ChatML
        let query = "user: Hello!\n\nassistant: Hi there! How can I help you?\n\nWhat is 2+2?";
        let prompt_from_query = adapter.format_chat_prompt("You are a helpful assistant.", query);
        assert_eq!(prompt_from_query, expected);
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
        // Some Qwen-family artifacts emit this assistant prefix.
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

    #[test]
    fn test_clean_preserves_long_output_exceeding_500_chars() {
        let adapter = QwenAdapter;

        // A poem or document that is well over 500 characters
        let poem = "\
In the quiet of the night, I find my heart's desire,
In the love of my life, my soul's truest guide.
Through the storms and the sunshine, we've stood together strong,
In each other's arms, we've found a place to belong.
Your laughter echoes in the halls of my memory,
A melody so sweet, it sets my spirit free.
With every passing day, my affection only grows,
A river of devotion that endlessly flows.
Side by side we walk this road hand in gentle hand,
The greatest journey across all time and land.
Forever and always, my promise is true,
In this life and the next, I belong with you.";

        assert!(
            poem.len() > 500,
            "Poem must be >500 chars (was {})",
            poem.len()
        );
        let raw = format!("{}<|im_end|>", poem);
        let cleaned = adapter.clean_output(&raw);
        assert_eq!(cleaned, poem);
    }

    #[test]
    fn test_clean_preserves_long_code_fence_block() {
        let adapter = QwenAdapter;

        // A long Forth / Lisp program exceeding 500 characters that ends with ```
        let program = r#"```forth
(say (write-file "love_poem.txt"
                 (concatenate 'string
                   "In the quiet of the night, I find my heart's desire,\n"
                   "In the love of my life, my soul's truest guide.\n"
                   "Through the storms and the sunshine, we've stood together strong,\n"
                   "In each other's arms, we've found a place to belong.\n"
                   "Your laughter echoes in the silent morning light,\n"
                   "A warmth that keeps me safe through every winter night.\n")))
```"#;

        assert!(
            program.len() > 500,
            "Program must be >500 chars (was {})",
            program.len()
        );
        let raw = format!("{}<|im_end|>", program);
        let cleaned = adapter.clean_output(&raw);
        assert_eq!(cleaned, program);
        // Ensure it did not get truncated down to 3 chars ("```")
        assert!(cleaned.len() > 500);
        assert!(cleaned.starts_with("```forth"));
        assert!(cleaned.ends_with("```"));
    }

    #[test]
    fn test_clean_reasoning_think_tags() {
        let adapter = QwenAdapter;

        let raw = "<think>\nThinking about the user's poem request...\nLet's write something nice.\n</think>\nHere is your poem:\nRoses are red,\nViolets are blue.<|im_end|>";
        let cleaned = adapter.clean_output(raw);
        assert!(!cleaned.contains("<think>"));
        assert!(!cleaned.contains("Thinking about"));
        assert!(cleaned.starts_with("Here is your poem:"));
        assert!(cleaned.contains("Roses are red"));
    }

    #[test]
    fn test_clean_preserves_assistant_in_normal_prose() {
        let adapter = QwenAdapter;

        let raw = "As an AI assistant, I can help you with your question.\nHere is the answer: 42.<|im_end|>";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(
            cleaned,
            "As an AI assistant, I can help you with your question.\nHere is the answer: 42."
        );
    }

    #[test]
    fn test_clean_assistant_marker_without_newline() {
        let adapter = QwenAdapter;

        let raw = "<|im_start|>assistantHello world!<|im_end|>";
        let cleaned = adapter.clean_output(raw);
        assert_eq!(cleaned, "Hello world!");
    }
}

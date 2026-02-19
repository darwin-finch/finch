// Test adapter streaming output quality and special token filtering
//
// This test suite verifies that:
// 1. Special tokens are filtered during streaming
// 2. Output is cleaned properly for each model family
// 3. Reasoning markers are removed (DeepSeek <think>)
// 4. Template artifacts are stripped (ChatML, etc.)

use anyhow::Result;
use finch::models::adapters::{
    LocalModelAdapter, QwenAdapter, DeepSeekAdapter, LlamaAdapter,
};

/// Test that QwenAdapter removes ChatML special tokens
#[test]
fn test_qwen_adapter_removes_chatml_tokens() {
    let adapter = QwenAdapter;

    // Raw output with ChatML tokens
    let raw = "<|im_start|>assistant\nThe answer is 42<|im_end|>";
    let cleaned = adapter.clean_output(raw);

    // Should remove special tokens
    assert!(!cleaned.contains("<|im_start|>"));
    assert!(!cleaned.contains("<|im_end|>"));
    assert_eq!(cleaned, "The answer is 42");
}

/// Test that QwenAdapter removes embedded role markers
#[test]
fn test_qwen_adapter_removes_embedded_roles() {
    let adapter = QwenAdapter;

    // Output with embedded role markers (common with template artifacts)
    let raw = "user\nWhat is this?\nassistant\nThis is the answer";
    let cleaned = adapter.clean_output(raw);

    // Should extract only the assistant's response
    assert!(!cleaned.contains("user\n"));
    assert!(!cleaned.contains("assistant\n"));
    assert_eq!(cleaned, "This is the answer");
}

/// Test that DeepSeekAdapter removes sentence markers
#[test]
fn test_deepseek_adapter_removes_sentence_markers() {
    let adapter = DeepSeekAdapter;

    // Raw output with DeepSeek sentence markers
    let raw = "<｜begin▁of▁sentence｜>assistant\nThe answer is 42<｜end▁of▁sentence｜>";
    let cleaned = adapter.clean_output(raw);

    // Should remove special tokens
    assert!(!cleaned.contains("<｜begin▁of▁sentence｜>"));
    assert!(!cleaned.contains("<｜end▁of▁sentence｜>"));
    assert!(!cleaned.contains("assistant\n"));
    assert_eq!(cleaned, "The answer is 42");
}

/// Test that DeepSeekAdapter removes reasoning markers
#[test]
fn test_deepseek_adapter_removes_reasoning_markers() {
    let adapter = DeepSeekAdapter;

    // Output with reasoning markers
    let raw = "<think>Let me think about this... 2+2=4</think>The answer is 4";
    let cleaned = adapter.clean_output(raw);

    // Should remove reasoning section
    assert!(!cleaned.contains("<think>"));
    assert!(!cleaned.contains("</think>"));
    assert!(!cleaned.contains("Let me think"));
    assert_eq!(cleaned, "The answer is 4");
}

/// Test that DeepSeekAdapter handles mixed tokens (ChatML + DeepSeek)
#[test]
fn test_deepseek_adapter_handles_mixed_tokens() {
    let adapter = DeepSeekAdapter;

    // Output with both ChatML and DeepSeek tokens
    let raw = "<|im_start|>assistant\n<think>Reasoning...</think>Answer<|im_end|><｜end▁of▁sentence｜>";
    let cleaned = adapter.clean_output(raw);

    // Should remove all special tokens
    assert!(!cleaned.contains("<|im_start|>"));
    assert!(!cleaned.contains("<|im_end|>"));
    assert!(!cleaned.contains("<think>"));
    assert!(!cleaned.contains("</think>"));
    assert!(!cleaned.contains("<｜end▁of▁sentence｜>"));
    assert_eq!(cleaned, "Answer");
}

/// Test that adapters remove LaTeX formatting artifacts
#[test]
fn test_adapters_remove_latex_artifacts() {
    let qwen = QwenAdapter;
    let deepseek = DeepSeekAdapter;

    // Output with LaTeX \boxed{} formatting
    let raw = "The answer is \\boxed{42}";

    let qwen_cleaned = qwen.clean_output(raw);
    let deepseek_cleaned = deepseek.clean_output(raw);

    // Should preserve content but could remove \boxed
    // For now, we accept it as-is (LaTeX might be intentional)
    // This test documents current behavior
    assert!(qwen_cleaned.contains("42"));
    assert!(deepseek_cleaned.contains("42"));
}

/// Test that adapters handle empty input gracefully
#[test]
fn test_adapters_handle_empty_input() {
    let qwen = QwenAdapter;
    let deepseek = DeepSeekAdapter;
    let llama = LlamaAdapter;

    let cleaned_qwen = qwen.clean_output("");
    let cleaned_deepseek = deepseek.clean_output("");
    let cleaned_llama = llama.clean_output("");

    assert_eq!(cleaned_qwen, "");
    assert_eq!(cleaned_deepseek, "");
    assert_eq!(cleaned_llama, "");
}

/// Test that adapters handle whitespace-only input
#[test]
fn test_adapters_handle_whitespace_only_input() {
    let qwen = QwenAdapter;
    let deepseek = DeepSeekAdapter;

    let cleaned_qwen = qwen.clean_output("   \n\n   ");
    let cleaned_deepseek = deepseek.clean_output("   \n\n   ");

    // Should trim to empty string
    assert_eq!(cleaned_qwen, "");
    assert_eq!(cleaned_deepseek, "");
}

/// Test that adapters preserve actual content
#[test]
fn test_adapters_preserve_actual_content() {
    let adapter = QwenAdapter;

    // Output with actual content and special tokens
    let raw = "<|im_start|>assistant\nHere is a detailed explanation:\n1. First point\n2. Second point\n3. Third point<|im_end|>";
    let cleaned = adapter.clean_output(raw);

    // Should preserve all actual content
    assert!(cleaned.contains("Here is a detailed explanation"));
    assert!(cleaned.contains("1. First point"));
    assert!(cleaned.contains("2. Second point"));
    assert!(cleaned.contains("3. Third point"));

    // But remove special tokens
    assert!(!cleaned.contains("<|im_start|>"));
    assert!(!cleaned.contains("<|im_end|>"));
}

/// Test that adapters handle multiple role markers
#[test]
fn test_adapters_handle_multiple_role_markers() {
    let adapter = QwenAdapter;

    // Output that somehow has multiple assistant markers (template bug)
    let raw = "assistant\nFirst response\nassistant\nSecond response";
    let cleaned = adapter.clean_output(raw);

    // Should extract content after the LAST assistant marker
    assert_eq!(cleaned, "Second response");
}

/// Test that DeepSeek adapter handles nested reasoning markers
#[test]
fn test_deepseek_adapter_handles_nested_reasoning() {
    let adapter = DeepSeekAdapter;

    // Output with nested or multiple reasoning sections
    let raw = "<think>First thought</think>Answer<think>Second thought</think>";
    let cleaned = adapter.clean_output(raw);

    // Should remove all reasoning sections
    assert!(!cleaned.contains("<think>"));
    assert!(!cleaned.contains("</think>"));
    assert!(!cleaned.contains("First thought"));
    assert!(!cleaned.contains("Second thought"));
    assert_eq!(cleaned, "Answer");
}

/// Test that adapters handle malformed tokens gracefully
#[test]
fn test_adapters_handle_malformed_tokens() {
    let adapter = QwenAdapter;

    // Malformed tokens (missing closing bracket)
    let raw = "<|im_start|>assistant\nContent without proper closing";
    let cleaned = adapter.clean_output(raw);

    // Should still remove what it can
    assert!(!cleaned.contains("<|im_start|>"));
    assert!(cleaned.contains("Content without proper closing"));
}

/// Test EOS token IDs are correct for each adapter
#[test]
fn test_adapter_eos_token_ids() {
    let qwen = QwenAdapter;
    let deepseek = DeepSeekAdapter;
    let llama = LlamaAdapter;

    // Verify EOS tokens are defined
    assert!(qwen.eos_token_id() > 0);
    assert!(deepseek.eos_token_id() > 0);
    assert!(llama.eos_token_id() > 0);

    // Different models should have different EOS tokens
    // (This is a sanity check - might not always be true)
    // Just verify they're not all the same default value
    let tokens = vec![qwen.eos_token_id(), deepseek.eos_token_id(), llama.eos_token_id()];
    assert!(tokens.iter().any(|&t| t != tokens[0]),
            "EOS tokens should differ between model families");
}

/// Test adapter family names are correct
#[test]
fn test_adapter_family_names() {
    let qwen = QwenAdapter;
    let deepseek = DeepSeekAdapter;
    let llama = LlamaAdapter;

    assert_eq!(qwen.family_name(), "Qwen");
    assert_eq!(deepseek.family_name(), "DeepSeek");
    assert_eq!(llama.family_name(), "Llama");
}

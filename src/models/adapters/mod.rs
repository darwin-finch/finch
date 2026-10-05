// Local Model Adapters - Format prompts and handle model-specific behavior
//
// These adapters handle local chat-model specifics (templates, tokens, output cleaning).
// This is DIFFERENT from the provider transports (crates/finch-providers, surfaced through
// src/providers/) which handle external API calls.
//
// LocalModelAdapter: format prompts for local chat inference.
// Provider transport: Make HTTP requests to external APIs (Claude, OpenAI, etc.)

pub mod deepseek;
pub mod gemma;
pub mod llama;
pub mod mistral;
pub mod phi;
pub mod qwen;

pub use deepseek::DeepSeekAdapter;
pub use gemma::GemmaAdapter;
pub use llama::LlamaAdapter;
pub use mistral::MistralAdapter;
pub use phi::PhiAdapter;
pub use qwen::QwenAdapter;

use std::fmt;

/// Local model adapter for formatting prompts and handling model-specific behavior
pub trait LocalModelAdapter: Send + Sync {
    /// Format a prompt with system message using model's chat template
    fn format_chat_prompt(&self, system: &str, user_message: &str) -> String {
        let (history, question) = parse_history_from_query(user_message);
        self.format_chat_history(system, &history, question)
    }

    /// Format a multi-turn conversation with system message and history turns using model's chat template
    fn format_chat_history(
        &self,
        system: &str,
        history: &[(&str, &str)],
        user_message: &str,
    ) -> String;

    /// Get model's EOS (End of Sequence) token ID
    fn eos_token_id(&self) -> u32;

    /// Get model's BOS (Beginning of Sequence) token ID (if any)
    fn bos_token_id(&self) -> Option<u32> {
        None
    }

    /// Clean/post-process model output (remove template artifacts, etc.)
    fn clean_output(&self, raw_output: &str) -> String {
        // Default: just trim whitespace
        raw_output.trim().to_string()
    }

    /// Get model family name for logging/debugging
    fn family_name(&self) -> &str;

    /// Get recommended generation parameters
    fn generation_config(&self) -> GenerationConfig {
        GenerationConfig::default()
    }
}

/// Generation configuration parameters
#[derive(Debug, Clone)]
pub struct GenerationConfig {
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: usize,
    pub repetition_penalty: f32,
    pub max_tokens: usize,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_p: 0.9,
            top_k: 50,
            repetition_penalty: 1.1,
            max_tokens: 512,
        }
    }
}

impl fmt::Debug for dyn LocalModelAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LocalModelAdapter({})", self.family_name())
    }
}

/// Registry for looking up adapters by model name
pub struct AdapterRegistry;

impl AdapterRegistry {
    /// Get appropriate adapter for a model by name
    pub fn get_adapter(model_name: &str) -> Box<dyn LocalModelAdapter> {
        let name_lower = model_name.to_lowercase();

        // Check DeepSeek BEFORE Qwen, since "DeepSeek-R1-Distill-Qwen" contains both
        if name_lower.contains("deepseek") {
            Box::new(DeepSeekAdapter)
        } else if name_lower.contains("qwen") {
            Box::new(QwenAdapter)
        } else if name_lower.contains("llama") {
            Box::new(LlamaAdapter)
        } else if name_lower.contains("mistral") {
            Box::new(MistralAdapter)
        } else if name_lower.contains("phi") {
            Box::new(PhiAdapter)
        } else if name_lower.contains("gemma") {
            Box::new(GemmaAdapter)
        } else {
            // Default to ChatML format (Qwen-style) - widely supported
            tracing::warn!(
                "Unknown model family '{}', defaulting to ChatML format",
                model_name
            );
            Box::new(QwenAdapter)
        }
    }
}

/// Parse multi-turn history and final question from a serialized query string.
/// Returns `(history, final_question)`.
pub fn parse_history_from_query(query: &str) -> (Vec<(&str, &str)>, &str) {
    if !query.starts_with("user: ") && !query.starts_with("assistant: ") {
        return (Vec::new(), query);
    }

    let mut turns = Vec::new();
    let mut current_role;
    let mut current_start;

    let len = query.len();
    let mut i;

    if query.starts_with("user: ") {
        current_role = "user";
        current_start = 6;
        i = 6;
    } else if query.starts_with("assistant: ") {
        current_role = "assistant";
        current_start = 11;
        i = 11;
    } else {
        return (Vec::new(), query);
    }

    while i < len {
        if i + 2 <= len && &query[i..i + 2] == "\n\n" {
            let after = &query[i + 2..];
            if after.starts_with("user: ") {
                let content = query[current_start..i].trim();
                turns.push((current_role, content));
                current_role = "user";
                current_start = i + 2 + 6;
                i = current_start;
                continue;
            } else if after.starts_with("assistant: ") {
                let content = query[current_start..i].trim();
                turns.push((current_role, content));
                current_role = "assistant";
                current_start = i + 2 + 11;
                i = current_start;
                continue;
            }
        }
        i += 1;
    }

    let remaining = &query[current_start..];
    if let Some(sep) = remaining.find("\n\n") {
        let content = remaining[..sep].trim();
        let question = remaining[sep + 2..].trim();
        turns.push((current_role, content));
        (turns, question)
    } else {
        (turns, remaining.trim())
    }
}

// #781: this module previously declared a second `ModelFamily` enum (with
// `from_name` and `AdapterRegistry::from_family`) competing with
// `unified_loader::ModelFamily`. Both had no production caller and drifted
// from the loader's family table; the loader enum is now the single
// declaration and adapters are selected by model name via `get_adapter`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_registry() {
        let qwen = AdapterRegistry::get_adapter("Qwen2.5-1.5B-Instruct");
        assert_eq!(qwen.family_name(), "Qwen");

        let llama = AdapterRegistry::get_adapter("Llama-3.1-8B-Instruct");
        assert_eq!(llama.family_name(), "Llama");

        let mistral = AdapterRegistry::get_adapter("Mistral-7B-Instruct-v0.3");
        assert_eq!(mistral.family_name(), "Mistral");

        let phi = AdapterRegistry::get_adapter("Phi-3-mini-4k-instruct");
        assert_eq!(phi.family_name(), "Phi");

        let deepseek = AdapterRegistry::get_adapter("deepseek-coder-6.7b-instruct");
        assert_eq!(deepseek.family_name(), "DeepSeek");

        // Test DeepSeek-R1-Distill-Qwen (contains both "deepseek" and "qwen")
        // Should match DeepSeek, not Qwen
        let deepseek_qwen = AdapterRegistry::get_adapter("DeepSeek-R1-Distill-Qwen-1.5B-GGUF");
        assert_eq!(deepseek_qwen.family_name(), "DeepSeek");

        // Gemma must get its own real chat-template adapter, not Llama's
        // (Gemma has no `<|begin_of_text|>`/`<|eot_id|>` tokens in its vocab).
        let gemma = AdapterRegistry::get_adapter("Gemma 2 9B");
        assert_eq!(gemma.family_name(), "Gemma");
    }

    #[test]
    fn test_parse_history_from_query() {
        // Single turn
        let (history, question) = parse_history_from_query("What is 2+2?");
        assert!(history.is_empty());
        assert_eq!(question, "What is 2+2?");

        // Multi-turn
        let query = "user: earlier turn\n\nassistant: earlier reply\n\ncurrent question";
        let (history, question) = parse_history_from_query(query);
        assert_eq!(
            history,
            vec![("user", "earlier turn"), ("assistant", "earlier reply")]
        );
        assert_eq!(question, "current question");

        // Multi-paragraph question
        let query = "user: q1\n\nassistant: a1\n\npara 1\n\npara 2";
        let (history, question) = parse_history_from_query(query);
        assert_eq!(history, vec![("user", "q1"), ("assistant", "a1")]);
        assert_eq!(question, "para 1\n\npara 2");
    }
}

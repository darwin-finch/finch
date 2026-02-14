// Local Model Adapters - Format prompts and handle model-specific behavior
//
// These adapters handle LOCAL ONNX model specifics (chat templates, tokens, output cleaning).
// This is DIFFERENT from TeacherProviders (src/providers/) which handle external API calls.
//
// LocalModelAdapter: Format prompts for local ONNX inference
// TeacherProvider: Make HTTP requests to external APIs (Claude, OpenAI, etc.)

pub mod qwen;
pub mod llama;
pub mod mistral;
pub mod phi;
pub mod deepseek;

pub use qwen::QwenAdapter;
pub use llama::LlamaAdapter;
pub use mistral::MistralAdapter;
pub use phi::PhiAdapter;
pub use deepseek::DeepSeekAdapter;

use std::fmt;

/// Local model adapter for formatting prompts and handling model-specific behavior
pub trait LocalModelAdapter: Send + Sync {
    /// Format a prompt with system message using model's chat template
    fn format_chat_prompt(&self, system: &str, user_message: &str) -> String;

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
            // Gemma uses similar format to Llama
            Box::new(LlamaAdapter)
        } else {
            // Default to ChatML format (Qwen-style) - widely supported
            tracing::warn!(
                "Unknown model family '{}', defaulting to ChatML format",
                model_name
            );
            Box::new(QwenAdapter)
        }
    }

    /// Get adapter from model family enum
    pub fn from_family(family: ModelFamily) -> Box<dyn LocalModelAdapter> {
        match family {
            ModelFamily::Qwen => Box::new(QwenAdapter),
            ModelFamily::Llama => Box::new(LlamaAdapter),
            ModelFamily::Mistral => Box::new(MistralAdapter),
            ModelFamily::Gemma => Box::new(LlamaAdapter),
            ModelFamily::Phi => Box::new(PhiAdapter),
            ModelFamily::DeepSeek => Box::new(DeepSeekAdapter),
        }
    }
}

/// Supported model families
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFamily {
    Qwen,
    Llama,
    Mistral,
    Gemma,
    Phi,
    DeepSeek,
}

impl ModelFamily {
    pub fn from_name(name: &str) -> Option<Self> {
        let name_lower = name.to_lowercase();

        if name_lower.contains("qwen") {
            Some(ModelFamily::Qwen)
        } else if name_lower.contains("llama") {
            Some(ModelFamily::Llama)
        } else if name_lower.contains("mistral") {
            Some(ModelFamily::Mistral)
        } else if name_lower.contains("phi") {
            Some(ModelFamily::Phi)
        } else if name_lower.contains("deepseek") {
            Some(ModelFamily::DeepSeek)
        } else if name_lower.contains("gemma") {
            Some(ModelFamily::Gemma)
        } else {
            None
        }
    }
}

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
        let deepseek_qwen = AdapterRegistry::get_adapter("DeepSeek-R1-Distill-Qwen-1.5B-ONNX");
        assert_eq!(deepseek_qwen.family_name(), "DeepSeek");
    }

    #[test]
    fn test_model_family_detection() {
        assert_eq!(ModelFamily::from_name("Qwen2.5-1.5B"), Some(ModelFamily::Qwen));
        assert_eq!(ModelFamily::from_name("Llama-3-8B"), Some(ModelFamily::Llama));
        assert_eq!(ModelFamily::from_name("Mistral-7B"), Some(ModelFamily::Mistral));
        assert_eq!(ModelFamily::from_name("Phi-3-mini"), Some(ModelFamily::Phi));
        assert_eq!(ModelFamily::from_name("deepseek-coder-7b"), Some(ModelFamily::DeepSeek));
        assert_eq!(ModelFamily::from_name("unknown-model"), None);
    }
}

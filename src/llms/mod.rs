// Generic LLM abstraction layer
//
// This module provides a unified interface for working with ANY LLM
// (local or remote) as primary, with other LLMs available as tools.

use crate::config::ProviderEntry;
use crate::providers::Message;
use crate::providers::{LlmProvider, ProviderRequest};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

/// Generic LLM trait - works with both local and remote models
#[async_trait::async_trait]
pub trait LLM: Send + Sync {
    /// Get the LLM name (e.g., "Claude", "GPT-4", "Local Qwen")
    fn name(&self) -> &str;

    /// Get the provider type (e.g., "anthropic", "openai", "local")
    fn provider(&self) -> &str;

    /// Get the model identifier (e.g., "claude-3-5-sonnet", "gpt-4-turbo")
    fn model(&self) -> &str;

    /// Generate a response for given messages
    async fn generate(&self, messages: &[Message]) -> Result<String>;

    /// Check if this LLM supports streaming
    fn supports_streaming(&self) -> bool {
        false
    }
}

/// Registry of available LLMs (primary + tools)
pub struct LLMRegistry {
    /// The primary LLM (answers queries by default)
    primary: Arc<dyn LLM>,

    /// Tool LLMs available for delegation (key = tool name, e.g., "claude", "gpt4")
    tools: HashMap<String, Arc<dyn LLM>>,
}

impl LLMRegistry {
    /// Create registry from the configured cloud provider entries.
    pub fn from_cloud_providers(cloud: &[ProviderEntry]) -> Result<Self> {
        if cloud.is_empty() {
            anyhow::bail!("No cloud providers configured - need at least one LLM");
        }

        // First cloud provider is primary
        let primary: Arc<dyn LLM> = Arc::new(create_llm_from_entry(&cloud[0])?);

        // Rest are tools
        let mut tools = HashMap::new();
        for entry in &cloud[1..] {
            let llm: Arc<dyn LLM> = Arc::new(create_llm_from_entry(entry)?);
            let tool_name = entry.profile_name();
            tools.insert(tool_name, llm);
        }

        Ok(Self { primary, tools })
    }

    /// Get the primary LLM
    pub fn primary(&self) -> &dyn LLM {
        self.primary.as_ref()
    }

    /// Get a tool LLM by name (returns Arc for tools)
    pub fn get_tool(&self, name: &str) -> Option<Arc<dyn LLM>> {
        self.tools.get(name).cloned()
    }

    /// List all available tool names
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

/// Wrapper around provider-based LLM
struct ProviderLLM {
    name: String,
    provider: String,
    model: String,
    llm_provider: Box<dyn LlmProvider>,
}

#[async_trait::async_trait]
impl LLM for ProviderLLM {
    fn name(&self) -> &str {
        &self.name
    }

    fn provider(&self) -> &str {
        &self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn generate(&self, messages: &[Message]) -> Result<String> {
        let request = ProviderRequest::new(messages.to_vec())
            .with_model(self.model.clone())
            .with_max_tokens(4096);

        let response = self.llm_provider.send_message(&request).await?;
        Ok(response.text())
    }

    fn supports_streaming(&self) -> bool {
        self.llm_provider.supports_streaming()
    }
}

/// The provider-family name for a simple cloud entry, used for LLM identity.
fn provider_family(entry: &ProviderEntry) -> &'static str {
    match entry {
        ProviderEntry::Claude { .. } => "claude",
        ProviderEntry::Openai { .. } => "openai",
        ProviderEntry::Grok { .. } => "grok",
        ProviderEntry::Gemini { .. } => "gemini",
        ProviderEntry::Mistral { .. } => "mistral",
        ProviderEntry::Groq { .. } => "groq",
        ProviderEntry::Openrouter { .. } => "openrouter",
        _ => "cloud",
    }
}

/// Create an LLM instance from a cloud provider entry
fn create_llm_from_entry(entry: &ProviderEntry) -> Result<ProviderLLM> {
    let provider = create_provider_boxed(entry)?;
    let model = entry
        .model()
        .map(str::to_string)
        .unwrap_or_else(|| provider.default_model().to_string());
    let name = entry.profile_name();

    Ok(ProviderLLM {
        name,
        provider: provider_family(entry).to_string(),
        model,
        llm_provider: provider,
    })
}

fn create_provider_boxed(entry: &ProviderEntry) -> Result<Box<dyn LlmProvider>> {
    crate::providers::create_provider_from_entry(entry)
}

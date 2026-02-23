// Generic LLM abstraction layer
//
// This module provides a unified interface for working with ANY LLM
// (local or remote) as primary, with other LLMs available as tools.

use crate::claude::types::Message;
use crate::config::TeacherEntry;
use crate::providers::{self, LlmProvider, ProviderRequest};
use anyhow::{Context, Result};
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
    /// Create registry from teacher configuration
    pub fn from_teachers(teachers: &[TeacherEntry]) -> Result<Self> {
        if teachers.is_empty() {
            anyhow::bail!("No teachers configured - need at least one LLM");
        }

        // First teacher is primary
        let primary: Arc<dyn LLM> = Arc::new(create_llm_from_teacher(&teachers[0])?);

        // Rest are tools
        let mut tools = HashMap::new();
        for teacher in &teachers[1..] {
            let llm: Arc<dyn LLM> = Arc::new(create_llm_from_teacher(teacher)?);
            let tool_name = teacher
                .name
                .clone()
                .unwrap_or_else(|| teacher.provider.clone());
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

/// Create an LLM instance from a teacher configuration
fn create_llm_from_teacher(teacher: &TeacherEntry) -> Result<ProviderLLM> {
    let provider = providers::factory::create_providers(std::slice::from_ref(teacher))?
        .into_iter()
        .next()
        .context("Failed to create provider")?;

    let model = teacher
        .model
        .clone()
        .unwrap_or_else(|| provider.default_model().to_string());

    let name = teacher
        .name
        .clone()
        .unwrap_or_else(|| format!("{} ({})", teacher.provider, model));

    Ok(ProviderLLM {
        name,
        provider: teacher.provider.clone(),
        model,
        llm_provider: provider,
    })
}

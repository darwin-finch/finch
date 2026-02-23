// LLM delegation tools
//
// These tools allow the primary LLM to delegate queries to other LLMs
// (Claude, GPT-4, Grok, etc.) when needed.

use crate::claude::types::Message;
use crate::llms::LLM;
use crate::tools::types::{ToolContext, ToolInputSchema};
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Tool for delegating to another LLM
pub struct LLMDelegationTool {
    name: String,
    llm: Arc<dyn LLM>,
    description: String,
}

impl LLMDelegationTool {
    /// Create a new delegation tool
    pub fn new(name: impl Into<String>, llm: Arc<dyn LLM>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            llm,
            description: description.into(),
        }
    }

    /// Create tool for Claude delegation
    pub fn for_claude(llm: Arc<dyn LLM>) -> Self {
        Self::new(
            "use_claude",
            llm,
            "Delegate to Claude (Anthropic) for complex reasoning, creative writing, or detailed analysis. Claude is very capable but costs money, so only use when necessary. Best for: complex logic, creative tasks, nuanced understanding.",
        )
    }

    /// Create tool for GPT-4 delegation
    pub fn for_gpt4(llm: Arc<dyn LLM>) -> Self {
        Self::new(
            "use_gpt4",
            llm,
            "Delegate to GPT-4 (OpenAI) for structured outputs, JSON generation, or mathematical reasoning. Good for: data transformation, structured analysis, technical writing.",
        )
    }

    /// Create tool for Grok delegation
    pub fn for_grok(llm: Arc<dyn LLM>) -> Self {
        Self::new(
            "use_grok",
            llm,
            "Delegate to Grok (xAI) for real-time data, current events, or up-to-date information. Best for: news, recent developments, time-sensitive queries.",
        )
    }

    /// Create tool for Gemini delegation
    pub fn for_gemini(llm: Arc<dyn LLM>) -> Self {
        Self::new(
            "use_gemini",
            llm,
            "Delegate to Gemini (Google) for multimodal tasks, large context windows, or Google-specific queries. Best for: long documents, visual understanding, integration with Google services.",
        )
    }

    /// Create tool for DeepSeek delegation
    pub fn for_deepseek(llm: Arc<dyn LLM>) -> Self {
        Self::new(
            "use_deepseek",
            llm,
            "Delegate to DeepSeek for advanced mathematical reasoning, code analysis, or technical problem-solving. Best for: complex math, algorithmic challenges, detailed code review.",
        )
    }
}

#[async_trait]
impl Tool for LLMDelegationTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "query": {
                    "type": "string",
                    "description": "The question or task to send to the LLM"
                },
                "reason": {
                    "type": "string",
                    "description": "Why you're delegating to this LLM (for training and logging)"
                }
            }),
            required: vec!["query".to_string(), "reason".to_string()],
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<String> {
        let query = input["query"]
            .as_str()
            .context("Missing 'query' parameter")?;

        let reason = input["reason"]
            .as_str()
            .context("Missing 'reason' parameter")?;

        tracing::info!(
            "Delegating to {} ({}): {} (reason: {})",
            self.llm.name(),
            self.llm.model(),
            query,
            reason
        );

        // Create a simple message
        let message = Message::user(query.to_string());

        // Generate response
        let response = self
            .llm
            .generate(&[message])
            .await
            .with_context(|| format!("Failed to get response from {}", self.llm.name()))?;

        tracing::debug!("Response from {}: {}", self.llm.name(), response);

        Ok(response)
    }
}

/// Helper to create all LLM delegation tools from registry
pub fn create_llm_tools(registry: &crate::llms::LLMRegistry) -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();

    for tool_name in registry.tool_names() {
        if let Some(llm_arc) = registry.get_tool(&tool_name) {
            let tool: Box<dyn Tool> = match tool_name.as_str() {
                name if name.contains("claude") => {
                    Box::new(LLMDelegationTool::for_claude(llm_arc.clone()))
                }
                name if name.contains("gpt") || name.contains("openai") => {
                    Box::new(LLMDelegationTool::for_gpt4(llm_arc.clone()))
                }
                name if name.contains("grok") => {
                    Box::new(LLMDelegationTool::for_grok(llm_arc.clone()))
                }
                name if name.contains("gemini") => {
                    Box::new(LLMDelegationTool::for_gemini(llm_arc.clone()))
                }
                name if name.contains("deepseek") => {
                    Box::new(LLMDelegationTool::for_deepseek(llm_arc.clone()))
                }
                _ => {
                    // Generic delegation tool
                    let name_str = llm_arc.name().to_string();
                    Box::new(LLMDelegationTool::new(
                        format!("use_{}", tool_name),
                        llm_arc,
                        format!("Delegate to {} for specialized tasks", name_str),
                    ))
                }
            };

            tools.push(tool);
        }
    }

    tools
}

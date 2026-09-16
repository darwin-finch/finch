// Qwen local generator implementation

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::local::LocalGenerator;
use crate::models::{ToolCallParser, ToolPromptFormatter};
use crate::providers::{ContentBlock, Message};
use crate::tools::ToolDefinition;

use super::{
    Generator, GeneratorCapabilities, GeneratorResponse, ResponseMetadata, StreamChunk, ToolUse,
};

/// Qwen local generator implementation.
///
/// Parses local tool markup and returns semantic [`ToolUse`] values. It does
/// not own or invoke a tool executor; the event loop executes tools.
pub struct QwenGenerator {
    local_generator: Arc<RwLock<LocalGenerator>>,
    capabilities: GeneratorCapabilities,
}

impl QwenGenerator {
    pub fn new(local_generator: Arc<RwLock<LocalGenerator>>) -> Self {
        Self {
            local_generator,
            capabilities: GeneratorCapabilities {
                supports_streaming: false,
                supports_tools: true,
                supports_conversation: true,
                max_context_messages: Some(5),
            },
        }
    }
}

#[async_trait]
impl Generator for QwenGenerator {
    async fn generate(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<GeneratorResponse> {
        if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
            return self.generate_proposing_tools(messages, tools).await;
        }
        self.generate_single_turn(messages).await
    }

    async fn generate_stream(
        &self,
        _messages: Vec<Message>,
        _tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>> {
        // Qwen doesn't support streaming
        Ok(None)
    }

    fn capabilities(&self) -> &GeneratorCapabilities {
        &self.capabilities
    }

    fn name(&self) -> &str {
        "Local"
    }
}

impl QwenGenerator {
    /// Generate a single-turn response without tools
    async fn generate_single_turn(&self, messages: Vec<Message>) -> Result<GeneratorResponse> {
        // Extract last user message
        let query = messages
            .last()
            .and_then(|m| {
                // Get text from first content block
                m.content.first().and_then(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
            })
            .ok_or_else(|| anyhow::anyhow!("No user message found"))?;

        // Estimate input tokens (words * ~1.3 tokens/word is a common BPE heuristic)
        let input_token_estimate = (query.split_whitespace().count() as f32 * 1.3) as u32;

        // Generate (blocking, so spawn_blocking)
        let local_generator = Arc::clone(&self.local_generator);
        let query = query.to_string();
        let t0 = std::time::Instant::now();

        let generated = tokio::task::spawn_blocking(move || -> Result<_> {
            // Get write lock synchronously
            let mut gen = local_generator.blocking_write();
            // Use try_generate which returns Option<String>
            match gen.try_generate_from_pattern(&query)? {
                Some(text) => Ok(crate::local::GeneratedResponse {
                    text,
                    method: "local".to_string(),
                    confidence: 0.8, // Default confidence from try_generate
                    pattern: "local".to_string(),
                }),
                None => Err(anyhow::anyhow!("Local generation returned None")),
            }
        })
        .await
        .context("Failed to spawn blocking task for Qwen generation")??;

        let latency_ms = t0.elapsed().as_millis() as u64;
        let output_token_estimate = (generated.text.split_whitespace().count() as f32 * 1.3) as u32;

        // Read the actual model name from LocalGenerator (reflects configured family)
        let model_display_name = {
            let gen = self.local_generator.read().await;
            gen.model_name().to_string()
        };

        Ok(GeneratorResponse {
            text: generated.text.clone(),
            content_blocks: vec![ContentBlock::Text {
                text: generated.text.clone(),
            }],
            tool_uses: vec![],
            metadata: ResponseMetadata {
                generator: "local".to_string(),
                model: model_display_name,
                confidence: Some(generated.confidence),
                stop_reason: None,
                input_tokens: Some(input_token_estimate),
                output_tokens: Some(output_token_estimate),
                latency_ms: Some(latency_ms),
                primary_allowance_used_percent: None,
                secondary_allowance_used_percent: None,
            },
        })
    }

    /// Generate one turn and return parsed tool calls without executing them.
    async fn generate_proposing_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> Result<GeneratorResponse> {
        let prompt = self.format_prompt_with_tools(&messages, &tools)?;
        let output = self.generate_text(&prompt).await?;
        let model_display_name = {
            let gen = self.local_generator.read().await;
            gen.model_name().to_string()
        };

        if !ToolCallParser::has_tool_calls(&output) {
            let text = ToolCallParser::extract_text(&output);
            return Ok(GeneratorResponse {
                text: text.clone(),
                content_blocks: vec![ContentBlock::Text { text: text.clone() }],
                tool_uses: vec![],
                metadata: ResponseMetadata {
                    generator: "local".to_string(),
                    model: model_display_name,
                    confidence: Some(0.8),
                    stop_reason: Some("end_turn".to_string()),
                    input_tokens: None,
                    output_tokens: Some(text.split_whitespace().count() as u32),
                    latency_ms: None,
                    primary_allowance_used_percent: None,
                    secondary_allowance_used_percent: None,
                },
            });
        }

        let parsed = ToolCallParser::parse(&output)
            .context("Failed to parse tool calls from local model output")?;
        let mut content_blocks = Vec::new();
        let text = ToolCallParser::extract_text(&output);
        if !text.is_empty() {
            content_blocks.push(ContentBlock::Text { text: text.clone() });
        }
        let tool_uses: Vec<ToolUse> = parsed
            .into_iter()
            .map(|call| {
                content_blocks.push(ContentBlock::ToolUse {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                });
                ToolUse {
                    id: call.id,
                    name: call.name,
                    input: call.input,
                }
            })
            .collect();

        Ok(GeneratorResponse {
            text,
            content_blocks,
            tool_uses,
            metadata: ResponseMetadata {
                generator: "local".to_string(),
                model: model_display_name,
                confidence: Some(0.8),
                stop_reason: Some("tool_use".to_string()),
                input_tokens: None,
                output_tokens: Some(output.split_whitespace().count() as u32),
                latency_ms: None,
                primary_allowance_used_percent: None,
                secondary_allowance_used_percent: None,
            },
        })
    }

    /// Format prompt with tool definitions in system message
    fn format_prompt_with_tools(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<String> {
        // Build system prompt
        let mut system = String::from("You are Qwen, a helpful AI assistant.\n");
        system.push_str("You can use tools to help answer questions.\n");

        // Add tool definitions
        if !tools.is_empty() {
            system.push_str(&ToolPromptFormatter::format_tools_for_prompt(tools));
        }

        // Extract user query from messages
        // For simplicity, we'll format the last few messages
        let mut conversation = String::new();

        // Take last N messages (limit context)
        let context_limit = self.capabilities.max_context_messages.unwrap_or(5);
        let messages_to_include = messages.iter().rev().take(context_limit).rev();

        for msg in messages_to_include {
            match msg.role.as_str() {
                "user" => {
                    conversation.push_str("User: ");
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text } => {
                                conversation.push_str(text);
                            }
                            ContentBlock::ToolResult {
                                content, is_error, ..
                            } => {
                                if *is_error == Some(true) {
                                    conversation.push_str(&format!("[Tool Error: {}]", content));
                                } else {
                                    conversation.push_str(&format!("[Tool Result: {}]", content));
                                }
                            }
                            _ => {}
                        }
                    }
                    conversation.push_str("\n\n");
                }
                "assistant" => {
                    conversation.push_str("Assistant: ");
                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text } => {
                                conversation.push_str(text);
                            }
                            ContentBlock::ToolUse { name, input, .. } => {
                                conversation.push_str(&format!(
                                    "[Called tool '{}' with params: {}]",
                                    name,
                                    serde_json::to_string(input).unwrap_or_default()
                                ));
                            }
                            _ => {}
                        }
                    }
                    conversation.push_str("\n\n");
                }
                _ => {}
            }
        }

        // Use LocalGenerator's format method (which uses the adapter)
        // For now, we'll construct a simple prompt
        // TODO: Use the adapter's format_chat_prompt method
        let prompt = format!("{}\n\n{}", system, conversation);

        Ok(prompt)
    }

    /// Low-level text generation (synchronous, blocking)
    async fn generate_text(&self, prompt: &str) -> Result<String> {
        let local_generator = Arc::clone(&self.local_generator);
        let prompt = prompt.to_string();

        tokio::task::spawn_blocking(move || -> Result<String> {
            let mut gen = local_generator.blocking_write();
            match gen.try_generate_from_pattern(&prompt)? {
                Some(text) => Ok(text),
                None => Err(anyhow::anyhow!("Local generation returned None")),
            }
        })
        .await
        .context("Failed to spawn blocking task")?
    }
}

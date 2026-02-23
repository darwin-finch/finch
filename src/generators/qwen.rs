// Qwen local generator implementation

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::claude::{ContentBlock, Message};
use crate::local::LocalGenerator;
use crate::models::tokenizer::TextTokenizer;
use crate::models::{ToolCallParser, ToolPromptFormatter};
use crate::tools::executor::ToolExecutor;
use crate::tools::types::ToolUse as ToolsToolUse;
use crate::tools::types::{ToolDefinition, ToolResult}; // Import with alias to avoid confusion

use super::{
    Generator,
    GeneratorCapabilities,
    GeneratorResponse,
    ResponseMetadata,
    StreamChunk,
    ToolUse as GenToolUse, // Use the generator's ToolUse type
};

/// Qwen local generator implementation
pub struct QwenGenerator {
    local_generator: Arc<RwLock<LocalGenerator>>,
    tokenizer: Arc<TextTokenizer>,
    tool_executor: Option<Arc<tokio::sync::Mutex<ToolExecutor>>>,
    capabilities: GeneratorCapabilities,
}

impl QwenGenerator {
    pub fn new(
        local_generator: Arc<RwLock<LocalGenerator>>,
        tokenizer: Arc<TextTokenizer>,
        tool_executor: Option<Arc<tokio::sync::Mutex<ToolExecutor>>>,
    ) -> Self {
        let supports_tools = tool_executor.is_some();

        Self {
            local_generator,
            tokenizer,
            tool_executor,
            capabilities: GeneratorCapabilities {
                supports_streaming: false,             // Qwen blocks (for now)
                supports_tools,                        // Enable if executor provided
                supports_conversation: supports_tools, // Enable multi-turn if tools enabled
                max_context_messages: Some(5),         // Limit context to prevent token overflow
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
        // If tools provided and executor available, use multi-turn tool loop
        if let Some(tools) = tools {
            if self.tool_executor.is_some() {
                return self.generate_with_tools(messages, tools).await;
            }
        }
        // Simple single-turn generation (no tools)
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
            },
        })
    }

    /// Generate with tool support (multi-turn loop)
    async fn generate_with_tools(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> Result<GeneratorResponse> {
        let max_turns = 5; // Prevent infinite loops
        let mut conversation_history = messages;
        let mut all_tool_uses: Vec<GenToolUse> = Vec::new(); // Use generator's ToolUse type

        for turn in 0..max_turns {
            tracing::debug!("Tool execution turn {}/{}", turn + 1, max_turns);

            // 1. Format prompt with tools in system message
            let prompt = self.format_prompt_with_tools(&conversation_history, &tools)?;

            // 2. Generate response
            let output = self.generate_text(&prompt).await?;

            tracing::debug!(
                "Generated output ({} chars): {}",
                output.len(),
                &output[..output.len().min(100)]
            );

            // 3. Check for tool calls
            if !ToolCallParser::has_tool_calls(&output) {
                // No tools → final answer
                let text = ToolCallParser::extract_text(&output);
                tracing::info!("No tool calls found, returning final answer");

                let model_display_name = {
                    let gen = self.local_generator.read().await;
                    gen.model_name().to_string()
                };
                return Ok(GeneratorResponse {
                    text: text.clone(),
                    content_blocks: vec![ContentBlock::Text { text: text.clone() }],
                    tool_uses: all_tool_uses,
                    metadata: ResponseMetadata {
                        generator: "local".to_string(),
                        model: model_display_name,
                        confidence: Some(0.8),
                        stop_reason: Some("end_turn".to_string()),
                        input_tokens: None,
                        output_tokens: Some(text.split_whitespace().count() as u32),
                        latency_ms: None,
                    },
                });
            }

            // 4. Parse tool calls (returns tools::types::ToolUse)
            let tool_calls: Vec<ToolsToolUse> =
                ToolCallParser::parse(&output).context("Failed to parse tool calls from output")?;

            tracing::info!("Parsed {} tool call(s)", tool_calls.len());

            // Convert tools::types::ToolUse to generators::ToolUse
            for tc in &tool_calls {
                all_tool_uses.push(GenToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.input.clone(),
                });
            }

            // 5. Execute tools
            let tool_results = self.execute_tools(&tool_calls).await?;

            // 6. Add assistant message with tool_use blocks
            let assistant_content: Vec<ContentBlock> = tool_calls
                .iter()
                .map(|tu| ContentBlock::ToolUse {
                    id: tu.id.clone(),
                    name: tu.name.clone(),
                    input: tu.input.clone(),
                })
                .collect();

            conversation_history.push(Message {
                role: "assistant".to_string(),
                content: assistant_content,
            });

            // 7. Add user message with tool_result blocks
            let result_content: Vec<ContentBlock> = tool_results
                .iter()
                .map(|tr| ContentBlock::ToolResult {
                    tool_use_id: tr.tool_use_id.clone(),
                    content: tr.content.clone(),
                    is_error: Some(tr.is_error),
                })
                .collect();

            conversation_history.push(Message {
                role: "user".to_string(),
                content: result_content,
            });

            // Loop: continue with updated conversation
        }

        // Max turns exceeded
        Err(anyhow::anyhow!(
            "Maximum tool use turns ({}) exceeded",
            max_turns
        ))
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

    /// Execute a list of tool calls
    async fn execute_tools(&self, tool_calls: &[ToolsToolUse]) -> Result<Vec<ToolResult>> {
        let tool_executor = self
            .tool_executor
            .as_ref()
            .context("Tool executor not available")?;

        let mut results = Vec::new();

        let executor = tool_executor.lock().await;

        for tool_use in tool_calls {
            tracing::info!("Executing tool: {} ({})", tool_use.name, tool_use.id);

            // Execute tool (note: ToolExecutor has execute_tool method)
            // For now, we'll use a simplified call
            let result = executor
                .execute_tool(
                    tool_use,
                    None,                                    // conversation
                    None::<fn() -> Result<()>>,              // save_models_fn
                    None,                                    // batch_trainer
                    Some(Arc::clone(&self.local_generator)), // local_generator (for query_local tool)
                    Some(Arc::clone(&self.tokenizer)),       // tokenizer
                    None,                                    // repl_mode
                    None,                                    // plan_content
                )
                .await
                .unwrap_or_else(|e| {
                    ToolResult::error(tool_use.id.clone(), format!("Tool execution failed: {}", e))
                });

            results.push(result);
        }

        Ok(results)
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

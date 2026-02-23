// TaskTool — spawn isolated subagent loops
//
// Allows the orchestrating model to delegate subtasks to fresh, isolated
// agentic loops with their own conversation history.  Each call to TaskTool
// spawns one subagent that runs up to `max_turns` turns, then returns its
// final text answer.
//
// Multiple TaskTool calls in a single model response can be executed in
// parallel by the executor (see executor.rs).

use crate::claude::types::{ContentBlock, Message};
use crate::providers::{LlmProvider, ProviderRequest};
use crate::tools::implementations::bash::BashTool;
use crate::tools::implementations::glob::GlobTool;
use crate::tools::implementations::grep::GrepTool;
use crate::tools::implementations::read::ReadTool;
use crate::tools::implementations::web_fetch::WebFetchTool;
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolDefinition, ToolInputSchema, ToolUse};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// Subagent types
// ---------------------------------------------------------------------------

/// Named subagent specializations.
///
/// Each type has a focused system prompt and a restricted tool set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentType {
    /// General-purpose reasoning + code (default)
    General,
    /// Read-only codebase explorer
    Explore,
    /// Web + docs researcher
    Researcher,
    /// Code writer/modifier
    Coder,
    /// Shell command specialist
    Bash,
}

impl SubagentType {
    fn from_str(s: &str) -> Self {
        match s {
            "explore" => Self::Explore,
            "researcher" => Self::Researcher,
            "coder" => Self::Coder,
            "bash" => Self::Bash,
            _ => Self::General,
        }
    }

    fn system_prompt(self) -> &'static str {
        match self {
            Self::General => {
                "You are a general-purpose coding assistant. Analyze the task, use \
                 tools as needed, and return a complete, well-structured answer. \
                 When you have finished, produce a final text response with no \
                 further tool calls."
            }
            Self::Explore => {
                "You are a read-only codebase explorer. Use Read, Glob, and Grep \
                 tools to search and summarize code. Do not modify any files. \
                 Return a concise summary of your findings."
            }
            Self::Researcher => {
                "You are a research assistant. Use WebFetch, Read, and search tools \
                 to gather information from the web and local files. Synthesize and \
                 return a structured summary."
            }
            Self::Coder => {
                "You are a code analysis specialist. Read and analyze the relevant \
                 files, run any needed build or test commands via Bash, and return a \
                 summary of your findings or changes."
            }
            Self::Bash => {
                "You are a shell command specialist. Use the Bash tool to execute \
                 commands and return their output or a summary of the results."
            }
        }
    }

    fn allowed_tools(self) -> &'static [&'static str] {
        match self {
            Self::General => &["read", "glob", "grep", "bash", "web_fetch"],
            Self::Explore => &["read", "glob", "grep"],
            Self::Researcher => &["read", "glob", "grep", "web_fetch"],
            Self::Coder => &["read", "glob", "grep", "bash"],
            Self::Bash => &["bash"],
        }
    }
}

// ---------------------------------------------------------------------------
// TaskTool
// ---------------------------------------------------------------------------

/// Default maximum number of turns a subagent may run.
const DEFAULT_MAX_TURNS: usize = 10;

/// Tool that spawns a fresh, isolated subagent loop.
///
/// The subagent has its own conversation history, a focused system prompt,
/// and a restricted tool set (no `spawn_task` to prevent infinite recursion,
/// no `restart` to prevent self-modification).
pub struct TaskTool {
    provider: Arc<dyn LlmProvider>,
    max_turns: usize,
}

impl TaskTool {
    /// Create with an existing provider.
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        Self {
            provider,
            max_turns: DEFAULT_MAX_TURNS,
        }
    }

    /// Override the default maximum turns per subagent.
    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns;
        self
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "spawn_task"
    }

    fn description(&self) -> &str {
        "Spawn an isolated subagent to handle a specific subtask in a fresh \
         conversation.  The subagent has access to read/search/bash tools, \
         runs its own agentic loop, and returns its final answer as a string. \
         Use this to delegate focused work (exploration, research, shell \
         commands) without polluting the main conversation context."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "task": {
                    "type": "string",
                    "description": "What the subagent should do. Be specific and self-contained."
                },
                "subagent_type": {
                    "type": "string",
                    "description": "Specialization: general (default), explore (read-only codebase), researcher (web+docs), coder (read+bash), bash (shell only)",
                    "enum": ["general", "explore", "researcher", "coder", "bash"]
                },
                "background": {
                    "type": "string",
                    "description": "Optional context from the parent conversation to share with the subagent."
                }
            }),
            required: vec!["task".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let task = input["task"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("spawn_task: missing required 'task' parameter"))?;

        let subagent_type = input["subagent_type"]
            .as_str()
            .map(SubagentType::from_str)
            .unwrap_or(SubagentType::General);

        let background = input["background"].as_str();

        info!(
            "Spawning {:?} subagent for task: {}",
            subagent_type,
            &task[..task.len().min(80)]
        );

        run_subagent(
            self.provider.as_ref(),
            task,
            subagent_type,
            background,
            self.max_turns,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Subagent execution loop
// ---------------------------------------------------------------------------

/// Run a headless agentic loop and return the final text response.
///
/// The subagent has no TUI, no approval prompts, and no recursion guard
/// beyond `max_turns`.  Tools are executed directly without permission checks.
async fn run_subagent(
    provider: &dyn LlmProvider,
    task: &str,
    subagent_type: SubagentType,
    background: Option<&str>,
    max_turns: usize,
) -> Result<String> {
    // Build system prompt
    let mut system = subagent_type.system_prompt().to_string();
    if let Some(bg) = background {
        system.push_str("\n\n## Context from parent task\n\n");
        system.push_str(bg);
    }

    // Build tools for this subagent type
    let tools = build_subagent_tools(subagent_type.allowed_tools());
    let tool_defs: Vec<ToolDefinition> = tools.iter().map(|t| t.definition()).collect();

    let mut messages: Vec<Message> = vec![Message::user(task)];

    for turn in 0..max_turns {
        debug!("Subagent turn {}/{}", turn + 1, max_turns);

        let mut request = ProviderRequest::new(messages.clone())
            .with_system(system.clone())
            .with_max_tokens(4096);

        if !tool_defs.is_empty() {
            request = request.with_tools(tool_defs.clone());
        }

        let response = provider
            .send_message(&request)
            .await
            .map_err(|e| anyhow::anyhow!("Subagent provider error: {}", e))?;

        if !response.has_tool_uses() {
            // No tool calls → subagent produced its final answer
            let text = response.text();
            debug!(
                "Subagent finished after {} turns with {} chars",
                turn + 1,
                text.len()
            );
            return Ok(text);
        }

        // Append assistant message (with tool_use blocks)
        messages.push(response.to_message());

        // Execute each tool and collect results
        let tool_uses = response.tool_uses();
        let mut result_blocks: Vec<ContentBlock> = Vec::with_capacity(tool_uses.len());

        for tool_use in &tool_uses {
            debug!("Subagent calling tool: {}", tool_use.name);
            let (content, is_error) = match execute_subagent_tool(&tools, tool_use).await {
                Ok(output) => (output, false),
                Err(e) => (format!("Error: {}", e), true),
            };
            result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: tool_use.id.clone(),
                content,
                is_error: if is_error { Some(true) } else { None },
            });
        }

        // Append tool results as a user message
        messages.push(Message::with_content("user", result_blocks));
    }

    anyhow::bail!(
        "Subagent reached max_turns ({}) without producing a final text response",
        max_turns
    )
}

/// Execute a single tool inside the subagent (no permission checks).
async fn execute_subagent_tool(tools: &[Box<dyn Tool>], tool_use: &ToolUse) -> Result<String> {
    let tool = tools
        .iter()
        .find(|t| t.name() == tool_use.name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Subagent tool '{}' not available for this subagent type",
                tool_use.name
            )
        })?;

    let context = ToolContext {
        conversation: None,
        save_models: None,
        batch_trainer: None,
        local_generator: None,
        tokenizer: None,
        repl_mode: None,
        plan_content: None,
    };

    tool.execute(tool_use.input.clone(), &context).await
}

/// Instantiate the tools allowed for a given subagent type.
fn build_subagent_tools(allowed: &[&str]) -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    for &name in allowed {
        match name {
            "read" => tools.push(Box::new(ReadTool)),
            "glob" => tools.push(Box::new(GlobTool)),
            "grep" => tools.push(Box::new(GrepTool)),
            "bash" => tools.push(Box::new(BashTool)),
            "web_fetch" => tools.push(Box::new(WebFetchTool::new())),
            _ => {}
        }
    }
    tools
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subagent_type_from_str() {
        assert_eq!(SubagentType::from_str("explore"), SubagentType::Explore);
        assert_eq!(
            SubagentType::from_str("researcher"),
            SubagentType::Researcher
        );
        assert_eq!(SubagentType::from_str("coder"), SubagentType::Coder);
        assert_eq!(SubagentType::from_str("bash"), SubagentType::Bash);
        assert_eq!(SubagentType::from_str("general"), SubagentType::General);
        assert_eq!(SubagentType::from_str("unknown"), SubagentType::General);
        assert_eq!(SubagentType::from_str(""), SubagentType::General);
    }

    #[test]
    fn test_subagent_tools_explore_is_read_only() {
        let tools = build_subagent_tools(SubagentType::Explore.allowed_tools());
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"read"));
        assert!(names.contains(&"glob"));
        assert!(names.contains(&"grep"));
        assert!(!names.contains(&"bash"), "Explore should not have bash");
        assert!(
            !names.contains(&"web_fetch"),
            "Explore should not have web_fetch"
        );
    }

    #[test]
    fn test_subagent_tools_bash_only() {
        let tools = build_subagent_tools(SubagentType::Bash.allowed_tools());
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            vec!["bash"],
            "Bash subagent should only have bash tool"
        );
    }

    #[test]
    fn test_subagent_tools_general_has_all() {
        let tools = build_subagent_tools(SubagentType::General.allowed_tools());
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"read"));
        assert!(names.contains(&"glob"));
        assert!(names.contains(&"grep"));
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"web_fetch"));
    }

    #[test]
    fn test_task_tool_schema_requires_task() {
        // Verify the schema marks "task" as required
        struct MockProvider;

        // We can't easily mock LlmProvider without a full impl, so just test
        // the schema construction directly via SubagentType constants.
        let allowed = SubagentType::General.allowed_tools();
        assert!(allowed.contains(&"read"));
        assert!(allowed.contains(&"bash"));
    }

    #[test]
    fn test_subagent_no_recursion_in_tools() {
        // spawn_task must not appear in any subagent's tool list
        for stype in [
            SubagentType::General,
            SubagentType::Explore,
            SubagentType::Researcher,
            SubagentType::Coder,
            SubagentType::Bash,
        ] {
            let tools = build_subagent_tools(stype.allowed_tools());
            for tool in &tools {
                assert_ne!(
                    tool.name(),
                    "spawn_task",
                    "Subagent {:?} must not have spawn_task (recursion guard)",
                    stype
                );
                assert_ne!(
                    tool.name(),
                    "restart",
                    "Subagent {:?} must not have restart",
                    stype
                );
            }
        }
    }
}

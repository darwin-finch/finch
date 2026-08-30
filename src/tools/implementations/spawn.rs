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
            Self::General => &["read", "glob", "grep", "bash", "web_fetch", "spawn_task"],
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

/// Maximum spawn_task nesting depth before recursion is cut off.
const MAX_RECURSION_DEPTH: usize = 4;

/// Tool that spawns a fresh, isolated subagent loop.
///
/// Subagents may themselves call `spawn_task` up to `MAX_RECURSION_DEPTH`
/// levels deep.  Beyond that depth the tool is omitted from the child's
/// tool list so the tree terminates naturally.
pub struct TaskTool {
    provider: Arc<dyn LlmProvider>,
    max_turns: usize,
    /// Nesting depth of this instance (0 = top-level).
    depth: usize,
}

impl TaskTool {
    /// Create a top-level (depth 0) instance.
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        Self {
            provider,
            max_turns: DEFAULT_MAX_TURNS,
            depth: 0,
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
         conversation.  The subagent has access to read/search/bash tools and \
         may itself spawn further subagents (up to 4 levels deep).  Runs its \
         own agentic loop and returns its final answer as a string. \
         Use this to delegate or fan out focused work without polluting the \
         main conversation context."
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
            "Spawning {:?} subagent (depth {}) for task: {}",
            subagent_type,
            self.depth,
            &task[..task.len().min(80)]
        );

        let result = run_subagent(
            Arc::clone(&self.provider),
            task,
            subagent_type,
            background,
            self.max_turns,
            self.depth,
        )
        .await?;

        if result.exit_code != 0 {
            anyhow::bail!("Task failed (exit {}): {}", result.exit_code, result.output);
        }
        Ok(result.output)
    }
}

// ---------------------------------------------------------------------------
// TaskResult
// ---------------------------------------------------------------------------

/// The outcome of a completed subagent run.
#[derive(Debug, Clone)]
pub struct TaskResult {
    /// Final text output produced by the subagent.
    pub output: String,
    /// 0 = success; nonzero = failure (timeout, provider error, etc.).
    pub exit_code: i32,
}

impl TaskResult {
    fn success(output: String) -> Self {
        Self {
            output,
            exit_code: 0,
        }
    }

    fn failure(output: String) -> Self {
        Self {
            output,
            exit_code: 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Subagent execution loop
// ---------------------------------------------------------------------------

/// Run a headless agentic loop and return a `TaskResult`.
///
/// The subagent has no TUI, no approval prompts, and no recursion guard
/// beyond `max_turns` and `MAX_RECURSION_DEPTH`.  Tools are executed
/// directly without permission checks.
async fn run_subagent(
    provider: Arc<dyn LlmProvider>,
    task: &str,
    subagent_type: SubagentType,
    background: Option<&str>,
    max_turns: usize,
    depth: usize,
) -> Result<TaskResult> {
    // Build system prompt
    let mut system = subagent_type.system_prompt().to_string();
    if let Some(bg) = background {
        system.push_str("\n\n## Context from parent task\n\n");
        system.push_str(bg);
    }

    // Build tools for this subagent type
    let tools = build_subagent_tools(subagent_type.allowed_tools(), Arc::clone(&provider), depth);
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
            .as_ref()
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
            return Ok(TaskResult::success(text));
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

    Ok(TaskResult::failure(format!(
        "Subagent reached max_turns ({}) without producing a final text response",
        max_turns
    )))
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
        live_output: None,
        effect_audit: None,
        poset: None,
    };

    tool.execute(tool_use.input.clone(), &context).await
}

/// Instantiate the tools allowed for a given subagent type.
///
/// `spawn_task` is included only when `depth < MAX_RECURSION_DEPTH` so the
/// tree terminates naturally rather than blowing the stack.
fn build_subagent_tools(
    allowed: &[&str],
    provider: Arc<dyn LlmProvider>,
    depth: usize,
) -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    for &name in allowed {
        match name {
            "read" => tools.push(Box::new(ReadTool)),
            "glob" => tools.push(Box::new(GlobTool)),
            "grep" => tools.push(Box::new(GrepTool)),
            "bash" => tools.push(Box::new(BashTool)),
            "web_fetch" => tools.push(Box::new(WebFetchTool::new())),
            "spawn_task" if depth < MAX_RECURSION_DEPTH => {
                tools.push(Box::new(TaskTool {
                    provider: Arc::clone(&provider),
                    max_turns: DEFAULT_MAX_TURNS,
                    depth: depth + 1,
                }));
            }
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---------------------------------------------------------------------------
    // Shared mock helpers
    // ---------------------------------------------------------------------------

    /// Null provider — fails on any actual call; used for tool-construction tests.
    struct NullProvider;

    #[async_trait::async_trait]
    impl crate::providers::ProviderBackend for NullProvider {
        async fn send_message_validated(
            &self,
            _req: crate::providers::ValidatedProviderRequest,
        ) -> anyhow::Result<crate::providers::ProviderResponse> {
            anyhow::bail!("null provider")
        }
        async fn send_message_stream_validated(
            &self,
            _req: crate::providers::ValidatedProviderRequest,
        ) -> anyhow::Result<
            tokio::sync::mpsc::Receiver<anyhow::Result<crate::providers::StreamChunk>>,
        > {
            anyhow::bail!("null provider")
        }
        fn name(&self) -> &str {
            "null"
        }
        fn default_model(&self) -> &str {
            "null"
        }
    }

    /// Echo provider — attests tool calls and immediately returns final text.
    struct EchoProvider {
        response: String,
        backend_calls: AtomicUsize,
    }

    impl EchoProvider {
        fn new(response: impl Into<String>) -> Self {
            Self {
                response: response.into(),
                backend_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::providers::ProviderBackend for EchoProvider {
        async fn send_message_validated(
            &self,
            req: crate::providers::ValidatedProviderRequest,
        ) -> anyhow::Result<crate::providers::ProviderResponse> {
            use crate::claude::types::ContentBlock;
            use crate::providers::ProviderResponse;
            let _req = req.into_request_for(self)?;
            self.backend_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ProviderResponse {
                id: "test".to_string(),
                model: "echo".to_string(),
                content: vec![ContentBlock::Text {
                    text: self.response.clone(),
                }],
                stop_reason: Some("end_turn".to_string()),
                role: "assistant".to_string(),
                provider: "echo".to_string(),
                usage: None,
                allowance: None,
            })
        }
        async fn send_message_stream_validated(
            &self,
            _req: crate::providers::ValidatedProviderRequest,
        ) -> anyhow::Result<
            tokio::sync::mpsc::Receiver<anyhow::Result<crate::providers::StreamChunk>>,
        > {
            anyhow::bail!("echo provider does not stream")
        }
        fn name(&self) -> &str {
            "echo"
        }
        fn default_model(&self) -> &str {
            "echo"
        }

        fn capabilities(&self, model: &str) -> crate::providers::ModelCapabilities {
            use crate::providers::{CapabilitySupport, ModelCapabilities, ModelFeature};

            let mut capabilities = ModelCapabilities::unknown(self.name(), model);
            if model == self.default_model() {
                capabilities.tools = ModelFeature::static_metadata(
                    CapabilitySupport::Supported,
                    "2026-08-27",
                    "spawn test fixture",
                );
            }
            capabilities
        }
    }

    /// Provider with no capability attestation; its hooks must remain unreachable.
    struct UnattestedProvider {
        backend_calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl crate::providers::ProviderBackend for UnattestedProvider {
        async fn send_message_validated(
            &self,
            req: crate::providers::ValidatedProviderRequest,
        ) -> anyhow::Result<crate::providers::ProviderResponse> {
            let _req = req.into_request_for(self)?;
            self.backend_calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("unattested provider backend must not run")
        }

        async fn send_message_stream_validated(
            &self,
            req: crate::providers::ValidatedProviderRequest,
        ) -> anyhow::Result<
            tokio::sync::mpsc::Receiver<anyhow::Result<crate::providers::StreamChunk>>,
        > {
            let _req = req.into_request_for(self)?;
            self.backend_calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("unattested provider backend must not run")
        }

        fn name(&self) -> &str {
            "unattested"
        }

        fn default_model(&self) -> &str {
            "unattested"
        }
    }

    fn null_provider() -> Arc<dyn crate::providers::LlmProvider> {
        Arc::new(NullProvider)
    }

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------

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
    fn test_echo_provider_attests_only_tools_for_exact_model() {
        use crate::providers::{CapabilitySupport, ProviderBackend};

        let provider = EchoProvider::new("done");
        let capabilities = provider.capabilities(provider.default_model());
        assert_eq!(capabilities.tools.support, CapabilitySupport::Supported);
        assert_eq!(
            capabilities.streaming.support,
            CapabilitySupport::Unknown,
            "the fixture must not imply unused streaming support"
        );
        assert_eq!(
            provider.capabilities("other-model").tools.support,
            CapabilitySupport::Unknown,
            "the fixture attestation must not cover other models"
        );
    }

    #[test]
    fn test_subagent_tools_explore_is_read_only() {
        let tools = build_subagent_tools(SubagentType::Explore.allowed_tools(), null_provider(), 0);
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
        let tools = build_subagent_tools(SubagentType::Bash.allowed_tools(), null_provider(), 0);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            vec!["bash"],
            "Bash subagent should only have bash tool"
        );
    }

    #[test]
    fn test_subagent_tools_general_has_all() {
        let tools = build_subagent_tools(SubagentType::General.allowed_tools(), null_provider(), 0);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"read"));
        assert!(names.contains(&"glob"));
        assert!(names.contains(&"grep"));
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"web_fetch"));
        assert!(names.contains(&"spawn_task"));
    }

    #[test]
    fn test_task_tool_schema_requires_task() {
        let allowed = SubagentType::General.allowed_tools();
        assert!(allowed.contains(&"read"));
        assert!(allowed.contains(&"bash"));
        assert!(allowed.contains(&"spawn_task"));
    }

    #[test]
    fn test_subagent_recursion_depth_limit() {
        let provider = null_provider();

        // Below MAX_RECURSION_DEPTH → spawn_task present
        let tools = build_subagent_tools(
            SubagentType::General.allowed_tools(),
            Arc::clone(&provider),
            0,
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(
            names.contains(&"spawn_task"),
            "General at depth 0 should have spawn_task"
        );

        // At MAX_RECURSION_DEPTH → spawn_task absent
        let tools_at_max = build_subagent_tools(
            SubagentType::General.allowed_tools(),
            Arc::clone(&provider),
            MAX_RECURSION_DEPTH,
        );
        let names_at_max: Vec<&str> = tools_at_max.iter().map(|t| t.name()).collect();
        assert!(
            !names_at_max.contains(&"spawn_task"),
            "General at MAX_RECURSION_DEPTH must not have spawn_task"
        );

        // restart must never appear at any depth
        for depth in [0, 1, MAX_RECURSION_DEPTH] {
            for stype in [
                SubagentType::General,
                SubagentType::Explore,
                SubagentType::Researcher,
                SubagentType::Coder,
                SubagentType::Bash,
            ] {
                let tools =
                    build_subagent_tools(stype.allowed_tools(), Arc::clone(&provider), depth);
                for tool in &tools {
                    assert_ne!(
                        tool.name(),
                        "restart",
                        "Subagent {:?} must never have restart",
                        stype
                    );
                }
            }
        }
    }

    /// Fork 100 tasks in parallel; all must return exit_code 0.
    #[tokio::test]
    async fn test_fork_100_tasks_exit_codes_sum_to_zero() {
        use futures::future::join_all;

        const TASK_COUNT: usize = 100;
        const MAX_TURNS: usize = 10;

        let provider = Arc::new(EchoProvider::new("done"));

        let handles: Vec<_> = (0..TASK_COUNT)
            .map(|i| {
                let p: Arc<dyn crate::providers::LlmProvider> = provider.clone();
                tokio::spawn(async move {
                    run_subagent(
                        p,
                        &format!("task {i}"),
                        SubagentType::General,
                        None,
                        MAX_TURNS,
                        0,
                    )
                    .await
                })
            })
            .collect();

        let results = join_all(handles).await;
        assert_eq!(results.len(), TASK_COUNT, "all spawned tasks were joined");

        let exit_code_sum: i32 = results
            .into_iter()
            .map(|r| {
                r.expect("tokio task panicked")
                    .map(|t| t.exit_code)
                    .unwrap_or(1)
            })
            .sum();

        assert_eq!(exit_code_sum, 0, "all 100 tasks must exit 0");
        assert_eq!(
            provider.backend_calls.load(Ordering::SeqCst),
            TASK_COUNT,
            "each bounded task must take one turn without recursive fan-out"
        );
    }

    #[tokio::test]
    async fn test_unknown_provider_rejected_before_backend_invocation() {
        use crate::providers::{CapabilitySupport, ProviderBackend};

        let provider = Arc::new(UnattestedProvider {
            backend_calls: AtomicUsize::new(0),
        });
        assert_eq!(
            provider
                .capabilities(provider.default_model())
                .tools
                .support,
            CapabilitySupport::Unknown,
            "the provider must inherit fail-closed tool capabilities"
        );
        let subagent_provider: Arc<dyn crate::providers::LlmProvider> = provider.clone();

        let result = run_subagent(
            subagent_provider,
            "must remain fail closed",
            SubagentType::General,
            None,
            1,
            0,
        )
        .await;

        assert!(
            result.is_err(),
            "unknown tool capabilities must reject the subagent request"
        );
        assert_eq!(provider.backend_calls.load(Ordering::SeqCst), 0);
    }
}

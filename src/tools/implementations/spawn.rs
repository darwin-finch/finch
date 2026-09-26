// TaskTool — spawn isolated subagent loops
//
// Allows the orchestrating model to delegate subtasks to fresh, isolated
// agentic loops with their own conversation history.  Each call to TaskTool
// spawns one subagent that runs up to `max_turns` turns, then returns its
// final text answer.
//
// Multiple TaskTool calls in a single model response can be executed in
// parallel by the executor (see executor.rs).

use crate::config::{Config, ProviderEntry};
use crate::providers::create_provider_profile_from_config;
use crate::providers::{ContentBlock, Message};
use crate::providers::{LlmProvider, ProviderRequest};
use crate::tools::implementations::bash::BashTool;
use crate::tools::implementations::glob::GlobTool;
use crate::tools::implementations::grep::GrepTool;
use crate::tools::implementations::read::ReadTool;
use crate::tools::implementations::web_fetch::WebFetchTool;
use crate::tools::types::{ToolContext, ToolDefinition, ToolInputSchema, ToolUse};
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use finch_programs::ExecutionEffect;
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
    /// Provider used when the caller omits `provider`: the same provider
    /// already driving the calling turn (or, for a nested `spawn_task`
    /// inside a subagent, the provider that subagent itself is running on).
    default_provider: Arc<dyn LlmProvider>,
    /// Unified configuration, consulted at `execute()` time to look up a
    /// named provider profile (the same names `/providers` lists) and used
    /// to build `description` from the providers actually configured.
    config: Arc<Config>,
    /// Tool description including the live list of configured provider
    /// profile names. Computed once, from `config`, when this `TaskTool` is
    /// constructed (session start, or one recursion level down inside
    /// `build_subagent_tools`) rather than literally per dispatch: the
    /// `Tool::description` contract returns `&str` borrowed from `&self`,
    /// so there is nowhere to materialize a freshly formatted `String` on
    /// every call. A `TaskTool` never outlives the `config` it was built
    /// from, so this stays accurate for the tool instance's lifetime.
    description: String,
    max_turns: usize,
    /// Nesting depth of this instance (0 = top-level).
    depth: usize,
}

impl TaskTool {
    /// Create a top-level (depth 0) instance.
    pub fn new(default_provider: Arc<dyn LlmProvider>, config: Arc<Config>) -> Self {
        let description = build_description(&config);
        Self {
            default_provider,
            config,
            description,
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

/// Build the tool description, inlining the live list of configured
/// provider profile names (the same names `/providers` shows) so the
/// calling model sees valid `provider` values without a separate lookup
/// round-trip.
///
/// Only profiles constructible as an [`LlmProvider`] (everything except
/// `ProviderEntry::Local`) are listed as selectable: on-device local model
/// profiles run through a separate generator runtime this tool does not
/// drive, so they are named but marked unavailable here rather than
/// silently omitted.
fn build_description(config: &Config) -> String {
    let mut selectable: Vec<String> = Vec::new();
    let mut local_only: Vec<String> = Vec::new();
    for entry in &config.providers {
        if entry.is_local() {
            local_only.push(entry.profile_name());
        } else {
            selectable.push(entry.profile_name());
        }
    }

    let mut description = String::from(
        "Spawn an isolated subagent to handle a specific subtask in a fresh \
         conversation.  The subagent has access to read/search/bash tools and \
         may itself spawn further subagents (up to 4 levels deep).  Runs its \
         own agentic loop and returns its final answer as a string. \
         Use this to delegate or fan out focused work without polluting the \
         main conversation context. \
         Optional 'provider': run the subagent against a specific configured \
         provider profile instead of the one driving this conversation.",
    );
    if selectable.is_empty() {
        description
            .push_str(" No other provider profiles are configured; omit 'provider' to proceed.");
    } else {
        description.push_str(&format!(
            " Available providers: {}. Omit 'provider' to use the current provider.",
            selectable.join(", ")
        ));
    }
    if !local_only.is_empty() {
        description.push_str(&format!(
            " On-device local model profiles ({}) are configured but not selectable by name \
             here yet; they run through a separate local-generator path this tool does not use.",
            local_only.join(", ")
        ));
    }
    description
}

/// Resolve a `provider` argument to a constructed [`LlmProvider`], matching
/// profile names case-insensitively the way `/model` selection does.
///
/// Fails closed with an actionable message (naming the requested profile and
/// why it was rejected) rather than falling back to the default provider or
/// panicking: an unconfigured or unsupported name is a caller mistake the
/// model should see and correct, not a silent substitution.
fn resolve_named_provider(config: &Config, name: &str) -> Result<Arc<dyn LlmProvider>> {
    let matches: Vec<&ProviderEntry> = config
        .providers
        .iter()
        .filter(|entry| entry.profile_name().eq_ignore_ascii_case(name))
        .collect();
    let entry = match matches.as_slice() {
        [] => anyhow::bail!(
            "spawn_task: provider '{name}' is not configured; run /providers to see configured \
             provider profiles"
        ),
        [entry] => *entry,
        _ => anyhow::bail!(
            "spawn_task: provider '{name}' is ambiguous; multiple configured profiles share \
             that name"
        ),
    };
    if entry.is_local() {
        anyhow::bail!(
            "spawn_task: provider '{name}' is an on-device local model profile; spawn_task can \
             only target a cloud or network provider profile today (see /providers for the full \
             list)"
        );
    }
    let profile_name = entry.profile_name();
    create_provider_profile_from_config(config, &profile_name)
        .with_context(|| format!("spawn_task: failed to construct configured provider '{name}'"))
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "spawn_task"
    }

    fn effect(&self) -> ExecutionEffect {
        ExecutionEffect::ExternalWrite
    }

    fn description(&self) -> &str {
        &self.description
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
                },
                "provider": {
                    "type": "string",
                    "description": "Name of a configured provider profile to run this subagent \
                        against (see this tool's description for the current list, or run \
                        /providers). Omit to use the provider already driving this conversation."
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

        let provider = match input["provider"].as_str() {
            Some(name) => resolve_named_provider(&self.config, name)?,
            None => Arc::clone(&self.default_provider),
        };

        info!(
            "Spawning {:?} subagent (depth {}) for task: {}",
            subagent_type,
            self.depth,
            &task[..task.len().min(80)]
        );

        let result = run_subagent(
            provider,
            Arc::clone(&self.config),
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
    config: Arc<Config>,
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

    // Build tools for this subagent type. A nested spawn_task defaults to
    // the provider this subagent is itself running on, not the top-level
    // caller's provider, so an explicit `provider` choice propagates down
    // rather than being silently reset one level in.
    let tools = build_subagent_tools(
        subagent_type.allowed_tools(),
        Arc::clone(&provider),
        Arc::clone(&config),
        depth,
    );
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
        save_models: None,
        host_mode_state: None,
        plan_content: None,
        live_output: None,
        effect_audit: None,
        skip_interactive_review: false,
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
    config: Arc<Config>,
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
                let description = build_description(&config);
                tools.push(Box::new(TaskTool {
                    default_provider: Arc::clone(&provider),
                    config: Arc::clone(&config),
                    description,
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
            use crate::providers::ContentBlock;
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
            use crate::providers::{
                CapabilitySupport, ModelCapabilities, ModelFeature, WireProtocol,
            };

            let mut capabilities = ModelCapabilities::unknown(self.name(), model);
            if model == self.default_model() {
                capabilities.tools = ModelFeature::static_metadata(
                    CapabilitySupport::Supported,
                    "2026-08-27",
                    "spawn test fixture",
                );
                capabilities = capabilities.with_wire_protocol(
                    WireProtocol::AnthropicMessages,
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

    /// Minimal config with no provider entries, for tests that only need
    /// `TaskTool`'s construction-time plumbing (tool wiring, recursion
    /// depth, permissions) and never resolve a named `provider`.
    fn empty_config() -> Arc<Config> {
        Arc::new(Config::new(vec![]))
    }

    /// An Ollama-shaped entry pointed at `base_url`, named `name`: stands in
    /// for a network-hosted model profile (e.g. a "qwen" model served
    /// locally or over the network) the way `/providers` would list one.
    /// Unlike the cloud `Openai`/`Claude`/etc. variants, Ollama's tool-use
    /// capability is attested live against the configured endpoint rather
    /// than gated on matching a canonical cloud URL, so a fixture pointed
    /// at a mock server can actually be driven through a real turn.
    fn named_ollama_entry(name: &str, base_url: &str) -> ProviderEntry {
        ProviderEntry::Ollama {
            model: "qwen2.5:7b".to_string(),
            base_url: base_url.to_string(),
            name: Some(name.to_string()),
        }
    }

    /// An on-device local model profile entry, named `name`: constructible
    /// in config but not reachable through the `LlmProvider` path
    /// `spawn_task` uses.
    fn named_local_entry(name: &str) -> ProviderEntry {
        use crate::config::ExecutionTarget;
        use crate::models::{InferenceProvider, ModelFamily, ModelSize};
        ProviderEntry::Local {
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            name: Some(name.to_string()),
        }
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
        let tools = build_subagent_tools(
            SubagentType::Explore.allowed_tools(),
            null_provider(),
            empty_config(),
            0,
        );
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
        let tools = build_subagent_tools(
            SubagentType::Bash.allowed_tools(),
            null_provider(),
            empty_config(),
            0,
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            vec!["bash"],
            "Bash subagent should only have bash tool"
        );
    }

    #[test]
    fn test_subagent_tools_general_has_all() {
        let tools = build_subagent_tools(
            SubagentType::General.allowed_tools(),
            null_provider(),
            empty_config(),
            0,
        );
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
        let config = empty_config();

        // Below MAX_RECURSION_DEPTH → spawn_task present
        let tools = build_subagent_tools(
            SubagentType::General.allowed_tools(),
            Arc::clone(&provider),
            Arc::clone(&config),
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
            Arc::clone(&config),
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
                let tools = build_subagent_tools(
                    stype.allowed_tools(),
                    Arc::clone(&provider),
                    Arc::clone(&config),
                    depth,
                );
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
                let config = empty_config();
                tokio::spawn(async move {
                    run_subagent(
                        p,
                        config,
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
            empty_config(),
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

    // ---------------------------------------------------------------------------
    // Provider selection (optional `provider` parameter)
    // ---------------------------------------------------------------------------

    #[test]
    fn test_description_lists_configured_provider_names() {
        let config = Arc::new(Config::new(vec![
            named_ollama_entry("qwen", "http://127.0.0.1:1"),
            named_local_entry("local-gemma-2-9b"),
        ]));
        let tool = TaskTool::new(null_provider(), config);
        assert!(
            tool.description().contains("qwen"),
            "description must inline the configured cloud/network provider names so the model \
             sees valid choices without a lookup round-trip; got: {}",
            tool.description()
        );
        assert!(
            tool.description().contains("local-gemma-2-9b"),
            "description must still name a configured local profile even though it is not \
             selectable through this tool; got: {}",
            tool.description()
        );
    }

    #[tokio::test]
    async fn test_spawn_task_falls_back_to_default_provider_when_omitted() {
        let default_provider = Arc::new(EchoProvider::new("default answer"));
        let tool = TaskTool::new(default_provider.clone(), empty_config());
        let context = ToolContext {
            save_models: None,
            host_mode_state: None,
            plan_content: None,
            live_output: None,
            effect_audit: None,
            skip_interactive_review: false,
        };

        let output = tool
            .execute(json!({"task": "say hi"}), &context)
            .await
            .expect("omitting 'provider' must fall back to the default provider");
        assert_eq!(output, "default answer");
        assert_eq!(
            default_provider.backend_calls.load(Ordering::SeqCst),
            1,
            "the default provider must be the one actually invoked"
        );
    }

    #[tokio::test]
    async fn test_spawn_task_uses_named_provider_profile_when_given() {
        let mut server = mockito::Server::new_async().await;
        // Ollama's tool-use capability is attested live against this
        // endpoint (issue #925); without it the turn would fail closed
        // before ever reaching the chat-completions mock below.
        server
            .mock("POST", "/api/show")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"capabilities":["completion","tools"]}"#)
            .create_async()
            .await;
        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(
                r#"{"id":"chat-1","object":"chat.completion","created":1,"model":"qwen2.5:7b",
                "choices":[{"index":0,"message":{"role":"assistant","content":"answer from qwen"},
                "finish_reason":"stop"}]}"#,
            )
            .create_async()
            .await;
        let config = Arc::new(Config::new(vec![named_ollama_entry("qwen", &server.url())]));
        // The default provider must never be reached: naming a valid profile
        // must route the whole turn to it, not merely prefer it.
        let tool = TaskTool::new(null_provider(), config);
        let context = ToolContext {
            save_models: None,
            host_mode_state: None,
            plan_content: None,
            live_output: None,
            effect_audit: None,
            skip_interactive_review: false,
        };

        let output = tool
            .execute(json!({"task": "say hi", "provider": "qwen"}), &context)
            .await
            .expect("a configured provider name must resolve and complete the turn");
        assert_eq!(output, "answer from qwen");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_spawn_task_unconfigured_provider_name_fails_actionably() {
        let default_provider = Arc::new(EchoProvider::new("should not be used"));
        let config = Arc::new(Config::new(vec![named_ollama_entry(
            "claude-subscription",
            "http://127.0.0.1:1",
        )]));
        let tool = TaskTool::new(default_provider.clone(), config);
        let context = ToolContext {
            save_models: None,
            host_mode_state: None,
            plan_content: None,
            live_output: None,
            effect_audit: None,
            skip_interactive_review: false,
        };

        let error = tool
            .execute(
                json!({"task": "say hi", "provider": "does-not-exist"}),
                &context,
            )
            .await
            .expect_err("an unconfigured provider name must fail, not silently fall back");
        let message = error.to_string();
        assert!(
            message.contains("does-not-exist"),
            "error must name the requested provider; got: {message}"
        );
        assert!(
            message.to_lowercase().contains("not configured"),
            "error must say the profile is not configured, not just that it failed; got: {message}"
        );
        assert_eq!(
            default_provider.backend_calls.load(Ordering::SeqCst),
            0,
            "an invalid provider name must not silently fall back to the default provider"
        );
    }

    #[tokio::test]
    async fn test_spawn_task_local_provider_profile_rejected_actionably() {
        let default_provider = Arc::new(EchoProvider::new("should not be used"));
        let config = Arc::new(Config::new(vec![named_local_entry("local-gemma-2-9b")]));
        let tool = TaskTool::new(default_provider.clone(), config);
        let context = ToolContext {
            save_models: None,
            host_mode_state: None,
            plan_content: None,
            live_output: None,
            effect_audit: None,
            skip_interactive_review: false,
        };

        let error = tool
            .execute(
                json!({"task": "say hi", "provider": "local-gemma-2-9b"}),
                &context,
            )
            .await
            .expect_err("a local on-device profile must be rejected, not silently substituted");
        let message = error.to_string();
        assert!(
            message.contains("local-gemma-2-9b"),
            "error must name the requested profile; got: {message}"
        );
        assert!(
            message.contains("local model"),
            "error must explain why a local profile cannot be targeted; got: {message}"
        );
        assert_eq!(
            default_provider.backend_calls.load(Ordering::SeqCst),
            0,
            "a rejected local profile must not silently fall back to the default provider"
        );
    }

    #[test]
    fn test_peer_hard_deny_still_covers_task_tool_with_config_constructor() {
        // Guards the #872-adjacent invariant this change touches directly:
        // widening TaskTool::new to accept a config/provider-lookup capability
        // must not change the name it registers under, which is what
        // PEER_HARD_DENY_TOOLS and test_peer_cannot_spawn key on.
        let tool = TaskTool::new(null_provider(), empty_config());
        assert_eq!(Tool::name(&tool), "spawn_task");
    }
}

// Tool execution engine
//
// Executes tools with permission checks and multi-turn support

use crate::cli::ConversationHistory;
use crate::tools::patterns::{ExactApproval, MatchType, PersistentPatternStore, ToolPattern};
use crate::tools::permissions::{PermissionCheck, PermissionManager};
use crate::tools::registry::ToolRegistry;
use crate::tools::types::{ToolResult, ToolUse};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

/// Signature for a tool execution, used for caching approval decisions
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolSignature {
    pub tool_name: String,
    pub context_key: String,

    // Structured components for flexible pattern matching
    /// Command being executed (for bash, save_and_exec)
    pub command: Option<String>,
    /// Arguments passed to the command
    pub args: Option<String>,
    /// Working directory for the execution
    pub directory: Option<String>,
}

/// Source of approval for a tool execution
#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalSource {
    NotApproved,
    SessionExact,
    SessionPattern(String), // Pattern ID
    PersistentExact,
    PersistentPattern(String), // Pattern ID
}

/// Enhanced cache for tool execution approvals with pattern matching and persistence
pub struct ToolConfirmationCache {
    // Session-only approvals (cleared on restart)
    session_exact: HashSet<ToolSignature>,
    session_patterns: Vec<ToolPattern>,

    // Persistent approvals (saved to disk)
    persistent: PersistentPatternStore,
    persistent_path: PathBuf,
    dirty: bool, // Track if save needed
}

impl ToolConfirmationCache {
    /// Create new cache with persistent storage path
    pub fn new(persistent_path: PathBuf) -> Result<Self> {
        let persistent = if persistent_path.exists() {
            match PersistentPatternStore::load(&persistent_path) {
                Ok(store) => {
                    info!(
                        "Loaded {} patterns and {} exact approvals from disk",
                        store.patterns.len(),
                        store.exact_approvals.len()
                    );
                    store
                }
                Err(e) => {
                    warn!("Failed to load patterns, starting fresh: {}", e);
                    PersistentPatternStore::default()
                }
            }
        } else {
            debug!("No existing patterns file, starting fresh");
            PersistentPatternStore::default()
        };

        Ok(Self {
            session_exact: HashSet::new(),
            session_patterns: Vec::new(),
            persistent,
            persistent_path,
            dirty: false,
        })
    }

    /// Check if a signature is approved, returning the approval source
    pub fn is_approved(&mut self, sig: &ToolSignature) -> ApprovalSource {
        // 1. Check persistent exact (highest priority)
        if self.persistent.has_exact(sig) {
            // Increment match count (this makes it dirty)
            if let Some(MatchType::Exact(_)) = self.persistent.matches(sig) {
                self.dirty = true;
                return ApprovalSource::PersistentExact;
            }
        }

        // 2. Check session exact
        if self.session_exact.contains(sig) {
            return ApprovalSource::SessionExact;
        }

        // 3. Check persistent patterns
        if let Some(MatchType::Pattern(id)) = self.persistent.matches(sig) {
            self.dirty = true; // Match count was incremented
            return ApprovalSource::PersistentPattern(id);
        }

        // 4. Check session patterns
        for pattern in &mut self.session_patterns {
            if pattern.matches(sig) {
                pattern.record_match();
                return ApprovalSource::SessionPattern(pattern.id.clone());
            }
        }

        ApprovalSource::NotApproved
    }

    /// Approve exact command for session only
    pub fn approve_exact_session(&mut self, sig: ToolSignature) {
        self.session_exact.insert(sig);
    }

    /// Approve pattern for session only
    pub fn approve_pattern_session(&mut self, pattern: ToolPattern) {
        self.session_patterns.push(pattern);
    }

    /// Approve exact command persistently
    pub fn approve_exact_persistent(&mut self, sig: ToolSignature) {
        self.persistent.add_exact(ExactApproval::new(sig));
        self.dirty = true;
    }

    /// Approve pattern persistently
    pub fn approve_pattern_persistent(&mut self, pattern: ToolPattern) {
        self.persistent.add_pattern(pattern);
        self.dirty = true;
    }

    /// Save persistent patterns if modified
    pub fn save_if_dirty(&mut self) -> Result<()> {
        if self.dirty {
            info!(
                "Saving {} patterns and {} exact approvals to disk",
                self.persistent.patterns.len(),
                self.persistent.exact_approvals.len()
            );
            self.persistent.save(&self.persistent_path)?;
            self.dirty = false;
        }
        Ok(())
    }

    /// Clear session approvals (keep persistent)
    pub fn clear(&mut self) {
        self.session_exact.clear();
        self.session_patterns.clear();
    }

    /// Get reference to persistent store (for management commands)
    pub fn persistent_store(&self) -> &PersistentPatternStore {
        &self.persistent
    }

    /// Get mutable reference to persistent store (for management commands)
    pub fn persistent_store_mut(&mut self) -> &mut PersistentPatternStore {
        self.dirty = true; // Assume any mutation makes it dirty
        &mut self.persistent
    }

    /// Remove a pattern or approval by ID
    pub fn remove_by_id(&mut self, id: &str) -> bool {
        let removed = self.persistent.remove(id);
        if removed {
            self.dirty = true;
        }
        removed
    }

    /// Clear all persistent patterns and approvals
    pub fn clear_persistent(&mut self) {
        self.persistent = PersistentPatternStore::default();
        self.dirty = true;
    }
}

/// Tool executor - manages tool execution lifecycle
pub struct ToolExecutor {
    registry: ToolRegistry,
    permissions: PermissionManager,
    confirmation_cache: ToolConfirmationCache,
    mcp_client: Option<Arc<crate::tools::mcp::McpClient>>,
}

impl ToolExecutor {
    /// Create new tool executor with persistent patterns path
    pub fn new(
        registry: ToolRegistry,
        permissions: PermissionManager,
        patterns_path: PathBuf,
    ) -> Result<Self> {
        Ok(Self {
            registry,
            permissions,
            confirmation_cache: ToolConfirmationCache::new(patterns_path)?,
            mcp_client: None,
        })
    }

    /// Add MCP client to enable MCP tools
    ///
    /// Always returns Self (never fails) - gracefully handles MCP connection errors
    pub async fn with_mcp(mut self, config: &crate::config::Config) -> Self {
        if !config.mcp_servers.is_empty() {
            info!(
                "Initializing MCP client with {} servers",
                config.mcp_servers.len()
            );
            match crate::tools::mcp::McpClient::from_config(&config.mcp_servers).await {
                Ok(mcp_client) => {
                    let connected_servers = mcp_client.list_servers().await;
                    info!(
                        "MCP client initialized with {} connected servers",
                        connected_servers.len()
                    );
                    self.mcp_client = Some(Arc::new(mcp_client));
                }
                Err(e) => {
                    warn!("Failed to initialize MCP client: {}", e);
                    warn!("Continuing without MCP tools");
                    // mcp_client remains None
                }
            }
        }
        self
    }

    /// Get reference to MCP client (for management commands)
    pub fn mcp_client(&self) -> Option<&Arc<crate::tools::mcp::McpClient>> {
        self.mcp_client.as_ref()
    }

    /// Get list of all available tools (built-in + MCP)
    pub async fn list_all_tools(&self) -> Vec<crate::tools::types::ToolDefinition> {
        let mut tools = Vec::new();

        // Add built-in tools from registry
        for tool_name in self.registry.tool_names() {
            if let Some(tool) = self.registry.get(&tool_name) {
                tools.push(crate::tools::types::ToolDefinition {
                    name: tool_name.to_string(),
                    description: tool.description().to_string(),
                    input_schema: tool.input_schema(),
                });
            }
        }

        // Add MCP tools if client is available
        if let Some(mcp) = &self.mcp_client {
            let mcp_tools = mcp.list_tools().await;
            tools.extend(mcp_tools);
        }

        tools
    }

    /// Check if a tool signature is pre-approved (returns approval source)
    pub fn is_approved(&mut self, sig: &ToolSignature) -> ApprovalSource {
        self.confirmation_cache.is_approved(sig)
    }

    /// Approve exact command for session only
    pub fn approve_exact_session(&mut self, sig: ToolSignature) {
        self.confirmation_cache.approve_exact_session(sig);
    }

    /// Approve pattern for session only
    pub fn approve_pattern_session(&mut self, pattern: ToolPattern) {
        self.confirmation_cache.approve_pattern_session(pattern);
    }

    /// Approve exact command persistently
    pub fn approve_exact_persistent(&mut self, sig: ToolSignature) {
        self.confirmation_cache.approve_exact_persistent(sig);
    }

    /// Approve pattern persistently
    pub fn approve_pattern_persistent(&mut self, pattern: ToolPattern) {
        self.confirmation_cache.approve_pattern_persistent(pattern);
    }

    /// Save patterns to disk if modified
    pub fn save_patterns(&mut self) -> Result<()> {
        self.confirmation_cache.save_if_dirty()
    }

    /// Clear session approvals (keep persistent)
    pub fn clear_session_approvals(&mut self) {
        self.confirmation_cache.clear();
    }

    /// Get reference to persistent store (for management commands)
    pub fn persistent_store(&self) -> &PersistentPatternStore {
        self.confirmation_cache.persistent_store()
    }

    /// Remove a pattern or approval by ID
    pub fn remove_pattern(&mut self, id: &str) -> bool {
        self.confirmation_cache.remove_by_id(id)
    }

    /// Clear all persistent patterns and approvals
    pub fn clear_persistent_patterns(&mut self) {
        self.confirmation_cache.clear_persistent();
    }

    /// Execute a single tool use
    #[instrument(skip(self, tool_use, conversation, save_models_fn, batch_trainer, local_generator, tokenizer, repl_mode, plan_content, live_output), fields(tool = %tool_use.name, id = %tool_use.id))]
    pub async fn execute_tool<F>(
        &self,
        tool_use: &ToolUse,
        conversation: Option<&ConversationHistory>,
        save_models_fn: Option<F>,
        batch_trainer: Option<
            Arc<tokio::sync::RwLock<crate::training::batch_trainer::BatchTrainer>>,
        >,
        local_generator: Option<Arc<tokio::sync::RwLock<crate::local::LocalGenerator>>>,
        tokenizer: Option<Arc<crate::models::tokenizer::TextTokenizer>>,
        repl_mode: Option<Arc<tokio::sync::RwLock<crate::cli::ReplMode>>>,
        plan_content: Option<Arc<tokio::sync::RwLock<Option<String>>>>,
        live_output: Option<Arc<dyn Fn(String) + Send + Sync>>,
    ) -> Result<ToolResult>
    where
        F: Fn() -> Result<()> + Send + Sync,
    {
        info!("Executing tool: {}", tool_use.name);

        // 1. Check if it's an MCP tool
        if tool_use.name.starts_with("mcp_") {
            if let Some(mcp) = &self.mcp_client {
                info!("Routing to MCP client: {}", tool_use.name);
                match mcp
                    .execute_tool(&tool_use.name, tool_use.input.clone())
                    .await
                {
                    Ok(output) => {
                        info!("MCP tool executed successfully");
                        return Ok(ToolResult::success(tool_use.id.clone(), output));
                    }
                    Err(e) => {
                        error!("MCP tool execution failed: {}", e);
                        return Ok(ToolResult::error(
                            tool_use.id.clone(),
                            format!("MCP execution error: {}", e),
                        ));
                    }
                }
            } else {
                error!("MCP tool requested but no MCP client available");
                return Ok(ToolResult::error(
                    tool_use.id.clone(),
                    "MCP tools not available (no MCP servers configured)".to_string(),
                ));
            }
        }

        // 2. Check if built-in tool exists
        let tool = self
            .registry
            .get(&tool_use.name)
            .context(format!("Tool '{}' not found", tool_use.name))?;

        // 3. Check permissions
        let permission_check = self
            .permissions
            .check_tool_use(&tool_use.name, &tool_use.input);

        match permission_check {
            PermissionCheck::Allow => {
                debug!("Tool execution allowed");
            }
            PermissionCheck::AskUser(_reason) => {
                // Confirmation was already handled by ToolExecutionCoordinator before
                // execute_tool() is called. AskUser here means "needs confirmation",
                // which has already been obtained — proceed with execution.
                debug!("Tool requires confirmation (handled by coordinator)");
            }
            PermissionCheck::Deny(reason) => {
                error!("Tool execution denied: {}", reason);
                return Ok(ToolResult::error(tool_use.id.clone(), reason));
            }
        }

        // 3. Check plan mode restrictions
        if let Some(ref mode) = repl_mode {
            let current_mode = mode.read().await;
            if let crate::cli::ReplMode::Planning { .. } = &*current_mode {
                // In planning mode, only allow read-only tools
                let allowed_tools = [
                    "read",
                    "glob",
                    "grep",
                    "web_fetch",
                    "enter_plan_mode",
                    "EnterPlanMode",
                    "present_plan",
                    "PresentPlan",
                    "ask_user_question",
                    "AskUserQuestion",
                ];
                if !allowed_tools.contains(&tool_use.name.as_str()) {
                    drop(current_mode);
                    warn!("Tool '{}' blocked in planning mode", tool_use.name);
                    return Ok(ToolResult::error(
                        tool_use.id.clone(),
                        format!(
                            "Tool '{}' is not allowed in planning mode.\n\
                             Available tools: read, glob, grep, web_fetch, present_plan, ask_user_question\n\
                             Use PresentPlan to show your plan for approval.",
                            tool_use.name
                        ),
                    ));
                }
            }
            drop(current_mode);
        }

        // 4. Execute tool with context
        let context = crate::tools::types::ToolContext {
            conversation,
            save_models: save_models_fn
                .as_ref()
                .map(|f| f as &(dyn Fn() -> Result<()> + Send + Sync)),
            batch_trainer,
            local_generator,
            tokenizer,
            repl_mode,
            plan_content,
            live_output,
        };

        match tool.execute(tool_use.input.clone(), &context).await {
            Ok(output) => {
                info!("Tool executed successfully");
                Ok(ToolResult::success(tool_use.id.clone(), output))
            }
            Err(e) => {
                error!("Tool execution failed: {}", e);
                Ok(ToolResult::error(
                    tool_use.id.clone(),
                    format!("Execution error: {}", e),
                ))
            }
        }
    }

    /// Execute multiple tool uses in sequence
    #[instrument(skip(
        self,
        tool_uses,
        conversation,
        save_models_fn,
        batch_trainer,
        local_generator,
        tokenizer
    ))]
    pub async fn execute_tool_loop<F>(
        &self,
        tool_uses: Vec<ToolUse>,
        conversation: Option<&ConversationHistory>,
        save_models_fn: Option<F>,
        batch_trainer: Option<
            Arc<tokio::sync::RwLock<crate::training::batch_trainer::BatchTrainer>>,
        >,
        local_generator: Option<Arc<tokio::sync::RwLock<crate::local::LocalGenerator>>>,
        tokenizer: Option<Arc<crate::models::tokenizer::TextTokenizer>>,
        repl_mode: Option<Arc<tokio::sync::RwLock<crate::cli::ReplMode>>>,
        plan_content: Option<Arc<tokio::sync::RwLock<Option<String>>>>,
    ) -> Result<Vec<ToolResult>>
    where
        F: Fn() -> Result<()> + Send + Sync + Clone,
    {
        info!("Executing {} tool(s)", tool_uses.len());

        let mut results = Vec::new();

        for tool_use in tool_uses {
            let result = self
                .execute_tool(
                    &tool_use,
                    conversation,
                    save_models_fn.clone(),
                    batch_trainer.clone(),
                    local_generator.clone(),
                    tokenizer.clone(),
                    repl_mode.clone(),
                    plan_content.clone(),
                    None,
                )
                .await?;
            results.push(result);
        }

        Ok(results)
    }

    /// Get reference to registry
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Get reference to permissions manager
    pub fn permissions(&self) -> &PermissionManager {
        &self.permissions
    }
}

/// Generate a context-specific signature for a tool use
pub fn generate_tool_signature(tool_use: &ToolUse, working_dir: &std::path::Path) -> ToolSignature {
    match tool_use.name.as_str() {
        "bash" => {
            let command = tool_use.input["command"].as_str().unwrap_or("");

            // Parse command into base command and args
            let (base_cmd, args) = if let Some(space_idx) = command.find(' ') {
                let (cmd, rest) = command.split_at(space_idx);
                (cmd.to_string(), Some(rest.trim().to_string()))
            } else {
                (command.to_string(), None)
            };

            ToolSignature {
                tool_name: "bash".to_string(),
                context_key: format!("{} in {}", command, working_dir.display()),
                command: Some(base_cmd),
                args,
                directory: Some(working_dir.display().to_string()),
            }
        }
        "read" => {
            let file_path = tool_use.input["file_path"].as_str().unwrap_or("");
            ToolSignature {
                tool_name: "read".to_string(),
                context_key: format!("reading {}", file_path),
                command: None,
                args: None,
                directory: Some(working_dir.display().to_string()),
            }
        }
        "glob" => {
            let pattern = tool_use.input["pattern"].as_str().unwrap_or("");
            ToolSignature {
                tool_name: "glob".to_string(),
                context_key: format!("pattern {}", pattern),
                command: None,
                args: None,
                directory: Some(working_dir.display().to_string()),
            }
        }
        "grep" => {
            let pattern = tool_use.input["pattern"].as_str().unwrap_or("");
            let path = tool_use
                .input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            ToolSignature {
                tool_name: "grep".to_string(),
                context_key: format!("pattern '{}' in {}", pattern, path),
                command: None,
                args: None,
                directory: Some(working_dir.display().to_string()),
            }
        }
        "web_fetch" => {
            let url = tool_use.input["url"].as_str().unwrap_or("");
            ToolSignature {
                tool_name: "web_fetch".to_string(),
                context_key: format!("fetching {}", url),
                command: None,
                args: None,
                directory: None,
            }
        }
        "save_and_exec" => {
            let command = tool_use.input["command"].as_str().unwrap_or("");

            // Parse command into base command and args (same as bash)
            let (base_cmd, args) = if let Some(space_idx) = command.find(' ') {
                let (cmd, rest) = command.split_at(space_idx);
                (cmd.to_string(), Some(rest.trim().to_string()))
            } else {
                (command.to_string(), None)
            };

            ToolSignature {
                tool_name: "save_and_exec".to_string(),
                context_key: format!("{} in {}", command, working_dir.display()),
                command: Some(base_cmd),
                args,
                directory: Some(working_dir.display().to_string()),
            }
        }
        "train" => {
            // Extract wait parameter if present
            let wait = tool_use.input["wait"].as_bool().unwrap_or(false);
            ToolSignature {
                tool_name: tool_use.name.clone(),
                context_key: format!("train wait={}", wait),
                command: None,
                args: None,
                directory: None,
            }
        }
        "query_local_model" => {
            // Extract query if present
            let query = tool_use.input["query"].as_str().unwrap_or("");
            let truncated_query = if query.len() > 50 {
                format!("{}...", &query[..50])
            } else {
                query.to_string()
            };
            ToolSignature {
                tool_name: tool_use.name.clone(),
                context_key: format!("query_local_model: {}", truncated_query),
                command: None,
                args: None,
                directory: None,
            }
        }
        "analyze_model" => {
            // Extract categories if present
            let categories = if let Some(cats) = tool_use.input["categories"].as_array() {
                cats.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                "all".to_string()
            };
            ToolSignature {
                tool_name: tool_use.name.clone(),
                context_key: format!("analyze_model categories={}", categories),
                command: None,
                args: None,
                directory: None,
            }
        }
        "generate_training_data" => {
            // Extract count of examples
            let examples_count = if let Some(examples) = tool_use.input["examples"].as_array() {
                examples.len()
            } else {
                0
            };
            ToolSignature {
                tool_name: tool_use.name.clone(),
                context_key: format!("generate_training_data count={}", examples_count),
                command: None,
                args: None,
                directory: None,
            }
        }
        "compare_responses" => ToolSignature {
            tool_name: tool_use.name.clone(),
            context_key: "compare_responses".to_string(),
            command: None,
            args: None,
            directory: None,
        },
        _ => {
            // Generic signature for unknown tools
            ToolSignature {
                tool_name: tool_use.name.clone(),
                context_key: format!("in {}", working_dir.display()),
                command: None,
                args: None,
                directory: Some(working_dir.display().to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::Tool;
    use crate::tools::types::{ToolContext, ToolInputSchema};
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use std::path::Path;

    // Mock tool for testing
    struct MockTool {
        should_fail: bool,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            "mock"
        }

        fn description(&self) -> &str {
            "A mock tool"
        }

        fn input_schema(&self) -> ToolInputSchema {
            ToolInputSchema::simple(vec![("param", "Test parameter")])
        }

        async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
            if self.should_fail {
                anyhow::bail!("Mock failure");
            }
            Ok(format!("Mock result: {}", input))
        }
    }

    fn create_test_executor(allow_tool: bool, tool_should_fail: bool) -> ToolExecutor {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(MockTool {
            should_fail: tool_should_fail,
        }));

        let permissions = if allow_tool {
            PermissionManager::new()
                .with_default_rule(crate::tools::permissions::PermissionRule::Allow)
        } else {
            PermissionManager::new()
                .with_default_rule(crate::tools::permissions::PermissionRule::Deny)
        };

        // Use temp path for tests
        let temp_path = std::env::temp_dir().join("finch_test_patterns.json");
        ToolExecutor::new(registry, permissions, temp_path).expect("Failed to create test executor")
    }

    #[tokio::test]
    async fn test_execute_tool_success() {
        let executor = create_test_executor(true, false);
        let tool_use = ToolUse::new("mock".to_string(), serde_json::json!({"param": "value"}));

        let result = executor
            .execute_tool(
                &tool_use,
                None,
                None::<fn() -> Result<()>>,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.tool_use_id, tool_use.id);
        assert!(!result.is_error);
        assert!(result.content.contains("Mock result"));
    }

    #[tokio::test]
    async fn test_execute_tool_not_found() {
        let executor = create_test_executor(true, false);
        let tool_use = ToolUse::new(
            "nonexistent".to_string(),
            serde_json::json!({"param": "value"}),
        );

        let result = executor
            .execute_tool(
                &tool_use,
                None,
                None::<fn() -> Result<()>>,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_execute_tool_permission_denied() {
        let executor = create_test_executor(false, false);
        let tool_use = ToolUse::new("mock".to_string(), serde_json::json!({"param": "value"}));

        let result = executor
            .execute_tool(
                &tool_use,
                None,
                None::<fn() -> Result<()>>,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.tool_use_id, tool_use.id);
        assert!(result.is_error);
        assert!(result.content.contains("not allowed"));
    }

    #[tokio::test]
    async fn test_execute_tool_execution_failure() {
        let executor = create_test_executor(true, true);
        let tool_use = ToolUse::new("mock".to_string(), serde_json::json!({"param": "value"}));

        let result = executor
            .execute_tool(
                &tool_use,
                None,
                None::<fn() -> Result<()>>,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(result.tool_use_id, tool_use.id);
        assert!(result.is_error);
        assert!(result.content.contains("Execution error"));
    }

    #[tokio::test]
    async fn test_execute_tool_loop() {
        let executor = create_test_executor(true, false);
        let tool_uses = vec![
            ToolUse::new("mock".to_string(), serde_json::json!({"param": "1"})),
            ToolUse::new("mock".to_string(), serde_json::json!({"param": "2"})),
        ];

        let results = executor
            .execute_tool_loop(
                tool_uses,
                None,
                None::<fn() -> Result<()>>,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(!results[0].is_error);
        assert!(!results[1].is_error);
    }

    #[test]
    fn test_confirmation_cache() {
        let temp_path = std::env::temp_dir().join("test_cache_patterns.json");
        let mut cache = ToolConfirmationCache::new(temp_path).expect("Failed to create cache");

        let sig1 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo test".to_string(),
            command: Some("cargo".to_string()),
            args: Some("test".to_string()),
            directory: None,
        };

        let sig2 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo build".to_string(),
            command: Some("cargo".to_string()),
            args: Some("build".to_string()),
            directory: None,
        };

        // Initially, nothing is approved
        assert_eq!(cache.is_approved(&sig1), ApprovalSource::NotApproved);
        assert_eq!(cache.is_approved(&sig2), ApprovalSource::NotApproved);

        // Approve sig1 for session
        cache.approve_exact_session(sig1.clone());
        assert_eq!(cache.is_approved(&sig1), ApprovalSource::SessionExact);
        assert_eq!(cache.is_approved(&sig2), ApprovalSource::NotApproved);

        // Approve sig2 for session
        cache.approve_exact_session(sig2.clone());
        assert_eq!(cache.is_approved(&sig1), ApprovalSource::SessionExact);
        assert_eq!(cache.is_approved(&sig2), ApprovalSource::SessionExact);

        // Clear session cache
        cache.clear();
        assert_eq!(cache.is_approved(&sig1), ApprovalSource::NotApproved);
        assert_eq!(cache.is_approved(&sig2), ApprovalSource::NotApproved);
    }

    #[test]
    fn test_tool_executor_approval_cache() {
        let mut executor = create_test_executor(true, false);

        let sig = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo fmt".to_string(),
            command: Some("cargo".to_string()),
            args: Some("fmt".to_string()),
            directory: None,
        };

        // Initially not approved
        assert_eq!(executor.is_approved(&sig), ApprovalSource::NotApproved);

        // Add session approval
        executor.approve_exact_session(sig.clone());
        assert_eq!(executor.is_approved(&sig), ApprovalSource::SessionExact);

        // Clear session approvals
        executor.clear_session_approvals();
        assert_eq!(executor.is_approved(&sig), ApprovalSource::NotApproved);
    }

    #[test]
    fn test_generate_tool_signature_bash() {
        let working_dir = Path::new("/test/dir");
        let tool_use = ToolUse::new(
            "bash".to_string(),
            json!({
                "command": "cargo test",
                "description": "Run tests"
            }),
        );

        let sig = generate_tool_signature(&tool_use, working_dir);

        assert_eq!(sig.tool_name, "bash");
        assert_eq!(sig.context_key, "cargo test in /test/dir");
    }

    #[test]
    fn test_generate_tool_signature_read() {
        let working_dir = Path::new("/test/dir");
        let tool_use = ToolUse::new(
            "read".to_string(),
            json!({"file_path": "/path/to/file.txt"}),
        );

        let sig = generate_tool_signature(&tool_use, working_dir);

        assert_eq!(sig.tool_name, "read");
        assert_eq!(sig.context_key, "reading /path/to/file.txt");
    }

    #[test]
    fn test_generate_tool_signature_grep() {
        let working_dir = Path::new("/test/dir");
        let tool_use = ToolUse::new(
            "grep".to_string(),
            json!({
                "pattern": "fn main",
                "path": "src/"
            }),
        );

        let sig = generate_tool_signature(&tool_use, working_dir);

        assert_eq!(sig.tool_name, "grep");
        assert_eq!(sig.context_key, "pattern 'fn main' in src/");
    }

    #[test]
    fn test_tool_signature_uniqueness() {
        let working_dir = Path::new("/test/dir");

        let cmd1 = ToolUse::new("bash".to_string(), json!({"command": "cargo test"}));
        let cmd2 = ToolUse::new("bash".to_string(), json!({"command": "cargo build"}));
        let cmd3 = ToolUse::new("bash".to_string(), json!({"command": "cargo test"}));

        let sig1 = generate_tool_signature(&cmd1, working_dir);
        let sig2 = generate_tool_signature(&cmd2, working_dir);
        let sig3 = generate_tool_signature(&cmd3, working_dir);

        // Different commands should have different signatures
        assert_ne!(sig1, sig2);

        // Same command should produce same signature
        assert_eq!(sig1, sig3);
    }

    #[test]
    fn test_generate_tool_signature_train() {
        let working_dir = Path::new("/test/dir");
        let tool_use = ToolUse::new(
            "train".to_string(),
            json!({
                "wait": true
            }),
        );

        let sig = generate_tool_signature(&tool_use, working_dir);

        assert_eq!(sig.tool_name, "train");
        assert_eq!(sig.context_key, "train wait=true");
    }

    #[test]
    fn test_generate_tool_signature_query_local_model() {
        let working_dir = Path::new("/test/dir");
        let tool_use = ToolUse::new(
            "query_local_model".to_string(),
            json!({
                "query": "What is Rust?"
            }),
        );

        let sig = generate_tool_signature(&tool_use, working_dir);

        assert_eq!(sig.tool_name, "query_local_model");
        assert_eq!(sig.context_key, "query_local_model: What is Rust?");
    }

    #[test]
    fn test_generate_tool_signature_analyze_model() {
        let working_dir = Path::new("/test/dir");
        let tool_use = ToolUse::new(
            "analyze_model".to_string(),
            json!({
                "categories": ["greetings", "math"]
            }),
        );

        let sig = generate_tool_signature(&tool_use, working_dir);

        assert_eq!(sig.tool_name, "analyze_model");
        assert_eq!(sig.context_key, "analyze_model categories=greetings, math");
    }

    #[test]
    fn test_generate_tool_signature_generate_training_data() {
        let working_dir = Path::new("/test/dir");
        let tool_use = ToolUse::new(
            "generate_training_data".to_string(),
            json!({
                "examples": [
                    {"query": "Hello", "response": "Hi!"},
                    {"query": "Bye", "response": "Goodbye!"}
                ]
            }),
        );

        let sig = generate_tool_signature(&tool_use, working_dir);

        assert_eq!(sig.tool_name, "generate_training_data");
        assert_eq!(sig.context_key, "generate_training_data count=2");
    }

    #[test]
    fn test_generate_tool_signature_compare_responses() {
        let working_dir = Path::new("/test/dir");
        let tool_use = ToolUse::new("compare_responses".to_string(), json!({}));

        let sig = generate_tool_signature(&tool_use, working_dir);

        assert_eq!(sig.tool_name, "compare_responses");
        assert_eq!(sig.context_key, "compare_responses");
    }
}

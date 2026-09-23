// Tool execution engine
//
// Executes tools with permission checks and multi-turn support

use crate::cli::ReplModeState;
use crate::tools::permissions::{
    bash_command_is_constitutionally_denied, path_argument_for_tool, raw_path_escapes_workspace,
    resolve_workspace_root, PermissionCheck, PermissionManager,
};
use crate::tools::types::{EffectAuditAuthority, ToolResult, ToolUse};
use crate::tools::ToolRegistry;
use crate::tools::{ExactApproval, MatchType, PersistentPatternStore, ToolPattern, ToolSignature};
use anyhow::{Context, Result};
use finch_programs::ExecutionEffect;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

// ─── Co-Forth trace helpers ───────────────────────────────────────────────────

/// Build a compact human-readable label for a tool call.
/// e.g. "Read src/main.rs", "Bash cargo test", "Grep fn main"
fn tool_label(name: &str, input: &serde_json::Value) -> String {
    let key_arg = match name {
        "Read" | "read" => input["path"].as_str().unwrap_or("").to_string(),
        "Glob" | "glob" => input["pattern"].as_str().unwrap_or("").to_string(),
        "Grep" | "grep" => input["pattern"].as_str().unwrap_or("").to_string(),
        "Bash" | "bash" => {
            let cmd = input["command"].as_str().unwrap_or("");
            cmd.chars().take(32).collect::<String>()
        }
        "Write" | "write" => input["path"].as_str().unwrap_or("").to_string(),
        "Edit" | "edit" => input["path"].as_str().unwrap_or("").to_string(),
        "WebFetch" | "web_fetch" => {
            let url = input["url"].as_str().unwrap_or("");
            url.chars().take(32).collect::<String>()
        }
        _ => String::new(),
    };
    let display_name = name.to_uppercase();
    if key_arg.is_empty() {
        display_name
    } else {
        format!("{} {}", display_name, key_arg)
    }
}

/// Map a tool name to a poset NodeKind.
fn tool_kind(name: &str) -> crate::poset::NodeKind {
    match name {
        "Bash" | "bash" | "Write" | "write" | "Edit" | "edit" => crate::poset::NodeKind::Task,
        _ => crate::poset::NodeKind::Observation,
    }
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
        if sig.constitutionally_denied {
            return ApprovalSource::NotApproved;
        }

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

        // Escaped paths: exact-only. A pattern can never satisfy an escape.
        if sig.path.is_some() && !sig.path_in_workspace {
            return ApprovalSource::NotApproved;
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
    /// Declared post-edit diagnostics sources (issue #757). `None`/inert by
    /// default: without a declared source, edit results are unchanged.
    diagnostics: Option<Arc<super::diagnostics::DiagnosticsService>>,
    /// When set, every successful tool call auto-pushes a node into the poset.
    /// The execution trace becomes the Co-Forth vocabulary.
    pub poset: Option<Arc<tokio::sync::Mutex<crate::poset::Poset>>>,
}

impl ToolExecutor {
    /// Create new tool executor with persistent patterns path
    pub fn new(
        registry: ToolRegistry,
        permissions: PermissionManager,
        patterns_path: PathBuf,
    ) -> Result<Self> {
        for tool in registry.get_all_tools() {
            if let Some(tool_root) = tool.workspace_root() {
                anyhow::ensure!(
                    tool_root == permissions.workspace_root(),
                    "tool '{}' workspace root {} differs from permission root {}",
                    tool.name(),
                    tool_root.display(),
                    permissions.workspace_root().display()
                );
            }
        }
        Ok(Self {
            registry,
            permissions,
            confirmation_cache: ToolConfirmationCache::new(patterns_path)?,
            mcp_client: None,
            diagnostics: None,
            poset: None,
        })
    }

    /// Attach declared post-edit diagnostics sources (issue #757).
    ///
    /// After a successful write/edit/patch result, a check command the user
    /// declared in `[diagnostics]` for the touched file's extension runs
    /// bounded and its report is appended to that same tool result. The
    /// command's authority verdict comes from the existing bash approval path;
    /// with no declared source this executor behaves exactly as before.
    pub fn with_diagnostics(mut self, config: &crate::config::DiagnosticsConfig) -> Self {
        self.diagnostics = Some(Arc::new(
            super::diagnostics::DiagnosticsService::from_config(
                config,
                self.permissions.cwd().to_path_buf(),
            ),
        ));
        self
    }

    /// Save persistent tool patterns if modified during this session.
    pub fn save_if_dirty(&mut self) -> Result<()> {
        self.confirmation_cache.save_if_dirty()
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

    /// Maximum execution time for a tool call. `None` means the adapter may
    /// first wait for a human in `$EDITOR`; the adapter itself must separately
    /// bound the subprocess it eventually launches.
    ///
    /// MCP servers can configure longer operations than Finch's built-in-tool
    /// default. The editor-backed compatibility tools are the one exception:
    /// timing a human review as though it were a subprocess made ordinary
    /// proposal review fail after thirty seconds.
    pub fn execution_timeout(&self, tool_name: &str) -> Option<std::time::Duration> {
        if matches!(tool_name, "bash" | "edit" | "write") {
            return None;
        }
        self.mcp_client
            .as_ref()
            .and_then(|client| client.timeout_for_tool(tool_name))
            .or_else(|| Some(std::time::Duration::from_secs(30)))
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
    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(self, tool_use, save_models_fn, repl_mode, plan_content, live_output, effect_audit), fields(tool = %tool_use.name, id = %tool_use.id))]
    pub async fn execute_tool<F>(
        &self,
        tool_use: &ToolUse,
        save_models_fn: Option<F>,
        repl_mode: Option<Arc<tokio::sync::RwLock<crate::cli::ReplMode>>>,
        plan_content: Option<Arc<tokio::sync::RwLock<Option<String>>>>,
        live_output: Option<crate::tools::types::LiveOutput>,
        effect_audit: Option<crate::server::RunnerEffectAuditControl>,
    ) -> Result<ToolResult>
    where
        F: Fn() -> Result<()> + Send + Sync,
    {
        info!("Executing tool: {}", tool_use.name);

        // 1. Check if it's an MCP tool
        if tool_use.name.starts_with("mcp_") {
            if let PermissionCheck::Deny(reason) = self
                .permissions
                .check_tool_use(&tool_use.name, &tool_use.input)
            {
                return Ok(ToolResult::error(tool_use.id.clone(), reason));
            }
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
            // The single authoritative planning gate (see #465): the same
            // function the dispatch path uses, so a tool that passes dispatch
            // is not refused here by a divergent copy of the list.
            if !crate::cli::is_tool_allowed_in_mode(&tool_use.name, &current_mode) {
                drop(current_mode);
                warn!("Tool '{}' blocked in planning mode", tool_use.name);
                return Ok(ToolResult::error(
                    tool_use.id.clone(),
                    format!(
                        "Tool '{}' is not allowed in planning mode.\n\
                         Available tools: {}\n\
                         Use present_plan to show your plan for approval.",
                        tool_use.name,
                        crate::cli::PLANNING_ALLOWED_TOOLS.join(", ")
                    ),
                ));
            }
            drop(current_mode);
        }

        // 4. Execute tool with context
        let context = crate::tools::types::ToolContext {
            save_models: save_models_fn
                .as_ref()
                .map(|f| f as &(dyn Fn() -> Result<()> + Send + Sync)),
            plan_content,
            live_output,
            host_mode_state: repl_mode.map(|mode| {
                Arc::new(ReplModeState(mode)) as Arc<dyn crate::tools::types::HostModeState>
            }),
            effect_audit: effect_audit
                .map(|authority| Arc::new(authority) as Arc<dyn EffectAuditAuthority>),
            // The coordinator already showed the dialog, applied edit:*, or
            // AutoAccept. execute() must not open $EDITOR again.
            skip_interactive_review: true,
        };

        match tool.execute(tool_use.input.clone(), &context).await {
            Ok(mut output) => {
                info!("Tool executed successfully");
                self.maybe_annotate_post_edit_diagnostics(
                    tool.name(),
                    &tool_use.input,
                    &mut output,
                )
                .await;
                // Auto-push a node into the poset so the execution trace
                // becomes the Co-Forth vocabulary.
                self.poset_record_tool(&tool_use.name, &tool_use.input)
                    .await;
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

    /// Append bounded post-edit diagnostics to a completed write/edit/patch
    /// result when the user declared a source for the touched file (issue
    /// #757). Never runs for failed edits, never changes `is_error`, and is
    /// inert — no lookup, no spawn — without a declared source.
    async fn maybe_annotate_post_edit_diagnostics(
        &self,
        canonical_tool_name: &str,
        input: &serde_json::Value,
        output: &mut String,
    ) {
        let Some(service) = &self.diagnostics else {
            return;
        };
        if service.is_inert() {
            return;
        }
        if !matches!(canonical_tool_name, "write" | "edit" | "patch") {
            return;
        }
        let Some(file_path) = input.get("file_path").and_then(serde_json::Value::as_str) else {
            return;
        };
        if let Some(annotation) = service
            .annotation_for_edit_result(file_path, &self.permissions)
            .await
        {
            output.push_str(&annotation);
        }
    }

    /// Record a tool call as a node in the Co-Forth poset.
    /// Skips the `push` tool itself (it manages the poset directly).
    async fn poset_record_tool(&self, tool_name: &str, input: &serde_json::Value) {
        // Don't record meta-tools that already manage the poset.
        if matches!(tool_name, "push" | "pop_stack") {
            return;
        }
        let Some(ref poset_arc) = self.poset else {
            return;
        };

        // Build a compact label: "Read src/foo.rs", "Bash cargo test", etc.
        let label = tool_label(tool_name, input);
        let kind = tool_kind(tool_name);

        let mut poset = poset_arc.lock().await;
        let new_id = poset.add_node(label, kind, crate::poset::NodeAuthor::Ai);
        // Chain: add an edge from the previous node (if any) to this one,
        // recording the sequential execution order.
        if new_id > 0 {
            poset.edges.push((new_id - 1, new_id));
        }
    }

    /// Execute multiple tool uses in sequence
    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(self, tool_uses, save_models_fn, repl_mode, plan_content))]
    pub async fn execute_tool_loop<F>(
        &self,
        tool_uses: Vec<ToolUse>,
        save_models_fn: Option<F>,
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
                    save_models_fn.clone(),
                    repl_mode.clone(),
                    plan_content.clone(),
                    None, // live_output
                    None, // effect_audit
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

    /// Register session-scoped tools after their runtime dependencies exist.
    pub fn registry_mut(&mut self) -> &mut ToolRegistry {
        &mut self.registry
    }

    /// Get reference to permissions manager
    pub fn permissions(&self) -> &PermissionManager {
        &self.permissions
    }
}

fn path_slot_for_tool(
    tool_name: &str,
    input: &serde_json::Value,
    cwd: &std::path::Path,
    workspace_root: &std::path::Path,
) -> (Option<String>, bool) {
    match path_argument_for_tool(tool_name, input) {
        Some(raw) => {
            let in_workspace = !raw_path_escapes_workspace(&raw, workspace_root, cwd);
            (Some(raw), in_workspace)
        }
        None => (None, true),
    }
}

#[allow(clippy::too_many_arguments)]
fn signature(
    tool_name: impl Into<String>,
    context_key: String,
    command: Option<String>,
    args: Option<String>,
    directory: Option<String>,
    path: Option<String>,
    path_in_workspace: bool,
    constitutionally_denied: bool,
) -> ToolSignature {
    ToolSignature {
        tool_name: tool_name.into(),
        context_key,
        command,
        args,
        directory,
        path,
        path_in_workspace,
        constitutionally_denied,
    }
}

/// Generate a context-specific signature for a tool use
pub fn generate_tool_signature(tool_use: &ToolUse, working_dir: &std::path::Path) -> ToolSignature {
    let cwd = if working_dir.is_absolute() {
        working_dir.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(working_dir))
            .unwrap_or_else(|_| working_dir.to_path_buf())
    };
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let workspace_root = resolve_workspace_root(&cwd);
    let (path, path_in_workspace) =
        path_slot_for_tool(&tool_use.name, &tool_use.input, &cwd, &workspace_root);
    let directory = Some(cwd.display().to_string());

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

            signature(
                "bash",
                format!("{} in {}", command, cwd.display()),
                Some(base_cmd),
                args,
                directory,
                None,
                true,
                bash_command_is_constitutionally_denied(command),
            )
        }
        "read" => signature(
            "read",
            format!("reading {}", path.as_deref().unwrap_or("")),
            None,
            None,
            directory,
            path,
            path_in_workspace,
            false,
        ),
        "write" => signature(
            "write",
            format!("writing {}", path.as_deref().unwrap_or("")),
            None,
            None,
            directory,
            path,
            path_in_workspace,
            false,
        ),
        "edit" => signature(
            "edit",
            format!("editing {}", path.as_deref().unwrap_or("")),
            None,
            None,
            directory,
            path,
            path_in_workspace,
            false,
        ),
        "patch" => signature(
            "patch",
            format!("patching {}", path.as_deref().unwrap_or("")),
            None,
            None,
            directory,
            path,
            path_in_workspace,
            false,
        ),
        "glob" => {
            let pattern = tool_use.input["pattern"].as_str().unwrap_or("");
            signature(
                "glob",
                format!("pattern {}", pattern),
                None,
                None,
                directory,
                path,
                path_in_workspace,
                false,
            )
        }
        "grep" => {
            let grep_pattern = tool_use.input["pattern"].as_str().unwrap_or("");
            let grep_path = path.as_deref().unwrap_or(".");
            signature(
                "grep",
                format!("pattern '{grep_pattern}' in {grep_path}"),
                None,
                None,
                directory,
                path,
                path_in_workspace,
                false,
            )
        }
        "web_fetch" => {
            let url = tool_use.input["url"].as_str().unwrap_or("");
            signature(
                "web_fetch",
                format!("fetching {url}"),
                None,
                None,
                None,
                None,
                true,
                false,
            )
        }
        "train" => {
            let wait = tool_use.input["wait"].as_bool().unwrap_or(false);
            signature(
                tool_use.name.clone(),
                format!("train wait={wait}"),
                None,
                None,
                None,
                None,
                true,
                false,
            )
        }
        "query_local_model" => {
            let query = tool_use.input["query"].as_str().unwrap_or("");
            let truncated_query = if query.len() > 50 {
                format!("{}...", query.chars().take(50).collect::<String>())
            } else {
                query.to_string()
            };
            signature(
                tool_use.name.clone(),
                format!("query_local_model: {truncated_query}"),
                None,
                None,
                None,
                None,
                true,
                false,
            )
        }
        "analyze_model" => {
            let categories = if let Some(cats) = tool_use.input["categories"].as_array() {
                cats.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                "all".to_string()
            };
            signature(
                tool_use.name.clone(),
                format!("analyze_model categories={categories}"),
                None,
                None,
                None,
                None,
                true,
                false,
            )
        }
        "generate_training_data" => {
            let examples_count = if let Some(examples) = tool_use.input["examples"].as_array() {
                examples.len()
            } else {
                0
            };
            signature(
                tool_use.name.clone(),
                format!("generate_training_data count={examples_count}"),
                None,
                None,
                None,
                None,
                true,
                false,
            )
        }
        "compare_responses" => signature(
            tool_use.name.clone(),
            "compare_responses".to_string(),
            None,
            None,
            None,
            None,
            true,
            false,
        ),
        _ => signature(
            tool_use.name.clone(),
            format!("in {}", cwd.display()),
            None,
            None,
            directory,
            path,
            path_in_workspace,
            false,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::types::{ToolContext, ToolInputSchema};
    use crate::tools::PermissionRule;
    use crate::tools::Tool;
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

        fn effect(&self) -> finch_programs::ExecutionEffect {
            finch_programs::ExecutionEffect::Unclassified
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

    #[test]
    fn editor_backed_tools_do_not_time_out_human_review() {
        let executor = create_test_executor(true, false);
        assert!(executor.execution_timeout("bash").is_none());
        assert!(executor.execution_timeout("edit").is_none());
        assert!(executor.execution_timeout("write").is_none());
        assert_eq!(
            executor
                .execution_timeout("mock")
                .map(|duration| duration.as_secs()),
            Some(30)
        );
    }

    #[tokio::test]
    async fn test_execute_tool_success() {
        let executor = create_test_executor(true, false);
        let tool_use = ToolUse::new("mock".to_string(), serde_json::json!({"param": "value"}));

        let result = executor
            .execute_tool(
                &tool_use,
                None::<fn() -> Result<()>>,
                None, // repl_mode
                None, // plan_content
                None, // live_output
                None, // effect_audit
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
                None::<fn() -> Result<()>>,
                None, // repl_mode
                None, // plan_content
                None, // live_output
                None, // effect_audit
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
                None::<fn() -> Result<()>>,
                None, // repl_mode
                None, // plan_content
                None, // live_output
                None, // effect_audit
            )
            .await
            .unwrap();

        assert_eq!(result.tool_use_id, tool_use.id);
        assert!(result.is_error);
        assert!(result.content.contains("not allowed"));
    }

    /// A mock tool registered under an arbitrary spelling, so the
    /// planning-gate agreement test can exercise every name the gate could
    /// see without running real tool implementations.
    struct NamedMockTool {
        name: String,
    }

    #[async_trait]
    impl Tool for NamedMockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn effect(&self) -> ExecutionEffect {
            ExecutionEffect::Unclassified
        }

        fn description(&self) -> &str {
            "A mock tool registered under a specific name"
        }

        fn input_schema(&self) -> ToolInputSchema {
            ToolInputSchema::simple(vec![("param", "A test parameter")])
        }

        async fn execute(&self, _input: Value, _context: &ToolContext<'_>) -> Result<String> {
            Ok("Mock result".to_string())
        }
    }

    /// Regression for #465 (divergent verdicts): the planning dispatch gate
    /// and the executor's execution-time live check disagreed, so a tool
    /// could pass one gate and be refused by the next — `bash`, `todo_read`
    /// and `todo_write` were admitted at dispatch and then rejected at
    /// execution with a different message from a different layer. For every
    /// name the gate could see (canonical entries, dispatch aliases, and
    /// representative blocked tools), both checks must return the same
    /// verdict, in both directions.
    #[tokio::test]
    async fn test_plan_mode_live_check_agrees_with_dispatch_gate() {
        use crate::cli::ReplMode;
        use crate::cli::{
            is_tool_allowed_in_mode, PLANNING_ALLOWED_TOOLS, PLANNING_ALLOWED_TOOL_ALIASES,
        };

        let spellings: Vec<String> = PLANNING_ALLOWED_TOOLS
            .iter()
            .map(|name| name.to_string())
            .chain(
                PLANNING_ALLOWED_TOOL_ALIASES
                    .iter()
                    .map(|(alias, _)| alias.to_string()),
            )
            .chain(
                ["write", "edit", "Write", "Edit"]
                    .iter()
                    .map(|name| name.to_string()),
            )
            .collect();

        let mut registry = ToolRegistry::new();
        for spelling in &spellings {
            registry.register(Box::new(NamedMockTool {
                name: spelling.clone(),
            }));
        }
        let permissions = PermissionManager::new()
            .with_default_rule(crate::tools::permissions::PermissionRule::Allow);
        let executor = ToolExecutor::new(
            registry,
            permissions,
            std::env::temp_dir().join("finch_test_patterns.json"),
        )
        .expect("Failed to build agreement-test executor");

        let mode = Arc::new(tokio::sync::RwLock::new(ReplMode::Planning {
            task: String::new(),
            plan_path: PathBuf::from("/tmp/plan-agreement-test.md"),
            created_at: chrono::Utc::now(),
        }));

        let mut disagreements = Vec::new();
        for name in &spellings {
            let gate_allows = {
                let current_mode = mode.read().await;
                is_tool_allowed_in_mode(name, &current_mode)
            };
            let tool_use = ToolUse::new(name.clone(), json!({}));
            let (live_allows, detail) = match executor
                .execute_tool(
                    &tool_use,
                    None::<fn() -> Result<()>>,
                    Some(Arc::clone(&mode)), // repl_mode
                    None,                    // plan_content
                    None,                    // live_output
                    None,                    // effect_audit
                )
                .await
            {
                Ok(result) => (
                    !result.is_error,
                    format!("result content: {:?}", result.content),
                ),
                Err(error) => (false, format!("executor returned Err: {error:#}")),
            };
            if gate_allows != live_allows {
                disagreements.push(format!(
                    "{name:?}: dispatch gate allows={gate_allows} but executor live check allows={live_allows} ({detail})"
                ));
            }
        }

        assert!(
            disagreements.is_empty(),
            "invariant (#465): the planning dispatch gate and the executor's live check \
             must give the same verdict for the same tool in the same mode — a tool that \
             passes one gate and fails the next surfaces its refusal late, from a layer \
             with a different message; disagreements: {disagreements:#?}"
        );
    }

    #[tokio::test]
    async fn test_execute_tool_execution_failure() {
        let executor = create_test_executor(true, true);
        let tool_use = ToolUse::new("mock".to_string(), serde_json::json!({"param": "value"}));

        let result = executor
            .execute_tool(
                &tool_use,
                None::<fn() -> Result<()>>,
                None, // repl_mode
                None, // plan_content
                None, // live_output
                None, // effect_audit
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
                None::<fn() -> Result<()>>,
                None, // repl_mode,
                None, // plan_content,
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
            ..Default::default()
        };

        let sig2 = ToolSignature {
            tool_name: "bash".to_string(),
            context_key: "cargo build".to_string(),
            command: Some("cargo".to_string()),
            args: Some("build".to_string()),
            directory: None,
            ..Default::default()
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
            ..Default::default()
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

    fn isolated_workspace() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("isolated workspace");
        std::fs::create_dir(dir.path().join(".git")).expect("git root marker");
        let root = dir.path().canonicalize().expect("canonical workspace");
        (dir, root)
    }

    #[tokio::test]
    async fn test_persistent_edit_star_applies_without_editor_review() {
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register(Box::new(crate::tools::EditTool));
        let (workspace, root) = isolated_workspace();
        let tempdir = tempfile::tempdir().expect("isolated pattern store");
        let mut executor = ToolExecutor::new(
            registry,
            crate::tools::PermissionManager::new().with_workspace_root(root.clone()),
            tempdir.path().join("patterns.json"),
        )
        .expect("construct executor for edit:* grant");
        executor.approve_pattern_persistent(crate::tools::ToolPattern::new(
            "*".to_string(),
            "edit".to_string(),
            "Yes, and don't ask again for: edit:*".to_string(),
        ));

        let path = root.join("file.txt");
        std::fs::write(&path, "before\n").expect("seed file");
        let tool_use = ToolUse::new(
            "edit".to_string(),
            json!({
                "file_path": path.to_string_lossy(),
                "old_string": "before",
                "new_string": "after",
            }),
        );
        let signature = generate_tool_signature(&tool_use, &root);
        assert!(
            signature.path_in_workspace,
            "edit target must sit inside the fixture workspace so * remains a narrowing grant; \
             path={path:?} root={root:?}"
        );
        assert!(
            !matches!(
                executor.is_approved(&signature),
                ApprovalSource::NotApproved
            ),
            "edit:* must match the persistent grant so the REPL dialog is skipped; source={:?}",
            executor.is_approved(&signature)
        );

        let result = executor
            .execute_tool(
                &tool_use,
                None::<fn() -> anyhow::Result<()>>,
                None, // repl_mode,
                None, // plan_content,
                None, // live_output,
                None, // effect_audit,
            )
            .await
            .expect("granted edit must return ToolResult");
        assert!(
            !result.is_error,
            "edit:* always-allow must apply without $EDITOR; content={:?}",
            result.content
        );
        let written = std::fs::read_to_string(&path).expect("read edited file");
        assert_eq!(
            written, "after\n",
            "granted edit must write the replacement; written={written:?}"
        );
        let _keep = workspace;
    }

    #[test]
    fn test_escaped_path_is_not_pattern_admissible_through_approval_path() {
        let (workspace, root) = isolated_workspace();
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register(Box::new(crate::tools::ReadTool));
        let tempdir = tempfile::tempdir().expect("pattern store");
        let permissions = crate::tools::PermissionManager::new().with_workspace_root(root.clone());
        let mut executor =
            ToolExecutor::new(registry, permissions, tempdir.path().join("patterns.json"))
                .expect("executor");
        executor.approve_pattern_persistent(crate::tools::ToolPattern::new(
            "*".to_string(),
            "read".to_string(),
            "allow all reads".to_string(),
        ));

        let escaped = ToolUse::new(
            "read".to_string(),
            json!({"file_path": "/etc/../etc/passwd"}),
        );
        let escaped_sig = generate_tool_signature(&escaped, &root);
        assert!(
            !escaped_sig.path_in_workspace,
            "invariant: /etc/../etc/passwd must resolve outside the workspace; \
             path={:?} root={root:?}",
            escaped_sig.path
        );
        assert_eq!(
            executor.is_approved(&escaped_sig),
            ApprovalSource::NotApproved,
            "invariant: a * grant must not admit an escaped path; \
             signature={escaped_sig:?}"
        );
        assert!(
            !crate::tools::invocation_runs_autonomously(
                finch_programs::ExecutionEffect::WorkspaceRead,
                "read",
                &escaped.input,
                executor.permissions(),
            ),
            "invariant: escaped read must not auto-approve at the production spawn site"
        );

        let inside = root.join("ok.txt");
        std::fs::write(&inside, "ok").expect("seed");
        let contained = ToolUse::new(
            "read".to_string(),
            json!({"file_path": inside.to_string_lossy()}),
        );
        let contained_sig = generate_tool_signature(&contained, &root);
        assert!(
            contained_sig.path_in_workspace,
            "control: workspace file must be contained; path={inside:?}"
        );
        assert!(
            matches!(
                executor.is_approved(&contained_sig),
                ApprovalSource::PersistentPattern(_)
            ),
            "control: workspace-contained path still matches *; got {:?}",
            executor.is_approved(&contained_sig)
        );
        assert!(
            crate::tools::invocation_runs_autonomously(
                finch_programs::ExecutionEffect::WorkspaceRead,
                "read",
                &contained.input,
                executor.permissions(),
            ),
            "control: contained WorkspaceRead still runs autonomously"
        );
        let _keep = workspace;
    }

    #[test]
    fn test_dialog_minted_persistent_grant_round_trips_across_restart() {
        // #902: the persistent always-allow the TUI dialog mints must survive
        // restart. The dialog's Selected(2) produces
        // ConfirmationResult::ApprovePatternPersistent; tool_execution.rs then
        // calls approve_pattern_persistent + save_patterns(). Reproduce that
        // exact sequence, then reload the same store path into a fresh executor
        // (the restart) and assert the grant still matches.
        let (workspace, root) = isolated_workspace();
        let tempdir = tempfile::tempdir().expect("pattern store");
        let store_path = tempdir.path().join("patterns.json");

        let make = || {
            let mut registry = crate::tools::ToolRegistry::new();
            registry.register(Box::new(crate::tools::ReadTool));
            ToolExecutor::new(
                registry,
                crate::tools::PermissionManager::new().with_workspace_root(root.clone()),
                store_path.clone(),
            )
            .expect("executor")
        };

        // Session 1: user picks "3. Yes, and always allow read:*".
        let mut first = make();
        let minted = crate::tools::ToolPattern::new(
            "*".to_string(),
            "read".to_string(),
            "Allow all read calls (persistent, always allow)".to_string(),
        );
        first.approve_pattern_persistent(minted);
        first
            .save_patterns()
            .expect("persistent approval must write to disk immediately");
        assert!(
            store_path.exists(),
            "save_patterns must create the store file at {}",
            store_path.display()
        );

        // Session 2 (restart): a fresh executor loads the same path.
        let mut restarted = make();
        let inside = root.join("ok.txt");
        std::fs::write(&inside, "ok").expect("seed");
        let tool_use = ToolUse::new(
            "read".to_string(),
            json!({"file_path": inside.to_string_lossy()}),
        );
        let sig = generate_tool_signature(&tool_use, &root);
        assert!(
            sig.path_in_workspace,
            "control: fixture path must be workspace-contained; root={root:?}"
        );
        assert!(
            matches!(
                restarted.is_approved(&sig),
                ApprovalSource::PersistentPattern(_)
            ),
            "the grant minted in the previous session must still approve after \
             restart; got {:?}",
            restarted.is_approved(&sig)
        );
        let _keep = workspace;
    }

    #[cfg(unix)]
    #[test]
    fn test_symlink_escape_is_not_pattern_admissible_through_approval_path() {
        let (workspace, root) = isolated_workspace();
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register(Box::new(crate::tools::ReadTool));
        let tempdir = tempfile::tempdir().expect("pattern store");
        let mut executor = ToolExecutor::new(
            registry,
            crate::tools::PermissionManager::new().with_workspace_root(root.clone()),
            tempdir.path().join("patterns.json"),
        )
        .expect("executor");
        executor.approve_pattern_persistent(crate::tools::ToolPattern::new(
            "*".to_string(),
            "read".to_string(),
            "allow all reads".to_string(),
        ));

        let live = root.join("link-out");
        std::os::unix::fs::symlink("/etc/passwd", &live).expect("live symlink");
        let live_use = ToolUse::new(
            "read".to_string(),
            json!({"file_path": live.to_string_lossy()}),
        );
        let live_sig = generate_tool_signature(&live_use, &root);
        assert!(
            !live_sig.path_in_workspace,
            "invariant: live symlink to /etc/passwd escapes; link={live:?}"
        );
        assert_eq!(
            executor.is_approved(&live_sig),
            ApprovalSource::NotApproved,
            "invariant: * must not admit a live symlink escape; got {:?}",
            executor.is_approved(&live_sig)
        );

        let dangling = root.join("dangling-out");
        std::os::unix::fs::symlink("/no/such/finch-escape-target", &dangling)
            .expect("dangling symlink");
        let dangling_use = ToolUse::new(
            "read".to_string(),
            json!({"file_path": dangling.to_string_lossy()}),
        );
        let dangling_sig = generate_tool_signature(&dangling_use, &root);
        assert!(
            !dangling_sig.path_in_workspace,
            "invariant: dangling symlink fail-closed as outside; link={dangling:?}"
        );
        assert_eq!(
            executor.is_approved(&dangling_sig),
            ApprovalSource::NotApproved,
            "invariant: * must not admit a dangling symlink; got {:?}",
            executor.is_approved(&dangling_sig)
        );
        let _keep = workspace;
    }

    #[test]
    fn test_constitutional_bash_is_not_pattern_admissible() {
        let (workspace, root) = isolated_workspace();
        let mut executor = create_test_executor(true, false);
        executor.approve_pattern_persistent(crate::tools::ToolPattern::new(
            "*".to_string(),
            "bash".to_string(),
            "allow all bash".to_string(),
        ));
        let denied = ToolUse::new("bash".to_string(), json!({"command": "rm -rf /"}));
        let sig = generate_tool_signature(&denied, &root);
        assert!(
            sig.constitutionally_denied,
            "invariant: rm -rf is constitutionally denied"
        );
        assert_eq!(
            executor.is_approved(&sig),
            ApprovalSource::NotApproved,
            "invariant: a pattern must not admit a constitutionally Denied bash command; \
             got {:?}",
            executor.is_approved(&sig)
        );
        let _keep = workspace;
    }

    #[test]
    fn test_write_edit_patch_signatures_carry_file_path() {
        let (workspace, root) = isolated_workspace();
        let path = root.join("file.txt");
        for (name, prefix) in [
            ("write", "writing"),
            ("edit", "editing"),
            ("patch", "patching"),
        ] {
            let tool_use = ToolUse::new(
                name.to_string(),
                json!({"file_path": path.to_string_lossy(), "content": "x", "patch": ""}),
            );
            let sig = generate_tool_signature(&tool_use, &root);
            assert_eq!(sig.tool_name, name);
            assert_eq!(sig.path.as_deref(), Some(path.to_string_lossy().as_ref()));
            assert!(
                sig.path_in_workspace,
                "{name} path must be contained; sig={sig:?}"
            );
            assert!(
                sig.context_key.starts_with(prefix),
                "{name} context_key must include the path, not drop it; got {:?}",
                sig.context_key
            );
        }
        let _keep = workspace;
    }

    // ── Post-edit diagnostics (#757) production boundary ────────────────────
    //
    // These drive the real executor and the real write/edit/patch tools with
    // tiny shell-script check fixtures (never cargo), asserting what reaches
    // the model on the tool result in the same turn.

    /// A tiny executable shell script (the declared check command fixture).
    fn write_check_script(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n"))
            .unwrap_or_else(|e| panic!("write fixture {name}: {e}"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .unwrap_or_else(|e| panic!("chmod fixture {name}: {e}"));
        }
        path.to_string_lossy().into_owned()
    }

    fn diagnostics_config(
        command: &str,
        timeout_secs: u64,
        max_output_chars: usize,
    ) -> crate::config::DiagnosticsConfig {
        crate::config::DiagnosticsConfig {
            check: vec![crate::config::CheckCommandSource {
                extensions: vec!["txt".to_string()],
                command: command.to_string(),
            }],
            timeout_secs,
            max_output_chars,
        }
    }

    fn executor_with_edit_tool(
        permissions: PermissionManager,
        diagnostics: &crate::config::DiagnosticsConfig,
    ) -> ToolExecutor {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::EditTool));
        let tempdir = tempfile::tempdir().expect("isolated pattern store");
        ToolExecutor::new(registry, permissions, tempdir.path().join("patterns.json"))
            .expect("construct executor")
            .with_diagnostics(diagnostics)
    }

    async fn run_edit(executor: &ToolExecutor, path: &Path, old: &str, new: &str) -> ToolResult {
        let tool_use = ToolUse::new(
            "edit".to_string(),
            json!({
                "file_path": path.to_string_lossy(),
                "old_string": old,
                "new_string": new,
            }),
        );
        executor
            .execute_tool(
                &tool_use,
                None::<fn() -> Result<()>>,
                None, // repl_mode
                None, // plan_content
                None, // live_output
                None, // effect_audit
            )
            .await
            .expect("edit execution must not error at the executor boundary")
    }

    #[tokio::test]
    async fn test_edit_result_carries_bounded_diagnostics_from_declared_check_command() {
        let (workspace, root) = isolated_workspace();
        let script = write_check_script(
            &root,
            "check.sh",
            "echo 'error[E0308]: mismatched types --> lib.txt:3:5'\nexit 1\n",
        );
        let declared = diagnostics_config(&script, 5, 2000);
        let executor = executor_with_edit_tool(
            PermissionManager::new()
                .with_default_rule(PermissionRule::Allow)
                .with_workspace_root(root.clone()),
            &declared,
        );

        let path = root.join("lib.txt");
        std::fs::write(&path, "before\n").expect("seed edit target");
        let result = run_edit(&executor, &path, "before", "after").await;

        assert!(
            !result.is_error,
            "the edit itself succeeded; diagnostics must never fail the edit; result: {result:?}"
        );
        assert!(
            result.content.contains("+after"),
            "the diff must still be present on the result; result: {result:?}"
        );
        assert!(
            result
                .content
                .contains("[post-edit diagnostics — declared check command]"),
            "the diagnostics annotation must be on the SAME tool result, same turn; \
             result: {result:?}"
        );
        assert!(
            result.content.contains("error[E0308]") && result.content.contains("exit 1"),
            "the annotation must carry the bounded check output and status; result: {result:?}"
        );
        let _keep = workspace;
    }

    #[tokio::test]
    async fn test_undeclared_source_leaves_edit_result_unchanged_and_executes_nothing() {
        let (workspace, root) = isolated_workspace();
        // The check fixture exists on disk but nothing declares it as a
        // diagnostics source, so it must never run.
        let _undeclared_script = write_check_script(&root, "check.sh", "touch marker; echo ran\n");
        let marker = root.join("marker");
        let inert = crate::config::DiagnosticsConfig::default();
        let no_sources = executor_with_edit_tool(
            PermissionManager::new()
                .with_default_rule(PermissionRule::Allow)
                .with_workspace_root(root.clone()),
            &inert,
        );

        let path = root.join("lib.txt");
        std::fs::write(&path, "before\n").expect("seed edit target");
        let annotated = run_edit(&no_sources, &path, "before", "after").await;

        // The same edit without any diagnostics service must be byte-identical.
        let mut plain_registry = ToolRegistry::new();
        plain_registry.register(Box::new(crate::tools::EditTool));
        let tempdir = tempfile::tempdir().expect("isolated pattern store");
        let plain = ToolExecutor::new(
            plain_registry,
            PermissionManager::new()
                .with_default_rule(PermissionRule::Allow)
                .with_workspace_root(root.clone()),
            tempdir.path().join("patterns.json"),
        )
        .expect("construct plain executor");
        std::fs::write(&path, "before\n").expect("reseed edit target");
        let plain_result = run_edit(&plain, &path, "before", "after").await;

        assert_eq!(
            annotated.content, plain_result.content,
            "an undeclared source must leave the edit result byte-identical to the \
             pre-feature behavior; annotated: {:?}\nplain: {:?}",
            annotated.content, plain_result.content
        );
        assert!(
            !marker.exists(),
            "an undeclared source must never execute the check command"
        );
        let _keep = workspace;
    }

    #[tokio::test]
    async fn test_failed_edit_produces_no_diagnostics_annotation() {
        let (workspace, root) = isolated_workspace();
        let script = write_check_script(&root, "check.sh", "echo checked\n");
        let declared = diagnostics_config(&script, 5, 2000);
        let executor = executor_with_edit_tool(
            PermissionManager::new()
                .with_default_rule(PermissionRule::Allow)
                .with_workspace_root(root.clone()),
            &declared,
        );

        let path = root.join("lib.txt");
        std::fs::write(&path, "before\n").expect("seed edit target");
        let result = run_edit(&executor, &path, "missing-anchor", "after").await;

        assert!(
            result.is_error,
            "an unapplicable edit must remain an error; result: {result:?}"
        );
        assert!(
            !result.content.contains("post-edit diagnostics"),
            "a failed edit must not run diagnostics; result: {result:?}"
        );
        let _keep = workspace;
    }

    #[tokio::test]
    async fn test_write_result_also_carries_declared_diagnostics() {
        let (workspace, root) = isolated_workspace();
        let script = write_check_script(&root, "check.sh", "echo 'warning: unused' \nexit 0\n");
        let declared = diagnostics_config(&script, 5, 2000);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::WriteTool));
        let tempdir = tempfile::tempdir().expect("isolated pattern store");
        let executor = ToolExecutor::new(
            registry,
            PermissionManager::new()
                .with_default_rule(PermissionRule::Allow)
                .with_workspace_root(root.clone()),
            tempdir.path().join("patterns.json"),
        )
        .expect("construct executor")
        .with_diagnostics(&declared);

        let path = root.join("notes.txt");
        let tool_use = ToolUse::new(
            "write".to_string(),
            json!({"file_path": path.to_string_lossy(), "content": "written\n"}),
        );
        let result = executor
            .execute_tool(
                &tool_use,
                None::<fn() -> Result<()>>,
                None, // repl_mode,
                None, // plan_content,
                None, // live_output,
                None, // effect_audit,
            )
            .await
            .expect("write execution");

        assert!(!result.is_error, "write must succeed; result: {result:?}");
        assert!(
            result.content.contains("post-edit diagnostics")
                && result.content.contains("check passed (exit 0)")
                && result.content.contains("warning: unused"),
            "write results must carry the same bounded annotation; result: {result:?}"
        );
        let _keep = workspace;
    }

    #[tokio::test]
    async fn test_patch_result_also_carries_declared_diagnostics() {
        let (workspace, root) = isolated_workspace();
        let script = write_check_script(&root, "check.sh", "echo 'error: bad token'\nexit 2\n");
        let declared = diagnostics_config(&script, 5, 2000);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::PatchTool));
        let tempdir = tempfile::tempdir().expect("isolated pattern store");
        let executor = ToolExecutor::new(
            registry,
            PermissionManager::new()
                .with_default_rule(PermissionRule::Allow)
                .with_workspace_root(root.clone()),
            tempdir.path().join("patterns.json"),
        )
        .expect("construct executor")
        .with_diagnostics(&declared);

        let path = root.join("patched.txt");
        std::fs::write(&path, "alpha\nbeta\n").expect("seed patch target");
        let tool_use = ToolUse::new(
            "patch".to_string(),
            json!({
                "file_path": path.to_string_lossy(),
                "patch": "@@ -1,2 +1,2 @@\n alpha\n-beta\n+BETA\n",
            }),
        );
        let result = executor
            .execute_tool(
                &tool_use,
                None::<fn() -> Result<()>>,
                None, // repl_mode,
                None, // plan_content,
                None, // live_output,
                None, // effect_audit,
            )
            .await
            .expect("patch execution");

        assert!(
            !result.is_error && result.content.contains("post-edit diagnostics"),
            "patch results must carry the same bounded annotation; result: {result:?}"
        );
        let _keep = workspace;
    }

    #[tokio::test]
    async fn test_peer_session_declared_check_command_is_ask_user_and_never_executes() {
        let (workspace, root) = isolated_workspace();
        let script = write_check_script(&root, "check.sh", "touch marker; echo ran\n");
        let marker = root.join("marker");
        let declared = diagnostics_config(&script, 5, 2000);
        let executor = executor_with_edit_tool(
            crate::tools::PermissionManager::for_peer().with_workspace_root(root.clone()),
            &declared,
        );

        let path = root.join("lib.txt");
        std::fs::write(&path, "before\n").expect("seed edit target");
        let result = run_edit(&executor, &path, "before", "after").await;

        assert!(
            !result.is_error,
            "the edit itself proceeds through its own authority path; result: {result:?}"
        );
        assert!(
            result.content.contains("skipped:"),
            "the peer result must declare the diagnostics skip, not silently omit \
             them; result: {result:?}"
        );
        assert!(
            !marker.exists(),
            "a peer must never execute the declared check command without the \
             approval bash would require"
        );
        let _keep = workspace;
    }
}

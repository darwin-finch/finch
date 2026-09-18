// Permission system for tool execution
//
// Implements constitutional constraints: "Would 1000 users do this?"
// Multi-layer defense: Allow, Ask, or Deny tool execution

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use tracing::{debug, warn};

use crate::programs::ExecutionEffect;

/// Constitutional bash denials. A pattern must not admit any of these
/// commands: the one-shot path Denies them, and a generalised approval
/// cannot widen that.
const BASH_DENIED_SUBSTRINGS: &[(&str, &str)] = &[
    ("rm -rf", "Recursive deletion is dangerous"),
    ("dd if=", "Disk operations are dangerous"),
    (":(){ :|:& };:", "Fork bombs are blocked"),
    ("sudo", "Privilege escalation requires manual execution"),
    ("chmod 777", "Unsafe permission changes are blocked"),
    ("> /dev/", "Direct device access is dangerous"),
    ("mkfs", "Filesystem operations are dangerous"),
    ("fdisk", "Disk partitioning is dangerous"),
];

/// Tools whose discrete path argument is `file_path`.
const FILE_PATH_TOOLS: &[&str] = &["read", "write", "edit", "patch"];

/// True when a bash command is constitutionally Denied on the one-shot path.
///
/// Patterns consult this at match time so a stored `*` grant cannot admit
/// `rm -rf` (or the rest of the denylist) after seeing a harmless command.
pub fn bash_command_is_constitutionally_denied(command: &str) -> bool {
    BASH_DENIED_SUBSTRINGS
        .iter()
        .any(|(pattern, _)| command.contains(pattern))
}

/// Workspace root used for path-argument containment.
///
/// Walking up from `start`, the first directory that contains `.git`
/// (directory or file — git worktrees count) is the root; otherwise `start`.
/// The result is canonicalised when that directory exists.
pub fn resolve_workspace_root(start: &Path) -> PathBuf {
    let start_abs = make_absolute(start);
    let mut dir = start_abs.as_path();
    loop {
        let git = dir.join(".git");
        if git.is_dir() || git.is_file() {
            return dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    start_abs
        .canonicalize()
        .unwrap_or_else(|_| start_abs.clone())
}

/// Resolve `path` against `cwd`, following symlinks on existing prefixes.
///
/// `.` and `..` are collapsed. A non-existent path canonicalises the longest
/// existing ancestor and joins the lexically normalised remainder. A symlink
/// whose target cannot be resolved returns `None` (fail closed: treat as
/// outside the workspace).
pub fn resolve_canonical_path(path: &Path, cwd: &Path) -> Option<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        make_absolute(cwd).join(path)
    };

    let mut resolved = PathBuf::new();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut past_existing = false;

    for component in abs.components() {
        match component {
            Component::Prefix(prefix) => {
                if past_existing {
                    missing.push(prefix.as_os_str().to_os_string());
                } else {
                    resolved.push(prefix.as_os_str());
                }
            }
            Component::RootDir => {
                if past_existing {
                    missing.push(component.as_os_str().to_os_string());
                } else {
                    resolved.push(component);
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if past_existing {
                    if missing.last().is_some_and(|part| part != "..") {
                        missing.pop();
                    } else {
                        missing.push(std::ffi::OsString::from(".."));
                    }
                } else {
                    resolved.pop();
                }
            }
            Component::Normal(name) => {
                if past_existing {
                    missing.push(name.to_os_string());
                    continue;
                }
                resolved.push(name);
                match std::fs::symlink_metadata(&resolved) {
                    Ok(meta) if meta.file_type().is_symlink() => match resolved.canonicalize() {
                        Ok(canonical) => resolved = canonical,
                        Err(_) => return None,
                    },
                    Ok(_) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        resolved.pop();
                        past_existing = true;
                        missing.push(name.to_os_string());
                    }
                    Err(_) => return None,
                }
            }
        }
    }

    if resolved.as_os_str().is_empty() {
        resolved = PathBuf::from(std::path::MAIN_SEPARATOR_STR);
    }

    if !past_existing {
        return if resolved.exists() {
            resolved.canonicalize().ok()
        } else {
            Some(resolved)
        };
    }

    if resolved.exists() {
        resolved = resolved.canonicalize().ok()?;
    }
    for part in missing {
        if part == ".." {
            resolved.pop();
        } else if part != "." {
            resolved.push(part);
        }
    }
    Some(resolved)
}

/// True when `canonical` is the workspace root or a descendant of it.
pub fn path_is_inside_workspace(canonical: &Path, root: &Path) -> bool {
    let path_parts: Vec<_> = canonical.components().collect();
    let root_parts: Vec<_> = root.components().collect();
    path_parts.starts_with(&root_parts)
}

/// Discrete path argument for tools that have one. Bash has no path slot.
pub fn path_argument_for_tool(tool_name: &str, input: &Value) -> Option<String> {
    let raw = if FILE_PATH_TOOLS.contains(&tool_name) {
        input.get("file_path").and_then(Value::as_str)?
    } else if tool_name == "grep" {
        input.get("path").and_then(Value::as_str).unwrap_or(".")
    } else if tool_name == "glob" {
        let pattern = input.get("pattern").and_then(Value::as_str)?;
        glob_path_argument(pattern)?
    } else {
        return None;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Relative globs stay workspace-relative. Absolute globs and those containing
/// `..` contribute a path slot (the literal prefix before glob metacharacters).
fn glob_path_argument(pattern: &str) -> Option<&str> {
    let path = Path::new(pattern);
    let needs_check = path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir));
    if !needs_check {
        return None;
    }
    let end = pattern
        .find(|c| matches!(c, '*' | '?' | '['))
        .unwrap_or(pattern.len());
    let prefix = pattern[..end].trim_end_matches(['/', '\\']);
    if prefix.is_empty() {
        Some(pattern)
    } else {
        Some(prefix)
    }
}

fn expand_user(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn make_absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    }
}

/// True when the tool's path argument resolves outside the workspace, or
/// cannot be resolved (fail closed). Tools without a path slot never escape.
pub fn path_argument_escapes_workspace(
    tool_name: &str,
    input: &Value,
    workspace_root: &Path,
    cwd: &Path,
) -> bool {
    let Some(raw) = path_argument_for_tool(tool_name, input) else {
        return false;
    };
    raw_path_escapes_workspace(&raw, workspace_root, cwd)
}

/// True when `raw` resolves outside `workspace_root` (or cannot be resolved).
pub fn raw_path_escapes_workspace(raw: &str, workspace_root: &Path, cwd: &Path) -> bool {
    match resolve_canonical_path(&expand_user(raw), cwd) {
        Some(canonical) => !path_is_inside_workspace(&canonical, workspace_root),
        None => true,
    }
}

fn escape_ask_reason(tool_name: &str) -> String {
    format!(
        "Path is outside the workspace — approve this one {tool_name} path? \
         A stored pattern cannot authorise paths outside the workspace."
    )
}

/// Registered tools that only inspect Finch's own typed runtime metadata.
/// They neither access the workspace nor cross a host-effect boundary, so a
/// provider must be able to use them to discover the VM protocol without
/// interrupting the user for an approval dialog.
///
/// Every entry must be a name some `Tool` registers or an alias key;
/// conformance-tested in `src/cli/repl/always_allow_tests.rs`.
pub const VM_DISCOVERY_TOOLS: &[&str] = &[
    "get_vm_state",
    "get_language_definition",
    "search_vm_vocabulary",
    "inspect_vm_word",
    "search_word",
    "inspect_word",
    "search_vocabulary",
    "inspect_program",
];

/// Registered tool names a peer is hard-denied regardless of configuration.
///
/// Keyed on the names the `Tool` implementations register — `restart_session`
/// (`implementations/restart.rs`) and `spawn_task` (`implementations/spawn.rs`)
/// — not on unregistered literals, so a rename cannot silently strand the deny
/// arm (the regression that made `test_peer_cannot_restart` and
/// `test_peer_cannot_spawn` pass while proving nothing). Peer agent
/// coordination (`spawn_agent`, `await_agent`, `poll_agent`, `cancel_agent`)
/// is scheduler-local and enforces task-tree ownership itself, so those names
/// stay on [`PEER_SILENT_ALLOW_TOOLS`].
///
/// Every entry must be a name some `Tool` registers or an alias key;
/// conformance-tested in `src/cli/repl/always_allow_tests.rs`.
pub const PEER_HARD_DENY_TOOLS: &[&str] = &["restart_session", "spawn_task"];

/// Registered tool names a peer may invoke silently, without an approval
/// dialog: read-only examination plus scheduler-local agent control.
///
/// Every entry must be a name some `Tool` registers or an alias key;
/// conformance-tested in `src/cli/repl/always_allow_tests.rs`.
pub const PEER_SILENT_ALLOW_TOOLS: &[&str] = &[
    "read",
    "glob",
    "grep",
    "get_vm_state",
    "get_language_definition",
    "search_vm_vocabulary",
    "inspect_vm_word",
    "search_word",
    "inspect_word",
    "search_vocabulary",
    "inspect_program",
    "spawn_agent",
    "await_agent",
    "poll_agent",
    "cancel_agent",
];

/// Registered tool names through which a peer proposes file changes. Each
/// surfaces as AskUser so a human reviews the diff before anything applies.
///
/// Every entry must be a name some `Tool` registers or an alias key;
/// conformance-tested in `src/cli/repl/always_allow_tests.rs`.
pub const PEER_REVIEWED_CHANGESET_TOOLS: &[&str] = &["write", "edit", "patch"];

/// Registered tool names the [`crate::tools::ToolExecutor`] admits while the
/// session is in `Planning` mode, keyed on the names the `Tool`
/// implementations register plus the dispatch-only alias keys the REPL
/// registry covers (issue #466). The executor table is narrower than the

/// Permission decision for a tool execution
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionCheck {
    /// Execute immediately without user confirmation
    Allow,

    /// Prompt user with explanation before executing
    AskUser(String),

    /// Block execution with reason
    Deny(String),
}

/// Permission rule configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PermissionRule {
    Allow,
    Ask,
    Deny,
}

/// Configuration for a specific tool's permissions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPermissionConfig {
    pub enabled: bool,
    pub rule: PermissionRule,
    #[serde(default)]
    pub allowed_patterns: Vec<String>,
    #[serde(default)]
    pub blocked_patterns: Vec<String>,
}

impl Default for ToolPermissionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rule: PermissionRule::Ask,
            allowed_patterns: Vec::new(),
            blocked_patterns: Vec::new(),
        }
    }
}

/// Who is executing the tool — affects permission defaults.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecutorRole {
    /// The human owner of the session. Default rules apply as configured.
    Owner,
    /// An AI peer in the room. Asymmetric rules:
    ///   - Read/glob/grep: Allow silently
    ///   - Write/edit/patch: surfaces as DiffPropose in the room (never auto-apply)
    ///   - Bash (read-only patterns): Allow
    ///   - Bash (side-effects): AskUser — dialog appears in the shared room
    ///   - Restart/recompile: always Deny
    Peer,
}

/// Permission manager - checks if tool execution is allowed
pub struct PermissionManager {
    /// Per-tool configuration
    configs: HashMap<String, ToolPermissionConfig>,

    /// Default rule for tools without explicit config
    default_rule: PermissionRule,

    /// Maximum number of tool turns (prevent infinite loops)
    pub max_tool_turns: usize,

    /// Role of the executor — Owner gets configured rules, Peer gets asymmetric rules.
    pub role: ExecutorRole,

    /// Directory relative path arguments resolve against.
    cwd: PathBuf,

    /// Canonical workspace root (git root if present, else `cwd`).
    workspace_root: PathBuf,
}

fn default_workspace_context() -> (PathBuf, PathBuf) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let workspace_root = resolve_workspace_root(&cwd);
    (cwd, workspace_root)
}

impl PermissionManager {
    /// Create new permission manager with default settings (Owner role).
    pub fn new() -> Self {
        let (cwd, workspace_root) = default_workspace_context();
        Self {
            configs: HashMap::new(),
            default_rule: PermissionRule::Ask,
            max_tool_turns: 25,
            role: ExecutorRole::Owner,
            cwd,
            workspace_root,
        }
    }

    /// Create a permission manager for an AI peer (asymmetric rules).
    pub fn for_peer() -> Self {
        let (cwd, workspace_root) = default_workspace_context();
        Self {
            configs: HashMap::new(),
            default_rule: PermissionRule::Ask,
            max_tool_turns: 25,
            role: ExecutorRole::Peer,
            cwd,
            workspace_root,
        }
    }

    /// Load from configuration
    pub fn from_config(configs: HashMap<String, ToolPermissionConfig>) -> Self {
        let (cwd, workspace_root) = default_workspace_context();
        Self {
            configs,
            default_rule: PermissionRule::Ask,
            max_tool_turns: 25,
            role: ExecutorRole::Owner,
            cwd,
            workspace_root,
        }
    }

    /// Pin path resolution to an explicit workspace (tests: pass a temp dir
    /// that contains `.git`; do not `chdir`).
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        let cwd = make_absolute(&root);
        let cwd = cwd.canonicalize().unwrap_or(cwd);
        self.cwd = cwd.clone();
        self.workspace_root = resolve_workspace_root(&cwd);
        self
    }

    /// Canonical workspace root used for containment.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Directory relative path arguments resolve against.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Set default rule for unconfigured tools
    pub fn with_default_rule(mut self, rule: PermissionRule) -> Self {
        self.default_rule = rule;
        self
    }

    /// Set maximum tool turns
    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_tool_turns = max_turns;
        self
    }

    /// Register tool-specific configuration
    pub fn register_tool_config(&mut self, tool_name: String, config: ToolPermissionConfig) {
        self.configs.insert(tool_name, config);
    }

    /// Whether policy allows advertising this tool to a provider.
    ///
    /// Input-dependent constitutional checks run at execution, not
    /// advertisement. Disabled tools and explicit Deny rules are not
    /// advertised. Peer hard-deny tools are never advertised to a peer.
    pub fn allows_advertising(&self, tool_name: &str) -> bool {
        if self.role == ExecutorRole::Peer && PEER_HARD_DENY_TOOLS.contains(&tool_name) {
            return false;
        }
        if let Some(config) = self.configs.get(tool_name) {
            if !config.enabled || config.rule == PermissionRule::Deny {
                return false;
            }
        }
        true
    }

    /// Check if tool execution is permitted
    pub fn check_tool_use(&self, tool_name: &str, input: &Value) -> PermissionCheck {
        // Peer role: asymmetric rules take precedence over per-tool config
        if self.role == ExecutorRole::Peer {
            return self.check_peer_tool_use(tool_name, input);
        }

        // Get tool config or use default
        let config = self.configs.get(tool_name);

        // Check if tool is enabled
        if let Some(cfg) = config {
            if !cfg.enabled {
                return PermissionCheck::Deny(format!("Tool '{}' is disabled", tool_name));
            }
        }

        // Apply constitutional constraints (safety checks)
        if let Some(reason) = self.check_constitutional_constraints(tool_name, input) {
            return PermissionCheck::Deny(reason);
        }

        // Escape is one-shot AskUser. A pattern can never satisfy it, and
        // WorkspaceRead autonomy must not skip the dialog.
        if self.path_escapes(tool_name, input) {
            return PermissionCheck::AskUser(escape_ask_reason(tool_name));
        }

        // These tools inspect Finch's own typed runtime metadata only. They
        // neither access the workspace nor cross a host-effect boundary, so a
        // provider must be able to use them to discover the VM protocol
        // without interrupting the user for an approval dialog.
        if VM_DISCOVERY_TOOLS.contains(&tool_name) {
            return PermissionCheck::Allow;
        }

        // Entering the typed broker is not itself authority. The verifier derives
        // concrete capability requirements from the program and the runtime
        // suspends on any requirement that is not granted. Never trust the
        // provider-supplied coarse `effect` label at this outer compatibility
        // boundary.
        if tool_name == "submit_program" {
            return PermissionCheck::Allow;
        }

        // Apply tool-specific patterns
        if let Some(cfg) = config {
            if let Some(reason) = self.check_patterns(tool_name, input, cfg) {
                return reason;
            }
        }

        // Use configured rule or default
        match config.map(|c| &c.rule).unwrap_or(&self.default_rule) {
            PermissionRule::Allow => PermissionCheck::Allow,
            PermissionRule::Ask => PermissionCheck::AskUser(format!("Execute {} tool?", tool_name)),
            PermissionRule::Deny => {
                PermissionCheck::Deny(format!("Tool '{}' is not allowed", tool_name))
            }
        }
    }

    /// Asymmetric permission check for AI peers.
    fn check_peer_tool_use(&self, tool_name: &str, input: &Value) -> PermissionCheck {
        // Hard deny: a peer cannot restart/recompile/kill the session or spawn
        // processes. Keyed on the registered tool names in PEER_HARD_DENY_TOOLS
        // so an unregistered spelling can never silently replace the deny.
        if PEER_HARD_DENY_TOOLS.contains(&tool_name) {
            return PermissionCheck::Deny("Peer cannot restart or spawn processes".to_string());
        }

        // Constitutional constraints still apply to everyone
        if let Some(reason) = self.check_constitutional_constraints(tool_name, input) {
            return PermissionCheck::Deny(reason);
        }

        // Containment before silent-allow: a peer cannot read an escaped
        // path without a one-shot AskUser. Scheduler children already bail
        // on AskUser.
        if self.path_escapes(tool_name, input) {
            return PermissionCheck::AskUser(escape_ask_reason(tool_name));
        }

        // Silent allow: read-only examination and scheduler-local control.
        // Agent tools enforce task-tree ownership themselves.
        if PEER_SILENT_ALLOW_TOOLS.contains(&tool_name) {
            return PermissionCheck::Allow;
        }

        // The typed broker, not this compatibility tool gate, authorizes
        // every concrete host effect inferred from a submitted program.
        if tool_name == "submit_program" {
            return PermissionCheck::Allow;
        }

        // Write/edit/patch: require an explicit reviewed changeset.
        if PEER_REVIEWED_CHANGESET_TOOLS.contains(&tool_name) {
            return PermissionCheck::AskUser(
                "Model proposes a file change — review diff".to_string(),
            );
        }

        // Bash: allow read-only commands silently, ask for everything else
        if tool_name == "bash" {
            let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            return if is_readonly_bash(command) {
                PermissionCheck::Allow
            } else {
                PermissionCheck::AskUser("Peer wants to run a shell command — approve?".to_string())
            };
        }

        // Everything else: ask
        PermissionCheck::AskUser(format!("Peer wants to use '{}' — approve?", tool_name))
    }

    /// Apply constitutional constraints (safety checks)
    fn check_constitutional_constraints(&self, tool_name: &str, input: &Value) -> Option<String> {
        match tool_name {
            // The background bash sibling runs the same shell authority as
            // `bash`, so the same denied substrings apply to it.
            "bash" | "background_bash" => self.check_bash_safety(input),
            "web_fetch" => self.check_web_fetch_safety(input),
            _ => None,
        }
    }

    fn path_escapes(&self, tool_name: &str, input: &Value) -> bool {
        path_argument_escapes_workspace(tool_name, input, &self.workspace_root, &self.cwd)
    }

    /// Check if bash command is safe
    fn check_bash_safety(&self, input: &Value) -> Option<String> {
        let command = input.get("command")?.as_str()?;

        for (pattern, reason) in BASH_DENIED_SUBSTRINGS {
            if command.contains(pattern) {
                warn!("Blocked dangerous bash command: {}", command);
                return Some(format!("Blocked: {}", reason));
            }
        }

        None
    }

    /// Check if web fetch is safe
    fn check_web_fetch_safety(&self, input: &Value) -> Option<String> {
        let url = input.get("url")?.as_str()?;

        // Block dangerous URL schemes
        let blocked_schemes = vec!["file://", "javascript:", "data:", "vbscript:"];

        for scheme in blocked_schemes {
            if url.to_lowercase().starts_with(scheme) {
                warn!("Blocked dangerous URL scheme: {}", url);
                return Some(format!("Blocked: URL scheme '{}' is not allowed", scheme));
            }
        }

        // Block private IP ranges
        if Self::is_private_url(url) {
            warn!("Blocked private IP access: {}", url);
            return Some("Blocked: Access to private IP addresses is not allowed".to_string());
        }

        None
    }

    /// Check if URL points to private IP
    fn is_private_url(url: &str) -> bool {
        // Simple check for common private IPs
        let private_patterns = vec![
            "127.0.0.1",
            "localhost",
            "192.168.",
            "10.",
            "172.16.",
            "172.17.",
            "172.18.",
            "172.19.",
            "172.20.",
            "172.21.",
            "172.22.",
            "172.23.",
            "172.24.",
            "172.25.",
            "172.26.",
            "172.27.",
            "172.28.",
            "172.29.",
            "172.30.",
            "172.31.",
        ];

        private_patterns.iter().any(|p| url.contains(p))
    }

    /// Check tool-specific allowed/blocked patterns
    fn check_patterns(
        &self,
        tool_name: &str,
        input: &Value,
        config: &ToolPermissionConfig,
    ) -> Option<PermissionCheck> {
        let input_str = serde_json::to_string(input).ok()?;

        // Check blocked patterns first
        for pattern in &config.blocked_patterns {
            if input_str.contains(pattern) {
                debug!("Tool {} blocked by pattern: {}", tool_name, pattern);
                return Some(PermissionCheck::Deny(format!(
                    "Blocked by pattern: {}",
                    pattern
                )));
            }
        }

        // If allowed patterns specified, input must match one
        if !config.allowed_patterns.is_empty() {
            let matches = config
                .allowed_patterns
                .iter()
                .any(|p| input_str.contains(p));
            if !matches {
                return Some(PermissionCheck::AskUser(format!(
                    "Tool {} input doesn't match allowed patterns",
                    tool_name
                )));
            }
        }

        None
    }
}

/// Returns true if a bash command is read-only (no side effects).
///
/// Conservative: any shell operator (`;`, `|`, `>`, `<`, `&`) causes a false return,
/// because operators can chain destructive commands after a harmless-looking prefix.
fn is_readonly_bash(command: &str) -> bool {
    let trimmed = command.trim();

    // Reject anything that could chain or redirect — too hard to parse safely.
    // This catches "ls; rm file", "cat foo | tee out", "echo hi > file", etc.
    if trimmed
        .chars()
        .any(|c| matches!(c, ';' | '|' | '>' | '<' | '&'))
    {
        return false;
    }

    let readonly_prefixes = [
        "ls",
        "cat",
        "head",
        "tail",
        "echo",
        "pwd",
        "find",
        "grep",
        "rg",
        "wc",
        "diff",
        "file",
        "stat",
        "which",
        "type",
        "env",
        "printenv",
        "uname",
        "whoami",
        "id",
        "ps",
        "df",
        "du",
        "lsof",
        "netstat",
        "ss",
        "curl -s",
        "curl --silent",
    ];
    readonly_prefixes.iter().any(|p| trimmed.starts_with(p))
}

/// Effect a tool use presents at the approval boundary.
///
/// [`Tool::effect`] is the declared authority and cannot see the invocation
/// input, so bash declares its worst case (`ExternalWrite`). This refinement
/// is applied **at the approval call sites that consume the effect**: a
/// read-only bash command (no shell operators, read-only prefix —
/// [`is_readonly_bash`]) presents as `WorkspaceRead`, so it stays autonomous
/// without ever widening the declared effect itself. Every other tool's
/// effect passes through untouched.
///
/// The `"bash"` literal is pinned to the name the real `BashTool` registers
/// by `test_bash_readonly_refinement_applies_to_the_registered_bash_tool_name`.
pub fn refined_effect_for_approval(
    declared: ExecutionEffect,
    tool_name: &str,
    input: &Value,
) -> ExecutionEffect {
    if tool_name == "bash"
        && declared == ExecutionEffect::ExternalWrite
        && is_readonly_bash(input.get("command").and_then(Value::as_str).unwrap_or(""))
    {
        return ExecutionEffect::WorkspaceRead;
    }
    declared
}

/// Production auto-approve predicate: refined-effect autonomy, but never
/// for a path that escapes the workspace.
///
/// `execute_tool` treats AskUser as already confirmed, so replacing the
/// denylist Deny with escape AskUser would *execute* escaped reads unless
/// this gate is false. Do not overload [`refined_effect_for_approval`] by
/// widening a read into `Unclassified`.
pub fn invocation_runs_autonomously(
    declared: ExecutionEffect,
    tool_name: &str,
    input: &Value,
    permissions: &PermissionManager,
) -> bool {
    if permissions.path_escapes(tool_name, input) {
        return false;
    }
    refined_effect_for_approval(declared, tool_name, input).runs_autonomously()
}

impl Default for PermissionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_bash_dangerous_commands_blocked() {
        let manager = PermissionManager::new();

        let dangerous_commands = vec![
            "rm -rf /",
            "dd if=/dev/zero of=/dev/sda",
            ":(){ :|:& };:",
            "sudo rm file",
            "chmod 777 /etc",
        ];

        for cmd in dangerous_commands {
            let input = serde_json::json!({"command": cmd});
            let check = manager.check_tool_use("bash", &input);
            assert!(
                matches!(check, PermissionCheck::Deny(_)),
                "Failed to block: {}",
                cmd
            );
        }
    }

    #[test]
    fn test_system_files_blocked() {
        // Escape is one-shot AskUser, not Deny: the user may authorise that
        // specific path. A pattern can never satisfy it.
        let manager = PermissionManager::new();

        let system_files = vec!["/etc/passwd", "/etc/shadow", "/dev/null"];

        for file in system_files {
            let input = serde_json::json!({"file_path": file});
            let check = manager.check_tool_use("read", &input);
            assert!(
                matches!(check, PermissionCheck::AskUser(_)),
                "invariant: escaped system path {file} must be one-shot AskUser, \
                 not Deny or Allow; got {check:?}"
            );
        }
    }

    #[test]
    fn test_dangerous_url_schemes_blocked() {
        let manager = PermissionManager::new();

        let dangerous_urls = vec![
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
        ];

        for url in dangerous_urls {
            let input = serde_json::json!({"url": url});
            let check = manager.check_tool_use("web_fetch", &input);
            assert!(
                matches!(check, PermissionCheck::Deny(_)),
                "Failed to block: {}",
                url
            );
        }
    }

    #[test]
    fn test_private_ip_blocked() {
        let manager = PermissionManager::new();

        let private_urls = vec![
            "http://127.0.0.1/",
            "http://localhost/",
            "http://192.168.1.1/",
            "http://10.0.0.1/",
        ];

        for url in private_urls {
            let input = serde_json::json!({"url": url});
            let check = manager.check_tool_use("web_fetch", &input);
            assert!(
                matches!(check, PermissionCheck::Deny(_)),
                "Failed to block: {}",
                url
            );
        }
    }

    #[test]
    fn test_safe_bash_command_requires_ask() {
        let manager = PermissionManager::new();

        let input = serde_json::json!({"command": "ls -la"});
        let check = manager.check_tool_use("bash", &input);
        assert!(matches!(check, PermissionCheck::AskUser(_)));
    }

    #[test]
    fn test_disabled_tool() {
        let mut manager = PermissionManager::new();
        manager.register_tool_config(
            "bash".to_string(),
            ToolPermissionConfig {
                enabled: false,
                rule: PermissionRule::Allow,
                allowed_patterns: vec![],
                blocked_patterns: vec![],
            },
        );

        let input = serde_json::json!({"command": "ls"});
        let check = manager.check_tool_use("bash", &input);
        assert!(matches!(check, PermissionCheck::Deny(_)));
    }

    // ── ExecutorRole::Peer tests ──────────────────────────────────────────────

    /// Null provider — fails on any actual call; used to construct TaskTool
    /// so the test can read the name the real tool implementation registers.
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

    #[test]
    fn test_peer_cannot_restart() {
        let mgr = PermissionManager::for_peer();
        // Exercise the name the real RestartTool registers, not a literal: a
        // deny arm keyed on a name no tool registers is unreachable in
        // production and would let a peer call fall through to AskUser.
        let name = crate::tools::Tool::name(&crate::tools::implementations::restart::RestartTool);
        let input = serde_json::json!({});
        let check = mgr.check_tool_use(name, &input);
        assert!(
            matches!(check, PermissionCheck::Deny(_)),
            "invariant: a peer must be hard-denied for '{name}', the tool name \
             RestartTool registers; got {check:?} (AskUser means the hard-deny \
             arm is unreachable for the real tool name)"
        );
    }

    #[test]
    fn test_peer_cannot_spawn() {
        let mgr = PermissionManager::for_peer();
        // Exercise the name the real TaskTool registers, not a literal.
        let task_tool =
            crate::tools::implementations::spawn::TaskTool::new(std::sync::Arc::new(NullProvider));
        let name = crate::tools::Tool::name(&task_tool);
        let input = serde_json::json!({});
        let check = mgr.check_tool_use(name, &input);
        assert!(
            matches!(check, PermissionCheck::Deny(_)),
            "invariant: a peer must be hard-denied for '{name}', the tool name \
             TaskTool registers; got {check:?} (AskUser means the hard-deny \
             arm is unreachable for the real tool name)"
        );
    }

    #[test]
    fn test_peer_hard_deny_table_names_are_declared_by_real_tool_implementations() {
        // Conformance against the same drift class as the original defect:
        // every name in PEER_HARD_DENY_TOOLS must be a name the real Tool
        // implementations register, and the table must name exactly the
        // restart/spawn tools — otherwise the deny arm has drifted onto an
        // unregistered literal (unreachable in production) or a registered
        // name has lost its deny.
        let task_tool =
            crate::tools::implementations::spawn::TaskTool::new(std::sync::Arc::new(NullProvider));
        let mut declared: Vec<String> = vec![
            crate::tools::Tool::name(&crate::tools::implementations::restart::RestartTool)
                .to_string(),
            crate::tools::Tool::name(&task_tool).to_string(),
        ];
        declared.sort();
        let mut table: Vec<String> = PEER_HARD_DENY_TOOLS
            .iter()
            .map(|n| (*n).to_string())
            .collect();
        table.sort();
        assert_eq!(
            table, declared,
            "PEER_HARD_DENY_TOOLS must name exactly the tool names the restart \
             and spawn Tool implementations register (declared = {declared:?}, \
             table = {table:?})"
        );
        // Cross-check the declared effects: a hard-denied tool must never
        // declare an autonomously-runnable effect, and the declarations must
        // reproduce exactly what the pre-#466 table computed for these
        // canonical names (restart_session → Destructive, spawn_task →
        // ExternalWrite), so the deny table and the effect declarations
        // cannot drift apart unnoticed.
        for (name, effect, expected) in [
            (
                crate::tools::Tool::name(&crate::tools::implementations::restart::RestartTool),
                crate::tools::Tool::effect(&crate::tools::implementations::restart::RestartTool),
                ExecutionEffect::Destructive,
            ),
            (
                crate::tools::Tool::name(&task_tool),
                crate::tools::Tool::effect(&task_tool),
                ExecutionEffect::ExternalWrite,
            ),
        ] {
            assert_eq!(
                effect, expected,
                "peer hard-deny tool '{name}' must declare {expected:?} — the \
                 classification the pre-refactor table computed for it"
            );
            assert!(
                matches!(
                    effect,
                    ExecutionEffect::Destructive | ExecutionEffect::ExternalWrite
                ),
                "peer hard-deny tool '{name}' declares {effect:?}; a hard-denied \
                 tool must declare Destructive or ExternalWrite so the deny and \
                 the declaration agree"
            );
            assert!(
                !effect.runs_autonomously(),
                "peer hard-deny tool '{name}' must not declare an autonomous effect"
            );
        }
    }

    #[test]
    fn test_peer_silent_allow_covers_every_vm_discovery_tool() {
        for tool in VM_DISCOVERY_TOOLS {
            assert!(
                PEER_SILENT_ALLOW_TOOLS.contains(tool),
                "{tool} is VM discovery and must stay silently allowed for peers; \
                 PEER_SILENT_ALLOW_TOOLS = {PEER_SILENT_ALLOW_TOOLS:?}"
            );
        }
    }

    fn isolated_workspace() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("isolated workspace");
        std::fs::create_dir(dir.path().join(".git")).expect("git root marker");
        let root = dir.path().canonicalize().expect("canonical workspace");
        (dir, root)
    }

    #[test]
    fn test_peer_read_glob_grep_silently_allowed() {
        let (workspace, root) = isolated_workspace();
        let inside = root.join("safe.txt");
        std::fs::write(&inside, "ok").expect("seed workspace file");
        let mgr = PermissionManager::for_peer().with_workspace_root(root.clone());
        let read = serde_json::json!({"file_path": inside.to_string_lossy()});
        let glob = serde_json::json!({"pattern": "*.txt"});
        let grep = serde_json::json!({"pattern": "ok", "path": inside.to_string_lossy()});
        assert!(
            matches!(mgr.check_tool_use("read", &read), PermissionCheck::Allow),
            "Peer should silently allow workspace read; got {:?}",
            mgr.check_tool_use("read", &read)
        );
        assert!(
            matches!(mgr.check_tool_use("glob", &glob), PermissionCheck::Allow),
            "Peer should silently allow relative glob; got {:?}",
            mgr.check_tool_use("glob", &glob)
        );
        assert!(
            matches!(mgr.check_tool_use("grep", &grep), PermissionCheck::Allow),
            "Peer should silently allow workspace grep; got {:?}",
            mgr.check_tool_use("grep", &grep)
        );
        let _keep = workspace;
    }

    #[test]
    fn test_peer_write_edit_patch_surfaces_as_ask() {
        let (workspace, root) = isolated_workspace();
        let inside = root.join("file.txt");
        let mgr = PermissionManager::for_peer().with_workspace_root(root);
        for tool in &["write", "edit", "patch"] {
            let input = serde_json::json!({"file_path": inside.to_string_lossy(), "content": "x"});
            assert!(
                matches!(
                    mgr.check_tool_use(tool, &input),
                    PermissionCheck::AskUser(_)
                ),
                "Peer {} must surface as AskUser (diff proposal), not auto-apply",
                tool
            );
        }
        let _keep = workspace;
    }

    #[test]
    fn test_peer_readonly_bash_silently_allowed() {
        let mgr = PermissionManager::for_peer();
        let readonly_cmds = ["ls -la", "cat README.md", "grep foo src/", "pwd", "whoami"];
        for cmd in &readonly_cmds {
            let input = serde_json::json!({"command": cmd});
            assert!(
                matches!(mgr.check_tool_use("bash", &input), PermissionCheck::Allow),
                "Peer should silently allow readonly bash: {}",
                cmd
            );
        }
    }

    #[test]
    fn test_peer_background_bash_surfaces_as_ask() {
        let mgr = PermissionManager::for_peer();
        // Even a read-only foreground command spawns a long-lived process in
        // the background, so the read-only refinement must not apply here: a
        // peer's background command always asks the owner.
        let input = serde_json::json!({"command": "ls -la"});
        assert!(
            matches!(
                mgr.check_tool_use("background_bash", &input),
                PermissionCheck::AskUser(_)
            ),
            "Peer background_bash must surface as AskUser, never silent-allow"
        );
    }

    #[test]
    fn test_background_bash_constitutional_deny_applies() {
        let input = serde_json::json!({"command": "echo ok; rm -rf /"});
        for role in [PermissionManager::new(), PermissionManager::for_peer()] {
            assert!(
                matches!(
                    role.check_tool_use("background_bash", &input),
                    PermissionCheck::Deny(_)
                ),
                "constitutional denied substrings must apply to \
                 background_bash for owner and peer alike"
            );
        }
    }

    #[test]
    fn test_peer_bash_with_side_effects_requires_ask() {
        let mgr = PermissionManager::for_peer();
        let side_effect_cmds = [
            "git commit -m 'x'",
            "cargo build",
            "touch file.txt",
            "mkdir foo",
        ];
        for cmd in &side_effect_cmds {
            let input = serde_json::json!({"command": cmd});
            assert!(
                matches!(
                    mgr.check_tool_use("bash", &input),
                    PermissionCheck::AskUser(_)
                ),
                "Peer bash with side effects must require AskUser: {}",
                cmd
            );
        }
    }

    #[test]
    fn test_peer_constitutional_constraints_still_apply() {
        let mgr = PermissionManager::for_peer();
        // Even a peer cannot run rm -rf
        let input = serde_json::json!({"command": "rm -rf /"});
        assert!(
            matches!(mgr.check_tool_use("bash", &input), PermissionCheck::Deny(_)),
            "Constitutional constraints must apply to peers too"
        );
    }

    // ── is_readonly_bash tests ────────────────────────────────────────────────

    #[test]
    fn test_is_readonly_bash_simple_ls() {
        assert!(is_readonly_bash("ls -la"), "ls -la is readonly");
        assert!(is_readonly_bash("ls"), "bare ls is readonly");
    }

    #[test]
    fn test_is_readonly_bash_cat_grep_wc() {
        assert!(is_readonly_bash("cat README.md"));
        assert!(is_readonly_bash("grep -r foo src/"));
        assert!(is_readonly_bash("wc -l file.txt"));
    }

    #[test]
    fn test_is_readonly_bash_rejects_write_commands() {
        assert!(!is_readonly_bash("rm file"), "rm is not readonly");
        assert!(
            !is_readonly_bash("git commit -m x"),
            "git commit is not readonly"
        );
        assert!(!is_readonly_bash("touch foo"), "touch is not readonly");
        assert!(!is_readonly_bash("mkdir bar"), "mkdir is not readonly");
    }

    #[test]
    fn test_is_readonly_bash_pipe_chain_is_rejected() {
        // Security: "ls; rm file" starts with "ls" but is destructive
        assert!(
            !is_readonly_bash("ls; rm file"),
            "semicolon-chained command must be rejected"
        );
        assert!(
            !is_readonly_bash("cat foo | tee out.txt"),
            "pipe to tee (writes file) must be rejected"
        );
    }

    #[test]
    fn test_is_readonly_bash_redirect_is_rejected() {
        assert!(
            !is_readonly_bash("echo hi > file.txt"),
            "stdout redirect must be rejected"
        );
        assert!(
            !is_readonly_bash("cat foo >> bar"),
            "append redirect must be rejected"
        );
    }

    #[test]
    fn test_is_readonly_bash_background_is_rejected() {
        assert!(
            !is_readonly_bash("ls &"),
            "background operator must be rejected"
        );
    }

    #[test]
    fn test_is_readonly_bash_leading_spaces_handled() {
        // Leading spaces after trim still resolve to the correct prefix
        assert!(
            is_readonly_bash("  ls -la"),
            "leading spaces should be trimmed"
        );
        assert!(
            is_readonly_bash("  cat file"),
            "leading spaces should be trimmed"
        );
    }

    #[test]
    fn test_allowed_patterns() {
        let mut manager = PermissionManager::new();
        manager.register_tool_config(
            "test".to_string(),
            ToolPermissionConfig {
                enabled: true,
                rule: PermissionRule::Allow,
                allowed_patterns: vec!["safe_pattern".to_string()],
                blocked_patterns: vec![],
            },
        );

        // Should allow matching pattern
        let input = serde_json::json!({"data": "safe_pattern"});
        let check = manager.check_tool_use("test", &input);
        assert!(matches!(check, PermissionCheck::Allow));

        // Should ask for non-matching pattern
        let input = serde_json::json!({"data": "other_pattern"});
        let check = manager.check_tool_use("test", &input);
        assert!(matches!(check, PermissionCheck::AskUser(_)));
    }

    #[test]
    fn test_declared_effects_auto_run_reads_but_not_writes() {
        use crate::tools::Tool;
        assert_eq!(
            crate::tools::ReadTool.effect(),
            ExecutionEffect::WorkspaceRead
        );
        assert!(crate::tools::ReadTool.effect().runs_autonomously());
        assert!(!crate::tools::WriteTool.effect().runs_autonomously());
        assert!(
            !ExecutionEffect::Unclassified.runs_autonomously(),
            "Unclassified must never run autonomously"
        );
    }

    #[test]
    fn test_declared_effect_is_unclassified_for_names_nothing_registers() {
        let registry = crate::tools::ToolRegistry::new();
        for name in ["push", "pop", "clear", "ExitPlanMode", "Bash"] {
            assert_eq!(
                registry.declared_effect(name),
                ExecutionEffect::Unclassified,
                "{name} is not a registered tool (`/clear` is a REPL slash \
                 command) and must not inherit any granted effect"
            );
            assert!(
                !registry.declared_effect(name).runs_autonomously(),
                "{name} must not run autonomously"
            );
        }
    }

    #[test]
    fn typed_program_declaration_ignores_untrusted_coarse_effect_input() {
        use crate::tools::{SubmitProgramTool, Tool};
        let tool = SubmitProgramTool::new(Arc::new(crate::runtime::ProgramRuntime::new()));
        assert_eq!(tool.effect(), ExecutionEffect::VmWrite);
        // The declaration is a property of the tool; a model-supplied coarse
        // label in the input payload is no longer consulted anywhere.
        for effect in ["pure", "destructive", "invented"] {
            let input = serde_json::json!({"effect": effect});
            assert_eq!(
                refined_effect_for_approval(tool.effect(), "submit_program", &input),
                ExecutionEffect::VmWrite,
                "input payload {effect} must not change the declared effect"
            );
        }
    }

    // ── refined_effect_for_approval (bash read-only refinement) ─────────────

    #[test]
    fn test_bash_readonly_refinement_applies_to_the_registered_bash_tool_name() {
        use crate::tools::{BashTool, Tool};
        // Pin the refinement's literal to the name the real tool registers so
        // a rename cannot strand the refinement (the #452/#466 drift class).
        assert_eq!(
            BashTool.name(),
            "bash",
            "refined_effect_for_approval refines the name BashTool registers; \
             if this fails, update the literal there together with this pin"
        );
        assert_eq!(
            refined_effect_for_approval(
                BashTool.effect(),
                BashTool.name(),
                &serde_json::json!({"command": "ls -la"})
            ),
            ExecutionEffect::WorkspaceRead,
            "read-only bash must refine the declared worst case to WorkspaceRead"
        );
        assert_eq!(
            refined_effect_for_approval(
                BashTool.effect(),
                BashTool.name(),
                &serde_json::json!({"command": "git commit -m x"})
            ),
            ExecutionEffect::ExternalWrite,
            "side-effecting bash keeps the declared worst case"
        );
    }

    #[test]
    fn test_bash_readonly_refinement_still_rejects_shell_operators() {
        use crate::tools::{BashTool, Tool};
        // Prefix-bypass invariant: operators disqualify the refinement.
        for cmd in ["ls; rm file", "cat foo | tee out.txt", "echo hi > file"] {
            assert_eq!(
                refined_effect_for_approval(
                    BashTool.effect(),
                    BashTool.name(),
                    &serde_json::json!({"command": cmd})
                ),
                ExecutionEffect::ExternalWrite,
                "'{cmd}' must not refine to WorkspaceRead"
            );
        }
    }

    #[test]
    fn test_approval_refinement_leaves_other_tools_untouched() {
        for (name, effect) in [
            ("read", ExecutionEffect::WorkspaceRead),
            ("write", ExecutionEffect::WorkspaceWrite),
            ("restart_session", ExecutionEffect::Destructive),
            ("anything_else", ExecutionEffect::Unclassified),
        ] {
            assert_eq!(
                refined_effect_for_approval(effect, name, &serde_json::json!({})),
                effect,
                "non-bash tools pass through unchanged"
            );
        }
        // A read-only-looking command refines nothing unless the declared
        // effect is bash's worst case.
        assert_eq!(
            refined_effect_for_approval(
                ExecutionEffect::WorkspaceWrite,
                "bash",
                &serde_json::json!({"command": "ls"})
            ),
            ExecutionEffect::WorkspaceWrite,
            "the refinement keys on the declared worst case, not the name alone"
        );
    }

    #[test]
    fn typed_program_always_enters_the_capability_broker_without_outer_approval() {
        let manager = PermissionManager::new();
        for effect in ["pure", "workspace_write", "destructive", "invented"] {
            assert!(matches!(
                manager.check_tool_use("submit_program", &serde_json::json!({"effect": effect})),
                PermissionCheck::Allow
            ));
        }
    }

    #[test]
    fn vm_discovery_tools_are_autonomous_vm_reads() {
        let memory_dir = tempfile::TempDir::new().expect("temp dir for memory catalog");
        let memory = Arc::new(
            crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
                db_path: memory_dir.path().join("memory.db"),
                use_neural_embeddings: false,
                ..crate::memory::MemoryConfig::default()
            })
            .expect("memory system for vm discovery classification"),
        );
        let runtime = Arc::new(crate::runtime::ProgramRuntime::new());

        let mut registry = crate::tools::ToolRegistry::new();
        for tool in [
            Box::new(crate::tools::GetVmStateTool::new(Arc::clone(&runtime)))
                as Box<dyn crate::tools::Tool>,
            Box::new(crate::tools::GetLanguageDefinitionTool),
            Box::new(crate::tools::SearchVmVocabularyTool::new(Arc::clone(
                &runtime,
            ))),
            Box::new(crate::tools::InspectVmWordTool::new(Arc::clone(&runtime))),
            Box::new(crate::tools::SearchWordTool::new(
                Arc::clone(&runtime),
                None,
            )),
            Box::new(crate::tools::InspectWordTool::new(
                Arc::clone(&runtime),
                None,
            )),
            Box::new(crate::tools::SearchVocabularyTool::new(Arc::clone(&memory))),
            Box::new(crate::tools::InspectProgramTool::new(memory)),
        ] {
            registry.register(tool);
        }

        for tool in VM_DISCOVERY_TOOLS {
            assert_eq!(
                registry.declared_effect(tool),
                ExecutionEffect::VmRead,
                "{tool} must not open a host-effect approval dialog"
            );
            assert!(
                registry.declared_effect(tool).runs_autonomously(),
                "{tool} is VM discovery and must run autonomously"
            );
        }
    }

    #[test]
    fn vm_discovery_tools_do_not_prompt_in_a_local_session() {
        let manager = PermissionManager::new();
        for tool in [
            "get_vm_state",
            "get_language_definition",
            "search_vm_vocabulary",
            "inspect_vm_word",
            "search_word",
            "inspect_word",
            "search_vocabulary",
            "inspect_program",
        ] {
            assert!(
                matches!(
                    manager.check_tool_use(tool, &serde_json::json!({})),
                    PermissionCheck::Allow
                ),
                "{tool} must be available for protocol discovery without approval"
            );
        }
    }

    #[test]
    fn vm_discovery_tools_do_not_prompt_for_a_peer() {
        let manager = PermissionManager::for_peer();
        for tool in ["search_word", "inspect_word", "inspect_program"] {
            assert!(
                matches!(
                    manager.check_tool_use(tool, &serde_json::json!({})),
                    PermissionCheck::Allow
                ),
                "{tool} must remain available for peer protocol discovery"
            );
        }
    }

    #[test]
    fn session_local_tools_skip_host_effect_confirmation_at_approval_boundary() {
        // Issue #426: spawn_tool_execution auto-approves when
        // refined_effect_for_approval(declared, name, input).runs_autonomously().
        // Unclassified never does, so the canonical names prompted. The
        // four-item todo_write payload is the reported failure.
        use crate::tools::{
            AskUserQuestionTool, PresentPlanTool, TodoReadTool, TodoWriteTool, ToolRegistry,
        };

        let todo_list = Arc::new(tokio::sync::RwLock::new(crate::tools::TodoList::default()));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(TodoWriteTool::new(Arc::clone(&todo_list))));
        registry.register(Box::new(TodoReadTool::new(todo_list)));
        registry.register(Box::new(PresentPlanTool));
        registry.register(Box::new(AskUserQuestionTool));
        registry.register(Box::new(crate::tools::WriteTool));
        registry.register_alias("TodoWrite", "todo_write");
        registry.register_alias("TodoRead", "todo_read");
        registry.register_alias("PresentPlan", "present_plan");
        registry.register_alias("AskUserQuestion", "ask_user_question");

        let four_item_list = serde_json::json!({"todos": [
            {"content": "Identify the harness implementation, website source, and deployment target",
             "id": "1", "priority": "high", "status": "in_progress"},
            {"content": "Implement the typed program runner in the harness",
             "id": "2", "priority": "high", "status": "pending"},
            {"content": "Build the website source from the harness output",
             "id": "3", "priority": "medium", "status": "pending"},
            {"content": "Verify the deployment target accepts the build",
             "id": "4", "priority": "low", "status": "completed"}
        ]});

        for (name, expected, input) in [
            ("todo_write", ExecutionEffect::VmWrite, &four_item_list),
            ("TodoWrite", ExecutionEffect::VmWrite, &four_item_list),
            ("todo_read", ExecutionEffect::VmRead, &serde_json::json!({})),
            (
                "present_plan",
                ExecutionEffect::VmWrite,
                &serde_json::json!({"plan": "do the work"}),
            ),
            (
                "ask_user_question",
                ExecutionEffect::VmWrite,
                &serde_json::json!({"questions": []}),
            ),
        ] {
            let declared = registry.declared_effect(name);
            let refined = refined_effect_for_approval(declared, name, input);
            assert_eq!(
                declared, expected,
                "invariant: '{name}' must declare {expected:?} at the approval \
                 boundary; Unclassified is AskUser in spawn_tool_execution \
                 (declared={declared:?}, input={input})"
            );
            assert_ne!(
                refined,
                ExecutionEffect::Unclassified,
                "invariant: '{name}' must not present Unclassified to \
                 refined_effect_for_approval; that is the AskUser the user hits \
                 (declared={declared:?}, refined={refined:?}, input={input})"
            );
            assert!(
                refined.runs_autonomously(),
                "invariant: '{name}' must skip host-effect confirmation; \
                 spawn_tool_execution emits ToolApprovalNeeded when the refined \
                 effect does not run autonomously (declared={declared:?}, \
                 refined={refined:?}, input={input})"
            );
        }

        let write_input = serde_json::json!({"file_path": "src/lib.rs", "content": "x"});
        let write_refined =
            refined_effect_for_approval(registry.declared_effect("write"), "write", &write_input);
        assert!(
            !write_refined.runs_autonomously(),
            "control: workspace write must still demand host-effect confirmation; \
             refined={write_refined:?}"
        );

        let owner = PermissionManager::new();
        let peer = PermissionManager::for_peer();
        for name in [
            "todo_write",
            "todo_read",
            "present_plan",
            "ask_user_question",
        ] {
            assert!(
                !matches!(
                    owner.check_tool_use(name, &serde_json::json!({})),
                    PermissionCheck::Deny(_)
                ),
                "invariant: PermissionManager must not Deny '{name}'; \
                 constitutional constraints still apply to bash/read/web_fetch \
                 only (got {:?})",
                owner.check_tool_use(name, &serde_json::json!({}))
            );
            assert!(
                !matches!(
                    peer.check_tool_use(name, &serde_json::json!({})),
                    PermissionCheck::Deny(_)
                ),
                "invariant: '{name}' is not a peer hard-deny tool; got {:?}",
                peer.check_tool_use(name, &serde_json::json!({}))
            );
        }

        let restart =
            crate::tools::Tool::name(&crate::tools::implementations::restart::RestartTool);
        assert!(
            matches!(
                peer.check_tool_use(restart, &serde_json::json!({})),
                PermissionCheck::Deny(_)
            ),
            "invariant: peer hard-deny still blocks '{restart}'"
        );
        assert!(
            matches!(
                owner.check_tool_use("bash", &serde_json::json!({"command": "rm -rf /"})),
                PermissionCheck::Deny(_)
            ),
            "invariant: constitutional constraints still deny dangerous bash"
        );
    }

    // ── Workspace containment (issue #429) ──────────────────────────────────

    #[test]
    fn test_workspace_root_is_git_root_when_present() {
        let (workspace, root) = isolated_workspace();
        let nested = root.join("src");
        std::fs::create_dir(&nested).expect("nested dir");
        let resolved = resolve_workspace_root(&nested);
        assert_eq!(
            resolved, root,
            "invariant: walking up from a nested dir must stop at the directory \
             that contains .git; resolved={resolved:?} root={root:?}"
        );
        let _keep = workspace;
    }

    #[test]
    fn test_workspace_root_falls_back_to_cwd_without_git() {
        let dir = tempfile::tempdir().expect("cwd without git");
        let cwd = dir.path().canonicalize().expect("canonical cwd");
        let resolved = resolve_workspace_root(&cwd);
        assert_eq!(
            resolved, cwd,
            "invariant: without .git the workspace root is cwd; \
             resolved={resolved:?} cwd={cwd:?}"
        );
    }

    #[test]
    fn test_dotdot_escape_is_ask_user_not_allow() {
        let (workspace, root) = isolated_workspace();
        let manager = PermissionManager::new().with_workspace_root(root.clone());
        let input = serde_json::json!({"file_path": "/etc/../etc/passwd"});
        let check = manager.check_tool_use("read", &input);
        assert!(
            matches!(check, PermissionCheck::AskUser(_)),
            "invariant: /etc/../etc/passwd must canonicalise to an escaped path \
             and AskUser; got {check:?} workspace={root:?}"
        );
        assert!(
            !invocation_runs_autonomously(ExecutionEffect::WorkspaceRead, "read", &input, &manager),
            "invariant: escaped read must not auto-approve via WorkspaceRead"
        );
        let _keep = workspace;
    }

    #[cfg(unix)]
    #[test]
    fn test_symlink_escape_is_ask_user_live_and_dangling() {
        let (workspace, root) = isolated_workspace();
        let manager = PermissionManager::new().with_workspace_root(root.clone());

        let live = root.join("link-out");
        std::os::unix::fs::symlink("/etc/passwd", &live).expect("live symlink");
        let live_input = serde_json::json!({"file_path": live.to_string_lossy()});
        let live_check = manager.check_tool_use("read", &live_input);
        assert!(
            matches!(live_check, PermissionCheck::AskUser(_)),
            "invariant: a workspace symlink to a path outside the root must \
             AskUser after canonicalisation; got {live_check:?} link={live:?}"
        );
        assert!(
            !invocation_runs_autonomously(
                ExecutionEffect::WorkspaceRead,
                "read",
                &live_input,
                &manager
            ),
            "invariant: live symlink escape must not auto-approve"
        );

        let dangling = root.join("dangling-out");
        std::os::unix::fs::symlink("/no/such/finch-escape-target", &dangling)
            .expect("dangling symlink");
        let dangling_input = serde_json::json!({"file_path": dangling.to_string_lossy()});
        let dangling_check = manager.check_tool_use("read", &dangling_input);
        assert!(
            matches!(dangling_check, PermissionCheck::AskUser(_)),
            "invariant: unresolvable symlink fail-closed as outside; \
             got {dangling_check:?} link={dangling:?}"
        );
        assert!(
            !invocation_runs_autonomously(
                ExecutionEffect::WorkspaceRead,
                "read",
                &dangling_input,
                &manager
            ),
            "invariant: dangling symlink must not auto-approve via ancestor fallback"
        );
        let _keep = workspace;
    }

    #[test]
    fn test_denylist_bypass_home_path_is_ask_user() {
        let (workspace, root) = isolated_workspace();
        let manager = PermissionManager::new().with_workspace_root(root);
        let home = dirs::home_dir().expect("home dir");
        let ssh = home.join(".ssh/id_rsa");
        let input = serde_json::json!({"file_path": ssh.to_string_lossy()});
        let check = manager.check_tool_use("read", &input);
        assert!(
            matches!(check, PermissionCheck::AskUser(_)),
            "invariant: ~/.ssh is outside the workspace and must AskUser even \
             though it is not on the old denylist; got {check:?} path={ssh:?}"
        );
        let _keep = workspace;
    }

    #[test]
    fn test_workspace_contained_read_still_autonomous() {
        let (workspace, root) = isolated_workspace();
        let inside = root.join("src.txt");
        std::fs::write(&inside, "ok").expect("seed");
        let manager = PermissionManager::new().with_workspace_root(root);
        let input = serde_json::json!({"file_path": inside.to_string_lossy()});
        assert!(
            matches!(
                manager.check_tool_use("read", &input),
                PermissionCheck::AskUser(_)
            ) || matches!(
                manager.check_tool_use("read", &input),
                PermissionCheck::Allow
            ),
            "contained read follows existing owner rules, not Deny; got {:?}",
            manager.check_tool_use("read", &input)
        );
        assert!(
            invocation_runs_autonomously(ExecutionEffect::WorkspaceRead, "read", &input, &manager),
            "invariant: a workspace-contained WorkspaceRead still runs autonomously; \
             path={inside:?}"
        );
        let _keep = workspace;
    }

    #[test]
    fn test_peer_escaped_read_asks_before_silent_allow() {
        let (workspace, root) = isolated_workspace();
        let mgr = PermissionManager::for_peer().with_workspace_root(root);
        let input = serde_json::json!({"file_path": "/etc/passwd"});
        let check = mgr.check_tool_use("read", &input);
        assert!(
            matches!(check, PermissionCheck::AskUser(_)),
            "invariant: containment applies before peer silent-allow; got {check:?}"
        );
        let _keep = workspace;
    }

    #[test]
    fn test_relative_glob_does_not_count_as_path_slot() {
        let (workspace, root) = isolated_workspace();
        let manager = PermissionManager::new().with_workspace_root(root);
        let input = serde_json::json!({"pattern": "**/*.rs"});
        assert!(
            !path_argument_escapes_workspace(
                "glob",
                &input,
                manager.workspace_root(),
                manager.cwd()
            ),
            "invariant: relative globs stay workspace-relative and have no path slot"
        );
        let _keep = workspace;
    }

    #[test]
    fn test_absolute_glob_escape_is_ask_user() {
        let (workspace, root) = isolated_workspace();
        let manager = PermissionManager::new().with_workspace_root(root);
        let input = serde_json::json!({"pattern": "/etc/*.conf"});
        let check = manager.check_tool_use("glob", &input);
        assert!(
            matches!(check, PermissionCheck::AskUser(_)),
            "invariant: absolute glob prefix /etc is an escaped path slot; got {check:?}"
        );
        let _keep = workspace;
    }

    #[test]
    fn test_nonexistent_inside_path_stays_contained() {
        let (workspace, root) = isolated_workspace();
        let manager = PermissionManager::new().with_workspace_root(root.clone());
        let missing = root.join("does-not-exist.txt");
        assert!(
            !raw_path_escapes_workspace(
                &missing.to_string_lossy(),
                manager.workspace_root(),
                manager.cwd()
            ),
            "invariant: a missing path under the workspace is still contained; \
             path={missing:?} root={root:?}"
        );
        let _keep = workspace;
    }
}

// Permission system for tool execution
//
// Implements constitutional constraints: "Would 1000 users do this?"
// Multi-layer defense: Allow, Ask, or Deny tool execution

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::{debug, warn};

use crate::programs::ExecutionEffect;

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
}

impl PermissionManager {
    /// Create new permission manager with default settings (Owner role).
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
            default_rule: PermissionRule::Ask,
            max_tool_turns: 25,
            role: ExecutorRole::Owner,
        }
    }

    /// Create a permission manager for an AI peer (asymmetric rules).
    pub fn for_peer() -> Self {
        Self {
            configs: HashMap::new(),
            default_rule: PermissionRule::Ask,
            max_tool_turns: 25,
            role: ExecutorRole::Peer,
        }
    }

    /// Load from configuration
    pub fn from_config(configs: HashMap<String, ToolPermissionConfig>) -> Self {
        Self {
            configs,
            default_rule: PermissionRule::Ask,
            max_tool_turns: 25,
            role: ExecutorRole::Owner,
        }
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

        // These tools inspect Finch's own typed runtime metadata only. They
        // neither access the workspace nor cross a host-effect boundary, so a
        // provider must be able to use them to discover the VM protocol
        // without interrupting the user for an approval dialog.
        if matches!(
            tool_name,
            "get_vm_state"
                | "get_language_definition"
                | "search_vm_vocabulary"
                | "inspect_vm_word"
                | "search_vocabulary"
                | "inspect_program"
        ) {
            return PermissionCheck::Allow;
        }

        // Pure and VM-local programs do not cross a host-effect boundary. External
        // program effects continue through the configured/default approval rule.
        if tool_name == "submit_program"
            && matches!(
                input.get("effect").and_then(Value::as_str),
                Some("pure" | "vm_read" | "vm_write")
            )
        {
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
        // Hard deny: peer cannot restart/recompile/kill the session
        if matches!(tool_name, "restart" | "spawn") {
            return PermissionCheck::Deny("Peer cannot restart or spawn processes".to_string());
        }

        // Constitutional constraints still apply to everyone
        if let Some(reason) = self.check_constitutional_constraints(tool_name, input) {
            return PermissionCheck::Deny(reason);
        }

        match tool_name {
            // Silent allow: read-only examination and scheduler-local control.
            // Agent tools enforce task-tree ownership themselves.
            "read"
            | "glob"
            | "grep"
            | "get_vm_state"
            | "get_language_definition"
            | "search_vm_vocabulary"
            | "inspect_vm_word"
            | "search_vocabulary"
            | "spawn_agent"
            | "await_agent"
            | "poll_agent"
            | "cancel_agent" => PermissionCheck::Allow,

            // Pure and VM-local programs cannot cross a host-effect boundary.
            "submit_program"
                if matches!(
                    input.get("effect").and_then(Value::as_str),
                    Some("pure" | "vm_read" | "vm_write")
                ) =>
            {
                PermissionCheck::Allow
            }

            // Write/edit/patch: must surface as diff proposal in the room
            // The caller (peer loop) is responsible for converting Allow here into
            // a DiffPropose SessionEvent rather than applying directly.
            "write" | "edit" | "patch" => {
                PermissionCheck::AskUser("Peer proposes a file change — review diff".to_string())
            }

            // Bash: allow read-only commands silently, ask for everything else
            "bash" => {
                let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
                if is_readonly_bash(command) {
                    PermissionCheck::Allow
                } else {
                    PermissionCheck::AskUser(
                        "Peer wants to run a shell command — approve?".to_string(),
                    )
                }
            }

            // Everything else: ask
            _ => PermissionCheck::AskUser(format!("Peer wants to use '{}' — approve?", tool_name)),
        }
    }

    /// Apply constitutional constraints (safety checks)
    fn check_constitutional_constraints(&self, tool_name: &str, input: &Value) -> Option<String> {
        match tool_name {
            "bash" => self.check_bash_safety(input),
            "read" => self.check_read_safety(input),
            "web_fetch" => self.check_web_fetch_safety(input),
            _ => None,
        }
    }

    /// Check if bash command is safe
    fn check_bash_safety(&self, input: &Value) -> Option<String> {
        let command = input.get("command")?.as_str()?;

        // Blocked patterns (always deny)
        let dangerous_patterns = vec![
            ("rm -rf", "Recursive deletion is dangerous"),
            ("dd if=", "Disk operations are dangerous"),
            (":(){ :|:& };:", "Fork bombs are blocked"),
            ("sudo", "Privilege escalation requires manual execution"),
            ("chmod 777", "Unsafe permission changes are blocked"),
            ("> /dev/", "Direct device access is dangerous"),
            ("mkfs", "Filesystem operations are dangerous"),
            ("fdisk", "Disk partitioning is dangerous"),
        ];

        for (pattern, reason) in dangerous_patterns {
            if command.contains(pattern) {
                warn!("Blocked dangerous bash command: {}", command);
                return Some(format!("Blocked: {}", reason));
            }
        }

        None
    }

    /// Check if file read is safe
    fn check_read_safety(&self, input: &Value) -> Option<String> {
        let file_path = input.get("file_path")?.as_str()?;

        // Block system files
        let system_paths = vec![
            "/etc/passwd",
            "/etc/shadow",
            "/etc/sudoers",
            "/dev/",
            "/proc/",
            "/sys/",
        ];

        for blocked_path in system_paths {
            if file_path.starts_with(blocked_path) {
                warn!("Blocked system file access: {}", file_path);
                return Some(format!(
                    "Blocked: Access to system files ({}) is not allowed",
                    blocked_path
                ));
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

/// Effect declaration for legacy tool adapters.
///
/// New VM programs carry this in their language-level signature. This table is
/// the compatibility boundary while provider-native tools are still exposed.
pub fn legacy_tool_effect(tool_name: &str, input: &Value) -> ExecutionEffect {
    match tool_name.to_ascii_lowercase().as_str() {
        // Transitional adapter: ordinary provider turns still invoke the
        // typed VM through a tool call. Preserve the program's declared upper
        // bound so pure/VM-local work is not treated as an unclassified shell
        // operation by the legacy approval gate.
        "submit_program" => input
            .get("effect")
            .and_then(Value::as_str)
            .and_then(|effect| effect.parse().ok())
            .unwrap_or(ExecutionEffect::Unclassified),
        "get_vm_state"
        | "get_language_definition"
        | "search_vm_vocabulary"
        | "inspect_vm_word"
        | "search_vocabulary"
        | "inspect_program"
        | "search_memory"
        | "list_recent_memories"
        | "todoread" => ExecutionEffect::VmRead,
        "todowrite" | "push" | "pop" | "clear" | "enterplanmode" | "presentplan"
        | "askuserquestion" | "create_memory" => ExecutionEffect::VmWrite,
        "read" | "glob" | "grep" | "hash_compare" | "excel_read" | "excel_range"
        | "excel_sheets" | "gui_inspect" => ExecutionEffect::WorkspaceRead,
        "web_fetch" => ExecutionEffect::ExternalRead,
        "write" | "edit" | "patch" | "excel_write" | "excel_formula" => {
            ExecutionEffect::WorkspaceWrite
        }
        "bash" => {
            let command = input.get("command").and_then(Value::as_str).unwrap_or("");
            if is_readonly_bash(command) {
                ExecutionEffect::WorkspaceRead
            } else {
                ExecutionEffect::ExternalWrite
            }
        }
        "restart_session" => ExecutionEffect::Destructive,
        "run" | "save_and_exec" | "spawn_task" | "ansible" | "gui_click" | "gui_type"
        | "excel_activate" => ExecutionEffect::ExternalWrite,
        _ => ExecutionEffect::Unclassified,
    }
}

impl Default for PermissionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let manager = PermissionManager::new();

        let system_files = vec!["/etc/passwd", "/etc/shadow", "/dev/null"];

        for file in system_files {
            let input = serde_json::json!({"file_path": file});
            let check = manager.check_tool_use("read", &input);
            assert!(
                matches!(check, PermissionCheck::Deny(_)),
                "Failed to block: {}",
                file
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

    #[test]
    fn test_peer_cannot_restart() {
        let mgr = PermissionManager::for_peer();
        let input = serde_json::json!({});
        assert!(
            matches!(
                mgr.check_tool_use("restart", &input),
                PermissionCheck::Deny(_)
            ),
            "Peer must not be allowed to restart"
        );
    }

    #[test]
    fn test_peer_cannot_spawn() {
        let mgr = PermissionManager::for_peer();
        let input = serde_json::json!({});
        assert!(
            matches!(
                mgr.check_tool_use("spawn", &input),
                PermissionCheck::Deny(_)
            ),
            "Peer must not be allowed to spawn processes"
        );
    }

    #[test]
    fn test_peer_read_glob_grep_silently_allowed() {
        let mgr = PermissionManager::for_peer();
        for tool in &["read", "glob", "grep"] {
            let input = serde_json::json!({"file_path": "/tmp/safe.txt"});
            assert!(
                matches!(mgr.check_tool_use(tool, &input), PermissionCheck::Allow),
                "Peer should silently allow {}",
                tool
            );
        }
    }

    #[test]
    fn test_peer_write_edit_patch_surfaces_as_ask() {
        let mgr = PermissionManager::for_peer();
        for tool in &["write", "edit", "patch"] {
            let input = serde_json::json!({"file_path": "/tmp/file.txt", "content": "x"});
            assert!(
                matches!(
                    mgr.check_tool_use(tool, &input),
                    PermissionCheck::AskUser(_)
                ),
                "Peer {} must surface as AskUser (diff proposal), not auto-apply",
                tool
            );
        }
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
    fn test_legacy_effects_auto_run_reads_but_not_writes() {
        assert_eq!(
            legacy_tool_effect("read", &serde_json::json!({"path": "src/lib.rs"})),
            ExecutionEffect::WorkspaceRead
        );
        assert!(legacy_tool_effect("read", &serde_json::json!({})).runs_autonomously());
        assert!(!legacy_tool_effect("write", &serde_json::json!({})).runs_autonomously());
        assert!(!legacy_tool_effect("unknown", &serde_json::json!({})).runs_autonomously());
    }

    #[test]
    fn typed_program_submission_preserves_its_declared_effect() {
        assert_eq!(
            legacy_tool_effect("submit_program", &serde_json::json!({"effect": "pure"})),
            ExecutionEffect::Pure
        );
        assert_eq!(
            legacy_tool_effect(
                "submit_program",
                &serde_json::json!({"effect": "workspace_read"})
            ),
            ExecutionEffect::WorkspaceRead
        );
    }

    #[test]
    fn vm_discovery_tools_are_autonomous_vm_reads() {
        for tool in [
            "get_vm_state",
            "get_language_definition",
            "search_vm_vocabulary",
            "inspect_vm_word",
            "search_vocabulary",
            "inspect_program",
        ] {
            assert_eq!(
                legacy_tool_effect(tool, &serde_json::json!({})),
                ExecutionEffect::VmRead,
                "{tool} must not open a host-effect approval dialog"
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
}

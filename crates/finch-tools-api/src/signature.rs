//! Signature for one tool invocation, the key approval decisions cache on.
//!
//! Moved verbatim from `src/tools/executor.rs`; the executor and the
//! persisted approval patterns share this exact type.

/// Signature for a tool execution, used for caching approval decisions
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolSignature {
    pub tool_name: String,
    pub context_key: String,

    // Structured components for flexible pattern matching
    /// Command being executed (for bash)
    pub command: Option<String>,
    /// Arguments passed to the command
    pub args: Option<String>,
    /// Working directory for the execution
    pub directory: Option<String>,
    /// Discrete path argument when the tool has one (`file_path`, grep `path`,
    /// escapable glob prefix). `None` for bash and other non-path tools.
    pub path: Option<String>,
    /// Whether [`Self::path`] resolved inside the workspace root. `true` when
    /// there is no path slot (not an escape).
    pub path_in_workspace: bool,
    /// True when the one-shot path would constitutionally Deny this command.
    /// Patterns must not match; [`ToolExecutor::is_approved`] returns
    /// [`ApprovalSource::NotApproved`].
    pub constitutionally_denied: bool,
}

impl ToolSignature {
    /// Reconstruct the bash command string from structured parts.
    pub fn full_command(&self) -> Option<String> {
        match (&self.command, &self.args) {
            (Some(cmd), Some(args)) if !args.is_empty() => Some(format!("{cmd} {args}")),
            (Some(cmd), _) => Some(cmd.clone()),
            _ => None,
        }
    }
}

impl Default for ToolSignature {
    fn default() -> Self {
        Self {
            tool_name: String::new(),
            context_key: String::new(),
            command: None,
            args: None,
            directory: None,
            path: None,
            path_in_workspace: true,
            constitutionally_denied: false,
        }
    }
}

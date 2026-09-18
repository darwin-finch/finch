//! Permission policy for tool execution — re-export seam.
//!
//! The permission types, the peer/constitutional tables, and the approval
//! refinements are defined in the dependency-free `finch-tools-api` crate
//! (see `crates/finch-tools-api`). This module keeps the crate-internal
//! `crate::tools::permissions::*` paths working and hosts the tests that
//! exercise the real registered tool implementations, at their original
//! `tools::permissions::tests::*` module path.

pub use finch_tools_api::{
    bash_command_is_constitutionally_denied, invocation_runs_autonomously, path_argument_for_tool,
    raw_path_escapes_workspace, refined_effect_for_approval, resolve_workspace_root,
    PermissionCheck, PermissionManager, PermissionRule, ToolPermissionConfig,
    PEER_HARD_DENY_TOOLS, PEER_REVIEWED_CHANGESET_TOOLS, PEER_SILENT_ALLOW_TOOLS,
    VM_DISCOVERY_TOOLS,
};

#[cfg(test)]
mod tests;

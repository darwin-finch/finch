//! Core tool surface types — re-export seam.
//!
//! The tool API types are defined in the dependency-free `finch-tools-api`
//! crate (see `crates/finch-tools-api`); this module keeps the
//! crate-internal `crate::tools::types::*` paths working. The provider wire
//! types are re-exported from `finch-providers`.

pub use finch_tools_api::{
    ContentBlock, EffectAuditAuthority, HostModeState, LiveOutput, LiveOutputSink, ToolContext,
    ToolDefinition, ToolInputSchema, ToolResult, ToolUse,
};

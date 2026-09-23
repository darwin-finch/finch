//! Dependency-free tool API for Finch.
//!
//! This crate owns the tool surface that every application layer shares:
//! the [`Tool`] trait and [`ToolRegistry`], the typed requests and results
//! (`ToolUse`, `ToolResult`, `ContentBlock`), the permission and approval
//! policy (`permissions`, `patterns`), the declared-effect vocabulary
//! (`ExecutionEffect`), the per-call [`ToolContext`] with its injected
//! application ports ([`HostModeState`], [`EffectAuditAuthority`]), and the
//! event-loop-owned tool-round protocol (`tool_loop`).
//!
//! It deliberately depends on no root-crate subsystem — no `cli`, `server`,
//! `runtime`, `local`, or `models`. Only the extracted leaf crates
//! `finch-providers` (provider wire types) and `finch-vm` (VM effect
//! vocabulary) are workspace dependencies. Concrete tool implementations
//! and the executor stay with the composition root (`src/tools`); this
//! crate names what they implement and consume.
//!
//! Child modules are private; the `pub use` list is the whole public
//! surface.

mod effects;
mod pattern_matcher;
mod patterns;
mod permissions;
mod registry;
mod semantic;
mod signature;
mod tool_loop;
mod types;
mod vm_effect;

pub use effects::ExecutionEffect;
pub use finch_providers::{ToolDefinition, ToolInputSchema, ToolUse};
pub use pattern_matcher::ToolPatternMatcher;
pub use patterns::{
    ExactApproval, MatchType, PathSlot, PatternType, PersistentPatternStore, ToolPattern,
};
pub use permissions::{
    bash_command_is_constitutionally_denied, invocation_runs_autonomously, path_argument_for_tool,
    raw_path_escapes_workspace, refined_effect_for_approval, resolve_workspace_root,
    PermissionCheck, PermissionManager, PermissionRule, ToolPermissionConfig, PEER_HARD_DENY_TOOLS,
    PEER_REVIEWED_CHANGESET_TOOLS, PEER_SILENT_ALLOW_TOOLS, VM_DISCOVERY_TOOLS,
};
pub use registry::{Tool, ToolRegistry};
pub use semantic::{
    compile_policy_from_registry, semantic_tools_for_advertisement, tool_authority_from_effect,
};
pub use signature::ToolSignature;
pub use tool_loop::{
    AdmitError, ObserveOutcome, PreparedCall, RejectReason, RejectedCall, ToolCatalog, ToolLoop,
    ToolLoopIdentity, ToolLoopResult, ToolLoopTerminal, ValidatedCall,
};
pub use types::{
    ContentBlock, EffectAuditAuthority, HostModeState, LiveOutput, LiveOutputSink, ToolContext,
    ToolResult,
};
pub use vm_effect::{VmEffectEnvelope, VmEffectHandle};

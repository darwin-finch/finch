//! Shared typed-machine contracts for Finch's language frontends and runtime.
//!
//! This unpublished crate owns the common IR, verifier, capability and type
//! vocabulary. Applications should depend on the `finch-vm` compatibility
//! facade rather than importing this crate directly.

mod capability;
mod construction;
mod diagnostic;
mod effects;
mod ir;
mod language;
mod signature;
mod surface_types;
mod types;
mod verifier;
mod vocabulary;

pub use capability::{
    ApprovalChoice, ApprovalPrompt, AuthorizationContext, AuthorizationDecision,
    CapabilityAuditAction, CapabilityAuditEntry, CapabilityAuthorizationAuditEntry,
    CapabilityAvailability, CapabilityGrant, CapabilityLedger, CapabilityPolicy, CapabilityRequest,
    GrantScope, GrantSet,
};
pub use construction::{
    certify_module, BoolBranch, Elaborated, FunctionCertified, LoopBinding, ModuleSealed,
    ModuleVerified, Parsed, SemanticBinding, SemanticBuilder, SEMANTIC_CONSTRUCTION_VERSION,
};
pub use diagnostic::{
    nearest_names, DiagnosticPhase, Severity, SourceLanguage, SourceOrigin, SourceSpan,
    VmDiagnostic,
};
pub use effects::{
    CapabilityKind, CapabilityRequirement, EffectSet, FileOperation, FileSelector,
    FileSelectorTemplate, FileSelectorTemplatePart, McpSelectorTemplate, NetworkSelectorTemplate,
    ProcessSelectorTemplate, ProgramSelectorTemplate, ResourceRoot, ResourceSelector,
    SelectorError, UiOperation,
};
pub use ir::{BasicBlock, BlockId, Function, Instruction, LocatedInstruction, Module};
pub use language::ProgramLanguage;
pub use signature::{ControlEffect, StackRow, StackSignature, SuspensionSignature};
pub use surface_types::parse_type_name;
pub use types::{TaskKind, Type, TypedValue};
pub use verifier::{
    apply_signature_types, instantiate_signature_types, VerifiedFunction, VerifiedModule, Verifier,
    Vocabulary,
};
pub use vocabulary::{
    agent_task_result_type, agent_task_snapshot_type, agent_task_spec_type,
    capability_grant_entry_type, core_vocabulary, core_word_documentation, core_word_registry,
    core_word_spec, tree_entry_type, tree_listing_type, CoreHostBinding, CoreWordDocumentation,
    CoreWordImplementation, CoreWordSpec,
};

/// Version of the typed VM contract and serialized IR family.
///
/// Version 5 adds first-class `fiber<Y,R>` values, scheduler instructions,
/// and serializable producer continuation records. Old modules/checkpoints
/// must be rejected rather than interpreting opaque handles without owners.
pub const VM_TYPE_SYSTEM_VERSION: u32 = 5;

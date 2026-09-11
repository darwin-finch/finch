//! Finch's typed, provider-neutral virtual-machine contracts.
//!
//! Co-Forth and Finch Lisp are source languages. Both compile to the typed IR
//! defined here and are checked by the same verifier before execution.

mod diagnostic;
mod effects;
mod fiber;
mod frontend;
mod interpreter;
mod ir;
mod language;
#[cfg(test)]
mod migration;
mod runtime;
mod signature;
mod types;
mod verifier;
mod vocabulary;

pub use capability::{
    ApprovalChoice, ApprovalPrompt, AuthorizationContext, AuthorizationDecision,
    CapabilityAuditAction, CapabilityAuditEntry, CapabilityAuthorizationAuditEntry,
    CapabilityAvailability, CapabilityGrant, CapabilityLedger, CapabilityPolicy, CapabilityRequest,
    GrantScope, GrantSet,
};
pub use diagnostic::{
    DiagnosticPhase, Severity, SourceLanguage, SourceOrigin, SourceSpan, VmDiagnostic,
};
pub use effects::{
    CapabilityKind, CapabilityRequirement, EffectSet, FileOperation, FileSelector,
    FileSelectorTemplate, FileSelectorTemplatePart, McpSelectorTemplate, NetworkSelectorTemplate,
    ProcessSelectorTemplate, ProgramSelectorTemplate, ResourceRoot, ResourceSelector,
    SelectorError,
};
pub use frontend::forth::{compile_forth, compile_forth_with_functions};
pub use frontend::lisp::{compile_lisp, compile_lisp_with_functions};
pub(crate) use interpreter::instantiate_requirement;
pub use interpreter::{
    CapabilityHandler, HostSideEffect, UiOperation, UiProgress, VmContinuation, VmFrame,
    VmSideEffect, VmStep, VmTrampoline,
};
pub use ir::{BasicBlock, Function, Instruction, LocatedInstruction, Module};
pub use language::ProgramLanguage;
pub use runtime::{
    EffectJournalEntry, EffectJournalState, PendingHostCall, ProducerFiberRecord,
    ProducerFiberState, TypedExecution, TypedExecutionStatus, TypedRuntime, TypedRuntimeCheckpoint,
    TypedSuspension,
};
pub use signature::{ControlEffect, StackRow, StackSignature, SuspensionSignature};
pub use types::{TaskKind, Type, TypedValue};
pub use verifier::{VerifiedFunction, VerifiedModule, Verifier, Vocabulary};
pub use vocabulary::{
    agent_task_result_type, agent_task_snapshot_type, agent_task_spec_type,
    capability_grant_entry_type, core_vocabulary, core_word_documentation, core_word_registry,
    core_word_spec, tree_entry_type, tree_listing_type, CoreHostBinding, CoreWordDocumentation,
    CoreWordImplementation,
};

/// Version of the typed VM contract and serialized IR family.
///
/// Version 5 adds first-class `fiber<Y,R>` values, scheduler instructions,
/// and serializable producer continuation records. Old modules/checkpoints
/// must be rejected rather than interpreting opaque handles without owners.
pub const VM_TYPE_SYSTEM_VERSION: u32 = 5;
mod capability;

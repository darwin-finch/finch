//! Finch's typed, provider-neutral virtual-machine contracts.
//!
//! Co-Forth and Finch Lisp are source languages. Both compile to the typed IR
//! defined here and are checked by the same verifier before execution.

pub mod diagnostic;
pub mod effects;
pub mod frontend;
pub mod interpreter;
pub mod ir;
pub mod migration;
pub mod runtime;
pub mod signature;
pub mod types;
pub mod verifier;
pub mod vocabulary;

pub use capability::{
    ApprovalChoice, ApprovalPrompt, AuthorizationContext, AuthorizationDecision,
    CapabilityAuditAction, CapabilityAuditEntry, CapabilityAuthorizationAuditEntry,
    CapabilityAvailability, CapabilityGrant, CapabilityLedger, CapabilityPolicy, CapabilityRequest,
    GrantScope, GrantSet,
};
pub use diagnostic::{
    render_vm_diagnostics, DiagnosticPhase, DiagnosticSource, Severity, SourceLanguage,
    SourceOrigin, SourceSpan, VmDiagnostic,
};
pub use effects::{
    CapabilityKind, CapabilityRequirement, EffectSet, FileOperation, FileSelector,
    FileSelectorTemplate, FileSelectorTemplatePart, McpSelectorTemplate, ResourceRoot,
    ResourceSelector, SelectorError,
};
pub use interpreter::{
    HostSideEffect, UiOperation, UiProgress, VmContinuation, VmFrame, VmSideEffect, VmStep,
    VmTrampoline,
};
pub use runtime::{
    EffectJournalEntry, EffectJournalState, PendingHostCall, ProducerFiberRecord,
    ProducerFiberState, TypedExecution, TypedExecutionStatus, TypedRuntime, TypedRuntimeCheckpoint,
    TypedSuspension,
};
pub use signature::{ControlEffect, StackRow, StackSignature, SuspensionSignature};
pub use types::{TaskKind, Type, TypedValue};
pub use verifier::{VerifiedFunction, VerifiedModule, Verifier, Vocabulary};
pub use vocabulary::core_vocabulary;

/// Version of the typed VM contract and serialized IR family.
///
/// Version 5 adds first-class `fiber<Y,R>` values, scheduler instructions,
/// and serializable producer continuation records. Old modules/checkpoints
/// must be rejected rather than interpreting opaque handles without owners.
pub const VM_TYPE_SYSTEM_VERSION: u32 = 5;
pub mod capability;

//! Finch's typed, provider-neutral virtual-machine contracts.
//!
//! Co-Forth and Finch Lisp are source languages. Both compile to the typed IR
//! defined here and are checked by the same verifier before execution.

mod fiber;
mod frontend;
mod interpreter;
mod lisp;
#[cfg(test)]
mod migration;
mod runtime;

pub use finch_vm_core::{
    agent_task_result_type, agent_task_snapshot_type, agent_task_spec_type,
    capability_grant_entry_type, core_vocabulary, core_word_documentation, core_word_registry,
    core_word_spec, tree_entry_type, tree_listing_type, ApprovalChoice, ApprovalPrompt,
    AuthorizationContext, AuthorizationDecision, BasicBlock, CapabilityAuditAction,
    CapabilityAuditEntry, CapabilityAuthorizationAuditEntry, CapabilityAvailability,
    CapabilityGrant, CapabilityKind, CapabilityLedger, CapabilityPolicy, CapabilityRequest,
    CapabilityRequirement, ControlEffect, CoreHostBinding, CoreWordDocumentation,
    CoreWordImplementation, CoreWordSpec, DiagnosticPhase, EffectSet, FileOperation, FileSelector,
    FileSelectorTemplate, FileSelectorTemplatePart, Function, GrantScope, GrantSet, Instruction,
    LocatedInstruction, McpSelectorTemplate, Module, NetworkSelectorTemplate,
    ProcessSelectorTemplate, ProgramLanguage, ProgramSelectorTemplate, ResourceRoot,
    ResourceSelector, SelectorError, Severity, SourceLanguage, SourceOrigin, SourceSpan, StackRow,
    StackSignature, SuspensionSignature, TaskKind, Type, TypedValue, UiOperation, VerifiedFunction,
    VerifiedModule, Verifier, VmDiagnostic, Vocabulary, VM_TYPE_SYSTEM_VERSION,
};
pub use frontend::forth::{compile_forth, compile_forth_with_functions};
pub use frontend::lisp::{compile_lisp, compile_lisp_with_functions};
pub use interpreter::instantiate_requirement;
pub use interpreter::{
    CapabilityHandler, HostSideEffect, InterpreterConfig, UiProgress, VmContinuation, VmFrame,
    VmSideEffect, VmStep, VmTrampoline,
};
pub use lisp::{parse_math, parse_str, parse_str_spanned, SpannedVal, Val};
pub use runtime::{
    EffectJournalEntry, EffectJournalState, PendingHostCall, ProducerFiberRecord,
    ProducerFiberState, TypedExecution, TypedExecutionStatus, TypedRuntime, TypedRuntimeCheckpoint,
    TypedSuspension,
};

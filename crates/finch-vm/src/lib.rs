//! Finch's typed, provider-neutral virtual-machine contracts.
//!
//! Execution consumes [`ModuleVerified`] certificates. Source compilation lives
//! in `finch-language`; this crate owns the reference interpreter and runtime.

mod fiber;
#[cfg(test)]
mod frontend_tests;
mod interpreter;
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
    LocatedInstruction, McpSelectorTemplate, Module, ModuleVerified, NetworkSelectorTemplate,
    ProcessSelectorTemplate, ProgramLanguage, ProgramSelectorTemplate, ResourceRoot,
    ResourceSelector, SelectorError, Severity, SourceLanguage, SourceOrigin, SourceSpan, StackRow,
    StackSignature, SuspensionSignature, TaskKind, Type, TypedValue, UiOperation, VerifiedFunction,
    VerifiedModule, Verifier, VmDiagnostic, Vocabulary, SEMANTIC_CONSTRUCTION_VERSION,
    VM_TYPE_SYSTEM_VERSION,
};

/// Version of the portable Runtime/Application effect, resume, and delivery ABI.
///
/// Independent of [`VM_TYPE_SYSTEM_VERSION`], which versions serialized IR and
/// typed-runtime checkpoints. Bump this when `ProgramRun`, diagnostics,
/// `VmSideEffect`, `VmResume`, output rows, or effect handles change on the
/// wire. Existing JSON goldens for those records must stay byte-identical
/// across a version that claims compatibility.
pub const RUNTIME_APPLICATION_ABI_VERSION: u32 = 1;
pub use interpreter::instantiate_requirement;
pub use interpreter::{
    CapabilityHandler, HostSideEffect, InterpreterConfig, UiProgress, VmContinuation, VmFrame,
    VmSideEffect, VmStep, VmTrampoline,
};
pub use runtime::{
    EffectJournalEntry, EffectJournalState, PendingHostCall, ProducerFiberRecord,
    ProducerFiberState, TypedExecution, TypedExecutionStatus, TypedRuntime, TypedRuntimeCheckpoint,
    TypedSuspension,
};

/// Why a provider's first Finch VM wire submission was not accepted.
///
/// Keep this deliberately coarse and source-free: conformance reporting needs
/// provider/model aggregates, not a second log of user prompts or generated
/// programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireFailureClass {
    RawProse,
    MarkdownFence,
    InventedWord,
    StackOrType,
    WrongLanguageDispatch,
    MissingOutputEffect,
    Capability,
    Other,
}

/// Return the stable leading diagnostic code without retaining diagnostic prose.
pub fn wire_diagnostic_code(diagnostic: &str) -> Option<String> {
    let head = diagnostic.lines().next()?;
    let code = match head
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
    {
        Some((code, _)) => code.trim(),
        None => head.split_once(':')?.0.trim(),
    };
    code.starts_with("E-").then(|| code.to_string())
}

/// Classify a rejected provider submission for aggregate conformance metrics.
///
/// This pure compiler-boundary projection intentionally retains neither source
/// nor diagnostic text.
pub fn classify_wire_failure(source: &str, diagnostic: &str) -> WireFailureClass {
    let trimmed = source.trim_start();
    let code = wire_diagnostic_code(diagnostic).unwrap_or_default();
    if trimmed.starts_with("```") || code == "E-WIRE-002" {
        return WireFailureClass::MarkdownFence;
    }
    if (trimmed.starts_with('(') && diagnostic.contains("Co-Forth"))
        || (!trimmed.starts_with('(') && diagnostic.contains("Lisp"))
    {
        return WireFailureClass::WrongLanguageDispatch;
    }
    if code.starts_with("E-STACK") || code.starts_with("E-TYPE") || code.starts_with("E-VERIFY") {
        return WireFailureClass::StackOrType;
    }
    if code.starts_with("E-CAP") || code.starts_with("E-EFFECT") || code.starts_with("E-AUTH") {
        return WireFailureClass::Capability;
    }
    if code.starts_with("E-LINK") || code.starts_with("E-NAME") {
        let first = trimmed.chars().next();
        let prose_punctuation = trimmed.contains(". ")
            || trimmed.contains("! ")
            || trimmed.contains("? ")
            || trimmed.lines().count() > 1;
        if first.is_some_and(char::is_uppercase)
            && (trimmed.contains(char::is_whitespace) || prose_punctuation)
        {
            return WireFailureClass::RawProse;
        }
        return WireFailureClass::InventedWord;
    }
    if code == "E-WIRE-001" {
        return WireFailureClass::RawProse;
    }
    WireFailureClass::Other
}

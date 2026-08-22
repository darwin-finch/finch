use super::effects::{CapabilityRequirement, EffectSet};
use super::types::Type;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Note,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticPhase {
    Reader,
    MacroExpansion,
    NameResolution,
    TypeInference,
    Verification,
    Linking,
    Authorization,
    Availability,
    Approval,
    Interpretation,
    HostCall,
    NativeExecution,
    TransactionCommit,
    ChildExecution,
    Cancellation,
    ResourceLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceLanguage {
    Forth,
    Lisp,
    FinchIr,
    Native,
    Provider,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub source_id: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub start_column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

impl SourceSpan {
    pub fn bytes(source_id: impl Into<String>, start_byte: usize, end_byte: usize) -> Self {
        Self {
            source_id: source_id.into(),
            start_byte,
            end_byte,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceOrigin {
    pub language: SourceLanguage,
    pub span: Option<SourceSpan>,
    pub word: Option<String>,
    pub expansion: Option<Box<SourceOrigin>>,
}

impl SourceOrigin {
    pub fn generated(word: impl Into<String>) -> Self {
        Self {
            language: SourceLanguage::FinchIr,
            span: None,
            word: Some(word.into()),
            expansion: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmDiagnostic {
    pub code: String,
    pub severity: Severity,
    pub phase: DiagnosticPhase,
    pub message: String,
    pub primary: Option<SourceOrigin>,
    pub related: Vec<SourceOrigin>,
    pub expected_types: Vec<Type>,
    pub found_types: Vec<Type>,
    pub expected_effects: EffectSet,
    pub found_effects: EffectSet,
    pub capability: Option<CapabilityRequirement>,
    pub trace: Vec<String>,
    pub hints: Vec<String>,
    pub cause: Option<Box<VmDiagnostic>>,
}

impl VmDiagnostic {
    pub fn error(
        code: impl Into<String>,
        phase: DiagnosticPhase,
        message: impl Into<String>,
        primary: Option<SourceOrigin>,
    ) -> Self {
        Self {
            code: code.into(),
            severity: Severity::Error,
            phase,
            message: message.into(),
            primary,
            related: Vec::new(),
            expected_types: Vec::new(),
            found_types: Vec::new(),
            expected_effects: EffectSet::pure(),
            found_effects: EffectSet::pure(),
            capability: None,
            trace: Vec::new(),
            hints: Vec::new(),
            cause: None,
        }
    }

    pub fn type_mismatch(expected: Type, found: Type, primary: Option<SourceOrigin>) -> Self {
        let mut diagnostic = Self::error(
            "E-TYPE-002",
            DiagnosticPhase::Verification,
            format!("expected {expected}, found {found}"),
            primary,
        );
        diagnostic.expected_types.push(expected);
        diagnostic.found_types.push(found);
        diagnostic
    }
}

impl fmt::Display for VmDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for VmDiagnostic {}

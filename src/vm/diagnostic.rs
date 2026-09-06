use super::effects::{CapabilityRequirement, EffectSet};
use super::types::Type;
use serde::{Deserialize, Serialize};
use std::fmt;
use unicode_width::UnicodeWidthChar;

const FOUND_VALUE_ORIGIN_CODE: &str = "N-VALUE-ORIGIN-001";
const MAX_RENDERED_CAUSE_DEPTH: usize = 16;
const MAX_RENDERED_DIAGNOSTIC_BYTES: usize = 64 * 1024;

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

    /// Attach compiler-proven provenance for the incompatible value.
    ///
    /// The relation is encoded as a stable, structured note in the existing
    /// diagnostic cause chain. This keeps aggregate construction of the public
    /// `VmDiagnostic` source-compatible while preserving the producer origin
    /// through serde and the existing recursive IPC codec.
    pub fn set_found_value_origin(&mut self, origin: SourceOrigin) {
        if self
            .cause
            .as_deref()
            .is_some_and(|cause| cause.code == FOUND_VALUE_ORIGIN_CODE)
        {
            self.cause.as_mut().expect("checked producer note").primary = Some(origin);
            return;
        }
        let mut provenance = Self::error(
            FOUND_VALUE_ORIGIN_CODE,
            self.phase,
            "incompatible value producer",
            Some(origin),
        );
        provenance.severity = Severity::Note;
        provenance.cause = self.cause.take();
        self.cause = Some(Box::new(provenance));
    }

    /// Return compiler-proven provenance for the incompatible value.
    pub fn found_value_origin(&self) -> Option<&SourceOrigin> {
        self.cause
            .as_deref()
            .filter(|cause| cause.code == FOUND_VALUE_ORIGIN_CODE)
            .and_then(|cause| cause.primary.as_ref())
    }

    fn presentation_cause(&self) -> Option<&VmDiagnostic> {
        let cause = self.cause.as_deref()?;
        if cause.code == FOUND_VALUE_ORIGIN_CODE {
            return cause.cause.as_deref();
        }
        Some(cause)
    }
}

/// Authored source available to a diagnostic presenter.
///
/// Diagnostics retain stable source identities and byte spans independently of
/// any frontend. A presentation boundary supplies only the source texts it is
/// authorized to show; missing or stale source falls back to the structured
/// code and message without inventing an excerpt.
#[derive(Debug, Clone, Copy)]
pub struct DiagnosticSource<'a> {
    /// Stable identifier stored in each matching [`SourceSpan`].
    pub source_id: &'a str,
    /// Exact authored source against which byte spans are validated.
    pub source: &'a str,
}

/// Render structured VM diagnostics for a human terminal.
///
/// The same `VmDiagnostic` objects remain the machine-readable contract. This
/// adapter adds source excerpts when their byte spans can be validated against
/// a supplied source and otherwise degrades to a concise, location-free form.
pub fn render_vm_diagnostics(
    diagnostics: &[VmDiagnostic],
    sources: &[DiagnosticSource<'_>],
) -> String {
    let mut rendered = String::new();
    for (index, diagnostic) in diagnostics.iter().enumerate() {
        if index > 0 {
            rendered.push_str("\n\n");
        }
        render_vm_diagnostic_chain(&mut rendered, diagnostic, sources);
        if rendered.len() >= MAX_RENDERED_DIAGNOSTIC_BYTES {
            truncate_rendered(&mut rendered);
            break;
        }
    }
    rendered
}

fn render_vm_diagnostic_chain(
    rendered: &mut String,
    diagnostic: &VmDiagnostic,
    sources: &[DiagnosticSource<'_>],
) {
    let mut current = Some(diagnostic);
    let mut depth = 0;
    while let Some(diagnostic) = current {
        if depth > 0 {
            let padding = " ".repeat(depth * 2 - 2);
            rendered.push('\n');
            rendered.push_str(&padding);
            rendered.push_str("Caused by:\n");
        }
        render_vm_diagnostic_body(rendered, diagnostic, sources, depth * 2);
        current = diagnostic.presentation_cause();
        depth += 1;
        if current.is_some() && depth == MAX_RENDERED_CAUSE_DEPTH {
            rendered.push_str("\n… additional diagnostic causes omitted");
            break;
        }
        if rendered.len() >= MAX_RENDERED_DIAGNOSTIC_BYTES {
            truncate_rendered(rendered);
            break;
        }
    }
}

fn render_vm_diagnostic_body(
    rendered: &mut String,
    diagnostic: &VmDiagnostic,
    sources: &[DiagnosticSource<'_>],
    indent: usize,
) {
    let padding = " ".repeat(indent);
    let location = diagnostic
        .primary
        .as_ref()
        .and_then(|origin| validated_excerpt(origin, sources));
    rendered.push_str(&format!(
        "{padding}{} · {} {}",
        diagnostic.code,
        phase_name(diagnostic.phase),
        severity_name(diagnostic.severity)
    ));
    if let Some(excerpt) = &location {
        rendered.push_str(&format!(
            " at {}:{}:{}",
            excerpt.source_id, excerpt.line, excerpt.column
        ));
    }
    rendered.push('\n');

    if let Some(excerpt) = &location {
        rendered.push_str(&padding);
        rendered.push_str(&excerpt.line_text);
        rendered.push('\n');
        rendered.push_str(&padding);
        rendered.push_str(&" ".repeat(excerpt.underline_column));
        rendered.push_str(&"^".repeat(excerpt.underline_width.max(1)));
        rendered.push('\n');
    }

    let word = diagnostic
        .primary
        .as_ref()
        .and_then(|origin| origin.word.as_deref());
    if diagnostic.code == "E-TYPE-002"
        && matches!(
            diagnostic.phase,
            DiagnosticPhase::TypeInference | DiagnosticPhase::Verification
        )
    {
        if let (Some(word), Some(expected), Some(found)) = (
            word,
            type_list(&diagnostic.expected_types),
            type_list(&diagnostic.found_types),
        ) {
            rendered.push_str(&format!(
                "{padding}`{word}` expected {expected}, but received {found}"
            ));
            if let Some(producer) = diagnostic.found_value_origin() {
                rendered.push_str(" produced by ");
                rendered.push_str(&origin_label(producer, sources));
            }
            rendered.push('.');
        } else {
            rendered.push_str(&padding);
            rendered.push_str(&diagnostic.message);
        }
    } else {
        rendered.push_str(&padding);
        rendered.push_str(&diagnostic.message);
    }

    for related in &diagnostic.related {
        rendered.push('\n');
        rendered.push_str(&padding);
        rendered.push_str("Related: ");
        rendered.push_str(&origin_label(related, sources));
    }
    let mut expansion = diagnostic
        .primary
        .as_ref()
        .and_then(|origin| origin.expansion.as_deref());
    let mut expansion_depth = 0;
    while let Some(origin) = expansion {
        if expansion_depth == MAX_RENDERED_CAUSE_DEPTH {
            rendered.push('\n');
            rendered.push_str(&padding);
            rendered.push_str("… additional expansion origins omitted");
            break;
        }
        rendered.push('\n');
        rendered.push_str(&padding);
        rendered.push_str("Expanded from: ");
        rendered.push_str(&origin_label(origin, sources));
        expansion = origin.expansion.as_deref();
        expansion_depth += 1;
    }
    for entry in &diagnostic.trace {
        rendered.push('\n');
        rendered.push_str(&format!("{padding}Trace: {entry}"));
    }
    for hint in &diagnostic.hints {
        rendered.push('\n');
        rendered.push_str(&format!("{padding}Hint: {hint}"));
    }
}

fn truncate_rendered(rendered: &mut String) {
    const MARKER: &str = "\n… diagnostic output truncated";
    if rendered.len() <= MAX_RENDERED_DIAGNOSTIC_BYTES {
        return;
    }
    let mut end = MAX_RENDERED_DIAGNOSTIC_BYTES.saturating_sub(MARKER.len());
    while !rendered.is_char_boundary(end) {
        end -= 1;
    }
    rendered.truncate(end);
    rendered.push_str(MARKER);
}

fn type_list(types: &[Type]) -> Option<String> {
    (!types.is_empty()).then(|| {
        types
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    })
}

fn origin_label(origin: &SourceOrigin, sources: &[DiagnosticSource<'_>]) -> String {
    let word = origin.word.as_deref().unwrap_or("source form");
    validated_excerpt(origin, sources).map_or_else(
        || format!("`{word}`"),
        |excerpt| {
            format!(
                "`{word}` at {}:{}:{}",
                excerpt.source_id, excerpt.line, excerpt.column
            )
        },
    )
}

struct ValidatedExcerpt<'a> {
    source_id: &'a str,
    line: usize,
    column: usize,
    line_text: String,
    underline_column: usize,
    underline_width: usize,
}

fn validated_excerpt<'a>(
    origin: &SourceOrigin,
    sources: &'a [DiagnosticSource<'a>],
) -> Option<ValidatedExcerpt<'a>> {
    let span = origin.span.as_ref()?;
    let source = sources
        .iter()
        .find(|source| source.source_id == span.source_id)?;
    if span.start_byte > span.end_byte
        || span.end_byte > source.source.len()
        || !source.source.is_char_boundary(span.start_byte)
        || !source.source.is_char_boundary(span.end_byte)
    {
        return None;
    }
    let line_start = source.source[..span.start_byte]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line_end = source.source[span.start_byte..]
        .find('\n')
        .map_or(source.source.len(), |index| span.start_byte + index);
    let underline_end = span.end_byte.min(line_end);
    let prefix = &source.source[line_start..span.start_byte];
    let underlined = &source.source[span.start_byte..underline_end];
    let line = source.source[..span.start_byte]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let column = prefix.chars().count() + 1;
    Some(ValidatedExcerpt {
        source_id: source.source_id,
        line,
        column,
        line_text: expand_tabs(&source.source[line_start..line_end]),
        underline_column: display_width_with_tabs(prefix),
        underline_width: display_width_with_tabs(underlined),
    })
}

fn expand_tabs(text: &str) -> String {
    const TAB_STOP: usize = 4;
    let mut rendered = String::new();
    let mut column = 0;
    for character in text.chars() {
        if character == '\t' {
            let spaces = TAB_STOP - column % TAB_STOP;
            rendered.push_str(&" ".repeat(spaces));
            column += spaces;
        } else {
            rendered.push(character);
            column += character.width().unwrap_or(0);
        }
    }
    rendered
}

fn display_width_with_tabs(text: &str) -> usize {
    const TAB_STOP: usize = 4;
    text.chars().fold(0, |column, character| {
        if character == '\t' {
            column + TAB_STOP - column % TAB_STOP
        } else {
            column + character.width().unwrap_or(0)
        }
    })
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Note => "note",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

fn phase_name(phase: DiagnosticPhase) -> &'static str {
    match phase {
        DiagnosticPhase::Reader => "reader",
        DiagnosticPhase::MacroExpansion => "macro-expansion",
        DiagnosticPhase::NameResolution => "name-resolution",
        DiagnosticPhase::TypeInference => "type-inference",
        DiagnosticPhase::Verification => "verification",
        DiagnosticPhase::Linking => "linking",
        DiagnosticPhase::Authorization => "authorization",
        DiagnosticPhase::Availability => "availability",
        DiagnosticPhase::Approval => "approval",
        DiagnosticPhase::Interpretation => "interpretation",
        DiagnosticPhase::HostCall => "host-call",
        DiagnosticPhase::NativeExecution => "native-execution",
        DiagnosticPhase::TransactionCommit => "transaction-commit",
        DiagnosticPhase::ChildExecution => "child-execution",
        DiagnosticPhase::Cancellation => "cancellation",
        DiagnosticPhase::ResourceLimit => "resource-limit",
    }
}

impl fmt::Display for VmDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for VmDiagnostic {}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(source_id: &str, source: &str, needle: &str, word: &str) -> SourceOrigin {
        let start = source.find(needle).expect("diagnostic fixture needle");
        SourceOrigin {
            language: SourceLanguage::Forth,
            span: Some(SourceSpan::bytes(source_id, start, start + needle.len())),
            word: Some(word.to_string()),
            expansion: None,
        }
    }

    #[test]
    fn test_renderer_preserves_all_structured_fields_and_stable_diagnostic_order() {
        let source = "\tα 3 4 + say";
        let mut first = VmDiagnostic::type_mismatch(
            Type::String,
            Type::Int,
            Some(origin("unicode.forth", source, "say", "say")),
        );
        first
            .related
            .push(origin("unicode.forth", source, "3", "literal input"));
        first
            .hints
            .push("convert the integer with `int-to-string`".into());
        first.trace.push("main -> say".into());
        let mut expansion = origin("unicode.forth", source, "+", "macro outer");
        expansion.expansion = Some(Box::new(origin(
            "unicode.forth",
            source,
            "3",
            "macro inner",
        )));
        first.primary.as_mut().expect("primary").expansion = Some(Box::new(expansion));
        first.cause = Some(Box::new(VmDiagnostic::error(
            "E-CAUSE-001",
            DiagnosticPhase::Interpretation,
            "nested failure",
            None,
        )));
        first.set_found_value_origin(origin("unicode.forth", source, "+", "+"));
        let second = VmDiagnostic::error(
            "E-SECOND-001",
            DiagnosticPhase::Linking,
            "later diagnostic",
            None,
        );

        let rendered = render_vm_diagnostics(
            &[first, second],
            &[DiagnosticSource {
                source_id: "unicode.forth",
                source,
            }],
        );
        for required in [
            "E-TYPE-002 · verification error at unicode.forth:1:10",
            "    α 3 4 + say",
            "received int produced by `+` at unicode.forth:1:8",
            "Related: `literal input` at unicode.forth:1:4",
            "Expanded from: `macro outer`",
            "Expanded from: `macro inner`",
            "Trace: main -> say",
            "Hint: convert the integer with `int-to-string`",
            "Caused by:\n  E-CAUSE-001 · interpretation error",
            "E-SECOND-001 · linking error\nlater diagnostic",
        ] {
            assert!(
                rendered.contains(required),
                "structured renderer omitted {required:?}; rendered={rendered:?}"
            );
        }
        assert!(
            rendered.find("E-TYPE-002") < rendered.find("E-SECOND-001"),
            "diagnostic order changed; rendered={rendered:?}"
        );
        assert_eq!(
            rendered.matches("Hint:").count(),
            1,
            "one mismatch should render one actionable correction; rendered={rendered:?}"
        );
    }

    #[test]
    fn test_renderer_falls_back_safely_for_malformed_or_missing_spans() {
        let source = "say";
        let malformed = VmDiagnostic::error(
            "E-SPAN-001",
            DiagnosticPhase::Reader,
            "bad external span",
            Some(SourceOrigin {
                language: SourceLanguage::Provider,
                span: Some(SourceSpan::bytes("external", 0, usize::MAX)),
                word: Some("say".into()),
                expansion: None,
            }),
        );
        let rendered = render_vm_diagnostics(
            &[malformed],
            &[DiagnosticSource {
                source_id: "external",
                source,
            }],
        );
        assert_eq!(
            rendered, "E-SPAN-001 · reader error\nbad external span",
            "malformed span should retain its diagnostic-specific message without an invented location"
        );
    }

    #[test]
    fn test_renderer_uses_terminal_cells_for_unicode_carets_and_scalar_source_columns() {
        let source = "界\tsay";
        let diagnostic = VmDiagnostic::type_mismatch(
            Type::String,
            Type::Int,
            Some(origin("wide.forth", source, "say", "say")),
        );
        let rendered = render_vm_diagnostics(
            &[diagnostic],
            &[DiagnosticSource {
                source_id: "wide.forth",
                source,
            }],
        );
        assert!(
            rendered.contains("wide.forth:1:3\n界  say\n    ^^^"),
            "source columns must remain Unicode-scalar based while carets and tabs use terminal cells; rendered={rendered:?}"
        );
    }

    #[test]
    fn test_renderer_bounds_hostile_nested_causes_and_output() {
        let mut diagnostic = VmDiagnostic::error(
            "E-ROOT-001",
            DiagnosticPhase::Interpretation,
            "root failure",
            None,
        );
        for index in 0..2_048 {
            let mut parent = VmDiagnostic::error(
                format!("E-CAUSE-{index:04}"),
                DiagnosticPhase::Interpretation,
                "nested failure",
                None,
            );
            parent.cause = Some(Box::new(diagnostic));
            diagnostic = parent;
        }
        let rendered = render_vm_diagnostics(std::slice::from_ref(&diagnostic), &[]);
        assert!(
            rendered.contains("additional diagnostic causes omitted"),
            "hostile cause chain was not explicitly bounded; rendered_len={}",
            rendered.len()
        );
        assert!(
            rendered.len() <= MAX_RENDERED_DIAGNOSTIC_BYTES,
            "hostile cause chain exceeded the public renderer output bound: {} bytes",
            rendered.len()
        );
        let mut current = Some(diagnostic);
        while let Some(mut node) = current {
            current = node.cause.take().map(|cause| *cause);
        }

        let oversized = VmDiagnostic::error(
            "E-LARGE-001",
            DiagnosticPhase::Interpretation,
            "x".repeat(MAX_RENDERED_DIAGNOSTIC_BYTES * 2),
            None,
        );
        let rendered = render_vm_diagnostics(&[oversized], &[]);
        assert!(
            rendered.ends_with("… diagnostic output truncated")
                && rendered.len() <= MAX_RENDERED_DIAGNOSTIC_BYTES,
            "oversized diagnostic did not honor the renderer byte bound; rendered_len={}",
            rendered.len()
        );
    }

    #[test]
    fn test_renderer_handles_missing_sources_and_cross_line_spans_without_inference() {
        let source = "ab\ncd";
        let crossing = SourceOrigin {
            language: SourceLanguage::Provider,
            span: Some(SourceSpan::bytes("crossing", 1, source.len())),
            word: Some("form".into()),
            expansion: None,
        };
        let unavailable = SourceOrigin {
            language: SourceLanguage::Provider,
            span: None,
            word: Some("say".into()),
            expansion: None,
        };
        let cross_line = VmDiagnostic::error(
            "E-CROSS-001",
            DiagnosticPhase::Reader,
            "cross-line failure",
            Some(crossing),
        );
        let missing_source =
            VmDiagnostic::type_mismatch(Type::String, Type::Int, Some(unavailable));
        let rendered = render_vm_diagnostics(
            &[cross_line, missing_source],
            &[DiagnosticSource {
                source_id: "crossing",
                source,
            }],
        );
        assert!(
            rendered.contains("crossing:1:2\nab\n ^\ncross-line failure"),
            "cross-line spans must underline only the available first source line; rendered={rendered:?}"
        );
        assert!(
            rendered.contains("E-TYPE-002 · verification error\n`say` expected string, but received int."),
            "missing source/span must keep typed facts without inventing a location; rendered={rendered:?}"
        );
    }

    #[test]
    fn test_renderer_preserves_non_verifier_diagnostic_specific_message() {
        let mut diagnostic = VmDiagnostic::error(
            "E-TYPE-002",
            DiagnosticPhase::HostCall,
            "host returned an incompatible value for request `lookup`",
            Some(SourceOrigin::generated("broken")),
        );
        diagnostic.expected_types.push(Type::String);
        diagnostic.found_types.push(Type::Int);
        let rendered = render_vm_diagnostics(&[diagnostic], &[]);
        assert!(
            rendered.contains("host returned an incompatible value for request `lookup`"),
            "renderer applied verifier-only wording to a diagnostic-specific host-call error; rendered={rendered:?}"
        );
    }
}

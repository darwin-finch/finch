use super::effects::{CapabilityRequirement, EffectSet};
use super::types::Type;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fmt::Write as _;

const FOUND_VALUE_ORIGIN_CODE: &str = "N-VALUE-ORIGIN-001";
const FORTH_INT_TO_STRING_HINT: &str = "convert the integer with `int-to-string` before `say`";
const MAX_INTERACTIVE_DIAGNOSTIC_BYTES: usize = 4 * 1024;
const MAX_INTERACTIVE_SOURCE_ID_BYTES: usize = 128;
const MAX_INTERACTIVE_SOURCE_LINE_BYTES: usize = 512;

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

    /// Attach the compiler-proven `+` origin for the narrow Forth `say`
    /// mismatch diagnostic without expanding the public diagnostic schema.
    pub(crate) fn set_forth_plus_value_origin(&mut self, origin: SourceOrigin) {
        if origin.word.as_deref() != Some("+") || origin.expansion.is_some() {
            return;
        }
        let Some(span) = origin.span.as_ref() else {
            return;
        };
        if span.source_id.len() > MAX_INTERACTIVE_SOURCE_ID_BYTES {
            return;
        }
        let mut provenance = Self::error(
            FOUND_VALUE_ORIGIN_CODE,
            self.phase,
            "incompatible value produced here",
            Some(origin),
        );
        provenance.severity = Severity::Note;
        provenance.cause = self.cause.take();
        self.cause = Some(Box::new(provenance));
    }

    pub(crate) fn add_supported_forth_int_to_string_hint(&mut self) {
        if !self
            .hints
            .iter()
            .any(|hint| hint == FORTH_INT_TO_STRING_HINT)
        {
            self.hints.push(FORTH_INT_TO_STRING_HINT.to_string());
        }
    }

    pub(crate) fn is_forth_say_int_mismatch(&self) -> bool {
        self.code == "E-TYPE-002"
            && matches!(
                self.phase,
                DiagnosticPhase::TypeInference | DiagnosticPhase::Verification
            )
            && self.primary.as_ref().is_some_and(|origin| {
                origin.language == SourceLanguage::Forth && origin.word.as_deref() == Some("say")
            })
            && self.expected_types == [Type::String]
            && self.found_types == [Type::Int]
    }

    fn forth_plus_value_origin(&self) -> Option<&SourceOrigin> {
        self.cause
            .as_deref()
            .filter(|cause| cause.code == FOUND_VALUE_ORIGIN_CODE)
            .and_then(|cause| cause.primary.as_ref())
            .filter(|origin| origin.word.as_deref() == Some("+"))
    }
}

/// Render the one source-cited diagnostic promised by issue #353's narrow
/// interactive-Forth slice. Every copied source component is rejected before
/// allocation when it exceeds the construction-time bounds.
pub(crate) fn render_interactive_forth_say_mismatch(
    diagnostic: &VmDiagnostic,
    source_id: &str,
    source: &str,
) -> Option<String> {
    if !diagnostic.is_forth_say_int_mismatch() {
        return None;
    }

    let primary = diagnostic
        .primary
        .as_ref()
        .and_then(|origin| validated_interactive_location(origin, source_id, source));
    let producer = diagnostic
        .forth_plus_value_origin()
        .and_then(|origin| validated_interactive_location(origin, source_id, source));

    let mut rendered = String::with_capacity(512);
    let _ = write!(
        rendered,
        "{} · {} error",
        diagnostic.code,
        diagnostic_phase_name(diagnostic.phase)
    );
    if let Some(location) = primary.as_ref() {
        let _ = write!(
            rendered,
            " at {}:{}:{}",
            source_id, location.line, location.column
        );
    }
    rendered.push('\n');

    if let Some(location) = primary
        .as_ref()
        .filter(|location| location.line_text.is_some())
    {
        let line_text = location.line_text.expect("filtered source line");
        rendered.push_str(line_text);
        rendered.push('\n');
        rendered.push_str(&" ".repeat(location.underline_column));
        rendered.push_str(&"^".repeat(location.underline_width.max(1)));
        rendered.push('\n');
    }

    rendered.push_str("`say` expected string, but received int");
    if let Some(location) = producer.as_ref() {
        let _ = write!(
            rendered,
            " produced by `+` at {}:{}:{}",
            source_id, location.line, location.column
        );
    } else if diagnostic.forth_plus_value_origin().is_some() {
        rendered.push_str(" produced by `+`");
    }
    rendered.push('.');

    if diagnostic
        .hints
        .iter()
        .any(|hint| hint == FORTH_INT_TO_STRING_HINT)
    {
        rendered.push_str("\nHint: ");
        rendered.push_str(FORTH_INT_TO_STRING_HINT);
        rendered.push('.');
    }

    debug_assert!(rendered.len() <= MAX_INTERACTIVE_DIAGNOSTIC_BYTES);
    Some(rendered)
}

struct InteractiveLocation<'a> {
    line: usize,
    column: usize,
    line_text: Option<&'a str>,
    underline_column: usize,
    underline_width: usize,
}

fn validated_interactive_location<'a>(
    origin: &SourceOrigin,
    source_id: &str,
    source: &'a str,
) -> Option<InteractiveLocation<'a>> {
    if source_id.len() > MAX_INTERACTIVE_SOURCE_ID_BYTES {
        return None;
    }
    let span = origin.span.as_ref()?;
    if span.source_id != source_id
        || span.start_byte > span.end_byte
        || span.end_byte > source.len()
        || !source.is_char_boundary(span.start_byte)
        || !source.is_char_boundary(span.end_byte)
    {
        return None;
    }

    let line_start = source[..span.start_byte]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let line_end = source[span.start_byte..]
        .find('\n')
        .map_or(source.len(), |index| span.start_byte + index);
    let prefix = &source[line_start..span.start_byte];
    let line = source[..span.start_byte]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let column = prefix.chars().count() + 1;

    let line_text = &source[line_start..line_end];
    let underline_end = span.end_byte.min(line_end);
    let underline = &source[span.start_byte..underline_end];
    let safely_renderable = line_text.len() <= MAX_INTERACTIVE_SOURCE_LINE_BYTES
        && line_text.is_ascii()
        && !line_text.contains('\t')
        && !source[span.start_byte..span.end_byte].contains('\n');

    Some(InteractiveLocation {
        line,
        column,
        line_text: safely_renderable.then_some(line_text),
        underline_column: prefix.len(),
        underline_width: underline.len(),
    })
}

fn diagnostic_phase_name(phase: DiagnosticPhase) -> &'static str {
    match phase {
        DiagnosticPhase::TypeInference => "type-inference",
        DiagnosticPhase::Verification => "verification",
        _ => "VM",
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

    fn say_mismatch(span: SourceSpan) -> VmDiagnostic {
        let mut diagnostic = VmDiagnostic::type_mismatch(
            Type::String,
            Type::Int,
            Some(SourceOrigin {
                language: SourceLanguage::Forth,
                span: Some(span),
                word: Some("say".into()),
                expansion: None,
            }),
        );
        diagnostic.set_forth_plus_value_origin(SourceOrigin {
            language: SourceLanguage::Forth,
            span: Some(SourceSpan::bytes("interactive.forth", 4, 5)),
            word: Some("+".into()),
            expansion: None,
        });
        diagnostic.add_supported_forth_int_to_string_hint();
        diagnostic
    }

    #[test]
    fn test_interactive_forth_renderer_falls_back_safely_for_hostile_spans() {
        let oversized_source = format!("{}say", "x".repeat(MAX_INTERACTIVE_SOURCE_LINE_BYTES + 1));
        let cases = [
            (
                "malformed",
                "3 4 + say",
                SourceSpan::bytes("interactive.forth", 6, usize::MAX),
            ),
            (
                "multiline",
                "3 4 + say\nnext",
                SourceSpan::bytes("interactive.forth", 6, 14),
            ),
            (
                "unicode-tab",
                "界\tsay",
                SourceSpan::bytes("interactive.forth", 4, 7),
            ),
            (
                "oversized-line",
                oversized_source.as_str(),
                SourceSpan::bytes(
                    "interactive.forth",
                    MAX_INTERACTIVE_SOURCE_LINE_BYTES + 1,
                    MAX_INTERACTIVE_SOURCE_LINE_BYTES + 4,
                ),
            ),
        ];

        for (name, source, span) in cases {
            let diagnostic = say_mismatch(span);
            let rendered =
                render_interactive_forth_say_mismatch(&diagnostic, "interactive.forth", source)
                    .unwrap_or_else(|| panic!("{name} fixture should retain a concise diagnostic"));
            assert!(
                rendered.contains("E-TYPE-002 · verification error")
                    && rendered.contains("`say` expected string, but received int")
                    && rendered.contains("Hint: convert the integer with `int-to-string`"),
                "{name} fallback lost stable structured facts; rendered={rendered:?} diagnostic={diagnostic:#?}"
            );
            assert!(
                rendered.len() <= MAX_INTERACTIVE_DIAGNOSTIC_BYTES,
                "{name} fallback exceeded the construction bound; len={} rendered={rendered:?}",
                rendered.len()
            );
        }
    }

    #[test]
    fn test_interactive_forth_renderer_rejects_noneligible_failures() {
        let mut diagnostic = say_mismatch(SourceSpan::bytes("interactive.forth", 6, 9));
        for (name, mutate) in [
            ("wrong code", 0_u8),
            ("wrong language", 1),
            ("wrong consumer", 2),
            ("wrong found type", 3),
        ] {
            let mut candidate = diagnostic.clone();
            match mutate {
                0 => candidate.code = "E-OTHER-001".into(),
                1 => {
                    candidate
                        .primary
                        .as_mut()
                        .expect("fixture primary")
                        .language = SourceLanguage::Lisp
                }
                2 => {
                    candidate.primary.as_mut().expect("fixture primary").word = Some("drop".into())
                }
                3 => candidate.found_types = vec![Type::Bool],
                _ => unreachable!("enumerated mutation"),
            }
            assert!(
                render_interactive_forth_say_mismatch(
                    &candidate,
                    "interactive.forth",
                    "3 4 + say",
                )
                .is_none(),
                "{name} failure entered the narrow standalone renderer; diagnostic={candidate:#?}"
            );
        }

        diagnostic.hints.clear();
        let rendered =
            render_interactive_forth_say_mismatch(&diagnostic, "interactive.forth", "3 4 + say")
                .expect("eligible mismatch without a supported hint should retain its core facts");
        assert!(
            !rendered.contains("Hint:"),
            "renderer invented an unsupported correction; rendered={rendered:?} diagnostic={diagnostic:#?}"
        );
    }
}

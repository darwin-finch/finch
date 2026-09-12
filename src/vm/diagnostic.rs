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

impl VmDiagnostic {
    /// A report that names the offending source, not just the failure.
    ///
    /// `Display` gives one line, which is right for a log and useless for correcting a program:
    /// the span, the expected and found types, the capability, and the cause chain are all carried
    /// on this struct and all dropped by it. Anything shown to a person — or handed to a model and
    /// asked for a fix — should render instead, passing the program text so the failing line is
    /// quoted and underlined.
    pub fn render(&self, source: Option<&str>) -> String {
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        };
        let mut lines = vec![format!("{severity}[{}]: {}", self.code, self.message)];
        if let Some(span) = self
            .primary
            .as_ref()
            .and_then(|origin| origin.span.as_ref())
        {
            lines.push(format!(
                " --> {}:{}:{}",
                span.source_id, span.start_line, span.start_column
            ));
            lines.extend(quote_span(span, source));
        }
        for note in self.notes() {
            lines.push(format!("  = {note}"));
        }
        if let Some(cause) = &self.cause {
            lines.push("caused by:".into());
            lines.extend(cause.render(source).lines().map(|line| format!("  {line}")));
        }
        lines.join("\n")
    }

    /// The annotations under a rendered diagnostic, in the order they help most.
    fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if !self.expected_types.is_empty() || !self.found_types.is_empty() {
            notes.push(format!(
                "expected: {}",
                join_display(&self.expected_types, "nothing")
            ));
            notes.push(format!(
                "found:    {}",
                join_display(&self.found_types, "nothing")
            ));
        }
        if self.expected_effects != self.found_effects {
            notes.push(format!(
                "declared effects: {}; actual effects: {}",
                self.expected_effects, self.found_effects
            ));
        }
        if let Some(capability) = &self.capability {
            notes.push(format!("requires capability: {:?}", capability.capability));
        }
        // Hints are the part a model can act on directly, so they come last and stand out.
        notes.extend(self.hints.iter().map(|hint| format!("hint: {hint}")));
        let mut expansion = self
            .primary
            .as_ref()
            .and_then(|origin| origin.expansion.as_ref());
        while let Some(origin) = expansion {
            if let Some(word) = &origin.word {
                notes.push(format!("in expansion of `{word}`"));
            }
            expansion = origin.expansion.as_ref();
        }
        for frame in &self.trace {
            notes.push(format!("in {frame}"));
        }
        for origin in &self.related {
            if let Some(span) = &origin.span {
                notes.push(format!(
                    "related: {}:{}:{}",
                    span.source_id, span.start_line, span.start_column
                ));
            }
        }
        notes
    }
}

/// Names close enough to `target` to be what the author meant, nearest first.
///
/// An unknown-word error that only says the word is unknown leaves a model to guess; a model that
/// guesses usually invents another word that does not exist. Naming the closest real words turns
/// the failure into a correction it can apply.
///
/// Spelling separators differently (`str_cat` for `str-cat`) is the most common near miss and is
/// not a typo at all, so it is matched first and exactly, before any edit-distance work.
pub fn nearest_names<'a>(target: &str, candidates: impl Iterator<Item = &'a str>) -> Vec<String> {
    fn squashed(name: &str) -> String {
        name.chars()
            .filter(|c| *c != '-' && *c != '_')
            .flat_map(char::to_lowercase)
            .collect()
    }
    let target_squashed = squashed(target);
    // An edit budget that grows with the word: one slip in `dup`, three in `network-connect`.
    let budget = (target.chars().count() / 3).max(1);
    let mut scored: Vec<(usize, String)> = Vec::new();
    for candidate in candidates {
        if candidate == target {
            continue;
        }
        let distance = if squashed(candidate) == target_squashed {
            0
        } else {
            match edit_distance(target, candidate, budget) {
                Some(distance) => distance,
                None => continue,
            }
        };
        scored.push((distance, candidate.to_string()));
    }
    scored.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    scored.truncate(3);
    scored.into_iter().map(|(_, name)| name).collect()
}

/// Levenshtein distance, abandoned once it exceeds `budget` so a large vocabulary stays cheap.
fn edit_distance(left: &str, right: &str, budget: usize) -> Option<usize> {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len().abs_diff(right.len()) > budget {
        return None;
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (i, left_char) in left.iter().enumerate() {
        current[0] = i + 1;
        let mut row_best = current[0];
        for (j, right_char) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_char != right_char);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
            row_best = row_best.min(current[j + 1]);
        }
        if row_best > budget {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let distance = previous[right.len()];
    (distance <= budget).then_some(distance)
}

fn join_display<T: fmt::Display>(values: &[T], empty: &str) -> String {
    if values.is_empty() {
        return empty.to_string();
    }
    values
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The failing line with a caret run under the span, when the source is available.
fn quote_span(span: &SourceSpan, source: Option<&str>) -> Vec<String> {
    let Some(text) = source else {
        return Vec::new();
    };
    let Some(line) = text.lines().nth(span.start_line.saturating_sub(1)) else {
        return Vec::new();
    };
    let number = span.start_line.to_string();
    let gutter = " ".repeat(number.len());
    // A span that ends on a later line is underlined to the end of this one; carets never wrap.
    let end_column = if span.end_line == span.start_line {
        span.end_column
    } else {
        line.chars().count() + 1
    };
    let start = span.start_column.max(1);
    let width = end_column.saturating_sub(start).max(1);
    vec![
        format!("{gutter} |"),
        format!("{number} | {line}"),
        format!("{gutter} | {}{}", " ".repeat(start - 1), "^".repeat(width)),
    ]
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

    fn span(source_id: &str, line: usize, column: usize, end_column: usize) -> SourceSpan {
        SourceSpan {
            source_id: source_id.into(),
            start_byte: 0,
            end_byte: 0,
            start_line: line,
            start_column: column,
            end_line: line,
            end_column,
        }
    }

    fn origin(span: SourceSpan) -> SourceOrigin {
        SourceOrigin {
            language: SourceLanguage::Forth,
            span: Some(span),
            word: None,
            expansion: None,
        }
    }

    #[test]
    fn test_render_quotes_the_failing_line_and_underlines_the_span() {
        // The whole point: a model told only "unknown word" cannot find the word it must replace.
        let diagnostic = VmDiagnostic::error(
            "E-LINK-002",
            DiagnosticPhase::Linking,
            "unknown Co-Forth word 'frobnicate'",
            Some(origin(span("program.forth", 2, 7, 18))),
        );
        let source = "1 2 +\n3 4 frobnicate\n";
        let rendered = diagnostic.render(Some(source));
        let expected = "\
error[E-LINK-002]: unknown Co-Forth word 'frobnicate'
 --> program.forth:2:7
  |
2 | 3 4 frobnicate
  |       ^^^^^^^^^^^";
        assert_eq!(
            expected, rendered,
            "a rendered diagnostic must locate the failure in the source:\n{rendered}"
        );
    }

    #[test]
    fn test_render_without_source_still_gives_the_location() {
        // Effect logs and IPC consumers do not always hold the program text.
        let diagnostic = VmDiagnostic::error(
            "E-LINK-002",
            DiagnosticPhase::Linking,
            "unknown Co-Forth word 'frobnicate'",
            Some(origin(span("program.forth", 2, 7, 18))),
        );
        let rendered = diagnostic.render(None);
        assert!(
            rendered.contains("--> program.forth:2:7"),
            "location must survive a missing source:\n{rendered}"
        );
        assert!(
            !rendered.contains('^'),
            "no caret can be drawn without the line it points at:\n{rendered}"
        );
    }

    #[test]
    fn test_render_carries_expected_and_found_types() {
        let diagnostic = VmDiagnostic::type_mismatch(
            Type::Int,
            Type::String,
            Some(origin(span("program.forth", 1, 1, 4))),
        );
        let rendered = diagnostic.render(Some("foo\n"));
        assert!(
            rendered.contains("expected: int") && rendered.contains("found:    string"),
            "a type mismatch must state both sides:\n{rendered}"
        );
    }

    #[test]
    fn test_render_carries_hints_and_the_cause_chain() {
        // Hints are the part a model can act on, and a cause explains a failure the top line cannot.
        let mut cause = VmDiagnostic::error(
            "E-TYPE-002",
            DiagnosticPhase::Verification,
            "expected Int, found String",
            None,
        );
        cause.hints.push("`length` returns Int".into());
        let mut diagnostic = VmDiagnostic::error(
            "E-LINK-002",
            DiagnosticPhase::Linking,
            "unknown Co-Forth word 'lenght'",
            Some(origin(span("program.forth", 1, 1, 7))),
        );
        diagnostic.hints.push("did you mean `length`?".into());
        diagnostic.cause = Some(Box::new(cause));
        let rendered = diagnostic.render(Some("lenght\n"));
        assert!(
            rendered.contains("hint: did you mean `length`?"),
            "hints must reach the reader:\n{rendered}"
        );
        assert!(
            rendered.contains("caused by:") && rendered.contains("E-TYPE-002"),
            "the cause chain must be rendered, not dropped:\n{rendered}"
        );
        assert!(
            rendered.contains("    = hint: `length` returns Int"),
            "a cause's own hints must survive, indented under it:\n{rendered}"
        );
    }

    #[test]
    fn test_render_names_the_expansion_a_failure_came_from() {
        let mut primary = origin(span("program.forth", 3, 1, 5));
        primary.expansion = Some(Box::new(SourceOrigin {
            language: SourceLanguage::Forth,
            span: None,
            word: Some("outer-word".into()),
            expansion: None,
        }));
        let diagnostic = VmDiagnostic::error(
            "E-LINK-002",
            DiagnosticPhase::Linking,
            "unknown word",
            Some(primary),
        );
        let rendered = diagnostic.render(None);
        assert!(
            rendered.contains("in expansion of `outer-word`"),
            "a failure inside an expansion must say which word expanded:\n{rendered}"
        );
    }

    #[test]
    fn test_nearest_names_matches_a_separator_spelling_exactly() {
        // The most common near miss is not a typo: a model writes `str_cat` for `str-cat`.
        let vocabulary = ["str-cat", "str-len", "network-connect"];
        let nearest = nearest_names("str_cat", vocabulary.into_iter());
        assert_eq!(
            vec!["str-cat".to_string()],
            nearest,
            "a separator-only difference must rank first and alone"
        );
    }

    #[test]
    fn test_nearest_names_finds_a_transposition_and_ranks_it_first() {
        let vocabulary = ["length", "len", "ledger"];
        let nearest = nearest_names("lenght", vocabulary.into_iter());
        assert_eq!(
            Some(&"length".to_string()),
            nearest.first(),
            "the nearest real word must lead the suggestions: {nearest:?}"
        );
    }

    #[test]
    fn test_nearest_names_stays_silent_when_nothing_is_close() {
        // A wrong suggestion is worse than none: it sends a model to another word that fails.
        let vocabulary = ["dup", "drop", "swap"];
        assert!(
            nearest_names("network-connect", vocabulary.into_iter()).is_empty(),
            "an unrelated word must produce no suggestion"
        );
    }

    #[test]
    fn test_nearest_names_budget_grows_with_the_word() {
        // One slip is allowed in a short word, more in a long one, so `dup`/`dip` stay distinct
        // while a long name survives a few typos.
        assert!(nearest_names("dupp", ["dup"].into_iter()).contains(&"dup".to_string()));
        assert!(
            nearest_names("netwrok-conect", ["network-connect"].into_iter())
                .contains(&"network-connect".to_string())
        );
    }

    #[test]
    fn test_nearest_names_never_suggests_the_word_itself() {
        assert!(nearest_names("dup", ["dup", "dip"].into_iter())
            .iter()
            .all(|name| name != "dup"));
    }

    #[test]
    fn test_display_stays_one_line_for_logs() {
        // Display is used in error chains; rendering multi-line there would wreck log output.
        let diagnostic = VmDiagnostic::error(
            "E-LINK-002",
            DiagnosticPhase::Linking,
            "unknown Co-Forth word 'frobnicate'",
            Some(origin(span("program.forth", 2, 7, 18))),
        );
        let shown = diagnostic.to_string();
        assert_eq!("E-LINK-002: unknown Co-Forth word 'frobnicate'", shown);
        assert!(
            !shown.contains('\n'),
            "Display must stay a single line: {shown}"
        );
    }
}

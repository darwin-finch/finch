//! Frontend-neutral rendering contracts for retained messages.
//!
//! Messages own their semantic projection and render it against an immutable
//! set of frontend capabilities. A frontend may then resolve the resulting
//! lines into terminal cells, HTML, speech, or another physical representation
//! without matching on the concrete message type.

use super::{MessageId, TranscriptRow, TranscriptRowId};
use crate::config::ColorScheme;

/// The frontend consuming a rendered message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontendKind {
    /// Interactive ANSI terminal frontend.
    Terminal,
    /// Plain-text transcript frontend.
    PlainText,
    /// Deterministic synthetic test frontend.
    Test,
}

/// Color capability advertised by a frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    /// No color support.
    Monochrome,
    /// Sixteen ANSI colors.
    Ansi16,
    /// 256-color ANSI palette.
    Ansi256,
    /// 24-bit color.
    TrueColor,
}

/// Immutable capabilities which may affect message rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderCapabilities {
    /// Controlled viewport width in terminal columns or frontend units.
    pub viewport_width: usize,
    /// Available color fidelity.
    pub color_depth: ColorDepth,
    /// Whether Unicode disclosure glyphs are supported.
    pub unicode: bool,
    /// Whether the frontend can resolve hyperlinks.
    pub hyperlinks: bool,
    /// Kind of consuming frontend.
    pub frontend: FrontendKind,
}

impl RenderCapabilities {
    /// Deterministic terminal defaults. Callers should override width with the
    /// actual controlled viewport when it is known.
    pub const fn terminal(viewport_width: usize) -> Self {
        Self {
            viewport_width,
            color_depth: ColorDepth::TrueColor,
            unicode: true,
            hyperlinks: false,
            frontend: FrontendKind::Terminal,
        }
    }

    /// A capability set intended for deterministic unit and golden tests.
    pub const fn synthetic(viewport_width: usize) -> Self {
        Self {
            viewport_width,
            color_depth: ColorDepth::Monochrome,
            unicode: false,
            hyperlinks: false,
            frontend: FrontendKind::Test,
        }
    }
}

/// Frontend-local disclosure and focus state.
///
/// The lookup is deliberately read-only and semantic: messages never receive
/// raw mouse coordinates or query process-global terminal state.
pub trait DisclosureLookup {
    /// Return an explicit disclosure override for this row, if any.
    fn expanded(&self, row_id: &TranscriptRowId) -> Option<bool>;
    /// Whether this row owns semantic keyboard focus.
    fn focused(&self, row_id: &TranscriptRowId) -> bool;
}

/// Immutable dependencies supplied to one message render.
pub struct RenderContext<'a> {
    /// Immutable color scheme chosen by the frontend.
    pub colors: &'a ColorScheme,
    /// Immutable frontend capabilities.
    pub capabilities: RenderCapabilities,
    /// Optional frontend-local disclosure and focus lookup.
    pub disclosure: Option<&'a dyn DisclosureLookup>,
    /// Whether every expandable row must render expanded.
    pub force_expanded: bool,
}

impl<'a> RenderContext<'a> {
    /// Create an immutable render context.
    pub const fn new(colors: &'a ColorScheme, capabilities: RenderCapabilities) -> Self {
        Self {
            colors,
            capabilities,
            disclosure: None,
            force_expanded: false,
        }
    }

    /// Add read-only frontend disclosure state.
    pub const fn with_disclosure(mut self, disclosure: &'a dyn DisclosureLookup) -> Self {
        self.disclosure = Some(disclosure);
        self
    }

    /// Request a fully expanded transcript projection.
    pub const fn fully_expanded(mut self) -> Self {
        self.force_expanded = true;
        self
    }

    fn expanded(&self, row: &TranscriptRow) -> bool {
        self.force_expanded
            || self
                .disclosure
                .and_then(|lookup| lookup.expanded(&row.id))
                .unwrap_or(row.default_expanded)
    }

    fn focused(&self, row_id: &TranscriptRowId) -> bool {
        self.disclosure.is_some_and(|lookup| lookup.focused(row_id))
    }
}

/// Semantic interaction attached to a rendered primitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderAction {
    /// Toggle one stable disclosure row.
    ToggleDisclosure { row_id: TranscriptRowId },
}

/// One frontend-neutral rendered line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedLine {
    /// Frontend-ready text without physical cell coordinates.
    pub text: String,
    /// Stable semantic row identity when this line is interactive.
    pub row_id: Option<TranscriptRowId>,
    /// Resolved disclosure state for interactive rows.
    pub row_expanded: Option<bool>,
    /// Semantic interaction associated with this line.
    pub action: Option<RenderAction>,
}

impl RenderedLine {
    /// Create a noninteractive rendered line.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            row_id: None,
            row_expanded: None,
            action: None,
        }
    }
}

/// Complete render result for one stable semantic message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMessage {
    /// Stable identity of the semantic message that rendered these lines.
    pub message_id: MessageId,
    /// Ordered frontend-neutral line primitives.
    pub lines: Vec<RenderedLine>,
}

pub(super) fn render_transcript_tree(
    message_id: MessageId,
    root: &TranscriptRow,
    context: &RenderContext<'_>,
) -> RenderedMessage {
    let mut lines = Vec::new();
    render_row(root, 0, context, &mut lines);
    RenderedMessage { message_id, lines }
}

fn render_row(
    row: &TranscriptRow,
    depth: usize,
    context: &RenderContext<'_>,
    lines: &mut Vec<RenderedLine>,
) {
    let expandable = !row.body.is_empty() || !row.children.is_empty();
    let expanded = expandable && context.expanded(row);
    let marker = if context.capabilities.unicode {
        match (expandable, expanded) {
            (true, true) => "▼",
            (true, false) => "▶",
            (false, _) => "•",
        }
    } else {
        match (expandable, expanded) {
            (true, true) => "v",
            (true, false) => ">",
            (false, _) => "*",
        }
    };
    let state = if expandable {
        if expanded {
            " [expanded]"
        } else {
            " [collapsed]"
        }
    } else {
        ""
    };
    let focus = if context.focused(&row.id) { "> " } else { "  " };
    let action = expandable.then(|| RenderAction::ToggleDisclosure {
        row_id: row.id.clone(),
    });
    lines.push(RenderedLine {
        text: format!(
            "{focus}{}{} {}{}",
            "  ".repeat(depth),
            marker,
            row.label,
            state
        ),
        row_id: expandable.then(|| row.id.clone()),
        row_expanded: expandable.then_some(expanded),
        action,
    });
    if !expanded {
        return;
    }
    for body in &row.body {
        lines.push(RenderedLine::plain(format!(
            "{}  {}",
            "  ".repeat(depth),
            body
        )));
    }
    for child in &row.children {
        render_row(child, depth + 1, context, lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::messages::TranscriptRowKind;

    struct SyntheticDisclosure {
        row_id: TranscriptRowId,
    }

    impl DisclosureLookup for SyntheticDisclosure {
        fn expanded(&self, row_id: &TranscriptRowId) -> Option<bool> {
            (row_id == &self.row_id).then_some(true)
        }

        fn focused(&self, row_id: &TranscriptRowId) -> bool {
            row_id == &self.row_id
        }
    }

    #[test]
    fn synthetic_render_context_controls_disclosure_without_terminal_state() {
        let message_id = MessageId::new();
        let row_id = TranscriptRowId {
            message_id,
            path: vec![0],
        };
        let tree = TranscriptRow {
            id: row_id.clone(),
            kind: TranscriptRowKind::Program,
            label: "Program source (lisp)".into(),
            body: vec!["(say \"hello\")".into()],
            children: Vec::new(),
            default_expanded: false,
        };
        let disclosure = SyntheticDisclosure {
            row_id: row_id.clone(),
        };
        let colors = ColorScheme::default();
        let context = RenderContext::new(&colors, RenderCapabilities::synthetic(40))
            .with_disclosure(&disclosure);

        let rendered = render_transcript_tree(message_id, &tree, &context);

        assert_eq!(rendered.message_id, message_id);
        assert_eq!(rendered.lines.len(), 2);
        assert_eq!(
            rendered.lines[0].text,
            "> v Program source (lisp) [expanded]"
        );
        assert_eq!(rendered.lines[0].row_id.as_ref(), Some(&row_id));
        assert_eq!(rendered.lines[0].row_expanded, Some(true));
        assert_eq!(
            rendered.lines[0].action,
            Some(RenderAction::ToggleDisclosure { row_id })
        );
        assert_eq!(rendered.lines[1].text, "  (say \"hello\")");
    }

    #[test]
    fn render_capabilities_select_unicode_without_changing_semantics() {
        let message_id = MessageId::new();
        let tree = TranscriptRow {
            id: TranscriptRowId {
                message_id,
                path: vec![0],
            },
            kind: TranscriptRowKind::Output,
            label: "Program output".into(),
            body: vec!["hello".into()],
            children: Vec::new(),
            default_expanded: false,
        };
        let colors = ColorScheme::default();
        let synthetic = render_transcript_tree(
            message_id,
            &tree,
            &RenderContext::new(&colors, RenderCapabilities::synthetic(20)),
        );
        let terminal = render_transcript_tree(
            message_id,
            &tree,
            &RenderContext::new(&colors, RenderCapabilities::terminal(20)),
        );

        assert!(synthetic.lines[0].text.contains("> Program output"));
        assert!(terminal.lines[0].text.contains("▶ Program output"));
        assert_eq!(synthetic.lines[0].row_id, terminal.lines[0].row_id);
        assert_eq!(synthetic.lines[0].action, terminal.lines[0].action);
    }
}

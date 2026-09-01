//! Frontend-neutral rendering contracts for retained messages.
//!
//! Messages own their semantic projection and render it against an immutable
//! set of frontend capabilities. A frontend may then resolve the resulting
//! lines into terminal cells, HTML, speech, or another physical representation
//! without matching on the concrete message type.

use super::{MessageDisclosure, MessageId, MessageRef};
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

    /// Plain, speakable transcript capabilities. Disclosure state is emitted
    /// as words because this consumer cannot rely on visual arrow direction.
    pub const fn plain_text(viewport_width: usize) -> Self {
        Self {
            viewport_width,
            color_depth: ColorDepth::Monochrome,
            unicode: false,
            hyperlinks: false,
            frontend: FrontendKind::PlainText,
        }
    }
}

pub(crate) fn normalize_legacy_text(text: &str, capabilities: RenderCapabilities) -> String {
    let text = if capabilities.color_depth == ColorDepth::Monochrome {
        strip_ansi(text)
    } else {
        text.to_string()
    };
    if capabilities.unicode {
        return text;
    }
    text.chars()
        .map(|character| match character {
            '❯' => '>',
            '⏺' | '•' => '*',
            '→' => '>',
            '─' => '-',
            '│' => '|',
            character if character.is_ascii() => character,
            _ => '?',
        })
        .collect()
}

fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' || chars.peek() != Some(&'[') {
            plain.push(character);
            continue;
        }
        chars.next();
        for next in chars.by_ref() {
            if ('@'..='~').contains(&next) {
                break;
            }
        }
    }
    plain
}

/// Frontend-local disclosure and focus state.
///
/// The lookup is deliberately read-only and semantic: messages never receive
/// raw mouse coordinates or query process-global terminal state.
pub trait DisclosureLookup {
    /// Return an explicit disclosure override for this Message, if any.
    fn expanded(&self, message_id: &MessageId) -> Option<bool>;
    /// Whether this Message owns semantic keyboard focus.
    fn focused(&self, message_id: &MessageId) -> bool;
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

    fn expanded(&self, message_id: &MessageId, disclosure: &MessageDisclosure) -> bool {
        self.force_expanded
            || self
                .disclosure
                .and_then(|lookup| lookup.expanded(message_id))
                .unwrap_or(disclosure.default_expanded)
    }

    fn focused(&self, message_id: &MessageId) -> bool {
        self.disclosure
            .is_some_and(|lookup| lookup.focused(message_id))
    }
}

/// Semantic interaction attached to a rendered primitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderAction {
    /// Toggle one stable Message.
    ToggleDisclosure { row_id: MessageId },
}

/// One frontend-neutral rendered line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedLine {
    /// Frontend-ready text without physical cell coordinates.
    pub text: String,
    /// Stable semantic Message identity when this line is interactive.
    pub row_id: Option<MessageId>,
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

pub(super) fn render_message_tree(
    message_id: MessageId,
    disclosure: &MessageDisclosure,
    children: Vec<MessageRef>,
    context: &RenderContext<'_>,
) -> RenderedMessage {
    let mut lines = Vec::new();
    render_message(message_id, disclosure, children, 0, context, &mut lines);
    RenderedMessage { message_id, lines }
}

fn render_message(
    message_id: MessageId,
    disclosure: &MessageDisclosure,
    children: Vec<MessageRef>,
    depth: usize,
    context: &RenderContext<'_>,
    lines: &mut Vec<RenderedLine>,
) {
    let expandable = !disclosure.body.is_empty() || !children.is_empty();
    let expanded = expandable && context.expanded(&message_id, disclosure);
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
    let focus = if context.focused(&message_id) {
        "> "
    } else {
        "  "
    };
    let action = expandable.then(|| RenderAction::ToggleDisclosure { row_id: message_id });
    let accessible_state = (context.capabilities.frontend == FrontendKind::PlainText && expandable)
        .then(|| {
            if expanded {
                " [expanded]"
            } else {
                " [collapsed]"
            }
        })
        .unwrap_or_default();
    lines.push(RenderedLine {
        text: format!(
            "{focus}{}{} {}{}",
            "  ".repeat(depth),
            marker,
            disclosure.label,
            accessible_state,
        ),
        row_id: expandable.then_some(message_id),
        row_expanded: expandable.then_some(expanded),
        action,
    });
    if !expanded {
        return;
    }
    for body in &disclosure.body {
        lines.push(RenderedLine::plain(format!(
            "{}  {}",
            "  ".repeat(depth),
            body
        )));
    }
    for child in children {
        if let Some(child_disclosure) = child.disclosure(context.colors) {
            render_message(
                child.id(),
                &child_disclosure,
                child.children(),
                depth + 1,
                context,
                lines,
            );
        } else {
            for line in child.render(context).lines {
                lines.push(RenderedLine::plain(format!(
                    "{}{}",
                    "  ".repeat(depth + 1),
                    line.text
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::messages::{Message, MessageDisclosure, MessageStatus};
    use std::sync::Arc;

    #[test]
    fn plain_text_disclosure_is_speakable_without_polluting_visual_terminal() {
        let message = crate::cli::messages::WorkUnit::new("Tools");
        message.add_row("read file");
        let colors = ColorScheme::default();
        let plain = message.render(&RenderContext::new(
            &colors,
            RenderCapabilities::plain_text(80),
        ));
        let visual = message.render(&RenderContext::new(
            &colors,
            RenderCapabilities::terminal(80),
        ));
        assert!(plain.lines[0].text.contains("[expanded]"));
        assert!(!visual.lines[0].text.contains("[expanded]"));
        assert!(!visual.lines[0].text.contains("[collapsed]"));
    }

    #[test]
    fn legacy_plain_render_honors_monochrome_ascii_capabilities() {
        let message = crate::cli::messages::UserQueryMessage::new("hello 世界");
        let colors = ColorScheme::default();
        let visual = message.render(&RenderContext::new(
            &colors,
            RenderCapabilities::terminal(80),
        ));
        let plain = message.render(&RenderContext::new(
            &colors,
            RenderCapabilities::plain_text(80),
        ));
        assert!(visual.lines[0].text.contains('\u{1b}'));
        assert!(!plain.lines[0].text.contains('\u{1b}'));
        assert!(plain.lines[0].text.is_ascii());
        assert_eq!(plain.lines[0].text, "hello ??");
    }

    struct SyntheticDisclosure {
        message_id: MessageId,
    }

    impl DisclosureLookup for SyntheticDisclosure {
        fn expanded(&self, message_id: &MessageId) -> Option<bool> {
            (message_id == &self.message_id).then_some(true)
        }

        fn focused(&self, message_id: &MessageId) -> bool {
            message_id == &self.message_id
        }
    }

    struct TestMessage {
        id: MessageId,
        label: &'static str,
        body: Vec<String>,
        children: Vec<MessageRef>,
    }

    impl Message for TestMessage {
        fn id(&self) -> MessageId {
            self.id
        }
        fn format(&self, _colors: &ColorScheme) -> String {
            self.body.join("\n")
        }
        fn status(&self) -> MessageStatus {
            MessageStatus::Complete
        }
        fn content(&self) -> String {
            self.body.join("\n")
        }
        fn children(&self) -> Vec<MessageRef> {
            self.children.clone()
        }
        fn disclosure(&self, _colors: &ColorScheme) -> Option<MessageDisclosure> {
            Some(MessageDisclosure {
                label: self.label.into(),
                body: self.body.clone(),
                default_expanded: false,
            })
        }
    }

    #[test]
    fn synthetic_render_context_controls_disclosure_without_terminal_state() {
        let message_id = MessageId::new();
        let message: MessageRef = Arc::new(TestMessage {
            id: message_id,
            label: "Program source (lisp)".into(),
            body: vec!["(say \"hello\")".into()],
            children: Vec::new(),
        });
        let disclosure = SyntheticDisclosure { message_id };
        let colors = ColorScheme::default();
        let context = RenderContext::new(&colors, RenderCapabilities::synthetic(40))
            .with_disclosure(&disclosure);

        let rendered = message.render(&context);

        assert_eq!(rendered.message_id, message_id);
        assert_eq!(rendered.lines.len(), 2);
        assert_eq!(rendered.lines[0].text, "> v Program source (lisp)");
        assert_eq!(rendered.lines[0].row_id, Some(message_id));
        assert_eq!(rendered.lines[0].row_expanded, Some(true));
        assert_eq!(
            rendered.lines[0].action,
            Some(RenderAction::ToggleDisclosure { row_id: message_id })
        );
        assert_eq!(rendered.lines[1].text, "  (say \"hello\")");
    }

    #[test]
    fn render_capabilities_select_unicode_without_changing_semantics() {
        let message_id = MessageId::new();
        let message: MessageRef = Arc::new(TestMessage {
            id: message_id,
            label: "Program output".into(),
            body: vec!["hello".into()],
            children: Vec::new(),
        });
        let colors = ColorScheme::default();
        let synthetic = message.render(&RenderContext::new(
            &colors,
            RenderCapabilities::synthetic(20),
        ));
        let terminal = message.render(&RenderContext::new(
            &colors,
            RenderCapabilities::terminal(20),
        ));

        assert!(synthetic.lines[0].text.contains("> Program output"));
        assert!(terminal.lines[0].text.contains("▶ Program output"));
        assert_eq!(synthetic.lines[0].row_id, terminal.lines[0].row_id);
        assert_eq!(synthetic.lines[0].action, terminal.lines[0].action);
    }
}

//! The generalized component view: every migrated typed message yields its
//! component through the `Message` trait (stage 3 of docs/TUI_DESIGN.md,
//! #1120).
//!
//! The maintainer's model (docs/TUI_DESIGN.md): each component maintains a
//! ViewModel — a retained, plain-data struct living on the message behind its
//! existing lock discipline — plus a chrome renderer and subwidgets
//! constructed from the outer ViewModel each frame, so a subwidget with
//! nothing to show claims zero rows and stays in the tree. The message
//! constructs its component snapshot; the renderer never matches on the
//! message type: it asks the trait for the snapshot and hands it to
//! [`component_lines`]. The per-variant dispatch is component semantics and
//! lives here in the presentation capsule — never in the engine.
//!
//! Components render plain text: glyphs carry the semantics (⏺ ⎿ ✓ ✗ █ ░),
//! and the style-spans migration (stage 4) replaces the retired
//! `format()`-era SGR bytes. This matches the say-turn template (#882) and
//! this crate's dependency contract (no `finch-theme`).

use crate::{say_turn::SayTurnView, RenderedTranscriptLine};

/// The component snapshot of one migrated typed message. A message type
/// constructs the variant that belongs to it from its retained ViewModel;
/// `None`-returning rows have not migrated and keep the legacy projection.
#[derive(Clone, Debug)]
pub enum ComponentView {
    /// A component-owned say turn (#882, stages 1–2). Migrated to this
    /// accessor in stage 3 (#1120): the say component rides the same
    /// generalized hook, and the `Message::say_turn_view` hook stays for the
    /// consolidated-source pairing helper and the disclosure-direction read.
    Say(SayTurnView),
    /// A static text message (#1120, stage 3): the text IS its view.
    StaticText(StaticTextView),
}

/// The ViewModel of a [`ComponentView::StaticText`] component: the message's
/// immutable content and the kind that decides its glyph. A `StaticMessage`
/// has no mutable state to retain — the content itself is the ViewModel —
/// and the message constructs the snapshot from its own immutable fields; no
/// lock is involved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticTextView {
    pub kind: StaticTextKind,
    pub content_lines: Vec<String>,
}

/// Which glyph a static text row wears. Byte-compatible with the retired
/// `format()` presentation: `Plain` passes the content through with no
/// prefix, so pre-formatted text reaches the record byte-exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaticTextKind {
    Info,
    Error,
    Success,
    Warning,
    Plain,
}

/// Render one component snapshot into the transcript lines it claims this
/// frame. The engine asks the `Message` trait for the snapshot and this
/// function for the lines; it never learns which concrete message produced
/// either.
pub fn component_lines(view: &ComponentView) -> Vec<RenderedTranscriptLine> {
    match view {
        ComponentView::Say(say) => crate::say_turn_lines(say),
        ComponentView::StaticText(static_text) => static_text_lines(static_text),
    }
}

/// A static text row's lines: one glyph-prefixed line per content line. An
/// empty content renders one empty line so the row still claims its place in
/// the transcript.
fn static_text_lines(view: &StaticTextView) -> Vec<RenderedTranscriptLine> {
    let prefix = match view.kind {
        StaticTextKind::Info => "ℹ️  ",
        StaticTextKind::Error => "❌ ",
        StaticTextKind::Success => "✓ ",
        StaticTextKind::Warning => "⚠️  ",
        StaticTextKind::Plain => "",
    };
    let lines = if view.content_lines.is_empty() {
        vec![String::new()]
    } else {
        view.content_lines.clone()
    };
    lines
        .into_iter()
        .map(|line| RenderedTranscriptLine {
            text: format!("{prefix}{line}"),
            ..RenderedTranscriptLine::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Axis, MessageId, Rect, Track, Widget};

    /// The say component rides the generalized accessor end to end: a say
    /// snapshot handed to `component_lines` renders exactly what
    /// `say_turn_lines` renders, and the engine needed no type information.
    #[test]
    fn test_say_view_renders_through_the_generalized_dispatch() {
        use crate::say_turn::{OutputVm, ProgramSourceVm, SayTurnStatus, WorkUnitViewModel};
        let view = SayTurnView {
            message_id: MessageId::new(),
            vm: WorkUnitViewModel {
                status: SayTurnStatus::Completed,
                program: ProgramSourceVm {
                    language: "Co-Forth".into(),
                    lines: vec!["(say \"hello\")".to_string()],
                },
                output: Some(OutputVm {
                    lines: vec!["hello".to_string()],
                }),
                show_program: false,
            },
            elapsed: std::time::Duration::from_millis(2350),
        };
        let via_component = component_lines(&ComponentView::Say(view.clone()));
        let direct = crate::say_turn_lines(&view);
        assert_eq!(
            via_component, direct,
            "the generalized dispatch must not change the say component's output"
        );
        let texts: Vec<String> = via_component.iter().map(|line| line.text.clone()).collect();
        assert_eq!(
            texts,
            vec!["hello", "", "(ran 2s)"],
            "the say card renders prose, a blank separator, and the elapsed annotation \
             through the accessor; got {texts:?}"
        );
    }

    /// The generalized dispatch participates in a claiming frame as a subtree,
    /// exactly like the component it forwards to.
    #[test]
    fn test_component_lines_claim_rows_in_a_layout_frame() {
        use crate::say_turn::{OutputVm, ProgramSourceVm, SayTurnStatus, WorkUnitViewModel};
        let view = SayTurnView {
            message_id: MessageId::new(),
            vm: WorkUnitViewModel {
                status: SayTurnStatus::Completed,
                program: ProgramSourceVm {
                    language: "Co-Forth".into(),
                    lines: vec!["(say \"hello\")".to_string()],
                },
                output: Some(OutputVm {
                    lines: vec!["hello".to_string()],
                }),
                show_program: false,
            },
            elapsed: std::time::Duration::from_millis(2350),
        };
        const CARD: u16 = 9;
        let tree = Widget::Stack {
            axis: Axis::Column,
            children: vec![(
                Track::Natural,
                Widget::Marked(
                    CARD,
                    Box::new(Widget::Text {
                        lines: component_lines(&ComponentView::Say(view))
                            .into_iter()
                            .map(|line| line.text)
                            .collect(),
                    }),
                ),
            )],
        };
        let layout = crate::layout(
            &tree,
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 20,
            },
        );
        let rect = layout.keyed(CARD).expect("the card claims a rect");
        assert_eq!(
            rect.height, 3,
            "prose + blank + annotation claim three rows; got {rect:?}"
        );
    }

    // ── StaticMessage: text is its view (#1120 stage 3) ─────────────────────

    fn static_view(kind: StaticTextKind, content: &str) -> StaticTextView {
        StaticTextView {
            kind,
            content_lines: content.lines().map(str::to_owned).collect(),
        }
    }

    fn static_texts(kind: StaticTextKind, content: &str) -> Vec<String> {
        component_lines(&ComponentView::StaticText(static_view(kind, content)))
            .into_iter()
            .map(|line| line.text)
            .collect()
    }

    /// INVARIANT (#1120): a StaticMessage's text IS its view — the component
    /// renders one glyph-prefixed line per content line, byte-compatible with
    /// the retired `format()` presentation.
    #[test]
    fn test_static_text_renders_one_glyph_prefixed_line_per_content_line() {
        assert_eq!(
            static_texts(StaticTextKind::Error, "Provider unreachable"),
            vec!["❌ Provider unreachable"],
            "an error row wears the error glyph"
        );
        assert_eq!(
            static_texts(StaticTextKind::Info, "line one\nline two"),
            vec!["ℹ️  line one", "ℹ️  line two"],
            "each content line carries the prefix"
        );
        assert_eq!(
            static_texts(StaticTextKind::Success, "done"),
            vec!["✓ done"],
            "a success row wears the check glyph"
        );
        assert_eq!(
            static_texts(StaticTextKind::Warning, "careful"),
            vec!["⚠️  careful"],
            "a warning row wears the warning glyph"
        );
    }

    /// INVARIANT: `Plain` passes pre-formatted content through with no prefix
    /// and no SGR, so text that already carries its own presentation reaches
    /// the record byte-exactly.
    #[test]
    fn test_static_plain_text_is_byte_exact_passthrough() {
        let content = "\x1b[32m[tool] styled output\x1b[0m\nsecond line";
        assert_eq!(
            static_texts(StaticTextKind::Plain, content),
            content.lines().map(str::to_owned).collect::<Vec<_>>(),
            "Plain content is the view itself: no glyph, no prefix, no mutation"
        );
    }

    /// A subwidget with nothing to show still claims one empty row (the
    /// furniture rule for a static row is one row minimum), and the snapshot
    /// is a plain-data value the message can hand out repeatedly.
    #[test]
    fn test_static_text_empty_content_claims_one_row_and_snapshot_is_plain_data() {
        let empty = static_view(StaticTextKind::Plain, "");
        assert_eq!(
            static_texts(StaticTextKind::Plain, ""),
            vec![String::new()],
            "empty content renders one empty line so the row claims its place"
        );
        let first = component_lines(&ComponentView::StaticText(empty.clone()));
        let second = component_lines(&ComponentView::StaticText(empty));
        assert_eq!(
            first, second,
            "rendering is a pure function of the snapshot"
        );
        let _layout = crate::layout(
            &Widget::Text {
                lines: first.into_iter().map(|line| line.text).collect(),
            },
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 4,
            },
        );
    }
}

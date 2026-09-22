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

use crate::{say_turn::SayTurnView, work_unit::MessageStatus, RenderedTranscriptLine};

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
    /// A download/upload progress message (#1120, stage 3).
    Progress(ProgressView),
    /// A live tool call message (#1120, stage 3): header plus streaming
    /// content and status.
    LiveTool(LiveToolView),
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

/// The ViewModel of a [`ComponentView::Progress`] component: the label, the
/// current/total byte counts, and the message status. The message constructs
/// the snapshot under its existing locks; `current` is the live state the
/// component re-renders every frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgressView {
    pub label: String,
    pub current: u64,
    pub total: u64,
    pub status: MessageStatus,
}

/// The ViewModel of a [`ComponentView::LiveTool`] component: the pre-formatted
/// header, the accumulated content lines, and the status. The message
/// constructs the snapshot under its existing lock; `content_lines` is the
/// live state that grows as the tool streams, and every frame re-renders
/// from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveToolView {
    pub header: String,
    pub content_lines: Vec<String>,
    pub status: MessageStatus,
}

/// Render one component snapshot into the transcript lines it claims this
/// frame. The engine asks the `Message` trait for the snapshot and this
/// function for the lines; it never learns which concrete message produced
/// either.
pub fn component_lines(view: &ComponentView) -> Vec<RenderedTranscriptLine> {
    match view {
        ComponentView::Say(say) => crate::say_turn_lines(say),
        ComponentView::StaticText(static_text) => static_text_lines(static_text),
        ComponentView::Progress(progress) => progress_lines(progress),
        ComponentView::LiveTool(live_tool) => live_tool_lines(live_tool),
    }
}

/// Progress bar furniture constants: a ten-cell bar.
const PROGRESS_BAR_CELLS: usize = 10;

/// One progress line: `label [████████░░] N%` with a status glyph —
/// `✓` when complete, `✗` when failed, nothing while running. The VM's
/// numbers decide the rendering; the component is a pure function of the
/// snapshot.
fn progress_lines(view: &ProgressView) -> Vec<RenderedTranscriptLine> {
    let percentage = if view.total > 0 {
        (view.current as f64 / view.total as f64 * 100.0) as u8
    } else {
        0
    };
    let filled = (percentage as usize / 10).min(PROGRESS_BAR_CELLS);
    let bar = format!(
        "[{}{}]",
        "█".repeat(filled),
        "░".repeat(PROGRESS_BAR_CELLS - filled)
    );
    let line = match view.status {
        MessageStatus::Complete => {
            format!("{} {bar} 100% ✓", view.label)
        }
        MessageStatus::Failed => format!("{} {bar} {percentage}% ✗", view.label),
        MessageStatus::InProgress => format!("{} {bar} {percentage}%", view.label),
    };
    vec![RenderedTranscriptLine {
        text: line,
        ..RenderedTranscriptLine::default()
    }]
}

/// A live tool row: the header, then the streaming content beneath it. The
/// subwidgets are constructed from the outer VM each frame: an empty content
/// subwidget claims nothing, so a just-started call renders the header with
/// a trailing `…` (Claude Code style, byte-compatible with the retired
/// `format()` presentation), and grown content renders beneath the header
/// with no trailing ellipsis.
fn live_tool_lines(view: &LiveToolView) -> Vec<RenderedTranscriptLine> {
    let mut lines = Vec::with_capacity(1 + view.content_lines.len());
    let mut header = view.header.clone();
    if view.content_lines.is_empty() && view.status == MessageStatus::InProgress {
        header.push_str("…");
    }
    lines.push(RenderedTranscriptLine {
        text: header,
        ..RenderedTranscriptLine::default()
    });
    lines.extend(
        LiveToolContent::from_vm(view)
            .render()
            .into_iter()
            .map(|text| RenderedTranscriptLine {
                text,
                ..RenderedTranscriptLine::default()
            }),
    );
    lines
}

/// The content subwidget: constructed from the outer ViewModel each frame, so
/// it chooses to render or not based on VM data — a subwidget with nothing to
/// show claims zero rows and stays in the tree (#882's furniture rule).
struct LiveToolContent<'a> {
    lines: &'a [String],
}

impl<'a> LiveToolContent<'a> {
    fn from_vm(vm: &'a LiveToolView) -> Self {
        Self {
            lines: &vm.content_lines,
        }
    }

    fn render(&self) -> Vec<String> {
        self.lines.to_vec()
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

    // ── ProgressMessage: the bar from the VM (#1120 stage 3) ────────────────

    fn progress_view(current: u64, total: u64, status: MessageStatus) -> ProgressView {
        ProgressView {
            label: "Download".to_string(),
            current,
            total,
            status,
        }
    }

    fn progress_text(view: &ProgressView) -> String {
        component_lines(&ComponentView::Progress(view.clone()))
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// INVARIANT (#1120): the bar is a pure function of the VM's numbers —
    /// filled cells track the percentage, the remainder stays empty.
    #[test]
    fn test_progress_bar_tracks_the_vm_numbers() {
        let start = progress_view(0, 100, MessageStatus::InProgress);
        assert_eq!(
            progress_text(&start),
            "Download [░░░░░░░░░░] 0%",
            "zero progress renders an all-empty bar"
        );
        let half = progress_view(50, 100, MessageStatus::InProgress);
        assert_eq!(
            progress_text(&half),
            "Download [█████░░░░░] 50%",
            "half progress fills half the bar"
        );
        let almost = progress_view(85, 100, MessageStatus::InProgress);
        assert_eq!(
            progress_text(&almost),
            "Download [████████░░] 85%",
            "85% fills eight cells"
        );
    }

    /// The status decides the glyph and the completed line: `✓` with a fixed
    /// 100% readout when complete, `✗` with the reached percentage when
    /// failed, nothing while running.
    #[test]
    fn test_progress_status_decides_the_glyph() {
        let running = progress_view(40, 100, MessageStatus::InProgress);
        let running_line = progress_text(&running);
        assert!(
            !running_line.contains('✓') && !running_line.contains('✗'),
            "a running bar wears no terminal glyph; got {running_line:?}"
        );
        let complete = progress_view(100, 100, MessageStatus::Complete);
        assert_eq!(
            progress_text(&complete),
            "Download [██████████] 100% ✓",
            "a complete bar reads 100% with the check glyph"
        );
        let failed = progress_view(30, 100, MessageStatus::Failed);
        assert_eq!(
            progress_text(&failed),
            "Download [███░░░░░░░] 30% ✗",
            "a failed bar keeps the reached percentage with the cross glyph"
        );
    }

    /// The component stays total over degenerate VMs: a zero total cannot
    /// divide by zero, and the bar stays empty.
    #[test]
    fn test_progress_zero_total_renders_an_empty_bar_without_panicking() {
        let indeterminate = progress_view(5, 0, MessageStatus::InProgress);
        assert_eq!(
            progress_text(&indeterminate),
            "Download [░░░░░░░░░░] 0%",
            "a zero total cannot divide by zero; the bar stays empty"
        );
    }

    // ── LiveToolMessage: header + streaming content (#1120 stage 3) ─────────

    fn live_tool_view(header: &str, content_lines: &[&str], status: MessageStatus) -> LiveToolView {
        LiveToolView {
            header: header.to_string(),
            content_lines: content_lines.iter().map(|line| line.to_string()).collect(),
            status,
        }
    }

    fn live_tool_texts(view: &LiveToolView) -> Vec<String> {
        component_lines(&ComponentView::LiveTool(view.clone()))
            .into_iter()
            .map(|line| line.text)
            .collect()
    }

    /// INVARIANT (#1120): a just-started call renders the header with a
    /// trailing `…` — the empty content subwidget claims zero rows and stays
    /// constructible from the outer VM.
    #[test]
    fn test_live_tool_empty_content_claims_zero_rows_and_the_header_carries_the_ellipsis() {
        let started = live_tool_view("⏺ bash(echo hi)", &[], MessageStatus::InProgress);
        assert_eq!(
            live_tool_texts(&started),
            vec!["⏺ bash(echo hi)…"],
            "the header is the whole surface while content is empty"
        );
        let hidden = LiveToolContent::from_vm(&started);
        assert!(
            hidden.render().is_empty(),
            "the empty content subwidget renders nothing"
        );
    }

    /// INVARIANT: arrived content renders beneath the header and is never
    /// hidden; growth between frames renders the new lines, and the
    /// completed state drops the running ellipsis.
    #[test]
    fn test_live_tool_content_renders_beneath_the_header_and_grows() {
        let partial = live_tool_view("⏺ bash(build)", &["compiling…"], MessageStatus::InProgress);
        assert_eq!(
            live_tool_texts(&partial),
            vec!["⏺ bash(build)", "compiling…"],
            "arrived content renders beneath the header without the ellipsis"
        );
        let grown = live_tool_view(
            "⏺ bash(build)",
            &["compiling…", "linking…"],
            MessageStatus::Complete,
        );
        let grown_lines = live_tool_texts(&grown);
        assert_eq!(
            grown_lines,
            vec!["⏺ bash(build)", "compiling…", "linking…"],
            "growth between frames renders the new lines; got {grown_lines:?}"
        );
        assert!(
            grown_lines[0].ends_with("bash(build)"),
            "a completed call wears no running ellipsis"
        );
    }

    /// A completed call with no output is the header alone; a failed call
    /// keeps its diagnostic visible.
    #[test]
    fn test_live_tool_completed_header_only_and_failed_keeps_diagnostics() {
        let silent = live_tool_view("⏺ bash(true)", &[], MessageStatus::Complete);
        assert_eq!(
            live_tool_texts(&silent),
            vec!["⏺ bash(true)"],
            "a completed call with no output is the header alone"
        );
        let failed = live_tool_view("⏺ bash(bad)", &["command not found"], MessageStatus::Failed);
        assert_eq!(
            live_tool_texts(&failed),
            vec!["⏺ bash(bad)", "command not found"],
            "the failure diagnostic stays visible"
        );
    }

    /// The card participates in a claiming frame: one row for the header plus
    /// one row per content line.
    #[test]
    fn test_live_tool_claims_one_row_per_line_in_a_layout_frame() {
        let view = live_tool_view("⏺ bash(build)", &["a", "b"], MessageStatus::InProgress);
        const CARD: u16 = 11;
        let tree = Widget::Stack {
            axis: Axis::Column,
            children: vec![(
                Track::Natural,
                Widget::Marked(
                    CARD,
                    Box::new(Widget::Text {
                        lines: component_lines(&ComponentView::LiveTool(view))
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
            "header + two content lines claim three rows; got {rect:?}"
        );
    }
}

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
//! Stage 4 (#1141): components render **styled spans**, never SGR bytes. The
//! styles come from the [`ComponentStylePalette`] the engine injects — built
//! from the user's `ColorScheme` where the scheme owns a role, and from the
//! retained glyph vocabulary where the pre-migration presentation was fixed.
//! The glyphs still carry the semantics (⏺ ⎿ ✓ ✗ █ ░); the palette only says
//! how they render. A scan regression pins that no escape sequence is
//! constructed anywhere in this file.

use crate::{
    say_turn::SayTurnView,
    span::{Span, SpanColor, SpanStyle},
    work_unit::{MessageStatus, WorkRowStatus},
    RenderedTranscriptLine,
};

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
    /// A grouped tool-call operation message (#1120, stage 3): chrome plus a
    /// row list with per-row status glyphs.
    Operation(OperationView),
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

/// The ViewModel of a [`ComponentView::Operation`] component: the chrome
/// header, the whole-operation status, and one row per tool call with its
/// status. The message constructs the snapshot under its existing locks;
/// `rows` is the live state that grows and transitions as calls run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationView {
    pub header: String,
    pub status: MessageStatus,
    pub rows: Vec<OperationRowView>,
}

/// One tool-call row of an [`OperationView`], with the shared row-status
/// vocabulary: the per-row glyph is a pure function of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationRowView {
    pub label: String,
    pub status: WorkRowStatus,
}

/// The style roles the component renderers read (stage 4, #1141).
///
/// Plain data, terminal-independent: the engine builds it from the user's
/// `ColorScheme` for the scheme-owned roles and keeps the retained glyph
/// vocabulary for the rest. The `Default` value is the pre-migration
/// presentation — the exact colours the retired `format()` paths painted — so
/// the migrated surfaces "regain" their styling and the default stays honest
/// without a scheme at hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentStylePalette {
    /// Progress line while running — pre-migration: `colors.status.operation`.
    pub progress_running: SpanStyle,
    /// Progress line when complete — pre-migration: `colors.status.download`.
    pub progress_complete: SpanStyle,
    /// Progress line when failed — pre-migration: `colors.messages.error`.
    pub progress_failed: SpanStyle,
    /// The operation chrome glyph `⏺` — retained fixed cyan.
    pub operation_glyph: SpanStyle,
    /// The operation row glyph `⎿` — retained fixed dark grey.
    pub operation_row_glyph: SpanStyle,
    /// Operation summaries and running ellipses — dark grey + dim.
    pub operation_summary: SpanStyle,
    /// Operation row `error:` prefix — retained fixed red.
    pub operation_error: SpanStyle,
    /// The live-tool running ellipsis — dark grey + dim.
    pub live_tool_ellipsis: SpanStyle,
    /// Static `ℹ️`/`✓` rows — pre-migration: `colors.messages.system`.
    pub static_info: SpanStyle,
    /// Static `❌` rows — pre-migration: `colors.messages.error`.
    pub static_error: SpanStyle,
    /// Static `✓` rows — pre-migration: `colors.messages.system`.
    pub static_success: SpanStyle,
    /// Static `⚠️` rows — pre-migration: `colors.status.operation`.
    pub static_warning: SpanStyle,
}

impl Default for ComponentStylePalette {
    fn default() -> Self {
        Self {
            progress_running: SpanStyle::fg(SpanColor::DARK_YELLOW),
            progress_complete: SpanStyle::fg(SpanColor::DARK_CYAN),
            progress_failed: SpanStyle::fg(SpanColor::DARK_RED),
            operation_glyph: SpanStyle::fg(SpanColor::CYAN),
            operation_row_glyph: SpanStyle::fg(SpanColor::DARK_GREY),
            operation_summary: SpanStyle::fg(SpanColor::DARK_GREY).with_dim(true),
            operation_error: SpanStyle::fg(SpanColor::RED),
            live_tool_ellipsis: SpanStyle::fg(SpanColor::DARK_GREY).with_dim(true),
            static_info: SpanStyle::fg(SpanColor::DARK_GREY),
            static_error: SpanStyle::fg(SpanColor::DARK_RED),
            static_success: SpanStyle::fg(SpanColor::DARK_GREY),
            static_warning: SpanStyle::fg(SpanColor::DARK_YELLOW),
        }
    }
}

/// Render one component snapshot into the transcript lines it claims this
/// frame. The engine asks the `Message` trait for the snapshot and this
/// function for the lines; it never learns which concrete message produced
/// either. The palette carries the styling; nothing here touches terminal
/// bytes.
pub fn component_lines(
    view: &ComponentView,
    palette: &ComponentStylePalette,
) -> Vec<RenderedTranscriptLine> {
    match view {
        ComponentView::Say(say) => crate::say_turn_lines(say),
        ComponentView::StaticText(static_text) => static_text_lines(static_text, palette),
        ComponentView::Progress(progress) => progress_lines(progress, palette),
        ComponentView::LiveTool(live_tool) => live_tool_lines(live_tool, palette),
        ComponentView::Operation(operation) => operation_lines(operation, palette),
    }
}

/// Progress bar furniture constants: a ten-cell bar.
const PROGRESS_BAR_CELLS: usize = 10;

/// One progress line: `label [████████░░] N%` with a status glyph —
/// `✓` when complete, `✗` when failed, nothing while running. The VM's
/// numbers decide the rendering; the component is a pure function of the
/// snapshot, and the whole line wears the palette's status colour — the
/// retired `format()` path coloured the label, bar, and readout alike.
fn progress_lines(
    view: &ProgressView,
    palette: &ComponentStylePalette,
) -> Vec<RenderedTranscriptLine> {
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
    let text = match view.status {
        MessageStatus::Complete => {
            format!("{} {bar} 100% ✓", view.label)
        }
        MessageStatus::Failed => format!("{} {bar} {percentage}% ✗", view.label),
        MessageStatus::InProgress => format!("{} {bar} {percentage}%", view.label),
    };
    let style = match view.status {
        MessageStatus::Complete => palette.progress_complete,
        MessageStatus::Failed => palette.progress_failed,
        MessageStatus::InProgress => palette.progress_running,
    };
    vec![RenderedTranscriptLine::from_spans(vec![Span::styled(
        text, style,
    )])]
}

/// A live tool row: the header, then the streaming content beneath it. The
/// subwidgets are constructed from the outer VM each frame: an empty content
/// subwidget claims nothing, so a just-started call renders the header with
/// a trailing `…` (Claude Code style, byte-compatible with the retired
/// `format()` presentation) — the running ellipsis wears the dimmed style the
/// old painter used, the header and content stay plain.
fn live_tool_lines(
    view: &LiveToolView,
    palette: &ComponentStylePalette,
) -> Vec<RenderedTranscriptLine> {
    let mut lines = Vec::with_capacity(1 + view.content_lines.len());
    let mut header_spans = vec![Span::plain(view.header.clone())];
    if view.content_lines.is_empty() && view.status == MessageStatus::InProgress {
        header_spans.push(Span::styled("\u{2026}", palette.live_tool_ellipsis));
    }
    lines.push(RenderedTranscriptLine::from_spans(header_spans));
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

/// An operation row: chrome (`⏺ header…`) then the row list (`⎿ label …`
/// per call). The rows subwidget is constructed from the outer ViewModel
/// each frame, so it chooses to render or not based on VM data — an
/// operation with no rows yet claims only its chrome. Per-row glyphs are a
/// pure function of the row status: `…` while running, the summary when
/// complete, `error:` when failed (byte-compatible with the retired
/// `format()` presentation). Styling is the pre-migration one: the chrome
/// glyph cyan, the row glyph dark grey, summaries and running ellipses
/// dimmed, the `error:` prefix red.
fn operation_lines(
    view: &OperationView,
    palette: &ComponentStylePalette,
) -> Vec<RenderedTranscriptLine> {
    let mut lines = Vec::with_capacity(1 + view.rows.len());
    let mut chrome_spans = vec![
        Span::styled("\u{23fa}", palette.operation_glyph),
        Span::plain(format!(" {}", view.header)),
    ];
    if view.status == MessageStatus::InProgress {
        chrome_spans.push(Span::plain("\u{2026}"));
    }
    lines.push(RenderedTranscriptLine::from_spans(chrome_spans));
    lines.extend(
        OperationRows::from_vm(view)
            .render(palette)
            .into_iter()
            .map(RenderedTranscriptLine::from_spans),
    );
    lines
}

/// The rows subwidget: constructed from the outer ViewModel each frame; a
/// subwidget with nothing to show claims zero rows and stays in the tree
/// (#882's furniture rule).
struct OperationRows<'a> {
    rows: &'a [OperationRowView],
}

impl<'a> OperationRows<'a> {
    fn from_vm(vm: &'a OperationView) -> Self {
        Self { rows: &vm.rows }
    }

    fn render(&self, palette: &ComponentStylePalette) -> Vec<Vec<Span>> {
        self.rows
            .iter()
            .map(|row| {
                // The shared furniture: two spaces, the dark-grey glyph, then
                // the label; only the status suffix differs per state.
                let mut spans = vec![
                    Span::plain("  "),
                    Span::styled("\u{23bf}", palette.operation_row_glyph),
                    Span::plain(format!(" {}", row.label)),
                ];
                match &row.status {
                    WorkRowStatus::Running => {
                        spans.push(Span::styled("\u{2026}", palette.operation_summary));
                    }
                    WorkRowStatus::Complete(summary) if summary.is_empty() => {}
                    WorkRowStatus::Complete(summary) => {
                        spans.push(Span::plain(" "));
                        spans.push(Span::styled(summary.clone(), palette.operation_summary));
                    }
                    WorkRowStatus::Error(error) => {
                        spans.push(Span::plain(" "));
                        spans.push(Span::styled("error:", palette.operation_error));
                        spans.push(Span::plain(format!(" {error}")));
                    }
                }
                spans
            })
            .collect()
    }
}

/// A static text row's lines: one glyph-prefixed line per content line. An
/// empty content renders one empty line so the row still claims its place in
/// the transcript. The whole line wears the palette's kind colour — the
/// retired `format()` path coloured glyph and content alike; `Plain`
/// passes pre-formatted content through untouched (no prefix, no spans).
fn static_text_lines(
    view: &StaticTextView,
    palette: &ComponentStylePalette,
) -> Vec<RenderedTranscriptLine> {
    let (prefix, style) = match view.kind {
        StaticTextKind::Info => ("ℹ️  ", palette.static_info),
        StaticTextKind::Error => ("❌ ", palette.static_error),
        StaticTextKind::Success => ("✓ ", palette.static_success),
        StaticTextKind::Warning => ("⚠️  ", palette.static_warning),
        StaticTextKind::Plain => ("", SpanStyle::PLAIN),
    };
    let lines = if view.content_lines.is_empty() {
        vec![String::new()]
    } else {
        view.content_lines.clone()
    };
    lines
        .into_iter()
        .map(|line| {
            if style.is_plain() {
                RenderedTranscriptLine {
                    text: format!("{prefix}{line}"),
                    ..RenderedTranscriptLine::default()
                }
            } else {
                RenderedTranscriptLine::from_spans(vec![Span::styled(
                    format!("{prefix}{line}"),
                    style,
                )])
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Axis, MessageId, Rect, Track, Widget};

    const PALETTE: ComponentStylePalette = ComponentStylePalette {
        progress_running: SpanStyle::fg(SpanColor::DARK_YELLOW),
        progress_complete: SpanStyle::fg(SpanColor::DARK_CYAN),
        progress_failed: SpanStyle::fg(SpanColor::DARK_RED),
        operation_glyph: SpanStyle::fg(SpanColor::CYAN),
        operation_row_glyph: SpanStyle::fg(SpanColor::DARK_GREY),
        operation_summary: SpanStyle::fg(SpanColor::DARK_GREY).with_dim(true),
        operation_error: SpanStyle::fg(SpanColor::RED),
        live_tool_ellipsis: SpanStyle::fg(SpanColor::DARK_GREY).with_dim(true),
        static_info: SpanStyle::fg(SpanColor::DARK_GREY),
        static_error: SpanStyle::fg(SpanColor::DARK_RED),
        static_success: SpanStyle::fg(SpanColor::DARK_GREY),
        static_warning: SpanStyle::fg(SpanColor::DARK_YELLOW),
    };

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
        let via_component = component_lines(&ComponentView::Say(view.clone()), &PALETTE);
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
                        lines: component_lines(&ComponentView::Say(view), &PALETTE)
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
        component_lines(
            &ComponentView::StaticText(static_view(kind, content)),
            &PALETTE,
        )
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
        let first = component_lines(&ComponentView::StaticText(empty.clone()), &PALETTE);
        let second = component_lines(&ComponentView::StaticText(empty), &PALETTE);
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
        component_lines(&ComponentView::Progress(view.clone()), &PALETTE)
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
        component_lines(&ComponentView::LiveTool(view.clone()), &PALETTE)
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
                        lines: component_lines(&ComponentView::LiveTool(view), &PALETTE)
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

    // ── OperationMessage: chrome + per-row glyphs (#1120 stage 3) ───────────

    fn operation_view(
        header: &str,
        status: MessageStatus,
        rows: &[(&str, WorkRowStatus)],
    ) -> OperationView {
        OperationView {
            header: header.to_string(),
            status,
            rows: rows
                .iter()
                .map(|(label, status)| OperationRowView {
                    label: label.to_string(),
                    status: status.clone(),
                })
                .collect(),
        }
    }

    fn operation_texts(view: &OperationView) -> Vec<String> {
        component_lines(&ComponentView::Operation(view.clone()), &PALETTE)
            .into_iter()
            .map(|line| line.text)
            .collect()
    }

    /// INVARIANT (#1120): each row renders its status glyph from the VM —
    /// `…` while running, the summary when complete (bare label when the
    /// summary is empty), `error:` when failed — and the whole-operation
    /// chrome carries its running ellipsis only while the operation is
    /// still in progress.
    #[test]
    fn test_operation_rows_render_status_glyphs_from_the_vm() {
        let running = operation_view(
            "Generating",
            MessageStatus::InProgress,
            &[
                ("bash(git push)", WorkRowStatus::Running),
                (
                    "read(src/foo.rs)",
                    WorkRowStatus::Complete("45 lines".into()),
                ),
                ("read(src/bar.rs)", WorkRowStatus::Complete(String::new())),
                (
                    "bash(bad)",
                    WorkRowStatus::Error("permission denied".into()),
                ),
            ],
        );
        assert_eq!(
            operation_texts(&running),
            vec![
                "⏺ Generating…",
                "  ⎿ bash(git push)…",
                "  ⎿ read(src/foo.rs) 45 lines",
                "  ⎿ read(src/bar.rs)",
                "  ⎿ bash(bad) error: permission denied",
            ],
            "chrome plus one line per row with the per-row glyph; got {:?}",
            operation_texts(&running)
        );
        let complete = operation_view("Generating", MessageStatus::Complete, &[]);
        let complete_texts = operation_texts(&complete);
        assert_eq!(
            complete_texts,
            vec!["⏺ Generating"],
            "a completed operation drops the chrome ellipsis"
        );
        assert!(
            complete_texts
                .iter()
                .all(|line| !line.contains('\u{23fa}') || !line.ends_with('…')),
            "no completed line wears a running ellipsis; got {complete_texts:?}"
        );
    }

    /// The rows subwidget is constructed from the outer VM each frame: an
    /// operation with no rows yet claims only its chrome, and the glyph
    /// vocabulary stays the pinned ⏺/⎿ pair (U+23FA / U+23BF), never the
    /// legacy ●/└ pair.
    #[test]
    fn test_operation_rows_subwidget_zero_claims_and_glyph_vocabulary_is_pinned() {
        let empty = operation_view("Generating", MessageStatus::InProgress, &[]);
        let empty_rows = OperationRows::from_vm(&empty);
        assert!(
            empty_rows.render(&PALETTE).is_empty(),
            "the empty rows subwidget renders nothing and stays constructible"
        );
        assert_eq!(
            operation_texts(&empty),
            vec!["⏺ Generating…"],
            "chrome alone claims one row while no call has started"
        );
        let dumped = operation_texts(&operation_view(
            "Generating",
            MessageStatus::Complete,
            &[("bash(ls)", WorkRowStatus::Complete(String::new()))],
        ))
        .join("\n");
        assert!(
            dumped.contains('\u{23fa}') && dumped.contains('\u{23bf}'),
            "the pinned glyph vocabulary renders; got {dumped:?}"
        );
        assert!(
            !dumped.contains('\u{25cf}') && !dumped.contains('\u{2514}'),
            "the legacy ● (U+25CF) / └ (U+2514) pair must not return; got {dumped:?}"
        );
    }

    /// The operation card claims one row per rendered line in a claiming
    /// frame.
    #[test]
    fn test_operation_card_claims_one_row_per_line_in_a_layout_frame() {
        let view = operation_view(
            "Generating",
            MessageStatus::InProgress,
            &[("bash(ls)", WorkRowStatus::Running)],
        );
        const CARD: u16 = 12;
        let tree = Widget::Stack {
            axis: Axis::Column,
            children: vec![(
                Track::Natural,
                Widget::Marked(
                    CARD,
                    Box::new(Widget::Text {
                        lines: component_lines(&ComponentView::Operation(view), &PALETTE)
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
            rect.height, 2,
            "chrome + one running row claim two rows; got {rect:?}"
        );
    }

    // ── Stage 4 (#1141): styled spans, never SGR bytes ──────────────────────

    /// SCAN REGRESSION (#1141): component renderers construct no SGR bytes —
    /// styling flows from the injected palette. This mirrors the stage-3
    /// type-agnostic scan: the production half of this file (everything
    /// before the test module) may not contain an escape sequence or an SGR
    /// constant at all.
    #[test]
    fn test_component_renderers_construct_no_sgr_bytes() {
        let source =
            std::fs::read_to_string(format!("{}/src/component.rs", env!("CARGO_MANIFEST_DIR")))
                .unwrap_or_else(|error| {
                    panic!("cannot read the component renderers under test: {error}")
                });
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("the file has a production half");
        let offenders: Vec<String> = production
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains("\\x1b") || line.contains("\\u{1b}"))
            .map(|(index, line)| format!("line {}: {line}", index + 1))
            .collect();
        assert!(
            offenders.is_empty(),
            "component renderers must construct no SGR bytes (stage 4 #1141); \
             found escape sequences in production code at:\n{}",
            offenders.join("\n")
        );
    }

    /// INVARIANT (#1141): the plain text of a styled line is exactly the
    /// concatenation of its span texts — the span pipeline cannot change what
    /// a row says, only how it renders.
    #[test]
    fn test_styled_lines_keep_text_equal_to_the_concatenated_spans() {
        for line in component_lines(
            &ComponentView::Operation(operation_view(
                "Generating",
                MessageStatus::InProgress,
                &[
                    ("bash(git push)", WorkRowStatus::Running),
                    (
                        "read(src/foo.rs)",
                        WorkRowStatus::Complete("45 lines".into()),
                    ),
                    ("bash(bad)", WorkRowStatus::Error("denied".into())),
                ],
            )),
            &PALETTE,
        ) {
            assert_eq!(
                crate::spans_text(&line.spans),
                line.text,
                "spans must concatenate to the line's plain text; line={line:?}"
            );
        }
    }

    /// The operation chrome regains its pre-migration styling: the `⏺` glyph
    /// cyan, the header plain, the running ellipsis dimmed.
    #[test]
    fn test_operation_chrome_and_rows_carry_the_pre_migration_styles() {
        let lines = component_lines(
            &ComponentView::Operation(operation_view(
                "Generating",
                MessageStatus::InProgress,
                &[
                    ("bash(git push)", WorkRowStatus::Running),
                    (
                        "read(src/foo.rs)",
                        WorkRowStatus::Complete("45 lines".into()),
                    ),
                    ("bash(bad)", WorkRowStatus::Error("denied".into())),
                ],
            )),
            &PALETTE,
        );
        let chrome = &lines[0];
        assert_eq!(
            (chrome.spans[0].style, chrome.spans[0].text.as_str()),
            (SpanStyle::fg(SpanColor::CYAN), "\u{23fa}"),
            "the chrome glyph wears the cyan glyph style; got {chrome:?}"
        );
        assert!(
            chrome.spans[1].style.is_plain(),
            "the header stays plain; got {chrome:?}"
        );
        let running_row = &lines[1];
        assert_eq!(
            running_row.spans[3].style,
            SpanStyle::fg(SpanColor::DARK_GREY).with_dim(true),
            "a running row's ellipsis is dimmed dark grey (the retired GrayDim); got {running_row:?}"
        );
        let complete_row = &lines[2];
        assert_eq!(
            complete_row.spans[4].style,
            SpanStyle::fg(SpanColor::DARK_GREY).with_dim(true),
            "a completed row's summary is dimmed; got {complete_row:?}"
        );
        let error_row = &lines[3];
        assert_eq!(
            error_row.spans[4],
            Span::styled("error:", SpanStyle::fg(SpanColor::RED)),
            "the error prefix wears the red error style; got {error_row:?}"
        );
    }

    /// The progress line wears its status colour end to end — the pre-migration
    /// `format()` path coloured label, bar, and readout alike.
    #[test]
    fn test_progress_line_wears_its_status_colour() {
        let running = component_lines(
            &ComponentView::Progress(progress_view(40, 100, MessageStatus::InProgress)),
            &PALETTE,
        )
        .remove(0);
        assert_eq!(
            running.spans[0].style,
            SpanStyle::fg(SpanColor::DARK_YELLOW),
            "a running bar wears the operation colour; got {running:?}"
        );
        let complete = component_lines(
            &ComponentView::Progress(progress_view(100, 100, MessageStatus::Complete)),
            &PALETTE,
        )
        .remove(0);
        assert_eq!(
            complete.spans[0].style,
            SpanStyle::fg(SpanColor::DARK_CYAN),
            "a complete bar wears the download colour; got {complete:?}"
        );
        let failed = component_lines(
            &ComponentView::Progress(progress_view(30, 100, MessageStatus::Failed)),
            &PALETTE,
        )
        .remove(0);
        assert_eq!(
            failed.spans[0].style,
            SpanStyle::fg(SpanColor::DARK_RED),
            "a failed bar wears the error colour; got {failed:?}"
        );
    }

    /// Static rows wear their kind colour; `Plain` stays a byte-exact
    /// passthrough with no spans at all.
    #[test]
    fn test_static_rows_wear_their_kind_colour_and_plain_stays_untouched() {
        let error = component_lines(
            &ComponentView::StaticText(static_view(StaticTextKind::Error, "boom")),
            &PALETTE,
        )
        .remove(0);
        assert_eq!(
            error.spans[0].style,
            SpanStyle::fg(SpanColor::DARK_RED),
            "an error row wears the error colour; got {error:?}"
        );
        let info = component_lines(
            &ComponentView::StaticText(static_view(StaticTextKind::Info, "note")),
            &PALETTE,
        )
        .remove(0);
        assert_eq!(
            info.spans[0].style,
            SpanStyle::fg(SpanColor::DARK_GREY),
            "an info row wears the system colour; got {info:?}"
        );
        let warning = component_lines(
            &ComponentView::StaticText(static_view(StaticTextKind::Warning, "careful")),
            &PALETTE,
        )
        .remove(0);
        assert_eq!(
            warning.spans[0].style,
            SpanStyle::fg(SpanColor::DARK_YELLOW),
            "a warning row wears the operation colour; got {warning:?}"
        );
        let plain = component_lines(
            &ComponentView::StaticText(static_view(StaticTextKind::Plain, "as-is")),
            &PALETTE,
        )
        .remove(0);
        assert!(
            plain.spans.is_empty(),
            "Plain passthrough carries no spans; got {plain:?}"
        );
    }

    /// A just-started live tool call dims its running ellipsis; the header
    /// itself stays plain.
    #[test]
    fn test_live_tool_running_ellipsis_is_dim_and_header_stays_plain() {
        let started = component_lines(
            &ComponentView::LiveTool(live_tool_view(
                "⏺ bash(echo hi)",
                &[],
                MessageStatus::InProgress,
            )),
            &PALETTE,
        )
        .remove(0);
        assert_eq!(
            (started.spans[0].style.is_plain(), started.spans[1].style),
            (true, SpanStyle::fg(SpanColor::DARK_GREY).with_dim(true)),
            "header plain + dimmed running ellipsis; got {started:?}"
        );
        assert_eq!(
            crate::spans_text(&started.spans),
            "⏺ bash(echo hi)…",
            "the plain projection still reads identically; got {started:?}"
        );
    }
}

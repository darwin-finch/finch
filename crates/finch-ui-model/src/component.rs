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
    MessageId, RenderedTranscriptLine, RowId,
};

/// The component snapshot of one migrated typed message. A message type
/// constructs the variant that belongs to it from its retained ViewModel;
/// `None`-returning rows have not migrated and keep the legacy projection.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
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
    /// A recalled/committed memory set for one turn: chrome plus a row per
    /// memory, its identity line, and the recalled text itself directly
    /// beneath it. Unlike a tool call, a memory has no input side and its
    /// content is fully known at construction — there is nothing to page
    /// through, so this component carries no Input/Output split and no
    /// bounded/scrollable child viewport (the tool-result control in
    /// `finch-tui`'s `tool_viewport.rs` applies to `NodeRole::ToolOutput`
    /// rows only, which this component never emits). Each row's recalled
    /// text is collapsed behind its identity/summary line by default and
    /// expands on click (#1235), the same component-owned disclosure
    /// mechanism the say turn's `show_program` uses.
    MemoryRecalled(MemoryRecalledView),
    /// A user turn component: local user query or attributed participant message.
    UserTurn(UserTurnView),
}

/// The ViewModel of a [`ComponentView::StaticText`] component: the message's
/// immutable content and the kind that decides its glyph. A `StaticMessage`
/// has no mutable state to retain — the content itself is the ViewModel —
/// and the message constructs the snapshot from its own immutable fields; no
/// lock is involved.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StaticTextView {
    pub kind: StaticTextKind,
    pub content_lines: Vec<String>,
}

/// Which glyph a static text row wears. Byte-compatible with the retired
/// `format()` presentation: `Plain` passes the content through with no
/// prefix, so pre-formatted text reaches the record byte-exactly.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
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
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LiveToolView {
    pub header: String,
    pub content_lines: Vec<String>,
    pub status: MessageStatus,
}

/// The ViewModel of a [`ComponentView::Operation`] component: the chrome
/// header, the whole-operation status, and one row per tool call with its
/// status. The message constructs the snapshot under its existing locks;
/// `rows` is the live state that grows and transitions as calls run.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OperationView {
    pub header: String,
    pub status: MessageStatus,
    pub rows: Vec<OperationRowView>,
}

/// One tool-call row of an [`OperationView`], with the shared row-status
/// vocabulary: the per-row glyph is a pure function of it.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OperationRowView {
    pub label: String,
    pub status: WorkRowStatus,
}

/// The ViewModel of a [`ComponentView::MemoryRecalled`] component: the chrome
/// header (e.g. "3 memories retrieved") and one row per recalled memory. The
/// message constructs the snapshot once, from the recall decision already
/// made for this turn — there is no running/streaming state to retain, but
/// each row's `expanded` flag is mutable UI state the message retains behind
/// its own lock (#1235), read fresh into this snapshot every frame.
/// `message_id` addresses the rows' `RowId`s the same way `SayTurnView`'s
/// does.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MemoryRecalledView {
    pub message_id: MessageId,
    pub header: String,
    pub rows: Vec<MemoryRecallRowView>,
}

/// One memory row of a [`MemoryRecalledView`]: its identity line (tier,
/// score, node id), a one-line presentation summary (raw vs. summarized),
/// and the recalled text itself. `body_lines` renders directly beneath the
/// row while `expanded` — no separate "Input"/"Output" disclosure, since a
/// memory has no input side. Collapsed (`expanded: false`) by default
/// (#1235): the identity/summary line is always visible, and a click (or the
/// keyboard disclosure path) reveals `body_lines`, mirroring the say turn's
/// `show_program` toggle.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MemoryRecallRowView {
    pub label: String,
    pub summary: String,
    pub body_lines: Vec<String>,
    pub expanded: bool,
}

/// The ViewModel of a [`ComponentView::UserTurn`] component: the prompt
/// marker, optional subject (e.g. participant name), the content lines,
/// and optional participant index for color palette selection.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct UserTurnView {
    pub marker: char,
    pub subject: Option<String>,
    pub content_lines: Vec<String>,
    pub participant_index: Option<usize>,
}

/// The one description of a turn's in-progress indicator: the verb, how long
/// the turn has run, and how many tokens have arrived.
///
/// Every surface that says "this turn is still working" reads this value —
/// the live transcript ([`turn_indicator_line`]), the plain node label of a
/// pending row, and the non-terminal `format()` text — so the wording, the
/// animation frame, and the `thinking` → `↓ N tokens` switch are decided in
/// one place. The animation frame is a pure function of `elapsed`, which the
/// message captures when it builds its snapshot; nothing here reads a clock.
///
/// The indicator is meaningful as plain text: the verb, the trailing
/// ellipsis, the elapsed time, and `thinking` or the token count all say the
/// turn is in progress without the pulsing marker.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TurnIndicatorView {
    pub verb: String,
    pub elapsed: std::time::Duration,
    pub token_count: usize,
}

/// Pulse frames, small → large → small.
const TURN_INDICATOR_FRAMES: &[&str] = &["✦", "✳", "✼", "✳"];

/// How long each pulse frame is held.
const TURN_INDICATOR_FRAME_MS: u128 = 200;

impl TurnIndicatorView {
    /// The pulse frame this snapshot's elapsed time selects.
    pub fn marker(&self) -> &'static str {
        let frame = (self.elapsed.as_millis() / TURN_INDICATOR_FRAME_MS) as usize;
        TURN_INDICATOR_FRAMES[frame % TURN_INDICATOR_FRAMES.len()]
    }

    /// The verb with its ellipsis: `Channeling…`. A blank verb reads
    /// `Working…` so the row never collapses to a bare marker.
    pub fn activity(&self) -> String {
        let verb = self.verb.trim();
        if verb.is_empty() {
            "Working…".to_string()
        } else {
            format!("{verb}…")
        }
    }

    /// Elapsed time, then `thinking` until the first token and the token
    /// count after: `18s · thinking`, `18s · ↓ 662 tokens`.
    pub fn stats(&self) -> String {
        let elapsed = turn_indicator_elapsed(self.elapsed.as_secs());
        if self.token_count == 0 {
            format!("{elapsed} · thinking")
        } else {
            format!(
                "{elapsed} · ↓ {} tokens",
                turn_indicator_tokens(self.token_count)
            )
        }
    }

    /// The whole indicator as unstyled text:
    /// `✳ Channeling… (18s · ↓ 662 tokens)`.
    pub fn plain_text(&self) -> String {
        format!("{} {} ({})", self.marker(), self.activity(), self.stats())
    }
}

fn turn_indicator_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

fn turn_indicator_tokens(count: usize) -> String {
    if count >= 1000 {
        format!("{:.1}k", count as f64 / 1000.0)
    } else {
        count.to_string()
    }
}

/// The live transcript row of a turn's in-progress indicator. Its text is
/// exactly [`TurnIndicatorView::plain_text`] behind the transcript's
/// two-column row indent; the marker wears the scheme accent and the stats
/// the muted summary style.
pub fn turn_indicator_line(
    view: &TurnIndicatorView,
    palette: &ComponentStylePalette,
) -> RenderedTranscriptLine {
    RenderedTranscriptLine::from_spans(vec![
        Span::plain("  "),
        Span::styled(view.marker(), palette.operation_glyph.clone()),
        Span::plain(format!(" {} ", view.activity())),
        Span::styled(
            format!("({})", view.stats()),
            palette.operation_summary.clone(),
        ),
    ])
}

/// The style roles the component renderers read (stage 4, #1141).
///
/// Plain data, terminal-independent: the engine builds it from the user's
/// `ColorScheme` for the scheme-owned roles and keeps the retained glyph
/// vocabulary for the rest. The `Default` value is the pre-migration
/// presentation — the exact colours the retired `format()` paths painted — so
/// the migrated surfaces "regain" their styling and the default stays honest
/// without a scheme at hand.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ComponentStylePalette {
    /// Progress line while running — pre-migration: `colors.status.operation`.
    pub progress_running: SpanStyle,
    /// Progress line when complete — pre-migration: `colors.status.download`.
    pub progress_complete: SpanStyle,
    /// Progress line when failed — pre-migration: `colors.messages.error`.
    pub progress_failed: SpanStyle,
    /// The operation chrome glyph `⏺` — the scheme accent (`ui.cursor`).
    pub operation_glyph: SpanStyle,
    /// The operation row glyph `⎿` — the scheme's muted `messages.system`.
    pub operation_row_glyph: SpanStyle,
    /// Operation summaries and running ellipses — dark grey + dim.
    pub operation_summary: SpanStyle,
    /// Operation row `error:` prefix — `messages.error`.
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
    /// User turn foreground colour.
    pub user_foreground: SpanColor,
    /// User turn background colour.
    pub user_background: SpanColor,
    /// Participant background colours (indexed 0..7).
    pub participant_backgrounds: [SpanColor; 8],
    /// Hover background colour for interactive transcript elements.
    pub hover_background: SpanColor,
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
            user_foreground: SpanColor::CYAN,
            user_background: SpanColor::Rgb(38, 38, 42),
            participant_backgrounds: [
                SpanColor::Rgb(24, 49, 70),
                SpanColor::Rgb(27, 55, 42),
                SpanColor::Rgb(62, 44, 24),
                SpanColor::Rgb(51, 36, 66),
                SpanColor::Rgb(22, 53, 55),
                SpanColor::Rgb(65, 34, 43),
                SpanColor::Rgb(54, 52, 27),
                SpanColor::Rgb(42, 47, 58),
            ],
            hover_background: SpanColor::Rgb(52, 54, 60),
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
        ComponentView::MemoryRecalled(memory) => memory_recalled_lines(memory, palette),
        ComponentView::UserTurn(user_turn) => user_turn_lines(user_turn, palette),
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
        MessageStatus::Complete => palette.progress_complete.clone(),
        MessageStatus::Failed => palette.progress_failed.clone(),
        MessageStatus::InProgress => palette.progress_running.clone(),
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
        header_spans.push(Span::styled("\u{2026}", palette.live_tool_ellipsis.clone()));
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
        Span::styled("\u{23fa}", palette.operation_glyph.clone()),
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
                    Span::styled("\u{23bf}", palette.operation_row_glyph.clone()),
                    Span::plain(format!(" {}", row.label)),
                ];
                match &row.status {
                    WorkRowStatus::Running => {
                        spans.push(Span::styled("\u{2026}", palette.operation_summary.clone()));
                    }
                    WorkRowStatus::Complete(summary) if summary.is_empty() => {}
                    WorkRowStatus::Complete(summary) => {
                        spans.push(Span::plain(" "));
                        spans.push(Span::styled(
                            summary.clone(),
                            palette.operation_summary.clone(),
                        ));
                    }
                    WorkRowStatus::Error(error) => {
                        spans.push(Span::plain(" "));
                        spans.push(Span::styled("error:", palette.operation_error.clone()));
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
        StaticTextKind::Info => ("ℹ️  ", palette.static_info.clone()),
        StaticTextKind::Error => ("❌ ", palette.static_error.clone()),
        StaticTextKind::Success => ("✓ ", palette.static_success.clone()),
        StaticTextKind::Warning => ("⚠️  ", palette.static_warning.clone()),
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
                    style.clone(),
                )])
            }
        })
        .collect()
}

/// A memory-recall row: chrome (`⏺ header`) then one row per memory — its
/// identity/summary line, then the recalled text directly beneath it while
/// that row is expanded. Unlike [`operation_lines`], a row here has no
/// running/complete/error status (a recalled memory is always already fully
/// known) and its body is plain content, never a bounded/scrollable viewport
/// — the whole point is that a couple of short lines of recalled context need
/// no pagination chrome, only a click to reveal them. Reuses the operation
/// glyph styles: visually this is the same "grouped activity" family as a
/// tool-call operation, just without the input side or the per-row running
/// state.
///
/// Collapsed by default (#1235): a row with recalled text is a component-
/// owned disclosure target, the same mechanism the say turn's completed
/// output region uses — every line belonging to the row (its identity/
/// summary line, and its body lines while expanded) carries the row's
/// `RowId` (`path = [row index]`), `component_owned: true`, and
/// `row_expanded` mirroring `expanded`, so a click anywhere in the row toggles
/// it via `MemoryRecalledMessage::transcript_action` /
/// `handle_transcript_action`. A row with no recalled text has nothing to
/// disclose and is not a click target.
fn memory_recalled_lines(
    view: &MemoryRecalledView,
    palette: &ComponentStylePalette,
) -> Vec<RenderedTranscriptLine> {
    let mut lines = Vec::with_capacity(1 + view.rows.len());
    lines.push(RenderedTranscriptLine::from_spans(vec![
        Span::styled("\u{23fa}", palette.operation_glyph.clone()),
        Span::plain(format!(" {}", view.header)),
    ]));
    for (index, row) in view.rows.iter().enumerate() {
        let expandable = !row.body_lines.is_empty();
        let target = expandable.then(|| RowId {
            message_id: view.message_id,
            path: vec![index as u32],
        });
        let mut row_spans = vec![
            Span::plain("  "),
            Span::styled("\u{23bf}", palette.operation_row_glyph.clone()),
        ];
        // #1259: a static chevron affordance -- collapsed "▸"/expanded "▾" --
        // prefixes the label of every row that has something to disclose, so
        // there is a visible hint the row is a click/keyboard target even
        // before the pointer moves. A row with no recalled text (not
        // `expandable`) gets no chevron: it is not a click target (#1235).
        if expandable {
            let chevron = if row.expanded { '\u{25be}' } else { '\u{25b8}' };
            row_spans.push(Span::styled(
                format!(" {chevron}"),
                palette.operation_row_glyph.clone(),
            ));
        }
        row_spans.push(Span::plain(format!(" {}", row.label)));
        if !row.summary.is_empty() {
            row_spans.push(Span::plain(" "));
            row_spans.push(Span::styled(
                format!("— {}", row.summary),
                palette.operation_summary.clone(),
            ));
        }
        lines.push(RenderedTranscriptLine {
            row_id: target.clone(),
            row_expanded: expandable.then_some(row.expanded),
            component_owned: expandable,
            ..RenderedTranscriptLine::from_spans(row_spans)
        });
        if expandable && row.expanded {
            lines.extend(
                row.body_lines
                    .iter()
                    .map(|body_line| RenderedTranscriptLine {
                        row_id: target.clone(),
                        row_expanded: Some(true),
                        component_owned: true,
                        ..RenderedTranscriptLine::from_spans(vec![Span::plain(format!(
                            "      {body_line}"
                        ))])
                    }),
            );
        }
    }
    lines
}

/// Render a user turn component: marker (and subject if present) on line 0,
/// content lines 1..N beneath it. Every line is styled with the user foreground
/// and theme-aware background, ensuring continuation lines retain their styling
/// even when scrolled down.
fn user_turn_lines(
    view: &UserTurnView,
    palette: &ComponentStylePalette,
) -> Vec<RenderedTranscriptLine> {
    let bg = match view.participant_index {
        Some(index) => {
            palette.participant_backgrounds[index % palette.participant_backgrounds.len()].clone()
        }
        None => palette.user_background.clone(),
    };
    let style = SpanStyle::fg(palette.user_foreground.clone()).with_bg(bg);
    let first_line_content = view.content_lines.first().map(|s| s.as_str()).unwrap_or("");
    let first_line_text = match &view.subject {
        Some(subject) => format!(" {} {}: {}", view.marker, subject, first_line_content),
        None => format!(" {} {}", view.marker, first_line_content),
    };

    let mut lines = Vec::with_capacity(view.content_lines.len().max(1));
    lines.push(RenderedTranscriptLine::from_spans(vec![Span::styled(
        first_line_text,
        style.clone(),
    )]));

    if view.content_lines.len() > 1 {
        for line in &view.content_lines[1..] {
            lines.push(RenderedTranscriptLine::from_spans(vec![Span::styled(
                line.clone(),
                style.clone(),
            )]));
        }
    }

    lines
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
        user_foreground: SpanColor::CYAN,
        user_background: SpanColor::Rgb(38, 38, 42),
        participant_backgrounds: [
            SpanColor::Rgb(24, 49, 70),
            SpanColor::Rgb(27, 55, 42),
            SpanColor::Rgb(62, 44, 24),
            SpanColor::Rgb(51, 36, 66),
            SpanColor::Rgb(22, 53, 55),
            SpanColor::Rgb(65, 34, 43),
            SpanColor::Rgb(54, 52, 27),
            SpanColor::Rgb(42, 47, 58),
        ],
        hover_background: SpanColor::Rgb(52, 54, 60),
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
            vec!["hello", "(ran 2s)"],
            "the say card renders its actionable prose and non-control elapsed metadata \
             through the generalized accessor; got {texts:?}"
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
            rect.height, 2,
            "prose + elapsed metadata claim two rows; got {rect:?}"
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

    // ── MemoryRecalled: recalled memories, no Input/Output split ────────────

    /// `expanded` mirrors what a click on that row would leave in place: the
    /// tuple's trailing bool is the row's `MemoryRecallRowView::expanded`.
    fn memory_recalled_view(
        header: &str,
        rows: &[(&str, &str, &[&str], bool)],
    ) -> MemoryRecalledView {
        MemoryRecalledView {
            message_id: MessageId::new(),
            header: header.to_string(),
            rows: rows
                .iter()
                .map(
                    |(label, summary, body_lines, expanded)| MemoryRecallRowView {
                        label: label.to_string(),
                        summary: summary.to_string(),
                        body_lines: body_lines.iter().map(|line| line.to_string()).collect(),
                        expanded: *expanded,
                    },
                )
                .collect(),
        }
    }

    fn memory_recalled_texts(view: &MemoryRecalledView) -> Vec<String> {
        component_lines(&ComponentView::MemoryRecalled(view.clone()), &PALETTE)
            .into_iter()
            .map(|line| line.text)
            .collect()
    }

    /// INVARIANT: a recalled memory renders its identity line, its
    /// presentation summary, and — while expanded — the recalled text
    /// directly beneath it; reproduces the reported bug at the production
    /// boundary (a memory row showed a generic tool-call "Input"/"Output"
    /// disclosure structure that makes no sense for a memory, since there is
    /// no input to a recall). No line may say "Input" or carry an
    /// "Output (" count header.
    #[test]
    fn test_memory_recalled_row_has_no_input_output_split() {
        let view = memory_recalled_view(
            "2 memories retrieved",
            &[
                (
                    "committed · score 0.64 · node 4",
                    "138 chars, sent raw",
                    &[
                        "user: this repo I'm in (files on disk) are your harness. what do you think of it?",
                        "assistant: I don't have direct access to your files or environment.",
                    ],
                    true,
                ),
                (
                    "recalled · score 0.60 · node 1",
                    "81 chars, sent raw",
                    &["user: Qwen, are you there?", "assistant: Qwen, I'm here."],
                    true,
                ),
            ],
        );
        let texts = memory_recalled_texts(&view);
        assert_eq!(
            texts,
            vec![
                "⏺ 2 memories retrieved",
                "  ⎿ ▾ committed · score 0.64 · node 4 — 138 chars, sent raw",
                "      user: this repo I'm in (files on disk) are your harness. what do you think of it?",
                "      assistant: I don't have direct access to your files or environment.",
                "  ⎿ ▾ recalled · score 0.60 · node 1 — 81 chars, sent raw",
                "      user: Qwen, are you there?",
                "      assistant: Qwen, I'm here.",
            ],
            "chrome plus one identity+summary line (prefixed with the #1259 expanded \
             chevron '▾') and, while expanded, the recalled text beneath it, per memory; \
             got {texts:?}"
        );
        assert!(
            !texts
                .iter()
                .any(|line| line == "Input" || line.contains("Output (")),
            "INVARIANT: a memory row must never show the tool-call \"Input\"/\"Output (\" \
             disclosure structure — a memory has no input side; lines were {texts:?}"
        );
    }

    /// #1235: a memory row is collapsed by default — the identity/summary
    /// line renders, but the recalled text stays hidden until the row is
    /// expanded. The summary line itself carries the row's `RowId`,
    /// `component_owned: true`, and `row_expanded: Some(false)`, the same
    /// component-owned disclosure metadata the say turn's completed output
    /// region carries, so the transcript engine's existing click/keyboard
    /// routing (`dispatch_component_disclosure`) picks it up unchanged.
    #[test]
    fn test_memory_recalled_row_collapsed_by_default_hides_recalled_text() {
        let view = memory_recalled_view(
            "1 memory retrieved",
            &[(
                "recalled · score 0.64 · node 4",
                "138 chars, sent raw",
                &["user: hi", "assistant: hello"],
                false,
            )],
        );
        let lines = component_lines(&ComponentView::MemoryRecalled(view.clone()), &PALETTE);
        let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "⏺ 1 memory retrieved",
                "  ⎿ ▸ recalled · score 0.64 · node 4 — 138 chars, sent raw",
            ],
            "collapsed by default: the recalled text must not render until expanded, and \
             the summary line leads with the #1259 collapsed chevron '▸' as the visible \
             affordance that the row is a click/keyboard target; got {texts:?}"
        );
        let summary_line = &lines[1];
        assert_eq!(
            summary_line.row_id,
            Some(RowId {
                message_id: view.message_id,
                path: vec![0],
            }),
            "the summary line is the row's disclosure hit target; line={summary_line:?}"
        );
        assert!(
            summary_line.component_owned,
            "disclosure state lives on the component ViewModel, same as the say turn \
             (#882); line={summary_line:?}"
        );
        assert_eq!(
            summary_line.row_expanded,
            Some(false),
            "row_expanded reports the collapsed state for assistive consumers; \
             line={summary_line:?}"
        );
    }

    /// #1235: the same row, expanded, renders its recalled text with every
    /// line — summary and body alike — sharing the row's `RowId` and
    /// `row_expanded: Some(true)`, mirroring the say turn's completed output
    /// region where every content line is the toggle hit target.
    #[test]
    fn test_memory_recalled_row_expanded_reveals_recalled_text() {
        let view = memory_recalled_view(
            "1 memory retrieved",
            &[(
                "recalled · score 0.64 · node 4",
                "138 chars, sent raw",
                &["user: hi", "assistant: hello"],
                true,
            )],
        );
        let lines = component_lines(&ComponentView::MemoryRecalled(view.clone()), &PALETTE);
        let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "⏺ 1 memory retrieved",
                "  ⎿ ▾ recalled · score 0.64 · node 4 — 138 chars, sent raw",
                "      user: hi",
                "      assistant: hello",
            ],
            "expanded: the recalled text renders beneath the summary line, and the summary \
             line's chevron flips to the #1259 expanded glyph '▾'; got {texts:?}"
        );
        let target = RowId {
            message_id: view.message_id,
            path: vec![0],
        };
        assert!(
            lines[1..]
                .iter()
                .all(|line| line.row_id == Some(target.clone())
                    && line.component_owned
                    && line.row_expanded == Some(true)),
            "every line of the expanded row — summary and body — is the same toggle hit \
             target reporting the expanded state; lines={:?}",
            &lines[1..]
        );
    }

    /// A memory with no recalled text (defensive: an empty presentation)
    /// still claims its identity/summary row without a phantom empty body
    /// line — unlike `StaticText`'s single-row furniture rule, an
    /// [`OperationRow`]-style row with zero body lines simply claims zero
    /// extra rows. With nothing to disclose, the row is also not a click
    /// target: no `RowId`, not component-owned, no `row_expanded`.
    #[test]
    fn test_memory_recalled_row_with_empty_body_claims_no_extra_rows() {
        let view = memory_recalled_view(
            "1 memory retrieved",
            &[(
                "committed · score 0.50 · node 9",
                "0 chars, sent raw",
                &[],
                false,
            )],
        );
        let lines = component_lines(&ComponentView::MemoryRecalled(view), &PALETTE);
        let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "⏺ 1 memory retrieved",
                "  ⎿ committed · score 0.50 · node 9 — 0 chars, sent raw",
            ],
            "a row with nothing to disclose carries no #1259 chevron ('▸'/'▾') at all -- \
             not even the collapsed glyph -- since it is not an interactive row; got {texts:?}"
        );
        let summary_line = &lines[1];
        assert!(
            !summary_line.text.contains('\u{25b8}') && !summary_line.text.contains('\u{25be}'),
            "a non-interactive row (nothing to disclose) must render no chevron glyph; \
             line={summary_line:?}"
        );
        assert_eq!(
            summary_line.row_id, None,
            "a row with nothing to disclose is not a click target; line={summary_line:?}"
        );
        assert!(
            !summary_line.component_owned,
            "a row with nothing to disclose is not component-owned; line={summary_line:?}"
        );
        assert_eq!(
            summary_line.row_expanded, None,
            "a row with nothing to disclose reports no disclosure state; \
             line={summary_line:?}"
        );
    }

    /// INVARIANT (#1141): a memory row's spans concatenate exactly to its
    /// plain text, same as every other migrated component.
    #[test]
    fn test_memory_recalled_spans_equal_concatenated_text() {
        let view = memory_recalled_view(
            "1 memory retrieved",
            &[(
                "committed · score 0.64 · node 4",
                "138 chars, sent raw",
                &["user: hi", "assistant: hello"],
                true,
            )],
        );
        for line in component_lines(&ComponentView::MemoryRecalled(view), &PALETTE) {
            assert_eq!(
                crate::spans_text(&line.spans),
                line.text,
                "spans must concatenate to the line's plain text; line={line:?}"
            );
        }
    }

    /// A memory row's body lines carry no [`NodeRole::ToolOutput`] tagging
    /// (indeed, no role at all) and no `body_of` owner — the bounded
    /// tool-result viewport in `finch-tui`'s `tool_viewport.rs` keys
    /// exclusively off `role == Some(NodeRole::ToolOutput)`, so a memory row
    /// can never be mistaken for a paginated tool result, however long its
    /// recalled text is.
    #[test]
    fn test_memory_recalled_body_lines_carry_no_tool_output_role() {
        let view = memory_recalled_view(
            "1 memory retrieved",
            &[(
                "committed · score 0.64 · node 4",
                "138 chars, sent raw",
                &["user: hi", "assistant: hello"],
                true,
            )],
        );
        for line in component_lines(&ComponentView::MemoryRecalled(view), &PALETTE) {
            assert!(
                line.role.is_none(),
                "INVARIANT: a memory row must never carry a NodeRole (in particular never \
                 ToolOutput), so the bounded tool-result viewport cannot claim it; line={line:?}"
            );
            assert!(
                line.body_of.is_none(),
                "INVARIANT: a memory row must never register a viewport owner id; line={line:?}"
            );
        }
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
            (chrome.spans[0].style.clone(), chrome.spans[0].text.as_str()),
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
            (
                started.spans[0].style.is_plain(),
                started.spans[1].style.clone()
            ),
            (true, SpanStyle::fg(SpanColor::DARK_GREY).with_dim(true)),
            "header plain + dimmed running ellipsis; got {started:?}"
        );
        assert_eq!(
            crate::spans_text(&started.spans),
            "⏺ bash(echo hi)…",
            "the plain projection still reads identically; got {started:?}"
        );
    }

    /// User turn lines style every line with foreground and background.
    #[test]
    fn test_user_turn_lines_style_every_line_with_foreground_and_background() {
        let view = UserTurnView {
            marker: '❯',
            subject: None,
            content_lines: vec![
                "first line".into(),
                "second line".into(),
                "third line".into(),
            ],
            participant_index: None,
        };
        let lines = component_lines(&ComponentView::UserTurn(view), &PALETTE);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, " ❯ first line");
        assert_eq!(lines[1].text, "second line");
        assert_eq!(lines[2].text, "third line");

        let expected_style =
            SpanStyle::fg(PALETTE.user_foreground).with_bg(PALETTE.user_background);
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(line.spans.len(), 1, "line {i} must have 1 span");
            assert_eq!(
                line.spans[0].style, expected_style,
                "line {i} must wear expected style"
            );
            assert_eq!(crate::spans_text(&line.spans), line.text);
        }
    }

    /// Participant turns use their assigned participant background from the palette.
    /// Participant turns use their assigned participant background from the palette.
    #[test]
    fn test_user_turn_lines_support_participant_background() {
        let view = UserTurnView {
            marker: '◆',
            subject: Some("alice@box".into()),
            content_lines: vec!["hello world".into()],
            participant_index: Some(3),
        };
        let lines = component_lines(&ComponentView::UserTurn(view), &PALETTE);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, " ◆ alice@box: hello world");
        let expected_style =
            SpanStyle::fg(PALETTE.user_foreground).with_bg(PALETTE.participant_backgrounds[3]);
        assert_eq!(lines[0].spans[0].style, expected_style);
    }

    fn indicator(millis: u64, token_count: usize) -> TurnIndicatorView {
        TurnIndicatorView {
            verb: "Channeling".into(),
            elapsed: std::time::Duration::from_millis(millis),
            token_count,
        }
    }

    /// INVARIANT (#1664): the indicator's pulse frame is a function of the
    /// elapsed time its snapshot carries — one frame per 200 ms, cycling
    /// small → large → small — and nothing else.
    #[test]
    fn test_turn_indicator_pulse_frame_is_a_function_of_carried_elapsed_time() {
        let frames =
            [0, 199, 200, 400, 600, 800].map(|millis| (millis, indicator(millis, 0).marker()));
        assert_eq!(
            frames,
            [
                (0, "✦"),
                (199, "✦"),
                (200, "✳"),
                (400, "✼"),
                (600, "✳"),
                (800, "✦"),
            ],
            "the pulse holds each frame for 200 ms and repeats after four"
        );
    }

    /// INVARIANT (#1664): the indicator reads `thinking` until the first
    /// token and the token count after, with elapsed time in both, and a
    /// blank verb still reads as words.
    #[test]
    fn test_turn_indicator_text_switches_from_thinking_to_the_token_count() {
        let waiting = indicator(18_000, 0);
        let streaming = indicator(18_000, 662);
        let long = indicator(125_000, 1_600);
        let blank = TurnIndicatorView {
            verb: "  ".into(),
            ..indicator(0, 0)
        };
        assert_eq!(
            [
                waiting.plain_text(),
                streaming.plain_text(),
                long.plain_text(),
                blank.plain_text(),
            ],
            [
                "✼ Channeling… (18s · thinking)".to_string(),
                "✼ Channeling… (18s · ↓ 662 tokens)".to_string(),
                "✳ Channeling… (2m 5s · ↓ 1.6k tokens)".to_string(),
                "✦ Working… (0s · thinking)".to_string(),
            ],
            "the indicator is one line of plain words in every state"
        );
    }

    /// INVARIANT (#1141, #1664): the live indicator row is styled spans from
    /// the injected palette — marker in the accent, stats in the muted
    /// summary style — and its text is exactly the shared plain description
    /// behind the transcript's row indent.
    #[test]
    fn test_turn_indicator_line_is_palette_styled_spans_over_the_shared_text() {
        let view = indicator(400, 12);
        let line = turn_indicator_line(&view, &PALETTE);
        assert_eq!(
            (line.text.as_str(), crate::spans_text(&line.spans)),
            (
                "  ✼ Channeling… (0s · ↓ 12 tokens)",
                format!("  {}", view.plain_text())
            ),
            "the row's text is the shared description and equals its spans; line={line:?}"
        );
        let styled = line
            .spans
            .iter()
            .filter(|span| !span.style.is_plain())
            .map(|span| (span.as_str().to_string(), span.style.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            styled,
            vec![
                ("✼".to_string(), PALETTE.operation_glyph.clone()),
                (
                    "(0s · ↓ 12 tokens)".to_string(),
                    PALETTE.operation_summary.clone()
                ),
            ],
            "the marker wears the palette accent and the stats the muted summary style; \
             line={line:?}"
        );
    }
}

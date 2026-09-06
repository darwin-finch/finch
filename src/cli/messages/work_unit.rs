// WorkUnit - Unified message type for one AI generation turn
//
// A WorkUnit covers the full lifecycle of one AI response:
//   1. Streaming phase  → animated "✦ Channeling… (Xs · thinking)" header
//   2. Tool call phase  → sub-rows with "⎿ bash(cmd)…" / "⎿ bash(cmd) N lines"
//   3. Complete phase   → "⏺ response text" with collapsed sub-rows
//
// WorkUnit replaces the combination of StreamingResponseMessage + OperationMessage.
// It lives in the shadow buffer, rendered by the blit cycle (~100ms tick).
// The throb animation is TIME-DRIVEN — no external counter required.

use crossterm::style::{Attribute, Color, SetAttribute, SetForegroundColor};
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// Curated word list for the thinking spinner verb.
const SPINNER_WORDS: &[&str] = &[
    "Analyzing",
    "Brainstorming",
    "Building",
    "Calculating",
    "Channeling",
    "Cogitating",
    "Considering",
    "Crafting",
    "Deliberating",
    "Envisioning",
    "Evaluating",
    "Exploring",
    "Formulating",
    "Generating",
    "Ideating",
    "Meditating",
    "Mulling",
    "Pondering",
    "Processing",
    "Reasoning",
    "Reflecting",
    "Ruminating",
    "Sifting",
    "Synthesizing",
    "Thinking",
    "Weighing",
    "Working",
];

/// Pick the next spinner verb in round-robin order.
/// Uses a global atomic counter — no `rand` dependency needed.
pub fn random_spinner_verb() -> &'static str {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let idx = COUNTER.fetch_add(1, Ordering::Relaxed) % SPINNER_WORDS.len();
    SPINNER_WORDS[idx]
}

use super::{Message, MessageId, MessageStatus, TranscriptRow, TranscriptRowId, TranscriptRowKind};
use crate::cli::diff::{render_files, DiffColorMode, FileDiff, MAX_DIFF_PREVIEW_LINES};
use crate::config::{ColorScheme, MessageBand};

// Animation frames: small → large → small (creates a "throb" pulse effect)
const THROB_FRAMES: &[&str] = &["✦", "✳", "✼", "✳"];

const RESET: SetAttribute = SetAttribute(Attribute::Reset);
const CYAN: SetForegroundColor = SetForegroundColor(Color::Cyan);
const GRAY: SetForegroundColor = SetForegroundColor(Color::DarkGrey);
const RED_COLOR: SetForegroundColor = SetForegroundColor(Color::Red);

struct GrayDim;

impl fmt::Display for GrayDim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}",
            SetForegroundColor(Color::DarkGrey),
            SetAttribute(Attribute::Dim)
        )
    }
}

const GRAY_DIM: GrayDim = GrayDim;

// ============================================================================
// WorkRowStatus / WorkRow
// ============================================================================

/// Status of an individual tool-call sub-row within a WorkUnit
#[derive(Clone, Debug)]
pub enum WorkRowStatus {
    /// Tool is currently running
    Running,
    /// Tool completed with an optional compact one-line summary
    Complete(String),
    /// Tool failed with an error description
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkRowPresentation {
    Tool,
    Activity,
}

/// How a completed unit is projected into the transcript.
///
/// Most units are ordinary assistant turns and retain the familiar `⏺` marker.
/// VM wire source and its emitted output are separate artifacts: source is
/// explicitly labelled and output is rendered as plain content so neither is
/// mistaken for a second assistant turn.
#[derive(Clone, Debug, Default)]
pub enum WorkUnitPresentation {
    #[default]
    Assistant,
    /// Internal lifecycle activity that must not be presented as model tool
    /// calls (for example, a named Brain run's status/result projection).
    Activity {
        title: String,
    },
    ProgramSource {
        language: String,
    },
    /// VM output with optional handle chrome. A private producer-owned state
    /// distinguishes successful `say` prose from ordinary or failed output
    /// without expanding this public presentation API.
    ProgramOutput {
        title: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ProgramOutputSemantics {
    Pending,
    AssistantSay,
    #[default]
    Ordinary,
}

/// A single tool-call sub-item rendered below the WorkUnit header
#[derive(Clone, Debug)]
pub struct WorkRow {
    /// Pre-formatted label, e.g. "bash(git status)"
    pub label: String,
    pub status: WorkRowStatus,
    presentation: WorkRowPresentation,
    /// When this row started — used for the Running animation
    started_at: Instant,
    /// Elapsed time captured at the moment the row completed (not recalculated)
    elapsed_at_finish: Option<std::time::Duration>,
    /// Optional body lines shown indented below the summary line (e.g. diff content, command output)
    pub body_lines: Vec<String>,
    /// Parsed once when a completed body is installed; retained rows reuse it.
    diffs: Option<Vec<FileDiff>>,
}

// ============================================================================
// WorkUnitInner (behind RwLock)
// ============================================================================

struct WorkUnitInner {
    /// Final AI response text (empty while InProgress)
    response_text: String,
    /// Approximate token count (accumulated from text deltas)
    token_count: usize,
    /// True while in the "thinking" phase (before tokens arrive)
    thinking: bool,
    /// Sub-rows for tool calls
    rows: Vec<WorkRow>,
    /// Overall status of this unit
    status: MessageStatus,
    /// Elapsed time captured when the unit completed (stable for scrollback display)
    elapsed_at_finish: Option<std::time::Duration>,
    presentation: WorkUnitPresentation,
    program_output_semantics: ProgramOutputSemantics,
    /// Host/UI state distinct from the output body. A VM `output-status`
    /// must not erase prior `output-append`/`output-replace` content.
    transient_status: Option<String>,
    /// Bounded or indeterminate progress reported by an explicit output
    /// handle. Plain `say` output never uses this field.
    progress: Option<(u64, Option<u64>)>,
}

// ============================================================================
// WorkUnit
// ============================================================================

/// A unified message covering one AI generation turn.
///
/// Created once per turn — before streaming begins.
/// Blit cycle calls `format()` every ~100ms; the throb icon is computed
/// purely from `started_at.elapsed()`, no external counter needed.
pub struct WorkUnit {
    id: MessageId,
    /// Verb shown in the animated header: "Channeling", "Building", etc.
    verb: String,
    /// When this unit started — drives time-driven animation
    started_at: Instant,
    inner: Arc<RwLock<WorkUnitInner>>,
}

impl fmt::Debug for WorkUnit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // WorkUnit contents are mutable UI state and can be very large. Event
        // logs only need a stable, human-useful identity.
        formatter
            .debug_struct("WorkUnit")
            .field("id", &self.id)
            .field("verb", &self.verb)
            .finish_non_exhaustive()
    }
}

impl WorkUnit {
    /// Create a new WorkUnit with the given verb (e.g. `"Channeling"`).
    pub fn new(verb: impl Into<String>) -> Self {
        Self::with_id(MessageId::new(), verb)
    }

    /// Reconstruct a WorkUnit with the stable ID carried by retained/canonical
    /// session data so disclosure state survives frontend reconnects.
    pub fn with_id(id: MessageId, verb: impl Into<String>) -> Self {
        Self {
            id,
            verb: verb.into(),
            started_at: Instant::now(),
            inner: Arc::new(RwLock::new(WorkUnitInner {
                response_text: String::new(),
                token_count: 0,
                thinking: false,
                rows: Vec::new(),
                status: MessageStatus::InProgress,
                elapsed_at_finish: None,
                presentation: WorkUnitPresentation::Assistant,
                program_output_semantics: ProgramOutputSemantics::Ordinary,
                transient_status: None,
                progress: None,
            })),
        }
    }

    // ── Update API ──────────────────────────────────────────────────────────

    /// Accumulate tokens from a text delta (approximate: counts whitespace words).
    pub fn add_tokens(&self, text: &str) {
        let count = text.split_whitespace().count();
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .token_count += count;
    }

    /// Set the "thinking" flag shown in the animated status line.
    pub fn set_thinking(&self, thinking: bool) {
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .thinking = thinking;
    }

    /// Set the final response text (call after streaming ends).
    pub fn set_response(&self, text: impl Into<String>) {
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .response_text = text.into();
    }

    /// Return a generation unit to ordinary assistant/tool presentation.
    /// Provider text may initially look like VM source before a later stream
    /// block reveals tool calls; that provisional text must not remain labelled
    /// as an executable program.
    pub fn set_assistant_presentation(&self) {
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .presentation = WorkUnitPresentation::Assistant;
    }

    /// Render retained rows as internal lifecycle activity rather than model
    /// tool calls.
    pub fn set_activity_presentation(&self, title: impl Into<String>) {
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .presentation = WorkUnitPresentation::Activity {
            title: title.into(),
        };
    }

    /// Append a chunk to the response text (for partial updates).
    pub fn append_response(&self, text: &str) {
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .response_text
            .push_str(text);
    }

    /// Render this unit as the exact program received from the provider.
    pub fn set_program_source(&self, language: impl Into<String>) {
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .presentation = WorkUnitPresentation::ProgramSource {
            language: language.into(),
        };
    }

    /// Render this unit as output emitted by a VM program, not an assistant
    /// message. `say` itself remains append-only; this only chooses UI chrome.
    pub fn set_program_output(&self) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        inner.presentation = WorkUnitPresentation::ProgramOutput { title: None };
        inner.program_output_semantics = ProgramOutputSemantics::Ordinary;
    }

    /// Start an unresolved VM response port. Its producer will explicitly
    /// classify the first effect; renderers never inspect response content.
    pub(crate) fn set_pending_program_output(&self) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        inner.presentation = WorkUnitPresentation::ProgramOutput { title: None };
        inner.program_output_semantics = ProgramOutputSemantics::Pending;
    }

    /// Present output emitted through the program's conversational `say`
    /// channel as assistant prose. The VM output projection owns this choice;
    /// renderers must not infer it from response text.
    pub(crate) fn set_assistant_output(&self) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if matches!(
            inner.presentation,
            WorkUnitPresentation::ProgramOutput { title: None }
        ) && matches!(
            inner.program_output_semantics,
            ProgramOutputSemantics::Pending | ProgramOutputSemantics::AssistantSay
        ) {
            inner.program_output_semantics = ProgramOutputSemantics::AssistantSay;
        }
    }

    /// Resolve a pending response port as host lifecycle output while leaving
    /// ordinary assistant/tool work units in their existing presentation.
    pub(crate) fn set_host_output(&self) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if matches!(
            inner.presentation,
            WorkUnitPresentation::ProgramOutput { title: None }
        ) {
            inner.program_output_semantics = ProgramOutputSemantics::Ordinary;
        }
    }

    /// Terminalize a program output as a diagnostic, replacing any provisional
    /// `say` presentation from effects emitted before the failure.
    pub(crate) fn set_program_failed(&self) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        inner.presentation = WorkUnitPresentation::ProgramOutput { title: None };
        inner.program_output_semantics = ProgramOutputSemantics::Ordinary;
        inner.status = MessageStatus::Failed;
        inner.elapsed_at_finish = Some(self.started_at.elapsed());
    }

    /// Render this unit as an independently addressable VM output handle.
    /// The title is presentation metadata supplied by `output-open`, not an
    /// emitted response fragment.
    pub fn set_output_handle(&self, title: impl Into<String>) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        inner.presentation = WorkUnitPresentation::ProgramOutput {
            title: Some(title.into()),
        };
        inner.program_output_semantics = ProgramOutputSemantics::Ordinary;
    }

    /// Update transient status independently from the durable visible body.
    pub fn set_transient_status(&self, status: Option<String>) {
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .transient_status = status;
    }

    /// Update an explicit output handle's progress independently from its
    /// body and status. `None` represents indeterminate progress.
    pub fn set_output_progress(&self, completed: u64, total: Option<u64>) {
        self.inner
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .progress = Some((completed, total));
    }

    /// Add a running tool-call sub-row; returns its index for later updates.
    pub fn add_row(&self, label: impl Into<String>) -> usize {
        self.add_row_with_presentation(label, WorkRowPresentation::Tool)
    }

    /// Add a running internal lifecycle row that is not a model tool call.
    pub fn add_activity_row(&self, label: impl Into<String>) -> usize {
        self.add_row_with_presentation(label, WorkRowPresentation::Activity)
    }

    fn add_row_with_presentation(
        &self,
        label: impl Into<String>,
        presentation: WorkRowPresentation,
    ) -> usize {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        let idx = inner.rows.len();
        inner.rows.push(WorkRow {
            label: label.into(),
            status: WorkRowStatus::Running,
            presentation,
            started_at: Instant::now(),
            elapsed_at_finish: None,
            body_lines: Vec::new(),
            diffs: None,
        });
        idx
    }

    /// Mark a sub-row complete with an optional compact one-line summary.
    pub fn complete_row(&self, idx: usize, summary: impl Into<String>) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if let Some(row) = inner.rows.get_mut(idx) {
            row.elapsed_at_finish = Some(row.started_at.elapsed());
            row.status =
                WorkRowStatus::Complete(crate::cli::diff::sanitize_terminal(&summary.into()));
        }
    }

    /// Mark a sub-row complete with a one-line summary and body lines shown below it.
    ///
    /// Body lines are rendered indented beneath the `⎿ label  summary` line —
    /// used for diff content (Edit), command output (Bash), match results (Grep), etc.
    pub fn complete_row_with_body(
        &self,
        idx: usize,
        summary: impl Into<String>,
        body_lines: Vec<String>,
    ) {
        let summary = summary.into();
        let summary_starts_diff = is_diff_start(&summary);
        let body_has_diff = !summary_starts_diff
            && body_lines
                .iter()
                .take(crate::cli::diff::MAX_DIFF_STRUCTURAL_LINES)
                .any(|line| is_diff_start(line));
        let parsed = if summary_starts_diff || body_has_diff {
            let parsed = if summary_starts_diff {
                FileDiff::parse_lines(
                    std::iter::once(summary.as_str()).chain(body_lines.iter().map(String::as_str)),
                )
            } else {
                FileDiff::parse_lines(body_lines.iter().map(String::as_str))
            };
            if parsed
                .iter()
                .any(|diff| !diff.old_path.is_empty() || !diff.new_path.is_empty())
            {
                Some(parsed)
            } else {
                None
            }
        } else {
            None
        };
        let display_body_lines = if parsed.is_some() {
            if summary_starts_diff {
                Vec::new()
            } else {
                body_lines
                    .into_iter()
                    .take_while(|line| !is_diff_start(line))
                    .collect()
            }
        } else {
            body_lines
        };
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if let Some(row) = inner.rows.get_mut(idx) {
            row.elapsed_at_finish = Some(row.started_at.elapsed());
            // The structured renderer owns the path/count header. Suppress the
            // producer's equivalent summary so each semantic header appears once.
            row.status = WorkRowStatus::Complete(if parsed.is_some() {
                String::new()
            } else {
                crate::cli::diff::sanitize_terminal(&summary)
            });
            row.diffs = parsed;
            row.body_lines = display_body_lines;
        }
    }

    /// Complete a tool row with a structured, theme-independent file diff.
    pub fn complete_row_with_diff(&self, idx: usize, diff: FileDiff) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if let Some(row) = inner.rows.get_mut(idx) {
            row.elapsed_at_finish = Some(row.started_at.elapsed());
            row.status = WorkRowStatus::Complete(String::new());
            row.body_lines = Vec::new();
            row.diffs = Some(vec![diff]);
        }
    }

    /// Append a live output line to a Running sub-row's body.
    ///
    /// Called by the bash tool's streaming path once per stdout line.
    /// The `format()` method shows the last 3 lines for Running rows,
    /// creating a live scrolling preview while the command executes.
    pub fn append_row_body_line(&self, idx: usize, line: String) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if let Some(row) = inner.rows.get_mut(idx) {
            row.body_lines
                .push(crate::cli::diff::sanitize_terminal(&line));
        }
    }

    /// Mark a sub-row as failed.
    pub fn fail_row(&self, idx: usize, error: impl Into<String>) {
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        if let Some(row) = inner.rows.get_mut(idx) {
            row.elapsed_at_finish = Some(row.started_at.elapsed());
            row.status = WorkRowStatus::Error(crate::cli::diff::sanitize_terminal(&error.into()));
        }
    }

    /// Mark the whole WorkUnit complete (stops animation, shows final content).
    pub fn set_complete(&self) {
        let elapsed = self.started_at.elapsed();
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        inner.elapsed_at_finish = Some(elapsed);
        inner.status = MessageStatus::Complete;
    }

    /// Mark the whole WorkUnit failed.
    pub fn set_failed(&self) {
        let elapsed = self.started_at.elapsed();
        let mut inner = self.inner.write().unwrap_or_else(|p| p.into_inner());
        inner.elapsed_at_finish = Some(elapsed);
        inner.status = MessageStatus::Failed;
    }

    /// Whether this unit has reached the failed terminal state.
    pub(crate) fn is_failed(&self) -> bool {
        self.inner.read().unwrap_or_else(|p| p.into_inner()).status == MessageStatus::Failed
    }
}

// ============================================================================
// Message trait impl
// ============================================================================

impl Message for WorkUnit {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let elapsed = self.started_at.elapsed();

        match inner.status {
            MessageStatus::InProgress => {
                // Provider text is a VM wire program. Unlike ordinary prose
                // streaming, its source is itself an inspectable artifact and
                // must remain visible while tokens arrive; otherwise it looks
                // as though Co-Forth/Lisp was eaten until the final frame.
                if let WorkUnitPresentation::ProgramSource { language } = &inner.presentation {
                    let secs = elapsed.as_secs();
                    let mut out = format!(
                        "{}→ program ({language}){} {}{}…{}",
                        GRAY,
                        RESET,
                        GRAY_DIM,
                        fmt_elapsed(secs),
                        RESET
                    );
                    if !inner.response_text.is_empty() {
                        out.push('\n');
                        out.push_str(&inner.response_text);
                    }
                    return out;
                }

                // A running VM program can emit progressive output long
                // before it completes. This is already a portable side
                // effect, not an assistant message or a spinner; hiding it
                // until completion made `say` chunks appear and then vanish
                // during longer programs.
                if matches!(
                    inner.presentation,
                    WorkUnitPresentation::ProgramOutput { .. }
                ) && program_output_has_visible_state(&inner)
                {
                    return format_program_output(&inner);
                }

                if let WorkUnitPresentation::Activity { title } = &inner.presentation {
                    let secs = elapsed.as_secs();
                    let mut out = format!(
                        "{}⏺{} {title} {}({} · working){}",
                        CYAN,
                        RESET,
                        GRAY_DIM,
                        fmt_elapsed(secs),
                        RESET
                    );
                    for row in &inner.rows {
                        out.push('\n');
                        out.push_str(&format_row_themed(row, colors, DiffColorMode::production()));
                    }
                    return out;
                }

                // Once a provider turn has requested tools, the unit represents
                // the entire query-level tool loop rather than a generic model
                // spinner. Keep that stable title while later tool-result
                // continuations append more rows.
                if !inner.rows.is_empty() {
                    let secs = elapsed.as_secs();
                    let mut out = format!(
                        "{}⏺{} Tools {}({} · working){}",
                        CYAN,
                        RESET,
                        GRAY_DIM,
                        fmt_elapsed(secs),
                        RESET
                    );
                    for row in &inner.rows {
                        out.push('\n');
                        out.push_str(&format_row_themed(row, colors, DiffColorMode::production()));
                    }
                    return out;
                }

                // Time-driven throb: frame changes every 200 ms, no external counter
                let frame_idx = (elapsed.as_millis() / 200) as usize % THROB_FRAMES.len();
                let icon = THROB_FRAMES[frame_idx];
                let secs = elapsed.as_secs();

                let stats = if inner.token_count == 0 {
                    format!("{} · thinking", fmt_elapsed(secs))
                } else {
                    format!(
                        "{} · ↓ {} tokens",
                        fmt_elapsed(secs),
                        fmt_tokens(inner.token_count)
                    )
                };

                let mut out = format!(
                    "{}{}{}  {}… ({}){}",
                    CYAN, icon, RESET, self.verb, stats, RESET
                );

                for row in &inner.rows {
                    out.push('\n');
                    out.push_str(&format_row_themed(row, colors, DiffColorMode::production()));
                }

                out
            }

            MessageStatus::Complete | MessageStatus::Failed => {
                // Use captured elapsed (stable), fall back to live elapsed before first commit
                let secs = inner.elapsed_at_finish.unwrap_or(elapsed).as_secs();
                let timing = if inner.token_count > 0 {
                    format!(
                        " {}({} · {} tokens){}",
                        GRAY_DIM,
                        fmt_elapsed(secs),
                        fmt_tokens(inner.token_count),
                        RESET
                    )
                } else if secs > 0 {
                    format!(" {}({}){}", GRAY_DIM, fmt_elapsed(secs), RESET)
                } else {
                    String::new()
                };

                let mut out = match &inner.presentation {
                    WorkUnitPresentation::Assistant => {
                        if inner.response_text.is_empty() {
                            let title = if inner.rows.is_empty() {
                                String::new()
                            } else if inner.rows.len() == 1 {
                                " Tools".to_string()
                            } else {
                                format!(" Tools ({})", inner.rows.len())
                            };
                            format!("{}⏺{}{}{}", CYAN, RESET, title, timing)
                        } else {
                            format!("{}⏺{} {}{}", CYAN, RESET, inner.response_text, timing)
                        }
                    }
                    WorkUnitPresentation::Activity { title } => {
                        format!("{}⏺{} {title}{timing}", CYAN, RESET)
                    }
                    WorkUnitPresentation::ProgramSource { language } => {
                        let mut source = format!("{}→ program ({language}){}", GRAY, RESET);
                        if !inner.response_text.is_empty() {
                            source.push('\n');
                            source.push_str(&inner.response_text);
                        }
                        source.push_str(&timing);
                        source
                    }
                    WorkUnitPresentation::ProgramOutput { .. } => {
                        // VM output is a portable side effect, not assistant
                        // prose. Preserve it exactly: timing belongs to the
                        // source/activity WorkUnit and must never become part
                        // of a `say`/output-handle payload.
                        format_program_output(&inner)
                    }
                };

                // Collapsed sub-rows: show what tools ran (label + summary + body lines)
                for row in &inner.rows {
                    out.push('\n');
                    out.push_str(&format_row_collapsed(
                        row,
                        colors,
                        DiffColorMode::production(),
                    ));
                }

                out
            }
        }
    }

    fn status(&self) -> MessageStatus {
        self.inner.read().unwrap_or_else(|p| p.into_inner()).status
    }

    fn content(&self) -> String {
        self.inner
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .response_text
            .clone()
    }

    fn complete_transcript(&self, colors: &ColorScheme) -> String {
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let mut out = format_work_unit_header(&inner);
        for row in &inner.rows {
            out.push('\n');
            out.push_str(&format_row_themed(row, colors, DiffColorMode::production()));
        }
        out
    }

    fn transcript_row(&self, colors: &ColorScheme) -> Option<TranscriptRow> {
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let children = inner
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| match row.presentation {
                WorkRowPresentation::Activity => {
                    transcript_activity_row(self.id, index, row, colors)
                }
                WorkRowPresentation::Tool => transcript_tool_row(self.id, index, row, colors),
            })
            .collect();
        let (kind, label, body, default_expanded) = match &inner.presentation {
            WorkUnitPresentation::Assistant if !inner.rows.is_empty() => {
                let actionable = inner.rows.iter().any(tool_row_requires_default_expansion);
                (
                    TranscriptRowKind::ToolGroup,
                    compact_tool_group_label(&inner.rows),
                    lines(&inner.response_text),
                    inner.status == MessageStatus::InProgress || actionable,
                )
            }
            WorkUnitPresentation::Assistant => (
                TranscriptRowKind::Response,
                "Assistant response".to_string(),
                lines(&inner.response_text),
                true,
            ),
            WorkUnitPresentation::Activity { title } => {
                let actionable = inner.rows.iter().any(tool_row_requires_default_expansion);
                (
                    TranscriptRowKind::Activity,
                    compact_activity_group_label(title, &inner.rows),
                    Vec::new(),
                    inner.status == MessageStatus::InProgress || actionable,
                )
            }
            WorkUnitPresentation::ProgramSource { language } => (
                TranscriptRowKind::Program,
                format!("Program source ({language})"),
                lines(&inner.response_text),
                inner.status != MessageStatus::Complete,
            ),
            WorkUnitPresentation::ProgramOutput { title: None }
                if program_output_is_assistant(&inner) =>
            {
                (
                    TranscriptRowKind::Response,
                    "Assistant response".to_string(),
                    program_output_lines(&inner),
                    true,
                )
            }
            WorkUnitPresentation::ProgramOutput { title } => (
                TranscriptRowKind::Output,
                title
                    .clone()
                    .unwrap_or_else(|| "Program output".to_string()),
                program_output_lines(&inner),
                true,
            ),
        };

        Some(TranscriptRow {
            id: TranscriptRowId {
                message_id: self.id,
                path: vec![0],
            },
            kind,
            label,
            body,
            children,
            default_expanded,
        })
    }

    fn background_style(&self, colors: &ColorScheme) -> Option<ratatui::style::Style> {
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        let band = match &inner.presentation {
            WorkUnitPresentation::ProgramSource { .. } => MessageBand::ProgramSource,
            WorkUnitPresentation::ProgramOutput { title: None }
                if program_output_is_assistant(&inner) =>
            {
                MessageBand::Assistant
            }
            WorkUnitPresentation::ProgramOutput { .. } => MessageBand::ProgramOutput,
            WorkUnitPresentation::Assistant | WorkUnitPresentation::Activity { .. } => {
                MessageBand::Assistant
            }
        };
        Some(colors.message_band_style(band))
    }

    fn background_style_for_line(
        &self,
        colors: &ColorScheme,
        line_index: usize,
        line_count: usize,
    ) -> Option<ratatui::style::Style> {
        let inner = self.inner.read().unwrap_or_else(|p| p.into_inner());
        if inner.rows.is_empty() {
            let band = match &inner.presentation {
                WorkUnitPresentation::ProgramSource { .. } => MessageBand::ProgramSource,
                WorkUnitPresentation::ProgramOutput { title: None }
                    if program_output_is_assistant(&inner) =>
                {
                    MessageBand::Assistant
                }
                WorkUnitPresentation::ProgramOutput { .. } => MessageBand::ProgramOutput,
                WorkUnitPresentation::Assistant | WorkUnitPresentation::Activity { .. } => {
                    MessageBand::Assistant
                }
            };
            return Some(colors.message_band_style(band));
        }

        if inner.status == MessageStatus::InProgress {
            let band = match &inner.presentation {
                WorkUnitPresentation::ProgramSource { .. } => MessageBand::ProgramSource,
                WorkUnitPresentation::ProgramOutput { .. }
                    if program_output_has_visible_state(&inner) =>
                {
                    MessageBand::ProgramOutput
                }
                _ => MessageBand::Tool,
            };
            return Some(colors.message_band_style(band));
        }

        let tool_line_count = inner
            .rows
            .iter()
            .map(|row| {
                format_row_collapsed(row, colors, DiffColorMode::production())
                    .lines()
                    .count()
            })
            .sum::<usize>();
        let band = if line_index >= line_count.saturating_sub(tool_line_count) {
            MessageBand::Tool
        } else {
            match &inner.presentation {
                WorkUnitPresentation::ProgramSource { .. } => MessageBand::ProgramSource,
                WorkUnitPresentation::ProgramOutput { title: None }
                    if program_output_is_assistant(&inner) =>
                {
                    MessageBand::Assistant
                }
                WorkUnitPresentation::ProgramOutput { .. } => MessageBand::ProgramOutput,
                WorkUnitPresentation::Assistant | WorkUnitPresentation::Activity { .. } => {
                    MessageBand::Assistant
                }
            }
        };
        Some(colors.message_band_style(band))
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn program_output_has_visible_state(inner: &WorkUnitInner) -> bool {
    !inner.response_text.is_empty()
        || inner.transient_status.is_some()
        || inner.progress.is_some()
        || matches!(
            &inner.presentation,
            WorkUnitPresentation::ProgramOutput { title: Some(_) }
        )
}

fn program_output_is_assistant(inner: &WorkUnitInner) -> bool {
    matches!(
        (&inner.presentation, inner.program_output_semantics),
        (
            WorkUnitPresentation::ProgramOutput { title: None },
            ProgramOutputSemantics::AssistantSay
        )
    )
}

fn lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').map(str::to_owned).collect()
    }
}

fn program_output_lines(inner: &WorkUnitInner) -> Vec<String> {
    let mut body = lines(&inner.response_text);
    if let Some(status) = &inner.transient_status {
        body.push(status.clone());
    }
    if let Some((completed, total)) = inner.progress {
        body.push(format_progress(completed, total));
    }
    body
}

fn transcript_tool_row(
    message_id: MessageId,
    index: usize,
    row: &WorkRow,
    colors: &ColorScheme,
) -> TranscriptRow {
    let summary = match &row.status {
        WorkRowStatus::Running => "running".to_string(),
        WorkRowStatus::Complete(summary) if summary.is_empty() => "complete".to_string(),
        WorkRowStatus::Complete(summary) => summary.clone(),
        WorkRowStatus::Error(error) => format!("failed: {error}"),
    };
    let input = TranscriptRow {
        id: TranscriptRowId {
            message_id,
            path: vec![1, index as u32, 0],
        },
        kind: TranscriptRowKind::Input,
        label: "Input".to_string(),
        body: vec![row.label.clone()],
        children: Vec::new(),
        default_expanded: false,
    };
    let output_body = if let Some(diffs) = &row.diffs {
        let mut body = row.body_lines.clone();
        body.extend(
            render_files(diffs, colors, DiffColorMode::production())
                .lines()
                .map(str::to_owned),
        );
        body
    } else {
        row.body_lines.clone()
    };
    let actionable = tool_row_requires_default_expansion(row);
    let mut children = vec![input];
    if !output_body.is_empty() {
        children.push(TranscriptRow {
            id: TranscriptRowId {
                message_id,
                path: vec![1, index as u32, 1],
            },
            kind: TranscriptRowKind::ToolOutput,
            label: format!("Output ({})", output_body.len()),
            body: output_body,
            children: Vec::new(),
            default_expanded: matches!(row.status, WorkRowStatus::Running) || actionable,
        });
    }
    TranscriptRow {
        id: TranscriptRowId {
            message_id,
            path: vec![1, index as u32],
        },
        kind: TranscriptRowKind::ToolCall,
        label: format!("{} — {summary}", row.label),
        body: Vec::new(),
        children,
        default_expanded: matches!(row.status, WorkRowStatus::Running) || actionable,
    }
}

fn transcript_activity_row(
    message_id: MessageId,
    index: usize,
    row: &WorkRow,
    colors: &ColorScheme,
) -> TranscriptRow {
    let summary = match &row.status {
        WorkRowStatus::Running => "running".to_string(),
        WorkRowStatus::Complete(summary) if summary.is_empty() => "complete".to_string(),
        WorkRowStatus::Complete(summary) => summary.clone(),
        WorkRowStatus::Error(error) => format!("failed: {error}"),
    };
    let body = if let Some(diffs) = &row.diffs {
        let mut body = row.body_lines.clone();
        body.extend(
            render_files(diffs, colors, DiffColorMode::production())
                .lines()
                .map(str::to_owned),
        );
        body
    } else {
        row.body_lines.clone()
    };
    TranscriptRow {
        id: TranscriptRowId {
            message_id,
            path: vec![1, index as u32],
        },
        kind: TranscriptRowKind::Activity,
        label: format!("{} — {summary}", row.label),
        body,
        children: Vec::new(),
        default_expanded: tool_row_requires_default_expansion(row),
    }
}

fn tool_row_requires_default_expansion(row: &WorkRow) -> bool {
    matches!(row.status, WorkRowStatus::Running)
        || (matches!(&row.status, WorkRowStatus::Complete(summary) if summary.trim().is_empty())
            && (row.diffs.as_ref().is_some_and(|diffs| !diffs.is_empty())
                || !row.body_lines.is_empty()))
}

fn compact_tool_group_label(rows: &[WorkRow]) -> String {
    let noun = if rows.len() == 1 { "call" } else { "calls" };
    let mut label = format!("Tools ({} {noun})", rows.len());
    let Some((call, error)) = rows.iter().find_map(|row| match &row.status {
        WorkRowStatus::Error(error) => Some((
            compact_summary_text(&row.label, 60),
            compact_summary_text(error, 120),
        )),
        _ => None,
    }) else {
        return label;
    };
    label.push_str(" — ");
    label.push_str(&call);
    label.push_str(" failed: ");
    label.push_str(&error);
    label
}

fn compact_activity_group_label(title: &str, rows: &[WorkRow]) -> String {
    let mut label = title.to_string();
    let Some((activity, error)) = rows.iter().find_map(|row| match &row.status {
        WorkRowStatus::Error(error) => {
            let activity = row
                .label
                .strip_prefix(title)
                .map(|suffix| suffix.trim_start_matches(|c| c == ' ' || c == '·'))
                .filter(|suffix| !suffix.is_empty())
                .unwrap_or(&row.label);
            let error = error.strip_prefix("failed: ").unwrap_or(error);
            Some((
                compact_summary_text(activity, 60),
                compact_summary_text(error, 120),
            ))
        }
        _ => None,
    }) else {
        return label;
    };
    label.push_str(" — ");
    label.push_str(&activity);
    label.push_str(" failed: ");
    label.push_str(&error);
    label
}

fn compact_summary_text(text: &str, max_chars: usize) -> String {
    let first_line = text.lines().next().unwrap_or_default().trim();
    let mut compact = first_line.chars().take(max_chars).collect::<String>();
    if first_line.chars().count() > max_chars {
        compact.push('…');
    }
    compact
}

fn format_work_unit_header(inner: &WorkUnitInner) -> String {
    match &inner.presentation {
        WorkUnitPresentation::Assistant if inner.response_text.is_empty() => {
            format!("⏺ Tools ({})", inner.rows.len())
        }
        WorkUnitPresentation::Assistant => format!("⏺ {}", inner.response_text),
        WorkUnitPresentation::Activity { title } => format!("⏺ {title}"),
        WorkUnitPresentation::ProgramSource { language } => {
            if inner.response_text.is_empty() {
                format!("→ program ({language})")
            } else {
                format!("→ program ({language})\n{}", inner.response_text)
            }
        }
        WorkUnitPresentation::ProgramOutput { .. } => format_program_output(inner),
    }
}

/// Keep reactive output-handle state structurally distinct from the emitted
/// response body. Plain `say` output remains byte-for-byte unchanged, while
/// an explicit handle gets a compact labelled view that can be updated in
/// place by the shadow-buffer renderer.
fn format_program_output(inner: &WorkUnitInner) -> String {
    let WorkUnitPresentation::ProgramOutput { title } = &inner.presentation else {
        return inner.response_text.clone();
    };
    let Some(title) = title else {
        return inner.response_text.clone();
    };

    let mut lines = vec![title.clone()];
    if !inner.response_text.is_empty() {
        lines.push(inner.response_text.clone());
    }
    if let Some(status) = &inner.transient_status {
        lines.push(status.clone());
    }
    if let Some((completed, total)) = inner.progress {
        lines.push(format_progress(completed, total));
    }
    lines.join("\n")
}

fn is_diff_start(line: &str) -> bool {
    line.starts_with("--- ") || line.starts_with("diff --git ") || line.starts_with("Binary files ")
}

fn format_progress(completed: u64, total: Option<u64>) -> String {
    const WIDTH: usize = 20;
    match total {
        Some(total) if total > 0 => {
            let filled = ((completed.saturating_mul(WIDTH as u64) / total) as usize).min(WIDTH);
            format!(
                "[{}{}] {completed} / {total}",
                "█".repeat(filled),
                "░".repeat(WIDTH - filled)
            )
        }
        Some(total) => format!("[{}] {completed} / {total}", "░".repeat(WIDTH)),
        None => format!("[{}] {completed}", "…".repeat(WIDTH)),
    }
}

fn format_row(row: &WorkRow) -> String {
    format_row_themed(row, &ColorScheme::default(), DiffColorMode::Theme)
}

fn format_row_themed(row: &WorkRow, colors: &ColorScheme, diff_mode: DiffColorMode) -> String {
    match &row.status {
        WorkRowStatus::Running => {
            let mut out = format!("  {}⎿{} {}{}…{}", GRAY, RESET, row.label, GRAY_DIM, RESET);
            // Show last 3 live output lines (sliding window while command runs)
            if !row.body_lines.is_empty() {
                let start = row.body_lines.len().saturating_sub(3);
                for line in &row.body_lines[start..] {
                    out.push('\n');
                    out.push_str(&format!("    {}{}{}", GRAY_DIM, line, RESET));
                }
            }
            out
        }
        WorkRowStatus::Complete(summary) => {
            // Use captured elapsed time (not recalculated) so scrollback timing is stable
            let timing = row
                .elapsed_at_finish
                .filter(|d| d.as_secs() >= 1)
                .map(|d| format!(" {}({}){}", GRAY_DIM, fmt_elapsed(d.as_secs()), RESET))
                .unwrap_or_default();
            let mut out = if summary.is_empty() {
                format!("  {}⎿{} {}{}", GRAY, RESET, row.label, timing)
            } else {
                format!(
                    "  {}⎿{} {} {}{}{}{}",
                    GRAY, RESET, row.label, GRAY_DIM, summary, RESET, timing
                )
            };
            // Render body lines (diff, bash output, grep matches, etc.) indented below
            if let Some(diffs) = &row.diffs {
                for line in &row.body_lines {
                    out.push('\n');
                    out.push_str(&format!(
                        "    {}",
                        crate::cli::diff::sanitize_terminal(line)
                    ));
                }
                for line in render_files(diffs, colors, diff_mode).lines() {
                    out.push('\n');
                    out.push_str(&format!("    {line}"));
                }
            } else {
                for line in &row.body_lines {
                    out.push('\n');
                    out.push_str(&format!(
                        "    {}",
                        crate::cli::diff::sanitize_terminal(line)
                    ));
                }
            }
            out
        }
        WorkRowStatus::Error(err) => {
            let timing = row
                .elapsed_at_finish
                .filter(|d| d.as_secs() >= 1)
                .map(|d| format!(" {}({}){}", GRAY_DIM, fmt_elapsed(d.as_secs()), RESET))
                .unwrap_or_default();
            format!(
                "  {}⎿{} {} {}❌ {}{}{}",
                GRAY, RESET, row.label, RED_COLOR, err, RESET, timing
            )
        }
    }
}

/// Compact one-line row for the collapsed (Complete) state.
/// Same as `format_row` but skips body lines — keeps scrollback tidy.
fn format_row_collapsed(row: &WorkRow, colors: &ColorScheme, diff_mode: DiffColorMode) -> String {
    match &row.status {
        WorkRowStatus::Running => {
            // Shouldn't happen in Complete state, but render gracefully.
            format!("  {}⎿{} {}{}…{}", GRAY, RESET, row.label, GRAY_DIM, RESET)
        }
        WorkRowStatus::Complete(summary) => {
            let timing = row
                .elapsed_at_finish
                .filter(|d| d.as_secs() >= 1)
                .map(|d| format!(" {}({}){}", GRAY_DIM, fmt_elapsed(d.as_secs()), RESET))
                .unwrap_or_default();
            let mut out = if summary.is_empty() {
                format!("  {}⎿{} {}{}", GRAY, RESET, row.label, timing)
            } else {
                format!(
                    "  {}⎿{} {} {}{}{}{}",
                    GRAY, RESET, row.label, GRAY_DIM, summary, RESET, timing
                )
            };
            if let Some(diffs) = &row.diffs {
                for line in &row.body_lines {
                    out.push('\n');
                    out.push_str(&format!(
                        "    {}",
                        crate::cli::diff::sanitize_terminal(line)
                    ));
                }
                let rendered = render_files(diffs, colors, diff_mode);
                let lines: Vec<_> = rendered.lines().collect();
                let visible = lines.len().min(MAX_DIFF_PREVIEW_LINES);
                for line in &lines[..visible] {
                    out.push('\n');
                    out.push_str(&format!("    {line}"));
                }
                if visible < lines.len() {
                    out.push_str(&format!(
                        "\n    … {} more diff lines",
                        lines.len() - visible
                    ));
                }
            } else {
                for line in &row.body_lines {
                    out.push('\n');
                    out.push_str(&format!(
                        "    {}",
                        crate::cli::diff::sanitize_terminal(line)
                    ));
                }
            }
            out
        }
        WorkRowStatus::Error(err) => {
            let timing = row
                .elapsed_at_finish
                .filter(|d| d.as_secs() >= 1)
                .map(|d| format!(" {}({}){}", GRAY_DIM, fmt_elapsed(d.as_secs()), RESET))
                .unwrap_or_default();
            format!(
                "  {}⎿{} {} {}❌ {}{}{}",
                GRAY, RESET, row.label, RED_COLOR, err, RESET, timing
            )
        }
    }
}

fn fmt_elapsed(secs: u64) -> String {
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

fn fmt_tokens(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{}", n)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn colors() -> ColorScheme {
        ColorScheme::default()
    }

    // ── Construction ─────────────────────────────────────────────────────────

    #[test]
    fn test_new_defaults() {
        let wu = WorkUnit::new("Channeling");
        assert_eq!(wu.verb, "Channeling");
        assert_eq!(wu.status(), MessageStatus::InProgress);
        assert_eq!(wu.content(), "");
    }

    #[test]
    fn test_structured_diff_direct_and_retained_render_match() {
        let diff = FileDiff::parse("--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n")
            .unwrap();
        let direct = diff.render(&colors(), DiffColorMode::NoColor);
        let wu = WorkUnit::new("Tools");
        let row = wu.add_row("edit(src/a.rs)");
        wu.complete_row_with_diff(row, diff);
        wu.set_complete();
        let retained = wu.format(&colors());
        for line in direct.lines() {
            assert!(
                retained.contains(line),
                "missing retained diff line: {line:?}\n{retained}"
            );
        }
    }

    #[test]
    fn test_edit_and_write_tool_display_payloads_survive_retained_work_unit() {
        for tool in ["edit", "write"] {
            let raw = FileDiff::from_texts("src/file.txt", "old\n", "new\nmore\n").to_unified();
            let (summary, body) =
                crate::cli::repl_event::tool_display::tool_result_to_display(tool, &raw);
            let wu = WorkUnit::new("Tools");
            let row = wu.add_row(format!("{tool}(src/file.txt)"));
            wu.complete_row_with_body(row, summary, body);
            wu.set_complete();
            let rendered = wu.format(&colors());
            assert!(rendered.contains("src/file.txt  +2 -1"), "{rendered}");
            assert!(rendered.contains("- old"), "{rendered}");
            assert!(rendered.contains("+ new"), "{rendered}");
            assert!(!rendered.contains("\x1b]"), "{rendered}");
        }
    }

    #[test]
    fn test_replayed_header_first_diff_reconstructs_cached_paths_and_markers() {
        let raw = FileDiff::from_texts("src/replayed.txt", "old\n", "new").to_unified();
        let mut lines = raw.lines();
        let summary = lines.next().unwrap().to_owned();
        let body = lines.map(str::to_owned).collect();
        let wu = WorkUnit::new("Reconnected tools");
        let row = wu.add_row("edit(src/replayed.txt)");
        wu.complete_row_with_body(row, summary, body);
        wu.set_complete();

        let inner = wu.inner.read().unwrap();
        let cached = inner.rows[row].diffs.as_deref().unwrap();
        assert_eq!(cached[0].display_path(), "src/replayed.txt");
        assert!(cached[0]
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .any(|line| line.kind == crate::cli::diff::DiffLineKind::NoNewline));
        assert!(matches!(
            &inner.rows[row].status,
            WorkRowStatus::Complete(summary) if summary.is_empty()
        ));
    }

    #[test]
    fn test_replayed_git_multi_file_diff_reconstructs_without_path_bleed() {
        let raw = "diff --git a/old.txt b/new.txt\nrename from old.txt\nrename to new.txt\n--- a/old.txt\n+++ b/new.txt\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/image.bin b/image.bin\nBinary files a/image.bin and b/image.bin differ\n";
        let mut lines = raw.lines();
        let summary = lines.next().unwrap().to_owned();
        let body = lines.map(str::to_owned).collect();
        let wu = WorkUnit::new("Reconnected tools");
        let row = wu.add_row("edit(replayed)");
        wu.complete_row_with_body(row, summary, body);

        let inner = wu.inner.read().unwrap();
        let cached = inner.rows[row].diffs.as_deref().unwrap();
        assert_eq!(cached.len(), 2);
        assert_eq!(
            (cached[0].old_path.as_str(), cached[0].new_path.as_str()),
            ("old.txt", "new.txt")
        );
        assert!(cached[0].is_rename());
        assert_eq!(cached[1].display_path(), "image.bin");
        assert!(cached[1].binary);
    }

    #[test]
    fn test_cached_row_renderer_supports_dark_light_and_no_color() {
        let diff = FileDiff::from_texts("x", "old\n", "new\n");
        let row = WorkRow {
            label: "edit(x)".into(),
            status: WorkRowStatus::Complete("+1 -1".into()),
            presentation: WorkRowPresentation::Tool,
            started_at: Instant::now(),
            elapsed_at_finish: None,
            body_lines: Vec::new(),
            diffs: Some(vec![diff]),
        };
        let dark = format_row_collapsed(
            &row,
            &crate::config::ColorTheme::Dark.to_scheme(),
            DiffColorMode::Theme,
        );
        let light = format_row_collapsed(
            &row,
            &crate::config::ColorTheme::Light.to_scheme(),
            DiffColorMode::Theme,
        );
        let plain = format_row_collapsed(&row, &colors(), DiffColorMode::NoColor);
        assert_ne!(dark, light);
        assert!(dark.contains("38;2;126;231;135"));
        assert!(light.contains("38;2;0;92;38"));
        assert!(!plain.contains("38;2;"));
    }

    #[test]
    fn test_completed_diff_has_one_header_and_bounded_preview_without_false_action() {
        let old = (0..80).map(|i| format!("old {i}\n")).collect::<String>();
        let new = (0..80).map(|i| format!("new {i}\n")).collect::<String>();
        let wu = WorkUnit::new("Tools");
        let row = wu.add_row("edit(src/large.rs)");
        wu.complete_row_with_diff(row, FileDiff::from_texts("src/large.rs", &old, &new));
        wu.set_complete();

        let preview = wu.format(&colors());
        assert_eq!(preview.matches("src/large.rs  +80 -80").count(), 1);
        assert!(preview.contains("more diff lines"));
        assert!(!preview.contains("expand diff"));
        assert!(
            !preview.contains("new 79"),
            "collapsed retained rows must not leak the full patch: {preview}"
        );
    }

    #[test]
    fn test_ids_are_unique() {
        let wu1 = WorkUnit::new("A");
        let wu2 = WorkUnit::new("A");
        assert_ne!(wu1.id(), wu2.id());
    }

    // ── Status transitions ───────────────────────────────────────────────────

    #[test]
    fn test_set_complete() {
        let wu = WorkUnit::new("Test");
        assert_eq!(wu.status(), MessageStatus::InProgress);
        wu.set_complete();
        assert_eq!(wu.status(), MessageStatus::Complete);
    }

    #[test]
    fn test_set_failed() {
        let wu = WorkUnit::new("Test");
        wu.set_failed();
        assert_eq!(wu.status(), MessageStatus::Failed);
    }

    // ── Token / thinking ────────────────────────────────────────────────────

    #[test]
    fn test_add_tokens_single_call() {
        let wu = WorkUnit::new("X");
        wu.add_tokens("hello world foo bar"); // 4 words
        let inner = wu.inner.read().unwrap();
        assert_eq!(inner.token_count, 4);
    }

    #[test]
    fn test_add_tokens_accumulates() {
        let wu = WorkUnit::new("X");
        wu.add_tokens("a b c"); // 3
        wu.add_tokens("d e"); // 2
        let inner = wu.inner.read().unwrap();
        assert_eq!(inner.token_count, 5);
    }

    #[test]
    fn test_add_tokens_empty_string() {
        let wu = WorkUnit::new("X");
        wu.add_tokens("");
        let inner = wu.inner.read().unwrap();
        assert_eq!(inner.token_count, 0);
    }

    #[test]
    fn test_set_thinking() {
        let wu = WorkUnit::new("X");
        wu.set_thinking(true);
        assert!(wu.inner.read().unwrap().thinking);
        wu.set_thinking(false);
        assert!(!wu.inner.read().unwrap().thinking);
    }

    // ── Response text ────────────────────────────────────────────────────────

    #[test]
    fn test_set_response() {
        let wu = WorkUnit::new("X");
        wu.set_response("The answer is 42.");
        assert_eq!(wu.content(), "The answer is 42.");
    }

    #[test]
    fn test_append_response() {
        let wu = WorkUnit::new("X");
        wu.set_response("Hello");
        wu.append_response(" world");
        assert_eq!(wu.content(), "Hello world");
    }

    #[test]
    fn program_source_and_output_have_no_assistant_bullet() {
        let source = WorkUnit::new("ignored");
        source.set_program_source("lisp");
        source.set_response("(say \"hello\")");
        source.set_complete();
        let source_rendered = source.format(&colors());
        assert!(source_rendered.contains("→ program (lisp)"));
        assert!(!source_rendered.contains('⏺'));

        let output = WorkUnit::new("ignored");
        output.set_program_output();
        output.set_response("hello");
        output.add_tokens("one two");
        output.set_complete();
        assert_eq!(output.format(&colors()), "hello");
    }

    #[test]
    fn presentation_and_tool_state_choose_distinct_semantic_bands() {
        let colors = crate::config::ColorTheme::Dark.to_scheme();
        let assistant = WorkUnit::new("assistant");
        let source = WorkUnit::new("source");
        source.set_program_source("forth");
        let output = WorkUnit::new("output");
        output.set_program_output();
        let tools = WorkUnit::new("tools");
        tools.add_row("bash(test)");

        let styles = [
            assistant.background_style(&colors),
            source.background_style(&colors),
            output.background_style(&colors),
            tools.background_style_for_line(&colors, 0, 2),
        ];
        for (index, style) in styles.iter().enumerate() {
            assert!(styles[index + 1..].iter().all(|other| style != other));
        }
    }

    #[test]
    fn completed_response_and_collapsed_tools_keep_separate_bands() {
        let colors = crate::config::ColorTheme::Dark.to_scheme();
        let unit = WorkUnit::new("mixed");
        unit.set_response("assistant prose");
        let row = unit.add_row("bash(test)");
        unit.complete_row_with_body(row, "ok", vec!["tool output".into()]);
        unit.set_complete();

        let rendered = unit.format(&colors);
        let line_count = rendered.lines().count();
        let assistant = colors.message_band_style(MessageBand::Assistant);
        let tool = colors.message_band_style(MessageBand::Tool);

        assert_eq!(
            unit.background_style_for_line(&colors, 0, line_count),
            Some(assistant)
        );
        for line_index in 1..line_count {
            assert_eq!(
                unit.background_style_for_line(&colors, line_index, line_count),
                Some(tool)
            );
        }
    }

    #[test]
    fn terminal_tool_projection_collapses_after_running_and_omits_zero_output() {
        let unit = WorkUnit::new("Tools");
        let row = unit.add_row("catalog.validate provider=chatgpt");

        let running = unit.transcript_row(&colors()).unwrap();
        assert!(running.default_expanded);
        assert!(running.children[0].default_expanded);
        assert_eq!(running.children[0].children.len(), 1);
        assert_eq!(
            running.children[0].children[0].kind,
            TranscriptRowKind::Input
        );

        unit.fail_row(row, "catalog unavailable");
        unit.set_failed();
        let failed = unit.transcript_row(&colors()).unwrap();
        assert!(!failed.default_expanded);
        assert!(!failed.children[0].default_expanded);
        assert!(failed
            .label
            .contains("catalog.validate provider=chatgpt failed: catalog unavailable"));
        assert_eq!(failed.children[0].children.len(), 1);

        let completed = WorkUnit::new("Tools");
        let row = completed.add_row("read config");
        completed.complete_row_with_body(row, "3 lines", vec!["one".into(), "two".into()]);
        completed.set_complete();
        let projected = completed.transcript_row(&colors()).unwrap();
        assert!(!projected.default_expanded);
        assert!(!projected.children[0].default_expanded);
        assert_eq!(projected.children[0].children.len(), 2);
    }

    #[test]
    fn terminal_parent_status_does_not_hide_unresolved_running_tool() {
        for terminal_status in [MessageStatus::Complete, MessageStatus::Failed] {
            let unit = WorkUnit::new("Tools");
            unit.add_row("brain.call still running");
            match terminal_status {
                MessageStatus::Complete => unit.set_complete(),
                MessageStatus::Failed => unit.set_failed(),
                MessageStatus::InProgress => unreachable!(),
            }

            let projected = unit.transcript_row(&colors()).unwrap();
            assert!(projected.default_expanded);
            assert!(projected.children[0].default_expanded);
            assert!(projected.children[0].label.contains("running"));
        }
    }

    #[test]
    fn duplicate_brain_status_and_result_failures_have_one_compact_summary() {
        let unit = WorkUnit::new("Brain tools");
        unit.set_response("Speculative run 1234");
        let status = unit.add_activity_row("Speculative run 1234 · status");
        unit.fail_row(status, "catalog validation failed");
        let result = unit.add_activity_row("result");
        unit.fail_row(result, "catalog validation failed");
        unit.set_complete();
        let canonical_before_activity_projection = unit.complete_transcript(&colors());
        unit.set_activity_presentation("Speculative run 1234");

        let projected = unit.transcript_row(&colors()).unwrap();
        assert!(!projected.default_expanded);
        assert_eq!(projected.kind, TranscriptRowKind::Activity);
        assert!(!projected.label.contains("Tools"));
        assert!(!projected.label.contains("calls"));
        assert_eq!(
            projected.label.matches("catalog validation failed").count(),
            1
        );
        assert!(projected.label.contains("status failed"));
        assert_eq!(projected.children.len(), 2);
        assert!(projected.children[0].label.contains("status"));
        assert!(projected.children[1].label.starts_with("result"));

        let canonical = unit.complete_transcript(&colors());
        assert_eq!(canonical, canonical_before_activity_projection);
        assert_eq!(canonical.matches("catalog validation failed").count(), 2);
        assert!(canonical.contains("Speculative run 1234 · status"));
        assert!(canonical.contains("result"));
    }

    #[test]
    fn assistant_tool_rows_keep_tool_group_semantics() {
        let unit = WorkUnit::new("Tools");
        let row = unit.add_row("read config");
        unit.complete_row(row, "3 lines");
        unit.set_complete();

        let projected = unit.transcript_row(&colors()).unwrap();
        assert_eq!(projected.kind, TranscriptRowKind::ToolGroup);
        assert_eq!(projected.label, "Tools (1 call)");
    }

    #[test]
    fn unsummarized_structured_output_remains_visible_by_default() {
        let unit = WorkUnit::new("Tools");
        let row = unit.add_row("edit config.toml");
        unit.complete_row_with_diff(
            row,
            FileDiff::parse("--- a/config.toml\n+++ b/config.toml\n@@ -1 +1 @@\n-old\n+new")
                .expect("valid diff"),
        );
        unit.set_complete();

        let projected = unit.transcript_row(&colors()).unwrap();
        assert!(projected.default_expanded);
        assert!(projected.children[0].default_expanded);
        let output = projected.children[0]
            .children
            .iter()
            .find(|child| child.kind == TranscriptRowKind::ToolOutput)
            .unwrap();
        assert!(output.default_expanded);
    }

    #[test]
    fn explicit_output_handle_keeps_body_status_and_progress_separate() {
        let output = WorkUnit::new("ignored");
        output.set_output_handle("Download");
        output.append_response("received metadata");
        output.set_transient_status(Some("transferring".into()));
        output.set_output_progress(2, Some(5));

        let in_progress = output.format(&colors());
        assert!(in_progress.contains("Download"));
        assert!(in_progress.contains("received metadata"));
        assert!(in_progress.contains("transferring"));
        assert!(in_progress.contains("2 / 5"));

        output.set_transient_status(None);
        output.set_complete();
        let completed = output.format(&colors());
        assert!(completed.contains("received metadata"));
        assert!(completed.contains("2 / 5"));
        assert!(!completed.contains("transferring"));
        assert!(!completed.contains('⏺'));
    }

    #[test]
    fn in_progress_program_source_keeps_received_wire_text_visible() {
        let source = WorkUnit::new("ignored");
        source.set_program_source("forth");
        source.append_response("s\"hello\" say");

        let rendered = source.format(&colors());
        assert!(rendered.contains("→ program (forth)"));
        assert!(rendered.contains("s\"hello\" say"));
        assert!(!rendered.contains("⏺"));
    }

    // ── Rows ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_add_row_returns_index() {
        let wu = WorkUnit::new("X");
        assert_eq!(wu.add_row("bash(ls)"), 0);
        assert_eq!(wu.add_row("read(foo.rs)"), 1);
        assert_eq!(wu.inner.read().unwrap().rows.len(), 2);
    }

    #[test]
    fn test_complete_row() {
        let wu = WorkUnit::new("X");
        let idx = wu.add_row("bash(ls)");
        wu.complete_row(idx, "3 files");
        let inner = wu.inner.read().unwrap();
        assert!(matches!(&inner.rows[0].status, WorkRowStatus::Complete(s) if s == "3 files"));
    }

    #[test]
    fn test_complete_row_empty_summary() {
        let wu = WorkUnit::new("X");
        let idx = wu.add_row("bash(true)");
        wu.complete_row(idx, "");
        let inner = wu.inner.read().unwrap();
        assert!(matches!(&inner.rows[0].status, WorkRowStatus::Complete(s) if s.is_empty()));
    }

    #[test]
    fn test_fail_row() {
        let wu = WorkUnit::new("X");
        let idx = wu.add_row("bash(rm -rf /)");
        wu.fail_row(idx, "permission denied");
        let inner = wu.inner.read().unwrap();
        assert!(
            matches!(&inner.rows[0].status, WorkRowStatus::Error(e) if e == "permission denied")
        );
    }

    #[test]
    fn test_out_of_bounds_row_ops_do_not_panic() {
        let wu = WorkUnit::new("X");
        wu.complete_row(99, "summary"); // should not panic
        wu.fail_row(99, "error"); // should not panic
    }

    // ── format() — InProgress ────────────────────────────────────────────────

    #[test]
    fn test_format_in_progress_thinking_phase() {
        let wu = WorkUnit::new("Channeling");
        // token_count == 0 → shows "thinking"
        let f = wu.format(&colors());
        assert!(f.contains("Channeling"), "should contain verb: {}", f);
        assert!(f.contains("thinking"), "should contain 'thinking': {}", f);
        let has_throb = THROB_FRAMES.iter().any(|fr| f.contains(fr));
        assert!(has_throb, "should contain a throb frame: {}", f);
    }

    #[test]
    fn test_format_in_progress_with_tokens() {
        let wu = WorkUnit::new("Channeling");
        wu.add_tokens("hello world foo bar baz"); // 5 words
        let f = wu.format(&colors());
        assert!(f.contains("Channeling"));
        assert!(f.contains("tokens"));
        assert!(f.contains("5"));
        assert!(!f.contains("thinking"));
    }

    #[test]
    fn test_format_in_progress_with_running_row() {
        let wu = WorkUnit::new("Channeling");
        wu.add_row("bash(git status)");
        let f = wu.format(&colors());
        assert!(f.contains("⎿"));
        assert!(f.contains("bash(git status)"));
        assert!(f.contains("…")); // running indicator
    }

    // ── format() — Complete ──────────────────────────────────────────────────

    #[test]
    fn test_format_complete_bare_bullet_when_no_text() {
        let wu = WorkUnit::new("Channeling");
        wu.set_complete();
        let f = wu.format(&colors());
        assert!(f.contains("⏺"), "should contain bullet: {}", f);
        assert!(
            !f.contains("Channeling"),
            "verb should be gone in complete state"
        );
    }

    #[test]
    fn test_format_complete_with_response_text() {
        let wu = WorkUnit::new("Channeling");
        wu.set_response("The answer is 42.");
        wu.set_complete();
        let f = wu.format(&colors());
        assert!(f.contains("⏺"));
        assert!(f.contains("The answer is 42."));
    }

    #[test]
    fn test_format_complete_with_rows() {
        let wu = WorkUnit::new("Channeling");
        let idx = wu.add_row("bash(ls)");
        wu.complete_row(idx, "3 files");
        wu.set_response("Done.");
        wu.set_complete();
        let f = wu.format(&colors());
        assert!(f.contains("⏺"));
        assert!(f.contains("Done."));
        assert!(f.contains("⎿"));
        assert!(f.contains("bash(ls)"));
        assert!(f.contains("3 files"));
    }

    #[test]
    fn completed_tool_only_unit_has_an_overall_title() {
        let wu = WorkUnit::new("Channeling");
        let first = wu.add_row("read(one.rs)");
        wu.complete_row(first, "10 lines");
        let second = wu.add_row("read(two.rs)");
        wu.complete_row(second, "20 lines");
        wu.set_complete();

        let formatted = wu.format(&colors());
        assert!(formatted.contains("⏺\u{1b}[0m Tools (2)"), "{formatted:?}");
    }

    #[test]
    fn running_tool_unit_has_a_stable_overall_title() {
        let wu = WorkUnit::new("Random spinner verb");
        wu.add_row("read(one.rs)");

        let formatted = wu.format(&colors());
        assert!(formatted.contains("⏺\u{1b}[0m Tools"), "{formatted:?}");
        assert!(!formatted.contains("Random spinner verb"), "{formatted:?}");
    }

    #[test]
    fn test_format_failed_shows_bullet() {
        let wu = WorkUnit::new("Channeling");
        wu.set_failed();
        let f = wu.format(&colors());
        assert!(f.contains("⏺"));
    }

    // ── format_row helpers ───────────────────────────────────────────────────

    #[test]
    fn test_format_row_running() {
        let row = WorkRow {
            label: "bash(echo hi)".into(),
            status: WorkRowStatus::Running,
            presentation: WorkRowPresentation::Tool,
            started_at: Instant::now(),
            elapsed_at_finish: None,
            body_lines: Vec::new(),
            diffs: None,
        };
        let f = format_row(&row);
        assert!(f.contains("⎿"));
        assert!(f.contains("bash(echo hi)"));
        assert!(f.contains("…"));
    }

    #[test]
    fn test_format_row_complete_with_summary() {
        let row = WorkRow {
            label: "read(foo.rs)".into(),
            status: WorkRowStatus::Complete("42 lines".into()),
            presentation: WorkRowPresentation::Tool,
            started_at: Instant::now(),
            elapsed_at_finish: None,
            body_lines: Vec::new(),
            diffs: None,
        };
        let f = format_row(&row);
        assert!(f.contains("⎿"));
        assert!(f.contains("read(foo.rs)"));
        assert!(f.contains("42 lines"));
    }

    #[test]
    fn test_format_row_complete_empty_summary() {
        let row = WorkRow {
            label: "bash(true)".into(),
            status: WorkRowStatus::Complete(String::new()),
            presentation: WorkRowPresentation::Tool,
            started_at: Instant::now(),
            elapsed_at_finish: None,
            body_lines: Vec::new(),
            diffs: None,
        };
        let f = format_row(&row);
        assert!(f.contains("⎿"));
        assert!(f.contains("bash(true)"));
        // No trailing ellipsis when complete
        assert!(!f.contains("…"));
    }

    #[test]
    fn test_format_row_error() {
        let row = WorkRow {
            label: "bash(bad cmd)".into(),
            status: WorkRowStatus::Error("exit 1".into()),
            presentation: WorkRowPresentation::Tool,
            started_at: Instant::now(),
            elapsed_at_finish: None,
            body_lines: Vec::new(),
            diffs: None,
        };
        let f = format_row(&row);
        assert!(f.contains("⎿"));
        assert!(f.contains("bash(bad cmd)"));
        assert!(f.contains("❌"));
        assert!(f.contains("exit 1"));
    }

    // ── complete_row_with_body ───────────────────────────────────────────────

    #[test]
    fn test_complete_row_with_body_renders_below_summary() {
        let wu = WorkUnit::new("X");
        let idx = wu.add_row("Edit(…/event_loop.rs)");
        wu.complete_row_with_body(
            idx,
            "Removed 3 lines",
            vec!["  line A".to_string(), "  line B".to_string()],
        );
        wu.set_complete();
        let f = wu.format(&ColorScheme::default());
        assert!(f.contains("Removed 3 lines"), "summary missing: {}", f);
        assert!(f.contains("line A"), "body line A missing: {}", f);
        assert!(f.contains("line B"), "body line B missing: {}", f);
        // Body lines must appear AFTER the summary line
        let summary_pos = f.find("Removed 3 lines").unwrap();
        let body_pos = f.find("line A").unwrap();
        assert!(body_pos > summary_pos, "body should follow summary");
    }

    #[test]
    fn test_complete_row_with_body_empty_body_is_fine() {
        let wu = WorkUnit::new("X");
        let idx = wu.add_row("Read(foo.rs)");
        wu.complete_row_with_body(idx, "42 lines", Vec::new());
        wu.set_complete();
        let f = wu.format(&ColorScheme::default());
        assert!(f.contains("42 lines"));
    }

    // ── fmt_elapsed / fmt_tokens ─────────────────────────────────────────────

    #[test]
    fn test_fmt_elapsed_seconds_only() {
        assert_eq!(fmt_elapsed(0), "0s");
        assert_eq!(fmt_elapsed(1), "1s");
        assert_eq!(fmt_elapsed(59), "59s");
    }

    #[test]
    fn test_fmt_elapsed_minutes() {
        assert_eq!(fmt_elapsed(60), "1m 0s");
        assert_eq!(fmt_elapsed(90), "1m 30s");
        assert_eq!(fmt_elapsed(125), "2m 5s");
    }

    #[test]
    fn test_fmt_tokens_small() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
    }

    #[test]
    fn test_fmt_tokens_thousands() {
        assert_eq!(fmt_tokens(1000), "1.0k");
        assert_eq!(fmt_tokens(1500), "1.5k");
        assert_eq!(fmt_tokens(9900), "9.9k");
    }

    // ── Timing (elapsed_at_finish) ───────────────────────────────────────────

    #[test]
    fn test_set_complete_captures_elapsed() {
        let wu = WorkUnit::new("X");
        std::thread::sleep(std::time::Duration::from_millis(5));
        wu.set_complete();
        let inner = wu.inner.read().unwrap();
        assert!(
            inner.elapsed_at_finish.is_some(),
            "elapsed_at_finish should be set after set_complete"
        );
        assert!(
            inner.elapsed_at_finish.unwrap().as_millis() >= 5,
            "elapsed should be at least 5ms"
        );
    }

    #[test]
    fn test_set_failed_captures_elapsed() {
        let wu = WorkUnit::new("X");
        wu.set_failed();
        let inner = wu.inner.read().unwrap();
        assert!(inner.elapsed_at_finish.is_some());
    }

    #[test]
    fn test_complete_row_captures_elapsed() {
        let wu = WorkUnit::new("X");
        let idx = wu.add_row("bash(sleep 0)");
        std::thread::sleep(std::time::Duration::from_millis(5));
        wu.complete_row(idx, "ok");
        let inner = wu.inner.read().unwrap();
        assert!(
            inner.rows[0].elapsed_at_finish.is_some(),
            "row elapsed should be captured at complete_row"
        );
    }

    #[test]
    fn test_fail_row_captures_elapsed() {
        let wu = WorkUnit::new("X");
        let idx = wu.add_row("bash(bad)");
        wu.fail_row(idx, "error");
        let inner = wu.inner.read().unwrap();
        assert!(inner.rows[0].elapsed_at_finish.is_some());
    }

    #[test]
    fn test_format_complete_shows_bullet() {
        let wu = WorkUnit::new("Channeling");
        wu.set_response("Done.");
        wu.set_complete();
        let f = wu.format(&colors());
        // Complete format always has the bullet
        assert!(f.contains("⏺"), "complete format should show bullet: {}", f);
        assert!(
            f.contains("Done."),
            "complete format should show response: {}",
            f
        );
    }

    #[test]
    fn test_format_complete_with_tokens_shows_token_count() {
        let wu = WorkUnit::new("Channeling");
        wu.add_tokens("hello world foo"); // 3 tokens
        wu.set_response("Done.");
        wu.set_complete();
        let f = wu.format(&colors());
        assert!(
            f.contains("tokens"),
            "complete format with tokens should say 'tokens': {}",
            f
        );
        assert!(
            f.contains("3"),
            "complete format should show token count: {}",
            f
        );
    }

    #[test]
    fn test_format_complete_row_timing_hidden_under_1s() {
        // A row that completes in < 1s should NOT show timing like "(0s)"
        let row = WorkRow {
            label: "bash(true)".into(),
            status: WorkRowStatus::Complete("ok".into()),
            presentation: WorkRowPresentation::Tool,
            started_at: Instant::now(),
            elapsed_at_finish: Some(std::time::Duration::from_millis(800)),
            body_lines: Vec::new(),
            diffs: None,
        };
        let f = format_row(&row);
        // The label contains "(true)" but timing should NOT appear as "(0s)" pattern
        assert!(
            !f.contains("(0s)"),
            "sub-second row should hide timing: {}",
            f
        );
        assert!(
            !f.contains("(800"),
            "sub-second row should hide timing: {}",
            f
        );
    }

    #[test]
    fn test_format_complete_row_timing_shown_over_1s() {
        // A row that completes in >= 1s SHOULD show timing
        let row = WorkRow {
            label: "bash(slow)".into(),
            status: WorkRowStatus::Complete("done".into()),
            presentation: WorkRowPresentation::Tool,
            started_at: Instant::now(),
            elapsed_at_finish: Some(std::time::Duration::from_secs(3)),
            body_lines: Vec::new(),
            diffs: None,
        };
        let f = format_row(&row);
        assert!(f.contains("3s"), "3-second row should show timing: {}", f);
    }

    // ── random_spinner_verb ──────────────────────────────────────────────────

    #[test]
    fn test_random_spinner_verb_is_non_empty() {
        let v = random_spinner_verb();
        assert!(!v.is_empty());
    }

    #[test]
    fn test_random_spinner_verb_is_in_word_list() {
        // Call it several times; every result must be in the curated list.
        for _ in 0..SPINNER_WORDS.len() * 2 {
            let v = random_spinner_verb();
            assert!(
                SPINNER_WORDS.contains(&v),
                "unexpected verb not in SPINNER_WORDS: {v}"
            );
        }
    }

    #[test]
    fn test_random_spinner_verb_cycles_through_all_words() {
        // Round-robin counter means after N calls we should have seen N distinct words
        // (assuming we start fresh, which we cannot guarantee in test, but we can at
        // least verify the set grows — call it 2×N times and check we get ≥ N/2 unique).
        let mut seen = std::collections::HashSet::new();
        for _ in 0..SPINNER_WORDS.len() * 3 {
            seen.insert(random_spinner_verb());
        }
        assert!(
            seen.len() >= SPINNER_WORDS.len() / 2,
            "expected to see at least half the word list; saw {}",
            seen.len()
        );
    }

    // ── Thread safety ────────────────────────────────────────────────────────

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WorkUnit>();
    }

    #[test]
    fn test_concurrent_updates() {
        use std::sync::Arc;
        use std::thread;

        let wu = Arc::new(WorkUnit::new("Parallel"));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let wu = Arc::clone(&wu);
                thread::spawn(move || {
                    wu.add_tokens("hello world");
                    wu.add_row("bash(ls)");
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        let inner = wu.inner.read().unwrap();
        assert_eq!(inner.token_count, 16); // 8 threads × 2 tokens
        assert_eq!(inner.rows.len(), 8);
    }
}

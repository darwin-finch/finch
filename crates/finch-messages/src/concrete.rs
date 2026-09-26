// Concrete Message Types
//
// Each message type has its own update interface appropriate for its use case.
// No need for downcasting - handlers receive concrete types directly.

use super::{ComponentAction, ComponentView, Message, MessageId, MessageStatus};
use crossterm::style::{Attribute, Color, SetAttribute, SetForegroundColor};
use finch_theme::{ColorScheme, ColorSpec, MessageBand};
use finch_ui_model::{
    LiveToolView, MemoryRecallRowView, MemoryRecalledView, OperationRowView, OperationView,
    ProgressView, StaticTextKind, StaticTextView, WorkRowStatus,
};
use std::fmt;
use std::sync::{Arc, RwLock};

/// Render a configured color using crossterm's terminal command formatter.
fn color_to_ansi(color: &ColorSpec) -> String {
    let color = match color {
        ColorSpec::Named(name) => match name.to_lowercase().as_str() {
            "black" => Color::Black,
            "red" => Color::DarkRed,
            "green" => Color::DarkGreen,
            "yellow" => Color::DarkYellow,
            "blue" => Color::DarkBlue,
            "magenta" => Color::DarkMagenta,
            "cyan" => Color::DarkCyan,
            "white" => Color::Grey,
            "gray" | "grey" | "darkgray" | "darkgrey" => Color::DarkGrey,
            "lightred" => Color::Red,
            "lightgreen" => Color::Green,
            "lightyellow" => Color::Yellow,
            "lightblue" => Color::Blue,
            "lightmagenta" => Color::Magenta,
            "lightcyan" => Color::Cyan,
            _ => Color::Grey,
        },
        ColorSpec::Rgb(r, g, b) => Color::Rgb {
            r: *r,
            g: *g,
            b: *b,
        },
    };
    SetForegroundColor(color).to_string()
}

const RESET: SetAttribute = SetAttribute(Attribute::Reset);

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

// ============================================================================
// UserQueryMessage - Immutable message for user input
// ============================================================================

/// User query message (immutable after creation)
pub struct UserQueryMessage {
    id: MessageId,
    content: String,
}

impl UserQueryMessage {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            content: content.into(),
        }
    }
}

impl Message for UserQueryMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        format!(
            "{} ❯ {}{}",
            color_to_ansi(&colors.messages.user),
            self.content,
            RESET
        )
    }

    fn status(&self) -> MessageStatus {
        MessageStatus::Complete
    }

    fn content(&self) -> String {
        self.content.clone()
    }

    fn background_style(&self, colors: &ColorScheme) -> Option<ratatui::style::Style> {
        Some(colors.message_band_style(MessageBand::LocalUser))
    }
}

// ============================================================================
// BrainParticipantMessage - attributed shared-Brain conversation event
// ============================================================================

/// An attributed message projected from a shared Brain. Prompt messages are
/// visibly addressed to the model; relay messages are conversation-only. A
/// stable participant-derived background lets multiple humans share one
/// transcript without conflating their turns with local system information.
pub struct BrainParticipantMessage {
    id: MessageId,
    subject: String,
    content: String,
    invokes_model: bool,
}

impl BrainParticipantMessage {
    pub fn new(
        subject: impl Into<String>,
        content: impl Into<String>,
        invokes_model: bool,
    ) -> Self {
        Self {
            id: MessageId::new(),
            subject: subject.into(),
            content: content.into(),
            invokes_model,
        }
    }

    fn palette_index(&self) -> usize {
        // FNV-1a is deliberately tiny and stable across processes/platforms;
        // DefaultHasher does not promise either property.
        let hash = self
            .subject
            .as_bytes()
            .iter()
            .fold(0xcbf29ce484222325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            });
        (hash as usize) % PARTICIPANT_PALETTE_SIZE
    }
}

const PARTICIPANT_PALETTE_SIZE: usize = 8;

impl Message for BrainParticipantMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        let marker = if self.invokes_model { '❯' } else { '◆' };
        format!(
            "{} {marker} {}: {}{}",
            color_to_ansi(&colors.messages.user),
            self.subject,
            self.content,
            RESET
        )
    }

    fn status(&self) -> MessageStatus {
        MessageStatus::Complete
    }

    fn content(&self) -> String {
        format!("{}: {}", self.subject, self.content)
    }

    fn background_style(&self, colors: &ColorScheme) -> Option<ratatui::style::Style> {
        Some(colors.message_band_style(MessageBand::Participant(self.palette_index())))
    }
}

// ============================================================================
// StreamingResponseMessage - Mutable message for Claude/Qwen responses
// ============================================================================

/// Streaming response message (for Claude/Qwen)
pub struct StreamingResponseMessage {
    id: MessageId,
    content: Arc<RwLock<String>>,
    status: Arc<RwLock<MessageStatus>>,
    thinking: Arc<RwLock<bool>>,
}

impl StreamingResponseMessage {
    pub fn new() -> Self {
        Self {
            id: MessageId::new(),
            content: Arc::new(RwLock::new(String::new())),
            status: Arc::new(RwLock::new(MessageStatus::InProgress)),
            thinking: Arc::new(RwLock::new(false)),
        }
    }

    /// Append a chunk of streamed text
    pub fn append_chunk(&self, text: &str) {
        match self.content.write() {
            Ok(mut content) => content.push_str(text),
            Err(poisoned) => {
                tracing::warn!(
                    "StreamingResponseMessage content lock poisoned in append_chunk, recovering"
                );
                let mut content = poisoned.into_inner();
                content.push_str(text);
            }
        }
    }

    /// Set whether the model is thinking (for UI indicator)
    pub fn set_thinking(&self, thinking: bool) {
        match self.thinking.write() {
            Ok(mut t) => *t = thinking,
            Err(poisoned) => {
                tracing::warn!(
                    "StreamingResponseMessage thinking lock poisoned in set_thinking, recovering"
                );
                *poisoned.into_inner() = thinking;
            }
        }
    }

    /// Mark this response as complete
    pub fn set_complete(&self) {
        match self.status.write() {
            Ok(mut s) => *s = MessageStatus::Complete,
            Err(poisoned) => {
                tracing::warn!(
                    "StreamingResponseMessage status lock poisoned in set_complete, recovering"
                );
                *poisoned.into_inner() = MessageStatus::Complete;
            }
        }
    }

    /// Mark this response as failed
    pub fn set_failed(&self) {
        match self.status.write() {
            Ok(mut s) => *s = MessageStatus::Failed,
            Err(poisoned) => {
                tracing::warn!(
                    "StreamingResponseMessage status lock poisoned in set_failed, recovering"
                );
                *poisoned.into_inner() = MessageStatus::Failed;
            }
        }
    }
}

impl Message for StreamingResponseMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        // Handle poisoned locks gracefully - recover with safe defaults
        let content = match self.content.read() {
            Ok(c) => c.clone(),
            Err(poisoned) => {
                tracing::warn!(
                    "StreamingResponseMessage content lock poisoned, using recovered data"
                );
                poisoned.into_inner().clone()
            }
        };

        let status = match self.status.read() {
            Ok(s) => *s,
            Err(poisoned) => {
                tracing::warn!(
                    "StreamingResponseMessage status lock poisoned, defaulting to InProgress"
                );
                *poisoned.into_inner()
            }
        };

        let thinking = match self.thinking.read() {
            Ok(t) => *t,
            Err(poisoned) => {
                tracing::warn!(
                    "StreamingResponseMessage thinking lock poisoned, defaulting to false"
                );
                *poisoned.into_inner()
            }
        };

        // No cleaning - already cleaned by daemon during streaming
        let text = content.clone();

        match status {
            MessageStatus::InProgress if thinking => {
                format!("{}⏺{} {}[thinking…]{}\n{}", CYAN, RESET, GRAY, RESET, text)
            }
            MessageStatus::InProgress => {
                if text.is_empty() {
                    // Waiting for first token — show bare bullet
                    format!("{}⏺{}", CYAN, RESET)
                } else {
                    // Streaming — trailing block cursor
                    format!("{}⏺{} {}▍", CYAN, RESET, text)
                }
            }
            MessageStatus::Failed => {
                format!(
                    "{}⏺{} {}❌ Response failed{}\n{}",
                    CYAN,
                    RESET,
                    color_to_ansi(&colors.messages.error),
                    RESET,
                    text
                )
            }
            MessageStatus::Complete => format!("{}⏺{} {}", CYAN, RESET, text),
        }
    }

    fn status(&self) -> MessageStatus {
        match self.status.read() {
            Ok(s) => *s,
            Err(poisoned) => {
                tracing::warn!("StreamingResponseMessage status lock poisoned in status(), using recovered data");
                *poisoned.into_inner()
            }
        }
    }

    fn content(&self) -> String {
        match self.content.read() {
            Ok(c) => c.clone(),
            Err(poisoned) => {
                tracing::warn!("StreamingResponseMessage content lock poisoned in content(), using recovered data");
                poisoned.into_inner().clone()
            }
        }
    }

    fn background_style(&self, colors: &ColorScheme) -> Option<ratatui::style::Style> {
        Some(colors.message_band_style(MessageBand::Assistant))
    }
}

impl Default for StreamingResponseMessage {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// ToolExecutionMessage - Message for tool execution with stdout/stderr
// ============================================================================

/// Tool execution message with separate stdout/stderr
pub struct ToolExecutionMessage {
    id: MessageId,
    tool_name: String,
    stdout: Arc<RwLock<String>>,
    stderr: Arc<RwLock<String>>,
    exit_code: Arc<RwLock<Option<i32>>>,
    status: Arc<RwLock<MessageStatus>>,
}

impl ToolExecutionMessage {
    pub fn new(tool_name: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            tool_name: tool_name.into(),
            stdout: Arc::new(RwLock::new(String::new())),
            stderr: Arc::new(RwLock::new(String::new())),
            exit_code: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(MessageStatus::InProgress)),
        }
    }

    /// Append to stdout
    pub fn append_stdout(&self, text: &str) {
        match self.stdout.write() {
            Ok(mut stdout) => stdout.push_str(text),
            Err(poisoned) => {
                tracing::warn!(
                    "ToolExecutionMessage stdout lock poisoned in append_stdout, recovering"
                );
                let mut stdout = poisoned.into_inner();
                stdout.push_str(text);
            }
        }
    }

    /// Append to stderr
    pub fn append_stderr(&self, text: &str) {
        match self.stderr.write() {
            Ok(mut stderr) => stderr.push_str(text),
            Err(poisoned) => {
                tracing::warn!(
                    "ToolExecutionMessage stderr lock poisoned in append_stderr, recovering"
                );
                let mut stderr = poisoned.into_inner();
                stderr.push_str(text);
            }
        }
    }

    /// Set exit code (marks as complete)
    pub fn set_exit_code(&self, code: i32) {
        match self.exit_code.write() {
            Ok(mut e) => *e = Some(code),
            Err(poisoned) => {
                tracing::warn!(
                    "ToolExecutionMessage exit_code lock poisoned in set_exit_code, recovering"
                );
                *poisoned.into_inner() = Some(code);
            }
        }
        match self.status.write() {
            Ok(mut s) => *s = MessageStatus::Complete,
            Err(poisoned) => {
                tracing::warn!(
                    "ToolExecutionMessage status lock poisoned in set_exit_code, recovering"
                );
                *poisoned.into_inner() = MessageStatus::Complete;
            }
        }
    }

    /// Mark as failed
    pub fn set_failed(&self) {
        match self.status.write() {
            Ok(mut s) => *s = MessageStatus::Failed,
            Err(poisoned) => {
                tracing::warn!(
                    "ToolExecutionMessage status lock poisoned in set_failed, recovering"
                );
                *poisoned.into_inner() = MessageStatus::Failed;
            }
        }
    }
}

impl Message for ToolExecutionMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        // Handle poisoned locks gracefully
        let stdout = match self.stdout.read() {
            Ok(s) => s.clone(),
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage stdout lock poisoned, using recovered data");
                poisoned.into_inner().clone()
            }
        };

        let stderr = match self.stderr.read() {
            Ok(s) => s.clone(),
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage stderr lock poisoned, using recovered data");
                poisoned.into_inner().clone()
            }
        };

        let exit_code = match self.exit_code.read() {
            Ok(e) => *e,
            Err(poisoned) => {
                tracing::warn!(
                    "ToolExecutionMessage exit_code lock poisoned, using recovered data"
                );
                *poisoned.into_inner()
            }
        };

        let mut result = format!(
            "{}[{}]{}",
            color_to_ansi(&colors.messages.tool),
            self.tool_name,
            RESET
        );

        if !stdout.is_empty() {
            result.push('\n');
            result.push_str(&stdout);
        }

        if !stderr.is_empty() {
            result.push('\n');
            result.push_str(&format!(
                "{}stderr: {}{}",
                color_to_ansi(&colors.messages.error),
                stderr,
                RESET
            ));
        }

        if let Some(code) = exit_code {
            result.push('\n');
            if code == 0 {
                result.push_str(&format!(
                    "{}✓ exit code: {}{}",
                    color_to_ansi(&colors.messages.system),
                    code,
                    RESET
                ));
            } else {
                result.push_str(&format!(
                    "{}✗ exit code: {}{}",
                    color_to_ansi(&colors.messages.error),
                    code,
                    RESET
                ));
            }
        }

        result
    }

    fn status(&self) -> MessageStatus {
        match self.status.read() {
            Ok(s) => *s,
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage status lock poisoned, using recovered data");
                *poisoned.into_inner()
            }
        }
    }

    fn content(&self) -> String {
        let stdout = match self.stdout.read() {
            Ok(s) => s.clone(),
            Err(poisoned) => {
                tracing::warn!(
                    "ToolExecutionMessage stdout lock poisoned in content(), using recovered data"
                );
                poisoned.into_inner().clone()
            }
        };

        let stderr = match self.stderr.read() {
            Ok(s) => s.clone(),
            Err(poisoned) => {
                tracing::warn!(
                    "ToolExecutionMessage stderr lock poisoned in content(), using recovered data"
                );
                poisoned.into_inner().clone()
            }
        };

        format!("{}\n{}", stdout, stderr)
    }

    fn background_style(&self, colors: &ColorScheme) -> Option<ratatui::style::Style> {
        Some(colors.message_band_style(MessageBand::Tool))
    }
}

// ============================================================================
// LiveToolMessage - Streaming tool call display (Claude Code-style)
// ============================================================================

/// A live tool call message that shows:
/// - "● Edit(src/foo.rs)" header immediately when tool starts
/// - Diff/output lines streaming in as they arrive
///
/// The `content` field grows as lines are appended. The TUI re-renders
/// automatically via the Arc<RwLock<>> update mechanism.
pub struct LiveToolMessage {
    id: MessageId,
    /// Pre-formatted header including the ● bullet and tool label
    header: String,
    /// Accumulated output lines (diff, command output, etc.)
    content: Arc<RwLock<String>>,
    status: Arc<RwLock<MessageStatus>>,
}

impl LiveToolMessage {
    pub fn new(header: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            header: header.into(),
            content: Arc::new(RwLock::new(String::new())),
            status: Arc::new(RwLock::new(MessageStatus::InProgress)),
        }
    }

    /// Append a line to the content (used for streaming diff lines)
    pub fn append_line(&self, line: &str) {
        if let Ok(mut c) = self.content.write() {
            c.push_str(line);
            c.push('\n');
        }
    }

    /// Replace the full content (for immediate complete display)
    pub fn set_content(&self, content: impl Into<String>) {
        if let Ok(mut c) = self.content.write() {
            *c = content.into();
        }
    }

    /// Mark as complete (hides the running indicator)
    pub fn set_complete(&self) {
        if let Ok(mut s) = self.status.write() {
            *s = MessageStatus::Complete;
        }
    }

    /// Mark as failed
    pub fn set_failed(&self) {
        if let Ok(mut s) = self.status.write() {
            *s = MessageStatus::Failed;
        }
    }
}

const CYAN: SetForegroundColor = SetForegroundColor(Color::Cyan);
const GRAY: SetForegroundColor = SetForegroundColor(Color::DarkGrey);
const RED_COLOR: SetForegroundColor = SetForegroundColor(Color::Red);
const GRAY_DIM: GrayDim = GrayDim;

impl Message for LiveToolMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    /// Stage 3 (#1120): the live surface renders from the VM — header,
    /// accumulated content lines, and status — read under the message's
    /// existing lock. Streaming appends land under that same lock and the
    /// next frame re-renders from the snapshot.
    fn component_view(&self) -> Option<ComponentView> {
        Some(ComponentView::LiveTool(LiveToolView {
            header: self.header.clone(),
            content_lines: self
                .content
                .read()
                .map(|content| content.lines().map(str::to_owned).collect())
                .unwrap_or_default(),
            status: self
                .status
                .read()
                .map(|status| *status)
                .unwrap_or(MessageStatus::InProgress),
        }))
    }

    fn format(&self, _colors: &finch_theme::ColorScheme) -> String {
        let content = self.content.read().map(|c| c.clone()).unwrap_or_default();
        let status = self
            .status
            .read()
            .map(|s| *s)
            .unwrap_or(MessageStatus::InProgress);

        match status {
            MessageStatus::InProgress => {
                if content.is_empty() {
                    // Just started - show inline trailing ellipsis (Claude Code style)
                    format!("{}{}…{}\n", self.header, GRAY_DIM, RESET)
                } else {
                    // Has some content already - show header + partial content
                    format!("{}\n{}", self.header, content)
                }
            }
            MessageStatus::Complete => {
                // Full output
                if content.is_empty() {
                    format!("{}\n", self.header)
                } else {
                    format!("{}\n{}", self.header, content)
                }
            }
            MessageStatus::Failed => {
                format!("{}\n{}", self.header, content)
            }
        }
    }

    fn status(&self) -> MessageStatus {
        self.status
            .read()
            .map(|s| *s)
            .unwrap_or(MessageStatus::InProgress)
    }

    fn content(&self) -> String {
        format!(
            "{}\n{}",
            self.header,
            self.content.read().map(|c| c.clone()).unwrap_or_default()
        )
    }

    fn background_style(&self, colors: &ColorScheme) -> Option<ratatui::style::Style> {
        Some(colors.message_band_style(MessageBand::Tool))
    }
}

// ============================================================================
// OperationMessage - Groups a generation turn's tool calls as a single row
//
// Appears in scrollback as:
//   ⏺ Generating
//     ⎿ bash(git push)…
//     ⎿ read(src/foo.rs) 45 lines
//
// Created lazily (only when the first tool call starts in a turn) so
// text-only turns produce no extra scrollback clutter.
// ============================================================================

/// Status of an individual row within an OperationMessage
#[derive(Clone)]
pub enum OperationRowStatus {
    Running,
    Complete(String), // compact one-line summary, may be empty
    Error(String),
}

/// A single sub-row representing one tool call
pub struct OperationRow {
    pub label: String, // pre-formatted label, e.g. "bash(git push)"
    pub status: OperationRowStatus,
}

/// Live operation message that groups tool calls for a generation turn.
pub struct OperationMessage {
    id: MessageId,
    header: String,
    rows: Arc<RwLock<Vec<OperationRow>>>,
    status: Arc<RwLock<MessageStatus>>,
}

impl OperationMessage {
    pub fn new(header: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            header: header.into(),
            rows: Arc::new(RwLock::new(Vec::new())),
            status: Arc::new(RwLock::new(MessageStatus::InProgress)),
        }
    }

    /// Append a running row and return its index for later updates.
    pub fn add_row(&self, label: impl Into<String>) -> usize {
        let mut rows = self.rows.write().unwrap_or_else(|p| p.into_inner());
        let idx = rows.len();
        rows.push(OperationRow {
            label: label.into(),
            status: OperationRowStatus::Running,
        });
        idx
    }

    /// Mark a row complete with an optional short summary.
    pub fn complete_row(&self, idx: usize, summary: impl Into<String>) {
        let mut rows = self.rows.write().unwrap_or_else(|p| p.into_inner());
        if let Some(row) = rows.get_mut(idx) {
            row.status = OperationRowStatus::Complete(summary.into());
        }
    }

    /// Mark a row as failed with an error message.
    pub fn fail_row(&self, idx: usize, error: impl Into<String>) {
        let mut rows = self.rows.write().unwrap_or_else(|p| p.into_inner());
        if let Some(row) = rows.get_mut(idx) {
            row.status = OperationRowStatus::Error(error.into());
        }
    }

    /// Mark the whole operation complete (all tools done).
    pub fn set_complete(&self) {
        *self.status.write().unwrap_or_else(|p| p.into_inner()) = MessageStatus::Complete;
    }
}

impl Message for OperationMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    /// Stage 3 (#1120): the chrome and the row list render from the VM —
    /// header, whole-operation status, and one row per tool call with its
    /// per-row status — read under the message's existing locks.
    fn component_view(&self) -> Option<ComponentView> {
        Some(ComponentView::Operation(OperationView {
            header: self.header.clone(),
            status: *self.status.read().unwrap_or_else(|p| p.into_inner()),
            rows: self
                .rows
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .map(|row| OperationRowView {
                    label: row.label.clone(),
                    status: match &row.status {
                        OperationRowStatus::Running => WorkRowStatus::Running,
                        OperationRowStatus::Complete(summary) => {
                            WorkRowStatus::Complete(summary.clone())
                        }
                        OperationRowStatus::Error(error) => WorkRowStatus::Error(error.clone()),
                    },
                })
                .collect(),
        }))
    }

    fn format(&self, _colors: &finch_theme::ColorScheme) -> String {
        let rows = self.rows.read().unwrap_or_else(|p| p.into_inner());
        let status = *self.status.read().unwrap_or_else(|p| p.into_inner());

        let ellipsis = if status == MessageStatus::InProgress {
            "…"
        } else {
            ""
        };
        let mut result = format!("{}⏺{} {}{}\n", CYAN, RESET, self.header, ellipsis);

        for row in rows.iter() {
            match &row.status {
                OperationRowStatus::Running => {
                    result.push_str(&format!(
                        "  {}⎿{} {}{}…{}\n",
                        GRAY, RESET, row.label, GRAY_DIM, RESET
                    ));
                }
                OperationRowStatus::Complete(summary) if summary.is_empty() => {
                    result.push_str(&format!("  {}⎿{} {}\n", GRAY, RESET, row.label));
                }
                OperationRowStatus::Complete(summary) => {
                    result.push_str(&format!(
                        "  {}⎿{} {} {}{}{}\n",
                        GRAY, RESET, row.label, GRAY_DIM, summary, RESET
                    ));
                }
                OperationRowStatus::Error(err) => {
                    result.push_str(&format!(
                        "  {}⎿{} {} {}error:{} {}\n",
                        GRAY, RESET, row.label, RED_COLOR, RESET, err
                    ));
                }
            }
        }

        result
    }

    fn status(&self) -> MessageStatus {
        *self.status.read().unwrap_or_else(|p| p.into_inner())
    }

    fn content(&self) -> String {
        self.header.clone()
    }

    fn background_style(&self, colors: &ColorScheme) -> Option<ratatui::style::Style> {
        Some(colors.message_band_style(MessageBand::Tool))
    }
}

// ============================================================================
// MemoryRecalledMessage - One turn's recalled/committed memory set
//
// Appears in scrollback as:
//   ⏺ 3 memories retrieved
//     ⎿ committed · score 0.64 · node 4 — 138 chars, sent raw
//     ⎿ committed · score 0.62 · node 12 — 421 chars, sent raw
//         user: this repo I'm in (files on disk) are your harness...
//         assistant: I don't have direct access to your files...
//
// Unlike OperationMessage, a recall has no running/streaming rows: every
// memory shown is already fully decided (raw or summarized) by the time the
// turn assembles its request, so the whole message is built once, complete,
// from the start -- there is no add_row/complete_row lifecycle, and (unlike
// a tool call) no input side to disclose separately from the recalled text.
//
// Each row's recalled text is collapsed behind its identity/summary line by
// default and expands on click (#1235), the same component-owned disclosure
// mechanism the say turn's `show_program` uses (docs/TUI_DESIGN.md, #882):
// the open/closed flag is mutable UI state retained on the message behind its
// own lock, addressed by row index through `transcript_action` /
// `handle_transcript_action`, never the renderer's RowId-keyed maps.
// ============================================================================

/// One recalled memory's presentation: its identity line, presentation
/// summary, and the recalled text lines shown beneath it.
pub struct MemoryRecallRow {
    /// Pre-formatted identity, e.g. "committed · score 0.64 · node 4".
    pub label: String,
    /// Pre-formatted one-line presentation summary, e.g. "138 chars, sent raw".
    pub summary: String,
    /// The recalled text, already split into lines.
    pub body_lines: Vec<String>,
}

/// The memory-recall component's action vocabulary (#1235): expand/collapse
/// one recalled memory's full text, addressed by its row index. Mirrors
/// `ToggleProgram`'s opaque-action pattern in `work_unit.rs` -- each
/// component defines its own payload beside the ViewModel it mutates, and the
/// engine's hit-rect routing never inspects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToggleMemoryRow(pub usize);

/// A recalled/committed memory set shown for one turn. The identity and
/// content (`header`, `rows`) are immutable once constructed -- unlike a
/// tool call, nothing about a recall streams in after the presentation
/// decision is made -- but each row's disclosure (full text shown, or
/// collapsed to its one-line summary) is mutable UI state behind its own
/// lock, collapsed by default (#1235).
pub struct MemoryRecalledMessage {
    id: MessageId,
    header: String,
    rows: Vec<MemoryRecallRow>,
    /// One flag per row in `rows`, collapsed (`false`) by default; `true`
    /// while that row's recalled text is expanded.
    expanded: RwLock<Vec<bool>>,
}

impl MemoryRecalledMessage {
    pub fn new(header: impl Into<String>, rows: Vec<MemoryRecallRow>) -> Self {
        let expanded = RwLock::new(vec![false; rows.len()]);
        Self {
            id: MessageId::new(),
            header: header.into(),
            rows,
            expanded,
        }
    }
}

impl Message for MemoryRecalledMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    /// The chrome header plus one row per memory, its identity/summary line,
    /// and its recalled text -- no Input/Output split, since a memory has no
    /// input side. The recalled text renders only while that row's
    /// `expanded` flag is set (#1235); reads the flags fresh under the lock
    /// every frame.
    fn component_view(&self) -> Option<ComponentView> {
        let expanded = self.expanded.read().unwrap_or_else(|p| p.into_inner());
        Some(ComponentView::MemoryRecalled(MemoryRecalledView {
            message_id: self.id,
            header: self.header.clone(),
            rows: self
                .rows
                .iter()
                .zip(expanded.iter())
                .map(|(row, &row_expanded)| MemoryRecallRowView {
                    label: row.label.clone(),
                    summary: row.summary.clone(),
                    body_lines: row.body_lines.clone(),
                    expanded: row_expanded,
                })
                .collect(),
        }))
    }

    /// The component-defined action a click on recalled-memory row
    /// `path[0]` produces (#1235): expand/collapse that row's full text. A
    /// row with no recalled text has nothing to disclose and is not a click
    /// target.
    fn transcript_action(&self, path: &[u32]) -> Option<ComponentAction> {
        if path.len() != 1 {
            return None;
        }
        let index = path[0] as usize;
        let row = self.rows.get(index)?;
        if row.body_lines.is_empty() {
            return None;
        }
        Some(ComponentAction::new(ToggleMemoryRow(index)))
    }

    /// Route a component action to the memory-recall component's handle:
    /// flips that row's `expanded` flag under the message's lock. False for
    /// foreign actions or an out-of-range row.
    fn handle_transcript_action(&self, action: &ComponentAction) -> bool {
        let Some(&ToggleMemoryRow(index)) = action.downcast_ref::<ToggleMemoryRow>() else {
            return false;
        };
        let mut expanded = self.expanded.write().unwrap_or_else(|p| p.into_inner());
        let Some(state) = expanded.get_mut(index) else {
            return false;
        };
        *state = !*state;
        true
    }

    fn format(&self, _colors: &ColorScheme) -> String {
        let mut result = format!("{}⏺{} {}\n", CYAN, RESET, self.header);
        for row in &self.rows {
            if row.summary.is_empty() {
                result.push_str(&format!("  {}⎿{} {}\n", GRAY, RESET, row.label));
            } else {
                result.push_str(&format!(
                    "  {}⎿{} {} {}— {}{}\n",
                    GRAY, RESET, row.label, GRAY_DIM, row.summary, RESET
                ));
            }
            for line in &row.body_lines {
                result.push_str(&format!("      {line}\n"));
            }
        }
        result
    }

    fn status(&self) -> MessageStatus {
        MessageStatus::Complete
    }

    fn content(&self) -> String {
        self.header.clone()
    }

    fn background_style(&self, colors: &ColorScheme) -> Option<ratatui::style::Style> {
        Some(colors.message_band_style(MessageBand::Tool))
    }
}

// ============================================================================
// ProgressMessage - Message for download/upload progress
// ============================================================================

/// Progress message for downloads, uploads, etc.
pub struct ProgressMessage {
    id: MessageId,
    label: String,
    current: Arc<RwLock<u64>>,
    total: u64,
    status: Arc<RwLock<MessageStatus>>,
}

impl ProgressMessage {
    pub fn new(label: impl Into<String>, total: u64) -> Self {
        Self {
            id: MessageId::new(),
            label: label.into(),
            current: Arc::new(RwLock::new(0)),
            total,
            status: Arc::new(RwLock::new(MessageStatus::InProgress)),
        }
    }

    /// Update progress
    pub fn update_progress(&self, current: u64) {
        match self.current.write() {
            Ok(mut c) => *c = current,
            Err(poisoned) => {
                tracing::warn!(
                    "ProgressMessage current lock poisoned in update_progress, recovering"
                );
                *poisoned.into_inner() = current;
            }
        }

        // Auto-complete when reaching 100%
        if current >= self.total {
            match self.status.write() {
                Ok(mut s) => *s = MessageStatus::Complete,
                Err(poisoned) => {
                    tracing::warn!(
                        "ProgressMessage status lock poisoned in update_progress, recovering"
                    );
                    *poisoned.into_inner() = MessageStatus::Complete;
                }
            }
        }
    }

    /// Mark as complete
    pub fn set_complete(&self) {
        match self.status.write() {
            Ok(mut s) => *s = MessageStatus::Complete,
            Err(poisoned) => {
                tracing::warn!("ProgressMessage status lock poisoned in set_complete, recovering");
                *poisoned.into_inner() = MessageStatus::Complete;
            }
        }
    }

    /// Mark as failed
    pub fn set_failed(&self) {
        match self.status.write() {
            Ok(mut s) => *s = MessageStatus::Failed,
            Err(poisoned) => {
                tracing::warn!("ProgressMessage status lock poisoned in set_failed, recovering");
                *poisoned.into_inner() = MessageStatus::Failed;
            }
        }
    }
}

impl Message for ProgressMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    /// Stage 3 (#1120): the bar/line renders from the VM — label, current
    /// bytes, total, and status — read under the message's existing locks.
    fn component_view(&self) -> Option<ComponentView> {
        Some(ComponentView::Progress(ProgressView {
            label: self.label.clone(),
            current: *self.current.read().unwrap_or_else(|p| p.into_inner()),
            total: self.total,
            status: self.status(),
        }))
    }

    fn format(&self, colors: &ColorScheme) -> String {
        // Handle poisoned locks gracefully
        let current = match self.current.read() {
            Ok(c) => *c,
            Err(poisoned) => {
                tracing::warn!("ProgressMessage current lock poisoned, using recovered data");
                *poisoned.into_inner()
            }
        };

        let status = match self.status.read() {
            Ok(s) => *s,
            Err(poisoned) => {
                tracing::warn!("ProgressMessage status lock poisoned, using recovered data");
                *poisoned.into_inner()
            }
        };

        let percentage = if self.total > 0 {
            (current as f64 / self.total as f64 * 100.0) as u8
        } else {
            0
        };

        // Progress bar: [████████░░] 80%
        let filled = (percentage / 10).min(10) as usize;
        let empty = 10 - filled;
        let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(empty));

        match status {
            MessageStatus::Complete => {
                format!(
                    "{}{} {} 100% ✓{}",
                    color_to_ansi(&colors.status.download),
                    self.label,
                    bar,
                    RESET
                )
            }
            MessageStatus::Failed => {
                format!(
                    "{}{} {} {}% ✗{}",
                    color_to_ansi(&colors.messages.error),
                    self.label,
                    bar,
                    percentage,
                    RESET
                )
            }
            MessageStatus::InProgress => {
                format!(
                    "{}{} {} {}%{}",
                    color_to_ansi(&colors.status.operation),
                    self.label,
                    bar,
                    percentage,
                    RESET
                )
            }
        }
    }

    fn status(&self) -> MessageStatus {
        match self.status.read() {
            Ok(s) => *s,
            Err(poisoned) => {
                tracing::warn!("ProgressMessage status lock poisoned, using recovered data");
                *poisoned.into_inner()
            }
        }
    }

    fn content(&self) -> String {
        let current = match self.current.read() {
            Ok(c) => *c,
            Err(poisoned) => {
                tracing::warn!(
                    "ProgressMessage current lock poisoned in content(), using recovered data"
                );
                *poisoned.into_inner()
            }
        };

        format!("{}: {}/{}", self.label, current, self.total)
    }

    fn background_style(&self, colors: &ColorScheme) -> Option<ratatui::style::Style> {
        Some(colors.message_band_style(MessageBand::Tool))
    }
}

// ============================================================================
// StaticMessage - Immutable message for errors, info, etc.
// ============================================================================

/// Static message (immutable, for errors, system info, etc.)
pub struct StaticMessage {
    id: MessageId,
    content: String,
    message_type: StaticMessageType,
}

#[derive(Debug, Clone, Copy)]
pub enum StaticMessageType {
    Info,
    Error,
    Success,
    Warning,
    Plain, // For messages that already have their own formatting
}

impl StaticMessage {
    pub fn info(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            content: content.into(),
            message_type: StaticMessageType::Info,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            content: content.into(),
            message_type: StaticMessageType::Error,
        }
    }

    pub fn success(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            content: content.into(),
            message_type: StaticMessageType::Success,
        }
    }

    pub fn warning(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            content: content.into(),
            message_type: StaticMessageType::Warning,
        }
    }

    pub fn plain(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            content: content.into(),
            message_type: StaticMessageType::Plain,
        }
    }
}

impl Message for StaticMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    /// Stage 3 (#1120): the text IS this component's view. The snapshot is
    /// constructed from the immutable fields — no lock is involved — and the
    /// renderer renders it through the generalized accessor.
    fn component_view(&self) -> Option<ComponentView> {
        Some(ComponentView::StaticText(StaticTextView {
            kind: match self.message_type {
                StaticMessageType::Info => StaticTextKind::Info,
                StaticMessageType::Error => StaticTextKind::Error,
                StaticMessageType::Success => StaticTextKind::Success,
                StaticMessageType::Warning => StaticTextKind::Warning,
                StaticMessageType::Plain => StaticTextKind::Plain,
            },
            content_lines: self.content.lines().map(str::to_owned).collect(),
        }))
    }

    fn format(&self, colors: &ColorScheme) -> String {
        match self.message_type {
            StaticMessageType::Info => {
                format!(
                    "{}ℹ️  {}{}",
                    color_to_ansi(&colors.messages.system),
                    self.content,
                    RESET
                )
            }
            StaticMessageType::Error => {
                format!(
                    "{}❌ {}{}",
                    color_to_ansi(&colors.messages.error),
                    self.content,
                    RESET
                )
            }
            StaticMessageType::Success => {
                format!(
                    "{}✓ {}{}",
                    color_to_ansi(&colors.messages.system),
                    self.content,
                    RESET
                )
            }
            StaticMessageType::Warning => {
                format!(
                    "{}⚠️  {}{}",
                    color_to_ansi(&colors.status.operation),
                    self.content,
                    RESET
                )
            }
            StaticMessageType::Plain => {
                // No prefix - content already formatted
                self.content.clone()
            }
        }
    }

    fn status(&self) -> MessageStatus {
        MessageStatus::Complete
    }

    fn content(&self) -> String {
        self.content.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn memory_row(label: &str, summary: &str, body_lines: &[&str]) -> MemoryRecallRow {
        MemoryRecallRow {
            label: label.to_string(),
            summary: summary.to_string(),
            body_lines: body_lines.iter().map(|line| line.to_string()).collect(),
        }
    }

    fn memory_row_view<'a>(view: &'a MemoryRecalledView, index: usize) -> &'a MemoryRecallRowView {
        &view.rows[index]
    }

    /// #1235: a freshly constructed recall message defaults every row to
    /// collapsed (`expanded: false`) -- the reported preference was that the
    /// full recalled text must not render until the reader asks for it.
    #[test]
    fn test_memory_recalled_message_rows_default_collapsed() {
        let message = MemoryRecalledMessage::new(
            "2 memories retrieved",
            vec![
                memory_row(
                    "recalled · score 0.64 · node 4",
                    "5 chars, sent raw",
                    &["hi"],
                ),
                memory_row(
                    "recalled · score 0.60 · node 1",
                    "5 chars, sent raw",
                    &["yo"],
                ),
            ],
        );
        let Some(ComponentView::MemoryRecalled(view)) = message.component_view() else {
            panic!("MemoryRecalledMessage must produce ComponentView::MemoryRecalled");
        };
        assert!(
            view.rows.iter().all(|row| !row.expanded),
            "every row must default to collapsed; rows={:?}",
            view.rows
        );
    }

    /// #1235: clicking a row's summary line (`transcript_action` at that
    /// row's path) toggles exactly that row's `expanded` flag under the
    /// message's lock, mirroring `WorkUnit::say_turn_action` /
    /// `handle_say_turn_action`'s `ToggleProgram` pattern -- and clicking
    /// again collapses it back.
    #[test]
    fn test_memory_recalled_message_transcript_action_toggles_one_row_expanded_state() {
        let message = MemoryRecalledMessage::new(
            "2 memories retrieved",
            vec![
                memory_row(
                    "recalled · score 0.64 · node 4",
                    "5 chars, sent raw",
                    &["hi"],
                ),
                memory_row(
                    "recalled · score 0.60 · node 1",
                    "5 chars, sent raw",
                    &["yo"],
                ),
            ],
        );

        let action = message
            .transcript_action(&[1])
            .expect("a row with recalled text produces a toggle action");
        assert!(
            message.handle_transcript_action(&action),
            "the trait handle must toggle the targeted row"
        );
        let Some(ComponentView::MemoryRecalled(view)) = message.component_view() else {
            panic!("MemoryRecalledMessage must produce ComponentView::MemoryRecalled");
        };
        assert!(
            !memory_row_view(&view, 0).expanded,
            "row 0 must be untouched by a click on row 1; rows={:?}",
            view.rows
        );
        assert!(
            memory_row_view(&view, 1).expanded,
            "row 1 must have opened; rows={:?}",
            view.rows
        );

        // Click again: the same action toggles it back closed.
        let action_again = message
            .transcript_action(&[1])
            .expect("the row still produces a toggle action once expanded");
        assert!(message.handle_transcript_action(&action_again));
        let Some(ComponentView::MemoryRecalled(view)) = message.component_view() else {
            panic!("MemoryRecalledMessage must produce ComponentView::MemoryRecalled");
        };
        assert!(
            !memory_row_view(&view, 1).expanded,
            "a second click must collapse the row again; rows={:?}",
            view.rows
        );

        let foreign = ComponentAction::new(7u32);
        assert!(
            !message.handle_transcript_action(&foreign),
            "a foreign action payload is rejected, never misinterpreted"
        );
    }

    /// #1235: a row with no recalled text has nothing to disclose and is
    /// never a click target -- `transcript_action` returns `None` rather
    /// than an action that would toggle an always-empty body.
    #[test]
    fn test_memory_recalled_message_row_with_empty_body_has_no_transcript_action() {
        let message = MemoryRecalledMessage::new(
            "1 memory retrieved",
            vec![memory_row(
                "committed · score 0.50 · node 9",
                "0 chars, sent raw",
                &[],
            )],
        );
        assert!(
            message.transcript_action(&[0]).is_none(),
            "a row with no recalled text must not be a click target"
        );
        assert!(
            message.transcript_action(&[5]).is_none(),
            "an out-of-range row index must not panic or produce an action"
        );
    }

    #[test]
    fn brain_participant_messages_distinguish_prompt_from_relay() {
        let colors = finch_theme::ColorScheme::default();
        let prompt = BrainParticipantMessage::new("alice@box", "please inspect", true);
        let relay = BrainParticipantMessage::new("alice@box", "I agree", false);

        assert!(prompt
            .format(&colors)
            .contains("❯ alice@box: please inspect"));
        assert!(relay.format(&colors).contains("◆ alice@box: I agree"));
        assert_eq!(
            prompt.background_style(&colors),
            BrainParticipantMessage::new("alice@box", "again", true).background_style(&colors)
        );
        assert_ne!(
            prompt.background_style(&colors),
            BrainParticipantMessage::new("bob@box", "hello", false).background_style(&colors)
        );
        assert_eq!(prompt.content(), "alice@box: please inspect");
    }

    #[test]
    fn test_streaming_message_handles_poisoned_lock() {
        let msg = StreamingResponseMessage::new();

        // Poison the content lock by panicking while holding it
        let content_clone = Arc::clone(&msg.content);
        let handle = std::thread::spawn(move || {
            let _guard = content_clone.write().unwrap();
            panic!("Intentional panic to poison lock");
        });
        let _ = handle.join(); // Let thread panic

        // Now the lock is poisoned - format() should NOT panic
        let colors = finch_theme::ColorScheme::default();
        let result = msg.format(&colors);

        // Should recover and return some string (not panic)
        assert!(!result.is_empty());
        // Should show the ⏺ bullet (new streaming format)
        assert!(result.contains("⏺") || result.is_empty());
    }

    #[test]
    fn test_streaming_message_concurrent_access() {
        let msg = Arc::new(StreamingResponseMessage::new());
        let mut handles = vec![];

        // Spawn 10 threads reading/writing concurrently
        for i in 0..10 {
            let msg_clone = Arc::clone(&msg);
            handles.push(std::thread::spawn(move || {
                if i % 2 == 0 {
                    msg_clone.append_chunk(&format!("chunk {}", i));
                } else {
                    let colors = finch_theme::ColorScheme::default();
                    let _ = msg_clone.format(&colors);
                }
            }));
        }

        // All threads should complete without deadlock or panic
        for handle in handles {
            handle.join().unwrap();
        }

        // Message should contain some content
        let content = msg.content();
        assert!(content.contains("chunk"));
    }

    #[test]
    fn test_tool_message_handles_poisoned_lock() {
        let msg = ToolExecutionMessage::new("test_tool");

        // Poison the stdout lock
        let stdout_clone = Arc::clone(&msg.stdout);
        let handle = std::thread::spawn(move || {
            let _guard = stdout_clone.write().unwrap();
            panic!("Intentional panic to poison lock");
        });
        let _ = handle.join();

        // format() should NOT panic
        let colors = finch_theme::ColorScheme::default();
        let result = msg.format(&colors);

        // Should recover and return formatted output
        assert!(result.contains("test_tool"));
    }

    #[test]
    fn test_progress_message_handles_poisoned_lock() {
        let msg = ProgressMessage::new("Download", 100);

        // Poison the current lock
        let current_clone = Arc::clone(&msg.current);
        let handle = std::thread::spawn(move || {
            let _guard = current_clone.write().unwrap();
            panic!("Intentional panic to poison lock");
        });
        let _ = handle.join();

        // format() should NOT panic
        let colors = finch_theme::ColorScheme::default();
        let result = msg.format(&colors);

        // Should recover and show progress bar
        assert!(result.contains("Download"));
        assert!(result.contains("["));
        assert!(result.contains("]"));
    }

    // ── LiveToolMessage format state tests ─────────────────────────────────

    #[test]
    fn test_live_tool_message_inprogress_empty_shows_ellipsis() {
        let colors = finch_theme::ColorScheme::default();
        let msg = LiveToolMessage::new("⏺ bash(echo hi)");
        let formatted = msg.format(&colors);
        // InProgress + empty content → header with trailing "…" on same line
        assert!(formatted.contains("bash(echo hi)"));
        assert!(
            formatted.contains('…'),
            "expected ellipsis '…' in: {:?}",
            formatted
        );
        // Must NOT contain old spinner symbol
        assert!(
            !formatted.contains('⟳'),
            "unexpected '⟳' in: {:?}",
            formatted
        );
    }

    #[test]
    fn test_live_tool_message_inprogress_with_content() {
        let colors = finch_theme::ColorScheme::default();
        let msg = LiveToolMessage::new("⏺ bash(echo hi)");
        msg.append_line("hello world");
        let formatted = msg.format(&colors);
        // InProgress + content → both header and content present
        assert!(
            formatted.contains("bash(echo hi)"),
            "header missing in: {:?}",
            formatted
        );
        assert!(
            formatted.contains("hello world"),
            "content missing in: {:?}",
            formatted
        );
    }

    #[test]
    fn test_live_tool_message_complete_with_output() {
        let colors = finch_theme::ColorScheme::default();
        let msg = LiveToolMessage::new("⏺ bash(echo hi)");
        msg.set_content("  ⎿ hello world\n");
        msg.set_complete();
        let formatted = msg.format(&colors);
        // Complete with output → shows output, no spinner
        assert!(
            formatted.contains("hello world"),
            "output missing in: {:?}",
            formatted
        );
        assert!(
            !formatted.contains('⟳'),
            "unexpected '⟳' in: {:?}",
            formatted
        );
    }

    #[test]
    fn test_live_tool_message_complete_no_output() {
        let colors = finch_theme::ColorScheme::default();
        let msg = LiveToolMessage::new("⏺ bash(true)");
        msg.set_complete();
        let formatted = msg.format(&colors);
        // Complete with no output → just header, no garbage
        assert!(
            formatted.contains("bash(true)"),
            "header missing in: {:?}",
            formatted
        );
        assert!(
            !formatted.contains('⟳'),
            "unexpected '⟳' in: {:?}",
            formatted
        );
        // Should not have a bare "…" (that would indicate still InProgress display)
        assert!(
            !formatted.contains('…'),
            "unexpected '…' in complete state: {:?}",
            formatted
        );
    }

    #[test]
    fn test_live_tool_message_failed_state() {
        let colors = finch_theme::ColorScheme::default();
        let msg = LiveToolMessage::new("⏺ bash(bad_cmd)");
        msg.set_content("command not found\n");
        msg.set_failed();
        let formatted = msg.format(&colors);
        assert!(
            formatted.contains("bash(bad_cmd)"),
            "header missing in: {:?}",
            formatted
        );
        assert!(
            formatted.contains("command not found"),
            "error content missing in: {:?}",
            formatted
        );
    }

    // ── OperationMessage format state tests ────────────────────────────────

    #[test]
    fn test_operation_message_uses_correct_unicode() {
        let colors = finch_theme::ColorScheme::default();
        let msg = OperationMessage::new("Generating");
        let idx = msg.add_row("bash(echo hi)");
        msg.complete_row(idx, "hi");
        msg.set_complete();
        let formatted = msg.format(&colors);
        // Must use ⏺ (U+23FA), not ● (U+25CF)
        assert!(
            formatted.contains('⏺'),
            "Expected ⏺ (U+23FA), got: {:?}",
            formatted
        );
        assert!(
            !formatted.contains('●'),
            "Found old ● (U+25CF) in: {:?}",
            formatted
        );
        // Must use ⎿ (U+23BF), not └ (U+2514)
        assert!(
            formatted.contains('⎿'),
            "Expected ⎿ (U+23BF), got: {:?}",
            formatted
        );
        assert!(
            !formatted.contains('└'),
            "Found old └ (U+2514) in: {:?}",
            formatted
        );
    }

    #[test]
    fn test_operation_message_inprogress_shows_ellipsis() {
        let colors = finch_theme::ColorScheme::default();
        let msg = OperationMessage::new("Generating");
        let formatted = msg.format(&colors);
        // InProgress: header ends with ellipsis
        assert!(
            formatted.contains("Generating…"),
            "expected 'Generating…' in: {:?}",
            formatted
        );
    }

    #[test]
    fn test_operation_message_complete_no_ellipsis() {
        let colors = finch_theme::ColorScheme::default();
        let msg = OperationMessage::new("Generating");
        msg.set_complete();
        let formatted = msg.format(&colors);
        // Complete: no trailing ellipsis
        assert!(
            !formatted.contains("Generating…"),
            "unexpected '…' in complete state: {:?}",
            formatted
        );
        assert!(
            formatted.contains("Generating"),
            "header missing in: {:?}",
            formatted
        );
    }

    #[test]
    fn test_operation_message_row_running_shows_ellipsis() {
        let colors = finch_theme::ColorScheme::default();
        let msg = OperationMessage::new("Generating");
        msg.add_row("bash(ls)");
        let formatted = msg.format(&colors);
        // Running row: label + "…"
        assert!(
            formatted.contains("bash(ls)"),
            "row label missing in: {:?}",
            formatted
        );
        assert!(
            formatted.contains('⎿'),
            "expected ⎿ prefix in: {:?}",
            formatted
        );
    }

    #[test]
    fn test_operation_message_row_error_shows_error() {
        let colors = finch_theme::ColorScheme::default();
        let msg = OperationMessage::new("Generating");
        let idx = msg.add_row("bash(bad)");
        msg.fail_row(idx, "permission denied");
        let formatted = msg.format(&colors);
        assert!(
            formatted.contains("bash(bad)"),
            "row label missing in: {:?}",
            formatted
        );
        assert!(
            formatted.contains("permission denied"),
            "error message missing in: {:?}",
            formatted
        );
    }

    // ── Stage-3 component views reflect the VMs under the locks (#1120) ────

    #[test]
    fn component_views_reflect_operation_vm_transitions_under_the_locks() {
        let operation = OperationMessage::new("Generating");
        assert!(
            operation.component_view().is_some(),
            "an OperationMessage always yields its component"
        );
        let first = operation.component_view().expect("component view");
        let ComponentView::Operation(view) = first else {
            panic!("an OperationMessage yields its Operation component; got {first:?}")
        };
        assert!(
            view.rows.is_empty() && view.status == MessageStatus::InProgress,
            "the fresh VM carries the chrome state with zero rows; got {view:?}"
        );

        let call = operation.add_row("bash(git push)");
        let ComponentView::Operation(running) = operation.component_view().expect("component view")
        else {
            panic!("component kind changed")
        };
        assert_eq!(
            running.rows,
            vec![OperationRowView {
                label: "bash(git push)".to_string(),
                status: WorkRowStatus::Running,
            }],
            "the running row rides the VM under the rows lock; got {running:?}"
        );

        operation.complete_row(call, "pushed");
        operation.set_complete();
        let ComponentView::Operation(done) = operation.component_view().expect("component view")
        else {
            panic!("component kind changed")
        };
        assert_eq!(
            done.rows[0].status,
            WorkRowStatus::Complete("pushed".to_string()),
            "the completed row's summary rides the VM; got {done:?}"
        );
        assert_eq!(
            done.status,
            MessageStatus::Complete,
            "the whole-operation status rides the VM; got {done:?}"
        );
    }

    #[test]
    fn component_views_reflect_live_tool_and_progress_vm_transitions_under_the_locks() {
        let live_tool = LiveToolMessage::new("⏺ bash(echo hi)");
        let ComponentView::LiveTool(started) = live_tool.component_view().expect("component view")
        else {
            panic!("a LiveToolMessage yields its LiveTool component")
        };
        assert!(
            started.content_lines.is_empty(),
            "a just-started call carries no content lines; got {started:?}"
        );
        live_tool.append_line("hello world");
        live_tool.append_line("goodbye");
        let ComponentView::LiveTool(grown) = live_tool.component_view().expect("component view")
        else {
            panic!("component kind changed")
        };
        assert_eq!(
            grown.content_lines,
            vec!["hello world".to_string(), "goodbye".to_string()],
            "streaming appends land under the lock and ride the snapshot; got {grown:?}"
        );

        let progress = ProgressMessage::new("weights.safetensors", 100);
        let ComponentView::Progress(initial) = progress.component_view().expect("component view")
        else {
            panic!("a ProgressMessage yields its Progress component")
        };
        assert_eq!(
            (initial.current, initial.total),
            (0, 100),
            "the fresh VM carries zero progress; got {initial:?}"
        );
        progress.update_progress(100);
        let ComponentView::Progress(complete) = progress.component_view().expect("component view")
        else {
            panic!("component kind changed")
        };
        assert_eq!(
            (complete.current, complete.status),
            (100, MessageStatus::Complete),
            "the auto-complete transition rides the VM; got {complete:?}"
        );

        let info = StaticMessage::info("all systems nominal");
        let ComponentView::StaticText(static_view) = info.component_view().expect("component view")
        else {
            panic!("a StaticMessage yields its StaticText component")
        };
        assert_eq!(
            (static_view.kind, static_view.content_lines.as_slice()),
            (
                StaticTextKind::Info,
                &["all systems nominal".to_string()][..]
            ),
            "the text IS its view: kind and content ride the snapshot; got {static_view:?}"
        );
    }
}

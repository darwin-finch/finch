//! The blit-time ViewModel and the one domain → widget projection.
//!
//! Every blit, the live console converts domain state into one owned
//! [`LiveViewModel`] snapshot — composer draft and cursor, completion rows
//! (empty unless the draft is a slash command or mention), status lines, and
//! the live transcript — and [`project_root`] projects that snapshot into a
//! tree of standard [`widgets`](super::widgets). Widgets layout and paint the
//! snapshot; they never query `WorkUnit`, the command registry, or each other
//! to decide visibility (#805).
//!
//! Disclosure and focus are renderer state: the accordion's open set is keyed
//! by stable [`RowId`] (message id plus append-only semantic path), so
//! streamed appends, terminal reflow, and reconnects never lose a choice, and
//! completing a run cannot collapse a result by flipping a domain default.

use crate::cli::messages::{
    AgentActivityView, MessageId, MessageRef, MessageStatus, WorkRowPresentation, WorkRowStatus,
    WorkRowView, WorkUnitHead, WorkUnitPresentation, WorkUnitView,
};
use crate::theme::ColorScheme;

use super::accordion::RenderedTranscriptLine;
use super::autocomplete_widget::{completion_pane_lines, AutocompleteState};
use super::widgets::{self, Axis, Rect, Track, Widget};

/// Stable identity for one expandable row within the transcript.
///
/// `path` is append-only semantic ancestry (unit, call index, input/output),
/// so streamed appends and terminal reflow never change an existing row's key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RowId {
    pub message_id: MessageId,
    pub path: Vec<u32>,
}

/// Renderer-facing role of one transcript node. The ViewModel derives it from
/// domain data at projection time; the renderer uses it to route disclosure,
/// focus, and bounded tool viewports — never as a widget kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeRole {
    Response,
    Activity,
    Program,
    Output,
    ToolGroup,
    ToolCall,
    Input,
    ToolOutput,
}

/// Widget props for one transcript row, projected from domain data.
#[derive(Debug, Clone)]
pub(crate) struct TranscriptNode {
    pub id: RowId,
    pub role: NodeRole,
    pub label: String,
    pub body: Vec<String>,
    pub children: Vec<TranscriptNode>,
    /// The renderer's default disclosure for this row, derived from domain
    /// status at projection time. The user's open-set choice overrides it.
    pub default_open: bool,
}

/// A message projected for the transcript: a WorkUnit run becomes a
/// [`TranscriptNode`]; any other message renders as its formatted lines.
pub(crate) enum ProjectedMessage {
    Node(TranscriptNode),
    Plain(Vec<String>),
}

/// Project one message into transcript widget props. This is the only
/// domain → widget conversion in the live console.
pub(crate) fn project_message(message: &MessageRef, colors: &ColorScheme) -> ProjectedMessage {
    match message.work_unit_view(colors) {
        Some(view) => ProjectedMessage::Node(project_work_unit(&view)),
        None => ProjectedMessage::Plain(
            message
                .format(colors)
                .split('\n')
                .map(str::to_owned)
                .collect(),
        ),
    }
}

/// Test-only projection entry: tests talk to the ViewModel the way the
/// renderer does, never to a `TranscriptRow`. `None` for messages that are
/// not WorkUnit runs.
#[cfg(test)]
pub(crate) fn try_project_for_test(
    message: &dyn crate::cli::messages::Message,
    colors: &ColorScheme,
) -> Option<TranscriptNode> {
    message
        .work_unit_view(colors)
        .map(|view| project_work_unit(&view))
}

/// Project one WorkUnit run into a transcript node.
pub(crate) fn project_work_unit(view: &WorkUnitView) -> TranscriptNode {
    let message_id = view.head.message_id;
    let mut children = view
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let mut projected = match row.presentation {
                WorkRowPresentation::Activity => project_activity_row(view, index, row),
                WorkRowPresentation::Tool => project_tool_row(view, index, row),
            };
            projected.children.extend(project_agent_activity_roots(
                message_id,
                &view.agent_activity,
                Some(index),
            ));
            projected
        })
        .collect::<Vec<_>>();
    children.extend(project_agent_activity_roots(
        message_id,
        &view.agent_activity,
        None,
    ));
    let head = &view.head;
    let (role, label, body, default_open) = match &head.presentation {
        WorkUnitPresentation::Assistant if !view.rows.is_empty() => {
            let actionable = view.rows.iter().any(tool_row_requires_default_expansion);
            (
                NodeRole::ToolGroup,
                compact_tool_group_label(&view.rows),
                text_lines(&head.response_text),
                head.status == MessageStatus::InProgress || actionable,
            )
        }
        WorkUnitPresentation::Assistant => (
            NodeRole::Response,
            assistant_prose_label(&view.verb, head),
            text_lines(&head.response_text),
            true,
        ),
        WorkUnitPresentation::Activity { title } => (
            NodeRole::Activity,
            compact_activity_group_label(title, &view.rows),
            Vec::new(),
            head.status == MessageStatus::InProgress,
        ),
        WorkUnitPresentation::ProgramSource { language } => (
            NodeRole::Program,
            format!("Program source ({language})"),
            text_lines(&head.response_text),
            head.status == MessageStatus::InProgress,
        ),
        WorkUnitPresentation::ProgramOutput { title } => {
            // Role stays Output so the live list can still swap completed IR
            // for this row. Only the label changes: successful untitled say
            // uses the assistant glyph instead of `Program output`.
            let label = if head.projects_as_prose {
                assistant_prose_label(&view.verb, head)
            } else {
                title
                    .clone()
                    .unwrap_or_else(|| "Program output".to_string())
            };
            (NodeRole::Output, label, head.output_body_lines(), true)
        }
    };

    TranscriptNode {
        id: RowId {
            message_id,
            path: vec![0],
        },
        role,
        label,
        body,
        children,
        default_open,
    }
}

fn project_tool_row(view: &WorkUnitView, index: usize, row: &WorkRowView) -> TranscriptNode {
    let message_id = view.head.message_id;
    let summary = row_status_summary(&row.status);
    let actionable = tool_row_requires_default_expansion(row);
    let input = TranscriptNode {
        id: RowId {
            message_id,
            path: vec![1, index as u32, 0],
        },
        role: NodeRole::Input,
        label: "Input".to_string(),
        body: vec![row.label.clone()],
        children: Vec::new(),
        default_open: false,
    };
    let mut output_body = row.body_lines.clone();
    if let Some(diffs) = &row.rendered_diffs {
        output_body.extend(diffs.iter().cloned());
    }
    let mut children = vec![input];
    if !output_body.is_empty() {
        children.push(TranscriptNode {
            id: RowId {
                message_id,
                path: vec![1, index as u32, 1],
            },
            role: NodeRole::ToolOutput,
            label: format!("Output ({})", output_body.len()),
            body: output_body,
            children: Vec::new(),
            default_open: matches!(row.status, WorkRowStatus::Running) || actionable,
        });
    }
    TranscriptNode {
        id: RowId {
            message_id,
            path: vec![1, index as u32],
        },
        role: NodeRole::ToolCall,
        label: format!("{} — {summary}", row.label),
        body: Vec::new(),
        children,
        default_open: matches!(row.status, WorkRowStatus::Running) || actionable,
    }
}

fn project_activity_row(view: &WorkUnitView, index: usize, row: &WorkRowView) -> TranscriptNode {
    let summary = row_status_summary(&row.status);
    let mut body = row.body_lines.clone();
    if let Some(diffs) = &row.rendered_diffs {
        body.extend(diffs.iter().cloned());
    }
    TranscriptNode {
        id: RowId {
            message_id: view.head.message_id,
            path: vec![1, index as u32],
        },
        role: NodeRole::Activity,
        label: format!("{} — {summary}", row.label),
        body,
        children: Vec::new(),
        default_open: matches!(row.status, WorkRowStatus::Running) || row.has_output(),
    }
}

fn row_status_summary(status: &WorkRowStatus) -> String {
    match status {
        WorkRowStatus::Running => "running".to_string(),
        WorkRowStatus::Complete(summary) if summary.is_empty() => "complete".to_string(),
        WorkRowStatus::Complete(summary) => summary.clone(),
        WorkRowStatus::Error(error) => format!("failed: {error}"),
    }
}

/// Roots of the child-agent activity forest: rows owned by `owner_row` whose
/// parent is not also owned by the same row.
fn project_agent_activity_roots(
    message_id: MessageId,
    activities: &[AgentActivityView],
    owner_row: Option<usize>,
) -> Vec<TranscriptNode> {
    activities
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            row.owner_row == owner_row
                && row.parent_agent_id.is_none_or(|parent| {
                    !activities.iter().any(|candidate| {
                        candidate.owner_row == owner_row && candidate.agent_id == parent
                    })
                })
        })
        .map(|(index, _)| project_agent_activity_row(message_id, activities, owner_row, index))
        .collect()
}

fn project_agent_activity_row(
    message_id: MessageId,
    activities: &[AgentActivityView],
    owner_row: Option<usize>,
    index: usize,
) -> TranscriptNode {
    let row = &activities[index];
    let summary = row_status_summary(&row.status);
    let owner_segment = owner_row.map_or(u32::MAX, |owner| owner as u32);
    let mut children = row
        .tools
        .iter()
        .enumerate()
        .map(|(tool_index, tool)| {
            let tool_summary = row_status_summary(&tool.status);
            TranscriptNode {
                id: RowId {
                    message_id,
                    path: vec![2, owner_segment, index as u32, 0, tool_index as u32],
                },
                role: NodeRole::Activity,
                label: format!("tool {} — {tool_summary}", tool.name),
                body: Vec::new(),
                children: Vec::new(),
                default_open: matches!(tool.status, WorkRowStatus::Running),
            }
        })
        .collect::<Vec<_>>();
    children.extend(
        activities
            .iter()
            .enumerate()
            .filter(|(_, child)| {
                child.owner_row == owner_row && child.parent_agent_id == Some(row.agent_id)
            })
            .map(|(child_index, _)| {
                project_agent_activity_row(message_id, activities, owner_row, child_index)
            }),
    );
    TranscriptNode {
        id: RowId {
            message_id,
            path: vec![2, owner_segment, index as u32],
        },
        role: NodeRole::Activity,
        label: format!("{} — {summary}", row.label),
        body: row.body_lines.clone(),
        children,
        default_open: matches!(row.status, WorkRowStatus::Running),
    }
}

/// Compact activity glyph standing in for a plain assistant prose row.
///
/// The row's own words are the content; a literal `Assistant response` label
/// named Finch's message plumbing rather than anything the assistant said, so
/// a simple turn read as program internals instead of prose (#350). Hollow
/// while the turn is still arriving, filled once it completed, struck through
/// when it failed — a turn that died must never look identical to one that
/// answered. Enumerated with no wildcard so a new `MessageStatus` is a compile
/// error here rather than a silently wrong glyph.
fn assistant_prose_glyph(status: MessageStatus) -> &'static str {
    match status {
        MessageStatus::InProgress => "\u{25cb}",
        MessageStatus::Complete => "\u{23fa}",
        MessageStatus::Failed => "\u{2298}",
    }
}

/// Label for a plain assistant prose row.
///
/// When the row carries the assistant's own words, the glyph alone is the
/// label: the words follow immediately on the next line and are the row's real
/// identity (#350). When it carries nothing, a bare glyph would leave the row
/// with no readable text at all — and the wordless shapes are exactly the ones
/// a user actually sees: the freshly created query unit that is on screen for
/// the whole provider round trip, and a turn that failed before its first
/// token. Those name their state in words as well as in a glyph, because a row
/// that cannot be read aloud is not an accessible interface (Key Principle 5).
fn assistant_prose_label(verb: &str, head: &WorkUnitHead) -> String {
    let glyph = assistant_prose_glyph(head.status);
    if !head.response_text.is_empty() {
        return glyph.to_string();
    }
    match head.status {
        MessageStatus::InProgress if !verb.trim().is_empty() => format!("{glyph} {verb}\u{2026}"),
        MessageStatus::InProgress => format!("{glyph} Working\u{2026}"),
        MessageStatus::Complete => format!("{glyph} No assistant text"),
        MessageStatus::Failed => format!("{glyph} Assistant turn failed"),
    }
}

fn text_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').map(str::to_owned).collect()
    }
}

fn tool_row_requires_default_expansion(row: &WorkRowView) -> bool {
    matches!(row.status, WorkRowStatus::Running)
        || (matches!(&row.status, WorkRowStatus::Complete(summary) if summary.trim().is_empty())
            && row.has_output())
}

fn compact_tool_group_label(rows: &[WorkRowView]) -> String {
    let noun = if rows.len() == 1 { "call" } else { "calls" };
    let mut label = format!("Tools ({} {noun})", rows.len());
    let Some((call, error)) = rows.iter().find_map(|row| match &row.status {
        WorkRowStatus::Error(error) => Some((
            crate::cli::messages::work_unit::compact_summary_text(&row.label, 60),
            crate::cli::messages::work_unit::compact_summary_text(error, 120),
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

fn compact_activity_group_label(title: &str, rows: &[WorkRowView]) -> String {
    let mut label = title.to_string();
    let Some((activity, error)) = rows.iter().find_map(|row| match &row.status {
        WorkRowStatus::Error(error) => {
            let activity = row
                .label
                .strip_prefix(title)
                .map(|suffix| suffix.trim_start_matches([' ', '·']))
                .filter(|suffix| !suffix.is_empty())
                .unwrap_or(&row.label);
            let error = error.strip_prefix("failed: ").unwrap_or(error);
            Some((
                crate::cli::messages::work_unit::compact_summary_text(activity, 60),
                crate::cli::messages::work_unit::compact_summary_text(error, 120),
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

// ─── The live frame ViewModel ─────────────────────────────────────────────────

/// Keys the root tree's claimable regions so the layout result can hand each
/// one back by name.
pub(crate) mod frame_key {
    pub const TRANSCRIPT: u16 = 0;
    pub const COMPLETIONS: u16 = 1;
    pub const SEPARATOR: u16 = 2;
    pub const COMPOSER: u16 = 3;
    pub const STATUS_RULE: u16 = 4;
    pub const STATUS: u16 = 5;
}

/// The regions of one live frame, claimed by the widget tree. Rects are in the
/// live frame's own coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FrameRects {
    pub transcript: Rect,
    /// `None` when the pane claims zero rows or a dialog owns the frame.
    pub completions: Option<Rect>,
    pub separator: Rect,
    pub composer: Rect,
    pub status_rule: Rect,
    pub status: Rect,
}

/// Everything the live-frame planner reads: the blit-time ViewModel snapshot.
///
/// Gathering the renderer's state into one owned struct is the whole point: a
/// frame becomes computable, and therefore assertable, without a terminal.
/// `TuiRenderer::new` enables raw mode and installs a global panic hook, so no
/// test can construct a renderer — but any test can assemble this snapshot and
/// lay it out.
pub(crate) struct LiveViewModel<'a> {
    pub terminal_width: usize,
    pub terminal_height: usize,
    pub input_lines: &'a [String],
    pub input_cursor: (usize, usize),
    pub ghost_text: Option<&'a str>,
    pub effective_status: &'a str,
    pub cwd_label: &'a str,
    pub session_label: &'a str,
    pub dialog: Option<&'a super::Dialog>,
    /// Lines of the focused expanded tool-result surface (#656), pre-rendered
    /// by the caller so the planner stays a pure function of its inputs.
    pub expanded_lines: Option<&'a [String]>,
    /// A render error, like a dialog, owns the viewport and suppresses
    /// completions.
    pub render_error: bool,
    /// Polled session task list, including `Done` rows: the layout reserves a
    /// row for each, and the draw skips the finished ones.
    pub task_rows: &'a [crate::cli::tui::activity::ActivityRow],
    /// Child-agent rows, already ordered by depth then identity.
    pub tracked_rows: &'a [crate::cli::tui::activity::ActivityRow],
    pub live_rendered: &'a [RenderedTranscriptLine],
}

/// The transcript viewport and completion pane content for one frame, sized by
/// the claiming pass and painted after it.
pub(crate) struct LiveFrameContent {
    pub viewport: Vec<RenderedTranscriptLine>,
    pub completions: Vec<String>,
}

/// Project the root of the live frame: a single column whose chrome allocates
/// from the bottom — status, hr, input, hr, completions (0–N) — with the live
/// transcript viewport claiming the leftover (#805).
///
/// `content` is `None` for the sizing pass, which carries the completions
/// pane's natural extent and an empty viewport; the claims are
/// content-independent, so both passes produce identical rects.
pub(crate) fn project_root(
    vm: &LiveViewModel<'_>,
    content: Option<&LiveFrameContent>,
    natural_completion_rows: usize,
) -> Widget {
    let width = vm.terminal_width.max(1);
    let separator = super::session_separator_line(width, vm.cwd_label, vm.session_label);
    let status_lines: Vec<String> = vm.effective_status.lines().map(str::to_owned).collect();
    let completion_rows = content.map_or(natural_completion_rows, |c| c.completions.len());
    let viewport_lines = content.map_or(Vec::new(), |c| c.viewport.clone());
    let completions_rows = content
        .map(|c| c.completions.clone())
        .unwrap_or_else(|| vec![String::new(); completion_rows]);
    Widget::Stack {
        axis: Axis::Column,
        children: vec![
            (
                Track::Flex {
                    weight: 1,
                    min: usize::from(!vm.live_rendered.is_empty()),
                },
                Widget::Marked(
                    frame_key::TRANSCRIPT,
                    Box::new(Widget::Viewport {
                        lines: viewport_lines,
                    }),
                ),
            ),
            (
                Track::Natural,
                Widget::Marked(
                    frame_key::COMPLETIONS,
                    Box::new(Widget::Completions {
                        rows: completions_rows,
                    }),
                ),
            ),
            (
                Track::Natural,
                Widget::Marked(
                    frame_key::SEPARATOR,
                    Box::new(Widget::Text {
                        lines: vec![separator],
                    }),
                ),
            ),
            (
                Track::Natural,
                Widget::Marked(
                    frame_key::COMPOSER,
                    Box::new(Widget::Composer {
                        input_lines: vm.input_lines.to_vec(),
                        ghost: vm.ghost_text.map(str::to_owned),
                    }),
                ),
            ),
            (
                Track::Natural,
                Widget::Marked(frame_key::STATUS_RULE, Box::new(Widget::Rule)),
            ),
            (
                Track::Natural,
                Widget::Marked(
                    frame_key::STATUS,
                    Box::new(Widget::Text {
                        lines: status_lines,
                    }),
                ),
            ),
        ],
    }
}

/// Run the claiming pass over the live frame and return the layout with the
/// marked regions' rects.
///
/// `natural_completion_rows` is the pane's unclamped extent (0 when a critical
/// surface suppresses it); `content`, when given, is the pane and viewport
/// content sized to the first pass's claims.
pub(crate) fn claim_live_frame(
    vm: &LiveViewModel<'_>,
    content: Option<&LiveFrameContent>,
    natural_completion_rows: usize,
) -> widgets::Layout {
    let frame = Rect {
        x: 0,
        y: 0,
        width: vm.terminal_width.max(1),
        height: vm.terminal_height,
    };
    widgets::layout(&project_root(vm, content, natural_completion_rows), frame)
}

/// The claimed regions of a layout, by name.
pub(crate) fn frame_rects(layout: &widgets::Layout) -> FrameRects {
    FrameRects {
        transcript: layout.keyed(frame_key::TRANSCRIPT).unwrap_or_default(),
        completions: layout
            .keyed(frame_key::COMPLETIONS)
            .filter(|rect| !rect.is_empty()),
        separator: layout.keyed(frame_key::SEPARATOR).unwrap_or_default(),
        composer: layout.keyed(frame_key::COMPOSER).unwrap_or_default(),
        status_rule: layout.keyed(frame_key::STATUS_RULE).unwrap_or_default(),
        status: layout.keyed(frame_key::STATUS).unwrap_or_default(),
    }
}

/// The completions pane's natural (unclamped) extent, rendered so the
/// ephemeral selection window matches what a fully-afforded pane would paint.
pub(crate) fn natural_completion_pane(
    autocomplete: &mut AutocompleteState,
    width: usize,
    suppress: bool,
) -> Vec<String> {
    if suppress {
        // A zero budget clears the pane's rendered-row cache inside the
        // widget, keeping keyboard interactivity consistent with the claim.
        completion_pane_lines(autocomplete, width, 0);
        return Vec::new();
    }
    completion_pane_lines(autocomplete, width, usize::MAX)
}

/// The completion pane re-rendered to the claimed extent. A zero claim also
/// clears the widget's rendered-row cache so keyboard interactivity matches
/// what the pane painted.
pub(crate) fn completion_pane_for_claim(
    autocomplete: &mut AutocompleteState,
    width: usize,
    claimed_rows: usize,
    natural: &[String],
) -> Vec<String> {
    if claimed_rows == 0 {
        completion_pane_lines(autocomplete, width, 0);
        return Vec::new();
    }
    if claimed_rows >= natural.len() {
        return natural.to_vec();
    }
    completion_pane_lines(autocomplete, width, claimed_rows)
}

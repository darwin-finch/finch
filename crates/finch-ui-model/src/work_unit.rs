//! Plain WorkUnit snapshots and their pure transcript-node projection.
//!
//! Message producers construct these terminal-independent snapshots. Render
//! engines consume the resulting [`TranscriptNode`] without naming Finch's
//! conversation types.

use crate::{markdown, MessageId, NodeRole, RowId};

/// Status of a retained application message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageStatus {
    InProgress,
    Complete,
    Failed,
}

/// How one WorkUnit is presented in the transcript.
#[derive(Clone, Debug, Default)]
pub enum WorkUnitPresentation {
    #[default]
    Assistant,
    Activity {
        title: String,
    },
    ProgramSource {
        language: String,
    },
    ProgramOutput {
        title: Option<String>,
    },
}

/// Status of an individual tool or activity row.
#[derive(Clone, Debug)]
pub enum WorkRowStatus {
    Running,
    Complete(String),
    Error(String),
}

/// Whether a row is a model tool call or internal lifecycle activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkRowPresentation {
    Tool,
    Activity,
}

/// Lightweight snapshot for consumers that classify or filter WorkUnits.
#[derive(Clone, Debug)]
pub struct WorkUnitHead {
    pub message_id: MessageId,
    pub status: MessageStatus,
    pub presentation: WorkUnitPresentation,
    pub projects_as_prose: bool,
    pub response_text: String,
    pub transient_status: Option<String>,
    pub progress: Option<(u64, Option<u64>)>,
}

impl WorkUnitHead {
    /// Visible output body, including transient status and progress.
    pub fn output_body_lines(&self) -> Vec<String> {
        let mut body = self
            .response_text
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if let Some(status) = &self.transient_status {
            body.push(status.clone());
        }
        if let Some((completed, total)) = self.progress {
            body.push(format_progress(completed, total));
        }
        body
    }
}

/// One tool or activity row, with diffs already rendered to display lines.
#[derive(Clone, Debug)]
pub struct WorkRowView {
    pub label: String,
    pub status: WorkRowStatus,
    pub presentation: WorkRowPresentation,
    pub body_lines: Vec<String>,
    pub rendered_diffs: Option<Vec<String>>,
}

impl WorkRowView {
    pub(crate) fn has_output(&self) -> bool {
        !self.body_lines.is_empty()
            || self
                .rendered_diffs
                .as_ref()
                .is_some_and(|diffs| !diffs.is_empty())
    }
}

/// One child-agent lifecycle row.
#[derive(Clone, Debug)]
pub struct AgentActivityView {
    pub owner_row: Option<usize>,
    pub agent_id: uuid::Uuid,
    pub parent_agent_id: Option<uuid::Uuid>,
    pub label: String,
    pub status: WorkRowStatus,
    pub body_lines: Vec<String>,
    pub tools: Vec<AgentToolView>,
}

/// One tool run inside an agent lifecycle row.
#[derive(Clone, Debug)]
pub struct AgentToolView {
    pub name: String,
    pub status: WorkRowStatus,
}

/// Full blit-time domain snapshot of one WorkUnit run.
#[derive(Clone, Debug)]
pub struct WorkUnitView {
    pub head: WorkUnitHead,
    pub verb: String,
    pub rows: Vec<WorkRowView>,
    pub agent_activity: Vec<AgentActivityView>,
}

/// Widget props for one transcript row, projected from domain data.
#[derive(Debug, Clone)]
pub struct TranscriptNode {
    pub id: RowId,
    pub role: NodeRole,
    pub label: String,
    pub body: Vec<String>,
    pub children: Vec<TranscriptNode>,
    pub default_open: bool,
    /// Raw source body used by canonical scrollback when `body` is markdown-rendered.
    pub raw_body: Option<Vec<String>>,
}

/// Project one WorkUnit snapshot into transcript widget props.
pub fn project_work_unit(view: &WorkUnitView) -> TranscriptNode {
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
    let (role, label, body, raw_body, default_open) = match &head.presentation {
        WorkUnitPresentation::Assistant if !view.rows.is_empty() => {
            let actionable = view.rows.iter().any(tool_row_requires_default_expansion);
            let (body, raw_body) = assistant_body(&head.response_text);
            (
                NodeRole::ToolGroup,
                compact_tool_group_label(&view.rows),
                body,
                raw_body,
                head.status == MessageStatus::InProgress || actionable,
            )
        }
        WorkUnitPresentation::Assistant => {
            let (body, raw_body) = assistant_body(&head.response_text);
            (
                NodeRole::Response,
                assistant_prose_label(&view.verb, head),
                body,
                raw_body,
                true,
            )
        }
        WorkUnitPresentation::Activity { title } => (
            NodeRole::Activity,
            compact_activity_group_label(title, &view.rows),
            Vec::new(),
            None,
            head.status == MessageStatus::InProgress,
        ),
        WorkUnitPresentation::ProgramSource { language } => (
            NodeRole::Program,
            format!("Program source ({language})"),
            text_lines(&head.response_text),
            None,
            head.status == MessageStatus::InProgress,
        ),
        WorkUnitPresentation::ProgramOutput { title } => {
            let label = if head.projects_as_prose {
                assistant_prose_label(&view.verb, head)
            } else {
                title
                    .clone()
                    .unwrap_or_else(|| "Program output".to_string())
            };
            (
                NodeRole::Output,
                label,
                head.output_body_lines(),
                None,
                true,
            )
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
        raw_body,
    }
}

fn assistant_body(response_text: &str) -> (Vec<String>, Option<Vec<String>>) {
    let raw = text_lines(response_text);
    if raw.is_empty() {
        return (raw, None);
    }
    let rendered = markdown::render_viewport_body(response_text);
    if rendered == raw {
        (raw, None)
    } else {
        (rendered, Some(raw))
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
        raw_body: None,
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
            raw_body: None,
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
        raw_body: None,
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
        raw_body: None,
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
                raw_body: None,
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
        raw_body: None,
    }
}

fn assistant_prose_glyph(status: MessageStatus) -> &'static str {
    match status {
        MessageStatus::InProgress => "\u{25cb}",
        MessageStatus::Complete => "\u{23fa}",
        MessageStatus::Failed => "\u{2298}",
    }
}

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

fn compact_summary_text(text: &str, max_chars: usize) -> String {
    let first_line = text.lines().next().unwrap_or_default().trim();
    let mut compact = first_line.chars().take(max_chars).collect::<String>();
    if first_line.chars().count() > max_chars {
        compact.push('…');
    }
    compact
}

fn compact_tool_group_label(rows: &[WorkRowView]) -> String {
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

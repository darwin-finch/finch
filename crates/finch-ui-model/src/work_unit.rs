//! Plain WorkUnit snapshots and their pure transcript-node projection.
//!
//! Message producers construct these terminal-independent snapshots. Render
//! engines consume the resulting [`TranscriptNode`] without naming Finch's
//! conversation types.

use crate::{component::TurnIndicatorView, markdown, MessageId, NodeRole, RowId};

/// Status of a retained application message.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageStatus {
    InProgress,
    Complete,
    Failed,
}

/// How one WorkUnit is presented in the transcript.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub enum WorkUnitPresentation {
    #[default]
    Assistant,
    /// A durable Interactive Brain run projected as the semantic assistant
    /// turn it represents, without exposing its internal RunId as chrome.
    Interactive,
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
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum WorkRowStatus {
    Running,
    Complete(String),
    Error(String),
}

/// Whether a row is a model tool call or internal lifecycle activity.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkRowPresentation {
    Tool,
    Activity,
}

/// Lightweight snapshot for consumers that classify or filter WorkUnits.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct WorkUnitHead {
    pub message_id: MessageId,
    pub status: MessageStatus,
    pub presentation: WorkUnitPresentation,
    pub projects_as_prose: bool,
    pub response_text: String,
    pub transient_status: Option<String>,
    pub progress: Option<(u64, Option<u64>)>,
    /// The turn was cancelled rather than failing on its own.
    pub cancelled: bool,
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
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
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
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
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
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct AgentToolView {
    pub name: String,
    pub status: WorkRowStatus,
}

/// Full blit-time domain snapshot of one WorkUnit run.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct WorkUnitView {
    pub head: WorkUnitHead,
    pub verb: String,
    pub rows: Vec<WorkRowView>,
    pub agent_activity: Vec<AgentActivityView>,
    /// How long the unit has run: live while in progress, the captured
    /// finish time afterwards.
    pub elapsed: std::time::Duration,
    /// Approximate tokens received from the provider for this unit.
    pub token_count: usize,
    /// True once the unit has been handed a provider request: it is the
    /// turn's generation unit, so it keeps the turn indicator while it
    /// streams a program or runs a tool round.
    pub awaiting_provider: bool,
}

impl WorkUnitView {
    /// What decides whether this unit paints differently from the last
    /// frame. `elapsed` and `token_count` change on every poll while a unit
    /// runs, but they reach the screen only through the turn indicator, so
    /// the key carries the indicator's text instead of the raw values: an
    /// in-progress unit repaints when its pulse frame, seconds, or token
    /// readout changes, and never merely because time passed.
    pub fn paint_key(&self) -> String {
        format!(
            "{:?} {:?} {:?} {:?} {:?}",
            self.head,
            self.verb,
            self.rows,
            self.agent_activity,
            turn_indicator(self).map(|indicator| indicator.plain_text()),
        )
    }

    /// A pending prose row: the unit will present assistant prose, and no
    /// tool rows have claimed it.
    fn is_pending_prose(&self) -> bool {
        match &self.head.presentation {
            WorkUnitPresentation::Assistant => self.rows.is_empty(),
            WorkUnitPresentation::Interactive => true,
            WorkUnitPresentation::ProgramOutput { .. } => self.head.projects_as_prose,
            WorkUnitPresentation::Activity { .. } | WorkUnitPresentation::ProgramSource { .. } => {
                false
            }
        }
    }

    /// Nothing of this unit's own is on screen yet: no text, no rows, no
    /// child-agent activity.
    fn has_no_content(&self) -> bool {
        self.head.output_body_lines().is_empty()
            && self.rows.is_empty()
            && self.agent_activity.is_empty()
    }
}

/// The in-progress indicator this unit owns, if it owns one.
///
/// A unit owns the turn's indicator while it is in progress and either is
/// the turn's generation unit (`awaiting_provider`) or is a pending prose row
/// with no text yet. Completed and failed units never own one, and a unit
/// that is only a local tool group, lifecycle activity, or typed program
/// does not either.
pub fn turn_indicator(view: &WorkUnitView) -> Option<TurnIndicatorView> {
    if view.head.status != MessageStatus::InProgress || view.head.cancelled {
        return None;
    }
    let pending_prose = view.is_pending_prose() && view.head.response_text.is_empty();
    (view.awaiting_provider || pending_prose).then(|| TurnIndicatorView {
        verb: view.verb.clone(),
        elapsed: view.elapsed,
        token_count: view.token_count,
    })
}

/// One WorkUnit as the live transcript draws it: its transcript node, when
/// it has anything of its own to show, and the turn indicator beneath it,
/// when it owns one.
#[derive(Debug, Clone)]
pub struct LiveWorkUnit {
    pub node: Option<TranscriptNode>,
    pub indicator: Option<TurnIndicatorView>,
}

/// Project one WorkUnit snapshot for the live transcript.
///
/// The indicator is always its own row, drawn once, after the unit's
/// content. A pending unit with nothing of its own on screen is the
/// indicator alone — its node, whose label would repeat the same indicator
/// text, is not drawn.
pub fn project_live_work_unit(view: &WorkUnitView) -> LiveWorkUnit {
    let indicator = turn_indicator(view);
    let indicator_only = indicator.is_some() && view.is_pending_prose() && view.has_no_content();
    LiveWorkUnit {
        node: (!indicator_only).then(|| project_work_unit(view)),
        indicator,
    }
}

/// Widget props for one transcript row, projected from domain data.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
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
                assistant_prose_label(view),
                body,
                raw_body,
                true,
            )
        }
        WorkUnitPresentation::Interactive => {
            let (body, raw_body) = assistant_body(&head.response_text);
            (
                NodeRole::Response,
                assistant_prose_label(view),
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
                assistant_prose_label(view)
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

/// The header label of a prose row. A pending row with no text yet has no
/// label of its own: it reads as the turn indicator, the same description
/// the live transcript draws, so the two can never disagree.
fn assistant_prose_label(view: &WorkUnitView) -> String {
    let head = &view.head;
    let glyph = assistant_prose_glyph(head.status);
    if !head.response_text.is_empty() {
        return glyph.to_string();
    }
    match head.status {
        MessageStatus::InProgress => turn_indicator(view)
            .map(|indicator| indicator.plain_text())
            .unwrap_or_else(|| glyph.to_string()),
        MessageStatus::Complete => format!("{glyph} No assistant text"),
        MessageStatus::Failed if head.cancelled => format!("{glyph} Turn cancelled"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn pending(presentation: WorkUnitPresentation) -> WorkUnitView {
        WorkUnitView {
            head: WorkUnitHead {
                message_id: MessageId::new(),
                status: MessageStatus::InProgress,
                presentation,
                projects_as_prose: false,
                response_text: String::new(),
                transient_status: None,
                progress: None,
                cancelled: false,
            },
            verb: "Channeling".into(),
            rows: Vec::new(),
            agent_activity: Vec::new(),
            elapsed: Duration::ZERO,
            token_count: 0,
            awaiting_provider: false,
        }
    }

    fn tool_row(status: WorkRowStatus) -> WorkRowView {
        WorkRowView {
            label: "bash(ls)".into(),
            status,
            presentation: WorkRowPresentation::Tool,
            body_lines: Vec::new(),
            rendered_diffs: None,
        }
    }

    /// INVARIANT (#1664): a unit owns the turn indicator only while it is in
    /// progress, and only when it is the turn's generation unit or a pending
    /// prose row with no text. Local tool groups, lifecycle activity, typed
    /// programs, and finished units own none.
    #[test]
    fn test_turn_indicator_ownership_follows_the_units_state() {
        let waiting = pending(WorkUnitPresentation::Assistant);

        let mut generation_with_tools = pending(WorkUnitPresentation::Assistant);
        generation_with_tools.awaiting_provider = true;
        generation_with_tools
            .rows
            .push(tool_row(WorkRowStatus::Running));

        let mut streaming_program = pending(WorkUnitPresentation::ProgramSource {
            language: "lisp".into(),
        });
        streaming_program.awaiting_provider = true;
        streaming_program.head.response_text = "(say".into();

        let mut local_tools = pending(WorkUnitPresentation::Assistant);
        local_tools.rows.push(tool_row(WorkRowStatus::Running));

        let typed_program = pending(WorkUnitPresentation::ProgramSource {
            language: "forth".into(),
        });
        let activity = pending(WorkUnitPresentation::Activity {
            title: "Speculative run".into(),
        });

        let mut streaming_prose = pending(WorkUnitPresentation::Interactive);
        streaming_prose.head.response_text = "Hello".into();

        let mut complete = pending(WorkUnitPresentation::Assistant);
        complete.awaiting_provider = true;
        complete.head.status = MessageStatus::Complete;
        let mut failed = complete.clone();
        failed.head.status = MessageStatus::Failed;

        let owns = [
            ("waiting prose row", &waiting),
            ("generation unit in a tool round", &generation_with_tools),
            ("generation unit streaming a program", &streaming_program),
            ("local tool group", &local_tools),
            ("typed program", &typed_program),
            ("lifecycle activity", &activity),
            ("prose row with text", &streaming_prose),
            ("completed generation unit", &complete),
            ("failed generation unit", &failed),
        ]
        .map(|(name, view)| (name, turn_indicator(view).is_some()));
        assert_eq!(
            owns,
            [
                ("waiting prose row", true),
                ("generation unit in a tool round", true),
                ("generation unit streaming a program", true),
                ("local tool group", false),
                ("typed program", false),
                ("lifecycle activity", false),
                ("prose row with text", false),
                ("completed generation unit", false),
                ("failed generation unit", false),
            ],
            "which unit states own the turn's in-progress indicator"
        );
    }

    /// INVARIANT (#1664): the live projection draws the indicator once. A
    /// pending unit with nothing of its own is the indicator alone — its
    /// node, whose label is the same indicator text, is not drawn beside it;
    /// a unit with content keeps its node and gains the indicator after it.
    #[test]
    fn test_live_projection_never_draws_the_indicator_twice() {
        let waiting = pending(WorkUnitPresentation::Assistant);
        let live = project_live_work_unit(&waiting);
        assert!(
            live.node.is_none() && live.indicator.is_some(),
            "a waiting unit is the indicator alone; live={live:?}"
        );
        assert_eq!(
            project_work_unit(&waiting).label,
            live.indicator
                .as_ref()
                .expect("the waiting unit owns an indicator")
                .plain_text(),
            "the pending node label, used outside the live transcript, is the same \
             indicator description"
        );

        let mut tool_round = pending(WorkUnitPresentation::Assistant);
        tool_round.awaiting_provider = true;
        tool_round.rows.push(tool_row(WorkRowStatus::Running));
        let live = project_live_work_unit(&tool_round);
        let node = live.node.as_ref().expect("a tool round keeps its node");
        let indicator = live
            .indicator
            .as_ref()
            .expect("the generation unit keeps the indicator in a tool round");
        assert!(
            !format!("{node:?}").contains(&indicator.activity()),
            "the node of a unit with content never repeats the indicator; node={node:?}"
        );
    }

    /// INVARIANT (#1664): the repaint key changes exactly when the painted
    /// indicator changes. Time passing within one pulse frame, or tokens
    /// arriving on a unit that owns no indicator, changes nothing on screen
    /// and must not change the key.
    #[test]
    fn test_paint_key_changes_only_when_the_painted_indicator_changes() {
        let at = |millis: u64, tokens: usize| {
            let mut view = pending(WorkUnitPresentation::Assistant);
            view.head.message_id = MessageId::from_uuid(uuid::Uuid::nil());
            view.elapsed = Duration::from_millis(millis);
            view.token_count = tokens;
            view.paint_key()
        };
        assert_eq!(
            at(0, 0),
            at(199, 0),
            "time passing inside one pulse frame repaints nothing"
        );
        assert_ne!(at(0, 0), at(200, 0), "a new pulse frame repaints");
        assert_ne!(at(0, 0), at(0, 3), "the first tokens repaint");

        let finished = |millis: u64, tokens: usize| {
            let mut view = pending(WorkUnitPresentation::Assistant);
            view.head.message_id = MessageId::from_uuid(uuid::Uuid::nil());
            view.head.status = MessageStatus::Complete;
            view.elapsed = Duration::from_millis(millis);
            view.token_count = tokens;
            view.paint_key()
        };
        assert_eq!(
            finished(0, 0),
            finished(60_000, 900),
            "a finished unit has no indicator, so neither time nor tokens repaint it"
        );
    }
}

//! Read-only projections: snapshots, unhydrated listings, and observer views.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::attachment::{AttachmentRole, BrainAttachment};
use super::journal::{
    scan_readonly, BrainEvent, BrainEventKind, BrainId, BrainMetadata, BrainProgram,
    CommittedMemoryRecord, BRAIN_METADATA_VERSION,
};
use super::run::{
    BrainRun, BrainRunKind, BrainRunStatus, BrainRunnerHandoff, BrainRunnerLease, RunId,
    RunnerLeaseId,
};
use super::schedule::{BrainSchedule, BrainScheduleDue};
use super::tasks::BrainTask;

/// The one machine/workspace boundary in which a brain may cause effects.
///
/// There is deliberately no separate `execution_head`: the machine that owns
/// the workspace is the only machine allowed to execute the brain's programs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainEnvironment {
    pub machine: String,
    pub workspace: PathBuf,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrainSnapshot {
    pub brain_id: BrainId,
    pub name: String,
    pub environment: BrainEnvironment,
    pub revision: u64,
    pub events: Vec<BrainEvent>,
    pub program_stack: Vec<BrainProgram>,
    pub attachments: Vec<BrainAttachment>,
    pub runner_lease: Option<BrainRunnerLease>,
    #[serde(default)]
    pub runner_handoff: Option<BrainRunnerHandoff>,
    #[serde(default)]
    pub runs: Vec<BrainRun>,
    /// Current task-list projection derived from `TaskListReplaced` events.
    #[serde(default)]
    pub tasks: Vec<BrainTask>,
    /// Current committed (byte-stable) recall-set projection derived from
    /// `CommittedMemoriesReplaced` events (#940).
    #[serde(default)]
    pub committed_memories: Vec<CommittedMemoryRecord>,
    #[serde(default)]
    pub schedules: Vec<BrainSchedule>,
    #[serde(default)]
    pub pending_schedule_dues: Vec<BrainScheduleDue>,
    /// Canonical projection of the schema-v15 effect-audit transitions.
    #[serde(default)]
    pub effect_audits: Vec<finch_runtime::EffectAuditEntry>,
}

impl BrainSnapshot {
    /// Whether this exact runner lease was durably replaced by an addressed
    /// handoff. Frontends use this terminal fact to stop renewal instead of
    /// treating a deliberate transfer like an incidental lease expiry.
    pub fn runner_lease_was_handed_off(&self, lease_id: RunnerLeaseId) -> bool {
        let requested: std::collections::HashSet<_> = self
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                BrainEventKind::RunnerHandoffRequested { handoff }
                    if handoff.from_lease_id == lease_id =>
                {
                    Some(handoff.handoff_id)
                }
                _ => None,
            })
            .collect();
        self.events.iter().any(|event| {
            matches!(
                &event.kind,
                BrainEventKind::RunnerHandoffCompleted { handoff_id, .. }
                    if requested.contains(handoff_id)
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrainWireMessage {
    Snapshot { brain: BrainSnapshot },
    Event { event: BrainEvent },
}

/// One currently connected participant, as projected from the event log
/// without hydrating the Brain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainListAttachment {
    pub subject: String,
    pub role: AttachmentRole,
}

/// One live subagent run, as projected from the event log without hydrating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainListAgent {
    pub run_id: RunId,
    pub status: BrainRunStatus,
    pub initiated_by: String,
}

/// Directory-and-journal facts about one named Brain, gathered without
/// replaying the reducer, opening effect-audit databases, or creating files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainListSummary {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brain_id: Option<BrainId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_ms: Option<u64>,
    pub bytes: u64,
    pub events: u64,
    pub revision: u64,
    pub turns: u64,
    pub attached: Vec<BrainListAttachment>,
    pub agents: Vec<BrainListAgent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
}

pub fn observer_effect_audit_event(event: &BrainEvent) -> BrainEvent {
    let mut projected = event.clone();
    if let BrainEventKind::EffectAuditTransition { transition } = &event.kind {
        projected.kind = BrainEventKind::EffectAuditTransition {
            transition: transition.observer_projection(),
        };
    }
    projected
}

pub fn directory_bytes(root: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if let Ok(metadata) = entry.metadata() {
                total += metadata.len();
            }
        }
    }
    total
}

/// Per-Brain listing facts without hydrating or creating files.
pub fn summarize_unhydrated(root: Option<&Path>, name: &str, now_ms: u64) -> BrainListSummary {
    let path = root.map(|root| root.join(name));
    let mut summary = BrainListSummary {
        name: name.to_string(),
        path: path.clone(),
        brain_id: None,
        created_ms: None,
        updated_ms: None,
        bytes: 0,
        events: 0,
        revision: 0,
        turns: 0,
        attached: Vec::new(),
        agents: Vec::new(),
        runner: None,
    };
    let Some(directory) = path else {
        return summary;
    };
    summary.bytes = directory_bytes(&directory);
    if let Ok(bytes) = std::fs::read(directory.join("metadata.json")) {
        if let Ok(metadata) = serde_json::from_slice::<BrainMetadata>(&bytes) {
            if metadata.version == BRAIN_METADATA_VERSION && metadata.brain_id != BrainId::nil() {
                summary.brain_id = Some(metadata.brain_id);
                summary.created_ms = Some(metadata.created_ms);
            }
        }
    }
    let projection = scan_readonly(&directory.join("events.jsonl"));
    summary.events = projection.events;
    summary.revision = projection.revision;
    summary.turns = projection.turns;
    summary.updated_ms = projection.updated_ms;
    summary.runner = projection
        .runner
        .filter(|lease| lease.expires_ms > now_ms)
        .map(|lease| lease.subject);
    let mut attached: Vec<BrainListAttachment> = projection
        .attachments
        .into_values()
        .filter(|attachment| attachment.connected)
        .map(|attachment| BrainListAttachment {
            subject: attachment.subject,
            role: attachment.role,
        })
        .collect();
    attached.sort_by(|left, right| {
        left.subject
            .cmp(&right.subject)
            .then_with(|| format!("{:?}", left.role).cmp(&format!("{:?}", right.role)))
    });
    summary.attached = attached;
    let mut agents: Vec<BrainListAgent> = projection
        .runs
        .into_values()
        .filter(|run| run.kind == BrainRunKind::Subagent && !run.status.is_terminal())
        .map(|run| BrainListAgent {
            run_id: run.run_id,
            status: run.status,
            initiated_by: run.initiated_by,
        })
        .collect();
    agents.sort_by_key(|agent| agent.run_id.0);
    summary.agents = agents;
    summary
}

#[cfg(test)]
mod tests;

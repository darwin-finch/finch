//! BrainRun lifecycle, runner leases, cancellation, and terminalization.
//!
//! Durable run records live in the journal. This facade owns the transition
//! table, lease/handoff types, and disconnect-terminalization intent files.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::attachment::AttachmentId;
use super::journal::{create_dir_all_durable, sync_directory};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunnerLeaseId(pub uuid::Uuid);

impl RunnerLeaseId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunnerHandoffId(pub uuid::Uuid);

impl RunnerHandoffId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub uuid::Uuid);

impl RunId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrainRunKind {
    Interactive,
    Speculative,
    Scheduled,
    Subagent,
    Maintenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrainRunStatus {
    QueuedForEnvironment,
    Running,
    AwaitingApproval,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl BrainRunStatus {
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainRun {
    pub run_id: RunId,
    pub kind: BrainRunKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
    pub request_seq: u64,
    pub initiating_attachment_id: AttachmentId,
    pub initiated_by: String,
    pub status: BrainRunStatus,
    pub started_ms: u64,
    pub updated_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainRunnerLease {
    pub lease_id: RunnerLeaseId,
    pub subject: String,
    pub environment_generation: u64,
    pub acquired_ms: u64,
    pub expires_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainRunnerHandoff {
    pub handoff_id: RunnerHandoffId,
    pub from_lease_id: RunnerLeaseId,
    pub requested_by: String,
    pub target_subject: String,
    pub environment_generation: u64,
    pub requested_ms: u64,
    pub expires_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrainRunCancellationReservation {
    pub run: BrainRun,
    pub needs_runner_cancel: bool,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisconnectTerminalizationIntent {
    pub sender: String,
    pub run_id: RunId,
    pub request_seq: u64,
    pub status: BrainRunStatus,
    pub detail: String,
}

/// Reject any transition that would leave a terminal run, or skip an allowed
/// step. Exact-once terminal state depends on this table staying closed.
pub fn validate_run_transition(from: BrainRunStatus, to: BrainRunStatus) -> Result<()> {
    use BrainRunStatus::*;
    let allowed = match from {
        QueuedForEnvironment => matches!(to, Running | Cancelled | Failed),
        Running => matches!(
            to,
            AwaitingApproval | Completed | Failed | Cancelled | Interrupted
        ),
        AwaitingApproval => matches!(to, Running | Failed | Cancelled | Interrupted),
        Interrupted => matches!(to, Running | Failed | Cancelled),
        Completed | Failed | Cancelled => false,
    };
    if !allowed {
        anyhow::bail!("invalid Brain run transition from {from:?} to {to:?}");
    }
    if from.is_terminal() {
        anyhow::bail!("terminal Brain run cannot transition");
    }
    Ok(())
}

pub fn sorted_runs(runs: &HashMap<RunId, BrainRun>) -> Vec<BrainRun> {
    let mut runs = runs.values().cloned().collect::<Vec<_>>();
    runs.sort_by_key(|run| (run.request_seq, run.run_id.0));
    runs
}

pub fn disconnect_intent_path(root: Option<&Path>, name: &str, run_id: RunId) -> Option<PathBuf> {
    root.map(|root| {
        root.join(name)
            .join("disconnect-terminalizations")
            .join(format!("{}.json", run_id.0))
    })
}

pub fn persist_disconnect_intent(
    root: Option<&Path>,
    name: &str,
    intent: &DisconnectTerminalizationIntent,
) -> Result<()> {
    let Some(path) = disconnect_intent_path(root, name, intent.run_id) else {
        return Ok(());
    };
    let directory = path.parent().expect("disconnect intent has a parent");
    create_dir_all_durable(directory)?;
    let temporary = directory.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, serde_json::to_vec(intent)?)?;
    std::fs::File::open(&temporary)?.sync_all()?;
    std::fs::rename(&temporary, &path)?;
    sync_directory(directory)
}

pub fn clear_disconnect_intent(root: Option<&Path>, name: &str, run_id: RunId) -> Result<()> {
    let Some(path) = disconnect_intent_path(root, name, run_id) else {
        return Ok(());
    };
    if path.exists() {
        std::fs::remove_file(&path)?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

pub fn read_disconnect_intents(
    root: Option<&Path>,
    name: &str,
) -> Result<HashMap<RunId, DisconnectTerminalizationIntent>> {
    let Some(root) = root else {
        return Ok(HashMap::new());
    };
    let directory = root.join(name).join("disconnect-terminalizations");
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(HashMap::new());
    };
    let mut intents = HashMap::new();
    for entry in entries {
        let entry = entry?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let intent: DisconnectTerminalizationIntent =
            serde_json::from_slice(&std::fs::read(entry.path())?)?;
        intents.insert(intent.run_id, intent);
    }
    Ok(intents)
}

#[cfg(test)]
mod tests;

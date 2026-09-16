//! Schedule records, initialization contract, and due-work selection.
//!
//! The journal is the durability path. This facade owns schedule types, the
//! store-wide due index, and the due-window arithmetic used to queue work.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

use super::attachment::AttachmentId;
use super::journal::BrainId;
use super::run::{BrainRun, BrainRunKind, BrainRunStatus, RunId};

mod index;

pub(crate) use index::ScheduleIndex;

const BRAIN_INITIALIZATION_VERSION: u32 = 1;
const DEFAULT_INITIALIZATION_MODULE: &str = "finch.brain.initialization";
pub(crate) const DEFAULT_INITIALIZATION_SOURCE: &str = "(define (finch-brain-initialized) : int 1)";

// `Ord` so the due index can key on `(next_due_ms, ScheduleId)` and keep a
// total order: two schedules due in the same millisecond still have a stable,
// deterministic position rather than colliding (#374).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScheduleId(pub uuid::Uuid);

impl ScheduleId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramLanguage {
    Forth,
    Lisp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrainScheduleDeliveryPolicy {
    Coalesce,
    BoundedCatchUp {
        max_catch_up: u32,
        expires_after_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainSchedule {
    pub schedule_id: ScheduleId,
    #[serde(default = "legacy_schedule_attachment_id")]
    pub initiating_attachment_id: AttachmentId,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub grant_ceiling: crate::vm::EffectSet,
    pub language: ProgramLanguage,
    pub source: String,
    pub next_due_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
    pub delivery_policy: BrainScheduleDeliveryPolicy,
    /// Set only by trusted, reviewed Brain-module scheduling paths. This is
    /// persisted so source-equivalent public schedules cannot impersonate or
    /// suppress the module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_identity: Option<BrainScheduleModuleIdentity>,
    pub active: bool,
}

/// One durable schedule delivery and the queued run that owns it. Keeping the
/// run in this event makes due calculation -> runnable work one atomic append.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainScheduleDue {
    pub schedule_id: ScheduleId,
    pub run: BrainRun,
    /// Immutable program snapshot for this delivery. Later schedule edits do
    /// not change already queued work.
    #[serde(default = "legacy_schedule_language")]
    pub language: ProgramLanguage,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub grant_ceiling: crate::vm::EffectSet,
    pub due_at_ms: u64,
    pub first_missed_at_ms: u64,
    pub missed_count: u32,
    /// The next occurrence after all ticks represented by this delivery.
    /// `None` atomically retires a one-shot schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_due_ms: Option<u64>,
}

/// Reviewed, immutable program that establishes a Brain's initial typed state.
/// Loading this record is inert: execution requires an explicit scheduled run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainInitialization {
    pub version: u32,
    pub brain_id: BrainId,
    pub module: String,
    pub module_revision: u32,
    pub language: ProgramLanguage,
    pub source: String,
    pub source_sha256: String,
    pub capability_budget: crate::vm::EffectSet,
}

/// Durable, non-authority-bearing identity for a reviewed module scheduled by
/// the Brain itself. Public schedule creation never accepts this marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainScheduleModuleIdentity {
    pub module: String,
    pub module_revision: u32,
    pub source_sha256: String,
}

impl BrainInitialization {
    pub(crate) fn reviewed_default(brain_id: BrainId) -> Self {
        Self {
            version: BRAIN_INITIALIZATION_VERSION,
            brain_id,
            module: DEFAULT_INITIALIZATION_MODULE.into(),
            module_revision: 1,
            language: ProgramLanguage::Lisp,
            source: DEFAULT_INITIALIZATION_SOURCE.into(),
            source_sha256: hex::encode(Sha256::digest(DEFAULT_INITIALIZATION_SOURCE.as_bytes())),
            capability_budget: crate::vm::EffectSet::pure(),
        }
    }

    pub(crate) fn validate(&self, brain_id: BrainId) -> Result<()> {
        anyhow::ensure!(
            self.version == BRAIN_INITIALIZATION_VERSION,
            "unsupported Brain initialization version {}",
            self.version
        );
        anyhow::ensure!(
            self.brain_id == brain_id,
            "Brain initialization identity does not match metadata"
        );
        anyhow::ensure!(
            !self.module.trim().is_empty() && self.module_revision > 0,
            "Brain initialization module identity is invalid"
        );
        anyhow::ensure!(
            !self.source.trim().is_empty(),
            "Brain initialization program is empty"
        );
        let actual = hex::encode(Sha256::digest(self.source.as_bytes()));
        anyhow::ensure!(
            self.source_sha256 == actual,
            "Brain initialization program digest does not match its source"
        );
        anyhow::ensure!(
            self == &Self::reviewed_default(brain_id),
            "Brain initialization contract is not the reviewed built-in module"
        );
        Ok(())
    }

    pub(crate) fn module_identity(&self) -> BrainScheduleModuleIdentity {
        BrainScheduleModuleIdentity {
            module: self.module.clone(),
            module_revision: self.module_revision,
            source_sha256: self.source_sha256.clone(),
        }
    }

    pub(crate) fn validate_schedule(&self, schedule: &BrainSchedule) -> Result<()> {
        let identity = schedule
            .module_identity
            .as_ref()
            .context("reviewed Brain-module schedule is missing its module identity")?;
        anyhow::ensure!(
            identity == &self.module_identity(),
            "Brain-module schedule identity does not match the reviewed initialization module"
        );
        anyhow::ensure!(
            schedule.language == self.language,
            "Brain initialization schedule language does not match the reviewed module"
        );
        let actual = hex::encode(Sha256::digest(schedule.source.as_bytes()));
        anyhow::ensure!(
            identity.source_sha256 == actual,
            "Brain initialization schedule digest does not match its source"
        );
        anyhow::ensure!(
            schedule.source == self.source,
            "Brain initialization schedule source is not the reviewed module"
        );
        anyhow::ensure!(
            schedule.grant_ceiling == self.capability_budget,
            "Brain initialization schedule capability budget is not the reviewed ceiling"
        );
        anyhow::ensure!(
            schedule.interval_ms.is_none()
                && schedule.delivery_policy == BrainScheduleDeliveryPolicy::Coalesce,
            "Brain initialization schedule must be a coalesced one-shot"
        );
        Ok(())
    }

    pub(crate) fn validate_schedule_due(
        &self,
        schedule: &BrainSchedule,
        due: &BrainScheduleDue,
    ) -> Result<()> {
        self.validate_schedule(schedule)?;
        anyhow::ensure!(
            due.language == schedule.language
                && due.source == schedule.source
                && due.grant_ceiling == schedule.grant_ceiling,
            "Brain initialization delivery does not match its reviewed schedule"
        );
        Ok(())
    }
}

pub fn legacy_schedule_attachment_id() -> AttachmentId {
    AttachmentId(uuid::Uuid::nil())
}

pub const fn legacy_schedule_language() -> ProgramLanguage {
    ProgramLanguage::Forth
}

pub fn queued_schedule_run(schedule: &BrainSchedule, request_seq: u64, now_ms: u64) -> BrainRun {
    BrainRun {
        run_id: RunId::new(),
        kind: BrainRunKind::Scheduled,
        parent_run_id: None,
        request_seq,
        initiating_attachment_id: schedule.initiating_attachment_id,
        initiated_by: schedule.created_by.clone(),
        status: BrainRunStatus::QueuedForEnvironment,
        started_ms: now_ms,
        updated_ms: now_ms,
        detail: None,
    }
}

pub fn schedule_due_window(
    schedule: &BrainSchedule,
    now_ms: u64,
) -> Result<(u32, u64, Option<u64>)> {
    let Some(interval_ms) = schedule.interval_ms else {
        return Ok((1, schedule.next_due_ms, None));
    };
    let elapsed = now_ms.saturating_sub(schedule.next_due_ms);
    let count = elapsed / interval_ms + 1;
    let last_due_ms = schedule
        .next_due_ms
        .checked_add(
            (count - 1)
                .checked_mul(interval_ms)
                .context("schedule overflow")?,
        )
        .context("schedule overflow")?;
    let next_due_ms = last_due_ms
        .checked_add(interval_ms)
        .context("schedule overflow")?;
    Ok((
        u32::try_from(count).unwrap_or(u32::MAX),
        last_due_ms,
        Some(next_due_ms),
    ))
}

pub fn sorted_schedules(schedules: &HashMap<ScheduleId, BrainSchedule>) -> Vec<BrainSchedule> {
    let mut schedules = schedules.values().cloned().collect::<Vec<_>>();
    schedules.sort_by_key(|schedule| (schedule.next_due_ms, schedule.schedule_id.0));
    schedules
}

pub fn sorted_schedule_dues(dues: &HashMap<RunId, BrainScheduleDue>) -> Vec<BrainScheduleDue> {
    let mut dues = dues.values().cloned().collect::<Vec<_>>();
    dues.sort_by_key(|due| (due.due_at_ms, due.schedule_id.0, due.run.run_id.0));
    dues
}

#[cfg(test)]
mod tests;

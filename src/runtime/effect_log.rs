//! Durable application-side delivery log for portable VM effects.

use crate::runtime::VmEffectEnvelope;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Maximum serialized intent admitted for one host effect. Admission occurs
/// before a permit can be issued, so an untrusted model cannot turn the audit
/// log into an unbounded payload sink.
pub const MAX_EFFECT_AUDIT_INTENT_BYTES: usize = 256 * 1024;
/// Maximum serialized terminal outcome retained for one host effect.
pub const MAX_EFFECT_AUDIT_OUTCOME_BYTES: usize = 64 * 1024;
/// Maximum number of non-terminal effects owned by one named-Brain run.
pub const MAX_ACTIVE_EFFECT_AUDITS_PER_RUN: usize = 64;
/// Brain-wide unresolved admission bound across concurrent runs.
pub const MAX_ACTIVE_EFFECT_AUDITS_PER_BRAIN: usize = 256;
/// Brain-wide bound for canonical pre-redaction effect payloads represented
/// by unresolved reservations.
pub const MAX_ACTIVE_EFFECT_AUDIT_BYTES_PER_BRAIN: usize = 16 * 1024 * 1024;
/// Maximum encoded bytes for one compact replay fence transition.
pub const MAX_EFFECT_AUDIT_REPLAY_FENCE_TRANSITION_BYTES: usize = 768;
/// Maximum encoded bytes for its complete canonical Brain event envelope.
pub const MAX_EFFECT_AUDIT_REPLAY_FENCE_EVENT_BYTES: usize = 1_024;
/// Byte budget reserved for the compact replay index.
pub const EFFECT_AUDIT_REPLAY_INDEX_BUDGET_BYTES: usize = 32 * 1024 * 1024;
/// Capacity derived from the fixed event bound and replay-index budget.
pub const MAX_EFFECT_AUDIT_REPLAY_FENCES_PER_BRAIN: usize =
    EFFECT_AUDIT_REPLAY_INDEX_BUDGET_BYTES / MAX_EFFECT_AUDIT_REPLAY_FENCE_EVENT_BYTES;
/// Upper bound for the audit share of the canonical Brain journal. Finite
/// storage cannot retain infinite exact membership: at replay-index
/// exhaustion admission fails closed and the operator must archive the Brain.
pub const MAX_EFFECT_AUDIT_JOURNAL_BYTES_PER_BRAIN: usize = 64 * 1024 * 1024;

/// Complete immutable identity of one named-Brain host effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EffectAuditIdentity {
    pub brain_id: Uuid,
    pub run_id: Uuid,
    pub request_seq: u64,
    pub execution_id: Uuid,
    pub effect_sequence: u64,
}

/// Provenance minted by the daemon when it creates a run-scoped reverse
/// capability. This is audit data, not a bearer token exposed on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectAuditAuthority {
    pub authority_id: Uuid,
    pub runner_lease_id: Uuid,
    pub runner_subject: String,
    pub connection_id: Option<Uuid>,
    pub environment_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectAuditIntent {
    pub identity: EffectAuditIdentity,
    pub capability: crate::vm::CapabilityKind,
    pub selector: crate::vm::ResourceSelector,
    pub output: Vec<crate::vm::Type>,
    pub effect_kind: String,
    pub payload_bytes: usize,
    pub canonical_sha256: String,
}

impl EffectAuditIntent {
    pub fn from_effect(
        identity: EffectAuditIdentity,
        effect: &crate::vm::VmSideEffect,
    ) -> Result<Self> {
        anyhow::ensure!(
            identity.effect_sequence == effect.sequence,
            "effect audit identity sequence does not match its effect"
        );
        let canonical = serde_json::to_vec(effect)?;
        anyhow::ensure!(
            canonical.len() <= MAX_EFFECT_AUDIT_INTENT_BYTES,
            "effect audit intent exceeds the bounded payload limit"
        );
        let effect_kind = match &effect.event {
            crate::vm::HostSideEffect::Emit { .. } => "emit",
            crate::vm::HostSideEffect::Ui { .. } => "ui",
            crate::vm::HostSideEffect::Request { .. } => "request",
        }
        .to_string();
        Ok(Self {
            identity,
            capability: effect.requirement.capability.clone(),
            selector: effect.requirement.selector.clone(),
            output: effect.output.clone(),
            effect_kind,
            payload_bytes: canonical.len(),
            canonical_sha256: hex::encode(Sha256::digest(canonical)),
        })
    }

    pub(crate) fn observer_projection(&self) -> Self {
        let mut projected = self.clone();
        projected.canonical_sha256.clear();
        projected
    }

    pub(crate) fn tombstone_projection(&self) -> Self {
        let mut projected = self.clone();
        projected.selector = crate::vm::ResourceSelector::None;
        projected.output.clear();
        projected.effect_kind = "compacted".into();
        projected
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum EffectAuditTerminalOutcome {
    Acknowledged {
        response: crate::runtime::VmResumeResponse,
    },
    NotApplied {
        reason: String,
    },
    FailedPartial {
        detail: String,
    },
    AbandonedNotApplied,
    UncertainProcessLoss,
    /// Bounded durable replay fence replacing an older terminal payload.
    /// The digest covers the complete canonical typed terminal outcome.
    Compacted {
        outcome_kind: String,
        canonical_sha256: String,
    },
    /// Observer-only terminal projection. Canonical reducers never accept
    /// this value as a durable finish transition.
    Redacted { outcome_kind: String },
    /// Read-only projection of a schema-v14 `EffectRecorded` event. New
    /// writers never emit this variant.
    LegacyV14Snapshot {
        state: crate::vm::EffectJournalState,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EffectAuditState {
    IntentAccepted,
    AwaitingHostResult,
    Terminal { outcome: EffectAuditTerminalOutcome },
}

impl EffectAuditState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectAuditEntry {
    pub intent: EffectAuditIntent,
    pub authority: EffectAuditAuthority,
    pub state: EffectAuditState,
}

impl EffectAuditEntry {
    pub(crate) fn observer_projection(&self) -> Self {
        let mut projected = self.clone();
        projected.intent = projected.intent.observer_projection();
        projected.authority.authority_id = Uuid::nil();
        projected.authority.runner_lease_id = Uuid::nil();
        projected.authority.connection_id = None;
        if let EffectAuditState::Terminal { outcome } = &projected.state {
            projected.state = EffectAuditState::Terminal {
                outcome: observer_terminal_outcome(outcome),
            };
        }
        projected
    }
}

/// Proof that the daemon durably committed `AwaitingHostResult`. Host
/// bindings must require this value immediately before the physical effect.
/// It is intentionally not serializable and cannot be constructed outside
/// the crate's daemon-owned audit path.
#[derive(Debug)]
pub struct HostEffectPermit {
    identity: EffectAuditIdentity,
    authority_id: Uuid,
}

impl HostEffectPermit {
    pub(crate) fn new(identity: EffectAuditIdentity, authority_id: Uuid) -> Self {
        Self {
            identity,
            authority_id,
        }
    }

    pub fn identity(&self) -> EffectAuditIdentity {
        self.identity
    }

    pub(crate) fn authority_id(&self) -> Uuid {
        self.authority_id
    }
}

/// One canonical reducer input. Exact replays are no-ops; any changed
/// content, authority, or outcome under an existing identity fails closed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "transition", rename_all = "snake_case")]
pub enum EffectAuditTransition {
    Reserve {
        intent: EffectAuditIntent,
        authority: EffectAuditAuthority,
    },
    Begin {
        identity: EffectAuditIdentity,
        authority_id: Uuid,
    },
    Finish {
        identity: EffectAuditIdentity,
        authority_id: Uuid,
        outcome: EffectAuditTerminalOutcome,
    },
    /// Fixed-size canonical replay index record replacing a detailed terminal
    /// Reserve/Begin/Finish history. It carries no effect or authority payload.
    Fence {
        identity: EffectAuditIdentity,
        intent_sha256: String,
        intent_bytes: usize,
        authority_id: Uuid,
        authority_sha256: String,
        outcome_kind: String,
        outcome_sha256: String,
    },
}

impl EffectAuditTransition {
    pub fn identity(&self) -> EffectAuditIdentity {
        match self {
            Self::Reserve { intent, .. } => intent.identity,
            Self::Begin { identity, .. } | Self::Finish { identity, .. }
                | Self::Fence { identity, .. } => *identity,
        }
    }

    pub(crate) fn observer_projection(&self) -> Self {
        match self {
            Self::Reserve { intent, authority } => {
                let mut authority = authority.clone();
                authority.authority_id = Uuid::nil();
                authority.runner_lease_id = Uuid::nil();
                authority.connection_id = None;
                Self::Reserve { intent: intent.observer_projection(), authority }
            }
            Self::Begin { identity, .. } => Self::Begin {
                identity: *identity,
                authority_id: Uuid::nil(),
            },
            Self::Finish { identity, outcome, .. } => Self::Finish {
                identity: *identity,
                authority_id: Uuid::nil(),
                outcome: observer_terminal_outcome(outcome),
            },
            Self::Fence { identity, outcome_kind, .. } => Self::Fence {
                identity: *identity,
                intent_sha256: String::new(),
                intent_bytes: 0,
                authority_id: Uuid::nil(),
                authority_sha256: String::new(),
                outcome_kind: outcome_kind.clone(),
                outcome_sha256: String::new(),
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EffectAuditReducer {
    entries: BTreeMap<EffectAuditIdentity, EffectAuditEntry>,
    active_by_run: HashMap<Uuid, usize>,
    active_count: usize,
    active_bytes: usize,
    total_count: usize,
    replay_fence_count: usize,
    detailed_terminal_order: VecDeque<EffectAuditIdentity>,
}

impl EffectAuditReducer {
    pub fn entries(&self) -> &BTreeMap<EffectAuditIdentity, EffectAuditEntry> {
        &self.entries
    }

    pub fn get(&self, identity: &EffectAuditIdentity) -> Option<&EffectAuditEntry> {
        self.entries.get(identity)
    }

    pub fn active_for_run(&self, run_id: Uuid) -> usize {
        self.active_by_run.get(&run_id).copied().unwrap_or(0)
    }

    pub fn active_count(&self) -> usize {
        self.active_count
    }

    pub fn active_bytes(&self) -> usize {
        self.active_bytes
    }

    pub fn total_count(&self) -> usize {
        self.total_count
    }

    pub fn replay_fence_count(&self) -> usize {
        self.replay_fence_count
    }

    /// Oldest detailed terminal identities beyond the retained observer tail.
    /// This queue is maintained as transitions apply, avoiding a scan and sort
    /// of the complete Brain event history during compaction.
    pub(crate) fn terminal_compaction_candidates(
        &self,
        retained_limit: usize,
    ) -> Vec<EffectAuditIdentity> {
        self.detailed_terminal_order
            .iter()
            .take(self.detailed_terminal_order.len().saturating_sub(retained_limit))
            .copied()
            .collect()
    }

    /// Validate one transition without cloning the complete replay index.
    /// The preview contains only the targeted entry plus copied scalar/index
    /// counters, so persistence callers remain O(log N) in mature Brains.
    pub fn validate(&self, transition: &EffectAuditTransition) -> Result<bool> {
        let identity = transition.identity();
        let mut entries = BTreeMap::new();
        if let Some(entry) = self.entries.get(&identity) {
            entries.insert(identity, entry.clone());
        }
        let mut active_by_run = HashMap::new();
        if let Some(active) = self.active_by_run.get(&identity.run_id) {
            active_by_run.insert(identity.run_id, *active);
        }
        let mut preview = Self {
            entries,
            active_by_run,
            active_count: self.active_count,
            active_bytes: self.active_bytes,
            total_count: self.total_count,
            replay_fence_count: self.replay_fence_count,
            detailed_terminal_order: VecDeque::new(),
        };
        preview.apply(transition.clone())
    }

    fn mark_terminal(&mut self, run_id: Uuid, payload_bytes: usize) {
        if let Some(active) = self.active_by_run.get_mut(&run_id) {
            *active = active.saturating_sub(1);
            if *active == 0 {
                self.active_by_run.remove(&run_id);
            }
        }
        self.active_count = self.active_count.saturating_sub(1);
        self.active_bytes = self.active_bytes.saturating_sub(payload_bytes);
    }

    /// Replace a detailed terminal projection with its permanent fixed-size
    /// intent/outcome digest fence. Unresolved write-ahead state is never
    /// eligible for retention compaction.
    pub(crate) fn compact_terminal(&mut self, identity: &EffectAuditIdentity) -> Result<()> {
        let entry = self
            .entries
            .get_mut(identity)
            .context("effect audit does not exist")?;
        let EffectAuditState::Terminal { outcome } = &entry.state else {
            bail!("unresolved effect audit cannot be compacted");
        };
        if matches!(outcome, EffectAuditTerminalOutcome::Compacted { .. }) {
            return Ok(());
        }
        let canonical_sha256 = terminal_outcome_sha256(outcome)?;
        let outcome_kind = terminal_outcome_kind(outcome).to_string();
        entry.state = EffectAuditState::Terminal {
            outcome: EffectAuditTerminalOutcome::Compacted {
                outcome_kind,
                canonical_sha256,
            },
        };
        entry.intent = entry.intent.tombstone_projection();
        if self.detailed_terminal_order.front() == Some(identity) {
            self.detailed_terminal_order.pop_front();
        } else {
            self.detailed_terminal_order.retain(|candidate| candidate != identity);
        }
        self.replay_fence_count += 1;
        Ok(())
    }

    /// Validate and apply one monotonic transition. Returns false for an
    /// exact retry and true only when the state changed.
    pub fn apply(&mut self, transition: EffectAuditTransition) -> Result<bool> {
        let identity = transition.identity();
        match transition {
            EffectAuditTransition::Reserve { intent, authority } => {
                anyhow::ensure!(intent.identity == identity, "effect audit identity changed");
                anyhow::ensure!(
                    intent.payload_bytes <= MAX_EFFECT_AUDIT_INTENT_BYTES,
                    "effect audit intent exceeds the bounded payload limit"
                );
                anyhow::ensure!(
                    intent.payload_bytes > 0
                        && intent.canonical_sha256.len() == 64
                        && intent
                            .canonical_sha256
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit()),
                    "effect audit intent digest metadata is invalid"
                );
                if let Some(existing) = self.entries.get(&identity) {
                    let compacted = matches!(existing.state,
                        EffectAuditState::Terminal {
                            outcome: EffectAuditTerminalOutcome::Compacted { .. }
                        });
                    let exact_intent = if compacted {
                        existing.intent.canonical_sha256 == intent.canonical_sha256
                            && existing.intent.payload_bytes == intent.payload_bytes
                    } else {
                        existing.intent == intent
                    };
                    let exact_authority = if compacted
                            && existing.authority.runner_lease_id == Uuid::nil()
                            && existing.authority.environment_generation == 0 {
                        existing.authority.authority_id == authority.authority_id
                            && existing.authority.runner_subject == authority_sha256(&authority)?
                    } else {
                        existing.authority == authority
                    };
                    anyhow::ensure!(exact_intent && exact_authority,
                        "conflicting effect audit reservation for {identity:?}");
                    return Ok(false);
                }
                anyhow::ensure!(self.total_count < MAX_EFFECT_AUDIT_REPLAY_FENCES_PER_BRAIN,
                    "effect audit replay index exhausted; archive this Brain before admitting more host effects");
                anyhow::ensure!(
                    self.active_for_run(identity.run_id) < MAX_ACTIVE_EFFECT_AUDITS_PER_RUN,
                    "effect audit active-intent quota exceeded for run {}",
                    identity.run_id
                );
                anyhow::ensure!(
                    self.active_count() < MAX_ACTIVE_EFFECT_AUDITS_PER_BRAIN,
                    "effect audit Brain-wide active-intent quota exceeded"
                );
                anyhow::ensure!(
                    self.active_bytes().saturating_add(intent.payload_bytes)
                        <= MAX_ACTIVE_EFFECT_AUDIT_BYTES_PER_BRAIN,
                    "effect audit Brain-wide active-byte quota exceeded"
                );
                let payload_bytes = intent.payload_bytes;
                self.entries.insert(
                    identity,
                    EffectAuditEntry {
                        intent,
                        authority,
                        state: EffectAuditState::IntentAccepted,
                    },
                );
                *self.active_by_run.entry(identity.run_id).or_insert(0) += 1;
                self.active_count += 1;
                self.active_bytes += payload_bytes;
                self.total_count += 1;
                Ok(true)
            }
            EffectAuditTransition::Begin {
                identity,
                authority_id,
            } => {
                let entry = self
                    .entries
                    .get_mut(&identity)
                    .context("cannot begin a host effect without a durable audit reservation")?;
                anyhow::ensure!(
                    entry.authority.authority_id == authority_id,
                    "effect audit authority mismatch"
                );
                match entry.state {
                    EffectAuditState::IntentAccepted => {
                        entry.state = EffectAuditState::AwaitingHostResult;
                        Ok(true)
                    }
                    EffectAuditState::AwaitingHostResult => Ok(false),
                    EffectAuditState::Terminal { .. } => {
                        bail!("cannot begin a terminal effect audit")
                    }
                }
            }
            EffectAuditTransition::Finish {
                identity,
                authority_id,
                outcome,
            } => {
                anyhow::ensure!(
                    serde_json::to_vec(&outcome)?.len() <= MAX_EFFECT_AUDIT_OUTCOME_BYTES,
                    "effect audit outcome exceeds the bounded payload limit"
                );
                let payload_bytes = {
                    let entry = self
                        .entries
                        .get_mut(&identity)
                        .context("cannot finish a host effect without a durable audit reservation")?;
                    anyhow::ensure!(
                        entry.authority.authority_id == authority_id,
                        "effect audit authority mismatch"
                    );
                    match &entry.state {
                    EffectAuditState::IntentAccepted => {
                        anyhow::ensure!(
                            matches!(
                                outcome,
                                EffectAuditTerminalOutcome::NotApplied { .. }
                                    | EffectAuditTerminalOutcome::AbandonedNotApplied
                                    | EffectAuditTerminalOutcome::Compacted { .. }
                                    | EffectAuditTerminalOutcome::LegacyV14Snapshot { .. }
                            ),
                            "a host outcome cannot be recorded before durable begin"
                        );
                        entry.state = EffectAuditState::Terminal { outcome };
                        entry.intent.payload_bytes
                    }
                    EffectAuditState::AwaitingHostResult => {
                        anyhow::ensure!(!matches!(outcome,
                                EffectAuditTerminalOutcome::Redacted { .. }),
                            "observer-redacted outcome cannot enter the canonical audit log");
                        entry.state = EffectAuditState::Terminal { outcome };
                        entry.intent.payload_bytes
                    }
                    EffectAuditState::Terminal { outcome: existing } => {
                        let matches = match existing {
                            EffectAuditTerminalOutcome::Compacted {
                                canonical_sha256, ..
                            } => canonical_sha256 == &terminal_outcome_sha256(&outcome)?,
                            _ => existing == &outcome,
                        };
                        anyhow::ensure!(matches, "conflicting terminal effect audit outcome");
                        return Ok(false);
                    }
                }};
                self.mark_terminal(identity.run_id, payload_bytes);
                if !matches!(self.entries.get(&identity).map(|entry| &entry.state),
                        Some(EffectAuditState::Terminal {
                            outcome: EffectAuditTerminalOutcome::Compacted { .. }
                        })) {
                    self.detailed_terminal_order.push_back(identity);
                }
                Ok(true)
            }
            EffectAuditTransition::Fence {
                identity,
                intent_sha256,
                intent_bytes,
                authority_id,
                authority_sha256,
                outcome_kind,
                outcome_sha256,
            } => {
                for digest in [&intent_sha256, &authority_sha256, &outcome_sha256] {
                    anyhow::ensure!(digest.len() == 64
                            && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                        "effect audit replay fence digest metadata is invalid");
                }
                anyhow::ensure!(intent_bytes > 0 && intent_bytes <= MAX_EFFECT_AUDIT_INTENT_BYTES,
                    "effect audit replay fence intent size is invalid");
                anyhow::ensure!(!outcome_kind.is_empty() && outcome_kind.len() <= 64,
                    "effect audit replay fence outcome kind is invalid");
                if let Some(existing) = self.entries.get(&identity) {
                    let exact = existing.intent.canonical_sha256 == intent_sha256
                        && existing.intent.payload_bytes == intent_bytes
                        && existing.authority.authority_id == authority_id
                        && existing.authority.runner_subject == authority_sha256
                        && matches!(&existing.state, EffectAuditState::Terminal {
                            outcome: EffectAuditTerminalOutcome::Compacted {
                                outcome_kind: existing_kind,
                                canonical_sha256: existing_sha256,
                            }
                        } if existing_kind == &outcome_kind
                            && existing_sha256 == &outcome_sha256);
                    anyhow::ensure!(exact,
                        "effect audit replay fence conflicts with existing state");
                    return Ok(false);
                }
                anyhow::ensure!(self.total_count < MAX_EFFECT_AUDIT_REPLAY_FENCES_PER_BRAIN,
                    "effect audit replay index exhausted; archive this Brain before admitting more host effects");
                self.entries.insert(identity, EffectAuditEntry {
                    intent: EffectAuditIntent {
                        identity,
                        capability: crate::vm::CapabilityKind::SessionEmit,
                        selector: crate::vm::ResourceSelector::None,
                        output: Vec::new(),
                        effect_kind: "compacted".into(),
                        payload_bytes: intent_bytes,
                        canonical_sha256: intent_sha256,
                    },
                    authority: EffectAuditAuthority {
                        authority_id,
                        runner_lease_id: Uuid::nil(),
                        runner_subject: authority_sha256,
                        connection_id: None,
                        environment_generation: 0,
                    },
                    state: EffectAuditState::Terminal {
                        outcome: EffectAuditTerminalOutcome::Compacted {
                            outcome_kind,
                            canonical_sha256: outcome_sha256,
                        },
                    },
                });
                self.total_count += 1;
                self.replay_fence_count += 1;
                Ok(true)
            }
        }
    }
}

pub(crate) fn authority_sha256(authority: &EffectAuditAuthority) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(authority)?)))
}

pub(crate) fn replay_fence_transition(
    entry: &EffectAuditEntry,
) -> Result<EffectAuditTransition> {
    let EffectAuditState::Terminal { outcome } = &entry.state else {
        bail!("unresolved effect audit cannot become a replay fence");
    };
    let transition = EffectAuditTransition::Fence {
        identity: entry.intent.identity,
        intent_sha256: entry.intent.canonical_sha256.clone(),
        intent_bytes: entry.intent.payload_bytes,
        authority_id: entry.authority.authority_id,
        authority_sha256: authority_sha256(&entry.authority)?,
        outcome_kind: terminal_outcome_kind(outcome),
        outcome_sha256: terminal_outcome_sha256(outcome)?,
    };
    anyhow::ensure!(serde_json::to_vec(&transition)?.len()
            <= MAX_EFFECT_AUDIT_REPLAY_FENCE_TRANSITION_BYTES,
        "effect audit replay fence exceeds its fixed encoded bound");
    Ok(transition)
}

fn terminal_outcome_sha256(outcome: &EffectAuditTerminalOutcome) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(outcome)?)))
}

fn terminal_outcome_kind(outcome: &EffectAuditTerminalOutcome) -> String {
    match outcome {
        EffectAuditTerminalOutcome::Acknowledged { .. } => "acknowledged".into(),
        EffectAuditTerminalOutcome::NotApplied { .. } => "not_applied".into(),
        EffectAuditTerminalOutcome::FailedPartial { .. } => "failed_partial".into(),
        EffectAuditTerminalOutcome::AbandonedNotApplied => "abandoned_not_applied".into(),
        EffectAuditTerminalOutcome::UncertainProcessLoss => "uncertain_process_loss".into(),
        EffectAuditTerminalOutcome::Compacted { outcome_kind, .. } => outcome_kind.clone(),
        EffectAuditTerminalOutcome::Redacted { outcome_kind } => outcome_kind.clone(),
        EffectAuditTerminalOutcome::LegacyV14Snapshot { .. } => "legacy_v14_snapshot".into(),
    }
}

fn observer_terminal_outcome(outcome: &EffectAuditTerminalOutcome) -> EffectAuditTerminalOutcome {
    match outcome {
        EffectAuditTerminalOutcome::AbandonedNotApplied =>
            EffectAuditTerminalOutcome::AbandonedNotApplied,
        EffectAuditTerminalOutcome::UncertainProcessLoss =>
            EffectAuditTerminalOutcome::UncertainProcessLoss,
        EffectAuditTerminalOutcome::Compacted { outcome_kind, .. } =>
            EffectAuditTerminalOutcome::Compacted {
                outcome_kind: outcome_kind.clone(),
                canonical_sha256: String::new(),
            },
        other => EffectAuditTerminalOutcome::Redacted {
            outcome_kind: terminal_outcome_kind(other),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum EffectLogRecord {
    Effect {
        envelope: VmEffectEnvelope,
    },
    Acknowledged {
        consumer: String,
        execution_id: Uuid,
        through_sequence: u64,
    },
}

/// Append-only effect delivery state owned by an embedding application. VM
/// execution remains valid if a client disconnects; a later client can replay
/// every unacknowledged correlated event from this log.
pub struct VmEffectDeliveryLog {
    path: PathBuf,
    writer: File,
    effects: BTreeMap<(Uuid, u64), VmEffectEnvelope>,
    acknowledgements: HashMap<(String, Uuid), u64>,
}

impl VmEffectDeliveryLog {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("create VM effect-log directory '{}'", parent.display())
            })?;
        }
        let mut effects = BTreeMap::new();
        let mut acknowledgements = HashMap::new();
        if path.exists() {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read VM effect log '{}'", path.display()))?;
            let records = bytes
                .split_inclusive(|byte| *byte == b'\n')
                .collect::<Vec<_>>();
            let mut committed_len = 0usize;
            for (index, terminated) in records.iter().enumerate() {
                if terminated.last() != Some(&b'\n') {
                    break;
                }
                let line = std::str::from_utf8(&terminated[..terminated.len() - 1]).with_context(
                    || {
                        format!(
                            "decode VM effect log '{}' line {}",
                            path.display(),
                            index + 1
                        )
                    },
                )?;
                if line.trim().is_empty() {
                    committed_len += terminated.len();
                    continue;
                }
                let record: EffectLogRecord = serde_json::from_str(&line).with_context(|| {
                    format!(
                        "decode VM effect log '{}' line {}",
                        path.display(),
                        index + 1
                    )
                })?;
                apply_record(&mut effects, &mut acknowledgements, record)?;
                committed_len += terminated.len();
            }
            if committed_len != bytes.len() {
                let file = OpenOptions::new().write(true).open(&path)?;
                file.set_len(committed_len as u64)?;
                file.sync_all()?;
            }
        }
        let new_file = !path.exists();
        let writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open VM effect log writer '{}'", path.display()))?;
        if new_file {
            if let Some(parent) = path.parent() {
                File::open(parent)?.sync_all()?;
            }
        }
        Ok(Self {
            path,
            writer,
            effects,
            acknowledgements,
        })
    }

    /// Persist one event before projecting it. Exact replay is idempotent;
    /// conflicting content under the same `(execution_id, sequence)` is a
    /// protocol violation.
    pub fn append(&mut self, envelope: VmEffectEnvelope) -> Result<bool> {
        let key = (envelope.execution_id, envelope.effect.sequence);
        if let Some(existing) = self.effects.get(&key) {
            if existing == &envelope {
                return Ok(false);
            }
            bail!(
                "conflicting VM effect replay for run {} sequence {}",
                key.0,
                key.1
            );
        }
        self.persist(&EffectLogRecord::Effect {
            envelope: envelope.clone(),
        })?;
        self.effects.insert(key, envelope);
        Ok(true)
    }

    /// Record that one consumer durably projected a contiguous prefix.
    pub fn acknowledge(
        &mut self,
        consumer: impl Into<String>,
        execution_id: Uuid,
        through_sequence: u64,
    ) -> Result<bool> {
        let consumer = consumer.into();
        let key = (consumer.clone(), execution_id);
        if self
            .acknowledgements
            .get(&key)
            .is_some_and(|current| *current >= through_sequence)
        {
            return Ok(false);
        }
        for sequence in 0..=through_sequence {
            if !self.effects.contains_key(&(execution_id, sequence)) {
                bail!(
                    "cannot acknowledge VM run {execution_id} through sequence {through_sequence}: sequence {sequence} is missing"
                );
            }
        }
        self.persist(&EffectLogRecord::Acknowledged {
            consumer: consumer.clone(),
            execution_id,
            through_sequence,
        })?;
        self.acknowledgements.insert(key, through_sequence);
        Ok(true)
    }

    pub fn pending(&self, consumer: &str) -> Vec<VmEffectEnvelope> {
        self.effects
            .values()
            .filter(|envelope| {
                self.acknowledgements
                    .get(&(consumer.to_string(), envelope.execution_id))
                    .is_none_or(|sequence| envelope.effect.sequence > *sequence)
            })
            .cloned()
            .collect()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn persist(&mut self, record: &EffectLogRecord) -> Result<()> {
        serde_json::to_writer(&mut self.writer, record)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        self.writer.sync_data()?;
        Ok(())
    }
}

fn apply_record(
    effects: &mut BTreeMap<(Uuid, u64), VmEffectEnvelope>,
    acknowledgements: &mut HashMap<(String, Uuid), u64>,
    record: EffectLogRecord,
) -> Result<()> {
    match record {
        EffectLogRecord::Effect { envelope } => {
            let key = (envelope.execution_id, envelope.effect.sequence);
            if let Some(existing) = effects.get(&key) {
                if existing != &envelope {
                    bail!(
                        "conflicting VM effect records for run {} sequence {}",
                        key.0,
                        key.1
                    );
                }
            } else {
                effects.insert(key, envelope);
            }
        }
        EffectLogRecord::Acknowledged {
            consumer,
            execution_id,
            through_sequence,
        } => {
            for sequence in 0..=through_sequence {
                if !effects.contains_key(&(execution_id, sequence)) {
                    bail!(
                        "VM effect acknowledgement for run {execution_id} crosses missing sequence {sequence}"
                    );
                }
            }
            acknowledgements
                .entry((consumer, execution_id))
                .and_modify(|current| *current = (*current).max(through_sequence))
                .or_insert(through_sequence);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{
        CapabilityKind, CapabilityRequirement, HostSideEffect, ResourceSelector, SourceOrigin,
        VmSideEffect,
    };

    fn effect(execution_id: Uuid, sequence: u64, text: &str) -> VmEffectEnvelope {
        VmEffectEnvelope {
            execution_id,
            effect: VmSideEffect {
                protocol_version: 1,
                sequence,
                requirement: CapabilityRequirement {
                    capability: CapabilityKind::SessionEmit,
                    selector: ResourceSelector::None,
                },
                output: Vec::new(),
                event: HostSideEffect::Emit { text: text.into() },
                origin: SourceOrigin::generated("effect-log-test"),
            },
        }
    }

    fn audit_fixture() -> (EffectAuditIntent, EffectAuditAuthority) {
        let identity = EffectAuditIdentity {
            brain_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            request_seq: 9,
            execution_id: Uuid::new_v4(),
            effect_sequence: 2,
        };
        (
            EffectAuditIntent::from_effect(
                identity,
                &effect(identity.execution_id, identity.effect_sequence, "audit").effect,
            )
            .unwrap(),
            EffectAuditAuthority {
                authority_id: Uuid::new_v4(),
                runner_lease_id: Uuid::new_v4(),
                runner_subject: "runner".into(),
                connection_id: Some(Uuid::new_v4()),
                environment_generation: 4,
            },
        )
    }

    #[test]
    fn audit_reducer_replays_exact_transitions_and_rejects_changed_content() {
        let (intent, authority) = audit_fixture();
        let reserve = EffectAuditTransition::Reserve {
            intent: intent.clone(),
            authority: authority.clone(),
        };
        let mut reducer = EffectAuditReducer::default();
        assert!(reducer.apply(reserve.clone()).unwrap());
        assert!(!reducer.apply(reserve).unwrap());

        let mut changed = intent.clone();
        changed.canonical_sha256 = "f".repeat(64);
        assert!(reducer
            .apply(EffectAuditTransition::Reserve {
                intent: changed,
                authority: authority.clone(),
            })
            .unwrap_err()
            .to_string()
            .contains("conflicting"));

        let begin = EffectAuditTransition::Begin {
            identity: intent.identity,
            authority_id: authority.authority_id,
        };
        assert!(reducer.apply(begin.clone()).unwrap());
        assert!(!reducer.apply(begin).unwrap());
        assert!(reducer
            .apply(EffectAuditTransition::Begin {
                identity: intent.identity,
                authority_id: Uuid::new_v4(),
            })
            .unwrap_err()
            .to_string()
            .contains("authority mismatch"));

        let finish = EffectAuditTransition::Finish {
            identity: intent.identity,
            authority_id: authority.authority_id,
            outcome: EffectAuditTerminalOutcome::Acknowledged {
                response: crate::runtime::VmResumeResponse::Result { values: Vec::new() },
            },
        };
        assert!(reducer.apply(finish.clone()).unwrap());
        assert!(!reducer.apply(finish).unwrap());
        assert!(reducer
            .apply(EffectAuditTransition::Finish {
                identity: intent.identity,
                authority_id: authority.authority_id,
                outcome: EffectAuditTerminalOutcome::FailedPartial {
                    detail: "changed".into(),
                },
            })
            .unwrap_err()
            .to_string()
            .contains("conflicting terminal"));
    }

    #[test]
    fn audit_reducer_never_acknowledges_before_durable_begin() {
        let (intent, authority) = audit_fixture();
        let mut reducer = EffectAuditReducer::default();
        reducer
            .apply(EffectAuditTransition::Reserve {
                intent: intent.clone(),
                authority: authority.clone(),
            })
            .unwrap();
        assert!(reducer
            .apply(EffectAuditTransition::Finish {
                identity: intent.identity,
                authority_id: authority.authority_id,
                outcome: EffectAuditTerminalOutcome::Acknowledged {
                    response: crate::runtime::VmResumeResponse::Result { values: Vec::new() },
                },
            })
            .unwrap_err()
            .to_string()
            .contains("before durable begin"));
        assert!(reducer
            .apply(EffectAuditTransition::Finish {
                identity: intent.identity,
                authority_id: authority.authority_id,
                outcome: EffectAuditTerminalOutcome::AbandonedNotApplied,
            })
            .unwrap());
    }

    #[test]
    fn audit_reducer_bounds_active_intents_before_any_permit() {
        let (template, authority) = audit_fixture();
        let mut reducer = EffectAuditReducer::default();
        for sequence in 0..MAX_ACTIVE_EFFECT_AUDITS_PER_RUN {
            let mut intent = template.clone();
            intent.identity.execution_id = Uuid::new_v4();
            intent.identity.effect_sequence = sequence as u64;
            intent.canonical_sha256 = format!("{sequence:064x}");
            reducer
                .apply(EffectAuditTransition::Reserve {
                    intent,
                    authority: authority.clone(),
                })
                .unwrap();
        }
        let mut overflow = template;
        overflow.identity.execution_id = Uuid::new_v4();
        overflow.identity.effect_sequence = 100;
        overflow.canonical_sha256 = format!("{:064x}", 100);
        let error = reducer
            .apply(EffectAuditTransition::Reserve {
                intent: overflow,
                authority,
            })
            .unwrap_err();
        assert!(error.to_string().contains("quota exceeded"));
    }

    #[test]
    fn audit_reducer_enforces_brain_wide_count_and_byte_admission() {
        let (template, authority) = audit_fixture();
        let mut count_limited = EffectAuditReducer::default();
        for index in 0..MAX_ACTIVE_EFFECT_AUDITS_PER_BRAIN {
            let mut intent = template.clone();
            intent.identity.run_id = Uuid::new_v4();
            intent.identity.execution_id = Uuid::new_v4();
            intent.canonical_sha256 = format!("{index:064x}");
            intent.payload_bytes = 1;
            count_limited
                .apply(EffectAuditTransition::Reserve {
                    intent,
                    authority: authority.clone(),
                })
                .unwrap();
        }
        let mut count_overflow = template.clone();
        count_overflow.identity.run_id = Uuid::new_v4();
        count_overflow.identity.execution_id = Uuid::new_v4();
        assert!(count_limited
            .apply(EffectAuditTransition::Reserve {
                intent: count_overflow,
                authority: authority.clone(),
            })
            .unwrap_err()
            .to_string()
            .contains("Brain-wide active-intent"));

        let mut byte_limited = EffectAuditReducer::default();
        let admitted = MAX_ACTIVE_EFFECT_AUDIT_BYTES_PER_BRAIN / MAX_EFFECT_AUDIT_INTENT_BYTES;
        for index in 0..admitted {
            let mut intent = template.clone();
            intent.identity.run_id = Uuid::new_v4();
            intent.identity.execution_id = Uuid::new_v4();
            intent.canonical_sha256 = format!("{index:064x}");
            intent.payload_bytes = MAX_EFFECT_AUDIT_INTENT_BYTES;
            byte_limited
                .apply(EffectAuditTransition::Reserve {
                    intent,
                    authority: authority.clone(),
                })
                .unwrap();
        }
        let mut byte_overflow = template;
        byte_overflow.identity.run_id = Uuid::new_v4();
        byte_overflow.identity.execution_id = Uuid::new_v4();
        byte_overflow.payload_bytes = 1;
        assert!(byte_limited
            .apply(EffectAuditTransition::Reserve {
                intent: byte_overflow,
                authority,
            })
            .unwrap_err()
            .to_string()
            .contains("Brain-wide active-byte"));
    }

    #[test]
    fn audit_reducer_supports_thousands_then_fails_closed_at_replay_index_exhaustion() {
        let (template, authority) = audit_fixture();
        let mut reducer = EffectAuditReducer::default();
        let mut first_reserve = None;
        for index in 0..1_024 {
            let mut intent = template.clone();
            intent.identity.run_id = Uuid::new_v4();
            intent.identity.execution_id = Uuid::new_v4();
            intent.identity.effect_sequence = index as u64;
            intent.canonical_sha256 = format!("{index:064x}");
            let reserve = EffectAuditTransition::Reserve {
                intent: intent.clone(),
                authority: authority.clone(),
            };
            if first_reserve.is_none() {
                first_reserve = Some(reserve.clone());
            }
            assert!(reducer.apply(reserve).unwrap());
            assert!(reducer.apply(EffectAuditTransition::Finish {
                identity: intent.identity,
                authority_id: authority.authority_id,
                outcome: EffectAuditTerminalOutcome::AbandonedNotApplied,
            }).unwrap());
            reducer.compact_terminal(&intent.identity).unwrap();
        }

        assert!(!reducer.apply(first_reserve.unwrap()).unwrap(),
            "an exact old identity remains permanently fenced as a no-op");

        let mut exhausted = EffectAuditReducer::default();
        let mut first_fence = None;
        for index in 0..MAX_EFFECT_AUDIT_REPLAY_FENCES_PER_BRAIN {
            let fence = EffectAuditTransition::Fence {
                identity: EffectAuditIdentity {
                    brain_id: template.identity.brain_id,
                    run_id: template.identity.run_id,
                    request_seq: template.identity.request_seq,
                    execution_id: Uuid::from_u128(index as u128 + 1),
                    effect_sequence: index as u64,
                },
                intent_sha256: format!("{index:064x}"),
                intent_bytes: 1,
                authority_id: authority.authority_id,
                authority_sha256: "a".repeat(64),
                outcome_kind: "abandoned_not_applied".into(),
                outcome_sha256: "b".repeat(64),
            };
            if first_fence.is_none() {
                first_fence = Some(fence.clone());
            }
            assert!(exhausted.apply(fence).unwrap());
        }
        assert!(!exhausted.apply(first_fence.unwrap()).unwrap(),
            "an exact replay fence is idempotent even at capacity");
        let error = exhausted.apply(EffectAuditTransition::Fence {
            identity: EffectAuditIdentity {
                execution_id: Uuid::new_v4(),
                ..template.identity
            },
            intent_sha256: "c".repeat(64),
            intent_bytes: 1,
            authority_id: authority.authority_id,
            authority_sha256: "a".repeat(64),
            outcome_kind: "not_applied".into(),
            outcome_sha256: "d".repeat(64),
        }).unwrap_err();
        assert!(error.to_string().contains("archive this Brain"));
        assert!(MAX_EFFECT_AUDIT_REPLAY_FENCES_PER_BRAIN > 512);
        assert!(MAX_EFFECT_AUDIT_REPLAY_FENCES_PER_BRAIN
                * MAX_EFFECT_AUDIT_REPLAY_FENCE_EVENT_BYTES
            < MAX_EFFECT_AUDIT_JOURNAL_BYTES_PER_BRAIN);
    }

    #[test]
    fn reopens_and_replays_only_each_consumers_unacknowledged_suffix() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("effects.jsonl");
        let execution_id = Uuid::new_v4();
        {
            let mut log = VmEffectDeliveryLog::open(&path).unwrap();
            assert!(log.append(effect(execution_id, 0, "one")).unwrap());
            assert!(!log.append(effect(execution_id, 0, "one")).unwrap());
            assert!(log.append(effect(execution_id, 1, "two")).unwrap());
            assert!(log.acknowledge("client-a", execution_id, 0).unwrap());
        }

        let log = VmEffectDeliveryLog::open(&path).unwrap();
        assert_eq!(
            log.pending("client-a"),
            vec![effect(execution_id, 1, "two")]
        );
        assert_eq!(
            log.pending("client-b"),
            vec![
                effect(execution_id, 0, "one"),
                effect(execution_id, 1, "two")
            ]
        );
    }

    #[test]
    fn rejects_acknowledging_across_a_delivery_gap() {
        let directory = tempfile::tempdir().unwrap();
        let mut log = VmEffectDeliveryLog::open(directory.path().join("effects.jsonl")).unwrap();
        let execution_id = Uuid::new_v4();
        log.append(effect(execution_id, 1, "late")).unwrap();
        let error = log
            .acknowledge("client", execution_id, 1)
            .expect_err("a missing prefix cannot be acknowledged");
        assert!(error.to_string().contains("sequence 0 is missing"));
    }

    #[test]
    fn reports_the_exact_corrupt_jsonl_line() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("effects.jsonl");
        std::fs::write(&path, "\nnot-json\n").unwrap();
        let error = VmEffectDeliveryLog::open(&path)
            .err()
            .expect("corrupt log must fail closed");
        assert!(error.to_string().contains("line 2"));
    }

    #[test]
    fn recovers_a_torn_final_delivery_record_but_rejects_nonfinal_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("effects.jsonl");
        let execution_id = Uuid::new_v4();
        {
            let mut log = VmEffectDeliveryLog::open(&path).unwrap();
            log.append(effect(execution_id, 0, "one")).unwrap();
        }
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(br#"{"record":"effect""#)
            .unwrap();
        let reopened = VmEffectDeliveryLog::open(&path).unwrap();
        assert_eq!(reopened.pending("client").len(), 1);
        assert_eq!(std::fs::read(&path).unwrap().last(), Some(&b'\n'));

        std::fs::write(&path, b"not-json\n{}\n").unwrap();
        let error = VmEffectDeliveryLog::open(&path).err().unwrap();
        assert!(error.to_string().contains("line 1"));
    }
}

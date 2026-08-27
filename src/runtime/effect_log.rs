//! Durable application-side delivery log for portable VM effects.

use crate::runtime::VmEffectEnvelope;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
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
    pub effect: crate::vm::VmSideEffect,
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
}

impl EffectAuditTransition {
    pub fn identity(&self) -> EffectAuditIdentity {
        match self {
            Self::Reserve { intent, .. } => intent.identity,
            Self::Begin { identity, .. } | Self::Finish { identity, .. } => *identity,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EffectAuditReducer {
    entries: BTreeMap<EffectAuditIdentity, EffectAuditEntry>,
}

impl EffectAuditReducer {
    pub fn entries(&self) -> &BTreeMap<EffectAuditIdentity, EffectAuditEntry> {
        &self.entries
    }

    pub fn get(&self, identity: &EffectAuditIdentity) -> Option<&EffectAuditEntry> {
        self.entries.get(identity)
    }

    pub fn active_for_run(&self, run_id: Uuid) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.intent.identity.run_id == run_id && !entry.state.is_terminal())
            .count()
    }

    /// Forget a terminal projection after its complete transition history has
    /// been durably removed from the canonical Brain journal. Unresolved
    /// write-ahead state is never eligible for retention compaction.
    pub(crate) fn remove_terminal(&mut self, identity: &EffectAuditIdentity) -> Result<()> {
        let entry = self.entries.get(identity).context("effect audit does not exist")?;
        anyhow::ensure!(entry.state.is_terminal(), "unresolved effect audit cannot be compacted");
        self.entries.remove(identity);
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
                    serde_json::to_vec(&intent)?.len() <= MAX_EFFECT_AUDIT_INTENT_BYTES,
                    "effect audit intent exceeds the bounded payload limit"
                );
                if let Some(existing) = self.entries.get(&identity) {
                    anyhow::ensure!(
                        existing.intent == intent && existing.authority == authority,
                        "conflicting effect audit reservation for {identity:?}"
                    );
                    return Ok(false);
                }
                anyhow::ensure!(
                    self.active_for_run(identity.run_id) < MAX_ACTIVE_EFFECT_AUDITS_PER_RUN,
                    "effect audit active-intent quota exceeded for run {}",
                    identity.run_id
                );
                self.entries.insert(
                    identity,
                    EffectAuditEntry {
                        intent,
                        authority,
                        state: EffectAuditState::IntentAccepted,
                    },
                );
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
                                    | EffectAuditTerminalOutcome::LegacyV14Snapshot { .. }
                            ),
                            "a host outcome cannot be recorded before durable begin"
                        );
                        entry.state = EffectAuditState::Terminal { outcome };
                        Ok(true)
                    }
                    EffectAuditState::AwaitingHostResult => {
                        entry.state = EffectAuditState::Terminal { outcome };
                        Ok(true)
                    }
                    EffectAuditState::Terminal { outcome: existing } => {
                        anyhow::ensure!(
                            existing == &outcome,
                            "conflicting terminal effect audit outcome"
                        );
                        Ok(false)
                    }
                }
            }
        }
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
            EffectAuditIntent {
                identity,
                effect: effect(identity.execution_id, identity.effect_sequence, "audit").effect,
            },
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
        changed.effect.origin = SourceOrigin::generated("forged");
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
            intent.effect.sequence = sequence as u64;
            reducer.apply(EffectAuditTransition::Reserve {
                intent,
                authority: authority.clone(),
            }).unwrap();
        }
        let mut overflow = template;
        overflow.identity.execution_id = Uuid::new_v4();
        overflow.identity.effect_sequence = 100;
        overflow.effect.sequence = 100;
        let error = reducer.apply(EffectAuditTransition::Reserve {
            intent: overflow,
            authority,
        }).unwrap_err();
        assert!(error.to_string().contains("quota exceeded"));
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

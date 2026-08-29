//! Durable application-side delivery log for portable VM effects.

use crate::runtime::VmEffectEnvelope;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

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
            let reader = BufReader::new(
                File::open(&path)
                    .with_context(|| format!("open VM effect log '{}'", path.display()))?,
            );
            for (index, line) in reader.lines().enumerate() {
                let line = line.with_context(|| {
                    format!("read VM effect log '{}' line {}", path.display(), index + 1)
                })?;
                if line.trim().is_empty() {
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
            }
        }
        let writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open VM effect log writer '{}'", path.display()))?;
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
}

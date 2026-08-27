//! Indexed, durable replay fences for named-Brain host effects.
//!
//! Detailed write-ahead transitions remain in the bounded active segment.
//! Terminal identities move into immutable SQLite epochs referenced by one
//! atomically replaced manifest. An epoch is a physical segment of the same
//! canonical audit log, not an independently mutable outcome tracker.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::runtime::effect_log::{EffectAuditIdentity, EffectAuditTransition};

const MANIFEST_VERSION: u32 = 1;
pub(crate) const MAX_REPLAY_EPOCH_RECORDS: u64 = 32_768;
pub(crate) const MAX_REPLAY_EPOCH_ENCODED_BYTES: u64 = 32 * 1024 * 1024;
/// Exact replay membership consumes durable information for every unique
/// identity. Four GiB admits millions of compact records while giving the
/// daemon a truthful, finite fail-before-effect storage boundary.
pub(crate) const MAX_REPLAY_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub(crate) const MAX_ACTIVE_JOURNAL_BYTES: u64 = 24 * 1024 * 1024;
const RESERVED_TERMINAL_RECORD_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReplayManifest {
    version: u32,
    brain_id: uuid::Uuid,
    active_epoch: u64,
    epochs: Vec<ReplayEpoch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReplayEpoch {
    generation: u64,
    file: String,
    sealed: bool,
    record_count: u64,
    encoded_bytes: u64,
    max_seq: u64,
}

impl ReplayManifest {
    fn initial(brain_id: uuid::Uuid) -> Self {
        Self {
            version: MANIFEST_VERSION,
            brain_id,
            active_epoch: 0,
            epochs: vec![ReplayEpoch {
                generation: 0,
                file: epoch_file_name(0),
                sealed: false,
                record_count: 0,
                encoded_bytes: 0,
                max_seq: 0,
            }],
        }
    }

    fn validate(&self, brain_id: uuid::Uuid) -> Result<()> {
        anyhow::ensure!(
            self.version == MANIFEST_VERSION,
            "unsupported effect-audit replay manifest version {}",
            self.version
        );
        anyhow::ensure!(
            self.brain_id == brain_id,
            "effect-audit replay manifest belongs to another Brain"
        );
        anyhow::ensure!(
            !self.epochs.is_empty(),
            "effect-audit replay manifest has no active epoch"
        );
        for (index, epoch) in self.epochs.iter().enumerate() {
            anyhow::ensure!(
                epoch.generation == index as u64,
                "effect-audit replay epochs are missing, duplicated, or reordered"
            );
            anyhow::ensure!(
                epoch.file == epoch_file_name(epoch.generation),
                "effect-audit replay epoch has a non-canonical path"
            );
            anyhow::ensure!(
                epoch.sealed == (epoch.generation != self.active_epoch),
                "effect-audit replay manifest has ambiguous active epochs"
            );
            anyhow::ensure!(
                epoch.record_count <= MAX_REPLAY_EPOCH_RECORDS,
                "effect-audit replay epoch exceeds its record bound"
            );
            anyhow::ensure!(
                epoch.encoded_bytes <= MAX_REPLAY_EPOCH_ENCODED_BYTES,
                "effect-audit replay epoch exceeds its encoded-byte bound"
            );
        }
        anyhow::ensure!(
            self.active_epoch + 1 == self.epochs.len() as u64,
            "effect-audit replay active epoch is not the final epoch"
        );
        Ok(())
    }
}

/// A path-contained indexed view over immutable replay epochs.
pub(crate) struct EffectAuditReplayArchive {
    directory: PathBuf,
    manifest_path: PathBuf,
    manifest: ReplayManifest,
}

/// Bounded durable write-ahead segment for unresolved effects. Rows are
/// removed only after their terminal replay fence is durable.
pub(crate) struct EffectAuditActiveJournal {
    path: PathBuf,
}

impl EffectAuditActiveJournal {
    pub(crate) fn open(brain_directory: &Path) -> Result<Self> {
        let directory = brain_directory.join("effect-audit-replay");
        reject_symlink(&directory)?;
        super::store::create_dir_all_durable(&directory)?;
        let path = directory.join("active.sqlite3");
        reject_symlink(&path)?;
        let created = !path.exists();
        let connection = open_active(&path)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS transitions (
                seq INTEGER PRIMARY KEY NOT NULL,
                identity BLOB NOT NULL,
                transition_json BLOB NOT NULL,
                encoded_bytes INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS transitions_by_identity
                ON transitions(identity, seq);",
            )
            .with_context(|| format!("initialize {}", path.display()))?;
        if created {
            super::store::sync_directory(&directory)?;
        }
        let journal = Self { path };
        anyhow::ensure!(
            journal.file_bytes()? <= MAX_ACTIVE_JOURNAL_BYTES,
            "effect-audit active journal exceeds its durable byte bound"
        );
        Ok(journal)
    }

    pub(crate) fn load(&self) -> Result<Vec<(u64, EffectAuditTransition)>> {
        let connection = open_active(&self.path)?;
        let mut statement =
            connection.prepare("SELECT seq, transition_json FROM transitions ORDER BY seq ASC")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)? as u64, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut transitions = Vec::new();
        for row in rows {
            let (seq, encoded) = row?;
            transitions.push((
                seq,
                serde_json::from_slice(&encoded)
                    .with_context(|| format!("decode active effect-audit transition #{seq}"))?,
            ));
        }
        Ok(transitions)
    }

    pub(crate) fn max_seq(&self) -> Result<u64> {
        let connection = open_active(&self.path)?;
        Ok(
            connection.query_row("SELECT COALESCE(MAX(seq), 0) FROM transitions", [], |row| {
                Ok(row.get::<_, i64>(0)? as u64)
            })?,
        )
    }

    pub(crate) fn last_seq_for(&self, identity: &EffectAuditIdentity) -> Result<Option<u64>> {
        let connection = open_active(&self.path)?;
        let seq: Option<i64> = connection
            .query_row(
                "SELECT MAX(seq) FROM transitions WHERE identity = ?1",
                params![identity_key(identity)?],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(seq.map(|value| value as u64))
    }

    pub(crate) fn file_bytes(&self) -> Result<u64> {
        Ok(std::fs::metadata(&self.path)
            .with_context(|| format!("stat {}", self.path.display()))?
            .len())
    }

    pub(crate) fn ensure_reserve_capacity(
        &self,
        archive: &EffectAuditReplayArchive,
        encoded_reserve_bytes: usize,
    ) -> Result<()> {
        let active = self.file_bytes()?;
        anyhow::ensure!(
            active
                .saturating_add(encoded_reserve_bytes as u64)
                .saturating_add(RESERVED_TERMINAL_RECORD_BYTES)
                <= MAX_ACTIVE_JOURNAL_BYTES,
            "effect-audit active journal quota exceeded before durable host permit"
        );
        archive.ensure_total_storage_bound(
            active.saturating_add(encoded_reserve_bytes as u64),
            RESERVED_TERMINAL_RECORD_BYTES,
        )
    }

    pub(crate) fn append(&self, seq: u64, transition: &EffectAuditTransition) -> Result<()> {
        let encoded = serde_json::to_vec(transition)?;
        let encoded_len = encoded.len();
        let identity = identity_key(&transition.identity())?;
        let connection = open_active(&self.path)?;
        let existing: Option<Vec<u8>> = connection
            .query_row(
                "SELECT transition_json FROM transitions WHERE seq = ?1",
                params![seq as i64],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            anyhow::ensure!(
                existing == encoded,
                "conflicting active effect-audit transition sequence {seq}"
            );
            return Ok(());
        }
        connection
            .execute(
                "INSERT INTO transitions(seq, identity, transition_json, encoded_bytes)
             VALUES (?1, ?2, ?3, ?4)",
                params![seq as i64, identity, encoded, encoded_len as i64],
            )
            .with_context(|| format!("append active effect-audit transition #{seq}"))?;
        anyhow::ensure!(
            self.file_bytes()? <= MAX_ACTIVE_JOURNAL_BYTES,
            "effect-audit active journal exceeded its durable byte bound"
        );
        Ok(())
    }

    pub(crate) fn append_batch(&self, transitions: &[(u64, EffectAuditTransition)]) -> Result<()> {
        let mut connection = open_active(&self.path)?;
        let transaction = connection.transaction()?;
        for (seq, transition) in transitions {
            let encoded = serde_json::to_vec(transition)?;
            let encoded_len = encoded.len();
            let existing: Option<Vec<u8>> = transaction
                .query_row(
                    "SELECT transition_json FROM transitions WHERE seq = ?1",
                    params![*seq as i64],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(existing) = existing {
                anyhow::ensure!(
                    existing == encoded,
                    "conflicting active effect-audit transition sequence {seq}"
                );
                continue;
            }
            transaction.execute(
                "INSERT INTO transitions(seq, identity, transition_json, encoded_bytes)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    *seq as i64,
                    identity_key(&transition.identity())?,
                    encoded,
                    encoded_len as i64
                ],
            )?;
        }
        transaction.commit()?;
        anyhow::ensure!(
            self.file_bytes()? <= MAX_ACTIVE_JOURNAL_BYTES,
            "effect-audit active journal exceeded its durable byte bound"
        );
        Ok(())
    }

    /// Delete superseded details only after `append_fence` durably committed.
    pub(crate) fn remove_identity(&self, identity: &EffectAuditIdentity) -> Result<()> {
        let connection = open_active(&self.path)?;
        connection.execute(
            "DELETE FROM transitions WHERE identity = ?1",
            params![identity_key(identity)?],
        )?;
        Ok(())
    }
}

impl EffectAuditReplayArchive {
    pub(crate) fn open(brain_directory: &Path, brain_id: uuid::Uuid) -> Result<Self> {
        let directory = brain_directory.join("effect-audit-replay");
        reject_symlink(&directory)?;
        super::store::create_dir_all_durable(&directory)?;
        let manifest_path = directory.join("manifest.json");
        reject_symlink(&manifest_path)?;
        let mut manifest = if manifest_path.exists() {
            serde_json::from_slice(
                &std::fs::read(&manifest_path)
                    .with_context(|| format!("read {}", manifest_path.display()))?,
            )
            .with_context(|| format!("parse {}", manifest_path.display()))?
        } else {
            ReplayManifest::initial(brain_id)
        };
        manifest.validate(brain_id)?;
        for epoch in &manifest.epochs {
            let path = directory.join(&epoch.file);
            reject_symlink(&path)?;
            anyhow::ensure!(
                path.parent() == Some(directory.as_path()),
                "effect-audit replay epoch escaped its Brain directory"
            );
            if epoch.sealed {
                anyhow::ensure!(
                    path.is_file(),
                    "sealed effect-audit replay epoch {} is missing",
                    epoch.generation
                );
            }
        }
        let active = manifest.active_epoch as usize;
        let active_path = directory.join(&manifest.epochs[active].file);
        initialize_epoch(&active_path)?;
        let (record_count, encoded_bytes, max_seq) = epoch_stats(&active_path)?;
        anyhow::ensure!(
            record_count <= MAX_REPLAY_EPOCH_RECORDS
                && encoded_bytes <= MAX_REPLAY_EPOCH_ENCODED_BYTES,
            "active effect-audit replay epoch exceeds its durable bound"
        );
        manifest.epochs[active].record_count = record_count;
        manifest.epochs[active].encoded_bytes = encoded_bytes;
        manifest.epochs[active].max_seq = max_seq;
        let archive = Self {
            directory,
            manifest_path,
            manifest,
        };
        archive.ensure_total_storage_bound(0, 0)?;
        if !archive.manifest_path.exists() {
            archive.persist_manifest()?;
        }
        Ok(archive)
    }

    pub(crate) fn max_seq(&self) -> u64 {
        self.manifest
            .epochs
            .iter()
            .map(|epoch| epoch.max_seq)
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn lookup(
        &self,
        identity: &EffectAuditIdentity,
    ) -> Result<Option<EffectAuditTransition>> {
        let key = identity_key(identity)?;
        let mut found = None;
        for epoch in self.manifest.epochs.iter().rev() {
            let path = self.directory.join(&epoch.file);
            let connection = open_epoch(&path)?;
            let encoded: Option<Vec<u8>> = connection
                .query_row(
                    "SELECT transition_json FROM fences WHERE identity = ?1",
                    params![&key],
                    |row| row.get(0),
                )
                .optional()
                .with_context(|| format!("query {}", path.display()))?;
            let Some(encoded) = encoded else {
                continue;
            };
            let transition: EffectAuditTransition = serde_json::from_slice(&encoded)
                .with_context(|| format!("decode replay fence in {}", path.display()))?;
            anyhow::ensure!(
                transition.identity() == *identity,
                "effect-audit replay index returned a mismatched identity"
            );
            anyhow::ensure!(
                found.is_none(),
                "effect-audit replay identity appears in multiple epochs"
            );
            found = Some(transition);
        }
        Ok(found)
    }

    /// Fail before reserve/begin when the archive cannot retain the eventual
    /// terminal replay fence plus all currently active detailed state.
    pub(crate) fn ensure_total_storage_bound(
        &self,
        active_segment_bytes: u64,
        reserved_terminal_bytes: u64,
    ) -> Result<()> {
        let archive_bytes = self.archive_file_bytes()?;
        anyhow::ensure!(archive_bytes
                .saturating_add(active_segment_bytes)
                .saturating_add(reserved_terminal_bytes)
                <= MAX_REPLAY_ARCHIVE_BYTES,
            "effect-audit replay storage exhausted; archive or rotate this Brain before admitting another host effect");
        Ok(())
    }

    /// Append one terminal fence. Exact replay is idempotent; conflicting
    /// content under an old identity fails closed across every epoch.
    pub(crate) fn append_fence(
        &mut self,
        seq: u64,
        transition: &EffectAuditTransition,
        active_segment_bytes: u64,
    ) -> Result<bool> {
        anyhow::ensure!(
            matches!(transition, EffectAuditTransition::Fence { .. }),
            "only compact replay fences may enter the replay archive"
        );
        let encoded = serde_json::to_vec(transition)?;
        anyhow::ensure!(
            encoded.len() as u64 <= MAX_REPLAY_EPOCH_ENCODED_BYTES,
            "effect-audit replay fence exceeds its epoch byte bound"
        );
        if let Some(existing) = self.lookup(&transition.identity())? {
            anyhow::ensure!(
                existing == *transition,
                "conflicting effect-audit replay fence for an archived identity"
            );
            return Ok(false);
        }
        self.ensure_total_storage_bound(active_segment_bytes, encoded.len() as u64)?;
        self.roll_epoch_if_needed(encoded.len() as u64)?;
        let active = self.manifest.active_epoch as usize;
        let path = self.directory.join(&self.manifest.epochs[active].file);
        let connection = open_epoch(&path)?;
        let key = identity_key(&transition.identity())?;
        connection.execute(
            "INSERT INTO fences(identity, seq, transition_json, encoded_bytes) VALUES (?1, ?2, ?3, ?4)",
            params![key, seq as i64, &encoded, encoded.len() as i64],
        ).with_context(|| format!("append replay fence to {}", path.display()))?;
        connection.execute_batch("PRAGMA wal_checkpoint(FULL);")?;
        self.manifest.epochs[active].record_count += 1;
        self.manifest.epochs[active].encoded_bytes += encoded.len() as u64;
        self.manifest.epochs[active].max_seq = self.manifest.epochs[active].max_seq.max(seq);
        self.persist_manifest()?;
        Ok(true)
    }

    fn roll_epoch_if_needed(&mut self, next_bytes: u64) -> Result<()> {
        let active = self.manifest.active_epoch as usize;
        let epoch = &self.manifest.epochs[active];
        if epoch.record_count < MAX_REPLAY_EPOCH_RECORDS
            && epoch.encoded_bytes.saturating_add(next_bytes) <= MAX_REPLAY_EPOCH_ENCODED_BYTES
        {
            return Ok(());
        }
        let generation = self.manifest.active_epoch + 1;
        let path = self.directory.join(epoch_file_name(generation));
        initialize_epoch(&path)?;
        self.manifest.epochs[active].sealed = true;
        self.manifest.active_epoch = generation;
        self.manifest.epochs.push(ReplayEpoch {
            generation,
            file: epoch_file_name(generation),
            sealed: false,
            record_count: 0,
            encoded_bytes: 0,
            max_seq: 0,
        });
        self.persist_manifest()
    }

    fn archive_file_bytes(&self) -> Result<u64> {
        let mut total = std::fs::metadata(&self.manifest_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        for epoch in &self.manifest.epochs {
            let path = self.directory.join(&epoch.file);
            total = total.saturating_add(
                std::fs::metadata(&path)
                    .with_context(|| format!("stat {}", path.display()))?
                    .len(),
            );
        }
        Ok(total)
    }

    fn persist_manifest(&self) -> Result<()> {
        self.manifest.validate(self.manifest.brain_id)?;
        let temporary = self
            .directory
            .join(format!(".manifest.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, serde_json::to_vec_pretty(&self.manifest)?)
            .with_context(|| format!("write {}", temporary.display()))?;
        std::fs::File::open(&temporary)?.sync_all()?;
        std::fs::rename(&temporary, &self.manifest_path)
            .with_context(|| format!("commit {}", self.manifest_path.display()))?;
        super::store::sync_directory(&self.directory)
    }
}

fn epoch_file_name(generation: u64) -> String {
    format!("epoch-{generation:08}.sqlite3")
}

fn reject_symlink(path: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    anyhow::ensure!(
        !metadata.file_type().is_symlink(),
        "effect-audit path cannot be a symbolic link: {}",
        path.display()
    );
    Ok(())
}

fn identity_key(identity: &EffectAuditIdentity) -> Result<Vec<u8>> {
    serde_json::to_vec(identity).context("encode effect-audit replay identity")
}

fn initialize_epoch(path: &Path) -> Result<()> {
    let created = !path.exists();
    let connection = open_epoch(path)?;
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS fences (
            identity BLOB PRIMARY KEY NOT NULL,
            seq INTEGER NOT NULL,
            transition_json BLOB NOT NULL,
            encoded_bytes INTEGER NOT NULL
        ) WITHOUT ROWID;
        CREATE INDEX IF NOT EXISTS fences_by_seq ON fences(seq);",
        )
        .with_context(|| format!("initialize {}", path.display()))?;
    connection.execute_batch("PRAGMA wal_checkpoint(FULL);")?;
    if created {
        if let Some(parent) = path.parent() {
            super::store::sync_directory(parent)?;
        }
    }
    Ok(())
}

fn open_epoch(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    connection.execute_batch(
        "PRAGMA journal_mode = DELETE;
         PRAGMA synchronous = FULL;
         PRAGMA trusted_schema = OFF;",
    )?;
    Ok(connection)
}

fn open_active(path: &Path) -> Result<Connection> {
    let connection = open_epoch(path)?;
    connection.execute_batch("PRAGMA secure_delete = ON;")?;
    Ok(connection)
}

fn epoch_stats(path: &Path) -> Result<(u64, u64, u64)> {
    let connection = open_epoch(path)?;
    connection
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(encoded_bytes), 0), COALESCE(MAX(seq), 0) FROM fences",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                ))
            },
        )
        .with_context(|| format!("inspect {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fence(brain_id: uuid::Uuid, index: u64) -> EffectAuditTransition {
        EffectAuditTransition::Fence {
            identity: EffectAuditIdentity {
                brain_id,
                run_id: uuid::Uuid::from_u128(2),
                request_seq: 3,
                execution_id: uuid::Uuid::from_u128(index as u128 + 10),
                effect_sequence: index,
            },
            intent_sha256: format!("{index:064x}"),
            intent_bytes: 1,
            authority_id: uuid::Uuid::from_u128(4),
            authority_sha256: "a".repeat(64),
            outcome_kind: "abandoned_not_applied".into(),
            outcome_sha256: "b".repeat(64),
        }
    }

    #[test]
    fn replay_archive_rolls_past_32768_without_resetting_identity_fences() {
        let temporary = tempfile::tempdir().unwrap();
        let brain_id = uuid::Uuid::new_v4();
        let mut archive = EffectAuditReplayArchive::open(temporary.path(), brain_id).unwrap();
        let active_path = archive.directory.join(&archive.manifest.epochs[0].file);
        let mut connection = open_epoch(&active_path).unwrap();
        let transaction = connection.transaction().unwrap();
        for index in 0..MAX_REPLAY_EPOCH_RECORDS {
            let transition = fence(brain_id, index);
            let encoded = serde_json::to_vec(&transition).unwrap();
            let encoded_len = encoded.len();
            transaction
                .execute(
                    "INSERT INTO fences(identity, seq, transition_json, encoded_bytes)
                 VALUES (?1, ?2, ?3, ?4)",
                    params![
                        identity_key(&transition.identity()).unwrap(),
                        index as i64 + 1,
                        encoded,
                        encoded_len as i64
                    ],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
        let (count, bytes, max_seq) = epoch_stats(&active_path).unwrap();
        archive.manifest.epochs[0].record_count = count;
        archive.manifest.epochs[0].encoded_bytes = bytes;
        archive.manifest.epochs[0].max_seq = max_seq;
        archive.persist_manifest().unwrap();

        let old = fence(brain_id, 0);
        let next = fence(brain_id, MAX_REPLAY_EPOCH_RECORDS);
        assert!(!archive.append_fence(1, &old, 0).unwrap());
        assert!(archive
            .append_fence(MAX_REPLAY_EPOCH_RECORDS + 1, &next, 0)
            .unwrap());
        assert_eq!(archive.manifest.epochs.len(), 2);
        assert_eq!(archive.lookup(&old.identity()).unwrap(), Some(old));
        assert_eq!(
            archive.lookup(&next.identity()).unwrap(),
            Some(next.clone())
        );

        let mut conflict = next;
        let EffectAuditTransition::Fence { outcome_sha256, .. } = &mut conflict else {
            unreachable!();
        };
        *outcome_sha256 = "c".repeat(64);
        assert!(archive
            .append_fence(MAX_REPLAY_EPOCH_RECORDS + 2, &conflict, 0)
            .unwrap_err()
            .to_string()
            .contains("conflicting"));

        drop(archive);
        let reopened = EffectAuditReplayArchive::open(temporary.path(), brain_id).unwrap();
        assert_eq!(reopened.lookup(&old.identity()).unwrap(), Some(old));
    }

    #[test]
    fn replay_manifest_rejects_reordered_duplicate_and_missing_epochs() {
        let temporary = tempfile::tempdir().unwrap();
        let brain_id = uuid::Uuid::new_v4();
        let archive = EffectAuditReplayArchive::open(temporary.path(), brain_id).unwrap();
        let mut invalid = archive.manifest.clone();
        invalid.epochs.push(invalid.epochs[0].clone());
        invalid.active_epoch = 1;
        std::fs::write(
            &archive.manifest_path,
            serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap();
        assert!(EffectAuditReplayArchive::open(temporary.path(), brain_id)
            .err()
            .unwrap()
            .to_string()
            .contains("missing, duplicated, or reordered"));

        archive.persist_manifest().unwrap();
        let next_path = archive.directory.join(epoch_file_name(1));
        initialize_epoch(&next_path).unwrap();
        let mut missing = archive.manifest.clone();
        missing.epochs[0].sealed = true;
        missing.active_epoch = 1;
        missing.epochs.push(ReplayEpoch {
            generation: 1,
            file: epoch_file_name(1),
            sealed: false,
            record_count: 0,
            encoded_bytes: 0,
            max_seq: 0,
        });
        let missing_archive = EffectAuditReplayArchive {
            directory: archive.directory.clone(),
            manifest_path: archive.manifest_path.clone(),
            manifest: missing,
        };
        missing_archive.persist_manifest().unwrap();
        std::fs::remove_file(archive.directory.join(&archive.manifest.epochs[0].file)).unwrap();
        assert!(EffectAuditReplayArchive::open(temporary.path(), brain_id).is_err());
    }

    #[test]
    fn active_journal_exact_append_is_idempotent_and_conflict_fails_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let journal = EffectAuditActiveJournal::open(temporary.path()).unwrap();
        let transition = fence(uuid::Uuid::new_v4(), 1);
        journal.append(7, &transition).unwrap();
        journal.append(7, &transition).unwrap();
        assert_eq!(journal.load().unwrap(), vec![(7, transition.clone())]);
        assert!(journal
            .append(7, &fence(uuid::Uuid::new_v4(), 2))
            .unwrap_err()
            .to_string()
            .contains("conflicting"));
    }

    #[cfg(unix)]
    #[test]
    fn replay_archive_rejects_symlinked_storage_paths() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        symlink(target.path(), temporary.path().join("effect-audit-replay")).unwrap();
        assert!(
            EffectAuditReplayArchive::open(temporary.path(), uuid::Uuid::new_v4())
                .err()
                .unwrap()
                .to_string()
                .contains("symbolic link")
        );
    }
}

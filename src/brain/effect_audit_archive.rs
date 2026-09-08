//! Indexed, durable replay fences for named-Brain host effects.
//!
//! Detailed write-ahead transitions remain in the bounded active segment.
//! Terminal identities move into immutable SQLite epochs referenced by one
//! atomically replaced manifest. An epoch is a physical segment of the same
//! canonical audit log, not an independently mutable outcome tracker.
//!
//! Exact execute-once replay requires durable information proportional to the
//! number of unique effect identities: no finite structure can provide exact,
//! permanent membership without eventual operator archival. Epochs bound each
//! SQLite working set; the aggregate four-GiB admission limit is an explicit
//! fail-before-effect operator boundary, not a claim of fixed-memory infinity.
//! `index.sqlite3` is only a path locator. Epoch records remain authoritative;
//! an index disagreement fails closed, and a crash while appending the active
//! epoch is reconciled before the archive accepts new work.

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
pub(crate) const MAX_ACTIVE_JOURNAL_BYTES: u64 = 48 * 1024 * 1024;
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
    index_path: PathBuf,
    manifest: ReplayManifest,
    sealed_epoch_file_bytes: u64,
    #[cfg(test)]
    lookup_epoch_queries: std::cell::Cell<usize>,
}

/// Bounded durable write-ahead segment for unresolved effects. Rows are
/// removed only after their terminal replay fence is durable.
pub(crate) struct EffectAuditActiveJournal {
    path: PathBuf,
    /// A reported COMMIT failure whose durable outcome could not be proven.
    /// No writer may allocate another canonical sequence until restart reopens
    /// and replays the journal from disk.
    commit_outcome_uncertain: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_next_batch_before_commit: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    report_next_commit_error_after_durable_commit: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    make_next_commit_reconciliation_unknowable: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Test-only override of [`MAX_ACTIVE_JOURNAL_BYTES`]. The production bound
    /// is 48 MiB, which a deterministic regression cannot reach cheaply; the
    /// admission decision under test is the ordering of the bound check against
    /// the commit, not the numeric value of the bound.
    #[cfg(test)]
    max_bytes: std::sync::atomic::AtomicU64,
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
        let journal = Self {
            path,
            commit_outcome_uncertain: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_next_batch_before_commit: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            #[cfg(test)]
            report_next_commit_error_after_durable_commit: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            #[cfg(test)]
            make_next_commit_reconciliation_unknowable: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            #[cfg(test)]
            max_bytes: std::sync::atomic::AtomicU64::new(MAX_ACTIVE_JOURNAL_BYTES),
        };
        anyhow::ensure!(
            journal.file_bytes()? <= journal.max_bytes(),
            "effect-audit active journal exceeds its durable byte bound"
        );
        Ok(journal)
    }

    /// The durable byte ceiling this journal admits work against.
    fn max_bytes(&self) -> u64 {
        #[cfg(test)]
        {
            self.max_bytes.load(std::sync::atomic::Ordering::SeqCst)
        }
        #[cfg(not(test))]
        {
            MAX_ACTIVE_JOURNAL_BYTES
        }
    }

    #[cfg(test)]
    pub(crate) fn set_max_bytes_for_test(&self, bytes: u64) {
        self.max_bytes
            .store(bytes, std::sync::atomic::Ordering::SeqCst);
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
        self.ensure_commit_outcome_known()?;
        let active = self.file_bytes()?;
        let connection = open_active(&self.path)?;
        let active_identities: u64 = connection.query_row(
            "SELECT COUNT(DISTINCT identity) FROM transitions",
            [],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )?;
        let reserved_terminal_bytes = active_identities
            .saturating_add(1)
            .saturating_mul(RESERVED_TERMINAL_RECORD_BYTES);
        anyhow::ensure!(
            active
                .saturating_add(encoded_reserve_bytes as u64)
                .saturating_add(reserved_terminal_bytes)
                <= self.max_bytes(),
            "effect-audit active journal quota exceeded before durable host permit"
        );
        archive.ensure_total_storage_bound(
            active.saturating_add(encoded_reserve_bytes as u64),
            reserved_terminal_bytes,
        )
    }

    /// Durably admit one transition. The caller's sequence number is consumed
    /// only if this returns `Ok`.
    pub(crate) fn append(&self, seq: u64, transition: &EffectAuditTransition) -> Result<()> {
        self.append_transitions(std::iter::once((seq, transition)), false)
    }

    /// Durably admit a batch of transitions as one atomic outcome. Either every
    /// transition is committed and every sequence number is consumed, or none
    /// is and the durable journal is left exactly as it was.
    pub(crate) fn append_batch(&self, transitions: &[(u64, EffectAuditTransition)]) -> Result<()> {
        self.append_transitions(
            transitions
                .iter()
                .map(|(seq, transition)| (*seq, transition)),
            true,
        )
    }

    /// Single admission path for both the batch and single-transition writers.
    ///
    /// The durable byte bound is enforced **before** the commit, so a caller
    /// that observes `Err` can rely on nothing having been committed and on its
    /// sequence numbers still being free. Checking after the commit made a full
    /// journal deterministically corrupt the Brain's canonical sequence: the
    /// transitions were durable, the caller saw a failure, `state.revision`
    /// stayed behind them, and the next canonical append reused a `seq` the
    /// journal already held — which is what made a Brain permanently unloadable
    /// (#379, and the artifact in #377).
    #[cfg_attr(not(test), allow(unused_variables))]
    fn append_transitions<'a>(
        &self,
        transitions: impl IntoIterator<Item = (u64, &'a EffectAuditTransition)>,
        injectable_failure: bool,
    ) -> Result<()> {
        self.ensure_commit_outcome_known()?;
        let mut connection = open_active(&self.path)?;
        let transaction = connection.transaction()?;
        let mut intended = Vec::new();
        for (seq, transition) in transitions {
            let encoded = serde_json::to_vec(transition)?;
            intended.push((seq, encoded.clone()));
            let encoded_len = encoded.len();
            let existing: Option<Vec<u8>> = transaction
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
                continue;
            }
            transaction
                .execute(
                    "INSERT INTO transitions(seq, identity, transition_json, encoded_bytes)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        seq as i64,
                        identity_key(&transition.identity())?,
                        encoded,
                        encoded_len as i64
                    ],
                )
                .with_context(|| format!("append active effect-audit transition #{seq}"))?;
        }
        #[cfg(test)]
        if injectable_failure
            && self
                .fail_next_batch_before_commit
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            anyhow::bail!("injected active effect-audit transaction failure before commit");
        }
        let bound = self.max_bytes();
        let projected = pending_file_bytes(&transaction)?;
        if projected > bound {
            transaction
                .rollback()
                .context("roll back a refused active effect-audit transaction")?;
            anyhow::bail!(
                "effect-audit active journal refused an append that would exceed its durable \
                 byte bound: {projected} bytes projected against a bound of {bound}. Nothing \
                 was committed and the Brain's canonical sequence is unchanged. The journal \
                 drains when in-flight host effects reach a terminal outcome, which fences \
                 them into the replay archive; new host effects are refused before any permit \
                 is granted while that headroom is gone."
            );
        }
        let commit = transaction.commit().map_err(anyhow::Error::from);
        #[cfg(test)]
        let commit = if commit.is_ok()
            && self
                .report_next_commit_error_after_durable_commit
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            Err(anyhow::anyhow!(
                "injected active effect-audit COMMIT error after the transaction became durable"
            ))
        } else {
            commit
        };
        match commit {
            Ok(()) => Ok(()),
            Err(error) => self.reconcile_reported_commit_error(&intended, error),
        }
    }

    /// SQLite's DELETE-mode commit can make the transaction durable and then
    /// report a failure while deleting the rollback journal or releasing the
    /// file lock. In that case the caller must not reuse the batch's canonical
    /// sequence numbers. Treat the append as successful only when a fresh
    /// connection proves that every intended row is present byte-for-byte.
    fn reconcile_reported_commit_error(
        &self,
        intended: &[(u64, Vec<u8>)],
        commit_error: anyhow::Error,
    ) -> Result<()> {
        if intended.is_empty() {
            self.mark_commit_outcome_uncertain();
            return Err(commit_error).context(
                "effect-audit COMMIT was reported unsuccessful and an empty transaction cannot \
                 prove whether it became durable",
            );
        }
        #[cfg(test)]
        if self
            .make_next_commit_reconciliation_unknowable
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.mark_commit_outcome_uncertain();
            anyhow::bail!(
                "effect-audit COMMIT was reported unsuccessful and its durable outcome could \
                 not be determined: injected reconciliation read failure; commit error: \
                 {commit_error:#}"
            );
        }
        let connection = open_active(&self.path).map_err(|reconcile_error| {
            self.mark_commit_outcome_uncertain();
            anyhow::anyhow!(
                "effect-audit COMMIT was reported unsuccessful and its durable outcome could \
                 not be determined by reopening {}: commit error: {commit_error:#}; \
                 reconciliation error: {reconcile_error:#}",
                self.path.display()
            )
        })?;
        for (seq, expected) in intended {
            let actual: Option<Vec<u8>> = connection
                .query_row(
                    "SELECT transition_json FROM transitions WHERE seq = ?1",
                    params![*seq as i64],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|reconcile_error| {
                    self.mark_commit_outcome_uncertain();
                    anyhow::anyhow!(
                        "effect-audit COMMIT was reported unsuccessful and transition #{seq} \
                         could not be checked in {}: commit error: {commit_error:#}; \
                         reconciliation error: {reconcile_error:#}",
                        self.path.display()
                    )
                })?;
            let Some(actual) = actual else {
                return Err(commit_error).with_context(|| {
                    format!(
                        "effect-audit COMMIT was reported unsuccessful and transition #{seq} \
                         is absent from {}; the atomic batch is treated as uncommitted",
                        self.path.display()
                    )
                });
            };
            if actual != *expected {
                self.mark_commit_outcome_uncertain();
                anyhow::bail!(
                    "effect-audit COMMIT was reported unsuccessful and durable transition #{seq} \
                     conflicts with the intended batch in {} (expected {} bytes, found {} bytes); \
                     refusing to guess whether canonical sequence numbers were consumed; commit \
                     error: {commit_error:#}",
                    self.path.display(),
                    expected.len(),
                    actual.len()
                );
            }
        }
        Ok(())
    }

    fn mark_commit_outcome_uncertain(&self) {
        self.commit_outcome_uncertain
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn ensure_commit_outcome_known(&self) -> Result<()> {
        anyhow::ensure!(
            !self
                .commit_outcome_uncertain
                .load(std::sync::atomic::Ordering::SeqCst),
            "effect-audit journal has an unresolved SQLite COMMIT outcome; canonical writes for \
             this Brain are fenced until restart reopens and replays the durable journal"
        );
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_next_batch_before_commit_for_test(&self) {
        self.fail_next_batch_before_commit
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn report_next_commit_error_after_durable_commit_for_test(&self) {
        self.report_next_commit_error_after_durable_commit
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn make_next_commit_reconciliation_unknowable_for_test(&self) {
        self.make_next_commit_reconciliation_unknowable
            .store(true, std::sync::atomic::Ordering::SeqCst);
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

    pub(crate) fn remove_identities(&self, identities: &[EffectAuditIdentity]) -> Result<()> {
        let mut connection = open_active(&self.path)?;
        let transaction = connection.transaction()?;
        for identity in identities {
            transaction.execute(
                "DELETE FROM transitions WHERE identity = ?1",
                params![identity_key(identity)?],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

impl EffectAuditReplayArchive {
    pub(crate) fn open(brain_directory: &Path, brain_id: uuid::Uuid) -> Result<Self> {
        let directory = brain_directory.join("effect-audit-replay");
        reject_symlink(&directory)?;
        super::store::create_dir_all_durable(&directory)?;
        let manifest_path = directory.join("manifest.json");
        let index_path = directory.join("index.sqlite3");
        reject_symlink(&manifest_path)?;
        reject_symlink(&index_path)?;
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
        initialize_index(&index_path)?;
        reconcile_active_index(&index_path, &active_path, manifest.active_epoch)?;
        validate_index_counts(&index_path, &manifest)?;
        let sealed_epoch_file_bytes = manifest
            .epochs
            .iter()
            .filter(|epoch| epoch.sealed)
            .try_fold(0u64, |total, epoch| {
                let path = directory.join(&epoch.file);
                Ok::<_, anyhow::Error>(
                    total.saturating_add(
                        std::fs::metadata(&path)
                            .with_context(|| format!("stat {}", path.display()))?
                            .len(),
                    ),
                )
            })?;
        let archive = Self {
            directory,
            manifest_path,
            index_path,
            manifest,
            sealed_epoch_file_bytes,
            #[cfg(test)]
            lookup_epoch_queries: std::cell::Cell::new(0),
        };
        archive.ensure_total_storage_bound(0, 0)?;
        // Reconcile a crash after the active epoch commit but before its
        // derived index/manifest commits, then atomically publish recovered
        // counts before accepting new work.
        archive.persist_manifest()?;
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

    /// Read only a bounded newest tail for the redacted observer projection.
    /// Replay authority continues to use `lookup`; callers must never expose
    /// these complete internal fence records directly.
    pub(crate) fn latest(&self, limit: usize) -> Result<Vec<EffectAuditTransition>> {
        let mut newest = Vec::with_capacity(limit);
        for epoch in self.manifest.epochs.iter().rev() {
            if newest.len() == limit {
                break;
            }
            let path = self.directory.join(&epoch.file);
            let connection = open_epoch(&path)?;
            let remaining = limit - newest.len();
            let mut statement = connection
                .prepare("SELECT transition_json FROM fences ORDER BY seq DESC LIMIT ?1")?;
            let rows =
                statement.query_map(params![remaining as i64], |row| row.get::<_, Vec<u8>>(0))?;
            for row in rows {
                newest.push(
                    serde_json::from_slice(&row?)
                        .with_context(|| format!("decode observer tail in {}", path.display()))?,
                );
            }
        }
        newest.reverse();
        Ok(newest)
    }

    pub(crate) fn lookup(
        &self,
        identity: &EffectAuditIdentity,
    ) -> Result<Option<EffectAuditTransition>> {
        let key = identity_key(identity)?;
        let index = open_index(&self.index_path)?;
        let generation: Option<i64> = index
            .query_row(
                "SELECT generation FROM fence_locations WHERE identity = ?1",
                params![&key],
                |row| row.get(0),
            )
            .optional()?;
        let Some(generation) = generation else {
            return Ok(None);
        };
        let epoch = self
            .manifest
            .epochs
            .get(generation as usize)
            .context("effect-audit replay index names an unknown epoch")?;
        #[cfg(test)]
        self.lookup_epoch_queries
            .set(self.lookup_epoch_queries.get() + 1);
        let path = self.directory.join(&epoch.file);
        let connection = open_epoch(&path)?;
        let encoded: Vec<u8> = connection
            .query_row(
                "SELECT transition_json FROM fences WHERE identity = ?1",
                params![&key],
                |row| row.get(0),
            )
            .with_context(|| format!("query indexed replay fence in {}", path.display()))?;
        let transition: EffectAuditTransition = serde_json::from_slice(&encoded)
            .with_context(|| format!("decode replay fence in {}", path.display()))?;
        anyhow::ensure!(
            transition.identity() == *identity,
            "effect-audit replay index returned a mismatched identity"
        );
        Ok(Some(transition))
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
        Ok(self.append_fences(&[(seq, transition.clone())], active_segment_bytes)? == 1)
    }

    pub(crate) fn append_fences(
        &mut self,
        transitions: &[(u64, EffectAuditTransition)],
        active_segment_bytes: u64,
    ) -> Result<usize> {
        let mut pending = Vec::new();
        let mut encoded_bytes = 0u64;
        for (seq, transition) in transitions {
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
                continue;
            }
            encoded_bytes = encoded_bytes.saturating_add(encoded.len() as u64);
            pending.push((*seq, transition.clone(), encoded));
        }
        if pending.is_empty() {
            return Ok(0);
        }
        self.ensure_total_storage_bound(active_segment_bytes, encoded_bytes)?;
        self.roll_epoch_for_batch_if_needed(pending.len() as u64, encoded_bytes)?;
        let active = self.manifest.active_epoch as usize;
        let path = self.directory.join(&self.manifest.epochs[active].file);
        let mut connection = open_epoch(&path)?;
        let transaction = connection.transaction()?;
        for (seq, transition, encoded) in &pending {
            transaction
                .execute(
                    "INSERT INTO fences(identity, seq, transition_json, encoded_bytes)
                 VALUES (?1, ?2, ?3, ?4)",
                    params![
                        identity_key(&transition.identity())?,
                        *seq as i64,
                        encoded,
                        encoded.len() as i64
                    ],
                )
                .with_context(|| format!("append replay fence to {}", path.display()))?;
        }
        transaction.commit()?;
        let mut index = open_index(&self.index_path)?;
        let index_transaction = index.transaction()?;
        for (_, transition, _) in &pending {
            index_transaction
                .execute(
                    "INSERT INTO fence_locations(identity, generation) VALUES (?1, ?2)",
                    params![
                        identity_key(&transition.identity())?,
                        self.manifest.active_epoch as i64
                    ],
                )
                .context("index durable effect-audit replay fence")?;
        }
        index_transaction.commit()?;
        self.manifest.epochs[active].record_count += pending.len() as u64;
        self.manifest.epochs[active].encoded_bytes += encoded_bytes;
        self.manifest.epochs[active].max_seq = self.manifest.epochs[active]
            .max_seq
            .max(pending.iter().map(|(seq, _, _)| *seq).max().unwrap_or(0));
        self.persist_manifest()?;
        Ok(pending.len())
    }

    fn roll_epoch_for_batch_if_needed(&mut self, next_records: u64, next_bytes: u64) -> Result<()> {
        anyhow::ensure!(
            next_records <= MAX_REPLAY_EPOCH_RECORDS
                && next_bytes <= MAX_REPLAY_EPOCH_ENCODED_BYTES,
            "effect-audit terminal batch exceeds one replay epoch"
        );
        let active = self.manifest.active_epoch as usize;
        let epoch = &self.manifest.epochs[active];
        if epoch.record_count.saturating_add(next_records) <= MAX_REPLAY_EPOCH_RECORDS
            && epoch.encoded_bytes.saturating_add(next_bytes) <= MAX_REPLAY_EPOCH_ENCODED_BYTES
        {
            return Ok(());
        }
        self.roll_epoch_if_needed(MAX_REPLAY_EPOCH_ENCODED_BYTES)
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
        let sealed_path = self.directory.join(&self.manifest.epochs[active].file);
        let sealed_bytes = std::fs::metadata(&sealed_path)
            .with_context(|| format!("stat {}", sealed_path.display()))?
            .len();
        let path = self.directory.join(epoch_file_name(generation));
        reject_symlink(&path)?;
        initialize_epoch(&path)?;
        anyhow::ensure!(
            epoch_stats(&path)? == (0, 0, 0),
            "orphan effect-audit replay epoch is not empty"
        );
        self.manifest.epochs[active].sealed = true;
        self.sealed_epoch_file_bytes = self.sealed_epoch_file_bytes.saturating_add(sealed_bytes);
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
        let mut total = self.sealed_epoch_file_bytes.saturating_add(
            std::fs::metadata(&self.manifest_path)
                .map(|metadata| metadata.len())
                .unwrap_or(0),
        );
        let active = self.manifest.active_epoch as usize;
        let active_path = self.directory.join(&self.manifest.epochs[active].file);
        total = total.saturating_add(
            std::fs::metadata(&active_path)
                .with_context(|| format!("stat {}", active_path.display()))?
                .len(),
        );
        total = total.saturating_add(
            std::fs::metadata(&self.index_path)
                .with_context(|| format!("stat {}", self.index_path.display()))?
                .len(),
        );
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

    #[cfg(test)]
    pub(crate) fn seed_mature_history_for_test(
        &mut self,
        brain_id: uuid::Uuid,
        records: usize,
        epochs: usize,
    ) -> Result<()> {
        anyhow::ensure!(
            records > 0 && epochs > 0 && records >= epochs,
            "mature replay fixture needs at least one record per epoch"
        );
        anyhow::ensure!(
            self.manifest.epochs.len() == 1 && self.manifest.epochs[0].record_count == 0,
            "mature replay fixture requires an empty archive"
        );
        self.manifest.epochs.clear();
        let mut next = 0usize;
        for generation in 0..epochs {
            let path = self.directory.join(epoch_file_name(generation as u64));
            initialize_epoch(&path)?;
            let mut connection = open_epoch(&path)?;
            let transaction = connection.transaction()?;
            let remaining = records - next;
            let generations_left = epochs - generation;
            let take = remaining.div_ceil(generations_left);
            for index in next..next + take {
                let transition = EffectAuditTransition::Fence {
                    identity: EffectAuditIdentity {
                        brain_id,
                        run_id: uuid::Uuid::from_u128(0x163),
                        request_seq: 1,
                        execution_id: uuid::Uuid::from_u128(index as u128 + 1),
                        effect_sequence: index as u64,
                    },
                    intent_sha256: format!("{index:064x}"),
                    intent_bytes: 1,
                    authority_id: uuid::Uuid::from_u128(7),
                    authority_sha256: "a".repeat(64),
                    outcome_kind: "abandoned_not_applied".into(),
                    outcome_sha256: "b".repeat(64),
                };
                let encoded = serde_json::to_vec(&transition)?;
                transaction.execute(
                    "INSERT INTO fences(identity, seq, transition_json, encoded_bytes)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        identity_key(&transition.identity())?,
                        index as i64 + 1,
                        &encoded,
                        encoded.len() as i64
                    ],
                )?;
            }
            transaction.commit()?;
            let (record_count, encoded_bytes, max_seq) = epoch_stats(&path)?;
            self.manifest.epochs.push(ReplayEpoch {
                generation: generation as u64,
                file: epoch_file_name(generation as u64),
                sealed: generation + 1 != epochs,
                record_count,
                encoded_bytes,
                max_seq,
            });
            reconcile_active_index(&self.index_path, &path, generation as u64)?;
            next += take;
        }
        self.manifest.active_epoch = epochs as u64 - 1;
        self.sealed_epoch_file_bytes = self
            .manifest
            .epochs
            .iter()
            .filter(|epoch| epoch.sealed)
            .try_fold(0u64, |total, epoch| {
                let path = self.directory.join(&epoch.file);
                Ok::<_, anyhow::Error>(total.saturating_add(std::fs::metadata(&path)?.len()))
            })?;
        validate_index_counts(&self.index_path, &self.manifest)?;
        self.persist_manifest()
    }

    #[cfg(test)]
    pub(crate) fn exhaust_storage_for_test(&self) -> Result<()> {
        let active = self.manifest.active_epoch as usize;
        let path = self.directory.join(&self.manifest.epochs[active].file);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)?
            .set_len(MAX_REPLAY_ARCHIVE_BYTES)?;
        Ok(())
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

/// Size the database file will have once the open transaction commits.
///
/// These connections run `journal_mode = DELETE`, so a write transaction's
/// pages are already in the main database file and the rollback journal holds
/// the originals; `page_count` therefore reports the pending size, and a
/// rollback restores both the pages and the file length. A SQLite database
/// file is exactly `page_count * page_size` bytes, so this is the same quantity
/// [`EffectAuditActiveJournal::file_bytes`] observes after the commit — read
/// before it, where refusing still costs nothing.
fn pending_file_bytes(connection: &Connection) -> Result<u64> {
    let page_count: i64 = connection
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .context("read pending effect-audit journal page count")?;
    let page_size: i64 = connection
        .query_row("PRAGMA page_size", [], |row| row.get(0))
        .context("read effect-audit journal page size")?;
    Ok((page_count.max(0) as u64).saturating_mul(page_size.max(0) as u64))
}

fn initialize_index(path: &Path) -> Result<()> {
    let created = !path.exists();
    let connection = open_index(path)?;
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS fence_locations (
            identity BLOB PRIMARY KEY NOT NULL,
            generation INTEGER NOT NULL
        ) WITHOUT ROWID;
        CREATE INDEX IF NOT EXISTS fence_locations_by_generation
            ON fence_locations(generation);",
        )
        .with_context(|| format!("initialize {}", path.display()))?;
    if created {
        if let Some(parent) = path.parent() {
            super::store::sync_directory(parent)?;
        }
    }
    Ok(())
}

fn open_index(path: &Path) -> Result<Connection> {
    let connection = open_epoch(path)?;
    connection.execute_batch("PRAGMA secure_delete = ON;")?;
    Ok(connection)
}

fn reconcile_active_index(index_path: &Path, epoch_path: &Path, generation: u64) -> Result<()> {
    let epoch = open_epoch(epoch_path)?;
    let mut statement = epoch.prepare("SELECT identity FROM fences")?;
    let identities = statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut index = open_index(index_path)?;
    let transaction = index.transaction()?;
    for identity in identities {
        transaction.execute(
            "INSERT OR IGNORE INTO fence_locations(identity, generation) VALUES (?1, ?2)",
            params![&identity, generation as i64],
        )?;
        let indexed: i64 = transaction.query_row(
            "SELECT generation FROM fence_locations WHERE identity = ?1",
            params![&identity],
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            indexed == generation as i64,
            "effect-audit replay identity appears in multiple epochs"
        );
    }
    transaction.commit()?;
    Ok(())
}

fn validate_index_counts(index_path: &Path, manifest: &ReplayManifest) -> Result<()> {
    let index = open_index(index_path)?;
    for epoch in &manifest.epochs {
        let indexed: u64 = index.query_row(
            "SELECT COUNT(*) FROM fence_locations WHERE generation = ?1",
            params![epoch.generation as i64],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )?;
        anyhow::ensure!(
            indexed == epoch.record_count,
            "effect-audit replay index count disagrees with epoch {}",
            epoch.generation
        );
    }
    let unknown: u64 = index.query_row(
        "SELECT COUNT(*) FROM fence_locations WHERE generation < 0 OR generation > ?1",
        params![manifest.active_epoch as i64],
        |row| Ok(row.get::<_, i64>(0)? as u64),
    )?;
    anyhow::ensure!(
        unknown == 0,
        "effect-audit replay index contains an unknown epoch"
    );
    Ok(())
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
        reconcile_active_index(&archive.index_path, &active_path, 0).unwrap();
        archive.persist_manifest().unwrap();

        let old = fence(brain_id, 0);
        let next = fence(brain_id, MAX_REPLAY_EPOCH_RECORDS);
        assert!(!archive.append_fence(1, &old, 0).unwrap());
        assert!(archive
            .append_fence(MAX_REPLAY_EPOCH_RECORDS + 1, &next, 0)
            .unwrap());
        assert_eq!(archive.manifest.epochs.len(), 2);
        archive.lookup_epoch_queries.set(0);
        assert_eq!(archive.lookup(&old.identity()).unwrap(), Some(old.clone()));
        assert_eq!(
            archive.lookup(&next.identity()).unwrap(),
            Some(next.clone())
        );
        assert_eq!(
            archive.lookup_epoch_queries.get(),
            2,
            "each mature-history hit opens exactly one indexed epoch"
        );
        let missing = fence(brain_id, MAX_REPLAY_EPOCH_RECORDS + 10);
        assert_eq!(archive.lookup(&missing.identity()).unwrap(), None);
        assert_eq!(
            archive.lookup_epoch_queries.get(),
            2,
            "a mature-history miss never scans replay epochs"
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
        invalid.epochs[0].sealed = true;
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
            index_path: archive.index_path.clone(),
            manifest: missing,
            sealed_epoch_file_bytes: archive.sealed_epoch_file_bytes,
            lookup_epoch_queries: std::cell::Cell::new(0),
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

    #[test]
    fn reported_commit_error_preserves_absent_error_and_rejects_conflicting_row() {
        let temporary = tempfile::tempdir().unwrap();
        let journal = EffectAuditActiveJournal::open(temporary.path()).unwrap();
        let intended = fence(uuid::Uuid::new_v4(), 1);
        let intended_bytes = serde_json::to_vec(&intended).unwrap();
        let absent = journal
            .reconcile_reported_commit_error(
                &[(7, intended_bytes.clone())],
                anyhow::anyhow!("original SQLite COMMIT error for absent row"),
            )
            .expect_err("an absent intended row must preserve the reported COMMIT failure");
        let absent_diagnostic = format!("{absent:#}");
        assert!(
            absent_diagnostic.contains("transition #7 is absent")
                && absent_diagnostic.contains("original SQLite COMMIT error for absent row"),
            "an absent-row reconciliation must identify the unconsumed sequence and retain the \
             original COMMIT error (expected_bytes={}, journal_bytes={}, journal_seqs={:?}, \
             diagnostic={absent_diagnostic})",
            intended_bytes.len(),
            journal.file_bytes().unwrap(),
            journal
                .load()
                .unwrap()
                .into_iter()
                .map(|(seq, _)| seq)
                .collect::<Vec<_>>()
        );
        journal
            .ensure_commit_outcome_known()
            .unwrap_or_else(|error| {
                panic!(
                "confirmed absence must leave the journal writable because the sequence was not \
                 consumed (expected_bytes={}, journal_bytes={}, error={error:#}, original \
                 diagnostic={absent_diagnostic})",
                intended_bytes.len(),
                journal.file_bytes().unwrap()
            )
            });

        let conflicting = fence(uuid::Uuid::new_v4(), 2);
        journal.append(7, &conflicting).unwrap();
        let conflict = journal
            .reconcile_reported_commit_error(
                &[(7, intended_bytes.clone())],
                anyhow::anyhow!("original SQLite COMMIT error for conflicting row"),
            )
            .expect_err("a conflicting durable row must fail closed rather than consume its seq");
        let conflict_diagnostic = format!("{conflict:#}");
        assert!(
            conflict_diagnostic.contains("transition #7")
                && conflict_diagnostic.contains("conflicts with the intended batch")
                && conflict_diagnostic.contains("original SQLite COMMIT error for conflicting row"),
            "a conflicting-row reconciliation must identify the ambiguous sequence and retain \
             the original COMMIT error (expected_bytes={}, actual_bytes={}, journal_bytes={}, \
             journal_seqs={:?}, diagnostic={conflict_diagnostic})",
            intended_bytes.len(),
            serde_json::to_vec(&conflicting).unwrap().len(),
            journal.file_bytes().unwrap(),
            journal
                .load()
                .unwrap()
                .into_iter()
                .map(|(seq, _)| seq)
                .collect::<Vec<_>>()
        );
        let poison = journal
            .ensure_commit_outcome_known()
            .expect_err("a conflicting durable row must fence later canonical writers");
        assert!(
            format!("{poison:#}").contains("fenced until restart"),
            "a conflicting durable row must produce an actionable persistent-process fence \
             (expected_bytes={}, actual_bytes={}, journal_bytes={}, conflict={conflict_diagnostic}, \
             fence={poison:#})",
            intended_bytes.len(),
            serde_json::to_vec(&conflicting).unwrap().len(),
            journal.file_bytes().unwrap()
        );
    }

    #[test]
    fn replay_archive_recovers_epoch_commit_before_index_and_manifest() {
        let temporary = tempfile::tempdir().unwrap();
        let brain_id = uuid::Uuid::new_v4();
        let archive = EffectAuditReplayArchive::open(temporary.path(), brain_id).unwrap();
        let transition = fence(brain_id, 44);
        let encoded = serde_json::to_vec(&transition).unwrap();
        let active_path = archive.directory.join(&archive.manifest.epochs[0].file);
        let connection = open_epoch(&active_path).unwrap();
        connection
            .execute(
                "INSERT INTO fences(identity, seq, transition_json, encoded_bytes)
             VALUES (?1, ?2, ?3, ?4)",
                params![
                    identity_key(&transition.identity()).unwrap(),
                    45_i64,
                    &encoded,
                    encoded.len() as i64
                ],
            )
            .unwrap();
        drop(archive);

        let recovered = EffectAuditReplayArchive::open(temporary.path(), brain_id).unwrap();
        assert_eq!(recovered.manifest.epochs[0].record_count, 1);
        assert_eq!(
            recovered.lookup(&transition.identity()).unwrap(),
            Some(transition)
        );
        drop(recovered);
        let reopened = EffectAuditReplayArchive::open(temporary.path(), brain_id).unwrap();
        assert_eq!(
            reopened.manifest.epochs[0].record_count, 1,
            "recovered epoch metadata was atomically republished"
        );
    }

    #[test]
    fn replay_archive_detects_corrupt_and_duplicate_epoch_membership() {
        let corrupt = tempfile::tempdir().unwrap();
        let brain_id = uuid::Uuid::new_v4();
        let mut archive = EffectAuditReplayArchive::open(corrupt.path(), brain_id).unwrap();
        archive
            .seed_mature_history_for_test(brain_id, 4, 2)
            .unwrap();
        let old = archive.latest(4).unwrap().remove(0);
        let sealed = archive.directory.join(epoch_file_name(0));
        drop(archive);
        std::fs::write(&sealed, b"not a sqlite database").unwrap();
        let reopened = EffectAuditReplayArchive::open(corrupt.path(), brain_id).unwrap();
        assert!(
            reopened.lookup(&old.identity()).is_err(),
            "an indexed corrupt sealed epoch must fail closed on exact lookup"
        );

        let duplicate = tempfile::tempdir().unwrap();
        let brain_id = uuid::Uuid::new_v4();
        let mut archive = EffectAuditReplayArchive::open(duplicate.path(), brain_id).unwrap();
        archive
            .seed_mature_history_for_test(brain_id, 4, 2)
            .unwrap();
        let old = archive.latest(4).unwrap().remove(0);
        let encoded = serde_json::to_vec(&old).unwrap();
        let active = archive.directory.join(epoch_file_name(1));
        let connection = open_epoch(&active).unwrap();
        connection
            .execute(
                "INSERT INTO fences(identity, seq, transition_json, encoded_bytes)
             VALUES (?1, ?2, ?3, ?4)",
                params![
                    identity_key(&old.identity()).unwrap(),
                    99_i64,
                    &encoded,
                    encoded.len() as i64
                ],
            )
            .unwrap();
        drop(archive);
        assert!(
            EffectAuditReplayArchive::open(duplicate.path(), brain_id)
                .err()
                .unwrap()
                .to_string()
                .contains("multiple epochs"),
            "duplicate identity membership across epochs must fail closed"
        );
    }

    /// #379 relies on `pending_file_bytes` reading, before the commit, the
    /// exact size `file_bytes` reports after it. If those two ever disagree the
    /// bound would be enforced against a different quantity than it is measured
    /// with at `open` and at reserve admission.
    #[test]
    fn test_pending_file_bytes_matches_committed_file_bytes() {
        let temporary = tempfile::tempdir().unwrap();
        let journal = EffectAuditActiveJournal::open(temporary.path()).unwrap();
        let brain_id = uuid::Uuid::new_v4();
        for round in 0..8u64 {
            let batch = (0..32u64)
                .map(|index| {
                    let seq = round * 32 + index + 1;
                    (seq, fence(brain_id, seq))
                })
                .collect::<Vec<_>>();
            journal.append_batch(&batch).unwrap();
            let mut connection = open_active(&journal.path).unwrap();
            let transaction = connection.transaction().unwrap();
            let pending = pending_file_bytes(&transaction).unwrap();
            transaction.rollback().unwrap();
            let committed = journal.file_bytes().unwrap();
            assert_eq!(
                pending, committed,
                "the pre-commit projection and the post-commit file size must be the same \
                 quantity after round {round}: projected {pending} bytes, file {committed} bytes"
            );
        }
    }

    /// #379: a refused append must leave the durable journal byte-identical and
    /// its sequence space untouched, at the journal boundary itself.
    #[test]
    fn test_append_batch_past_byte_bound_rolls_back_and_commits_no_sequence() {
        let temporary = tempfile::tempdir().unwrap();
        let journal = EffectAuditActiveJournal::open(temporary.path()).unwrap();
        let brain_id = uuid::Uuid::new_v4();
        let seeded = (1..=64u64)
            .map(|seq| (seq, fence(brain_id, seq)))
            .collect::<Vec<_>>();
        journal.append_batch(&seeded).unwrap();
        let settled = journal.file_bytes().unwrap();
        let bytes_before = std::fs::read(&journal.path).unwrap();
        let seqs_before = journal
            .load()
            .unwrap()
            .into_iter()
            .map(|(seq, _)| seq)
            .collect::<Vec<_>>();

        journal.set_max_bytes_for_test(settled);
        let crossing = (65..=256u64)
            .map(|seq| (seq, fence(brain_id, seq)))
            .collect::<Vec<_>>();
        let refusal = journal.append_batch(&crossing).expect_err(
            "a batch that grows the journal past a bound equal to its settled size must be \
             refused",
        );

        let bytes_after = std::fs::read(&journal.path).unwrap();
        let seqs_after = journal
            .load()
            .unwrap()
            .into_iter()
            .map(|(seq, _)| seq)
            .collect::<Vec<_>>();
        assert!(
            format!("{refusal:#}").contains("durable byte bound"),
            "the refusal must name the durable byte bound (bound={settled} bytes, \
             refusal={refusal:#})"
        );
        assert!(
            bytes_before == bytes_after,
            "a refused append must leave the durable journal byte-identical: {} bytes before, \
             {} bytes after, bound={settled}, refusal={refusal:#}",
            bytes_before.len(),
            bytes_after.len()
        );
        assert_eq!(
            seqs_after,
            seqs_before,
            "a refused append must commit no sequence: journal held {} rows ending at {:?} \
             before and {} rows ending at {:?} after, bound={settled} bytes, refusal={refusal:#}",
            seqs_before.len(),
            seqs_before.last(),
            seqs_after.len(),
            seqs_after.last()
        );
        journal.set_max_bytes_for_test(MAX_ACTIVE_JOURNAL_BYTES);
        journal
            .append_batch(&crossing)
            .expect("the refused batch must be admissible once the bound allows it");
        let seqs_final = journal
            .load()
            .unwrap()
            .into_iter()
            .map(|(seq, _)| seq)
            .collect::<Vec<_>>();
        assert_eq!(
            seqs_final.len(),
            256,
            "the retried batch must admit every transition exactly once (journal holds {} rows, \
             last {:?})",
            seqs_final.len(),
            seqs_final.last()
        );
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

//! Physical JSONL persistence for the Brain event journal.

use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{BrainEvent, BrainEventKind, BrainJournalRecord};
use crate::attachment::{self, AttachmentId, BrainAttachment};
use crate::run::{BrainRun, BrainRunnerLease, RunId};

/// Append-only event log rooted at a Brain store directory.
#[derive(Clone)]
pub struct EventJournal {
    root: Option<PathBuf>,
}

impl EventJournal {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self { root }
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn read(&self, name: &str) -> Result<Vec<BrainEvent>> {
        read_events(self.root.as_deref(), name)
    }

    pub fn append(&self, name: &str, event: &BrainEvent) -> Result<()> {
        append_event(self.root.as_deref(), name, event)
    }

    pub fn append_batch(&self, name: &str, events: &[BrainEvent]) -> Result<()> {
        append_event_batch(self.root.as_deref(), name, events)
    }

    pub fn rewrite(&self, name: &str, events: &[BrainEvent]) -> Result<()> {
        rewrite_events(self.root.as_deref(), name, events)
    }
}

#[derive(Default)]
pub struct JournalProjection {
    pub events: u64,
    pub revision: u64,
    pub turns: u64,
    pub updated_ms: Option<u64>,
    pub attachments: HashMap<AttachmentId, BrainAttachment>,
    pub runs: HashMap<RunId, BrainRun>,
    pub runner: Option<BrainRunnerLease>,
}

pub fn event_path(root: Option<&Path>, name: &str) -> Option<PathBuf> {
    root.map(|root| root.join(name).join("events.jsonl"))
}

pub fn read_events(root: Option<&Path>, name: &str) -> Result<Vec<BrainEvent>> {
    let Some(path) = event_path(root, name) else {
        return Ok(Vec::new());
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(Vec::new());
    };
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        let committed_len = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|offset| offset + 1)
            .unwrap_or(0);
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .with_context(|| format!("open {} for torn-tail recovery", path.display()))?;
        file.set_len(committed_len as u64)
            .with_context(|| format!("truncate torn tail in {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync recovered {}", path.display()))?;
    }
    let mut events = Vec::new();
    let records = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    let mut committed_offset = 0usize;
    for (line_no, terminated) in records.iter().enumerate() {
        // Committed records are newline-terminated. A torn final append
        // projects none of its logical events after restart.
        if terminated.last() != Some(&b'\n') {
            break;
        }
        let line = &terminated[..terminated.len() - 1];
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let parsed = match serde_json::from_slice::<BrainJournalRecord>(line) {
            Ok(BrainJournalRecord::EventBatch {
                event_count,
                payload_sha256,
                events: batch,
            }) => {
                let valid_framing = match (event_count, payload_sha256) {
                    (None, None) => true,
                    (Some(count), Some(checksum)) => {
                        count == batch.len()
                            && serde_json::to_vec(&batch).is_ok_and(|payload| {
                                hex::encode(Sha256::digest(payload)) == checksum
                            })
                    }
                    _ => false,
                };
                valid_framing.then_some(batch)
            }
            Err(_) => serde_json::from_slice::<BrainEvent>(line)
                .ok()
                .map(|event| vec![event]),
        };
        if let Some(batch) = parsed {
            events.extend(batch);
            committed_offset += terminated.len();
            continue;
        }
        if line_no + 1 == records.len() {
            let file = OpenOptions::new()
                .write(true)
                .open(&path)
                .with_context(|| format!("open {} for corrupt-tail recovery", path.display()))?;
            file.set_len(committed_offset as u64)
                .with_context(|| format!("truncate corrupt tail in {}", path.display()))?;
            file.sync_all()
                .with_context(|| format!("sync recovered {}", path.display()))?;
            break;
        }
        anyhow::bail!("parse {} line {}", path.display(), line_no + 1);
    }
    Ok(events)
}

pub fn append_event(root: Option<&Path>, name: &str, event: &BrainEvent) -> Result<()> {
    append_journal_value(root, name, event)
}

pub fn append_event_batch(root: Option<&Path>, name: &str, events: &[BrainEvent]) -> Result<()> {
    anyhow::ensure!(!events.is_empty(), "Brain event batch cannot be empty");
    let payload = serde_json::to_vec(events)?;
    append_journal_value(
        root,
        name,
        &BrainJournalRecord::EventBatch {
            event_count: Some(events.len()),
            payload_sha256: Some(hex::encode(Sha256::digest(payload))),
            events: events.to_vec(),
        },
    )
}

/// Atomically replace the canonical journal with an equivalent sequence
/// of individual events. This is used only for bounded audit-history
/// compaction; mutation receipts and every non-audit event are preserved.
pub fn rewrite_events(root: Option<&Path>, name: &str, events: &[BrainEvent]) -> Result<()> {
    let Some(path) = event_path(root, name) else {
        return Ok(());
    };
    let directory = path.parent().context("Brain event log has no parent")?;
    create_dir_all_durable(directory)?;
    let temporary = directory.join(format!(".events.{}.tmp", uuid::Uuid::new_v4()));
    let rewrite = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        for event in events {
            serde_json::to_writer(&mut file, event)?;
            file.write_all(b"\n")?;
        }
        file.sync_all()
            .with_context(|| format!("sync {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("replace {}", path.display()))?;
        sync_directory(directory)
    })();
    if rewrite.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    rewrite
}

pub fn append_journal_value<T: Serialize>(
    root: Option<&Path>,
    name: &str,
    value: &T,
) -> Result<()> {
    let Some(path) = event_path(root, name) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        create_dir_all_durable(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let new_file = !path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let mut record = serde_json::to_vec(value)?;
    record.push(b'\n');
    file.write_all(&record)?;
    file.sync_all()
        .with_context(|| format!("sync {}", path.display()))?;
    if new_file {
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)
                .with_context(|| format!("open {} for directory sync", parent.display()))?
                .sync_all()
                .with_context(|| format!("sync directory {}", parent.display()))?;
        }
    }
    Ok(())
}

pub fn sync_directory(path: &std::path::Path) -> Result<()> {
    std::fs::File::open(path)
        .with_context(|| format!("open {} for directory sync", path.display()))?
        .sync_all()
        .with_context(|| format!("sync directory {}", path.display()))
}

/// Create and durably link every missing directory component, including the
/// configured Brain root when it does not yet exist.
pub fn create_dir_all_durable(path: &std::path::Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut cursor = path;
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        let Some(parent) = cursor.parent() else { break };
        cursor = parent;
    }
    std::fs::create_dir_all(path)?;
    for directory in missing {
        sync_directory(&directory)?;
        if let Some(parent) = directory.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

/// Best-effort read of a Brain journal. Unreadable or torn lines are skipped;
/// the file is never truncated.
pub fn scan_readonly(path: &Path) -> JournalProjection {
    let mut projection = JournalProjection::default();
    let Ok(bytes) = std::fs::read(path) else {
        return projection;
    };
    for terminated in bytes.split_inclusive(|byte| *byte == b'\n') {
        if terminated.last() != Some(&b'\n') {
            break;
        }
        let line = &terminated[..terminated.len() - 1];
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let parsed = match serde_json::from_slice::<BrainJournalRecord>(line) {
            Ok(BrainJournalRecord::EventBatch { events: batch, .. }) => batch,
            Err(_) => match serde_json::from_slice::<BrainEvent>(line) {
                Ok(event) => vec![event],
                Err(_) => continue,
            },
        };
        for event in parsed {
            attachment::apply_event(&mut projection.attachments, &event);
            apply_scan_event(&mut projection, event);
        }
    }
    projection
}

fn apply_scan_event(projection: &mut JournalProjection, event: BrainEvent) {
    projection.events += 1;
    projection.revision = projection.revision.max(event.seq);
    projection.updated_ms = Some(projection.updated_ms.unwrap_or(0).max(event.created_ms));
    match event.kind {
        BrainEventKind::Prompt { .. } => {
            projection.turns += 1;
        }
        BrainEventKind::ClientAttached { .. } | BrainEventKind::ClientDetached { .. } => {}
        BrainEventKind::RunStarted { run } => {
            projection.runs.insert(run.run_id, run);
        }
        BrainEventKind::RunStatusChanged {
            run_id,
            status,
            detail,
        } => {
            if let Some(run) = projection.runs.get_mut(&run_id) {
                run.status = status;
                run.updated_ms = event.created_ms;
                run.detail = detail;
            }
        }
        BrainEventKind::ScheduleDue { due } => {
            projection.runs.insert(due.run.run_id, due.run);
        }
        BrainEventKind::RunnerLeaseAcquired { lease } => {
            projection.runner = Some(lease);
        }
        BrainEventKind::RunnerLeaseReleased { lease_id } => {
            if projection
                .runner
                .as_ref()
                .is_some_and(|lease| lease.lease_id == lease_id)
            {
                projection.runner = None;
            }
        }
        BrainEventKind::RunnerHandoffCompleted { lease, .. } => {
            projection.runner = Some(lease);
        }
        _ => {}
    }
}

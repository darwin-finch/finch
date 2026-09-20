//! Attachment identities, live connections, and durable projection cursors.
//!
//! The journal records attach/detach; this facade owns the participant
//! records and the `attachments.json` cursor file that survives reconnect.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use super::journal::{create_dir_all_durable, BrainId};

/// Stable identity of one client projection of a Brain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttachmentId(pub uuid::Uuid);

impl AttachmentId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// Identity of one live transport connection for a durable attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectionId(pub uuid::Uuid);

impl ConnectionId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for ConnectionId {
    fn default() -> Self {
        Self(uuid::Uuid::nil())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRole {
    Runner,
    Driver,
    Consultant,
    Observer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainAttachment {
    pub attachment_id: AttachmentId,
    pub subject: String,
    pub role: AttachmentRole,
    pub acknowledged_seq: u64,
    pub connected: bool,
    pub connection_id: Option<ConnectionId>,
}

/// Exact participant/environment boundary to which a Brain-owned approval
/// request is addressed. This is policy input, not a bearer credential;
/// possession of this record does not authorize a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainApprovalAudience {
    pub brain_id: BrainId,
    pub brain: String,
    pub attachment_id: AttachmentId,
    pub subject: String,
    pub role: AttachmentRole,
    pub environment_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AttachmentCursorFile {
    version: u32,
    brain_id: BrainId,
    cursors: HashMap<AttachmentId, u64>,
}

/// Load durable acknowledgement cursors. Missing files are empty; identity
/// mismatch is an error so a swapped directory cannot silently rewind.
pub fn read_cursors(
    root: Option<&Path>,
    name: &str,
    brain_id: BrainId,
) -> Result<HashMap<AttachmentId, u64>> {
    let Some(root) = root else {
        return Ok(HashMap::new());
    };
    let path = root.join(name).join("attachments.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(HashMap::new());
    };
    let file: AttachmentCursorFile =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    if file.version != 1 || file.brain_id != brain_id {
        anyhow::bail!("attachment cursor identity mismatch at {}", path.display());
    }
    Ok(file.cursors)
}

/// Persist acknowledgement cursors for every known attachment.
pub fn write_cursors(
    root: Option<&Path>,
    name: &str,
    brain_id: BrainId,
    attachments: &HashMap<AttachmentId, BrainAttachment>,
) -> Result<()> {
    let Some(root) = root else {
        return Ok(());
    };
    let file = AttachmentCursorFile {
        version: 1,
        brain_id,
        cursors: attachments
            .iter()
            .map(|(id, attachment)| (*id, attachment.acknowledged_seq))
            .collect(),
    };
    let directory = root.join(name);
    create_dir_all_durable(&directory)
        .with_context(|| format!("create {}", directory.display()))?;
    let path = directory.join("attachments.json");
    let temporary = directory.join(format!(".attachments.{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, serde_json::to_vec_pretty(&file)?)
        .with_context(|| format!("write {}", temporary.display()))?;
    std::fs::rename(&temporary, &path).with_context(|| format!("commit {}", path.display()))?;
    Ok(())
}

pub fn sorted_attachments(
    attachments: &HashMap<AttachmentId, BrainAttachment>,
) -> Vec<BrainAttachment> {
    let mut attachments = attachments.values().cloned().collect::<Vec<_>>();
    attachments.sort_by_key(|attachment| attachment.attachment_id.0);
    attachments
}

/// Fold one attach/detach event into the live attachment map.
pub fn apply_event(
    attachments: &mut HashMap<AttachmentId, BrainAttachment>,
    event: &super::journal::BrainEvent,
) {
    use super::journal::BrainEventKind;
    match &event.kind {
        BrainEventKind::ClientAttached {
            attachment_id,
            connection_id,
            subject,
            role,
        } => {
            let acknowledged_seq = attachments
                .get(attachment_id)
                .map(|attachment| attachment.acknowledged_seq)
                .unwrap_or(0);
            attachments.insert(
                *attachment_id,
                BrainAttachment {
                    attachment_id: *attachment_id,
                    subject: subject.clone(),
                    role: *role,
                    acknowledged_seq,
                    connected: true,
                    connection_id: Some(*connection_id),
                },
            );
        }
        BrainEventKind::ClientDetached {
            attachment_id,
            connection_id,
        } => {
            if let Some(attachment) = attachments.get_mut(attachment_id) {
                if attachment.connection_id == Some(*connection_id) {
                    attachment.connected = false;
                    attachment.connection_id = None;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;

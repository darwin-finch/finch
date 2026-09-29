//! Production-boundary proof that the occurrence-chain link-conflict
//! recovery message never reaches a transcript-visible (WARN-or-above)
//! sink (#1383): the race is expected, self-recovering, and non-fatal (the
//! new occurrence and its point are still recorded, just not linked from
//! their predecessor), so it belongs at `tracing::debug!`, not
//! `tracing::warn!`. Before the fix this fired as `tracing::warn!` and the
//! finch binary's `OutputManagerLayer` (`src/cli/output_layer.rs`) forwards
//! every WARN/ERROR event from a non-`finch::`-prefixed crate straight into
//! the interactive transcript, unframed, as raw internal detail
//! (`⚠️  [finch_memory] occurrence chain link lost a race...`).
//!
//! This lives in its own integration binary, matching
//! `gate_observability_test.rs`'s precedent in this same crate: a
//! `tracing::subscriber::with_default` capture window races against any
//! sibling lib test hitting the same tracing callsite concurrently (tracing's
//! per-callsite interest cache is process-global), so a dedicated binary
//! gives the capture window the process to itself.

use std::io::Write;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use finch_memory::{MemoryConfig, MemorySystem};
use rusqlite::{params, Connection};
use uuid::Uuid;

#[derive(Clone)]
struct SharedLogBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for SharedLogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedLogBuffer {
    type Writer = SharedLogBuffer;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn substantive(tag: &str) -> String {
    format!(
        "The deploy key for the {tag} environment lives in the Employee \
         vault under the Finch signing item, not in the repository."
    )
}

/// Deterministically arrange the same `LinkNextError::Conflict` outcome this
/// crate's own `test_a_link_conflict_does_not_fail_or_corrupt_the_losing_turn`
/// (`src/lib.rs`) proves is handled correctly: insert one occurrence, rig its
/// `next_uuid` to a rival directly in the durable store (so the real forward
/// `link_next` call this test drives is guaranteed to lose), then insert a
/// second occurrence in the same session that attempts to link from it. No
/// wall-clock race, per this crate's rule against timing as a correctness
/// oracle.
async fn trigger_link_conflict(db_path: &std::path::Path) -> Result<()> {
    let config = MemoryConfig {
        db_path: db_path.to_path_buf(),
        ..Default::default()
    };
    let memory = MemorySystem::new(config)?;

    let zero_text = substantive("race-turn-zero");
    memory
        .insert_conversation("user", &zero_text, None, Some("sess-race"))
        .await?;

    let zero_uuid: String = {
        let conn = Connection::open(db_path)?;
        conn.query_row(
            "SELECT ro.uuid
             FROM conversations c
             JOIN memory_sources ms ON ms.conversation_id = c.id
             JOIN routing_occurrences ro ON ro.point_id = ms.node_id
             WHERE c.content = ?1",
            params![zero_text],
            |row| row.get(0),
        )
        .context("turn zero must have been projected to a routing occurrence")?
    };

    // Rig the conflict this test is about: link turn zero forward to a rival
    // uuid before the next insert gets a chance to. The next insert's own
    // `prev` resolution still finds turn zero, so it attempts to link from
    // turn zero via `link_next` and loses.
    let rival_uuid = Uuid::new_v4();
    {
        let conn = Connection::open(db_path)?;
        conn.execute(
            "UPDATE routing_occurrences SET next_uuid = ?1 WHERE uuid = ?2",
            params![rival_uuid.to_string(), zero_uuid],
        )?;
    }

    memory
        .insert_conversation(
            "assistant",
            &substantive("race-turn-one"),
            None,
            Some("sess-race"),
        )
        .await
        .context("a losing link_next must not fail the turn that lost it")?;

    Ok(())
}

#[test]
fn test_link_conflict_recovery_message_does_not_reach_a_warn_level_sink() -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("a current-thread runtime to drive the rigged conflict")?;

    // Scenario A: a WARN-and-above sink, standing in for the finch binary's
    // `OutputManagerLayer` in its default (non-debug) configuration, which
    // always forwards WARN/ERROR from a non-`finch::`-prefixed crate like
    // `finch_memory` straight into the transcript. Before this fix (a
    // `tracing::warn!` at the link-conflict site), this buffer would have
    // captured the raw "occurrence chain link lost a race" message.
    let warn_buffer = SharedLogBuffer(Arc::new(Mutex::new(Vec::new())));
    let warn_subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(warn_buffer.clone())
        .finish();
    let warn_db = tempfile::NamedTempFile::new()?;
    tracing::subscriber::with_default(warn_subscriber, || {
        rt.block_on(trigger_link_conflict(warn_db.path()))
    })?;
    let warn_log = String::from_utf8(
        warn_buffer
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .context("captured WARN-level log is valid utf-8")?;
    assert!(
        !warn_log.contains("occurrence chain link lost a race"),
        "invariant: the link-conflict recovery message must never reach a WARN-or-above \
         sink -- it is self-recovering and non-fatal internal detail, not something a \
         non-technical user needs to see verbatim in their transcript (#1383); \
         warn_log={warn_log:?}"
    );

    // Scenario B: the same rigged conflict, captured at DEBUG-and-above, in a
    // fresh store. The message must still be observable there -- suppressing
    // it from the transcript is not the same as deleting the diagnostic.
    let debug_buffer = SharedLogBuffer(Arc::new(Mutex::new(Vec::new())));
    let debug_subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(debug_buffer.clone())
        .finish();
    let debug_db = tempfile::NamedTempFile::new()?;
    tracing::subscriber::with_default(debug_subscriber, || {
        rt.block_on(trigger_link_conflict(debug_db.path()))
    })?;
    let debug_log = String::from_utf8(
        debug_buffer
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .context("captured DEBUG-level log is valid utf-8")?;
    assert!(
        debug_log.contains("occurrence chain link lost a race"),
        "invariant: the link-conflict recovery message must still be observable at debug \
         level -- it is moved out of the transcript, not deleted; debug_log={debug_log:?}"
    );

    Ok(())
}

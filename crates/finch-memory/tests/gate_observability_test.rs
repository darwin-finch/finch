//! Production-boundary proof that the turn-level injection gate's decision
//! is observable in the trace (#1134): a skipped turn logs `info` carrying
//! the turn floor, the best score, and the candidate count, and an allowed
//! turn under an enabled gate logs `debug`.
//!
//! This lives in its own integration binary rather than beside
//! `query_with_sources` because tracing caches per-callsite interest
//! globally within a process: when sibling lib tests that exercise the gate
//! run in parallel with a `with_default` capture, a concurrent
//! no-dispatcher evaluation can re-cache a gate callsite as disabled
//! mid-capture and the gate's line is silently dropped, making the
//! assertion order-dependent and flaky. A dedicated binary gives the
//! capture window the process to itself, so the gate callsites are first
//! evaluated under the capturing subscriber.

use std::io::Write;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use finch_memory::{MemoryConfig, MemorySystem};

/// A stored fact and a probe over part of its vocabulary: the probe
/// retrieves the fact at a weighted score that clears the per-result floor
/// (0.15) -- the leak the turn-level gate exists for -- while sitting
/// strictly below self-similarity.
const GATE_SEED: &str = "The deploy key for the production environment lives \
     in the Employee vault under the Finch signing item, not in the repository.";
const GATE_PROBE: &str = "The deploy key for the production environment lives \
     in the Employee vault";

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

#[test]
fn test_turn_gate_skip_and_allow_decisions_are_logged() -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("a current-thread runtime to drive the captured recalls")?;
    let buffer = SharedLogBuffer(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(buffer.clone())
        .finish();

    // The subscriber is bound per-thread via `with_default` and the
    // current-thread runtime drives the async recalls on that same thread,
    // so everything `query_with_sources` emits inside the window lands in
    // the buffer.
    let (best, allowed_len, skipped_len) = tracing::subscriber::with_default(subscriber, || {
        rt.block_on(async {
            // Enabled well below self-similarity: this turn is allowed,
            // and the allow decision must surface at debug.
            let allow_store = tempfile::NamedTempFile::new()?;
            let memory = MemorySystem::new(MemoryConfig {
                db_path: allow_store.path().to_path_buf(),
                min_turn_relevance_score: Some(0.5),
                ..Default::default()
            })?;
            memory
                .insert_conversation("user", GATE_SEED, None, None)
                .await?;
            let allowed = memory.query_with_sources(GATE_SEED, Some(5)).await?;

            // A gate-off twin measures the probe's best score; the skip
            // twin enables the floor just above it, so the very same
            // recall must skip.
            let measure_store = tempfile::NamedTempFile::new()?;
            let measure = MemorySystem::new(MemoryConfig {
                db_path: measure_store.path().to_path_buf(),
                ..Default::default()
            })?;
            measure
                .insert_conversation("user", GATE_SEED, None, None)
                .await?;
            let baseline = measure.query_with_sources(GATE_PROBE, Some(5)).await?;
            let best = baseline.iter().map(|r| r.score).fold(0.0_f32, f32::max);
            anyhow::ensure!(
                !baseline.is_empty() && best > 0.15,
                "the probe must recall the seed above the 0.15 per-result \
                     floor for the skip to be the gate's doing; count {}, best \
                     score {best}",
                baseline.len()
            );
            let skip_store = tempfile::NamedTempFile::new()?;
            let skip_memory = MemorySystem::new(MemoryConfig {
                db_path: skip_store.path().to_path_buf(),
                min_turn_relevance_score: Some(best + 0.02),
                ..Default::default()
            })?;
            skip_memory
                .insert_conversation("user", GATE_SEED, None, None)
                .await?;
            let skipped = skip_memory.query_with_sources(GATE_PROBE, Some(5)).await?;
            Ok::<(f32, usize, usize), anyhow::Error>((best, allowed.len(), skipped.len()))
        })
    })
    .context("the capture window must run both recalls to completion")?;

    assert!(
        allowed_len >= 1,
        "the allow-half must actually inject under its turn floor for the log \
         assertion to mean anything; got {allowed_len} results"
    );
    assert_eq!(
        skipped_len, 0,
        "the skip-half must actually skip (floor above the observed best \
         score {best}) for the log assertion to mean anything"
    );
    let captured = String::from_utf8_lossy(
        &buffer
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .into_owned();
    assert!(
        captured.contains("turn-level injection gate skipped recall"),
        "a skipped turn must be observable: the gate's info line is missing \
         from the captured trace:\n{captured}"
    );
    assert!(
        captured.contains("turn_floor=")
            && captured.contains("best_score=")
            && captured.contains("candidates="),
        "the skip log must carry the floor, the best score, and the candidate \
         count; captured trace:\n{captured}"
    );
    assert!(
        captured.contains("turn-level injection gate allowed recall"),
        "an allowed turn under an enabled gate must be visible at debug so \
         the gate's allow decision is observable too; captured trace:\n{captured}"
    );
    Ok(())
}

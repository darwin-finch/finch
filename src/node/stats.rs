// Work Statistics — track what this node has done.
//
// Persisted to ~/.finch/work_stats.json and appended to
// ~/.finch/work_log.jsonl for detailed per-query records.
// Foundation for the future worker-network reputation system.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Aggregate statistics for this node's work
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkStats {
    /// Total queries processed (local + forwarded)
    pub queries_processed: u64,
    /// Queries answered by local model
    pub local_queries: u64,
    /// Queries forwarded to teacher API
    pub teacher_queries: u64,
    /// Cumulative response latency in milliseconds
    pub total_latency_ms: u64,
    /// Node start time
    pub started_at: Option<DateTime<Utc>>,
    /// Last query time
    pub last_query_at: Option<DateTime<Utc>>,
}

impl WorkStats {
    pub fn new() -> Self {
        Self {
            started_at: Some(Utc::now()),
            ..Default::default()
        }
    }

    /// Average response latency in milliseconds
    pub fn avg_latency_ms(&self) -> f64 {
        if self.queries_processed == 0 {
            0.0
        } else {
            self.total_latency_ms as f64 / self.queries_processed as f64
        }
    }

    /// Local model usage percentage
    pub fn local_pct(&self) -> f64 {
        if self.queries_processed == 0 {
            0.0
        } else {
            (self.local_queries as f64 / self.queries_processed as f64) * 100.0
        }
    }
}

/// Thread-safe work statistics tracker
#[derive(Debug)]
pub struct WorkTracker {
    queries: AtomicU64,
    local_queries: AtomicU64,
    teacher_queries: AtomicU64,
    latency_ms_total: AtomicU64,
    started_at: DateTime<Utc>,
}

impl WorkTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            queries: AtomicU64::new(0),
            local_queries: AtomicU64::new(0),
            teacher_queries: AtomicU64::new(0),
            latency_ms_total: AtomicU64::new(0),
            started_at: Utc::now(),
        })
    }

    /// Record a completed query
    pub fn record_query(&self, latency_ms: u64, used_local: bool) {
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.latency_ms_total.fetch_add(latency_ms, Ordering::Relaxed);
        if used_local {
            self.local_queries.fetch_add(1, Ordering::Relaxed);
        } else {
            self.teacher_queries.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Snapshot current stats
    pub fn snapshot(&self) -> WorkStats {
        let total = self.queries.load(Ordering::Relaxed);
        WorkStats {
            queries_processed: total,
            local_queries: self.local_queries.load(Ordering::Relaxed),
            teacher_queries: self.teacher_queries.load(Ordering::Relaxed),
            total_latency_ms: self.latency_ms_total.load(Ordering::Relaxed),
            started_at: Some(self.started_at),
            last_query_at: if total > 0 { Some(Utc::now()) } else { None },
        }
    }

    /// Save snapshot to ~/.finch/work_stats.json
    pub fn persist(&self) -> Result<()> {
        let stats = self.snapshot();
        let path = stats_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&stats)
            .context("Failed to serialize work stats")?;
        std::fs::write(&path, json)
            .with_context(|| format!("Failed to write work stats to {}", path.display()))?;
        Ok(())
    }

    /// Load previously persisted stats (for cumulative totals across restarts)
    pub fn load_persisted() -> Result<WorkStats> {
        let path = stats_path()?;
        if !path.exists() {
            return Ok(WorkStats::new());
        }
        let raw = std::fs::read_to_string(&path)
            .context("Failed to read work stats")?;
        serde_json::from_str(&raw).context("Failed to parse work stats")
    }
}


fn stats_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    Ok(home.join(".finch").join("work_stats.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_work_tracker_records_queries() {
        let tracker = WorkTracker::new();
        tracker.record_query(100, true);
        tracker.record_query(200, false);
        tracker.record_query(150, true);

        let snap = tracker.snapshot();
        assert_eq!(snap.queries_processed, 3);
        assert_eq!(snap.local_queries, 2);
        assert_eq!(snap.teacher_queries, 1);
        assert_eq!(snap.total_latency_ms, 450);
    }

    #[test]
    fn test_avg_latency() {
        let tracker = WorkTracker::new();
        tracker.record_query(100, true);
        tracker.record_query(200, true);

        let snap = tracker.snapshot();
        assert_eq!(snap.avg_latency_ms(), 150.0);
    }

    #[test]
    fn test_local_pct() {
        let tracker = WorkTracker::new();
        tracker.record_query(100, true);
        tracker.record_query(100, false);

        let snap = tracker.snapshot();
        assert_eq!(snap.local_pct(), 50.0);
    }

    #[test]
    fn test_empty_stats() {
        let tracker = WorkTracker::new();
        let snap = tracker.snapshot();
        assert_eq!(snap.queries_processed, 0);
        assert_eq!(snap.avg_latency_ms(), 0.0);
        assert_eq!(snap.local_pct(), 0.0);
    }
}

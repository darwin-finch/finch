// Metrics logger

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use super::types::{ProviderKind, RequestMetric, RequestOutcome, WireAdherenceMetric};

/// Appends and reads source-free metric rows under one directory.
///
/// The directory is always chosen by the caller. Nothing in this type, or in
/// the code that records through it, derives a path from the home directory:
/// production passes `Config::metrics_dir`, and a test passes its own
/// temporary directory (issue #1629, test runs filling the user's real
/// `/metrics` report with fixture providers).
#[derive(Debug, Clone)]
pub struct MetricsLogger {
    metrics_dir: PathBuf,
}

impl MetricsLogger {
    pub fn new(metrics_dir: PathBuf) -> Result<Self> {
        // Create metrics directory if it doesn't exist
        fs::create_dir_all(&metrics_dir).with_context(|| {
            format!(
                "Failed to create metrics directory: {}",
                metrics_dir.display()
            )
        })?;

        Ok(Self { metrics_dir })
    }

    /// The directory this logger appends to and reads from.
    pub fn metrics_dir(&self) -> &std::path::Path {
        &self.metrics_dir
    }

    /// Log a request metric to today's JSONL file
    pub fn log(&self, metric: &RequestMetric) -> Result<()> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let log_file = self.metrics_dir.join(format!("{}.jsonl", today));

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .with_context(|| format!("Failed to open metrics log: {}", log_file.display()))?;

        let json = serde_json::to_string(metric).context("Failed to serialize metric")?;

        writeln!(file, "{}", json).context("Failed to write metric to log")?;

        Ok(())
    }

    /// Append one provider-wire conformance result without retaining prompt or
    /// source text. This uses a separate file so legacy routing metrics remain
    /// backwards-compatible.
    pub fn log_wire(&self, metric: &WireAdherenceMetric) -> Result<()> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let log_file = self.metrics_dir.join(format!("wire-{today}.jsonl"));
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .with_context(|| format!("Failed to open wire metrics log: {}", log_file.display()))?;
        let json = serde_json::to_string(metric).context("Failed to serialize wire metric")?;
        writeln!(file, "{json}").context("Failed to write wire metric")?;
        Ok(())
    }

    pub fn read_wire_metrics(&self, date: &str) -> Result<Vec<WireAdherenceMetric>> {
        let log_file = self.metrics_dir.join(format!("wire-{date}.jsonl"));
        if !log_file.exists() {
            return Ok(Vec::new());
        }
        let contents = fs::read_to_string(&log_file)
            .with_context(|| format!("Failed to read wire metrics: {}", log_file.display()))?;
        contents
            .lines()
            .filter(|line| !line.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to parse wire metrics")
    }

    /// Hash a query for privacy (SHA256)
    pub fn hash_query(query: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(query.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Read request metrics for a specific date.
    ///
    /// A line that is not a request row at all (truncated by a crash, or
    /// written by something else) is skipped rather than failing the read,
    /// so one bad line cannot blank the whole report.
    pub fn read_metrics(&self, date: &str) -> Result<Vec<RequestMetric>> {
        Ok(self.read_metrics_counting_unreadable(date)?.0)
    }

    fn read_metrics_counting_unreadable(&self, date: &str) -> Result<(Vec<RequestMetric>, usize)> {
        let log_file = self.metrics_dir.join(format!("{}.jsonl", date));

        if !log_file.exists() {
            return Ok((Vec::new(), 0));
        }

        let contents = fs::read_to_string(&log_file)
            .with_context(|| format!("Failed to read metrics log: {}", log_file.display()))?;

        let mut metrics = Vec::new();
        let mut unreadable = 0;
        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str::<RequestMetric>(line) {
                Ok(metric) => metrics.push(metric),
                Err(_) => unreadable += 1,
            }
        }
        Ok((metrics, unreadable))
    }

    /// Summarise the requests recorded in the 24 hours ending now.
    pub fn request_summary_last_24_hours(&self) -> Result<RequestSummary> {
        self.request_summary_for_24_hours_before(Utc::now())
    }

    /// Summarise the requests recorded in the 24 hours ending at `now`.
    ///
    /// Files are named by UTC date, so the window spans the file for `now`'s
    /// date and the one before it; rows outside the window are left out.
    pub fn request_summary_for_24_hours_before(
        &self,
        now: DateTime<Utc>,
    ) -> Result<RequestSummary> {
        let cutoff = now - chrono::Duration::hours(24);
        let mut summary = RequestSummary::default();
        let mut groups = BTreeMap::<(String, String, Option<ProviderKind>), GroupTotals>::new();
        let mut completed_ms_total: u64 = 0;

        for date in [cutoff.date_naive(), now.date_naive()] {
            let date = date.format("%Y-%m-%d").to_string();
            let (metrics, unreadable) = self.read_metrics_counting_unreadable(&date)?;
            summary.unreadable_rows += unreadable;
            for metric in metrics {
                if metric.timestamp <= cutoff || metric.timestamp > now {
                    continue;
                }
                let outcome = metric.effective_outcome();
                let kind = metric.effective_provider_kind();
                summary.total += 1;
                match kind {
                    Some(ProviderKind::Local) => summary.local += 1,
                    Some(ProviderKind::Cloud) => summary.cloud += 1,
                    None => summary.unclassified += 1,
                }
                let group = groups
                    .entry((
                        metric
                            .provider
                            .unwrap_or_else(|| UNRECORDED_IDENTITY.to_string()),
                        metric
                            .model
                            .unwrap_or_else(|| UNRECORDED_IDENTITY.to_string()),
                        kind,
                    ))
                    .or_default();
                group.total += 1;
                match outcome {
                    RequestOutcome::Completed => {
                        summary.completed += 1;
                        completed_ms_total += metric.response_time_ms;
                        group.completed += 1;
                        group.completed_ms_total += metric.response_time_ms;
                    }
                    RequestOutcome::Failed => {
                        summary.failed += 1;
                        group.failed += 1;
                    }
                    RequestOutcome::Cancelled => {
                        summary.cancelled += 1;
                        group.cancelled += 1;
                    }
                }
            }
        }

        summary.avg_completed_ms = average(completed_ms_total, summary.completed);
        summary.groups = groups
            .into_iter()
            .map(|((provider, model, kind), totals)| RequestGroupSummary {
                provider,
                model,
                kind,
                total: totals.total,
                completed: totals.completed,
                failed: totals.failed,
                cancelled: totals.cancelled,
                avg_completed_ms: average(totals.completed_ms_total, totals.completed),
            })
            .collect();
        Ok(summary)
    }
}

/// Shown for a row that did not record which provider entry or model served
/// it: the daemon's HTTP message route and builds before issue #1629.
pub const UNRECORDED_IDENTITY: &str = "not recorded";

fn average(total_ms: u64, count: usize) -> Option<u64> {
    (count > 0).then(|| total_ms / count as u64)
}

#[derive(Default)]
struct GroupTotals {
    total: usize,
    completed: usize,
    failed: usize,
    cancelled: usize,
    completed_ms_total: u64,
}

/// Requests in one 24-hour window. Every count is a count of recorded rows;
/// nothing here is inferred or defaulted, so an empty window is `total == 0`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RequestSummary {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
    /// Served by a `Local` provider entry.
    pub local: usize,
    /// Served by any other provider entry.
    pub cloud: usize,
    /// Rows that recorded neither a provider kind nor a routing decision.
    pub unclassified: usize,
    /// Mean duration of completed requests; `None` when none completed.
    pub avg_completed_ms: Option<u64>,
    /// Lines in the window's files that were not request rows.
    pub unreadable_rows: usize,
    /// Per provider entry and model, ordered by provider then model.
    pub groups: Vec<RequestGroupSummary>,
}

/// One provider entry and model within a [`RequestSummary`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestGroupSummary {
    pub provider: String,
    pub model: String,
    pub kind: Option<ProviderKind>,
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub avg_completed_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_query() {
        let hash1 = MetricsLogger::hash_query("Hello");
        let hash2 = MetricsLogger::hash_query("Hello");
        let hash3 = MetricsLogger::hash_query("World");

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
        assert_eq!(hash1.len(), 64); // SHA256 produces 64 hex chars
    }

    #[test]
    fn wire_metrics_use_a_separate_source_free_jsonl_file() {
        let dir = tempfile::tempdir().unwrap();
        let logger = MetricsLogger::new(dir.path().to_path_buf()).unwrap();
        let metric = WireAdherenceMetric::first_pass("xai", "grok", "interactive");
        logger.log_wire(&metric).unwrap();
        let today = Utc::now().format("%Y-%m-%d").to_string();
        assert_eq!(logger.read_wire_metrics(&today).unwrap(), vec![metric]);
        assert!(!dir.path().join(format!("{today}.jsonl")).exists());
    }

    fn turn_at(
        timestamp: DateTime<Utc>,
        provider: &str,
        kind: ProviderKind,
        outcome: RequestOutcome,
        ms: u64,
    ) -> RequestMetric {
        let mut metric =
            RequestMetric::turn(provider, "model-a", Some(kind), outcome, "interactive", ms);
        metric.timestamp = timestamp;
        metric
    }

    fn append_raw(dir: &std::path::Path, date: &str, line: &str) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{date}.jsonl")))
            .unwrap();
        writeln!(file, "{line}").unwrap();
    }

    #[test]
    fn test_request_summary_counts_outcomes_kinds_and_groups_within_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let logger = MetricsLogger::new(dir.path().to_path_buf()).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-10-04T06:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let rows = [
            // Yesterday's file, inside the window.
            turn_at(
                now - chrono::Duration::hours(10),
                "cloud-entry",
                ProviderKind::Cloud,
                RequestOutcome::Completed,
                1000,
            ),
            // Yesterday's file, outside the window.
            turn_at(
                now - chrono::Duration::hours(25),
                "cloud-entry",
                ProviderKind::Cloud,
                RequestOutcome::Completed,
                9999,
            ),
            // Today's file.
            turn_at(
                now - chrono::Duration::hours(1),
                "cloud-entry",
                ProviderKind::Cloud,
                RequestOutcome::Completed,
                3000,
            ),
            turn_at(
                now - chrono::Duration::hours(1),
                "cloud-entry",
                ProviderKind::Cloud,
                RequestOutcome::Failed,
                70,
            ),
            turn_at(
                now - chrono::Duration::hours(1),
                "local-entry",
                ProviderKind::Local,
                RequestOutcome::Cancelled,
                80,
            ),
        ];
        for row in &rows {
            let date = row.timestamp.format("%Y-%m-%d").to_string();
            append_raw(dir.path(), &date, &serde_json::to_string(row).unwrap());
        }

        let summary = logger.request_summary_for_24_hours_before(now).unwrap();
        assert_eq!(
            (
                summary.total,
                summary.completed,
                summary.failed,
                summary.cancelled
            ),
            (4, 2, 1, 1),
            "the window must hold exactly the four rows newer than 24 hours; summary={summary:?}"
        );
        assert_eq!(
            (summary.cloud, summary.local, summary.unclassified),
            (3, 1, 0),
            "local versus cloud must follow each row's provider kind; summary={summary:?}"
        );
        assert_eq!(
            summary.avg_completed_ms,
            Some(2000),
            "average duration must cover completed requests only; summary={summary:?}"
        );
        assert_eq!(
            summary.groups,
            vec![
                RequestGroupSummary {
                    provider: "cloud-entry".into(),
                    model: "model-a".into(),
                    kind: Some(ProviderKind::Cloud),
                    total: 3,
                    completed: 2,
                    failed: 1,
                    cancelled: 0,
                    avg_completed_ms: Some(2000),
                },
                RequestGroupSummary {
                    provider: "local-entry".into(),
                    model: "model-a".into(),
                    kind: Some(ProviderKind::Local),
                    total: 1,
                    completed: 0,
                    failed: 0,
                    cancelled: 1,
                    avg_completed_ms: None,
                },
            ],
            "groups must be keyed by provider entry and model"
        );
    }

    #[test]
    fn test_request_summary_reads_routing_rows_and_skips_unreadable_lines() {
        let dir = tempfile::tempdir().unwrap();
        let logger = MetricsLogger::new(dir.path().to_path_buf()).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-10-04T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // The shape the daemon's HTTP message route and older builds write.
        append_raw(
            dir.path(),
            "2026-10-04",
            r#"{"timestamp":"2026-10-04T11:00:00Z","query_hash":"abc","routing_decision":"forward","pattern_id":null,"confidence":null,"forward_reason":null,"response_time_ms":500,"comparison":{"quality_score":1.0,"similarity_score":null,"divergence":null},"router_confidence":null,"validator_confidence":null}"#,
        );
        append_raw(
            dir.path(),
            "2026-10-04",
            r#"{"timestamp":"2026-10-04T11:30:00Z","added_later":true}"#,
        );
        append_raw(dir.path(), "2026-10-04", "{ truncated");

        let summary = logger.request_summary_for_24_hours_before(now).unwrap();
        assert_eq!(
            (
                summary.total,
                summary.completed,
                summary.cloud,
                summary.unclassified
            ),
            (2, 2, 1, 1),
            "a routing row and a sparse row must both be counted; summary={summary:?}"
        );
        assert_eq!(
            summary.unreadable_rows, 1,
            "a line that is not a request row must be skipped and counted, not fail the read; \
             summary={summary:?}"
        );
        assert!(
            summary
                .groups
                .iter()
                .all(|group| group.provider == UNRECORDED_IDENTITY),
            "rows without a provider must be reported as not recorded, not guessed; \
             summary={summary:?}"
        );
    }
}

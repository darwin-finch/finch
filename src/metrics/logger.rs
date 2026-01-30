// Metrics logger

use anyhow::{Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use super::types::RequestMetric;

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

    /// Hash a query for privacy (SHA256)
    pub fn hash_query(query: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(query.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Read metrics for a specific date
    pub fn read_metrics(&self, date: &str) -> Result<Vec<RequestMetric>> {
        let log_file = self.metrics_dir.join(format!("{}.jsonl", date));

        if !log_file.exists() {
            return Ok(Vec::new());
        }

        let contents = fs::read_to_string(&log_file)
            .with_context(|| format!("Failed to read metrics log: {}", log_file.display()))?;

        let metrics: Vec<RequestMetric> = contents
            .lines()
            .filter(|line| !line.is_empty())
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to parse metrics")?;

        Ok(metrics)
    }

    /// Get summary statistics for today
    pub fn get_today_summary(&self) -> Result<MetricsSummary> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let metrics = self.read_metrics(&today)?;

        let total = metrics.len();
        let local_count = metrics
            .iter()
            .filter(|m| m.routing_decision == "local")
            .count();
        let forward_count = total - local_count;

        let crisis_count = metrics
            .iter()
            .filter(|m| m.forward_reason.as_deref() == Some("crisis"))
            .count();

        let no_match_count = metrics
            .iter()
            .filter(|m| m.forward_reason.as_deref() == Some("no_match"))
            .count();

        let avg_local_time = if local_count > 0 {
            metrics
                .iter()
                .filter(|m| m.routing_decision == "local")
                .map(|m| m.response_time_ms)
                .sum::<u64>()
                / local_count as u64
        } else {
            0
        };

        let avg_forward_time = if forward_count > 0 {
            metrics
                .iter()
                .filter(|m| m.routing_decision == "forward")
                .map(|m| m.response_time_ms)
                .sum::<u64>()
                / forward_count as u64
        } else {
            0
        };

        // Count top patterns
        let mut pattern_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for metric in &metrics {
            if let Some(pattern_id) = &metric.pattern_id {
                *pattern_counts.entry(pattern_id.clone()).or_insert(0) += 1;
            }
        }

        let mut top_patterns: Vec<(String, usize)> = pattern_counts.into_iter().collect();
        top_patterns.sort_by(|a, b| b.1.cmp(&a.1));
        top_patterns.truncate(3);

        Ok(MetricsSummary {
            total,
            local_count,
            forward_count,
            crisis_count,
            no_match_count,
            avg_local_time,
            avg_forward_time,
            top_patterns,
        })
    }
}

#[derive(Debug)]
pub struct MetricsSummary {
    pub total: usize,
    pub local_count: usize,
    pub forward_count: usize,
    pub crisis_count: usize,
    pub no_match_count: usize,
    pub avg_local_time: u64,
    pub avg_forward_time: u64,
    pub top_patterns: Vec<(String, usize)>,
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
}

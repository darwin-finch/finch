// Feedback system for response quality tracking
//
// Users can rate responses to collect training data for LoRA fine-tuning.
// Feedback is logged to ~/.finch/feedback.jsonl

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

/// Feedback rating for a response
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackRating {
    /// Positive feedback (good response)
    Good,
    /// Negative feedback (bad response)
    Bad,
}

impl FeedbackRating {
    /// Get the LoRA training weight for this feedback
    pub fn training_weight(&self) -> f64 {
        match self {
            FeedbackRating::Good => 1.0, // Normal weight (1x)
            FeedbackRating::Bad => 10.0, // High weight (10x) - learn from mistakes
        }
    }

    /// Get display string
    pub fn display_str(&self) -> &'static str {
        match self {
            FeedbackRating::Good => "👍 Good",
            FeedbackRating::Bad => "👎 Bad",
        }
    }
}

/// Feedback entry logged to JSONL
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEntry {
    /// Timestamp (Unix timestamp)
    pub timestamp: u64,
    /// User query that generated the response
    pub query: String,
    /// Response that was rated
    pub response: String,
    /// Feedback rating
    pub rating: FeedbackRating,
    /// Training weight (derived from rating)
    pub weight: f64,
    /// Optional note from user
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl FeedbackEntry {
    /// Create a new feedback entry
    pub fn new(query: String, response: String, rating: FeedbackRating) -> Self {
        Self {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            query,
            response,
            weight: rating.training_weight(),
            rating,
            note: None,
        }
    }

    /// Add a note to the feedback
    pub fn with_note(mut self, note: String) -> Self {
        self.note = Some(note);
        self
    }
}

/// Feedback logger - writes feedback to JSONL file
pub struct FeedbackLogger {
    file_path: PathBuf,
}

impl FeedbackLogger {
    /// Create a new feedback logger
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;

        let finch_dir = home.join(".finch");
        fs::create_dir_all(&finch_dir).context("Failed to create ~/.finch directory")?;

        let file_path = finch_dir.join("feedback.jsonl");

        Ok(Self { file_path })
    }

    /// Log a feedback entry
    pub fn log(&self, entry: &FeedbackEntry) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file_path)
            .with_context(|| {
                format!("Failed to open feedback log: {}", self.file_path.display())
            })?;

        let json = serde_json::to_string(entry).context("Failed to serialize feedback entry")?;

        writeln!(file, "{}", json).context("Failed to write feedback entry")?;

        Ok(())
    }

    /// Get the path to the feedback log
    pub fn path(&self) -> &PathBuf {
        &self.file_path
    }

    /// Count total feedback entries
    pub fn count_entries(&self) -> Result<usize> {
        if !self.file_path.exists() {
            return Ok(0);
        }

        let contents =
            fs::read_to_string(&self.file_path).context("Failed to read feedback log")?;

        Ok(contents.lines().filter(|l| !l.trim().is_empty()).count())
    }

    /// Load all feedback entries
    pub fn load_all(&self) -> Result<Vec<FeedbackEntry>> {
        if !self.file_path.exists() {
            return Ok(Vec::new());
        }

        let contents =
            fs::read_to_string(&self.file_path).context("Failed to read feedback log")?;

        let mut entries = Vec::new();
        for line in contents.lines() {
            if line.trim().is_empty() {
                continue;
            }

            let entry: FeedbackEntry =
                serde_json::from_str(line).context("Failed to parse feedback entry")?;
            entries.push(entry);
        }

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feedback_rating_weights() {
        assert_eq!(FeedbackRating::Good.training_weight(), 1.0);
        assert_eq!(FeedbackRating::Bad.training_weight(), 10.0);
    }

    #[test]
    fn test_feedback_entry_creation() {
        let entry = FeedbackEntry::new(
            "What is 2+2?".to_string(),
            "4".to_string(),
            FeedbackRating::Good,
        );

        assert_eq!(entry.query, "What is 2+2?");
        assert_eq!(entry.response, "4");
        assert_eq!(entry.rating, FeedbackRating::Good);
        assert_eq!(entry.weight, 1.0);
        assert!(entry.note.is_none());
    }

    #[test]
    fn test_feedback_entry_with_note() {
        let entry = FeedbackEntry::new(
            "Test".to_string(),
            "Response".to_string(),
            FeedbackRating::Bad,
        )
        .with_note("Wrong algorithm".to_string());

        assert_eq!(entry.note, Some("Wrong algorithm".to_string()));
    }
}

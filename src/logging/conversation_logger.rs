// Conversation logger for LoRA training data collection

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use tracing::debug;

/// Feedback type for weighted training
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Feedback {
    Good,
    Bad,
    Critical, // For high-weight corrections
}

impl Feedback {
    /// Get the training weight for this feedback type
    pub fn weight(&self) -> f64 {
        match self {
            Feedback::Good => 1.0,       // Normal weight
            Feedback::Bad => 3.0,        // Medium weight
            Feedback::Critical => 10.0,  // High weight
        }
    }
}

/// Token usage statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub total_tokens: usize,
}

/// A single logged conversation entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Unique ID for this entry
    pub id: String,

    /// When this interaction occurred
    pub timestamp: DateTime<Utc>,

    /// User's query
    pub query: String,

    /// AI's response
    pub response: String,

    /// Which LLM generated the response (e.g., "Local Qwen-7B", "Claude Sonnet")
    pub model: String,

    /// Which tools were used during execution
    pub tools_used: Vec<String>,

    /// Token usage (if available)
    #[serde(default)]
    pub tokens: TokenUsage,

    /// User feedback (None until user provides it)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<Feedback>,

    /// Training weight (1.0 = normal, 3.0 = medium, 10.0 = high)
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_weight() -> f64 {
    1.0
}

impl LogEntry {
    /// Create a new log entry
    pub fn new(
        query: String,
        response: String,
        model: String,
        tools_used: Vec<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            query,
            response,
            model,
            tools_used,
            tokens: TokenUsage::default(),
            feedback: None,
            weight: 1.0,
        }
    }

    /// Set feedback and update weight
    pub fn set_feedback(&mut self, feedback: Feedback) {
        self.weight = feedback.weight();
        self.feedback = Some(feedback);
    }
}

/// Conversation logger that writes to JSONL
pub struct ConversationLogger {
    log_path: PathBuf,
    buffer: Vec<LogEntry>,
    flush_threshold: usize,
}

impl ConversationLogger {
    /// Create a new logger
    pub fn new(log_path: PathBuf) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)
                .context("Failed to create logging directory")?;
        }

        Ok(Self {
            log_path,
            buffer: Vec::new(),
            flush_threshold: 10, // Flush every 10 entries
        })
    }

    /// Log a conversation interaction
    pub async fn log_interaction(
        &mut self,
        query: &str,
        response: &str,
        model: &str,
        tools_used: &[String],
    ) -> Result<String> {
        let entry = LogEntry::new(
            query.to_string(),
            response.to_string(),
            model.to_string(),
            tools_used.to_vec(),
        );

        let id = entry.id.clone();
        self.buffer.push(entry);

        // Auto-flush if threshold reached
        if self.buffer.len() >= self.flush_threshold {
            self.flush().await?;
        }

        Ok(id)
    }

    /// Flush buffered entries to disk
    pub async fn flush(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        debug!("Flushing {} log entries to disk", self.buffer.len());

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .context("Failed to open log file")?;

        for entry in &self.buffer {
            let json = serde_json::to_string(entry)
                .context("Failed to serialize log entry")?;
            writeln!(file, "{}", json)
                .context("Failed to write log entry")?;
        }

        self.buffer.clear();
        Ok(())
    }

    /// Add feedback to a logged entry
    pub async fn add_feedback(&mut self, entry_id: &str, feedback: Feedback) -> Result<()> {
        // Read all entries from file
        let contents = std::fs::read_to_string(&self.log_path)
            .context("Failed to read log file")?;

        let mut updated_entries = Vec::new();
        let mut found = false;

        for line in contents.lines() {
            if line.trim().is_empty() {
                continue;
            }

            let mut entry: LogEntry = serde_json::from_str(line)
                .context("Failed to parse log entry")?;

            if entry.id == entry_id {
                entry.set_feedback(feedback.clone());
                found = true;
                debug!("Updated feedback for entry {}: {:?}", entry_id, feedback);
            }

            updated_entries.push(entry);
        }

        if !found {
            anyhow::bail!("Log entry {} not found", entry_id);
        }

        // Write back all entries
        let mut file = File::create(&self.log_path)
            .context("Failed to open log file for writing")?;

        for entry in updated_entries {
            let json = serde_json::to_string(&entry)?;
            writeln!(file, "{}", json)?;
        }

        Ok(())
    }

    /// Get the log file path
    pub fn path(&self) -> &PathBuf {
        &self.log_path
    }
}

impl Drop for ConversationLogger {
    fn drop(&mut self) {
        // Flush on drop
        if !self.buffer.is_empty() {
            if let Err(e) = futures::executor::block_on(self.flush()) {
                eprintln!("Failed to flush logs on drop: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_log_and_flush() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        let mut logger = ConversationLogger::new(path.clone()).unwrap();

        let id = logger
            .log_interaction("What is 2+2?", "4", "Local Qwen", &[])
            .await
            .unwrap();

        logger.flush().await.unwrap();

        // Read back
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("What is 2+2?"));
        assert!(contents.contains("\"model\":\"Local Qwen\""));
    }

    #[tokio::test]
    async fn test_add_feedback() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        let mut logger = ConversationLogger::new(path.clone()).unwrap();

        let id = logger
            .log_interaction("Test query", "Test response", "Model", &[])
            .await
            .unwrap();

        logger.flush().await.unwrap();

        // Add feedback
        logger.add_feedback(&id, Feedback::Critical).await.unwrap();

        // Read back and verify weight
        let contents = std::fs::read_to_string(&path).unwrap();
        let entry: LogEntry = serde_json::from_str(contents.lines().next().unwrap()).unwrap();

        assert_eq!(entry.weight, 10.0);
        assert!(matches!(entry.feedback, Some(Feedback::Critical)));
    }
}

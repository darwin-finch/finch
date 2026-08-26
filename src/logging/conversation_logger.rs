// Legacy conversation logger API.
//
// Automatic query/response persistence was previously used as implicit LoRA
// training-data collection. That behavior is disabled: only explicit feedback
// may be persisted through `FeedbackLogger`.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Returned by every legacy conversation persistence operation.
#[derive(Debug)]
pub struct AutomaticConversationLoggingDisabled;

impl std::fmt::Display for AutomaticConversationLoggingDisabled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "Automatic conversation logging is disabled; submit explicit feedback instead",
        )
    }
}

impl std::error::Error for AutomaticConversationLoggingDisabled {}

/// Historical feedback type retained for serialized-record compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Feedback {
    Good,
    Bad,
    Critical, // For high-weight corrections
}

impl Feedback {
    /// Get the historical compatibility weight for this feedback type.
    ///
    /// Automatic training is disabled; returning this value does not enqueue
    /// or initiate training.
    pub fn weight(&self) -> f64 {
        match self {
            Feedback::Good => 1.0,      // Normal weight
            Feedback::Bad => 3.0,       // Medium weight
            Feedback::Critical => 10.0, // High weight
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

/// Historical conversation-entry shape retained for API compatibility.
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

    /// Historical weight (1.0 = normal, 3.0 = medium, 10.0 = high).
    /// This value is not consumed by an automatic trainer.
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_weight() -> f64 {
    1.0
}

impl LogEntry {
    /// Create a new log entry
    pub fn new(query: String, response: String, model: String, tools_used: Vec<String>) -> Self {
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

    /// Set legacy feedback metadata and update its compatibility weight.
    pub fn set_feedback(&mut self, feedback: Feedback) {
        self.weight = feedback.weight();
        self.feedback = Some(feedback);
    }
}

/// Compatibility handle for the disabled automatic conversation collector.
pub struct ConversationLogger {
    log_path: PathBuf,
}

impl ConversationLogger {
    /// Create a new logger
    pub fn new(log_path: PathBuf) -> Result<Self> {
        // Construction is deliberately side-effect free. In particular, an
        // ordinary REPL query must not create a legacy conversations.jsonl.
        Ok(Self { log_path })
    }

    /// Log a conversation interaction
    pub async fn log_interaction(
        &mut self,
        _query: &str,
        _response: &str,
        _model: &str,
        _tools_used: &[String],
    ) -> Result<String> {
        Err(AutomaticConversationLoggingDisabled.into())
    }

    /// Flush buffered entries to disk
    pub async fn flush(&mut self) -> Result<()> {
        Err(AutomaticConversationLoggingDisabled.into())
    }

    /// Add feedback to a logged entry
    pub async fn add_feedback(&mut self, _entry_id: &str, _feedback: Feedback) -> Result<()> {
        Err(AutomaticConversationLoggingDisabled.into())
    }

    /// Get the log file path
    pub fn path(&self) -> &PathBuf {
        &self.log_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_construction_does_not_create_legacy_storage() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("legacy/conversations.jsonl");

        let logger = ConversationLogger::new(path.clone()).unwrap();

        assert_eq!(logger.path(), &path);
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn test_automatic_log_flush_and_feedback_fail_closed_without_mutation() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("conversations.jsonl");
        std::fs::write(&path, "preserved legacy data\n").unwrap();
        let before = std::fs::read(&path).unwrap();

        let mut logger = ConversationLogger::new(path.clone()).unwrap();

        let log_error = logger
            .log_interaction("What is 2+2?", "4", "Local Qwen", &[])
            .await
            .unwrap_err();
        assert!(log_error
            .downcast_ref::<AutomaticConversationLoggingDisabled>()
            .is_some());
        assert!(logger.flush().await.is_err());
        assert!(logger
            .add_feedback("legacy-id", Feedback::Critical)
            .await
            .is_err());
        drop(logger);

        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

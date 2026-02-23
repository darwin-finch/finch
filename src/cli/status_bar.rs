// Status Bar - Multi-line status display at bottom of terminal
//
// This module manages the status bar area that shows:
// - Training statistics
// - Download progress
// - Operation status
//
// Supports dynamic addition/removal of status lines.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Types of status lines
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StatusLineType {
    /// Session label shown permanently (e.g. "◆ swift-falcon · ~/repos/finch")
    SessionLabel,
    /// Memory context: engine type + recall info ("🧠 neural · 142 memories · recalled 3")
    MemoryContext,
    /// Conversation topic derived from MemTree overall centroid ("📋 <topic>")
    ConversationTopic,
    /// Conversation focus derived from MemTree recency centroid ("   └─ now: <focus>")
    ConversationFocus,
    /// Live query statistics (tokens, latency, model)
    LiveStats,
    /// Training statistics (queries, local%, quality)
    TrainingStats,
    /// Model download progress
    DownloadProgress,
    /// Current operation status
    OperationStatus,
    /// Contextual suggestions (like Claude Code)
    Suggestions,
    /// Auto-compaction percentage (displayed on right side)
    CompactionPercent,
    /// Custom status line with ID
    Custom(String),
}

/// A single status line
#[derive(Debug, Clone)]
pub struct StatusLine {
    /// Type of status line
    pub line_type: StatusLineType,
    /// Content to display
    pub content: String,
}

/// Thread-safe status bar manager
pub struct StatusBar {
    /// Active status lines (keyed by type)
    lines: Arc<RwLock<HashMap<StatusLineType, String>>>,
}

impl StatusBar {
    /// Create a new StatusBar
    pub fn new() -> Self {
        Self {
            lines: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add or update a status line
    pub fn update_line(&self, line_type: StatusLineType, content: impl Into<String>) {
        let mut lines = self.lines.write().unwrap();
        lines.insert(line_type, content.into());
    }

    /// Remove a status line
    pub fn remove_line(&self, line_type: &StatusLineType) {
        let mut lines = self.lines.write().unwrap();
        lines.remove(line_type);
    }

    /// Clear all status lines
    pub fn clear(&self) {
        let mut lines = self.lines.write().unwrap();
        lines.clear();
    }

    /// Get all status lines in a consistent order
    pub fn get_lines(&self) -> Vec<StatusLine> {
        let lines = self.lines.read().unwrap();

        // Order: SessionLabel, MemoryContext, LiveStats, TrainingStats, DownloadProgress,
        //        OperationStatus, then Custom
        let mut result = Vec::new();

        // Add in preferred order
        if let Some(content) = lines.get(&StatusLineType::SessionLabel) {
            result.push(StatusLine {
                line_type: StatusLineType::SessionLabel,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::MemoryContext) {
            result.push(StatusLine {
                line_type: StatusLineType::MemoryContext,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::ConversationTopic) {
            result.push(StatusLine {
                line_type: StatusLineType::ConversationTopic,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::ConversationFocus) {
            result.push(StatusLine {
                line_type: StatusLineType::ConversationFocus,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::LiveStats) {
            result.push(StatusLine {
                line_type: StatusLineType::LiveStats,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::TrainingStats) {
            result.push(StatusLine {
                line_type: StatusLineType::TrainingStats,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::DownloadProgress) {
            result.push(StatusLine {
                line_type: StatusLineType::DownloadProgress,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::OperationStatus) {
            result.push(StatusLine {
                line_type: StatusLineType::OperationStatus,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::Suggestions) {
            result.push(StatusLine {
                line_type: StatusLineType::Suggestions,
                content: content.clone(),
            });
        }

        if let Some(content) = lines.get(&StatusLineType::CompactionPercent) {
            result.push(StatusLine {
                line_type: StatusLineType::CompactionPercent,
                content: content.clone(),
            });
        }

        // Add custom lines (sorted by ID for consistency)
        let mut custom_lines: Vec<_> = lines
            .iter()
            .filter_map(|(k, v)| {
                if let StatusLineType::Custom(id) = k {
                    Some((id.clone(), v.clone()))
                } else {
                    None
                }
            })
            .collect();
        custom_lines.sort_by(|a, b| a.0.cmp(&b.0));

        for (id, content) in custom_lines {
            result.push(StatusLine {
                line_type: StatusLineType::Custom(id),
                content,
            });
        }

        result
    }

    /// Get the number of active status lines
    pub fn len(&self) -> usize {
        self.lines.read().unwrap().len()
    }

    /// Check if there are any status lines
    pub fn is_empty(&self) -> bool {
        self.lines.read().unwrap().is_empty()
    }

    /// Get status content as a string (for change detection)
    pub fn get_status(&self) -> String {
        let lines = self.get_lines();
        lines
            .iter()
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Render the status bar as a multi-line string
    pub fn render(&self) -> String {
        let lines = self.get_lines();

        if lines.is_empty() {
            return String::new();
        }

        lines
            .iter()
            .map(|line| line.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Update training stats line
    pub fn update_training_stats(
        &self,
        total_queries: usize,
        local_percentage: f64,
        quality_score: f64,
    ) {
        let content = format!(
            "Training: {} queries | Local: {:.0}% | Quality: {:.2}",
            total_queries,
            local_percentage * 100.0,
            quality_score
        );
        self.update_line(StatusLineType::TrainingStats, content);
    }

    /// Update download progress line
    pub fn update_download_progress(
        &self,
        model_name: impl Into<String>,
        percentage: f64,
        downloaded: u64,
        total: u64,
    ) {
        let model_name = model_name.into();
        let bar_width = 20;
        let filled = (percentage * bar_width as f64) as usize;
        let empty = bar_width - filled;

        let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(empty));

        let content = format!(
            "Downloading {}: {} {:.0}% ({:.1}GB/{:.1}GB)",
            model_name,
            bar,
            percentage * 100.0,
            downloaded as f64 / 1_000_000_000.0,
            total as f64 / 1_000_000_000.0
        );

        self.update_line(StatusLineType::DownloadProgress, content);
    }

    /// Update operation status line
    pub fn update_operation(&self, operation: impl Into<String>) {
        self.update_line(StatusLineType::OperationStatus, operation.into());
    }

    /// Clear operation status (shorthand)
    pub fn clear_operation(&self) {
        self.remove_line(&StatusLineType::OperationStatus);
    }

    /// Update live query statistics
    pub fn update_live_stats(
        &self,
        model: impl Into<String>,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
        latency_ms: Option<u64>,
    ) {
        let model_name = model.into();

        let mut parts = vec![format!("Model: {}", model_name)];

        if let Some(input) = input_tokens {
            if let Some(output) = output_tokens {
                parts.push(format!("Tokens: {}→{}", input, output));
            } else {
                parts.push(format!("Input: {} tokens", input));
            }
        } else if let Some(output) = output_tokens {
            parts.push(format!("Output: {} tokens", output));
        }

        if let Some(latency) = latency_ms {
            let latency_sec = latency as f64 / 1000.0;
            parts.push(format!("Latency: {:.2}s", latency_sec));

            // Calculate tokens/sec if we have output tokens
            if let Some(output) = output_tokens {
                let tokens_per_sec = output as f64 / latency_sec;
                parts.push(format!("Speed: {:.1} tok/s", tokens_per_sec));
            }
        }

        let content = parts.join(" | ");
        self.update_line(StatusLineType::LiveStats, content);
    }

    /// Clear live stats (shorthand)
    pub fn clear_live_stats(&self) {
        self.remove_line(&StatusLineType::LiveStats);
    }
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for StatusBar {
    fn clone(&self) -> Self {
        Self {
            lines: Arc::clone(&self.lines),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_operations() {
        let status = StatusBar::new();

        status.update_line(StatusLineType::TrainingStats, "Test stats");
        status.update_line(StatusLineType::OperationStatus, "Test operation");

        assert_eq!(status.len(), 2);

        let lines = status.get_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].line_type, StatusLineType::TrainingStats);
        assert_eq!(lines[1].line_type, StatusLineType::OperationStatus);
    }

    #[test]
    fn test_update_overwrites() {
        let status = StatusBar::new();

        status.update_line(StatusLineType::TrainingStats, "First");
        status.update_line(StatusLineType::TrainingStats, "Second");

        assert_eq!(status.len(), 1);

        let lines = status.get_lines();
        assert_eq!(lines[0].content, "Second");
    }

    #[test]
    fn test_remove_line() {
        let status = StatusBar::new();

        status.update_line(StatusLineType::TrainingStats, "Test");
        assert_eq!(status.len(), 1);

        status.remove_line(&StatusLineType::TrainingStats);
        assert_eq!(status.len(), 0);
        assert!(status.is_empty());
    }

    #[test]
    fn test_line_ordering() {
        let status = StatusBar::new();

        // Add in random order
        status.update_line(StatusLineType::OperationStatus, "Operation");
        status.update_line(StatusLineType::TrainingStats, "Training");
        status.update_line(StatusLineType::DownloadProgress, "Download");

        let lines = status.get_lines();

        // Should be ordered: Training, Download, Operation
        assert_eq!(lines[0].line_type, StatusLineType::TrainingStats);
        assert_eq!(lines[1].line_type, StatusLineType::DownloadProgress);
        assert_eq!(lines[2].line_type, StatusLineType::OperationStatus);
    }

    #[test]
    fn test_status_line_ordering_with_session_and_memory() {
        let status = StatusBar::new();

        // Add in reverse order
        status.update_line(StatusLineType::LiveStats, "Live");
        status.update_line(StatusLineType::MemoryContext, "Memory");
        status.update_line(StatusLineType::SessionLabel, "Session");

        let lines = status.get_lines();

        // SessionLabel must be first, MemoryContext second, LiveStats third
        assert_eq!(lines[0].line_type, StatusLineType::SessionLabel);
        assert_eq!(lines[1].line_type, StatusLineType::MemoryContext);
        assert_eq!(lines[2].line_type, StatusLineType::LiveStats);
    }

    #[test]
    fn test_training_stats_format() {
        let status = StatusBar::new();

        status.update_training_stats(42, 0.38, 0.82);

        let lines = status.get_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].content,
            "Training: 42 queries | Local: 38% | Quality: 0.82"
        );
    }

    #[test]
    fn test_download_progress_format() {
        let status = StatusBar::new();

        status.update_download_progress("Qwen-2.5-3B", 0.80, 2_100_000_000, 2_600_000_000);

        let lines = status.get_lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].content.contains("Downloading Qwen-2.5-3B"));
        assert!(lines[0].content.contains("80%"));
        assert!(lines[0].content.contains("2.1GB"));
        assert!(lines[0].content.contains("2.6GB"));
    }

    #[test]
    fn test_render() {
        let status = StatusBar::new();

        status.update_line(StatusLineType::TrainingStats, "Line 1");
        status.update_line(StatusLineType::OperationStatus, "Line 2");

        let rendered = status.render();
        assert_eq!(rendered, "Line 1\nLine 2");
    }

    #[test]
    fn test_status_line_ordering_conversation_before_live_stats() {
        let status = StatusBar::new();

        // Add in reverse order
        status.update_line(StatusLineType::LiveStats, "Live");
        status.update_line(StatusLineType::ConversationFocus, "Focus");
        status.update_line(StatusLineType::ConversationTopic, "Topic");
        status.update_line(StatusLineType::MemoryContext, "Memory");
        status.update_line(StatusLineType::SessionLabel, "Session");

        let lines = status.get_lines();

        assert_eq!(lines[0].line_type, StatusLineType::SessionLabel);
        assert_eq!(lines[1].line_type, StatusLineType::MemoryContext);
        assert_eq!(lines[2].line_type, StatusLineType::ConversationTopic);
        assert_eq!(lines[3].line_type, StatusLineType::ConversationFocus);
        assert_eq!(lines[4].line_type, StatusLineType::LiveStats);
    }

    #[test]
    fn test_custom_lines() {
        let status = StatusBar::new();

        status.update_line(StatusLineType::Custom("test1".to_string()), "Custom 1");
        status.update_line(StatusLineType::Custom("test2".to_string()), "Custom 2");
        status.update_line(StatusLineType::TrainingStats, "Training");

        let lines = status.get_lines();

        // Training should be first, then custom lines (sorted)
        assert_eq!(lines[0].line_type, StatusLineType::TrainingStats);
        assert_eq!(
            lines[1].line_type,
            StatusLineType::Custom("test1".to_string())
        );
        assert_eq!(
            lines[2].line_type,
            StatusLineType::Custom("test2".to_string())
        );
    }
}
